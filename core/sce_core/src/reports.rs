use crate::{analyzer::Metrics, drift::DriftResult, gates::GateEvaluation};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;
use tokio::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    gate_eval: &GateEvaluation,
    drift: &DriftResult,
    meta: &ReportMeta,
) -> anyhow::Result<()> {
    fs::create_dir_all(run_dir).await?;

    let metrics_path = run_dir.join("metrics.json");
    let gates_path = run_dir.join("gates.json");
    let drift_path = run_dir.join("drift.json");
    let report_json_path = run_dir.join("report.json");
    let report_html_path = run_dir.join("report.html");

    let metrics_payload = json!({
        "meta": meta,
        "metrics": metrics
    });

    fs::write(&metrics_path, serde_json::to_vec_pretty(&metrics_payload)?).await?;

    let gates_payload = json!({
        "meta": meta,
        "gate_status": gate_eval.gate_status,
        "gates": gate_eval.gates,
    });

    fs::write(&gates_path, serde_json::to_vec_pretty(&gates_payload)?).await?;

    let drift_payload = json!({
        "meta": meta,
        "drift_raw": drift.drift_raw,
        "drift_score": drift.drift_score,
        "domain_scores": drift.domain_scores,
        "domain_weights_effective": drift.domain_weights_effective,
        "drift_vector": drift.drift_vector,
        "fix_list": drift.fix_list,
        "notes": drift.notes,
    });

    fs::write(&drift_path, serde_json::to_vec_pretty(&drift_payload)?).await?;

    let top_deltas: Vec<serde_json::Value> = drift
        .drift_vector
        .iter()
        .take(5)
        .map(|delta| {
            json!({
                "metric_id": delta.metric_id,
                "direction": delta.direction,
                "weighted_deviation": delta.weighted_deviation,
            })
        })
        .collect();

    let report = json!({
        "meta": meta,
        "status": "done",
        "summary": {
            "gate_status": gate_eval.gate_status,
            "drift_score": drift.drift_score,
            "top_deltas": top_deltas,
        },
        "metrics": metrics,
        "gates": {
            "gate_status": gate_eval.gate_status,
            "gates": gate_eval.gates,
        },
        "drift": {
            "drift_raw": drift.drift_raw,
            "drift_score": drift.drift_score,
            "domain_scores": drift.domain_scores,
            "domain_weights_effective": drift.domain_weights_effective,
            "drift_vector": drift.drift_vector,
            "fix_list": drift.fix_list,
            "notes": drift.notes,
        }
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
    use crate::{
        analyzer::{BandEnergies, Metrics},
        drift::{
            DomainScores, DomainWeightsEffective, DriftResult, DriftVectorItem, FixSuggestion,
        },
        gates::{GateEvaluation, GateResult, GateSeverity, GateStatus},
    };
    use serde_json::json;

    #[tokio::test]
    async fn report_contains_meta_and_governance_artifacts() {
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
            band_energies_db_rel: BandEnergies {
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

        let gate_eval = GateEvaluation {
            gate_status: GateStatus::Warn,
            gates: vec![GateResult {
                gate_name: "W001_StereoCorrelation".to_string(),
                severity: GateSeverity::Warn,
                pass_fail: false,
                evidence: json!({"configured": true, "value": -0.2, "min": 0.0}),
                rationale: "stereo correlation is outside corridor".to_string(),
            }],
        };

        let drift = DriftResult {
            drift_raw: 0.35,
            drift_score: 35,
            domain_scores: DomainScores {
                loudness: Some(0.2),
                dynamics: Some(0.0),
                spectral_balance: Some(0.5),
                stereo: Some(0.7),
            },
            domain_weights_effective: DomainWeightsEffective {
                loudness: Some(1.0),
                dynamics: Some(1.0),
                spectral_balance: Some(1.2),
                stereo: Some(1.0),
            },
            drift_vector: vec![DriftVectorItem {
                metric_id: "stereo.correlation_min".to_string(),
                deviation: 0.7,
                weighted_deviation: 0.7,
                direction: "too_low".to_string(),
                evidence: json!({"delta": 0.2}),
            }],
            fix_list: vec![FixSuggestion {
                metric_id: "stereo.correlation_min".to_string(),
                direction: "too_low".to_string(),
                delta: 0.2,
                human_action: "Increase mono compatibility".to_string(),
            }],
            notes: vec!["inside test".to_string()],
        };

        write_report(dir.path(), &metrics, &gate_eval, &drift, &meta)
            .await
            .expect("write");

        let report = tokio::fs::read_to_string(dir.path().join("report.json"))
            .await
            .expect("read report");
        let gates = tokio::fs::read_to_string(dir.path().join("gates.json"))
            .await
            .expect("read gates");
        let drift_json = tokio::fs::read_to_string(dir.path().join("drift.json"))
            .await
            .expect("read drift");

        assert!(report.contains("constitution_path"));
        assert!(report.contains("asset_content_hash"));
        assert!(report.contains("\"gate_status\": \"WARN\""));
        assert!(report.contains("\"drift_score\": 35"));
        assert!(gates.contains("W001_StereoCorrelation"));
        assert!(drift_json.contains("stereo.correlation_min"));
    }
}
