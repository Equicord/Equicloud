use anyhow::{Context, Result, anyhow};
use hex;
use include_dir::{Dir, File, include_dir};
use scylla::client::session::Session;
use scylla::value::{CqlValue, Row};
use sha2::{Digest, Sha256};
use std::env;
use tracing::{debug, info, warn};

const MIGRATIONS_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/migrations");

const SCHEMA_MIGRATIONS_DDL: &str = "CREATE TABLE IF NOT EXISTS equicloud.schema_migrations (\
     name TEXT PRIMARY KEY, \
     checksum TEXT, \
     applied_at BIGINT)";

const DEFAULT_KEYSPACE_REPLICATION: &str = "{'class': 'SimpleStrategy', 'replication_factor': 1}";

pub struct MigrationRunner<'a> {
    session: &'a Session,
}

impl<'a> MigrationRunner<'a> {
    pub fn new(session: &'a Session) -> Self {
        Self { session }
    }

    pub async fn run_migrations(&self) -> Result<()> {
        let mut files: Vec<&File<'_>> = MIGRATIONS_DIR
            .files()
            .filter(|f| f.path().extension().is_some_and(|e| e == "cql"))
            .collect();
        files.sort_by_key(|f| f.path().to_path_buf());

        if files.is_empty() {
            warn!("No embedded migrations found");
        }

        let replication = env::var("SCYLLA_KEYSPACE_REPLICATION")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_KEYSPACE_REPLICATION.to_string());

        let create_keyspace_sql = format!(
            "CREATE KEYSPACE IF NOT EXISTS equicloud WITH REPLICATION = {} AND DURABLE_WRITES = true",
            replication
        );

        self.execute_cql(&create_keyspace_sql)
            .await
            .context("creating keyspace")?;

        self.execute_cql(SCHEMA_MIGRATIONS_DDL)
            .await
            .context("creating schema_migrations table")?;

        let mut applied_count = 0usize;
        let mut skipped_count = 0usize;

        for file in files {
            let name = file
                .path()
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| anyhow!("migration file has no name"))?;
            let content = file
                .contents_utf8()
                .ok_or_else(|| anyhow!("migration {} is not valid UTF-8", name))?;
            let checksum = compute_checksum(content);

            match self.lookup_applied(name).await? {
                Some(stored_checksum) => {
                    if stored_checksum != checksum {
                        return Err(anyhow!(
                            "Migration {} was previously applied with checksum {} \
                             but the current file has checksum {}. \
                             Migration files must not be modified after deployment; \
                             add a new migration instead.",
                            name,
                            stored_checksum,
                            checksum
                        ));
                    }
                    debug!("Migration {} already applied; skipping", name);
                    skipped_count += 1;
                }
                None => {
                    debug!("Applying migration: {}", name);
                    self.execute_cql(content)
                        .await
                        .with_context(|| format!("running migration {}", name))?;
                    self.record_applied(name, &checksum).await?;
                    applied_count += 1;
                }
            }
        }

        info!(
            "Migrations: {} applied, {} already up to date",
            applied_count, skipped_count
        );

        Ok(())
    }

    async fn execute_cql(&self, content: &str) -> Result<()> {
        let no_blocks = strip_block_comments(content);
        let cleaned = strip_comments(&no_blocks);
        let trimmed = cleaned.trim();
        let statement = trimmed.strip_suffix(';').unwrap_or(trimmed).trim();

        if statement.is_empty() {
            return Ok(());
        }

        if statement.contains(';') {
            return Err(anyhow!(
                "Migration contains multiple statements; each .cql file must contain exactly one statement"
            ));
        }

        self.session.query_unpaged(statement, &[]).await?;
        Ok(())
    }

    async fn lookup_applied(&self, name: &str) -> Result<Option<String>> {
        let result = self
            .session
            .query_unpaged(
                "SELECT checksum FROM equicloud.schema_migrations WHERE name = ?",
                (name,),
            )
            .await?;
        let rows_result = result.into_rows_result()?;
        if let Some(row) = rows_result.rows::<(String,)>()?.next() {
            let (checksum,) = row?;
            return Ok(Some(checksum));
        }
        Ok(None)
    }

    async fn record_applied(&self, name: &str, checksum: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let result = self
            .session
            .query_unpaged(
                "INSERT INTO equicloud.schema_migrations (name, checksum, applied_at) \
                 VALUES (?, ?, ?) IF NOT EXISTS",
                (name, checksum, now),
            )
            .await?;

        let rows_result = result.into_rows_result()?;
        let row: Row = rows_result.first_row::<Row>()?;
        let applied = matches!(row.columns.first(), Some(Some(CqlValue::Boolean(true))));

        if applied {
            return Ok(());
        }

        // Lost the race: another runner inserted the bookkeeping row between our
        // lookup_applied and this INSERT. Verify the recorded checksum matches
        // ours; if it does, the DDL we ran was idempotent and harmless. If it
        // doesn't, two different versions of the migration ran concurrently
        // (rolling-deploy mismatch) and we should fail loudly.
        match self.lookup_applied(name).await? {
            Some(existing) if existing == checksum => {
                debug!(
                    "Migration {} bookkeeping row inserted by another runner with matching checksum",
                    name
                );
                Ok(())
            }
            Some(existing) => Err(anyhow!(
                "Migration {} race detected: bookkeeping row has checksum {} but we tried to record {}. \
                 Likely two replicas running different code revisions.",
                name,
                existing,
                checksum
            )),
            None => Err(anyhow!(
                "Migration {} bookkeeping row vanished after a not-applied INSERT — DB consistency issue",
                name
            )),
        }
    }
}

fn compute_checksum(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

fn strip_comments(content: &str) -> String {
    content
        .lines()
        .map(|line| {
            if let Some(idx) = line.find("--") {
                &line[..idx]
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Strip C-style `/* … */` block comments. Does not support nesting (CQL
/// itself doesn't allow it). Mismatched openers leave the rest of the input
/// intact so that the downstream parser still surfaces a meaningful error.
fn strip_block_comments(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut chars = content.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next(); // consume the `*`
            let mut closed = false;
            while let Some(c) = chars.next() {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    closed = true;
                    break;
                }
            }
            if !closed {
                // Unterminated block comment — bail out, let the CQL parser
                // surface a syntax error for the original content instead of
                // emitting a half-stripped statement.
                return content.to_string();
            }
        } else {
            out.push(c);
        }
    }
    out
}
