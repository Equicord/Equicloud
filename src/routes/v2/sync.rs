use axum::{Extension, Json, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::error;

use equicloud::utils::CONFIG;
use equicloud::{DataManifestEntry, DatabaseService, compute_checksum, validate_key};

#[derive(Deserialize)]
pub struct SyncRequest {
    client_manifest: Vec<ClientManifestEntry>,
    #[serde(default)]
    uploads: Vec<UploadEntry>,
}

#[derive(Deserialize)]
pub struct ClientManifestEntry {
    key: String,
    version: i64,
    checksum: String,
}

#[derive(Deserialize)]
pub struct UploadEntry {
    key: String,
    #[serde(with = "base64_serde")]
    value: Vec<u8>,
    #[serde(default)]
    checksum: Option<String>,
}

mod base64_serde {
    use base64::prelude::*;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        BASE64_STANDARD.decode(&s).map_err(serde::de::Error::custom)
    }

    pub fn serialize<S>(bytes: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&BASE64_STANDARD.encode(bytes))
    }
}

#[derive(Serialize)]
pub struct SyncResponse {
    server_manifest: Vec<DataManifestEntry>,
    downloads: Vec<DownloadEntry>,
    uploaded: Vec<UploadResult>,
    errors: Vec<SyncError>,
}

#[derive(Serialize)]
pub struct DownloadEntry {
    key: String,
    #[serde(with = "base64_serde")]
    value: Vec<u8>,
    version: i64,
    checksum: String,
}

#[derive(Serialize)]
pub struct UploadResult {
    key: String,
    version: i64,
    checksum: String,
}

#[derive(Serialize)]
pub struct SyncError {
    key: String,
    error: String,
}

pub async fn delta_sync(
    Extension(db): Extension<DatabaseService>,
    Extension(user_id): Extension<String>,
    Json(request): Json<SyncRequest>,
) -> impl IntoResponse {
    let server_manifest = match db.get_data_manifest(&user_id).await {
        Ok(m) => m,
        Err(e) => {
            error!("Failed to get manifest: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "Database error"})),
            )
                .into_response();
        }
    };

    let mut downloads = Vec::with_capacity(server_manifest.len());
    let mut uploaded = Vec::with_capacity(request.uploads.len());
    let mut errors = Vec::new();

    let server_map: HashMap<&str, &DataManifestEntry> = server_manifest
        .iter()
        .map(|e| (e.key.as_str(), e))
        .collect();

    let client_map: HashMap<&str, &ClientManifestEntry> = request
        .client_manifest
        .iter()
        .map(|e| (e.key.as_str(), e))
        .collect();

    let keys_to_download: Vec<String> = server_manifest
        .iter()
        .filter(|s| {
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
                    errors.push(SyncError {
                        key,
                        error: "Failed to download".into(),
                    });
                }
            }
        }
    }

    let current_size: i64 = server_manifest.iter().map(|e| e.size_bytes as i64).sum();
    let max_size = CONFIG.max_backup_size_bytes as i64;
    let mut running_size = current_size;

    let mut valid_uploads: Vec<(String, Vec<u8>, String)> =
        Vec::with_capacity(request.uploads.len());
    let mut keys_to_check: Vec<String> = Vec::with_capacity(request.uploads.len());

    for upload in request.uploads {
        if let Err(e) = validate_key(&upload.key) {
            errors.push(SyncError {
                key: upload.key,
                error: e.message().into(),
            });
            continue;
        }

        if !CONFIG.datastore_enabled && upload.key.starts_with("dataStore/") {
            errors.push(SyncError {
                key: upload.key,
                error: "DataStore sync is disabled".into(),
            });
            continue;
        }

        let key_max_size = if upload.key.starts_with("dataStore/") {
            CONFIG.max_datastore_key_size_bytes
        } else {
            CONFIG.max_key_size_bytes
        };

        if upload.value.len() > key_max_size {
            let limit_mb = key_max_size / 1024 / 1024;
            errors.push(SyncError {
                key: upload.key,
                error: format!("Value exceeds {}MB limit", limit_mb),
            });
            continue;
        }

        let checksum = match &upload.checksum {
            Some(provided) => {
                let computed = compute_checksum(&upload.value);
                if computed != *provided {
                    errors.push(SyncError {
                        key: upload.key,
                        error: "Checksum mismatch".into(),
                    });
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
            continue;
        }

        let existing_size = server_map
            .get(upload.key.as_str())
            .map(|e| e.size_bytes as i64)
            .unwrap_or(0);

        let new_running = running_size - existing_size + upload.value.len() as i64;
        if new_running > max_size {
            errors.push(SyncError {
                key: upload.key,
                error: "Total storage limit exceeded".into(),
            });
            continue;
        }

        running_size = new_running;
        keys_to_check.push(upload.key.clone());
        valid_uploads.push((upload.key, upload.value, checksum));
    }

    let mut updated_keys: HashMap<String, (i64, String, i32)> = HashMap::new();

    if !valid_uploads.is_empty() {
        let upload_info: HashMap<String, (String, i32)> = valid_uploads
            .iter()
            .map(|(k, v, c)| (k.clone(), (c.clone(), v.len() as i32)))
            .collect();

        match db.get_versions_batch(&user_id, &keys_to_check).await {
            Ok(existing_versions) => {
                match db
                    .save_data_keys_batch(&user_id, valid_uploads, &existing_versions)
                    .await
                {
                    Ok(saved) => {
                        for (key, version, _) in saved {
                            if let Some((checksum, size)) = upload_info.get(&key) {
                                updated_keys
                                    .insert(key.clone(), (version, checksum.clone(), *size));
                                uploaded.push(UploadResult {
                                    key,
                                    version,
                                    checksum: checksum.clone(),
                                });
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to save batch: {}", e);
                        for key in keys_to_check {
                            errors.push(SyncError {
                                key,
                                error: "Failed to save".into(),
                            });
                        }
                    }
                }
            }
            Err(e) => {
                error!("Failed to get versions batch: {}", e);
                for (key, _, _) in valid_uploads {
                    errors.push(SyncError {
                        key,
                        error: "Failed to save".into(),
                    });
                }
            }
        }
    }

    let final_manifest = if updated_keys.is_empty() {
        server_manifest
    } else {
        let now = chrono::Utc::now().timestamp_millis();
        let mut manifest: Vec<DataManifestEntry> = server_manifest
            .into_iter()
            .map(|mut e| {
                if let Some((version, checksum, size)) = updated_keys.remove(&e.key) {
                    e.version = version;
                    e.checksum = checksum;
                    e.size_bytes = size;
                    e.updated_at = now;
                }
                e
            })
            .collect();

        for (key, (version, checksum, size_bytes)) in updated_keys {
            manifest.push(DataManifestEntry {
                key,
                version,
                checksum,
                size_bytes,
                updated_at: now,
            });
        }
        manifest
    };

    Json(SyncResponse {
        server_manifest: final_manifest,
        downloads,
        uploaded,
        errors,
    })
    .into_response()
}
