use crate::{
    cll::{self, ValidRun},
    gates::{GateSeverity, GateStatus},
    paths, policy_registry,
};
use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

const SCHEMA_VERSION: &str = "1.0";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CiEvaluatedRun {
    pub run_id: String,
    pub gate_status: GateStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CiCounts {
    pub pass: usize,
    pub warn: usize,
    pub fail: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FailedBlockerCount {
    pub gate_name: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConstitutionSummary {
    pub version: String,
    pub hash: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CiSummary {
    pub schema_version: String,
    pub project_root: String,
    pub evaluated_runs: Vec<CiEvaluatedRun>,
    pub worst_gate_status: GateStatus,
    pub counts: CiCounts,
    pub top_failed_blockers: Vec<FailedBlockerCount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constitution: Option<ConstitutionSummary>,
}

pub fn build_summary(
    project_root: &Path,
    run_id: Option<&str>,
    last_n: usize,
) -> anyhow::Result<CiSummary> {
    let canonical_root = policy_registry::canonical_project_root(project_root)?;
    let evaluated = select_runs(&canonical_root, run_id, last_n)?;

    let mut counts = CiCounts {
        pass: 0,
        warn: 0,
        fail: 0,
    };
    let mut worst = GateStatus::Pass;
    let mut blocker_counts = BTreeMap::<String, usize>::new();

    for run in &evaluated {
        match run.gate_status {
            GateStatus::Pass => counts.pass += 1,
            GateStatus::Warn => counts.warn += 1,
            GateStatus::Fail => counts.fail += 1,
        }
        worst = worst_status(worst, run.gate_status);

        for gate in run
            .gates
            .iter()
            .filter(|gate| gate.severity == GateSeverity::Blocker && !gate.pass_fail)
        {
            *blocker_counts.entry(gate.gate_name.clone()).or_default() += 1;
        }
    }

    let mut top_failed_blockers = blocker_counts
        .into_iter()
        .map(|(gate_name, count)| FailedBlockerCount { gate_name, count })
        .collect::<Vec<_>>();
    top_failed_blockers.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.gate_name.cmp(&right.gate_name))
    });

    let constitution = recover_constitution(&evaluated);

    Ok(CiSummary {
        schema_version: SCHEMA_VERSION.to_string(),
        project_root: canonical_root.to_string_lossy().to_string(),
        evaluated_runs: evaluated
            .iter()
            .map(|run| CiEvaluatedRun {
                run_id: run.run_id.clone(),
                gate_status: run.gate_status,
            })
            .collect(),
        worst_gate_status: worst,
        counts,
        top_failed_blockers,
        constitution,
    })
}

pub fn write_json_out(summary: &CiSummary, output_path: &Path) -> anyhow::Result<PathBuf> {
    policy_registry::write_atomic_json(output_path, summary)?;
    Ok(output_path.to_path_buf())
}

pub fn exit_code(summary: &CiSummary) -> i32 {
    match summary.worst_gate_status {
        GateStatus::Pass => 0,
        GateStatus::Warn => 1,
        GateStatus::Fail => 2,
    }
}

pub fn write_operator_output(
    project_root: &Path,
    run_id: Option<&str>,
    last_n: usize,
) -> anyhow::Result<(CiSummary, PathBuf)> {
    let canonical_root = policy_registry::canonical_project_root(project_root)?;
    let summary = build_summary(&canonical_root, run_id, last_n)?;
    let output_path = paths::mixops_ci_operator_output_path(&canonical_root);
    policy_registry::write_atomic_json(&output_path, &summary)?;
    Ok((summary, output_path))
}

fn select_runs(
    project_root: &Path,
    run_id: Option<&str>,
    last_n: usize,
) -> anyhow::Result<Vec<ValidRun>> {
    if let Some(run_id) = run_id {
        let candidate = cll::find_candidate_run(project_root, run_id)?
            .ok_or_else(|| anyhow!("run not found: {run_id}"))?;
        let run = cll::read_valid_run(&candidate)
            .with_context(|| format!("load valid run for ci-check: {run_id}"))?;
        return Ok(vec![run]);
    }

    let loaded = cll::load_valid_runs(project_root)?;
    Ok(cll::select_window(&loaded.valid_runs, last_n))
}

fn worst_status(current: GateStatus, incoming: GateStatus) -> GateStatus {
    match (current, incoming) {
        (GateStatus::Fail, _) | (_, GateStatus::Fail) => GateStatus::Fail,
        (GateStatus::Warn, _) | (_, GateStatus::Warn) => GateStatus::Warn,
        _ => GateStatus::Pass,
    }
}

fn recover_constitution(runs: &[ValidRun]) -> Option<ConstitutionSummary> {
    let first = runs.first()?;
    let shared = runs.iter().all(|run| {
        run.constitution_version == first.constitution_version
            && run.constitution_hash == first.constitution_hash
            && run.constitution_path == first.constitution_path
    });

    if shared {
        Some(ConstitutionSummary {
            version: first.constitution_version.clone(),
            hash: first.constitution_hash.clone(),
            path: first.constitution_path.clone(),
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{build_summary, exit_code, write_json_out, write_operator_output};
    use crate::{
        gates::{GateResult, GateSeverity, GateStatus},
        paths,
        reports::ReportMeta,
    };
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn summary_uses_last_n_or_specific_run_selection() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");

        write_run(
            &project_root,
            "asset-a",
            "run-1",
            "2026-03-06T00:00:01.000Z",
            GateStatus::Pass,
            "v1.0",
            "hash-v1.0",
        );
        write_run(
            &project_root,
            "asset-a",
            "run-2",
            "2026-03-06T00:00:02.000Z",
            GateStatus::Warn,
            "v1.0",
            "hash-v1.0",
        );

        let windowed = build_summary(&project_root, None, 1).expect("windowed");
        assert_eq!(windowed.evaluated_runs.len(), 1);
        assert_eq!(windowed.evaluated_runs[0].run_id, "run-2");

        let explicit = build_summary(&project_root, Some("run-1"), 25).expect("explicit");
        assert_eq!(explicit.evaluated_runs.len(), 1);
        assert_eq!(explicit.evaluated_runs[0].run_id, "run-1");
    }

    #[test]
    fn worst_status_and_blocker_ordering_are_locked() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");

        write_run(
            &project_root,
            "asset-a",
            "run-1",
            "2026-03-06T00:00:01.000Z",
            GateStatus::Warn,
            "v1.0",
            "hash-v1.0",
        );
        write_run(
            &project_root,
            "asset-a",
            "run-2",
            "2026-03-06T00:00:02.000Z",
            GateStatus::Fail,
            "v1.0",
            "hash-v1.0",
        );
        write_run(
            &project_root,
            "asset-a",
            "run-3",
            "2026-03-06T00:00:03.000Z",
            GateStatus::Fail,
            "v1.0",
            "hash-v1.0",
        );

        let summary = build_summary(&project_root, None, 25).expect("summary");
        assert_eq!(summary.schema_version, "1.0");
        assert_eq!(summary.worst_gate_status, GateStatus::Fail);
        assert_eq!(summary.counts.pass, 0);
        assert_eq!(summary.counts.warn, 1);
        assert_eq!(summary.counts.fail, 2);
        assert_eq!(
            summary.top_failed_blockers[0].gate_name,
            "G003_TruePeakCeiling"
        );
        assert_eq!(summary.top_failed_blockers[0].count, 2);
        assert_eq!(
            summary.top_failed_blockers[1].gate_name,
            "G004_HardClipping"
        );
        assert_eq!(exit_code(&summary), 2);
    }

    #[test]
    fn json_out_and_operator_output_reuse_same_builder() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        write_run(
            &project_root,
            "asset-a",
            "run-1",
            "2026-03-06T00:00:01.000Z",
            GateStatus::Pass,
            "v1.0",
            "hash-v1.0",
        );

        let summary = build_summary(&project_root, None, 25).expect("summary");
        let explicit_path = td.path().join("ci.json");
        write_json_out(&summary, &explicit_path).expect("write explicit");
        let explicit_bytes = std::fs::read(&explicit_path).expect("read explicit");

        let (operator_summary, operator_path) =
            write_operator_output(&project_root, None, 25).expect("operator");
        let operator_bytes = std::fs::read(&operator_path).expect("read operator");

        assert_eq!(summary, operator_summary);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&explicit_bytes).expect("explicit json"),
            serde_json::from_slice::<serde_json::Value>(&operator_bytes).expect("operator json")
        );
    }

    #[test]
    fn constitution_is_only_reported_when_recoverable() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");

        write_run(
            &project_root,
            "asset-a",
            "run-1",
            "2026-03-06T00:00:01.000Z",
            GateStatus::Pass,
            "v1.0",
            "hash-v1.0",
        );
        write_run(
            &project_root,
            "asset-a",
            "run-2",
            "2026-03-06T00:00:02.000Z",
            GateStatus::Pass,
            "v1.1",
            "hash-v1.1",
        );

        let summary = build_summary(&project_root, None, 25).expect("summary");
        assert!(summary.constitution.is_none());
    }

    fn write_run(
        project_root: &Path,
        asset_id: &str,
        run_id: &str,
        finished_at: &str,
        gate_status: GateStatus,
        constitution_version: &str,
        constitution_hash: &str,
    ) {
        let run_dir = paths::run_dir(project_root, asset_id, run_id);
        std::fs::create_dir_all(&run_dir).expect("mkdir run");

        let meta = ReportMeta {
            project_id: "project-1".to_string(),
            asset_id: asset_id.to_string(),
            run_id: run_id.to_string(),
            asset_content_hash: format!("hash-{run_id}"),
            constitution_version: constitution_version.to_string(),
            constitution_hash: constitution_hash.to_string(),
            constitution_path: project_root
                .join("SCE/constitution.yaml")
                .to_string_lossy()
                .to_string(),
            analyzer_version: "TelemetryAnalyzer/1.0.0".to_string(),
            created_at: finished_at.to_string(),
            started_at: finished_at.to_string(),
            finished_at: finished_at.to_string(),
        };

        std::fs::write(
            run_dir.join("metrics.json"),
            serde_json::to_vec_pretty(&json!({
                "meta": meta,
                "metrics": {
                    "analyzer_version": "TelemetryAnalyzer/1.0.0",
                    "sample_rate_hz": 48000,
                    "channels": 2,
                    "frame_count": 48000,
                    "duration_seconds": 1.0,
                    "sample_peak_linear": 0.5,
                    "sample_peak_dbfs": -6.0,
                    "clipping_sample_count": 0,
                    "rms_dbfs": -12.0,
                    "crest_factor_db": 6.0,
                    "short_term_rms_series_dbfs": [-12.0],
                    "approx_true_peak_dbtp": -2.0,
                    "true_peak_dbtp": -1.9,
                    "band_energies_db_rel": {
                        "hz_20_60": -4.0,
                        "hz_60_150": -3.0,
                        "hz_150_500": -2.0,
                        "hz_500_2000": -1.0,
                        "hz_2000_8000": -2.0,
                        "hz_8000_16000": -5.0
                    },
                    "spectral_centroid_hz": 1100.0,
                    "correlation_min": 0.7,
                    "correlation_mean": 0.8,
                    "lr_balance_db": 0.1,
                    "lossy_source": false,
                    "integrated_lufs": -14.0,
                    "short_term_lufs_series": [-14.0],
                    "tonal_balance_curve": vec![0.0; 30],
                    "transient_density": 0.2
                }
            }))
            .expect("metrics"),
        )
        .expect("write metrics");

        let blocker_fail = gate_status == GateStatus::Fail;
        std::fs::write(
            run_dir.join("gates.json"),
            serde_json::to_vec_pretty(&json!({
                "meta": meta,
                "gate_status": gate_status,
                "gates": [
                    GateResult {
                        gate_name: "G003_TruePeakCeiling".to_string(),
                        severity: GateSeverity::Blocker,
                        pass_fail: !blocker_fail,
                        evidence: json!({"configured": true}),
                        rationale: "primary blocker".to_string(),
                    },
                    GateResult {
                        gate_name: "G004_HardClipping".to_string(),
                        severity: GateSeverity::Blocker,
                        pass_fail: gate_status != GateStatus::Fail || run_id == "run-2",
                        evidence: json!({"configured": true}),
                        rationale: "secondary blocker".to_string(),
                    }
                ]
            }))
            .expect("gates"),
        )
        .expect("write gates");

        std::fs::write(
            run_dir.join("drift.json"),
            serde_json::to_vec_pretty(&json!({
                "meta": meta,
                "drift_score": 10,
                "domain_scores": {
                    "loudness": 0.1,
                    "dynamics": 0.1,
                    "spectral_balance": 0.1,
                    "stereo": 0.1
                },
                "drift_vector": []
            }))
            .expect("drift"),
        )
        .expect("write drift");
    }
}
