use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metrics {
    pub analyzer_version: String,
    pub notes: String,
    pub placeholder: bool,
}

pub async fn analyze_stub(_path: &Path, analyzer_version: &str) -> Metrics {
    Metrics {
        analyzer_version: analyzer_version.to_string(),
        notes: "AnalyzerStub v0: telemetry not yet implemented".to_string(),
        placeholder: true,
    }
}
