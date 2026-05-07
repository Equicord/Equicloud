use serde::Serialize;

#[derive(Debug, Clone)]
pub struct DataEntry {
    pub key: String,
    pub value: Vec<u8>,
    pub version: i64,
    pub checksum: String,
    pub size_bytes: i32,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DataManifestEntry {
    pub key: String,
    pub version: i64,
    pub checksum: String,
    pub size_bytes: i32,
    pub updated_at: i64,
}
