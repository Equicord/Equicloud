use ahash::{AHashMap, AHashSet};
use axum::{
    Extension, Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use std::borrow::Cow;
use tracing::error;

use equicloud::types::DataManifestEntry;
use equicloud::types::sync::{
    ClientManifestEntry, DownloadEntry, SyncError, SyncRequest, SyncResponse, UploadResult,
};
use equicloud::utils::{CONFIG, error_response};
use equicloud::{DatabaseService, compute_checksum, validate_key};

#[derive(Copy, Clone, PartialEq, Eq)]
enum KeyKind {
    Plain,
    DataStore,
}

impl KeyKind {
    #[inline]
    fn from_key(key: &str) -> Self {
        if key.starts_with("dataStore/") {
            Self::DataStore
        } else {
            Self::Plain
        }
    }

    #[inline]
    fn max_size(self) -> usize {
        match self {
            Self::DataStore => CONFIG.max_datastore_key_size_bytes,
            Self::Plain => CONFIG.max_key_size_bytes,
        }
    }

    #[inline]
    fn is_blocked(self) -> bool {
        matches!(self, Self::DataStore) && !CONFIG.datastore_enabled
    }
}

fn err(status: StatusCode, message: &str) -> Response {
    (status, Json(error_response(message))).into_response()
}

#[inline]
fn sync_error(key: String, msg: impl Into<Cow<'static, str>>) -> SyncError {
    SyncError {
        key,
        error: msg.into(),
    }
}

pub async fn delta_sync(
    Extension(db): Extension<DatabaseService>,
    Extension(user_id): Extension<String>,
    Json(request): Json<SyncRequest>,
) -> Response {
    if request.uploads.len() > CONFIG.max_sync_uploads {
        return err(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Too many uploads in a single request",
        );
    }

    if request.client_manifest.len() > CONFIG.max_sync_manifest_entries {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "Client manifest too large");
    }

    let server_manifest = match db.get_data_manifest(&user_id).await {
        Ok(m) => m,
        Err(e) => {
            error!("Failed to get manifest: {}", e);
            return err(StatusCode::INTERNAL_SERVER_ERROR, "Database error");
        }
    };

    let upload_count = request.uploads.len();
    let mut downloads = Vec::with_capacity(server_manifest.len().min(upload_count.max(16)));
    let mut uploaded = Vec::with_capacity(upload_count);
    let mut errors: Vec<SyncError> = Vec::with_capacity(upload_count);
    let mut valid_uploads: Vec<(String, Vec<u8>, String)> = Vec::with_capacity(upload_count);
    let mut keys_to_check: Vec<String> = Vec::with_capacity(upload_count);
    let mut seen_upload_keys: AHashSet<u64> = AHashSet::with_capacity(upload_count);

    {
        let server_map: AHashMap<&str, &DataManifestEntry> = server_manifest
            .iter()
            .map(|e| (e.key.as_str(), e))
            .collect();

        // Only build the client map if there are uploads to validate against.
        let client_map: AHashMap<&str, &ClientManifestEntry> = if upload_count > 0 {
            request
                .client_manifest
                .iter()
                .map(|e| (e.key.as_str(), e))
                .collect()
        } else {
            AHashMap::new()
        };

        let keys_to_download: Vec<String> = server_manifest
            .iter()
            .filter(|s| {
                let kind = KeyKind::from_key(&s.key);
                if kind.is_blocked() {
                    return false;
                }
                !client_map
                    .get(s.key.as_str())
                    .is_some_and(|c| c.version >= s.version && c.checksum == s.checksum)
            })
            .map(|s| s.key.clone())
            .collect();

        if !keys_to_download.is_empty() {
            match db.get_data_keys(&user_id, &keys_to_download).await {
                Ok(entries) => {
                    for entry in entries {
                        downloads.push(DownloadEntry {
                            key: entry.key,
                            value: entry.value,
                            version: entry.version,
                            checksum: entry.checksum,
                        });
                    }
                }
                Err(e) => {
                    error!("Failed to get data keys: {}", e);
                    for key in keys_to_download {
                        errors.push(sync_error(key, "Failed to download"));
                    }
                }
            }
        }

        let current_size: i64 = server_manifest.iter().map(|e| e.size_bytes as i64).sum();
        let max_size = CONFIG.max_backup_size_bytes as i64;
        let mut running_size = current_size;

        for upload in request.uploads {
            // Hash-based dedup avoids cloning the key just to insert into a set.
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&upload.key, &mut hasher);
            let key_hash = std::hash::Hasher::finish(&hasher);
            if !seen_upload_keys.insert(key_hash) {
                errors.push(sync_error(upload.key, "Duplicate key in same upload batch"));
                continue;
            }

            if let Err(e) = validate_key(&upload.key) {
                errors.push(sync_error(upload.key, e.message()));
                continue;
            }

            let kind = KeyKind::from_key(&upload.key);

            if kind.is_blocked() {
                errors.push(sync_error(upload.key, "DataStore sync is disabled"));
                continue;
            }

            let key_max_size = kind.max_size();
            if upload.value.len() > key_max_size {
                let limit_mb = key_max_size / 1024 / 1024;
                errors.push(sync_error(
                    upload.key,
                    format!("Value exceeds {}MB limit", limit_mb),
                ));
                continue;
            }

            let checksum = match &upload.checksum {
                Some(provided) => {
                    let computed = compute_checksum(&upload.value);
                    if computed != *provided {
                        errors.push(sync_error(upload.key, "Checksum mismatch"));
                        continue;
                    }
                    computed
                }
                None => compute_checksum(&upload.value),
            };

            let dominated_by_server = server_map.get(upload.key.as_str()).is_some_and(|s| {
                client_map
                    .get(upload.key.as_str())
                    .is_none_or(|c| c.version <= s.version)
            });

            if dominated_by_server {
                errors.push(sync_error(
                    upload.key,
                    "Server has equal or newer version; pull before pushing",
                ));
                continue;
            }

            let existing_size = server_map
                .get(upload.key.as_str())
                .map(|e| e.size_bytes as i64)
                .unwrap_or(0);

            let new_running = running_size - existing_size + upload.value.len() as i64;
            if new_running > max_size {
                errors.push(sync_error(upload.key, "Total storage limit exceeded"));
                continue;
            }

            running_size = new_running;
            keys_to_check.push(upload.key.clone());
            valid_uploads.push((upload.key, upload.value, checksum));
        }
    }

    let mut updated_manifest: AHashMap<String, DataManifestEntry> =
        AHashMap::with_capacity(valid_uploads.len());

    if !valid_uploads.is_empty() {
        match db.save_data_keys_batch(&user_id, valid_uploads).await {
            Ok(saved) => {
                // The DB layer threads checksum + size_bytes through, so we
                // can build both `uploaded` and the manifest delta in one
                // pass without rebuilding a lookup map.
                for (key, version, now, checksum, size_bytes) in saved {
                    uploaded.push(UploadResult {
                        key: key.clone(),
                        version,
                        checksum: checksum.clone(),
                    });
                    updated_manifest.insert(
                        key.clone(),
                        DataManifestEntry {
                            key,
                            version,
                            checksum,
                            size_bytes,
                            updated_at: now,
                        },
                    );
                }
            }
            Err(e) => {
                error!("Failed to save batch: {}", e);
                for key in keys_to_check {
                    errors.push(sync_error(key, "Failed to save"));
                }
            }
        }
    }

    // Apply the writes to the server manifest in-memory rather than re-fetching
    // the whole partition from Scylla. Concurrent writes from other devices
    // won't appear until the next sync; that's an accepted trade-off.
    let mut final_manifest = server_manifest;
    for entry in final_manifest.iter_mut() {
        if let Some(updated) = updated_manifest.remove(&entry.key) {
            *entry = updated;
        }
    }
    // Any updated entries that didn't exist in the original manifest are new keys.
    final_manifest.extend(updated_manifest.into_values());

    if !CONFIG.datastore_enabled {
        final_manifest.retain(|e| !e.key.starts_with("dataStore/"));
    }

    Json(SyncResponse {
        server_manifest: final_manifest,
        downloads,
        uploaded,
        errors,
    })
    .into_response()
}
