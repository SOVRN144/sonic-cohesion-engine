use crate::{
    analyzer, constitution, drift, gates, paths, policy_registry,
    reports::{self, ReportMeta},
    storage::Db,
    util,
};
use anyhow::Context;
use sqlx::FromRow;
use std::{path::PathBuf, time::Duration};
use tokio::sync::watch;

#[derive(Debug, FromRow)]
struct ClaimedRun {
    id: String,
    asset_id: String,
    analyzer_version: String,
    constitution_version: String,
}

#[derive(Debug, FromRow)]
struct AssetProjectContext {
    project_id: String,
    asset_id: String,
    source_path: String,
    file_path: String,
    content_hash: String,
    bit_depth: Option<i64>,
    root_path: String,
    constitution_path: String,
}

pub async fn run_worker_loop(db: Db, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
    recover_running_without_started_at(&db).await?;

    loop {
        if *shutdown.borrow() {
            tracing::info!("worker shutdown requested");
            break;
        }

        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!("worker shutdown signal received");
                    break;
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
        }

        let started_at = util::now_rfc3339();
        let Some(run) = claim_next_run(&db, &started_at).await? else {
            continue;
        };

        if let Err(err) = process_run(&db, &run, &started_at).await {
            let finished_at = util::now_rfc3339();
            mark_failed(&db, &run.id, &finished_at, &format!("{err:#}")).await?;
        }
    }

    Ok(())
}

async fn process_run(db: &Db, run: &ClaimedRun, started_at: &str) -> anyhow::Result<()> {
    let ctx: AssetProjectContext = sqlx::query_as(
        r#"
        SELECT a.project_id, a.id as asset_id, a.source_path, a.file_path, a.content_hash, a.bit_depth, p.root_path, p.constitution_path
        FROM assets a
        INNER JOIN projects p ON p.id = a.project_id
        WHERE a.id=?
        "#,
    )
    .bind(&run.asset_id)
    .fetch_one(db.pool())
    .await
    .with_context(|| format!("load context for asset {}", run.asset_id))?;

    let internal_path = PathBuf::from(&ctx.file_path);
    if let Err(err) = tokio::fs::metadata(&internal_path).await {
        let finished_at = util::now_rfc3339();
        mark_failed(
            db,
            &run.id,
            &finished_at,
            &format!(
                "internal copy missing: {} (source={}) ({err})",
                ctx.file_path, ctx.source_path
            ),
        )
        .await?;
        return Ok(());
    }

    let project_root = PathBuf::from(&ctx.root_path);
    let constitution_path = PathBuf::from(&ctx.constitution_path);
    let resolved_constitution = match policy_registry::resolve_worker_constitution(
        &project_root,
        &constitution_path,
        &run.constitution_version,
    ) {
        Ok(resolved) => resolved,
        Err(err) => {
            let finished_at = util::now_rfc3339();
            mark_failed(
                db,
                &run.id,
                &finished_at,
                &format!(
                    "constitution resolution failed for version {}: {err:#}",
                    run.constitution_version
                ),
            )
            .await?;
            return Ok(());
        }
    };

    let metrics = match analyzer::analyze_file(&internal_path, &run.analyzer_version).await {
        Ok(metrics) => metrics,
        Err(analyzer::AnalyzerError::Decode(reason)) => {
            let finished_at = util::now_rfc3339();
            let format_ext = internal_path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("bin")
                .to_ascii_lowercase();
            mark_failed(
                db,
                &run.id,
                &finished_at,
                &format!(
                    "decode failed: {reason} (file={}, format={format_ext})",
                    ctx.file_path
                ),
            )
            .await?;
            return Ok(());
        }
        Err(analyzer::AnalyzerError::Analysis(reason)) => {
            let finished_at = util::now_rfc3339();
            mark_failed(
                db,
                &run.id,
                &finished_at,
                &format!("analysis failed: {reason}"),
            )
            .await?;
            return Ok(());
        }
    };

    let availability = constitution::build_target_availability(
        &resolved_constitution.constitution,
        &metrics,
        ctx.bit_depth,
    );
    let gate_eval = gates::evaluate_gates(
        &metrics,
        &resolved_constitution.constitution,
        &availability,
        ctx.bit_depth,
    );
    let drift_result =
        drift::compute_drift(&metrics, &resolved_constitution.constitution, &availability);

    let finished_at = util::now_rfc3339();
    let created_at = util::now_rfc3339();

    let run_dir = paths::run_dir(
        PathBuf::from(&ctx.root_path).as_path(),
        &ctx.asset_id,
        &run.id,
    );
    let meta = ReportMeta {
        project_id: ctx.project_id.clone(),
        asset_id: ctx.asset_id.clone(),
        run_id: run.id.clone(),
        asset_content_hash: ctx.content_hash.clone(),
        constitution_version: resolved_constitution.constitution_version,
        constitution_hash: resolved_constitution.constitution_hash,
        constitution_path: resolved_constitution.constitution_path,
        analyzer_version: run.analyzer_version.clone(),
        created_at,
        started_at: started_at.to_string(),
        finished_at: finished_at.clone(),
    };

    if let Err(err) = reports::write_report(&run_dir, &metrics, &gate_eval, &drift_result, &meta)
        .await
        .with_context(|| format!("write report for run {}", run.id))
    {
        let finished_at = util::now_rfc3339();
        mark_failed(
            db,
            &run.id,
            &finished_at,
            &format!("report write failed: {err:#}"),
        )
        .await?;
        return Ok(());
    }

    mark_done(db, &run.id, &finished_at).await?;
    tracing::info!(run_id = %run.id, asset_id = %ctx.asset_id, "analysis run complete");

    Ok(())
}

async fn claim_next_run(db: &Db, now: &str) -> anyhow::Result<Option<ClaimedRun>> {
    let claimed: Option<ClaimedRun> = sqlx::query_as(
        r#"
        UPDATE analysis_runs
        SET status='running', started_at=COALESCE(started_at, ?)
        WHERE id = (
          SELECT id FROM analysis_runs
          WHERE status='queued'
          ORDER BY queued_at ASC, id ASC
          LIMIT 1
        )
        AND status='queued'
        RETURNING id, asset_id, analyzer_version, constitution_version
        "#,
    )
    .bind(now)
    .fetch_optional(db.pool())
    .await?;

    Ok(claimed)
}

async fn mark_done(db: &Db, run_id: &str, finished_at: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE analysis_runs SET status='done', finished_at=?, error_message=NULL WHERE id=? AND status='running'",
    )
    .bind(finished_at)
    .bind(run_id)
    .execute(db.pool())
    .await?;

    Ok(())
}

async fn mark_failed(
    db: &Db,
    run_id: &str,
    finished_at: &str,
    message: &str,
) -> anyhow::Result<()> {
    let message = if message.trim().is_empty() {
        "unknown failure"
    } else {
        message
    };

    sqlx::query(
        "UPDATE analysis_runs SET status='failed', finished_at=?, error_message=? WHERE id=?",
    )
    .bind(finished_at)
    .bind(message)
    .bind(run_id)
    .execute(db.pool())
    .await?;

    Ok(())
}

async fn recover_running_without_started_at(db: &Db) -> anyhow::Result<()> {
    let finished_at = util::now_rfc3339();
    let result = sqlx::query(
        r#"
        UPDATE analysis_runs
        SET status='failed', finished_at=?, error_message='running with no started_at'
        WHERE status='running' AND started_at IS NULL
        "#,
    )
    .bind(finished_at)
    .execute(db.pool())
    .await?;

    if result.rows_affected() > 0 {
        tracing::warn!(
            recovered = result.rows_affected(),
            "recovered running rows with missing started_at"
        );
    }

    Ok(())
}
