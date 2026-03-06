use crate::{cll, paths, policy_registry};
use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

const SCHEMA_VERSION: &str = "1.0";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChainAdvisory {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChainMetadata {
    pub schema_version: String,
    pub run_id: String,
    pub source_path: String,
    pub sidecar_candidates: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_sidecar_path: Option<String>,
    pub sidecar_status: String,
    pub compiler_status: String,
    pub not_implemented: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advisory: Option<ChainAdvisory>,
}

pub fn write_worker_chain_meta(
    run_dir: &Path,
    run_id: &str,
    source_path: &Path,
) -> anyhow::Result<PathBuf> {
    let metadata = build_chain_metadata(run_id, source_path);
    let path = paths::run_chain_meta_path(run_dir);
    policy_registry::write_atomic_json(&path, &metadata)?;
    Ok(path)
}

pub fn show(project_root: &Path, run_id: &str) -> anyhow::Result<ChainMetadata> {
    let canonical_root = policy_registry::canonical_project_root(project_root)?;
    let candidate = cll::find_candidate_run(&canonical_root, run_id)?
        .ok_or_else(|| anyhow!("run not found: {run_id}"))?;
    let meta_path = paths::run_chain_meta_path(&candidate.run_dir);

    serde_json::from_str(
        &fs::read_to_string(&meta_path)
            .with_context(|| format!("read chain meta: {}", meta_path.display()))?,
    )
    .with_context(|| format!("parse chain meta: {}", meta_path.display()))
}

pub fn write_operator_output(
    project_root: &Path,
    run_id: &str,
) -> anyhow::Result<(ChainMetadata, PathBuf)> {
    let canonical_root = policy_registry::canonical_project_root(project_root)?;
    let metadata = show(&canonical_root, run_id)?;
    let output_path = paths::plugin_chain_operator_output_path(&canonical_root, run_id);
    policy_registry::write_atomic_json(&output_path, &metadata)?;
    Ok((metadata, output_path))
}

fn build_chain_metadata(run_id: &str, source_path: &Path) -> ChainMetadata {
    let candidates = candidate_paths(source_path);
    let selected = candidates.iter().find(|path| path.exists()).cloned();

    let (sidecar_status, advisory) = match selected.as_ref() {
        None => ("absent".to_string(), None),
        Some(path) => match fs::read_to_string(path) {
            Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(_) => ("selected".to_string(), None),
                Err(_) => (
                    "invalid".to_string(),
                    Some(ChainAdvisory {
                        code: "invalid_sidecar".to_string(),
                        message: "selected sidecar is not valid JSON".to_string(),
                    }),
                ),
            },
            Err(_) => (
                "unreadable".to_string(),
                Some(ChainAdvisory {
                    code: "unreadable_sidecar".to_string(),
                    message: "selected sidecar could not be read".to_string(),
                }),
            ),
        },
    };

    ChainMetadata {
        schema_version: SCHEMA_VERSION.to_string(),
        run_id: run_id.to_string(),
        source_path: source_path.to_string_lossy().to_string(),
        sidecar_candidates: candidates
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect(),
        selected_sidecar_path: selected.map(|path| path.to_string_lossy().to_string()),
        sidecar_status,
        compiler_status: "todo".to_string(),
        not_implemented: true,
        advisory,
    }
}

fn candidate_paths(source_path: &Path) -> Vec<PathBuf> {
    let mut source_chain = OsString::from(source_path.as_os_str());
    source_chain.push(".chain.json");
    let source_chain = PathBuf::from(source_chain);

    let stem = source_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("source");
    let stem_chain = source_path.with_file_name(format!("{stem}.chain.json"));

    vec![source_chain, stem_chain]
}

#[cfg(test)]
mod tests {
    use super::{show, write_operator_output, write_worker_chain_meta};
    use crate::paths;
    use std::{io::Write, path::PathBuf};

    #[test]
    fn precedence_prefers_source_chain_over_stem_chain() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        let source_path = td.path().join("audio/mix.wav");
        let run_dir = project_root.join("SCE/assets/asset-a/analysis/run-1");

        std::fs::create_dir_all(source_path.parent().expect("audio parent")).expect("mkdir");
        std::fs::create_dir_all(&run_dir).expect("mkdir run");
        std::fs::write(&source_path, b"wave").expect("write source");
        std::fs::write(
            source_path.with_extension("chain.json"),
            b"{\"valid\":true}",
        )
        .expect("write stem chain");
        std::fs::write(format!("{}.chain.json", source_path.display()), b"not-json")
            .expect("write source chain");

        write_worker_chain_meta(&run_dir, "run-1", &source_path).expect("write meta");
        let shown = show(&project_root, "run-1").expect("show chain");

        assert_eq!(shown.sidecar_status, "invalid");
        assert_eq!(
            shown.selected_sidecar_path,
            Some(format!("{}.chain.json", source_path.display()))
        );
        assert_eq!(shown.advisory.expect("advisory").code, "invalid_sidecar");
    }

    #[test]
    fn operator_output_reuses_written_chain_meta() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        let source_path = td.path().join("audio/mix.wav");
        let run_dir = project_root.join("SCE/assets/asset-a/analysis/run-1");

        std::fs::create_dir_all(source_path.parent().expect("audio parent")).expect("mkdir");
        std::fs::create_dir_all(&run_dir).expect("mkdir run");
        std::fs::write(&source_path, b"wave").expect("write source");
        std::fs::write(
            format!("{}.chain.json", source_path.display()),
            b"{\"valid\":true}",
        )
        .expect("write source chain");
        write_worker_chain_meta(&run_dir, "run-1", &source_path).expect("write meta");

        let (operator_output, operator_path) =
            write_operator_output(&project_root, "run-1").expect("operator output");
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(operator_path).expect("read operator"))
                .expect("parse operator");

        assert_eq!(operator_output.sidecar_status, "selected");
        assert_eq!(written["schema_version"], "1.0");
    }

    #[test]
    fn unreadable_sidecar_is_advisory_only() {
        let td = tempfile::tempdir().expect("tempdir");
        let source_path = td.path().join("audio/mix.wav");
        let run_dir = td.path().join("project/SCE/assets/asset-a/analysis/run-1");

        std::fs::create_dir_all(source_path.parent().expect("audio parent")).expect("mkdir");
        std::fs::create_dir_all(&run_dir).expect("mkdir run");
        std::fs::write(&source_path, b"wave").expect("write source");

        let sidecar_path = PathBuf::from(format!("{}.chain.json", source_path.display()));
        let mut file = std::fs::File::create(&sidecar_path).expect("create sidecar");
        file.write_all(b"{\"valid\": true}").expect("write sidecar");
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&sidecar_path, std::fs::Permissions::from_mode(0o000))
                .expect("chmod");
        }

        let metadata = write_worker_chain_meta(&run_dir, "run-1", &source_path)
            .and_then(|path| {
                serde_json::from_str::<serde_json::Value>(
                    &std::fs::read_to_string(path).expect("read meta"),
                )
                .map_err(Into::into)
            })
            .expect("metadata");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&sidecar_path, std::fs::Permissions::from_mode(0o644))
                .expect("restore perms");
        }

        assert_eq!(metadata["sidecar_status"], "unreadable");
        assert_eq!(metadata["advisory"]["code"], "unreadable_sidecar");
        assert!(paths::run_chain_meta_path(&run_dir).exists());
    }
}
