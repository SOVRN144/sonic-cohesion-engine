use crate::{api_error::AppError, paths, storage::Db, util};
use anyhow::Context;
use sqlx::FromRow;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const ANALYZER_VERSION: &str = "TelemetryAnalyzer/1.0.0";

#[derive(Debug, FromRow)]
struct ProjectRoot {
    root_path: String,
}

#[derive(Debug, FromRow)]
struct ExistingAsset {
    id: String,
}

pub async fn register_asset_and_enqueue(
    db: &Db,
    project_id: &str,
    file_path: &Path,
    kind: &str,
) -> Result<(String, String), AppError> {
    let project: ProjectRoot = sqlx::query_as("SELECT root_path FROM projects WHERE id=?")
        .bind(project_id)
        .fetch_optional(db.pool())
        .await?
        .ok_or_else(|| AppError::BadRequest(format!("project not found: {project_id}")))?;

    let root = tokio::fs::canonicalize(&project.root_path)
        .await
        .map_err(|e| {
            AppError::BadRequest(format!(
                "invalid project root path: {} ({e})",
                project.root_path
            ))
        })?;

    let src = tokio::fs::canonicalize(file_path).await.map_err(|e| {
        AppError::BadRequest(format!("invalid asset path: {} ({e})", file_path.display()))
    })?;

    if !src.starts_with(&root) {
        return Err(AppError::BadRequest(format!(
            "asset path must be inside project root: {}",
            root.display()
        )));
    }

    let hash = util::sha256_file(&src)?;
    let format = src
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("bin")
        .to_ascii_lowercase();

    let existing: Option<ExistingAsset> =
        sqlx::query_as("SELECT id FROM assets WHERE project_id=? AND content_hash=?")
            .bind(project_id)
            .bind(&hash)
            .fetch_optional(db.pool())
            .await?;

    let asset_id = if let Some(asset) = existing {
        // Dedupe hit: keep-first semantics. Reuse existing asset_id and paths.
        asset.id
    } else {
        let asset_id = Uuid::new_v4().to_string();
        let dest_path = materialize_internal_copy(&root, &asset_id, &src, &format).await?;

        let created_at = util::now_rfc3339();
        sqlx::query(
            r#"
            INSERT INTO assets (id, project_id, source_path, file_path, content_hash, kind, tags_json, format, created_at)
            VALUES (?, ?, ?, ?, ?, ?, '[]', ?, ?)
            "#,
        )
        .bind(&asset_id)
        .bind(project_id)
        .bind(src.to_string_lossy().to_string())
        .bind(dest_path.to_string_lossy().to_string())
        .bind(&hash)
        .bind(kind)
        .bind(&format)
        .bind(created_at)
        .execute(db.pool())
        .await
        .with_context(|| "insert asset")?;

        asset_id
    };

    let run_id = Uuid::new_v4().to_string();
    let queued_at = util::now_rfc3339();

    sqlx::query(
        r#"
        INSERT INTO analysis_runs (id, asset_id, status, analyzer_version, queued_at)
        VALUES (?, ?, 'queued', ?, ?)
        "#,
    )
    .bind(&run_id)
    .bind(&asset_id)
    .bind(ANALYZER_VERSION)
    .bind(&queued_at)
    .execute(db.pool())
    .await
    .with_context(|| "insert analysis run")?;

    Ok((asset_id, run_id))
}

async fn materialize_internal_copy(
    root: &Path,
    asset_id: &str,
    src: &Path,
    ext: &str,
) -> Result<PathBuf, AppError> {
    let dest_dir = paths::asset_dir(root, asset_id);
    tokio::fs::create_dir_all(&dest_dir)
        .await
        .with_context(|| format!("create asset dir: {}", dest_dir.display()))?;

    let dest = dest_dir.join(format!("original.{ext}"));

    if tokio::fs::metadata(&dest).await.is_ok() {
        tokio::fs::remove_file(&dest)
            .await
            .with_context(|| format!("remove stale destination: {}", dest.display()))?;
    }

    match std::fs::hard_link(src, &dest) {
        Ok(_) => {
            tracing::debug!(
                source = %src.display(),
                destination = %dest.display(),
                "materialized internal artifact via hard link"
            );
        }
        Err(err) => {
            tracing::debug!(
                source = %src.display(),
                destination = %dest.display(),
                reason = %err,
                "hard link failed; falling back to copy"
            );
            tokio::fs::copy(src, &dest)
                .await
                .with_context(|| format!("copy source to internal artifact: {}", dest.display()))?;
        }
    }

    let canonical_dest = tokio::fs::canonicalize(&dest).await.map_err(|e| {
        AppError::Anyhow(anyhow::anyhow!(
            "canonicalize internal artifact path {}: {e}",
            dest.display()
        ))
    })?;

    Ok(canonical_dest)
}

#[cfg(test)]
mod tests {
    use super::materialize_internal_copy;

    #[tokio::test]
    async fn creates_internal_original_file() {
        let td = tempfile::tempdir().expect("tempdir");
        let root = td.path().join("project");
        tokio::fs::create_dir_all(&root).await.expect("mkdir");

        let src = root.join("source.wav");
        tokio::fs::write(&src, b"wave").await.expect("write src");

        let got = materialize_internal_copy(&root, "asset-1", &src, "wav")
            .await
            .expect("materialize");

        assert!(got.ends_with("original.wav"));
        assert!(tokio::fs::metadata(got).await.is_ok());
    }
}
