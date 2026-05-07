use crate::types::{DataEntry, DataManifestEntry};
use crate::utils::{CONFIG, compress, decompress, hash_user_id, validate_key};
use anyhow::Result;
use futures::{StreamExt, join, stream};
use scylla::client::session::Session;
use scylla::response::query_result::QueryResult;
use scylla::statement::batch::{Batch, BatchType};
use scylla::statement::prepared::PreparedStatement;
use scylla::value::{CqlValue, Row};
use std::sync::Arc;
use tracing::warn;

const LWT_MAX_RETRIES: u32 = 5;
const QUERY_CONCURRENCY: usize = 16;

fn check_key(key: &str) -> Result<()> {
    validate_key(key).map_err(|e| anyhow::anyhow!(e.message()))
}

fn max_size_for_key(key: &str) -> usize {
    if key.starts_with("dataStore/") {
        CONFIG.max_datastore_key_size_bytes
    } else {
        CONFIG.max_key_size_bytes
    }
}

fn value_too_large(max_size: usize) -> anyhow::Error {
    let limit_mb = max_size / 1024 / 1024;
    anyhow::anyhow!("Value exceeds {}MB limit", limit_mb)
}

fn size_to_i32(len: usize) -> Result<i32> {
    i32::try_from(len).map_err(|_| anyhow::anyhow!("value size {} exceeds i32::MAX", len))
}

fn is_lwt_applied(result: QueryResult) -> Result<bool> {
    let rows_result = result.into_rows_result()?;
    let row: Row = rows_result.first_row::<Row>()?;
    match row.columns.first() {
        Some(Some(CqlValue::Boolean(b))) => Ok(*b),
        _ => Err(anyhow::anyhow!("LWT result missing [applied] column")),
    }
}

struct PreparedStatements {
    get_user_updated_at: PreparedStatement,
    get_user_settings: PreparedStatement,
    insert_user_settings_if_not_exists: PreparedStatement,
    update_user_settings_if_exists: PreparedStatement,
    delete_user: PreparedStatement,
    get_auth_secret: PreparedStatement,
    insert_auth_secret_if_not_exists: PreparedStatement,
    delete_auth: PreparedStatement,
    get_data_manifest: PreparedStatement,
    get_data_key: PreparedStatement,
    get_data_key_metadata: PreparedStatement,
    get_data_version: PreparedStatement,
    get_data_version_and_size: PreparedStatement,
    insert_data_key_if_not_exists: PreparedStatement,
    update_data_key_if_version: PreparedStatement,
    delete_data_key: PreparedStatement,
    delete_all_data: PreparedStatement,
    get_user_total_size: PreparedStatement,
    get_key_size: PreparedStatement,
    health_check: PreparedStatement,
}

#[derive(Clone)]
pub struct DatabaseService {
    session: Arc<Session>,
    prepared: Arc<PreparedStatements>,
}

/// Mark a prepared statement as idempotent so the driver's retry policy can
/// safely retry it on transient errors (read timeouts, connection drops).
/// LWT statements are idempotent under Paxos semantics; plain SELECTs and
/// DELETEs are also idempotent.
fn idempotent(mut stmt: PreparedStatement) -> PreparedStatement {
    stmt.set_is_idempotent(true);
    stmt
}

/// Apply a tighter page size to a paginated SELECT statement.
fn with_page_size(mut stmt: PreparedStatement, size: i32) -> PreparedStatement {
    stmt.set_page_size(size);
    stmt
}

impl DatabaseService {
    pub async fn new(session: Session) -> Result<Self> {
        session.use_keyspace("equicloud", false).await?;

        // Prepare every statement in parallel — ~20× faster cold start.
        let (
            get_user_updated_at,
            get_user_settings,
            insert_user_settings_if_not_exists,
            update_user_settings_if_exists,
            delete_user,
            get_auth_secret,
            insert_auth_secret_if_not_exists,
            delete_auth,
            get_data_manifest,
            get_data_key,
            get_data_key_metadata,
            get_data_version,
            get_data_version_and_size,
            insert_data_key_if_not_exists,
            update_data_key_if_version,
            delete_data_key,
            delete_all_data,
            get_user_total_size,
            get_key_size,
            health_check,
        ) = tokio::try_join!(
            session.prepare("SELECT updated_at FROM users WHERE id = ?"),
            session.prepare("SELECT settings, updated_at FROM users WHERE id = ?"),
            session.prepare("INSERT INTO users (id, settings, created_at, updated_at) VALUES (?, ?, ?, ?) IF NOT EXISTS"),
            session.prepare("UPDATE users SET settings = ?, updated_at = ? WHERE id = ? IF EXISTS"),
            session.prepare("DELETE FROM users WHERE id = ?"),
            session.prepare("SELECT secret FROM auth WHERE user_id = ?"),
            session.prepare("INSERT INTO auth (user_id, secret, created_at, updated_at) VALUES (?, ?, ?, ?) IF NOT EXISTS"),
            session.prepare("DELETE FROM auth WHERE user_id = ?"),
            session.prepare("SELECT key, version, checksum, size_bytes, updated_at FROM data WHERE user_id = ?"),
            session.prepare("SELECT key, value, version, checksum, size_bytes, created_at, updated_at FROM data WHERE user_id = ? AND key = ?"),
            session.prepare("SELECT version, checksum, size_bytes, updated_at FROM data WHERE user_id = ? AND key = ?"),
            session.prepare("SELECT version, created_at FROM data WHERE user_id = ? AND key = ?"),
            session.prepare("SELECT version, size_bytes FROM data WHERE user_id = ? AND key = ?"),
            session.prepare("INSERT INTO data (user_id, key, value, version, checksum, size_bytes, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?) IF NOT EXISTS"),
            session.prepare("UPDATE data SET value = ?, version = ?, checksum = ?, size_bytes = ?, updated_at = ? WHERE user_id = ? AND key = ? IF version = ?"),
            session.prepare("DELETE FROM data WHERE user_id = ? AND key = ?"),
            session.prepare("DELETE FROM data WHERE user_id = ?"),
            session.prepare("SELECT SUM(CAST(size_bytes AS BIGINT)) FROM data WHERE user_id = ?"),
            session.prepare("SELECT size_bytes FROM data WHERE user_id = ? AND key = ?"),
            session.prepare("SELECT now() FROM system.local"),
        )?;

        // SELECTs and DELETEs are idempotent. LWT statements are also idempotent
        // under Paxos semantics. Setting the flag lets `DefaultRetryPolicy`
        // retry on transient read-timeout errors instead of failing the request.
        let prepared = PreparedStatements {
            get_user_updated_at: idempotent(get_user_updated_at),
            get_user_settings: idempotent(get_user_settings),
            insert_user_settings_if_not_exists: idempotent(insert_user_settings_if_not_exists),
            update_user_settings_if_exists: idempotent(update_user_settings_if_exists),
            delete_user: idempotent(delete_user),
            get_auth_secret: idempotent(get_auth_secret),
            insert_auth_secret_if_not_exists: idempotent(insert_auth_secret_if_not_exists),
            delete_auth: idempotent(delete_auth),
            // Tighter page size keeps coordinator memory bounded for large users.
            get_data_manifest: idempotent(with_page_size(get_data_manifest, 1000)),
            get_data_key: idempotent(get_data_key),
            get_data_key_metadata: idempotent(get_data_key_metadata),
            get_data_version: idempotent(get_data_version),
            get_data_version_and_size: idempotent(get_data_version_and_size),
            insert_data_key_if_not_exists: idempotent(insert_data_key_if_not_exists),
            update_data_key_if_version: idempotent(update_data_key_if_version),
            delete_data_key: idempotent(delete_data_key),
            delete_all_data: idempotent(delete_all_data),
            get_user_total_size: idempotent(get_user_total_size),
            get_key_size: idempotent(get_key_size),
            health_check: idempotent(health_check),
        };

        Ok(Self {
            session: Arc::new(session),
            prepared: Arc::new(prepared),
        })
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub async fn health_check(&self) -> Result<()> {
        self.session
            .execute_unpaged(&self.prepared.health_check, &[])
            .await?;
        Ok(())
    }

    pub async fn get_settings_metadata(&self, user_id: &str) -> Result<Option<String>> {
        let hash_key = hash_user_id(user_id);

        if let Some(updated_at) = self.query_updated_at(&hash_key).await? {
            return Ok(Some(updated_at.to_string()));
        }

        Ok(None)
    }

    async fn query_updated_at(&self, key: &str) -> Result<Option<i64>> {
        let result = self
            .session
            .execute_unpaged(&self.prepared.get_user_updated_at, (key,))
            .await?;
        let rows_result = result.into_rows_result()?;
        if let Some(row) = rows_result.rows::<(i64,)>()?.next() {
            let (updated_at,) = row?;
            return Ok(Some(updated_at));
        }
        Ok(None)
    }

    pub async fn get_user_settings(&self, user_id: &str) -> Result<Option<(Vec<u8>, String)>> {
        let hash_key = hash_user_id(user_id);

        if let Some((settings, updated_at)) = self.query_settings(&hash_key).await? {
            return Ok(Some((settings, updated_at.to_string())));
        }

        Ok(None)
    }

    async fn query_settings(&self, key: &str) -> Result<Option<(Vec<u8>, i64)>> {
        let result = self
            .session
            .execute_unpaged(&self.prepared.get_user_settings, (key,))
            .await?;
        let rows_result = result.into_rows_result()?;
        if let Some(row) = rows_result.rows::<(Vec<u8>, i64)>()?.next() {
            let (settings, updated_at) = row?;
            return Ok(Some((settings, updated_at)));
        }
        Ok(None)
    }

    pub async fn save_user_settings(&self, user_id: &str, settings: Vec<u8>) -> Result<i64> {
        let hash_key = hash_user_id(user_id);
        let now = chrono::Utc::now().timestamp_millis();

        for _ in 0..LWT_MAX_RETRIES {
            let update_result = self
                .session
                .execute_unpaged(
                    &self.prepared.update_user_settings_if_exists,
                    (&settings, now, &hash_key),
                )
                .await?;
            if is_lwt_applied(update_result)? {
                return Ok(now);
            }

            let insert_result = self
                .session
                .execute_unpaged(
                    &self.prepared.insert_user_settings_if_not_exists,
                    (&hash_key, &settings, now, now),
                )
                .await?;
            if is_lwt_applied(insert_result)? {
                return Ok(now);
            }
        }

        Err(anyhow::anyhow!(
            "Failed to save user settings after {} retries",
            LWT_MAX_RETRIES
        ))
    }

    pub async fn delete_user_settings(&self, user_id: &str) -> Result<()> {
        let hash_key = hash_user_id(user_id);

        self.session
            .execute_unpaged(&self.prepared.delete_user, (&hash_key,))
            .await?;

        Ok(())
    }

    pub async fn get_user_auth_secret(&self, user_id: &str) -> Result<Option<String>> {
        let hash_key = hash_user_id(user_id);
        let result = self
            .session
            .execute_unpaged(&self.prepared.get_auth_secret, (&hash_key,))
            .await?;
        let rows_result = result.into_rows_result()?;
        if let Some(row) = rows_result.rows::<(String,)>()?.next() {
            let (secret,) = row?;
            return Ok(Some(secret));
        }
        Ok(None)
    }

    pub async fn get_or_create_user_auth_secret(&self, user_id: &str) -> Result<String> {
        // Skip the upfront read: go straight to INSERT IF NOT EXISTS. On the
        // happy path (existing user) this single LWT also returns the existing
        // row, which we read back; on the first-login path it inserts. Either
        // way, one round-trip instead of two.
        let secret_bytes: [u8; 32] = rand::random();
        let new_secret = hex::encode(secret_bytes);

        let hash_key = hash_user_id(user_id);
        let now = chrono::Utc::now().timestamp_millis();

        let result = self
            .session
            .execute_unpaged(
                &self.prepared.insert_auth_secret_if_not_exists,
                (&hash_key, &new_secret, now, now),
            )
            .await?;

        if is_lwt_applied(result)? {
            Ok(new_secret)
        } else {
            // Row already existed — read it back. (LWT result also contains
            // the existing values, but parsing the variable-shape result row
            // is messier than just doing a typed read.)
            self.get_user_auth_secret(user_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("auth row vanished between LWT and re-read"))
        }
    }

    pub async fn delete_user_auth(&self, user_id: &str) -> Result<()> {
        let hash_key = hash_user_id(user_id);
        self.session
            .execute_unpaged(&self.prepared.delete_auth, (&hash_key,))
            .await?;
        Ok(())
    }

    pub async fn delete_user_account(&self, user_id: &str) -> Result<()> {
        let hash_key = hash_user_id(user_id);
        // Unlogged: the three deletes are independent (different tables) and
        // each is individually idempotent. Logged batches go through the
        // batchlog (sync write to two replicas before any reply) which is
        // ~3× more expensive than unlogged for no correctness benefit here.
        let mut batch = Batch::new(BatchType::Unlogged);
        batch.append_statement(self.prepared.delete_user.clone());
        batch.append_statement(self.prepared.delete_all_data.clone());
        batch.append_statement(self.prepared.delete_auth.clone());

        self.session
            .batch(&batch, ((&hash_key,), (&hash_key,), (&hash_key,)))
            .await?;
        Ok(())
    }

    pub async fn get_data_manifest(&self, user_id: &str) -> Result<Vec<DataManifestEntry>> {
        let hash_key = hash_user_id(user_id);
        let pager = self
            .session
            .execute_iter(self.prepared.get_data_manifest.clone(), (&hash_key,))
            .await?;
        let mut row_stream = pager.rows_stream::<(String, i64, String, i32, i64)>()?;

        let mut entries = Vec::new();
        while let Some(row) = row_stream.next().await {
            let (key, version, checksum, size_bytes, updated_at) = row?;
            entries.push(DataManifestEntry {
                key,
                version,
                checksum,
                size_bytes,
                updated_at,
            });
        }
        Ok(entries)
    }

    /// Lightweight metadata-only fetch: returns `(version, checksum, size_bytes, updated_at)`
    /// without the (potentially MB-large, decompressed) value. Used to short-circuit
    /// conditional GETs (`If-None-Match`) before paying for the full row.
    pub async fn get_data_key_metadata(
        &self,
        user_id: &str,
        key: &str,
    ) -> Result<Option<(i64, String, i32, i64)>> {
        check_key(key)?;
        let hash_key = hash_user_id(user_id);
        let result = self
            .session
            .execute_unpaged(&self.prepared.get_data_key_metadata, (&hash_key, key))
            .await?;
        let rows_result = result.into_rows_result()?;
        Ok(rows_result
            .rows::<(i64, String, i32, i64)>()?
            .next()
            .transpose()?)
    }

    pub async fn get_data_key(&self, user_id: &str, key: &str) -> Result<Option<DataEntry>> {
        check_key(key)?;
        let hash_key = hash_user_id(user_id);
        let result = self
            .session
            .execute_unpaged(&self.prepared.get_data_key, (&hash_key, key))
            .await?;
        let rows_result = result.into_rows_result()?;

        if let Some(row) = rows_result
            .rows::<(String, Vec<u8>, i64, String, i32, i64, i64)>()?
            .next()
        {
            let (key, compressed_value, version, checksum, size_bytes, created_at, updated_at) =
                row?;
            return Ok(Some(DataEntry {
                key,
                value: decompress(&compressed_value),
                version,
                checksum,
                size_bytes,
                created_at,
                updated_at,
            }));
        }
        Ok(None)
    }

    pub async fn get_data_keys(&self, user_id: &str, keys: &[String]) -> Result<Vec<DataEntry>> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }

        for key in keys {
            check_key(key)?;
        }

        let hash_key: Arc<str> = hash_user_id(user_id).into();

        let futures = keys.iter().cloned().map(|key| {
            let session = Arc::clone(&self.session);
            let prepared = Arc::clone(&self.prepared);
            let hash_key = Arc::clone(&hash_key);
            async move {
                let result = session
                    .execute_unpaged(&prepared.get_data_key, (hash_key.as_ref(), &key))
                    .await?;
                let rows_result = result.into_rows_result()?;
                if let Some(row) = rows_result
                    .rows::<(String, Vec<u8>, i64, String, i32, i64, i64)>()?
                    .next()
                {
                    let (
                        key,
                        compressed_value,
                        version,
                        checksum,
                        size_bytes,
                        created_at,
                        updated_at,
                    ) = row?;
                    return Ok::<_, anyhow::Error>(Some(DataEntry {
                        key,
                        value: decompress(&compressed_value),
                        version,
                        checksum,
                        size_bytes,
                        created_at,
                        updated_at,
                    }));
                }
                Ok(None)
            }
        });

        let mut buffered = stream::iter(futures).buffer_unordered(QUERY_CONCURRENCY);
        let mut entries = Vec::with_capacity(keys.len());
        while let Some(result) = buffered.next().await {
            if let Some(entry) = result? {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    pub async fn save_data_key(
        &self,
        user_id: &str,
        key: &str,
        value: Vec<u8>,
        checksum: &str,
    ) -> Result<(i64, i64)> {
        check_key(key)?;

        let max_size = max_size_for_key(key);
        if value.len() > max_size {
            return Err(value_too_large(max_size));
        }

        let hash_key = hash_user_id(user_id);
        let size_bytes = size_to_i32(value.len())?;

        let compressed_value = if value.len() > 64 * 1024 {
            tokio::task::spawn_blocking(move || compress(&value))
                .await
                .map_err(|e| anyhow::anyhow!("compress task panicked: {}", e))?
        } else {
            compress(&value)
        };

        save_data_key_lwt(
            &self.session,
            &self.prepared,
            &hash_key,
            key,
            &compressed_value,
            checksum,
            size_bytes,
        )
        .await
    }

    pub async fn delete_data_key(&self, user_id: &str, key: &str) -> Result<()> {
        check_key(key)?;
        let hash_key = hash_user_id(user_id);
        self.session
            .execute_unpaged(&self.prepared.delete_data_key, (&hash_key, key))
            .await?;
        Ok(())
    }

    pub async fn delete_all_data(&self, user_id: &str) -> Result<()> {
        let hash_key = hash_user_id(user_id);
        self.session
            .execute_unpaged(&self.prepared.delete_all_data, (&hash_key,))
            .await?;
        Ok(())
    }

    /// Returns one tuple per saved entry: `(key, version, updated_at, checksum, size_bytes)`.
    /// The checksum and size_bytes are threaded through from the input so callers
    /// can build their post-write manifest without rebuilding a lookup map.
    pub async fn save_data_keys_batch(
        &self,
        user_id: &str,
        entries: Vec<(String, Vec<u8>, String)>,
    ) -> Result<Vec<(String, i64, i64, String, i32)>> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }

        let hash_key: Arc<str> = hash_user_id(user_id).into();

        let prepared_entries: Vec<_> = entries
            .into_iter()
            .filter_map(|(key, value, checksum)| {
                let max_size = max_size_for_key(&key);
                if value.len() > max_size {
                    warn!(
                        "save_data_keys_batch: dropping oversized entry for key {} (caller must validate before calling)",
                        key
                    );
                    return None;
                }
                let size_bytes = match size_to_i32(value.len()) {
                    Ok(n) => n,
                    Err(e) => {
                        warn!(
                            "save_data_keys_batch: dropping entry for key {}: {}",
                            key, e
                        );
                        return None;
                    }
                };
                let compressed_value = compress(&value);
                Some((key, compressed_value, checksum, size_bytes))
            })
            .collect();

        let futures =
            prepared_entries
                .into_iter()
                .map(|(key, compressed_value, checksum, size_bytes)| {
                    let session = Arc::clone(&self.session);
                    let prepared = Arc::clone(&self.prepared);
                    let hash_key = Arc::clone(&hash_key);

                    async move {
                        let (version, now) = save_data_key_lwt(
                            &session,
                            &prepared,
                            hash_key.as_ref(),
                            &key,
                            &compressed_value,
                            &checksum,
                            size_bytes,
                        )
                        .await?;
                        Ok::<_, anyhow::Error>((key, version, now, checksum, size_bytes))
                    }
                });

        let mut buffered = stream::iter(futures).buffer_unordered(QUERY_CONCURRENCY);
        let mut saved = Vec::new();
        while let Some(result) = buffered.next().await {
            saved.push(result?);
        }
        Ok(saved)
    }

    pub async fn get_user_total_size(&self, user_id: &str) -> Result<i64> {
        let hash_key = hash_user_id(user_id);
        let result = self
            .session
            .execute_unpaged(&self.prepared.get_user_total_size, (&hash_key,))
            .await?;
        let rows_result = result.into_rows_result()?;
        // SUM aggregates always return exactly one row in CQL; treat the absent
        // case as 0 only defensively.
        let sum = rows_result
            .rows::<(Option<i64>,)>()?
            .next()
            .transpose()?
            .and_then(|(s,)| s)
            .unwrap_or(0);
        Ok(sum)
    }

    pub async fn get_user_size_and_key_size(&self, user_id: &str, key: &str) -> Result<(i64, i64)> {
        check_key(key)?;
        let hash_key: Arc<str> = hash_user_id(user_id).into();
        let key: Arc<str> = key.into();

        let session1 = Arc::clone(&self.session);
        let session2 = Arc::clone(&self.session);
        let prepared1 = Arc::clone(&self.prepared);
        let prepared2 = Arc::clone(&self.prepared);
        let hash_key1 = Arc::clone(&hash_key);
        let hash_key2 = hash_key;
        let key = Arc::clone(&key);

        let total_future = async move {
            let result = session1
                .execute_unpaged(&prepared1.get_user_total_size, (hash_key1.as_ref(),))
                .await?;
            let rows_result = result.into_rows_result()?;
            let total = match rows_result.rows::<(Option<i64>,)>()?.next() {
                Some(row) => row?.0.unwrap_or(0),
                None => 0,
            };
            Ok::<i64, anyhow::Error>(total)
        };

        let key_future = async move {
            let result = session2
                .execute_unpaged(&prepared2.get_key_size, (hash_key2.as_ref(), key.as_ref()))
                .await?;
            let rows_result = result.into_rows_result()?;
            let size = match rows_result.rows::<(i32,)>()?.next() {
                Some(row) => row?.0 as i64,
                None => 0,
            };
            Ok::<i64, anyhow::Error>(size)
        };

        let (total_result, key_result) = join!(total_future, key_future);
        Ok((total_result?, key_result?))
    }

    pub async fn save_data_key_with_quota_check(
        &self,
        user_id: &str,
        key: &str,
        value: Vec<u8>,
        checksum: &str,
        max_total_size: i64,
    ) -> Result<Option<(i64, i64)>> {
        check_key(key)?;

        let max_size = max_size_for_key(key);
        if value.len() > max_size {
            return Err(value_too_large(max_size));
        }

        let hash_key = hash_user_id(user_id);
        let new_size = size_to_i32(value.len())?;

        // zstd of a multi-MB value is CPU-bound. Punt to the blocking pool when
        // the payload is large; inline for small ones to avoid spawn overhead.
        let compressed_value = if value.len() > 64 * 1024 {
            tokio::task::spawn_blocking(move || compress(&value))
                .await
                .map_err(|e| anyhow::anyhow!("compress task panicked: {}", e))?
        } else {
            compress(&value)
        };

        for _ in 0..LWT_MAX_RETRIES {
            let total_future = async {
                let result = self
                    .session
                    .execute_unpaged(&self.prepared.get_user_total_size, (&hash_key,))
                    .await?;
                let rows_result = result.into_rows_result()?;
                Ok::<i64, anyhow::Error>(
                    rows_result
                        .rows::<(Option<i64>,)>()?
                        .next()
                        .transpose()?
                        .and_then(|r| r.0)
                        .unwrap_or(0),
                )
            };

            let version_future = async {
                let result = self
                    .session
                    .execute_unpaged(&self.prepared.get_data_version_and_size, (&hash_key, key))
                    .await?;
                let rows_result = result.into_rows_result()?;
                Ok::<Option<(i64, i32)>, anyhow::Error>(
                    rows_result.rows::<(i64, i32)>()?.next().transpose()?,
                )
            };

            let (total_size_result, version_result) = join!(total_future, version_future);
            let total_size = total_size_result?;
            let existing = version_result?;

            let (existing_version, existing_size) = match existing {
                Some((v, s)) => (Some(v), s as i64),
                None => (None, 0),
            };

            let new_total = total_size - existing_size + new_size as i64;
            if new_total > max_total_size {
                return Ok(None);
            }

            // Recompute `now` per attempt so the persisted updated_at reflects
            // the actual write moment after Paxos contention.
            let now = chrono::Utc::now().timestamp_millis();

            match existing_version {
                None => {
                    let r = self
                        .session
                        .execute_unpaged(
                            &self.prepared.insert_data_key_if_not_exists,
                            (
                                &hash_key,
                                key,
                                &compressed_value,
                                1i64,
                                checksum,
                                new_size,
                                now,
                                now,
                            ),
                        )
                        .await?;
                    if is_lwt_applied(r)? {
                        return Ok(Some((1, now)));
                    }
                }
                Some(v) => {
                    let new_version = v + 1;
                    let r = self
                        .session
                        .execute_unpaged(
                            &self.prepared.update_data_key_if_version,
                            (
                                &compressed_value,
                                new_version,
                                checksum,
                                new_size,
                                now,
                                &hash_key,
                                key,
                                v,
                            ),
                        )
                        .await?;
                    if is_lwt_applied(r)? {
                        return Ok(Some((new_version, now)));
                    }
                }
            }
        }

        Err(anyhow::anyhow!(
            "Failed to save key after {} concurrent-write retries",
            LWT_MAX_RETRIES
        ))
    }
}

async fn save_data_key_lwt(
    session: &Session,
    prepared: &PreparedStatements,
    hash_key: &str,
    key: &str,
    compressed_value: &[u8],
    checksum: &str,
    size_bytes: i32,
) -> Result<(i64, i64)> {
    for _ in 0..LWT_MAX_RETRIES {
        let result = session
            .execute_unpaged(&prepared.get_data_version, (hash_key, key))
            .await?;
        let rows_result = result.into_rows_result()?;
        let existing = rows_result
            .rows::<(i64, i64)>()?
            .next()
            .transpose()?
            .map(|(v, _c)| v);

        // Recompute `now` per attempt so the persisted updated_at reflects
        // the actual write moment after Paxos contention.
        let now = chrono::Utc::now().timestamp_millis();

        match existing {
            None => {
                let r = session
                    .execute_unpaged(
                        &prepared.insert_data_key_if_not_exists,
                        (
                            hash_key,
                            key,
                            compressed_value,
                            1i64,
                            checksum,
                            size_bytes,
                            now,
                            now,
                        ),
                    )
                    .await?;
                if is_lwt_applied(r)? {
                    return Ok((1, now));
                }
            }
            Some(v) => {
                let new_version = v + 1;
                let r = session
                    .execute_unpaged(
                        &prepared.update_data_key_if_version,
                        (
                            compressed_value,
                            new_version,
                            checksum,
                            size_bytes,
                            now,
                            hash_key,
                            key,
                            v,
                        ),
                    )
                    .await?;
                if is_lwt_applied(r)? {
                    return Ok((new_version, now));
                }
            }
        }
    }

    Err(anyhow::anyhow!(
        "Failed to save key after {} concurrent-write retries",
        LWT_MAX_RETRIES
    ))
}
