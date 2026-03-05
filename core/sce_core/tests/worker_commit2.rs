use sce_core::{paths, storage::Db, util, worker};
use sqlx::FromRow;
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::sync::watch;
use uuid::Uuid;

#[derive(Debug, FromRow)]
struct RunRow {
    status: String,
    started_at: Option<String>,
    finished_at: Option<String>,
    error_message: Option<String>,
}

#[tokio::test]
async fn worker_marks_decode_failures_with_expected_message() {
    let td = tempfile::tempdir().expect("tempdir");
    let db = setup_db(td.path()).await;

    let root = td.path().join("project");
    tokio::fs::create_dir_all(root.join("SCE"))
        .await
        .expect("create project dir");

    let bad_mp3 = root.join("broken.mp3");
    tokio::fs::write(&bad_mp3, b"this is not an mp3")
        .await
        .expect("write corrupt mp3");

    let ids = insert_project_asset_run(&db, &root, &bad_mp3, "mp3").await;
    run_worker_once(db.clone()).await;

    let row = fetch_run(&db, &ids.run_id).await;
    assert_eq!(row.status, "failed");
    assert!(row.started_at.is_some());
    assert!(row.finished_at.is_some());

    let message = row.error_message.expect("error message");
    assert!(message.starts_with("decode failed:"));
    assert!(message.contains("(file="));
    assert!(message.contains("format=mp3)"));
}

#[tokio::test]
async fn worker_writes_non_placeholder_telemetry_artifacts() {
    let td = tempfile::tempdir().expect("tempdir");
    let db = setup_db(td.path()).await;

    let root = td.path().join("project");
    tokio::fs::create_dir_all(root.join("SCE"))
        .await
        .expect("create project dir");

    let wav_path = root.join("tone.wav");
    write_wav_i16(&wav_path, 48_000, 2, 48_000, |frame_idx, _| {
        let t = frame_idx as f64 / 48_000.0;
        (std::f64::consts::TAU * 440.0 * t).sin() * 0.4
    })
    .expect("write wav");

    let ids = insert_project_asset_run(&db, &root, &wav_path, "wav").await;
    run_worker_once(db.clone()).await;

    let row = fetch_run(&db, &ids.run_id).await;
    assert_eq!(row.status, "done");
    assert!(row.started_at.is_some());
    assert!(row.finished_at.is_some());
    assert_eq!(row.error_message, None);

    let run_dir = paths::run_dir(&root, &ids.asset_id, &ids.run_id);
    let metrics_path = run_dir.join("metrics.json");
    let gates_path = run_dir.join("gates.json");
    let drift_path = run_dir.join("drift.json");
    let report_path = run_dir.join("report.json");

    assert!(tokio::fs::metadata(&metrics_path).await.is_ok());
    assert!(tokio::fs::metadata(&gates_path).await.is_ok());
    assert!(tokio::fs::metadata(&drift_path).await.is_ok());
    assert!(tokio::fs::metadata(&report_path).await.is_ok());

    let metrics_json = tokio::fs::read_to_string(&metrics_path)
        .await
        .expect("read metrics");
    assert!(metrics_json.contains("\"sample_rate_hz\""));
    assert!(!metrics_json.contains("\"placeholder\""));

    let report_json = tokio::fs::read_to_string(&report_path)
        .await
        .expect("read report");
    assert!(report_json.contains("\"constitution_path\""));
    assert!(report_json.contains("\"asset_content_hash\""));
    assert!(report_json.contains("\"gate_status\": \"PASS\""));
    assert!(report_json.contains("\"drift_score\""));

    let gates_json = tokio::fs::read_to_string(&gates_path)
        .await
        .expect("read gates");
    assert!(gates_json.contains("\"gates\""));
    assert!(gates_json.contains("\"gate_status\""));

    let drift_json = tokio::fs::read_to_string(&drift_path)
        .await
        .expect("read drift");
    assert!(drift_json.contains("\"drift_raw\""));
    assert!(drift_json.contains("\"fix_list\""));
}

#[tokio::test]
async fn worker_processes_mp3_fixture_successfully() {
    let td = tempfile::tempdir().expect("tempdir");
    let db = setup_db(td.path()).await;

    let root = td.path().join("project");
    tokio::fs::create_dir_all(root.join("SCE"))
        .await
        .expect("create project dir");

    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny.mp3");
    let project_mp3 = root.join("tiny.mp3");
    tokio::fs::copy(&fixture_path, &project_mp3)
        .await
        .expect("copy mp3 fixture");

    let ids = insert_project_asset_run(&db, &root, &project_mp3, "mp3").await;
    run_worker_once(db.clone()).await;

    let row = fetch_run(&db, &ids.run_id).await;
    assert_eq!(row.status, "done");
    assert!(row.started_at.is_some());
    assert!(row.finished_at.is_some());
    assert_eq!(row.error_message, None);

    let run_dir = paths::run_dir(&root, &ids.asset_id, &ids.run_id);
    let metrics_path = run_dir.join("metrics.json");
    let metrics_json = tokio::fs::read_to_string(&metrics_path)
        .await
        .expect("read metrics");

    assert!(metrics_json.contains("\"lossy_source\": true"));
}

async fn setup_db(temp_root: &Path) -> Db {
    let db_path = temp_root.join("test.db");
    let db_url = format!("sqlite://{}?mode=rwc", db_path.display());
    Db::connect(&db_url).await.expect("connect db")
}

async fn insert_project_asset_run(
    db: &Db,
    root_path: &Path,
    file_path: &Path,
    format: &str,
) -> InsertIds {
    let root = tokio::fs::canonicalize(root_path)
        .await
        .expect("canonical root path");

    let constitution_path = root.join("SCE/constitution.yaml");
    tokio::fs::write(
        &constitution_path,
        r#"constitution_version: "1.0"
project: "worker-test"
"#,
    )
    .await
    .expect("write constitution");

    let canonical_file = tokio::fs::canonicalize(file_path)
        .await
        .expect("canonical file path");

    let project_id = Uuid::new_v4().to_string();
    let asset_id = Uuid::new_v4().to_string();
    let run_id = Uuid::new_v4().to_string();
    let now = "2026-03-05T12:00:00.000Z";
    let content_hash = util::sha256_file(&canonical_file).expect("hash file");

    sqlx::query(
        r#"
        INSERT INTO projects (id, name, root_path, constitution_path, watch_paths_json, created_at, updated_at)
        VALUES (?, ?, ?, ?, '[]', ?, ?)
        "#,
    )
    .bind(&project_id)
    .bind("worker-test")
    .bind(root.to_string_lossy().to_string())
    .bind(constitution_path.to_string_lossy().to_string())
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await
    .expect("insert project");

    sqlx::query(
        r#"
        INSERT INTO assets (id, project_id, source_path, file_path, content_hash, kind, tags_json, format, created_at)
        VALUES (?, ?, ?, ?, ?, 'mix', '[]', ?, ?)
        "#,
    )
    .bind(&asset_id)
    .bind(&project_id)
    .bind(canonical_file.to_string_lossy().to_string())
    .bind(canonical_file.to_string_lossy().to_string())
    .bind(content_hash)
    .bind(format)
    .bind(now)
    .execute(db.pool())
    .await
    .expect("insert asset");

    sqlx::query(
        r#"
        INSERT INTO analysis_runs (id, asset_id, status, analyzer_version, queued_at)
        VALUES (?, ?, 'queued', 'TelemetryAnalyzer/1.0.0', ?)
        "#,
    )
    .bind(&run_id)
    .bind(&asset_id)
    .bind(now)
    .execute(db.pool())
    .await
    .expect("insert run");

    InsertIds { asset_id, run_id }
}

async fn run_worker_once(db: Db) {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(async move { worker::run_worker_loop(db, shutdown_rx).await });

    tokio::time::sleep(Duration::from_millis(1500)).await;
    let _ = shutdown_tx.send(true);

    let joined = tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("worker join timeout")
        .expect("worker join error");

    joined.expect("worker loop result");
}

async fn fetch_run(db: &Db, run_id: &str) -> RunRow {
    sqlx::query_as(
        "SELECT status, started_at, finished_at, error_message FROM analysis_runs WHERE id=?",
    )
    .bind(run_id)
    .fetch_one(db.pool())
    .await
    .expect("fetch run")
}

struct InsertIds {
    asset_id: String,
    run_id: String,
}

fn write_wav_i16<F>(
    path: &Path,
    sample_rate_hz: u32,
    channels: u16,
    frames: usize,
    mut sample_fn: F,
) -> anyhow::Result<()>
where
    F: FnMut(usize, usize) -> f64,
{
    let mut pcm = Vec::<i16>::with_capacity(frames * channels as usize);

    for frame_idx in 0..frames {
        for channel_idx in 0..channels as usize {
            let sample = sample_fn(frame_idx, channel_idx).clamp(-1.0, 1.0);
            let i16_sample = (sample * 32767.0).round() as i16;
            pcm.push(i16_sample);
        }
    }

    let data_size = (pcm.len() * 2) as u32;
    let byte_rate = sample_rate_hz * channels as u32 * 2;
    let block_align = channels * 2;
    let riff_size = 36 + data_size;

    let mut file = std::fs::File::create(path)?;
    file.write_all(b"RIFF")?;
    file.write_all(&riff_size.to_le_bytes())?;
    file.write_all(b"WAVE")?;

    file.write_all(b"fmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&channels.to_le_bytes())?;
    file.write_all(&sample_rate_hz.to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&16u16.to_le_bytes())?;

    file.write_all(b"data")?;
    file.write_all(&data_size.to_le_bytes())?;

    for sample in pcm {
        file.write_all(&sample.to_le_bytes())?;
    }

    Ok(())
}
