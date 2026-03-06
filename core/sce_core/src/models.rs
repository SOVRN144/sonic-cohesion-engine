use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ProjectRow {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub constitution_path: String,
    pub watch_paths_json: String,
    pub created_at: String,
    pub updated_at: String,
}

impl ProjectRow {
    pub fn watch_paths(&self) -> Vec<String> {
        serde_json::from_str(&self.watch_paths_json).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AssetRow {
    pub id: String,
    pub project_id: String,
    pub source_path: String,
    pub file_path: String,
    pub content_hash: String,
    pub kind: String,
    pub tags_json: String,
    pub format: String,
    pub sample_rate: Option<i64>,
    pub bit_depth: Option<i64>,
    pub channels: Option<i64>,
    pub duration_s: Option<f64>,
    pub created_at: String,
}

impl AssetRow {
    pub fn tags(&self) -> Vec<String> {
        serde_json::from_str(&self.tags_json).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AnalysisRunRow {
    pub id: String,
    pub asset_id: String,
    pub status: String,
    pub analyzer_version: String,
    pub constitution_version: String,
    pub queued_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub error_message: Option<String>,
}
