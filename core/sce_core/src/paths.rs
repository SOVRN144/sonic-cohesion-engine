use std::path::{Path, PathBuf};

pub fn project_sce_dir(project_root: &Path) -> PathBuf {
    project_root.join("SCE")
}

pub fn operators_dir(project_root: &Path) -> PathBuf {
    project_sce_dir(project_root).join("operators")
}

pub fn operator_dir(project_root: &Path, operator_name: &str) -> PathBuf {
    operators_dir(project_root).join(operator_name)
}

pub fn ref_canon_operator_output_path(project_root: &Path) -> PathBuf {
    operator_dir(project_root, "ref_canon").join("latest.json")
}

pub fn mixops_ci_operator_output_path(project_root: &Path) -> PathBuf {
    operator_dir(project_root, "mixops_ci").join("latest.json")
}

pub fn translation_matrix_operator_output_path(project_root: &Path, run_id: &str) -> PathBuf {
    operator_dir(project_root, "translation_matrix").join(format!("{run_id}.json"))
}

pub fn stemgraph_operator_output_path(project_root: &Path) -> PathBuf {
    operator_dir(project_root, "stemgraph").join("latest.json")
}

pub fn plugin_chain_operator_output_path(project_root: &Path, run_id: &str) -> PathBuf {
    operator_dir(project_root, "plugin_chain_compiler").join(format!("{run_id}.json"))
}

pub fn registry_dir(project_root: &Path) -> PathBuf {
    project_sce_dir(project_root).join("registry")
}

pub fn registry_lock_path(project_root: &Path) -> PathBuf {
    registry_dir(project_root).join(".lock")
}

pub fn registry_constitutions_dir(project_root: &Path) -> PathBuf {
    registry_dir(project_root).join("constitutions")
}

pub fn registry_constitution_path(project_root: &Path, version: &str) -> PathBuf {
    registry_constitutions_dir(project_root).join(format!("{version}.yaml"))
}

pub fn registry_active_path(project_root: &Path) -> PathBuf {
    registry_dir(project_root).join("active.json")
}

pub fn registry_index_path(project_root: &Path) -> PathBuf {
    registry_dir(project_root).join("index.json")
}

pub fn registry_shadow_impact_path(project_root: &Path, version: &str) -> PathBuf {
    registry_dir(project_root)
        .join("shadow")
        .join(version)
        .join("impact.json")
}

pub fn registry_canary_projection_path(project_root: &Path, version: &str) -> PathBuf {
    registry_dir(project_root)
        .join("canary")
        .join(version)
        .join("canary_state.json")
}

pub fn asset_dir(project_root: &Path, asset_id: &str) -> PathBuf {
    project_sce_dir(project_root).join("assets").join(asset_id)
}

pub fn run_dir(project_root: &Path, asset_id: &str, run_id: &str) -> PathBuf {
    asset_dir(project_root, asset_id)
        .join("analysis")
        .join(run_id)
}

pub fn refs_dir(project_root: &Path) -> PathBuf {
    project_sce_dir(project_root).join("refs")
}

pub fn refs_canon_dir(project_root: &Path) -> PathBuf {
    refs_dir(project_root).join("canon")
}

pub fn refs_canon_index_path(project_root: &Path) -> PathBuf {
    refs_canon_dir(project_root).join("index.json")
}

pub fn refs_profiles_dir(project_root: &Path) -> PathBuf {
    refs_dir(project_root).join("profiles")
}

pub fn refs_profile_path(project_root: &Path, content_hash: &str) -> PathBuf {
    refs_profiles_dir(project_root).join(format!("{content_hash}.json"))
}

pub fn refs_files_dir(project_root: &Path) -> PathBuf {
    refs_dir(project_root).join("files")
}

pub fn graph_dir(project_root: &Path) -> PathBuf {
    project_sce_dir(project_root).join("graph")
}

pub fn graph_lineage_path(project_root: &Path) -> PathBuf {
    graph_dir(project_root).join("lineage.json")
}

pub fn run_chain_meta_path(run_dir: &Path) -> PathBuf {
    run_dir.join("chain.meta.json")
}
