use crate::{cll, paths, policy_registry};
use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: &str = "1.0";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranslationTargetPlaceholder {
    pub target_id: String,
    pub status: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranslationMatrixOutput {
    pub schema_version: String,
    pub not_implemented: bool,
    pub project_root: String,
    pub run_id: String,
    pub targets: Vec<TranslationTargetPlaceholder>,
}

pub fn run(
    project_root: &Path,
    run_id: &str,
) -> anyhow::Result<(TranslationMatrixOutput, PathBuf)> {
    let canonical_root = policy_registry::canonical_project_root(project_root)?;
    let candidate = cll::find_candidate_run(&canonical_root, run_id)?
        .ok_or_else(|| anyhow!("run not found: {run_id}"))?;

    let output = TranslationMatrixOutput {
        schema_version: SCHEMA_VERSION.to_string(),
        not_implemented: true,
        project_root: canonical_root.to_string_lossy().to_string(),
        run_id: candidate.run_id,
        targets: vec![
            TranslationTargetPlaceholder {
                target_id: "ableton_live".to_string(),
                status: "todo".to_string(),
                note: "translation matrix scaffold only".to_string(),
            },
            TranslationTargetPlaceholder {
                target_id: "logic_pro".to_string(),
                status: "todo".to_string(),
                note: "translation matrix scaffold only".to_string(),
            },
            TranslationTargetPlaceholder {
                target_id: "pro_tools".to_string(),
                status: "todo".to_string(),
                note: "translation matrix scaffold only".to_string(),
            },
        ],
    };

    let output_path = paths::translation_matrix_operator_output_path(&canonical_root, run_id);
    policy_registry::write_atomic_json(&output_path, &output)
        .with_context(|| format!("write translation matrix output: {}", output_path.display()))?;
    Ok((output, output_path))
}

#[cfg(test)]
mod tests {
    use super::run;
    use crate::{gates::GateStatus, paths, reports::ReportMeta};
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn translation_placeholder_has_top_level_not_implemented() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        write_run(&project_root, "asset-a", "run-1");

        let (output, path) = run(&project_root, "run-1").expect("translation");
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("read output"))
                .expect("parse output");

        assert!(output.not_implemented);
        assert_eq!(written["not_implemented"], true);
        assert_eq!(written["schema_version"], "1.0");
    }

    fn write_run(project_root: &Path, asset_id: &str, run_id: &str) {
        let run_dir = paths::run_dir(project_root, asset_id, run_id);
        std::fs::create_dir_all(&run_dir).expect("mkdir run");

        let meta = ReportMeta {
            project_id: "project-1".to_string(),
            asset_id: asset_id.to_string(),
            run_id: run_id.to_string(),
            asset_content_hash: format!("hash-{run_id}"),
            constitution_version: "v1.0".to_string(),
            constitution_hash: "hash-v1.0".to_string(),
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

        std::fs::write(
            run_dir.join("gates.json"),
            serde_json::to_vec_pretty(&json!({
                "meta": meta,
                "gate_status": GateStatus::Pass,
                "gates": []
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
