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
            analyzer_version: "TelemetryAnalyzer/1.0.0".into(),
            created_at: "2026-03-05T12:00:00.000Z".into(),
            started_at: "2026-03-05T12:00:00.000Z".into(),
            finished_at: "2026-03-05T12:00:01.000Z".into(),
        };
        let metrics = Metrics {
            analyzer_version: "TelemetryAnalyzer/1.0.0".into(),
            sample_rate_hz: 48_000,
            channels: 2,
            frame_count: 48_000,
            duration_seconds: 1.0,
            sample_peak_linear: 0.5,
            sample_peak_dbfs: -6.020_599_913_279_624,
            clipping_sample_count: 0,
            rms_dbfs: -9.0,
            crest_factor_db: 2.979_400_086_720_376,
            short_term_rms_series_dbfs: vec![-9.0, -9.1],
            approx_true_peak_dbtp: -6.0,
            true_peak_dbtp: -5.9,
            band_energies_db_rel: crate::analyzer::BandEnergies {
                hz_20_60: -30.0,
                hz_60_150: -20.0,
                hz_150_500: -10.0,
                hz_500_2000: -5.0,
                hz_2000_8000: -15.0,
                hz_8000_16000: -25.0,
            },
            spectral_centroid_hz: 1000.0,
            correlation_min: Some(0.9),
            correlation_mean: Some(0.95),
            lr_balance_db: Some(0.0),
            lossy_source: false,
            integrated_lufs: -16.0,
            short_term_lufs_series: vec![-16.0, -15.8],
            tonal_balance_curve: vec![0.0; 30],
            transient_density: 0.25,
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
