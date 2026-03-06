use crate::{
    analyzer::{self, BandEnergies, Metrics},
    cll, drift,
    gates::GateStatus,
    paths, policy_registry, translation_sim,
};
use anyhow::{anyhow, bail, Context};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

const MONO_NOT_APPLICABLE_NOTE: &str =
    "mono target; stereo correlation and L/R balance are not applicable";
const SCHEMA_VERSION: &str = "1.0";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranslationMatrixOutput {
    pub schema_version: String,
    pub project_root: String,
    pub run_id: String,
    pub source_constitution_version: String,
    pub source_constitution_hash: String,
    pub targets: Vec<TranslationTargetOutput>,
    pub summary: TranslationSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranslationTargetOutput {
    pub target_id: String,
    pub gate_status: GateStatus,
    pub drift_score: u32,
    pub translation_risk: u32,
    pub failed_blockers: u32,
    pub failed_warns: u32,
    pub top_deltas: Vec<TranslationTopDelta>,
    pub telemetry: TranslationTelemetry,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranslationTopDelta {
    pub metric_id: String,
    pub direction: String,
    pub weighted_deviation: f64,
    pub delta: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranslationTelemetry {
    pub channels: u16,
    pub integrated_lufs: f64,
    pub true_peak_dbtp: f64,
    pub approx_true_peak_dbtp: f64,
    pub crest_factor_db: f64,
    pub spectral_centroid_hz: f64,
    pub band_energies_db_rel: TranslationBandEnergies,
    pub correlation_min: Option<f64>,
    pub correlation_mean: Option<f64>,
    pub lr_balance_db: Option<f64>,
    pub transient_density: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranslationBandEnergies {
    pub hz_20_60: f64,
    pub hz_60_150: f64,
    pub hz_150_500: f64,
    pub hz_500_2000: f64,
    pub hz_2000_8000: f64,
    pub hz_8000_16000: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranslationSummary {
    pub worst_target: String,
    pub best_target: String,
    pub average_translation_risk: f64,
    pub top_failing_targets: Vec<TranslationFailingTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranslationFailingTarget {
    pub target_id: String,
    pub gate_status: GateStatus,
    pub translation_risk: u32,
}

pub fn run(
    project_root: &Path,
    run_id: &str,
) -> anyhow::Result<(TranslationMatrixOutput, PathBuf)> {
    let canonical_root = policy_registry::canonical_project_root(project_root)?;
    let candidate = cll::find_candidate_run(&canonical_root, run_id)?
        .ok_or_else(|| anyhow!("run not found: {run_id}"))?;
    let valid_run = cll::read_valid_run(&candidate)
        .with_context(|| format!("load valid run for translation matrix: {run_id}"))?;
    let source_path = resolve_internal_source_audio(&canonical_root, &candidate.asset_id)?;
    let resolved_constitution = policy_registry::resolve_worker_constitution(
        &canonical_root,
        Path::new(&valid_run.constitution_path),
        &valid_run.constitution_version,
    )?;
    let decoded = analyzer::decode_audio(&source_path).with_context(|| {
        format!(
            "decode translation matrix source: {}",
            source_path.display()
        )
    })?;
    let raw_results = translation_sim::simulate_targets(
        &decoded,
        &valid_run.analyzer_version,
        valid_run.metrics.lossy_source,
        &resolved_constitution.constitution,
    )?;
    let output = project_output(
        &canonical_root,
        &valid_run.run_id,
        &resolved_constitution.constitution_version,
        &resolved_constitution.constitution_hash,
        &raw_results,
    );

    let output_path =
        paths::translation_matrix_operator_output_path(&canonical_root, &valid_run.run_id);
    // Projection structs are the only serializable boundary for Translation Matrix output; raw
    // simulation results must not be serialized directly.
    policy_registry::write_atomic_json(&output_path, &output)
        .with_context(|| format!("write translation matrix output: {}", output_path.display()))?;
    Ok((output, output_path))
}

fn resolve_internal_source_audio(project_root: &Path, asset_id: &str) -> anyhow::Result<PathBuf> {
    let asset_dir = paths::asset_dir(project_root, asset_id);
    let mut matches = Vec::new();

    if asset_dir.exists() {
        for entry in fs::read_dir(&asset_dir)
            .with_context(|| format!("read asset directory: {}", asset_dir.display()))?
        {
            let entry =
                entry.with_context(|| format!("read asset entry: {}", asset_dir.display()))?;
            if !entry
                .file_type()
                .with_context(|| format!("read asset file type: {}", entry.path().display()))?
                .is_file()
            {
                continue;
            }

            let file_name = entry.file_name().to_string_lossy().to_string();
            if file_name.starts_with("original.") {
                matches.push(file_name);
            }
        }
    }

    matches.sort();

    match matches.as_slice() {
        [file_name] => Ok(asset_dir.join(file_name)),
        [] => bail!(
            "expected exactly one original.* in {}; found 0",
            asset_dir.display()
        ),
        _ => bail!(
            "expected exactly one original.* in {}; found multiple: {}",
            asset_dir.display(),
            matches.join(", ")
        ),
    }
}

fn project_output(
    canonical_root: &Path,
    run_id: &str,
    constitution_version: &str,
    constitution_hash: &str,
    raw_results: &[translation_sim::RawTargetResult],
) -> TranslationMatrixOutput {
    let targets = raw_results
        .iter()
        .map(project_target_output)
        .collect::<Vec<_>>();
    let summary = build_summary(&targets);

    TranslationMatrixOutput {
        schema_version: SCHEMA_VERSION.to_string(),
        project_root: canonical_root.to_string_lossy().to_string(),
        run_id: run_id.to_string(),
        source_constitution_version: constitution_version.to_string(),
        source_constitution_hash: constitution_hash.to_string(),
        targets,
        summary,
    }
}

fn project_target_output(raw: &translation_sim::RawTargetResult) -> TranslationTargetOutput {
    TranslationTargetOutput {
        target_id: raw.target_id.to_string(),
        gate_status: raw.gate_evaluation.gate_status,
        drift_score: raw.drift_result.drift_score,
        translation_risk: raw.translation_risk,
        failed_blockers: raw.failed_blockers,
        failed_warns: raw.failed_warns,
        top_deltas: raw
            .drift_result
            .drift_vector
            .iter()
            .take(5)
            .map(project_top_delta)
            .collect(),
        telemetry: project_telemetry(&raw.metrics),
        notes: build_notes(raw),
    }
}

fn project_top_delta(item: &drift::DriftVectorItem) -> TranslationTopDelta {
    let delta = item.evidence["delta"].as_f64().unwrap_or(item.deviation);

    TranslationTopDelta {
        metric_id: item.metric_id.clone(),
        direction: item.direction.clone(),
        weighted_deviation: round6(item.weighted_deviation),
        delta: round6(delta),
    }
}

fn project_telemetry(metrics: &Metrics) -> TranslationTelemetry {
    TranslationTelemetry {
        channels: metrics.channels,
        integrated_lufs: round6(metrics.integrated_lufs),
        true_peak_dbtp: round6(metrics.true_peak_dbtp),
        approx_true_peak_dbtp: round6(metrics.approx_true_peak_dbtp),
        crest_factor_db: round6(metrics.crest_factor_db),
        spectral_centroid_hz: round6(metrics.spectral_centroid_hz),
        band_energies_db_rel: project_band_energies(&metrics.band_energies_db_rel),
        correlation_min: round_opt(metrics.correlation_min),
        correlation_mean: round_opt(metrics.correlation_mean),
        lr_balance_db: round_opt(metrics.lr_balance_db),
        transient_density: round6(metrics.transient_density),
    }
}

fn project_band_energies(bands: &BandEnergies) -> TranslationBandEnergies {
    TranslationBandEnergies {
        hz_20_60: round6(bands.hz_20_60),
        hz_60_150: round6(bands.hz_60_150),
        hz_150_500: round6(bands.hz_150_500),
        hz_500_2000: round6(bands.hz_500_2000),
        hz_2000_8000: round6(bands.hz_2000_8000),
        hz_8000_16000: round6(bands.hz_8000_16000),
    }
}

fn build_notes(raw: &translation_sim::RawTargetResult) -> Vec<String> {
    let mut notes = vec![format!("preset={}", raw.target_id)];
    if raw.is_mono {
        notes.push(MONO_NOT_APPLICABLE_NOTE.to_string());
    }
    notes.extend(raw.drift_result.notes.iter().cloned());
    notes
}

fn build_summary(targets: &[TranslationTargetOutput]) -> TranslationSummary {
    let mut worst_target = String::new();
    let mut worst_risk = 0u32;
    let mut best_target = String::new();
    let mut best_risk = u32::MAX;

    for target in targets {
        if worst_target.is_empty() || target.translation_risk > worst_risk {
            worst_target = target.target_id.clone();
            worst_risk = target.translation_risk;
        }

        if best_target.is_empty() || target.translation_risk < best_risk {
            best_target = target.target_id.clone();
            best_risk = target.translation_risk;
        }
    }

    let average_translation_risk = if targets.is_empty() {
        0.0
    } else {
        round6(
            targets
                .iter()
                .map(|target| target.translation_risk as f64)
                .sum::<f64>()
                / targets.len() as f64,
        )
    };

    let mut failing = targets
        .iter()
        .enumerate()
        .filter(|(_, target)| target.gate_status != GateStatus::Pass)
        .collect::<Vec<_>>();

    failing.sort_by(|(left_index, left), (right_index, right)| {
        gate_rank(right.gate_status)
            .cmp(&gate_rank(left.gate_status))
            .then_with(|| right.translation_risk.cmp(&left.translation_risk))
            .then_with(|| left_index.cmp(right_index))
    });

    let top_failing_targets = failing
        .into_iter()
        .take(3)
        .map(|(_, target)| TranslationFailingTarget {
            target_id: target.target_id.clone(),
            gate_status: target.gate_status,
            translation_risk: target.translation_risk,
        })
        .collect();

    TranslationSummary {
        worst_target,
        best_target,
        average_translation_risk,
        top_failing_targets,
    }
}

fn gate_rank(status: GateStatus) -> u8 {
    match status {
        GateStatus::Fail => 2,
        GateStatus::Warn => 1,
        GateStatus::Pass => 0,
    }
}

fn round6(value: f64) -> f64 {
    policy_registry::round6(value)
}

fn round_opt(value: Option<f64>) -> Option<f64> {
    value.map(round6)
}

#[cfg(test)]
mod tests {
    use super::{
        build_summary, project_output, project_target_output, resolve_internal_source_audio,
        TranslationTargetOutput,
    };
    use crate::{
        analyzer::{BandEnergies, Metrics},
        drift::{
            DomainScores, DomainWeightsEffective, DriftResult, DriftVectorItem, FixSuggestion,
        },
        gates::{GateEvaluation, GateResult, GateSeverity, GateStatus},
        paths, policy_registry,
        reports::ReportMeta,
        util,
    };
    use serde_json::{json, Value};
    use std::{
        io::Write,
        path::{Path, PathBuf},
    };

    #[test]
    fn translation_matrix_replaces_scaffold_with_locked_schema() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        let source_hash = setup_project(&project_root, "asset-a", "run-1", true);

        let (output, path) = super::run(&project_root, "run-1").expect("translation");
        let written = std::fs::read_to_string(&path).expect("read output");
        let parsed: Value = serde_json::from_str(&written).expect("parse");
        let canonical_root = std::fs::canonicalize(&project_root).expect("canonical root");

        assert_eq!(output.schema_version, "1.0");
        assert_eq!(output.project_root, canonical_root.to_string_lossy());
        assert_eq!(output.run_id, "run-1");
        assert_eq!(output.source_constitution_version, "v1.0");
        assert_eq!(output.source_constitution_hash, source_hash);
        assert_eq!(
            output
                .targets
                .iter()
                .map(|target| target.target_id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "mono_phone",
                "laptop_speakers",
                "earbuds",
                "car",
                "club_pa",
                "mono_compat"
            ]
        );
        assert!(parsed.get("not_implemented").is_none());
        assert!(!written.contains("\"ableton_live\""));
        assert!(!written.contains("\"logic_pro\""));
        assert!(!written.contains("\"pro_tools\""));
        assert!(!written.contains("created_at"));
        assert!(!written.contains("started_at"));
        assert!(!written.contains("finished_at"));
        assert!(!written.contains("generated_at"));
        assert_field_order(
            &written,
            &[
                "\"schema_version\"",
                "\"project_root\"",
                "\"run_id\"",
                "\"source_constitution_version\"",
                "\"source_constitution_hash\"",
                "\"targets\"",
                "\"summary\"",
            ],
        );
        let first_target = first_object_after(&written, "\"targets\": [");
        assert_field_order(
            first_target,
            &[
                "\"target_id\"",
                "\"gate_status\"",
                "\"drift_score\"",
                "\"translation_risk\"",
                "\"failed_blockers\"",
                "\"failed_warns\"",
                "\"top_deltas\"",
                "\"telemetry\"",
                "\"notes\"",
            ],
        );
        let first_telemetry = first_object_after(first_target, "\"telemetry\": ");
        assert_field_order(
            first_telemetry,
            &[
                "\"channels\"",
                "\"integrated_lufs\"",
                "\"true_peak_dbtp\"",
                "\"approx_true_peak_dbtp\"",
                "\"crest_factor_db\"",
                "\"spectral_centroid_hz\"",
                "\"band_energies_db_rel\"",
                "\"correlation_min\"",
                "\"correlation_mean\"",
                "\"lr_balance_db\"",
                "\"transient_density\"",
            ],
        );
    }

    #[test]
    fn mono_targets_emit_null_stereo_telemetry() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        setup_project(&project_root, "asset-a", "run-1", true);

        let (output, _path) = super::run(&project_root, "run-1").expect("translation");
        let mono_phone = &output.targets[0];
        let mono_compat = &output.targets[5];

        assert_eq!(mono_phone.telemetry.channels, 1);
        assert_eq!(mono_phone.telemetry.correlation_min, None);
        assert_eq!(mono_phone.telemetry.correlation_mean, None);
        assert_eq!(mono_phone.telemetry.lr_balance_db, None);
        assert!(mono_phone
            .notes
            .iter()
            .any(|note| note == "preset=mono_phone"));
        assert!(mono_phone
            .notes
            .iter()
            .any(|note| note.contains("mono target")));

        assert_eq!(mono_compat.telemetry.channels, 1);
        assert_eq!(mono_compat.telemetry.correlation_min, None);
        assert_eq!(mono_compat.telemetry.correlation_mean, None);
        assert_eq!(mono_compat.telemetry.lr_balance_db, None);
    }

    #[test]
    fn translation_matrix_output_is_byte_stable() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        setup_project(&project_root, "asset-a", "run-1", true);

        let (_first, path) = super::run(&project_root, "run-1").expect("first");
        let first_bytes = std::fs::read(&path).expect("read first");

        let (_second, _path) = super::run(&project_root, "run-1").expect("second");
        let second_bytes = std::fs::read(&path).expect("read second");

        assert_eq!(first_bytes, second_bytes);
    }

    #[test]
    fn translation_matrix_does_not_mutate_existing_artifacts() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        setup_project(&project_root, "asset-a", "run-1", true);

        let run_dir = paths::run_dir(&project_root, "asset-a", "run-1");
        let report_path = run_dir.join("report.json");
        let metrics_path = run_dir.join("metrics.json");
        let gates_path = run_dir.join("gates.json");
        let drift_path = run_dir.join("drift.json");
        let trends_path = project_root.join("SCE/trends/project_trends.json");

        let report_before = std::fs::read(&report_path).expect("report before");
        let metrics_before = std::fs::read(&metrics_path).expect("metrics before");
        let gates_before = std::fs::read(&gates_path).expect("gates before");
        let drift_before = std::fs::read(&drift_path).expect("drift before");
        let trends_before = std::fs::read(&trends_path).expect("trends before");

        super::run(&project_root, "run-1").expect("translation");

        assert_eq!(
            report_before,
            std::fs::read(&report_path).expect("report after")
        );
        assert_eq!(
            metrics_before,
            std::fs::read(&metrics_path).expect("metrics after")
        );
        assert_eq!(
            gates_before,
            std::fs::read(&gates_path).expect("gates after")
        );
        assert_eq!(
            drift_before,
            std::fs::read(&drift_path).expect("drift after")
        );
        assert_eq!(
            trends_before,
            std::fs::read(&trends_path).expect("trends after")
        );
    }

    #[test]
    fn source_resolution_requires_exactly_one_original_file() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        let asset_dir = paths::asset_dir(&project_root, "asset-a");
        std::fs::create_dir_all(&asset_dir).expect("mkdir asset");

        let err = resolve_internal_source_audio(&project_root, "asset-a").expect_err("missing");
        assert!(err.to_string().contains("found 0"));

        std::fs::write(asset_dir.join("original.wav"), b"one").expect("write one");
        let resolved = resolve_internal_source_audio(&project_root, "asset-a").expect("resolved");
        assert_eq!(resolved, asset_dir.join("original.wav"));

        std::fs::write(asset_dir.join("original.mp3"), b"two").expect("write two");
        let err = resolve_internal_source_audio(&project_root, "asset-a").expect_err("multiple");
        let message = err.to_string();
        assert!(message.contains("original.mp3, original.wav"));
        assert!(!message.contains(&asset_dir.join("original.mp3").to_string_lossy().to_string()));
    }

    #[test]
    fn decode_failure_writes_nothing() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        setup_project(&project_root, "asset-a", "run-1", false);

        let output_path = paths::translation_matrix_operator_output_path(&project_root, "run-1");
        let err = super::run(&project_root, "run-1").expect_err("decode failure");

        assert!(err.to_string().contains("decode translation matrix source"));
        assert!(!output_path.exists());
    }

    #[test]
    fn projection_rounds_nested_fields_and_delta_fallback() {
        let raw = raw_target_result(
            "mono_phone",
            true,
            GateStatus::Warn,
            42,
            1,
            2,
            Some(3.25123456),
        );
        let projected = project_target_output(&raw);

        assert_eq!(
            projected.telemetry.integrated_lufs,
            policy_registry::round6(-14.1234567)
        );
        assert_eq!(
            projected.telemetry.band_energies_db_rel.hz_20_60,
            policy_registry::round6(-8.1234567)
        );
        assert_eq!(
            projected.top_deltas[0].weighted_deviation,
            policy_registry::round6(0.321654321)
        );
        assert_eq!(
            projected.top_deltas[0].delta,
            policy_registry::round6(3.25123456)
        );

        let fallback = raw_target_result("mono_phone", true, GateStatus::Warn, 42, 1, 2, None);
        let projected = project_target_output(&fallback);
        assert_eq!(
            projected.top_deltas[0].delta,
            policy_registry::round6(0.7654321)
        );
    }

    #[test]
    fn summary_uses_locked_top_failing_ordering() {
        let summary = build_summary(&[
            target_output("first-warn", GateStatus::Warn, 70),
            target_output("first-fail", GateStatus::Fail, 40),
            target_output("second-fail", GateStatus::Fail, 80),
            target_output("pass", GateStatus::Pass, 5),
            target_output("second-warn", GateStatus::Warn, 60),
        ]);

        assert_eq!(summary.worst_target, "second-fail");
        assert_eq!(summary.best_target, "pass");
        assert_eq!(
            summary.average_translation_risk,
            policy_registry::round6(255.0 / 5.0)
        );
        assert_eq!(
            summary
                .top_failing_targets
                .iter()
                .map(|target| target.target_id.as_str())
                .collect::<Vec<_>>(),
            vec!["second-fail", "first-fail", "first-warn"]
        );
    }

    #[test]
    fn project_output_keeps_target_order_for_ties() {
        let source_hash = "hash-v1.0".to_string();
        let projected = project_output(
            Path::new("/tmp/project"),
            "run-1",
            "v1.0",
            &source_hash,
            &[
                raw_target_result("first", false, GateStatus::Warn, 50, 0, 0, Some(1.0)),
                raw_target_result("second", false, GateStatus::Warn, 50, 0, 0, Some(1.0)),
            ],
        );

        assert_eq!(projected.summary.worst_target, "first");
        assert_eq!(projected.summary.best_target, "first");
    }

    fn target_output(
        target_id: &str,
        gate_status: GateStatus,
        translation_risk: u32,
    ) -> TranslationTargetOutput {
        TranslationTargetOutput {
            target_id: target_id.to_string(),
            gate_status,
            drift_score: translation_risk,
            translation_risk,
            failed_blockers: 0,
            failed_warns: 0,
            top_deltas: Vec::new(),
            telemetry: super::TranslationTelemetry {
                channels: 2,
                integrated_lufs: 0.0,
                true_peak_dbtp: 0.0,
                approx_true_peak_dbtp: 0.0,
                crest_factor_db: 0.0,
                spectral_centroid_hz: 0.0,
                band_energies_db_rel: super::TranslationBandEnergies {
                    hz_20_60: 0.0,
                    hz_60_150: 0.0,
                    hz_150_500: 0.0,
                    hz_500_2000: 0.0,
                    hz_2000_8000: 0.0,
                    hz_8000_16000: 0.0,
                },
                correlation_min: Some(0.0),
                correlation_mean: Some(0.0),
                lr_balance_db: Some(0.0),
                transient_density: 0.0,
            },
            notes: Vec::new(),
        }
    }

    fn raw_target_result(
        target_id: &'static str,
        is_mono: bool,
        gate_status: GateStatus,
        translation_risk: u32,
        failed_blockers: u32,
        failed_warns: u32,
        delta: Option<f64>,
    ) -> crate::translation_sim::RawTargetResult {
        crate::translation_sim::RawTargetResult {
            target_id,
            is_mono,
            metrics: base_metrics(is_mono),
            gate_evaluation: GateEvaluation {
                gate_status,
                gates: vec![
                    GateResult {
                        gate_name: "blocker".to_string(),
                        severity: GateSeverity::Blocker,
                        pass_fail: failed_blockers == 0,
                        evidence: json!({}),
                        rationale: "x".to_string(),
                    },
                    GateResult {
                        gate_name: "warn".to_string(),
                        severity: GateSeverity::Warn,
                        pass_fail: failed_warns == 0,
                        evidence: json!({}),
                        rationale: "y".to_string(),
                    },
                ],
            },
            drift_result: DriftResult {
                drift_raw: 0.42,
                drift_score: 42,
                domain_scores: DomainScores {
                    loudness: Some(0.1),
                    dynamics: Some(0.2),
                    spectral_balance: Some(0.3),
                    stereo: if is_mono { None } else { Some(0.4) },
                },
                domain_weights_effective: DomainWeightsEffective {
                    loudness: Some(1.0),
                    dynamics: Some(1.0),
                    spectral_balance: Some(1.0),
                    stereo: if is_mono { None } else { Some(1.0) },
                },
                drift_vector: vec![DriftVectorItem {
                    metric_id: "spectral.hz_20_60".to_string(),
                    deviation: 0.7654321,
                    weighted_deviation: 0.321654321,
                    direction: "band_cold".to_string(),
                    evidence: match delta {
                        Some(value) => json!({ "delta": value }),
                        None => json!({}),
                    },
                }],
                fix_list: vec![FixSuggestion {
                    metric_id: "spectral.hz_20_60".to_string(),
                    direction: "band_cold".to_string(),
                    delta: 0.7654321,
                    human_action: "fix".to_string(),
                }],
                notes: vec!["Inside constitution corridors; no action required.".to_string()],
            },
            failed_blockers,
            failed_warns,
            translation_risk,
        }
    }

    fn base_metrics(is_mono: bool) -> Metrics {
        Metrics {
            analyzer_version: "TelemetryAnalyzer/1.0.0".to_string(),
            sample_rate_hz: 48_000,
            channels: if is_mono { 1 } else { 2 },
            frame_count: 48_000,
            duration_seconds: 1.0,
            sample_peak_linear: 0.5,
            sample_peak_dbfs: -6.0205999,
            clipping_sample_count: 0,
            rms_dbfs: -12.5,
            crest_factor_db: 6.1234567,
            short_term_rms_series_dbfs: vec![-12.0],
            approx_true_peak_dbtp: -1.2345678,
            true_peak_dbtp: -1.1234567,
            band_energies_db_rel: BandEnergies {
                hz_20_60: -8.1234567,
                hz_60_150: -5.1234567,
                hz_150_500: -2.1234567,
                hz_500_2000: 0.1234567,
                hz_2000_8000: 1.1234567,
                hz_8000_16000: -6.1234567,
            },
            spectral_centroid_hz: 2145.1234567,
            correlation_min: if is_mono { None } else { Some(0.4567891) },
            correlation_mean: if is_mono { None } else { Some(0.5678912) },
            lr_balance_db: if is_mono { None } else { Some(0.1234567) },
            lossy_source: false,
            integrated_lufs: -14.1234567,
            short_term_lufs_series: vec![-14.0],
            tonal_balance_curve: vec![0.0; 30],
            transient_density: 0.1234567,
        }
    }

    fn setup_project(
        project_root: &Path,
        asset_id: &str,
        run_id: &str,
        valid_audio: bool,
    ) -> String {
        let run_dir = paths::run_dir(project_root, asset_id, run_id);
        let asset_dir = paths::asset_dir(project_root, asset_id);
        std::fs::create_dir_all(&run_dir).expect("mkdir run");
        std::fs::create_dir_all(project_root.join("SCE/trends")).expect("mkdir trends");

        let constitution_path = write_registry_constitution(project_root, "v1.0");
        let constitution_hash = util::sha256_file(&constitution_path).expect("constitution hash");
        std::fs::write(
            project_root.join("SCE/constitution.yaml"),
            std::fs::read(&constitution_path).expect("read constitution"),
        )
        .expect("legacy constitution");

        if valid_audio {
            write_wav_i16(
                asset_dir.join("original.wav").as_path(),
                48_000,
                2,
                48_000,
                |frame_idx, channel_idx| {
                    let t = frame_idx as f64 / 48_000.0;
                    let value = (std::f64::consts::TAU * 440.0 * t).sin() * 0.2;
                    if channel_idx == 0 {
                        value
                    } else {
                        value * 0.85
                    }
                },
            )
            .expect("write wav");
        } else {
            std::fs::create_dir_all(&asset_dir).expect("mkdir asset");
            std::fs::write(asset_dir.join("original.wav"), b"not-a-wave").expect("write invalid");
        }

        let meta = ReportMeta {
            project_id: "project-1".to_string(),
            asset_id: asset_id.to_string(),
            run_id: run_id.to_string(),
            asset_content_hash: format!("hash-{run_id}"),
            constitution_version: "v1.0".to_string(),
            constitution_hash: constitution_hash.clone(),
            constitution_path: project_root
                .join("SCE/constitution.yaml")
                .to_string_lossy()
                .to_string(),
            analyzer_version: "TelemetryAnalyzer/1.0.0".to_string(),
            created_at: "2026-03-06T00:00:01.000Z".to_string(),
            started_at: "2026-03-06T00:00:01.000Z".to_string(),
            finished_at: "2026-03-06T00:00:01.000Z".to_string(),
        };

        std::fs::write(
            run_dir.join("metrics.json"),
            serde_json::to_vec_pretty(&json!({
                "meta": meta.clone(),
                "metrics": base_metrics(false),
            }))
            .expect("metrics json"),
        )
        .expect("write metrics");

        std::fs::write(
            run_dir.join("gates.json"),
            serde_json::to_vec_pretty(&json!({
                "meta": meta.clone(),
                "gate_status": GateStatus::Pass,
                "gates": []
            }))
            .expect("gates json"),
        )
        .expect("write gates");

        std::fs::write(
            run_dir.join("drift.json"),
            serde_json::to_vec_pretty(&json!({
                "meta": meta,
                "drift_raw": 0.0,
                "drift_score": 0,
                "domain_scores": {
                    "loudness": 0.0,
                    "dynamics": 0.0,
                    "spectral_balance": 0.0,
                    "stereo": 0.0
                },
                "domain_weights_effective": {
                    "loudness": 1.0,
                    "dynamics": 1.0,
                    "spectral_balance": 1.0,
                    "stereo": 1.0
                },
                "drift_vector": [],
                "fix_list": [],
                "notes": []
            }))
            .expect("drift json"),
        )
        .expect("write drift");

        std::fs::write(run_dir.join("report.json"), br#"{"status":"done"}"#).expect("report");
        std::fs::write(
            project_root.join("SCE/trends/project_trends.json"),
            br#"{"schema_version":"1.0"}"#,
        )
        .expect("trends");

        constitution_hash
    }

    fn write_registry_constitution(project_root: &Path, version: &str) -> PathBuf {
        let path = paths::registry_constitution_path(project_root, version);
        std::fs::create_dir_all(path.parent().expect("registry parent")).expect("mkdir registry");
        std::fs::write(
            &path,
            r#"
constitution_version: "v1.0"
audio_contract:
  allowed_sample_rates_hz: [48000]
  allowed_bit_depths: [16, 24]
  max_true_peak_dbtp: -0.5
  clipping_allowed: false
targets:
  loudness:
    integrated_lufs_range: [-18.0, -8.0]
  dynamics:
    crest_factor_db_range: [3.0, 18.0]
  spectral_balance:
    bands_db:
      hz_20_60: [-12.0, 6.0]
      hz_60_150: [-12.0, 6.0]
      hz_150_500: [-12.0, 6.0]
      hz_500_2000: [-12.0, 6.0]
      hz_2000_8000: [-12.0, 6.0]
      hz_8000_16000: [-12.0, 6.0]
  stereo:
    correlation_min: -1.0
    lr_balance_db_max_abs: 6.0
priorities:
  weights:
    low_end_translation: 1.4
    harshness_control: 1.2
    width_control: 0.9
    transient_punch: 1.0
    loudness_compliance: 1.1
"#,
        )
        .expect("write constitution");
        path
    }

    fn assert_field_order(text: &str, fields: &[&str]) {
        let mut cursor = 0usize;
        for field in fields {
            let next = text[cursor..]
                .find(field)
                .unwrap_or_else(|| panic!("missing field {field} in {text}"));
            cursor += next + field.len();
        }
    }

    fn first_object_after<'a>(text: &'a str, marker: &str) -> &'a str {
        let start = text.find(marker).expect("marker");
        let object_start = text[start..].find('{').expect("object start") + start;
        let mut depth = 0i32;
        for (offset, ch) in text[object_start..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &text[object_start..=object_start + offset];
                    }
                }
                _ => {}
            }
        }
        panic!("unterminated object");
    }

    fn write_wav_i16<F>(
        path: &Path,
        sample_rate_hz: u32,
        channels: u16,
        frames: usize,
        mut sample_fn: F,
    ) -> anyhow::Result<()>
    where
        F: FnMut(usize, usize) -> f64,
    {
        std::fs::create_dir_all(path.parent().expect("audio parent"))?;

        let mut pcm = Vec::<i16>::with_capacity(frames * channels as usize);
        for frame_idx in 0..frames {
            for channel_idx in 0..channels as usize {
                let value = (sample_fn(frame_idx, channel_idx) * 32767.0)
                    .round()
                    .clamp(i16::MIN as f64, i16::MAX as f64) as i16;
                pcm.push(value);
            }
        }

        let data_size = (pcm.len() * 2) as u32;
        let byte_rate = sample_rate_hz * channels as u32 * 2;
        let block_align = channels * 2;
        let riff_size = 36 + data_size;

        let mut file = std::fs::File::create(path)?;
        file.write_all(b"RIFF")?;
        file.write_all(&riff_size.to_le_bytes())?;
        file.write_all(b"WAVE")?;
        file.write_all(b"fmt ")?;
        file.write_all(&16u32.to_le_bytes())?;
        file.write_all(&1u16.to_le_bytes())?;
        file.write_all(&channels.to_le_bytes())?;
        file.write_all(&sample_rate_hz.to_le_bytes())?;
        file.write_all(&byte_rate.to_le_bytes())?;
        file.write_all(&block_align.to_le_bytes())?;
        file.write_all(&16u16.to_le_bytes())?;
        file.write_all(b"data")?;
        file.write_all(&data_size.to_le_bytes())?;
        for sample in pcm {
            file.write_all(&sample.to_le_bytes())?;
        }
        Ok(())
    }
}
