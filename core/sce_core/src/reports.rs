use crate::analyzer::Metrics;
use serde::Serialize;
use serde_json::json;
use std::path::Path;
use tokio::fs;

#[derive(Debug, Clone, Serialize)]
pub struct ReportMeta {
    pub project_id: String,
    pub asset_id: String,
    pub run_id: String,
    pub asset_content_hash: String,
    pub constitution_version: String,
    pub constitution_hash: String,
    pub constitution_path: String,
    pub analyzer_version: String,
    pub created_at: String,
    pub started_at: String,
    pub finished_at: String,
}

pub async fn write_report(
    run_dir: &Path,
    metrics: &Metrics,
    meta: &ReportMeta,
) -> anyhow::Result<()> {
    fs::create_dir_all(run_dir).await?;

    let metrics_path = run_dir.join("metrics.json");
    let report_json_path = run_dir.join("report.json");
    let report_html_path = run_dir.join("report.html");

    let metrics_payload = json!({
        "meta": meta,
        "metrics": metrics
    });

    fs::write(&metrics_path, serde_json::to_vec_pretty(&metrics_payload)?).await?;

    let report = json!({
        "meta": meta,
        "status": "done",
        "summary": {
            "gate_status": "INFO",
            "drift_score": 0,
            "top_deltas": []
        },
        "metrics": metrics
    });

    fs::write(&report_json_path, serde_json::to_vec_pretty(&report)?).await?;

    let html = format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>SCE Report</title></head><body><h1>SCE Report</h1><pre>{}</pre></body></html>",
        serde_json::to_string_pretty(&report)?
    );

    fs::write(&report_html_path, html).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{write_report, ReportMeta};
    use crate::analyzer::Metrics;

    #[tokio::test]
    async fn report_contains_meta() {
        let dir = tempfile::tempdir().expect("tempdir");
        let meta = ReportMeta {
            project_id: "p".into(),
            asset_id: "a".into(),
            run_id: "r".into(),
            asset_content_hash: "h".into(),
            constitution_version: "1.0".into(),
            constitution_hash: "ch".into(),
            constitution_path: "/tmp/c.yaml".into(),
            analyzer_version: "AnalyzerStub/0.0.1".into(),
            created_at: "2026-03-05T12:00:00.000Z".into(),
            started_at: "2026-03-05T12:00:00.000Z".into(),
            finished_at: "2026-03-05T12:00:01.000Z".into(),
        };
        let metrics = Metrics {
            analyzer_version: "AnalyzerStub/0.0.1".into(),
            notes: "n".into(),
            placeholder: true,
        };

        write_report(dir.path(), &metrics, &meta)
            .await
            .expect("write");
        let report = tokio::fs::read_to_string(dir.path().join("report.json"))
            .await
            .expect("read");
        assert!(report.contains("constitution_path"));
        assert!(report.contains("asset_content_hash"));
    }
}
