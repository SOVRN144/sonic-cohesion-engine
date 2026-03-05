use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::path::PathBuf;
use tower_http::trace::TraceLayer;

use crate::{
    api_error::AppError,
    ingest,
    storage::Db,
    util::{self, now_rfc3339},
};

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
}

#[derive(Debug, Deserialize)]
pub struct CreateProjectReq {
    pub name: String,
    pub root_path: String,
}

#[derive(Debug, Serialize)]
pub struct CreateProjectResp {
    pub project_id: String,
}

#[derive(Debug, Deserialize)]
pub struct RegisterAssetReq {
    pub file_path: String,
    pub kind: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
struct ProjectListRow {
    id: String,
    name: String,
    root_path: String,
}

const DEFAULT_CONSTITUTION_YAML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../shared/templates/constitutions/constitution.default.yaml"
));

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(|| async { "ok" }))
        .route("/v1/projects", post(create_project).get(list_projects))
        .route("/v1/projects/:project_id/assets", post(register_asset))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

async fn create_project(
    State(state): State<AppState>,
    Json(req): Json<CreateProjectReq>,
) -> Result<Json<CreateProjectResp>, AppError> {
    if req.name.trim().is_empty() {
        return Err(AppError::BadRequest("project name cannot be empty".into()));
    }

    let root_requested = PathBuf::from(&req.root_path);
    tokio::fs::create_dir_all(&root_requested)
        .await
        .map_err(|e| {
            AppError::BadRequest(format!("cannot create root_path: {} ({e})", req.root_path))
        })?;

    let root = tokio::fs::canonicalize(&root_requested)
        .await
        .map_err(|e| AppError::BadRequest(format!("invalid root_path: {} ({e})", req.root_path)))?;

    let sce_dir = root.join("SCE");
    tokio::fs::create_dir_all(&sce_dir).await?;

    let constitution_path = sce_dir.join("constitution.yaml");
    if tokio::fs::metadata(&constitution_path).await.is_err() {
        tokio::fs::write(&constitution_path, DEFAULT_CONSTITUTION_YAML).await?;
    }

    let project_id = uuid::Uuid::new_v4().to_string();
    let now = now_rfc3339();

    sqlx::query(
        r#"
        INSERT INTO projects (id, name, root_path, constitution_path, watch_paths_json, created_at, updated_at)
        VALUES (?, ?, ?, ?, '[]', ?, ?)
        "#,
    )
    .bind(&project_id)
    .bind(req.name.trim())
    .bind(root.to_string_lossy().to_string())
    .bind(constitution_path.to_string_lossy().to_string())
    .bind(&now)
    .bind(&now)
    .execute(state.db.pool())
    .await?;

    Ok(Json(CreateProjectResp { project_id }))
}

async fn list_projects(State(state): State<AppState>) -> Result<Json<serde_json::Value>, AppError> {
    let rows: Vec<ProjectListRow> =
        sqlx::query_as("SELECT id, name, root_path FROM projects ORDER BY created_at DESC")
            .fetch_all(state.db.pool())
            .await?;

    Ok(Json(serde_json::json!({ "projects": rows })))
}

async fn register_asset(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Json(req): Json<RegisterAssetReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let parsed = uuid::Uuid::parse_str(&project_id)
        .map_err(|_| AppError::BadRequest("project_id must be a canonical UUID string".into()))?;
    if parsed.to_string() != project_id {
        return Err(AppError::BadRequest(
            "project_id must be lowercase hyphenated UUID".into(),
        ));
    }

    let source = PathBuf::from(&req.file_path);
    let extension = util::audio_extension(&source).unwrap_or_else(|| "unknown".to_string());
    if extension == "m4a" {
        return Err(AppError::BadRequest(
            "unsupported format: m4a (supported: wav, aiff, aif, flac, mp3)".into(),
        ));
    }
    if !util::is_audio_file(&source) {
        return Err(AppError::BadRequest(format!(
            "unsupported format: {extension} (supported: wav, aiff, aif, flac, mp3)"
        )));
    }

    let kind = req.kind.unwrap_or_else(|| "mix".to_string());
    let (_asset_id, run_id) =
        ingest::register_asset_and_enqueue(&state.db, &project_id, &source, &kind).await?;

    Ok(Json(serde_json::json!({ "queued_run_id": run_id })))
}
