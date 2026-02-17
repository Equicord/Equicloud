use crate::hash_migration::legacy;
use crate::utils::{CONFIG, compress, decompress, hash_user_id, validate_key};
use anyhow::Result;
use futures::{future::join_all, join};
use scylla::client::session::Session;
use scylla::statement::prepared::PreparedStatement;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataEntry {
    pub key: String,
    pub value: Vec<u8>,
    pub version: i64,
    pub checksum: String,
    pub size_bytes: i32,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataManifestEntry {
    pub key: String,
    pub version: i64,
    pub checksum: String,
    pub size_bytes: i32,
    pub updated_at: i64,
}

fn check_key(key: &str) -> Result<()> {
    validate_key(key).map_err(|e| anyhow::anyhow!(e.message()))
}

fn get_legacy_key_if_different(user_id: &str, new_key: &str) -> Option<String> {
    let legacy_key = legacy::hash_user_id(user_id);
    if legacy_key != new_key {
        Some(legacy_key)
    } else {
        None
    }
}

struct PreparedStatements {
    get_user_updated_at: PreparedStatement,
    get_user_settings: PreparedStatement,
    insert_user_settings: PreparedStatement,
    delete_user: PreparedStatement,
    get_user_created_at: PreparedStatement,
    get_data_manifest: PreparedStatement,
    get_data_key: PreparedStatement,
    get_data_version: PreparedStatement,
    get_data_version_and_size: PreparedStatement,
    insert_data_key: PreparedStatement,
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

impl DatabaseService {
    pub async fn new(session: Session) -> Result<Self> {
        session.use_keyspace("equicloud", false).await?;

        let prepared = PreparedStatements {
            get_user_updated_at: session
                .prepare("SELECT updated_at FROM users WHERE id = ?")
                .await?,
            get_user_settings: session
                .prepare("SELECT settings, updated_at FROM users WHERE id = ?")
                .await?,
            insert_user_settings: session
                .prepare("INSERT INTO users (id, settings, created_at, updated_at) VALUES (?, ?, ?, ?)")
                .await?,
            delete_user: session
                .prepare("DELETE FROM users WHERE id = ?")
                .await?,
            get_user_created_at: session
                .prepare("SELECT created_at FROM users WHERE id = ?")
                .await?,
            get_data_manifest: session
                .prepare("SELECT key, version, checksum, size_bytes, updated_at FROM data WHERE user_id = ?")
                .await?,
            get_data_key: session
                .prepare("SELECT key, value, version, checksum, size_bytes, created_at, updated_at FROM data WHERE user_id = ? AND key = ?")
                .await?,
            get_data_version: session
                .prepare("SELECT version, created_at FROM data WHERE user_id = ? AND key = ?")
                .await?,
            get_data_version_and_size: session
                .prepare("SELECT version, created_at, size_bytes FROM data WHERE user_id = ? AND key = ?")
                .await?,
            insert_data_key: session
                .prepare("INSERT INTO data (user_id, key, value, version, checksum, size_bytes, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
                .await?,
            delete_data_key: session
                .prepare("DELETE FROM data WHERE user_id = ? AND key = ?")
                .await?,
            delete_all_data: session
                .prepare("DELETE FROM data WHERE user_id = ?")
                .await?,
            get_user_total_size: session
                .prepare("SELECT SUM(size_bytes) FROM data WHERE user_id = ?")
                .await?,
            get_key_size: session
                .prepare("SELECT size_bytes FROM data WHERE user_id = ? AND key = ?")
                .await?,
            health_check: session
                .prepare("SELECT now() FROM system.local")
                .await?,
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

        if let Some(legacy_key) = get_legacy_key_if_different(user_id, &hash_key)
            && let Some(updated_at) = self.query_updated_at(&legacy_key).await?
        {
            warn!("Found legacy data for user, will migrate on next write");
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

        if let Some(legacy_key) = get_legacy_key_if_different(user_id, &hash_key)
            && let Some((settings, updated_at)) = self.query_settings(&legacy_key).await?
        {
            info!(
                "Found legacy settings for user {}, migrating to new hash format",
                user_id
            );

            if let Err(e) = self
                .migrate_user_data(user_id, &legacy_key, &hash_key, &settings, updated_at)
                .await
            {
                warn!("Failed to migrate user data: {}", e);
            }

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

        self.session
            .execute_unpaged(
                &self.prepared.insert_user_settings,
                (&hash_key, &settings, now, now),
            )
            .await?;

        self.cleanup_legacy_data(user_id, &hash_key).await;

        Ok(now)
    }

    pub async fn delete_user_settings(&self, user_id: &str) -> Result<()> {
        let hash_key = hash_user_id(user_id);

        self.session
            .execute_unpaged(&self.prepared.delete_user, (&hash_key,))
            .await?;

        self.cleanup_legacy_data(user_id, &hash_key).await;

        Ok(())
    }

    async fn cleanup_legacy_data(&self, user_id: &str, new_key: &str) {
        if let Some(legacy_key) = get_legacy_key_if_different(user_id, new_key)
            && let Err(e) = self.delete_legacy_data(&legacy_key).await
        {
            warn!("Failed to clean up legacy data: {}", e);
        }
    }

    async fn migrate_user_data(
        &self,
        user_id: &str,
        legacy_key: &str,
        new_key: &str,
        settings: &[u8],
        updated_at: i64,
    ) -> Result<()> {
        info!("Migrating user {} from legacy hash to SHA-256", user_id);

        let result = self
            .session
            .execute_unpaged(&self.prepared.get_user_created_at, (legacy_key,))
            .await?;
        let rows_result = result.into_rows_result()?;

        let created_at = rows_result
            .rows::<(i64,)>()?
            .next()
            .transpose()?
            .map(|row| row.0)
            .unwrap_or(updated_at);

        self.session
            .execute_unpaged(
                &self.prepared.insert_user_settings,
                (new_key, settings, created_at, updated_at),
            )
            .await?;

        self.delete_legacy_data(legacy_key).await?;

        info!("Successfully migrated user {}", user_id);
        Ok(())
    }

    async fn delete_legacy_data(&self, legacy_key: &str) -> Result<()> {
        self.session
            .execute_unpaged(&self.prepared.delete_user, (legacy_key,))
            .await?;
        Ok(())
    }

    pub async fn get_data_manifest(&self, user_id: &str) -> Result<Vec<DataManifestEntry>> {
        let hash_key = hash_user_id(user_id);
        let result = self
            .session
            .execute_unpaged(&self.prepared.get_data_manifest, (&hash_key,))
            .await?;
        let rows_result = result.into_rows_result()?;

        let mut entries = Vec::new();
        for row in rows_result.rows::<(String, i64, String, i32, i64)>()? {
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

        let futures = keys.iter().map(|key| {
            let session = Arc::clone(&self.session);
            let prepared = Arc::clone(&self.prepared);
            let hash_key = Arc::clone(&hash_key);
            let key = key.clone();
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

        let results = join_all(futures).await;
        let mut entries = Vec::with_capacity(keys.len());
        for result in results {
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

        let max_size = if key.starts_with("dataStore/") {
            CONFIG.max_datastore_key_size_bytes
        } else {
            CONFIG.max_key_size_bytes
        };

        if value.len() > max_size {
            let limit_mb = max_size / 1024 / 1024;
            return Err(anyhow::anyhow!("Value exceeds {}MB limit", limit_mb));
        }

        let hash_key = hash_user_id(user_id);
        let now = chrono::Utc::now().timestamp_millis();
        let size_bytes = value.len() as i32;
        let compressed_value = compress(&value);

        let result = self
            .session
            .execute_unpaged(&self.prepared.get_data_version, (&hash_key, key))
            .await?;
        let rows_result = result.into_rows_result()?;

        let (version, created_at) = if let Some(row) = rows_result.rows::<(i64, i64)>()?.next() {
            let (v, c) = row?;
            (v + 1, c)
        } else {
            (1, now)
        };

        self.session
            .execute_unpaged(
                &self.prepared.insert_data_key,
                (
                    &hash_key,
                    key,
                    &compressed_value,
                    version,
                    checksum,
                    size_bytes,
                    created_at,
                    now,
                ),
            )
            .await?;

        Ok((version, now))
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

    pub async fn save_data_keys_batch(
        &self,
        user_id: &str,
        entries: Vec<(String, Vec<u8>, String)>,
        existing_versions: &std::collections::HashMap<String, (i64, i64)>,
    ) -> Result<Vec<(String, i64, i64)>> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }

        let hash_key: Arc<str> = hash_user_id(user_id).into();
        let now = chrono::Utc::now().timestamp_millis();

        let prepared_entries: Vec<_> = entries
            .into_iter()
            .filter_map(|(key, value, checksum)| {
                let max_size = if key.starts_with("dataStore/") {
                    CONFIG.max_datastore_key_size_bytes
                } else {
                    CONFIG.max_key_size_bytes
                };
                if value.len() > max_size {
                    return None;
                }
                let size_bytes = value.len() as i32;
                let compressed_value = compress(&value);
                let (version, created_at) = match existing_versions.get(&key).copied() {
                    Some((v, c)) => (v + 1, c),
                    None => (1, now),
                };
                Some((
                    key,
                    compressed_value,
                    checksum,
                    size_bytes,
                    version,
                    created_at,
                ))
            })
            .collect();

        let futures = prepared_entries.into_iter().map(
            |(key, compressed_value, checksum, size_bytes, version, created_at)| {
                let session = Arc::clone(&self.session);
                let prepared = Arc::clone(&self.prepared);
                let hash_key = Arc::clone(&hash_key);

                async move {
                    session
                        .execute_unpaged(
                            &prepared.insert_data_key,
                            (
                                hash_key.as_ref(),
                                &key,
                                &compressed_value,
                                version,
                                &checksum,
                                size_bytes,
                                created_at,
                                now,
                            ),
                        )
                        .await?;

                    Ok::<_, anyhow::Error>((key, version, now))
                }
            },
        );

        let results = join_all(futures).await;
        let mut saved = Vec::with_capacity(results.len());
        for result in results {
            saved.push(result?);
        }
        Ok(saved)
    }

    pub async fn get_versions_batch(
        &self,
        user_id: &str,
        keys: &[String],
    ) -> Result<std::collections::HashMap<String, (i64, i64)>> {
        if keys.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        let hash_key: Arc<str> = hash_user_id(user_id).into();

        let futures = keys.iter().map(|key| {
            let session = Arc::clone(&self.session);
            let prepared = Arc::clone(&self.prepared);
            let hash_key = Arc::clone(&hash_key);
            let key = key.clone();
            async move {
                let result = session
                    .execute_unpaged(&prepared.get_data_version, (hash_key.as_ref(), &key))
                    .await?;
                let rows_result = result.into_rows_result()?;
                if let Some(row) = rows_result.rows::<(i64, i64)>()?.next() {
                    let (version, created_at) = row?;
                    return Ok::<_, anyhow::Error>(Some((key, version, created_at)));
                }
                Ok(None)
            }
        });

        let results = join_all(futures).await;
        let mut versions = std::collections::HashMap::with_capacity(keys.len());
        for result in results {
            if let Some((key, version, created_at)) = result? {
                versions.insert(key, (version, created_at));
            }
        }
        Ok(versions)
    }

    pub async fn get_user_total_size(&self, user_id: &str) -> Result<i64> {
        let hash_key = hash_user_id(user_id);
        let result = self
            .session
            .execute_unpaged(&self.prepared.get_user_total_size, (&hash_key,))
            .await?;
        let rows_result = result.into_rows_result()?;

        if let Some(row) = rows_result.rows::<(Option<i32>,)>()?.next() {
            let (sum,) = row?;
            return Ok(sum.unwrap_or(0) as i64);
        }
        Ok(0)
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
            let total = match rows_result.rows::<(Option<i32>,)>()?.next() {
                Some(row) => row?.0.unwrap_or(0) as i64,
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

        let max_size = if key.starts_with("dataStore/") {
            CONFIG.max_datastore_key_size_bytes
        } else {
            CONFIG.max_key_size_bytes
        };

        if value.len() > max_size {
            let limit_mb = max_size / 1024 / 1024;
            return Err(anyhow::anyhow!("Value exceeds {}MB limit", limit_mb));
        }

        let hash_key: Arc<str> = hash_user_id(user_id).into();
        let now = chrono::Utc::now().timestamp_millis();
        let new_size = value.len() as i32;
        let key: Arc<str> = key.into();

        let (total_size_result, version_result) = {
            let session1 = Arc::clone(&self.session);
            let session2 = Arc::clone(&self.session);
            let prepared1 = Arc::clone(&self.prepared);
            let prepared2 = Arc::clone(&self.prepared);
            let hash_key1 = Arc::clone(&hash_key);
            let hash_key2 = Arc::clone(&hash_key);
            let key_clone = Arc::clone(&key);

            let total_future = async move {
                let result = session1
                    .execute_unpaged(&prepared1.get_user_total_size, (hash_key1.as_ref(),))
                    .await?;
                let rows_result = result.into_rows_result()?;
                Ok::<i64, anyhow::Error>(
                    rows_result
                        .rows::<(Option<i32>,)>()?
                        .next()
                        .transpose()?
                        .and_then(|r| r.0)
                        .unwrap_or(0) as i64,
                )
            };

            let version_future = async move {
                let result = session2
                    .execute_unpaged(
                        &prepared2.get_data_version_and_size,
                        (hash_key2.as_ref(), key_clone.as_ref()),
                    )
                    .await?;
                let rows_result = result.into_rows_result()?;
                Ok::<Option<(i64, i64, i32)>, anyhow::Error>(
                    rows_result.rows::<(i64, i64, i32)>()?.next().transpose()?,
                )
            };

            join!(total_future, version_future)
        };

        let total_size = total_size_result?;
        let existing = version_result?;

        let (version, created_at, existing_size) = match existing {
            Some((v, c, s)) => (v + 1, c, s as i64),
            None => (1, now, 0),
        };

        let new_total = total_size - existing_size + new_size as i64;
        if new_total > max_total_size {
            return Ok(None);
        }

        let compressed_value = compress(&value);

        self.session
            .execute_unpaged(
                &self.prepared.insert_data_key,
                (
                    hash_key.as_ref(),
                    key.as_ref(),
                    &compressed_value,
                    version,
                    checksum,
                    new_size,
                    created_at,
                    now,
                ),
            )
            .await?;

        Ok(Some((version, now)))
    }
}
