use sce_core::{
    analyzer::{BandEnergies, Metrics},
    constitution_advisor::suggest_constitution,
    drift::{DomainScores, DriftVectorItem},
    gates::{GateResult, GateSeverity, GateStatus},
    reports::ReportMeta,
};
use serde_json::json;
use serde_yaml::Value;
use std::path::Path;

#[test]
fn suggest_constitution_writes_expected_keys_and_is_byte_stable() {
    let td = tempfile::tempdir().expect("tempdir");
    let project_root = td.path().join("project");
    std::fs::create_dir_all(project_root.join("SCE")).expect("mkdir SCE");
    std::fs::write(
        project_root.join("SCE/constitution.yaml"),
        "constitution_version: \"1.0\"\n",
    )
    .expect("seed constitution");

    for idx in 0..6 {
        let mut metrics = base_metrics(true);
        metrics.integrated_lufs = -14.0 + idx as f64 * 0.3;
        metrics.crest_factor_db = 8.0 + idx as f64 * 0.2;
        metrics.band_energies_db_rel.hz_20_60 = -4.0 + idx as f64 * 0.3;
        metrics.band_energies_db_rel.hz_2000_8000 = -2.0 + idx as f64 * 0.2;
        metrics.correlation_min = Some(0.4 + idx as f64 * 0.03);
        metrics.lr_balance_db = Some(0.2 + idx as f64 * 0.1);
        write_run_with_metrics(
            &project_root,
            "asset-a",
            &format!("run-{idx:02}"),
            &format!("2026-03-06T00:05:{idx:02}.000Z"),
            GateStatus::Pass,
            12,
            metrics,
        );
    }

    let first = suggest_constitution(&project_root, 25, None).expect("suggest first");
    let second_out = project_root.join("SCE/constitution.suggested.copy.yaml");
    let second =
        suggest_constitution(&project_root, 25, Some(&second_out)).expect("suggest second");

    let first_content = std::fs::read_to_string(&first.output_path).expect("read first");
    let second_content = std::fs::read_to_string(&second.output_path).expect("read second");
    assert_eq!(first_content, second_content);
    assert!(first_content.ends_with('\n'));
    assert!(first_content.contains("audio_contract:"));
    assert!(first_content.contains("max_true_peak_dbtp"));
    assert!(first_content.contains("integrated_lufs_range"));
    assert!(first_content.contains("crest_factor_db_range"));
    assert!(first_content.contains("bands_db:"));
    assert!(first_content.contains("hz_20_60"));
    assert!(first_content.contains("hz_8000_16000"));
    assert!(first_content.contains("notes:"));
}

#[test]
fn suggest_constitution_refuses_to_overwrite_constitution_yaml() {
    let td = tempfile::tempdir().expect("tempdir");
    let project_root = td.path().join("project");
    std::fs::create_dir_all(project_root.join("SCE/assets")).expect("mkdir");
    let constitution_path = project_root.join("SCE/constitution.yaml");
    std::fs::write(&constitution_path, "constitution_version: \"1.0\"\n").expect("seed");

    write_run_with_metrics(
        &project_root,
        "asset-a",
        "run-01",
        "2026-03-06T00:06:01.000Z",
        GateStatus::Pass,
        10,
        base_metrics(true),
    );

    let err = suggest_constitution(&project_root, 25, Some(&constitution_path))
        .expect_err("must refuse overwrite");
    assert!(err
        .to_string()
        .contains("refusing to overwrite active constitution"));
}

#[test]
fn suggest_constitution_includes_stereo_only_when_enough_stereo_evidence_exists() {
    let td = tempfile::tempdir().expect("tempdir");
    let project_root = td.path().join("project");
    std::fs::create_dir_all(project_root.join("SCE")).expect("mkdir SCE");
    std::fs::write(
        project_root.join("SCE/constitution.yaml"),
        "constitution_version: \"1.0\"\n",
    )
    .expect("seed constitution");

    for idx in 0..3 {
        write_run_with_metrics(
            &project_root,
            "asset-mono",
            &format!("run-mono-{idx:02}"),
            &format!("2026-03-06T00:07:{idx:02}.000Z"),
            GateStatus::Pass,
            10,
            base_metrics(false),
        );
    }

    let mono_only_out = project_root.join("SCE/constitution.mono.yaml");
    suggest_constitution(&project_root, 0, Some(&mono_only_out)).expect("suggest mono");
    let mono_yaml = std::fs::read_to_string(&mono_only_out).expect("read mono");
    assert!(!mono_yaml.contains("\n  stereo:\n"));

    for idx in 0..2 {
        let mut metrics = base_metrics(true);
        metrics.correlation_min = Some(0.5 + idx as f64 * 0.1);
        metrics.lr_balance_db = Some(0.2 + idx as f64 * 0.1);
        write_run_with_metrics(
            &project_root,
            "asset-stereo",
            &format!("run-stereo-{idx:02}"),
            &format!("2026-03-06T00:08:{idx:02}.000Z"),
            GateStatus::Pass,
            10,
            metrics,
        );
    }

    let stereo_out = project_root.join("SCE/constitution.stereo.yaml");
    suggest_constitution(&project_root, 0, Some(&stereo_out)).expect("suggest stereo");
    let stereo_yaml = std::fs::read_to_string(&stereo_out).expect("read stereo");
    assert!(stereo_yaml.contains("\n  stereo:\n"));
    assert!(stereo_yaml.contains("correlation_min"));
    assert!(stereo_yaml.contains("lr_balance_db_max_abs"));
}

#[test]
fn suggest_constitution_widens_corridors_when_diversity_warns() {
    let td = tempfile::tempdir().expect("tempdir");

    let stable_root = td.path().join("stable");
    std::fs::create_dir_all(stable_root.join("SCE")).expect("mkdir stable SCE");
    std::fs::write(
        stable_root.join("SCE/constitution.yaml"),
        "constitution_version: \"1.0\"\n",
    )
    .expect("seed stable constitution");

    for idx in 0..10 {
        let mut metrics = base_metrics(true);
        metrics.integrated_lufs = -14.0;
        metrics.crest_factor_db = 8.0;
        write_run_with_metrics(
            &stable_root,
            "asset-stable",
            &format!("run-{idx:02}"),
            &format!("2026-03-06T00:09:{idx:02}.000Z"),
            GateStatus::Pass,
            10,
            metrics,
        );
    }

    let stable_out = stable_root.join("SCE/stable.suggested.yaml");
    suggest_constitution(&stable_root, 25, Some(&stable_out)).expect("stable suggest");
    let stable_yaml = std::fs::read_to_string(&stable_out).expect("read stable");
    let stable_width = integrated_lufs_width(&stable_yaml);

    let warn_root = td.path().join("warn");
    std::fs::create_dir_all(warn_root.join("SCE")).expect("mkdir warn SCE");
    std::fs::write(
        warn_root.join("SCE/constitution.yaml"),
        "constitution_version: \"1.0\"\n",
    )
    .expect("seed warn constitution");

    for idx in 0..10 {
        let mut metrics = base_metrics(true);
        metrics.integrated_lufs = -14.0;
        metrics.crest_factor_db = 8.0;
        write_run_with_metrics(
            &warn_root,
            "asset-warn",
            &format!("run-{idx:02}"),
            &format!("2026-03-06T00:10:{idx:02}.000Z"),
            GateStatus::Pass,
            0,
            metrics,
        );
    }

    let warn_out = warn_root.join("SCE/warn.suggested.yaml");
    suggest_constitution(&warn_root, 25, Some(&warn_out)).expect("warn suggest");
    let warn_yaml = std::fs::read_to_string(&warn_out).expect("read warn");
    let warn_width = integrated_lufs_width(&warn_yaml);

    assert!(warn_width > stable_width);
    assert!(warn_yaml.contains("corridors were widened by an additional 20%"));
}

#[test]
fn suggest_constitution_clamps_stereo_and_true_peak_values() {
    let td = tempfile::tempdir().expect("tempdir");
    let project_root = td.path().join("project");
    std::fs::create_dir_all(project_root.join("SCE")).expect("mkdir SCE");
    std::fs::write(
        project_root.join("SCE/constitution.yaml"),
        "constitution_version: \"1.0\"\n",
    )
    .expect("seed constitution");

    for idx in 0..3 {
        let mut metrics = base_metrics(true);
        metrics.correlation_min = Some(1.5 + idx as f64 * 0.1);
        metrics.lr_balance_db = Some(9.0 + idx as f64);
        write_run_with_metrics(
            &project_root,
            "asset-a",
            &format!("run-{idx:02}"),
            &format!("2026-03-06T00:11:{idx:02}.000Z"),
            GateStatus::Pass,
            10,
            metrics,
        );
    }

    let out_path = project_root.join("SCE/clamped.suggested.yaml");
    suggest_constitution(&project_root, 25, Some(&out_path)).expect("suggest");
    let yaml = std::fs::read_to_string(out_path).expect("read");
    let parsed: Value = serde_yaml::from_str(&yaml).expect("parse yaml");

    let max_true_peak = parsed["audio_contract"]["max_true_peak_dbtp"]
        .as_f64()
        .expect("max true peak");
    assert!(max_true_peak <= 0.0);

    let correlation_min = parsed["targets"]["stereo"]["correlation_min"]
        .as_f64()
        .expect("corr");
    assert!((-1.0..=1.0).contains(&correlation_min));

    let lr_balance_max_abs = parsed["targets"]["stereo"]["lr_balance_db_max_abs"]
        .as_f64()
        .expect("lr balance");
    assert!((0.0..=6.0).contains(&lr_balance_max_abs));
}

fn integrated_lufs_width(yaml_text: &str) -> f64 {
    let parsed: Value = serde_yaml::from_str(yaml_text).expect("parse yaml");
    let range = parsed["targets"]["loudness"]["integrated_lufs_range"]
        .as_sequence()
        .expect("range");
    let min = range[0].as_f64().expect("min");
    let max = range[1].as_f64().expect("max");
    max - min
}

fn write_run_with_metrics(
    project_root: &Path,
    asset_id: &str,
    run_id: &str,
    finished_at: &str,
    gate_status: GateStatus,
    drift_score: u32,
    metrics: Metrics,
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

    let metrics_payload = json!({
        "meta": meta,
        "metrics": metrics,
    });
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
            dynamics: Some(0.2),
            spectral_balance: Some(0.3),
            stereo: Some(0.4),
        },
        "domain_weights_effective": {
            "loudness": 1.0,
            "dynamics": 1.0,
            "spectral_balance": 1.0,
            "stereo": 1.0,
        },
        "drift_vector": vec![DriftVectorItem {
            metric_id: "spectral.hz_20_60".to_string(),
            deviation: 0.1,
            weighted_deviation: 0.1,
            direction: "band_hot".to_string(),
            evidence: json!({"delta": 0.4}),
        }],
        "fix_list": Vec::<serde_json::Value>::new(),
        "notes": Vec::<String>::new(),
    });
    std::fs::write(
        run_dir.join("drift.json"),
        serde_json::to_vec_pretty(&drift_payload).expect("drift json"),
    )
    .expect("write drift");
}

fn base_metrics(stereo: bool) -> Metrics {
    Metrics {
        analyzer_version: "TelemetryAnalyzer/1.0.0".to_string(),
        sample_rate_hz: 48_000,
        channels: if stereo { 2 } else { 1 },
        frame_count: 48_000,
        duration_seconds: 1.0,
        sample_peak_linear: 0.6,
        sample_peak_dbfs: -4.0,
        clipping_sample_count: 0,
        rms_dbfs: -10.0,
        crest_factor_db: 8.0,
        short_term_rms_series_dbfs: vec![-10.0],
        approx_true_peak_dbtp: -2.0,
        true_peak_dbtp: -1.7,
        band_energies_db_rel: BandEnergies {
            hz_20_60: -4.0,
            hz_60_150: -3.5,
            hz_150_500: -2.5,
            hz_500_2000: -1.0,
            hz_2000_8000: -2.0,
            hz_8000_16000: -5.0,
        },
        spectral_centroid_hz: 1100.0,
        correlation_min: if stereo { Some(0.7) } else { None },
        correlation_mean: if stereo { Some(0.8) } else { None },
        lr_balance_db: if stereo { Some(0.1) } else { None },
        lossy_source: false,
        integrated_lufs: -14.0,
        short_term_lufs_series: vec![-14.0],
        tonal_balance_curve: vec![0.0; 30],
        transient_density: 0.25,
    }
}
