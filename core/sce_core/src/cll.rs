use crate::{
    analyzer::{BandEnergies, Metrics},
    diversity::{compute_diversity_sentinel, DiversityRunInput, DiversitySentinelOutput},
    drift::{DomainScores, DriftVectorItem},
    gates::{GateResult, GateSeverity, GateStatus},
    paths,
    reports::ReportMeta,
    util,
};
use anyhow::{anyhow, Context};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectTrends {
    pub meta: TrendsMeta,
    pub gate_trends: GateTrends,
    pub drift_trends: DriftTrends,
    pub telemetry_trends: TelemetryTrends,
    pub diversity_sentinel: DiversitySentinelOutput,
    pub flags: TrendFlags,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrendsMeta {
    pub project_root: String,
    pub generated_at: String,
    pub window: TrendWindow,
    pub includes_run_ids: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrendWindow {
    pub requested_last_n: usize,
    pub selected_run_count: usize,
    pub valid_runs: usize,
    pub discovered_runs: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateTrends {
    pub counts: GateStatusCounts,
    pub top_failed_blockers: Vec<GateFailureCount>,
    pub top_failed_warns: Vec<GateFailureCount>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateStatusCounts {
    pub pass: usize,
    pub warn: usize,
    pub fail: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateFailureCount {
    pub gate_name: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DriftTrends {
    pub drift_score_series: Vec<DriftScoreSeriesEntry>,
    pub domain_score_series: Vec<DomainScoreSeriesEntry>,
    pub top_recurring_deltas: Vec<MetricCount>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DriftScoreSeriesEntry {
    pub run_id: String,
    pub finished_at: String,
    pub gate_status: GateStatus,
    pub drift_score: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DomainScoreSeriesEntry {
    pub run_id: String,
    pub finished_at: String,
    pub gate_status: GateStatus,
    pub loudness: Option<f64>,
    pub dynamics: Option<f64>,
    pub spectral_balance: Option<f64>,
    pub stereo: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricCount {
    pub metric_id: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TelemetryTrends {
    pub integrated_lufs_series: Vec<ScalarSeriesEntry>,
    pub true_peak_series: Vec<ScalarSeriesEntry>,
    pub crest_factor_series: Vec<ScalarSeriesEntry>,
    pub band_energy_series: BandEnergySeries,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScalarSeriesEntry {
    pub run_id: String,
    pub finished_at: String,
    pub gate_status: GateStatus,
    pub value: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BandEnergySeries {
    pub hz_20_60: Vec<ScalarSeriesEntry>,
    pub hz_60_150: Vec<ScalarSeriesEntry>,
    pub hz_150_500: Vec<ScalarSeriesEntry>,
    pub hz_500_2000: Vec<ScalarSeriesEntry>,
    pub hz_2000_8000: Vec<ScalarSeriesEntry>,
    pub hz_8000_16000: Vec<ScalarSeriesEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrendFlags {
    pub data_gaps: bool,
    pub skipped_runs: usize,
    pub notes: Vec<String>,
    pub data_gap_warnings: Vec<DataGapWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DataGapWarning {
    pub run_id: String,
    pub asset_id: Option<String>,
    pub kind: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct TrendsGenerationSummary {
    pub output_path: PathBuf,
    pub requested_last_n: usize,
    pub selected_run_count: usize,
    pub valid_runs: usize,
    pub skipped_runs: usize,
    pub data_gaps: bool,
    pub baseline_run_count: usize,
    pub recent_run_count: usize,
    pub warn_alert_count: usize,
    pub info_alert_count: usize,
}

#[derive(Debug, Deserialize)]
struct MetricsArtifact {
    meta: ReportMeta,
    metrics: Metrics,
}

#[derive(Debug, Deserialize)]
struct GatesArtifact {
    meta: ReportMeta,
    gate_status: GateStatus,
    gates: Vec<GateResult>,
}

#[derive(Debug, Deserialize)]
struct DriftArtifact {
    meta: ReportMeta,
    drift_score: u32,
    domain_scores: DomainScores,
    drift_vector: Vec<DriftVectorItem>,
}

#[derive(Debug, Clone)]
struct CandidateRun {
    asset_id: String,
    run_id: String,
    run_dir: PathBuf,
}

#[derive(Debug, Clone)]
struct ValidRun {
    run_id: String,
    finished_at: String,
    finished_at_parsed: DateTime<FixedOffset>,
    gate_status: GateStatus,
    metrics: Metrics,
    gates: Vec<GateResult>,
    drift_score: u32,
    domain_scores: DomainScores,
    drift_vector: Vec<DriftVectorItem>,
}

pub fn generate_project_trends(
    project_root: &Path,
    requested_last_n: usize,
) -> anyhow::Result<TrendsGenerationSummary> {
    let canonical_root = fs::canonicalize(project_root)
        .with_context(|| format!("canonicalize project root: {}", project_root.display()))?;

    let discovered_runs = discover_runs(&canonical_root)?;
    let discovered_count = discovered_runs.len();

    let mut valid_runs = Vec::new();
    let mut data_gap_warnings = Vec::new();

    for candidate in discovered_runs {
        match read_valid_run(&candidate) {
            Ok(valid) => valid_runs.push(valid),
            Err(err) => data_gap_warnings.push(DataGapWarning {
                run_id: candidate.run_id,
                asset_id: Some(candidate.asset_id),
                kind: None,
                detail: err.to_string(),
            }),
        }
    }

    valid_runs.sort_by(|left, right| {
        left.finished_at_parsed
            .cmp(&right.finished_at_parsed)
            .then_with(|| left.run_id.cmp(&right.run_id))
    });

    let selected_runs = select_window(&valid_runs, requested_last_n);
    let selected_count = selected_runs.len();
    let valid_count = valid_runs.len();
    let skipped_runs = discovered_count.saturating_sub(valid_count);

    let mut notes = Vec::new();
    if selected_count == 0 {
        notes.push("no valid runs found".to_string());
    }

    if requested_last_n > 0 && selected_count < requested_last_n {
        notes.push(format!(
            "requested_last_n={requested_last_n}, available_valid_runs={selected_count}"
        ));
    }

    let diversity_input = selected_runs
        .iter()
        .map(|run| DiversityRunInput {
            run_id: run.run_id.clone(),
            finished_at: run.finished_at.clone(),
            drift_score: run.drift_score,
            metrics: run.metrics.clone(),
        })
        .collect::<Vec<_>>();

    let diversity_sentinel = compute_diversity_sentinel(&diversity_input);

    let trends = ProjectTrends {
        meta: TrendsMeta {
            project_root: canonical_root.to_string_lossy().to_string(),
            generated_at: util::now_rfc3339(),
            window: TrendWindow {
                requested_last_n,
                selected_run_count: selected_count,
                valid_runs: valid_count,
                discovered_runs: discovered_count,
            },
            includes_run_ids: true,
        },
        gate_trends: build_gate_trends(&selected_runs),
        drift_trends: build_drift_trends(&selected_runs),
        telemetry_trends: build_telemetry_trends(&selected_runs),
        diversity_sentinel: diversity_sentinel.clone(),
        flags: TrendFlags {
            data_gaps: !data_gap_warnings.is_empty()
                || (requested_last_n > 0 && selected_count < requested_last_n),
            skipped_runs,
            notes,
            data_gap_warnings,
        },
    };

    let trends_dir = paths::project_sce_dir(&canonical_root).join("trends");
    fs::create_dir_all(&trends_dir)
        .with_context(|| format!("create trends directory: {}", trends_dir.display()))?;

    let output_path = trends_dir.join("project_trends.json");
    fs::write(&output_path, serde_json::to_vec_pretty(&trends)?)
        .with_context(|| format!("write trends artifact: {}", output_path.display()))?;

    Ok(TrendsGenerationSummary {
        output_path,
        requested_last_n,
        selected_run_count: selected_count,
        valid_runs: valid_count,
        skipped_runs,
        data_gaps: trends.flags.data_gaps,
        baseline_run_count: diversity_sentinel.baseline_run_count,
        recent_run_count: diversity_sentinel.recent_run_count,
        warn_alert_count: diversity_sentinel.warn_alert_count(),
        info_alert_count: diversity_sentinel.info_alert_count(),
    })
}

fn discover_runs(project_root: &Path) -> anyhow::Result<Vec<CandidateRun>> {
    let assets_root = paths::project_sce_dir(project_root).join("assets");
    if !assets_root.exists() {
        return Ok(Vec::new());
    }

    let mut asset_ids = fs::read_dir(&assets_root)
        .with_context(|| format!("read assets directory: {}", assets_root.display()))?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();

    asset_ids.sort();

    let mut runs = Vec::new();
    for asset_id in asset_ids {
        let analysis_dir = assets_root.join(&asset_id).join("analysis");
        if !analysis_dir.exists() {
            continue;
        }

        let mut run_ids = fs::read_dir(&analysis_dir)
            .with_context(|| format!("read analysis directory: {}", analysis_dir.display()))?
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>();

        run_ids.sort();
        for run_id in run_ids {
            runs.push(CandidateRun {
                asset_id: asset_id.clone(),
                run_id: run_id.clone(),
                run_dir: analysis_dir.join(run_id),
            });
        }
    }

    Ok(runs)
}

fn read_valid_run(candidate: &CandidateRun) -> anyhow::Result<ValidRun> {
    let metrics_path = candidate.run_dir.join("metrics.json");
    let gates_path = candidate.run_dir.join("gates.json");
    let drift_path = candidate.run_dir.join("drift.json");

    let metrics: MetricsArtifact = read_json(&metrics_path)
        .with_context(|| format!("parse metrics.json at {}", metrics_path.display()))?;
    let gates: GatesArtifact = read_json(&gates_path)
        .with_context(|| format!("parse gates.json at {}", gates_path.display()))?;
    let drift: DriftArtifact = read_json(&drift_path)
        .with_context(|| format!("parse drift.json at {}", drift_path.display()))?;

    validate_run_meta(&candidate.run_id, &metrics.meta, "metrics")?;
    validate_run_meta(&candidate.run_id, &gates.meta, "gates")?;
    validate_run_meta(&candidate.run_id, &drift.meta, "drift")?;

    let finished_at_parsed = DateTime::parse_from_rfc3339(&metrics.meta.finished_at)
        .with_context(|| format!("parse metrics finished_at for run {}", candidate.run_id))?;

    Ok(ValidRun {
        run_id: candidate.run_id.clone(),
        finished_at: metrics.meta.finished_at,
        finished_at_parsed,
        gate_status: gates.gate_status,
        metrics: metrics.metrics,
        gates: gates.gates,
        drift_score: drift.drift_score,
        domain_scores: drift.domain_scores,
        drift_vector: drift.drift_vector,
    })
}

fn validate_run_meta(
    expected_run_id: &str,
    meta: &ReportMeta,
    artifact_name: &str,
) -> anyhow::Result<()> {
    if meta.run_id != expected_run_id {
        return Err(anyhow!(
            "{artifact_name} run_id mismatch: expected {expected_run_id}, found {}",
            meta.run_id
        ));
    }

    DateTime::parse_from_rfc3339(&meta.finished_at).with_context(|| {
        format!(
            "{artifact_name} finished_at is not parseable RFC3339: {}",
            meta.finished_at
        )
    })?;

    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> anyhow::Result<T> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("read artifact file: {}", path.display()))?;
    let value = serde_json::from_str::<T>(&content)
        .with_context(|| format!("deserialize JSON: {}", path.display()))?;
    Ok(value)
}

fn select_window(valid_runs: &[ValidRun], requested_last_n: usize) -> Vec<ValidRun> {
    if requested_last_n == 0 || requested_last_n >= valid_runs.len() {
        valid_runs.to_vec()
    } else {
        valid_runs[valid_runs.len() - requested_last_n..].to_vec()
    }
}

fn build_gate_trends(runs: &[ValidRun]) -> GateTrends {
    let mut counts = GateStatusCounts {
        pass: 0,
        warn: 0,
        fail: 0,
    };

    let mut blocker_failures = BTreeMap::<String, usize>::new();
    let mut warn_failures = BTreeMap::<String, usize>::new();

    for run in runs {
        match run.gate_status {
            GateStatus::Pass => counts.pass += 1,
            GateStatus::Warn => counts.warn += 1,
            GateStatus::Fail => counts.fail += 1,
        }

        for gate in run.gates.iter().filter(|gate| !gate.pass_fail) {
            match gate.severity {
                GateSeverity::Blocker => {
                    *blocker_failures.entry(gate.gate_name.clone()).or_default() += 1;
                }
                GateSeverity::Warn => {
                    *warn_failures.entry(gate.gate_name.clone()).or_default() += 1;
                }
                GateSeverity::Info => {}
            }
        }
    }

    GateTrends {
        counts,
        top_failed_blockers: sorted_gate_counts(blocker_failures),
        top_failed_warns: sorted_gate_counts(warn_failures),
    }
}

fn sorted_gate_counts(counts: BTreeMap<String, usize>) -> Vec<GateFailureCount> {
    let mut rows = counts
        .into_iter()
        .map(|(gate_name, count)| GateFailureCount { gate_name, count })
        .collect::<Vec<_>>();

    rows.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.gate_name.cmp(&right.gate_name))
    });

    rows
}

fn build_drift_trends(runs: &[ValidRun]) -> DriftTrends {
    let drift_score_series = runs
        .iter()
        .map(|run| DriftScoreSeriesEntry {
            run_id: run.run_id.clone(),
            finished_at: run.finished_at.clone(),
            gate_status: run.gate_status,
            drift_score: run.drift_score,
        })
        .collect::<Vec<_>>();

    let domain_score_series = runs
        .iter()
        .map(|run| DomainScoreSeriesEntry {
            run_id: run.run_id.clone(),
            finished_at: run.finished_at.clone(),
            gate_status: run.gate_status,
            loudness: run.domain_scores.loudness,
            dynamics: run.domain_scores.dynamics,
            spectral_balance: run.domain_scores.spectral_balance,
            stereo: run.domain_scores.stereo,
        })
        .collect::<Vec<_>>();

    let mut delta_counts = BTreeMap::<String, usize>::new();
    for run in runs {
        for delta in &run.drift_vector {
            *delta_counts.entry(delta.metric_id.clone()).or_default() += 1;
        }
    }

    let mut top_recurring_deltas = delta_counts
        .into_iter()
        .map(|(metric_id, count)| MetricCount { metric_id, count })
        .collect::<Vec<_>>();

    top_recurring_deltas.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.metric_id.cmp(&right.metric_id))
    });

    DriftTrends {
        drift_score_series,
        domain_score_series,
        top_recurring_deltas,
    }
}

fn build_telemetry_trends(runs: &[ValidRun]) -> TelemetryTrends {
    let integrated_lufs_series = runs
        .iter()
        .map(|run| scalar_entry(run, run.metrics.integrated_lufs))
        .collect::<Vec<_>>();

    let true_peak_series = runs
        .iter()
        .map(|run| scalar_entry(run, run.metrics.true_peak_dbtp))
        .collect::<Vec<_>>();

    let crest_factor_series = runs
        .iter()
        .map(|run| scalar_entry(run, run.metrics.crest_factor_db))
        .collect::<Vec<_>>();

    TelemetryTrends {
        integrated_lufs_series,
        true_peak_series,
        crest_factor_series,
        band_energy_series: build_band_energy_series(runs),
    }
}

fn scalar_entry(run: &ValidRun, value: f64) -> ScalarSeriesEntry {
    ScalarSeriesEntry {
        run_id: run.run_id.clone(),
        finished_at: run.finished_at.clone(),
        gate_status: run.gate_status,
        value,
    }
}

fn build_band_energy_series(runs: &[ValidRun]) -> BandEnergySeries {
    let entries_for = |extract: fn(&BandEnergies) -> f64| {
        runs.iter()
            .map(|run| scalar_entry(run, extract(&run.metrics.band_energies_db_rel)))
            .collect::<Vec<_>>()
    };

    BandEnergySeries {
        hz_20_60: entries_for(|bands| bands.hz_20_60),
        hz_60_150: entries_for(|bands| bands.hz_60_150),
        hz_150_500: entries_for(|bands| bands.hz_150_500),
        hz_500_2000: entries_for(|bands| bands.hz_500_2000),
        hz_2000_8000: entries_for(|bands| bands.hz_2000_8000),
        hz_8000_16000: entries_for(|bands| bands.hz_8000_16000),
    }
}

#[cfg(test)]
mod tests {
    use super::generate_project_trends;
    use crate::{
        analyzer::{BandEnergies, Metrics},
        drift::{DomainScores, DriftVectorItem},
        gates::{GateResult, GateSeverity, GateStatus},
        reports::ReportMeta,
    };
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn writes_empty_trends_when_no_valid_runs_exist() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        std::fs::create_dir_all(project_root.join("SCE/assets")).expect("mkdir");

        let summary = generate_project_trends(&project_root, 25).expect("generate trends");
        let output = std::fs::read_to_string(&summary.output_path).expect("read trends");
        let value: serde_json::Value = serde_json::from_str(&output).expect("parse trends");

        assert_eq!(value["meta"]["window"]["selected_run_count"], 0);
        assert!(value["diversity_sentinel"]["diversity_alerts"]
            .as_array()
            .expect("array")
            .is_empty());
        assert!(value["flags"]["notes"]
            .as_array()
            .expect("array")
            .iter()
            .any(|note| note.as_str() == Some("no valid runs found")));
    }

    #[test]
    fn includes_pass_warn_fail_runs_and_sorts_by_finished_at_then_run_id() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");

        write_run(
            &project_root,
            "asset-a",
            "run-b",
            "2026-03-06T00:00:10.000Z",
            GateStatus::Pass,
            10,
            0.0,
            Some(0.8),
            Some(0.1),
        );
        write_run(
            &project_root,
            "asset-b",
            "run-a",
            "2026-03-06T00:00:10.000Z",
            GateStatus::Warn,
            20,
            0.5,
            Some(0.2),
            Some(0.3),
        );
        write_run(
            &project_root,
            "asset-c",
            "run-c",
            "2026-03-06T00:00:11.000Z",
            GateStatus::Fail,
            30,
            -0.5,
            Some(0.4),
            Some(0.4),
        );

        let summary = generate_project_trends(&project_root, 25).expect("generate trends");
        let output = std::fs::read_to_string(&summary.output_path).expect("read trends");
        let value: serde_json::Value = serde_json::from_str(&output).expect("parse trends");

        let series = value["drift_trends"]["drift_score_series"]
            .as_array()
            .expect("array");

        assert_eq!(series[0]["run_id"], "run-a");
        assert_eq!(series[1]["run_id"], "run-b");
        assert_eq!(series[2]["run_id"], "run-c");

        assert_eq!(value["gate_trends"]["counts"]["pass"], 1);
        assert_eq!(value["gate_trends"]["counts"]["warn"], 1);
        assert_eq!(value["gate_trends"]["counts"]["fail"], 1);
    }

    #[test]
    fn last_n_zero_uses_full_history_and_gap_warning_for_invalid_runs() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");

        write_run(
            &project_root,
            "asset-a",
            "run-1",
            "2026-03-06T00:00:01.000Z",
            GateStatus::Pass,
            8,
            0.0,
            Some(0.7),
            Some(0.1),
        );
        write_run(
            &project_root,
            "asset-a",
            "run-2",
            "2026-03-06T00:00:02.000Z",
            GateStatus::Pass,
            9,
            0.0,
            Some(0.6),
            Some(0.2),
        );

        // Invalid run: gates meta run_id does not match directory run_id.
        write_run(
            &project_root,
            "asset-x",
            "run-bad",
            "2026-03-06T00:00:03.000Z",
            GateStatus::Warn,
            50,
            0.0,
            Some(0.2),
            Some(0.2),
        );
        let bad_gates_path = project_root.join("SCE/assets/asset-x/analysis/run-bad/gates.json");
        let mut bad_gates: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&bad_gates_path).expect("read gates"))
                .expect("parse gates");
        bad_gates["meta"]["run_id"] = json!("other-run-id");
        std::fs::write(
            &bad_gates_path,
            serde_json::to_vec_pretty(&bad_gates).expect("serialize"),
        )
        .expect("rewrite gates");

        let summary = generate_project_trends(&project_root, 0).expect("generate trends");
        assert_eq!(summary.selected_run_count, 2);
        assert_eq!(summary.valid_runs, 2);
        assert_eq!(summary.skipped_runs, 1);
        assert!(summary.data_gaps);
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
                    rationale: "test gate".to_string(),
                },
                GateResult {
                    gate_name: "W003_SpectralBandOutside".to_string(),
                    severity: GateSeverity::Warn,
                    pass_fail: true,
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
                loudness: Some(((drift_score as f64) / 100.0).min(1.0)),
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

    fn base_metrics(
        offset: f64,
        correlation_mean: Option<f64>,
        lr_balance_db: Option<f64>,
    ) -> Metrics {
        let mut tonal_curve = Vec::with_capacity(30);
        for i in 0..30 {
            tonal_curve.push((i as f64 * 0.1) + offset);
        }

        Metrics {
            analyzer_version: "TelemetryAnalyzer/1.0.0".to_string(),
            sample_rate_hz: 48_000,
            channels: 2,
            frame_count: 48_000,
            duration_seconds: 1.0,
            sample_peak_linear: 0.5,
            sample_peak_dbfs: -6.0,
            clipping_sample_count: 0,
            rms_dbfs: -9.0,
            crest_factor_db: 9.0 + offset,
            short_term_rms_series_dbfs: vec![-9.0],
            approx_true_peak_dbtp: -2.0,
            true_peak_dbtp: -1.8,
            band_energies_db_rel: BandEnergies {
                hz_20_60: -4.0 + offset,
                hz_60_150: -3.5 + offset,
                hz_150_500: -2.5 + offset,
                hz_500_2000: -1.0 + offset,
                hz_2000_8000: -2.0 + offset,
                hz_8000_16000: -5.0 + offset,
            },
            spectral_centroid_hz: 1200.0 + offset * 100.0,
            correlation_min: Some(0.4),
            correlation_mean,
            lr_balance_db,
            lossy_source: false,
            integrated_lufs: -14.0 + offset,
            short_term_lufs_series: vec![-14.0 + offset],
            tonal_balance_curve: tonal_curve,
            transient_density: 0.3 + offset * 0.01,
        }
    }
}
