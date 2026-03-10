use serde::{Deserialize, Serialize};

use super::data::DataManifestEntry;

#[derive(Deserialize)]
pub struct SyncRequest {
    pub client_manifest: Vec<ClientManifestEntry>,
    #[serde(default)]
    pub uploads: Vec<UploadEntry>,
}

#[derive(Deserialize)]
pub struct ClientManifestEntry {
    pub key: String,
    pub version: i64,
    pub checksum: String,
}

#[derive(Deserialize)]
pub struct UploadEntry {
    pub key: String,
    #[serde(with = "base64_serde")]
    pub value: Vec<u8>,
    #[serde(default)]
    pub checksum: Option<String>,
}

pub mod base64_serde {
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
    pub server_manifest: Vec<DataManifestEntry>,
    pub downloads: Vec<DownloadEntry>,
    pub uploaded: Vec<UploadResult>,
    pub errors: Vec<SyncError>,
}

#[derive(Serialize)]
pub struct DownloadEntry {
    pub key: String,
    #[serde(with = "base64_serde")]
    pub value: Vec<u8>,
    pub version: i64,
    pub checksum: String,
}

#[derive(Serialize)]
pub struct UploadResult {
    pub key: String,
    pub version: i64,
    pub checksum: String,
}

#[derive(Serialize)]
pub struct SyncError {
    pub key: String,
    pub error: String,
}
