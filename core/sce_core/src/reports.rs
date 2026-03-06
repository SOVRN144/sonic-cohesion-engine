use crate::{analyzer::Metrics, drift::DriftResult, gates::GateEvaluation};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{fmt::Write as _, path::Path};
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

    let trends = load_trends_snippet(meta).await;
    let html = build_report_html(&report, gate_eval, drift, trends);

    fs::write(&report_html_path, html).await?;
    Ok(())
}

#[derive(Debug, Clone)]
struct TrendsSnippet {
    pass_count: u64,
    warn_count: u64,
    fail_count: u64,
    last_five_drift_scores: Vec<u32>,
    diversity_warn_alerts: Vec<String>,
}

async fn load_trends_snippet(meta: &ReportMeta) -> Option<TrendsSnippet> {
    let constitution_path = Path::new(&meta.constitution_path);
    let sce_dir = constitution_path.parent()?;
    let trends_path = sce_dir.join("trends").join("project_trends.json");
    let content = fs::read_to_string(&trends_path).await.ok()?;
    let value: Value = serde_json::from_str(&content).ok()?;

    let pass_count = value["gate_trends"]["counts"]["pass"].as_u64().unwrap_or(0);
    let warn_count = value["gate_trends"]["counts"]["warn"].as_u64().unwrap_or(0);
    let fail_count = value["gate_trends"]["counts"]["fail"].as_u64().unwrap_or(0);

    let mut drift_scores = value["drift_trends"]["drift_score_series"]
        .as_array()
        .map(|series| {
            series
                .iter()
                .filter_map(|entry| entry["drift_score"].as_u64().map(|score| score as u32))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if drift_scores.len() > 5 {
        drift_scores = drift_scores[drift_scores.len() - 5..].to_vec();
    }

    let diversity_warn_alerts = value["diversity_sentinel"]["diversity_alerts"]
        .as_array()
        .map(|alerts| {
            alerts
                .iter()
                .filter(|alert| alert["level"].as_str() == Some("WARN"))
                .map(|alert| {
                    let alert_type = alert["type"].as_str().unwrap_or("unknown");
                    let message = alert["evidence"]["message"]
                        .as_str()
                        .unwrap_or("no message");
                    format!("{alert_type}: {message}")
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(TrendsSnippet {
        pass_count,
        warn_count,
        fail_count,
        last_five_drift_scores: drift_scores,
        diversity_warn_alerts,
    })
}

fn build_report_html(
    report: &Value,
    gate_eval: &GateEvaluation,
    drift: &DriftResult,
    trends: Option<TrendsSnippet>,
) -> String {
    let mut html = String::new();
    html.push_str("<!doctype html>\n");
    html.push_str("<html><head><meta charset=\"utf-8\"><title>SCE Report</title>");
    html.push_str(
        "<style>body{font-family:-apple-system,BlinkMacSystemFont,\"Segoe UI\",sans-serif;margin:24px;color:#111}h1,h2{margin:0 0 12px}section{margin:20px 0;padding:12px;border:1px solid #ddd;border-radius:8px}.muted{color:#666}.pill{display:inline-block;padding:2px 8px;border-radius:999px;background:#f1f1f1}ul,ol{margin:8px 0 0 20px}pre{white-space:pre-wrap;word-break:break-word;background:#fafafa;border:1px solid #eee;border-radius:8px;padding:12px}</style>",
    );
    html.push_str("</head><body>");
    html.push_str("<h1>SCE Report</h1>");

    let gate_status = report["summary"]["gate_status"]
        .as_str()
        .unwrap_or("UNKNOWN");
    let drift_score = report["summary"]["drift_score"].as_u64().unwrap_or(0);
    write!(
        &mut html,
        "<section><h2>Summary</h2><p><span class=\"pill\">Gate status: {}</span></p><p><span class=\"pill\">Drift score: {}</span></p></section>",
        escape_html(gate_status),
        drift_score
    )
    .expect("write html");

    let failed_gates = gate_eval
        .gates
        .iter()
        .filter(|gate| !gate.pass_fail)
        .collect::<Vec<_>>();
    html.push_str("<section><h2>Failed Gates</h2>");
    if failed_gates.is_empty() {
        html.push_str("<p class=\"muted\">No failed gates.</p>");
    } else {
        html.push_str("<ul>");
        for gate in failed_gates {
            write!(
                &mut html,
                "<li><strong>{}</strong> [{}] - {}</li>",
                escape_html(&gate.gate_name),
                escape_html(&format!("{:?}", gate.severity)),
                escape_html(&gate.rationale)
            )
            .expect("write html");
        }
        html.push_str("</ul>");
    }
    html.push_str("</section>");

    html.push_str("<section><h2>Drift Deltas</h2>");
    if drift.drift_vector.is_empty() {
        html.push_str("<p class=\"muted\">No drift deltas recorded.</p>");
    } else {
        html.push_str("<ol>");
        for delta in drift.drift_vector.iter().take(5) {
            write!(
                &mut html,
                "<li>{} ({}, weighted {:.3})</li>",
                escape_html(&delta.metric_id),
                escape_html(&delta.direction),
                delta.weighted_deviation
            )
            .expect("write html");
        }
        html.push_str("</ol>");
    }
    html.push_str("</section>");

    html.push_str("<section><h2>Fix List</h2>");
    if drift.fix_list.is_empty() {
        html.push_str("<p class=\"muted\">No fix suggestions generated.</p>");
    } else {
        html.push_str("<ol>");
        for suggestion in &drift.fix_list {
            write!(
                &mut html,
                "<li>{}: {} (delta {:.3})</li>",
                escape_html(&suggestion.metric_id),
                escape_html(&suggestion.human_action),
                suggestion.delta
            )
            .expect("write html");
        }
        html.push_str("</ol>");
    }
    html.push_str("</section>");

    html.push_str("<section><h2>Trends</h2>");
    match trends {
        Some(snippet) => {
            write!(
                &mut html,
                "<p><span class=\"pill\">PASS/WARN/FAIL: {}/{}/{}</span></p>",
                snippet.pass_count, snippet.warn_count, snippet.fail_count
            )
            .expect("write html");
            if snippet.last_five_drift_scores.is_empty() {
                html.push_str("<p class=\"muted\">No drift score history available.</p>");
            } else {
                let joined = snippet
                    .last_five_drift_scores
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(
                    &mut html,
                    "<p>Last 5 drift scores: {}</p>",
                    escape_html(&joined)
                )
                .expect("write html");
            }

            if snippet.diversity_warn_alerts.is_empty() {
                html.push_str("<p class=\"muted\">No diversity WARN alerts.</p>");
            } else {
                html.push_str("<ul>");
                for alert in snippet.diversity_warn_alerts {
                    write!(&mut html, "<li>{}</li>", escape_html(&alert)).expect("write html");
                }
                html.push_str("</ul>");
            }
        }
        None => {
            html.push_str("<p class=\"muted\">Trends unavailable</p>");
        }
    }
    html.push_str("</section>");

    html.push_str("<details><summary>Raw report.json</summary><pre>");
    html.push_str(&escape_html(
        &serde_json::to_string_pretty(report).unwrap_or_else(|_| "{}".to_string()),
    ));
    html.push_str("</pre></details>");
    html.push_str("</body></html>");
    html
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
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
        let report_html = tokio::fs::read_to_string(dir.path().join("report.html"))
            .await
            .expect("read html");

        assert!(report.contains("constitution_path"));
        assert!(report.contains("asset_content_hash"));
        assert!(report.contains("\"gate_status\": \"WARN\""));
        assert!(report.contains("\"drift_score\": 35"));
        assert!(gates.contains("W001_StereoCorrelation"));
        assert!(drift_json.contains("stereo.correlation_min"));
        assert!(report_html.contains("Gate status"));
        assert!(report_html.contains("Drift score"));
        assert!(report_html.contains("Trends unavailable"));
    }

    #[tokio::test]
    async fn report_html_includes_trends_snippet_when_available() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project_root = dir.path().join("project");
        let run_dir = project_root
            .join("SCE")
            .join("assets")
            .join("a")
            .join("analysis")
            .join("r");
        tokio::fs::create_dir_all(&run_dir)
            .await
            .expect("mkdir run");

        let trends_dir = project_root.join("SCE").join("trends");
        tokio::fs::create_dir_all(&trends_dir)
            .await
            .expect("mkdir trends");
        let trends_payload = serde_json::json!({
            "gate_trends": {
                "counts": {"pass": 3, "warn": 2, "fail": 1}
            },
            "drift_trends": {
                "drift_score_series": [
                    {"drift_score": 10},
                    {"drift_score": 20},
                    {"drift_score": 30},
                    {"drift_score": 40},
                    {"drift_score": 50},
                    {"drift_score": 60}
                ]
            },
            "diversity_sentinel": {
                "diversity_alerts": [
                    {
                        "level": "WARN",
                        "type": "homogenization_risk",
                        "evidence": {"message": "watch"}
                    }
                ]
            }
        });
        tokio::fs::write(
            trends_dir.join("project_trends.json"),
            serde_json::to_vec_pretty(&trends_payload).expect("serialize trends"),
        )
        .await
        .expect("write trends");

        let meta = ReportMeta {
            project_id: "p".into(),
            asset_id: "a".into(),
            run_id: "r".into(),
            asset_content_hash: "h".into(),
            constitution_version: "1.0".into(),
            constitution_hash: "ch".into(),
            constitution_path: project_root
                .join("SCE")
                .join("constitution.yaml")
                .to_string_lossy()
                .to_string(),
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
            sample_peak_dbfs: -6.0,
            clipping_sample_count: 0,
            rms_dbfs: -9.0,
            crest_factor_db: 3.0,
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

        write_report(&run_dir, &metrics, &gate_eval, &drift, &meta)
            .await
            .expect("write");

        let report_html = tokio::fs::read_to_string(run_dir.join("report.html"))
            .await
            .expect("read html");
        assert!(report_html.contains("PASS/WARN/FAIL: 3/2/1"));
        assert!(report_html.contains("Last 5 drift scores: 20, 30, 40, 50, 60"));
        assert!(report_html.contains("homogenization_risk: watch"));
    }
}
