use std::path::{Path, PathBuf};

pub fn project_sce_dir(project_root: &Path) -> PathBuf {
    project_root.join("SCE")
}

pub fn asset_dir(project_root: &Path, asset_id: &str) -> PathBuf {
    project_sce_dir(project_root).join("assets").join(asset_id)
}

pub fn run_dir(project_root: &Path, asset_id: &str, run_id: &str) -> PathBuf {
    asset_dir(project_root, asset_id)
        .join("analysis")
        .join(run_id)
}
