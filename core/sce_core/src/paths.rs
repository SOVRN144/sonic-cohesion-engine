use std::path::{Path, PathBuf};

pub fn project_sce_dir(project_root: &Path) -> PathBuf {
    project_root.join("SCE")
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
