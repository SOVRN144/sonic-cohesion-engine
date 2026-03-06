use sce_core::{
    analyzer::{BandEnergies, Metrics},
    cll::generate_project_trends,
    drift::{DomainScores, DriftVectorItem},
    gates::{GateResult, GateSeverity, GateStatus},
    reports::ReportMeta,
};
use serde_json::{json, Value};
use std::path::Path;

#[test]
fn trends_file_contains_expected_gate_counts_and_sorted_series() {
    let td = tempfile::tempdir().expect("tempdir");
    let project_root = td.path().join("project");

    write_run(
        &project_root,
        "asset-1",
        "run-b",
        "2026-03-06T00:00:10.000Z",
        GateStatus::Pass,
        12,
        0.1,
        Some(0.8),
        Some(0.1),
    );
    write_run(
        &project_root,
        "asset-2",
        "run-a",
        "2026-03-06T00:00:10.000Z",
        GateStatus::Warn,
        18,
        -0.1,
        Some(0.6),
        Some(0.2),
    );
    write_run(
        &project_root,
        "asset-3",
        "run-c",
        "2026-03-06T00:00:11.000Z",
        GateStatus::Fail,
        36,
        0.3,
        Some(0.3),
        Some(0.4),
    );

    let summary = generate_project_trends(&project_root, 25).expect("generate trends");
    let trends = read_json(&summary.output_path);

    assert_eq!(trends["gate_trends"]["counts"]["pass"], 1);
    assert_eq!(trends["gate_trends"]["counts"]["warn"], 1);
    assert_eq!(trends["gate_trends"]["counts"]["fail"], 1);

    let series = trends["drift_trends"]["drift_score_series"]
        .as_array()
        .expect("series array");
    assert_eq!(series[0]["run_id"], "run-a");
    assert_eq!(series[1]["run_id"], "run-b");
    assert_eq!(series[2]["run_id"], "run-c");
}

#[test]
fn diversity_diverse_vectors_do_not_emit_warn() {
    let td = tempfile::tempdir().expect("tempdir");
    let project_root = td.path().join("project");

    for idx in 0..10 {
        write_run(
            &project_root,
            "asset-diverse",
            &format!("run-{idx:02}"),
            &format!("2026-03-06T00:00:{idx:02}.000Z"),
            GateStatus::Pass,
            20,
            idx as f64 * 0.45,
            Some(0.2 + idx as f64 * 0.05),
            Some(-0.4 + idx as f64 * 0.08),
        );
    }

    let summary = generate_project_trends(&project_root, 25).expect("generate trends");
    let trends = read_json(&summary.output_path);

    let alerts = trends["diversity_sentinel"]["diversity_alerts"]
        .as_array()
        .expect("alerts array");
    assert!(!alerts
        .iter()
        .any(|alert| alert["level"].as_str() == Some("WARN")));
}

#[test]
fn diversity_collapsed_vectors_emit_warn() {
    let td = tempfile::tempdir().expect("tempdir");
    let project_root = td.path().join("project");

    for idx in 0..10 {
        write_run(
            &project_root,
            "asset-collapsed",
            &format!("run-{idx:02}"),
            &format!("2026-03-06T00:01:{idx:02}.000Z"),
            GateStatus::Pass,
            0,
            0.0,
            Some(0.7),
            Some(0.1),
        );
    }

    let summary = generate_project_trends(&project_root, 25).expect("generate trends");
    let trends = read_json(&summary.output_path);

    let alerts = trends["diversity_sentinel"]["diversity_alerts"]
        .as_array()
        .expect("alerts array");
    assert!(alerts
        .iter()
        .any(|alert| alert["level"].as_str() == Some("WARN")));
}

#[test]
fn diversity_small_n_emits_only_insufficient_history_info() {
    let td = tempfile::tempdir().expect("tempdir");
    let project_root = td.path().join("project");

    for idx in 0..2 {
        write_run(
            &project_root,
            "asset-small-n",
            &format!("run-{idx:02}"),
            &format!("2026-03-06T00:04:{idx:02}.000Z"),
            GateStatus::Pass,
            10,
            idx as f64 * 0.1,
            Some(0.6),
            Some(0.1),
        );
    }

    let summary = generate_project_trends(&project_root, 25).expect("generate trends");
    let trends = read_json(&summary.output_path);
    let alerts = trends["diversity_sentinel"]["diversity_alerts"]
        .as_array()
        .expect("alerts array");

    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0]["level"], "INFO");
    assert_eq!(alerts[0]["type"], "insufficient_history");
    assert_eq!(
        alerts[0]["evidence"]["message"].as_str(),
        Some("insufficient history for diversity inference")
    );
    assert_eq!(alerts[0]["evidence"]["selected_run_count"], 2);
    assert_eq!(alerts[0]["evidence"]["m_used"], 2);
    assert_eq!(alerts[0]["evidence"]["minimum_required"], 5);
    assert!(!alerts
        .iter()
        .any(|alert| alert["type"].as_str() == Some("homogenization_watch")));
    assert!(!alerts
        .iter()
        .any(|alert| alert["level"].as_str() == Some("WARN")));
}

#[test]
fn missing_artifacts_are_tolerated_and_recorded() {
    let td = tempfile::tempdir().expect("tempdir");
    let project_root = td.path().join("project");

    write_run(
        &project_root,
        "asset-good",
        "run-1",
        "2026-03-06T00:02:01.000Z",
        GateStatus::Pass,
        10,
        0.0,
        Some(0.6),
        Some(0.1),
    );

    write_run(
        &project_root,
        "asset-bad",
        "run-2",
        "2026-03-06T00:02:02.000Z",
        GateStatus::Pass,
        10,
        0.0,
        Some(0.6),
        Some(0.1),
    );

    let bad_drift = project_root.join("SCE/assets/asset-bad/analysis/run-2/drift.json");
    std::fs::remove_file(&bad_drift).expect("remove drift");

    let summary = generate_project_trends(&project_root, 25).expect("generate trends");
    let trends = read_json(&summary.output_path);

    assert!(summary.data_gaps);
    assert_eq!(trends["flags"]["skipped_runs"], 1);
    assert!(!trends["flags"]["data_gap_warnings"]
        .as_array()
        .expect("warnings")
        .is_empty());
}

#[test]
fn determinism_differs_only_on_generated_at() {
    let td = tempfile::tempdir().expect("tempdir");
    let project_root = td.path().join("project");

    for idx in 0..5 {
        write_run(
            &project_root,
            "asset-det",
            &format!("run-{idx:02}"),
            &format!("2026-03-06T00:03:{idx:02}.000Z"),
            if idx % 2 == 0 {
                GateStatus::Pass
            } else {
                GateStatus::Warn
            },
            15,
            idx as f64 * 0.1,
            None,
            None,
        );
    }

    let first = generate_project_trends(&project_root, 0).expect("first generate");
    let second = generate_project_trends(&project_root, 0).expect("second generate");

    let mut first_json = read_json(&first.output_path);
    let mut second_json = read_json(&second.output_path);

    first_json["meta"]["generated_at"] = json!("normalized");
    second_json["meta"]["generated_at"] = json!("normalized");

    assert_eq!(first_json, second_json);
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read json")).expect("parse json")
}

#[allow(clippy::too_many_arguments)]
fn write_run(
    project_root: &Path,
    asset_id: &str,
    run_id: &str,
    finished_at: &str,
    gate_status: GateStatus,
    drift_score: u32,
    offset: f64,
    correlation_mean: Option<f64>,
    lr_balance_db: Option<f64>,
) {
    let run_dir = project_root
        .join("SCE")
        .join("assets")
        .join(asset_id)
        .join("analysis")
        .join(run_id);
    std::fs::create_dir_all(&run_dir).expect("mkdir run");

    let meta = ReportMeta {
        project_id: "project-1".to_string(),
        asset_id: asset_id.to_string(),
        run_id: run_id.to_string(),
        asset_content_hash: format!("hash-{run_id}"),
        constitution_version: "1.0".to_string(),
        constitution_hash: "constitution-hash".to_string(),
        constitution_path: project_root
            .join("SCE/constitution.yaml")
            .to_string_lossy()
            .to_string(),
        analyzer_version: "TelemetryAnalyzer/1.0.0".to_string(),
        created_at: finished_at.to_string(),
        started_at: finished_at.to_string(),
        finished_at: finished_at.to_string(),
    };

    let metrics = base_metrics(offset, correlation_mean, lr_balance_db);
    let metrics_payload = json!({"meta": meta, "metrics": metrics});
    std::fs::write(
        run_dir.join("metrics.json"),
        serde_json::to_vec_pretty(&metrics_payload).expect("metrics json"),
    )
    .expect("write metrics");

    let gates_payload = json!({
        "meta": meta,
        "gate_status": gate_status,
        "gates": vec![
            GateResult {
                gate_name: "G003_TruePeakCeiling".to_string(),
                severity: GateSeverity::Blocker,
                pass_fail: gate_status != GateStatus::Fail,
                evidence: json!({"configured": true}),
                rationale: "test blocker".to_string(),
            },
            GateResult {
                gate_name: "W003_SpectralBandOutside".to_string(),
                severity: GateSeverity::Warn,
                pass_fail: gate_status == GateStatus::Pass,
                evidence: json!({"configured": true}),
                rationale: "test warn".to_string(),
            }
        ]
    });

    std::fs::write(
        run_dir.join("gates.json"),
        serde_json::to_vec_pretty(&gates_payload).expect("gates json"),
    )
    .expect("write gates");

    let drift_payload = json!({
        "meta": meta,
        "drift_raw": (drift_score as f64) / 100.0,
        "drift_score": drift_score,
        "domain_scores": DomainScores {
            loudness: Some((drift_score as f64) / 100.0),
            dynamics: Some(0.2 + offset * 0.01),
            spectral_balance: Some(0.3 + offset * 0.01),
            stereo: Some(0.4 + offset * 0.01),
        },
        "domain_weights_effective": {
            "loudness": 1.0,
            "dynamics": 1.0,
            "spectral_balance": 1.0,
            "stereo": 1.0,
        },
        "drift_vector": vec![
            DriftVectorItem {
                metric_id: "spectral.hz_20_60".to_string(),
                deviation: 0.1,
                weighted_deviation: 0.1,
                direction: "band_hot".to_string(),
                evidence: json!({"delta": 0.4}),
            },
            DriftVectorItem {
                metric_id: "dynamics.crest_factor_db".to_string(),
                deviation: 0.08,
                weighted_deviation: 0.08,
                direction: "too_low".to_string(),
                evidence: json!({"delta": 0.2}),
            }
        ],
        "fix_list": Vec::<serde_json::Value>::new(),
        "notes": Vec::<String>::new(),
    });

    std::fs::write(
        run_dir.join("drift.json"),
        serde_json::to_vec_pretty(&drift_payload).expect("drift json"),
    )
    .expect("write drift");
}

fn base_metrics(offset: f64, correlation_mean: Option<f64>, lr_balance_db: Option<f64>) -> Metrics {
    Metrics {
        analyzer_version: "TelemetryAnalyzer/1.0.0".to_string(),
        sample_rate_hz: 48_000,
        channels: if correlation_mean.is_some() { 2 } else { 1 },
        frame_count: 48_000,
        duration_seconds: 1.0,
        sample_peak_linear: 0.6,
        sample_peak_dbfs: -4.0,
        clipping_sample_count: 0,
        rms_dbfs: -10.0,
        crest_factor_db: 8.0 + offset,
        short_term_rms_series_dbfs: vec![-10.0],
        approx_true_peak_dbtp: -2.0,
        true_peak_dbtp: -1.7,
        band_energies_db_rel: BandEnergies {
            hz_20_60: -4.0 + offset,
            hz_60_150: -3.5 + offset,
            hz_150_500: -2.5 + offset,
            hz_500_2000: -1.0 + offset,
            hz_2000_8000: -2.0 + offset,
            hz_8000_16000: -5.0 + offset,
        },
        spectral_centroid_hz: 1100.0 + offset * 60.0,
        correlation_min: correlation_mean,
        correlation_mean,
        lr_balance_db,
        lossy_source: false,
        integrated_lufs: -14.0 + offset,
        short_term_lufs_series: vec![-14.0 + offset],
        tonal_balance_curve: (0..30).map(|idx| offset + idx as f64 * 0.05).collect(),
        transient_density: 0.25 + offset * 0.01,
    }
}
