use serde::Serialize;

use super::data::DataManifestEntry;

#[derive(Serialize)]
pub struct ManifestResponse {
    pub entries: Vec<DataManifestEntry>,
    pub total_size: i64,
}
