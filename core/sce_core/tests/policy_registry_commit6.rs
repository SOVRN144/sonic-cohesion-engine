use sce_core::{ingest, paths, policy_registry, storage::Db, util, worker};
use sqlx::Row;
use std::{io::Write, path::Path, time::Duration};
use tokio::sync::watch;
use uuid::Uuid;

#[tokio::test]
async fn migration_adds_constitution_version_and_policy_canary_table() {
    let td = tempfile::tempdir().expect("tempdir");
    let db = setup_db(td.path()).await;

    let table_info = sqlx::query("PRAGMA table_info('analysis_runs')")
        .fetch_all(db.pool())
        .await
        .expect("query table info");

    let column = table_info
        .iter()
        .find(|row| row.get::<String, _>("name") == "constitution_version")
        .expect("constitution_version column exists");

    assert_eq!(column.get::<i64, _>("notnull"), 1);
    let default_value = column
        .get::<Option<String>, _>("dflt_value")
        .unwrap_or_default();
    assert!(default_value.contains("active"));

    let canary_exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='policy_canary_state'",
    )
    .fetch_one(db.pool())
    .await
    .expect("table exists query");
    assert_eq!(canary_exists, 1);
}

#[tokio::test]
async fn enqueue_uses_db_canary_state_even_if_projection_is_stale() {
    let td = tempfile::tempdir().expect("tempdir");
    let db = setup_db(td.path()).await;
    let root = td.path().join("project");

    let project_id = seed_project_with_registry(&db, &root).await;
    add_candidate_version(&root, "v1.1", -9.0);

    policy_registry::set_canary_budget(&db, &root, "v1.1", 1)
        .await
        .expect("set canary");

    // Stale projection should never influence enqueue decisions.
    let stale_projection_path = paths::registry_canary_projection_path(&root, "v1.1");
    std::fs::create_dir_all(stale_projection_path.parent().expect("parent")).expect("mkdir");
    std::fs::write(
        &stale_projection_path,
        r#"{
  "project_id": "stale",
  "candidate_version": "v1.1",
  "remaining_budget": 999,
  "started_at": "2026-03-06T00:00:00.000Z",
  "updated_at": "2026-03-06T00:00:00.000Z"
}
"#,
    )
    .expect("write stale projection");

    let first_asset = root.join("audio/first.wav");
    let second_asset = root.join("audio/second.wav");
    std::fs::create_dir_all(first_asset.parent().expect("parent")).expect("mkdir audio");
    write_wav_i16(&first_asset, 48_000, 2, 4_800, |idx, _| {
        let t = idx as f64 / 48_000.0;
        (std::f64::consts::TAU * 220.0 * t).sin() * 0.3
    })
    .expect("write first wav");
    write_wav_i16(&second_asset, 48_000, 2, 4_800, |idx, _| {
        let t = idx as f64 / 48_000.0;
        (std::f64::consts::TAU * 330.0 * t).sin() * 0.3
    })
    .expect("write second wav");

    let (_, first_run) = ingest::register_asset_and_enqueue(&db, &project_id, &first_asset, "mix")
        .await
        .expect("enqueue first");
    let (_, second_run) =
        ingest::register_asset_and_enqueue(&db, &project_id, &second_asset, "mix")
            .await
            .expect("enqueue second");

    let first_version: String =
        sqlx::query_scalar("SELECT constitution_version FROM analysis_runs WHERE id=?")
            .bind(&first_run)
            .fetch_one(db.pool())
            .await
            .expect("first version");
    let second_version: String =
        sqlx::query_scalar("SELECT constitution_version FROM analysis_runs WHERE id=?")
            .bind(&second_run)
            .fetch_one(db.pool())
            .await
            .expect("second version");

    assert_eq!(first_version, "v1.1");
    assert_eq!(second_version, "v1.0");
}

#[tokio::test]
async fn canary_budget_one_pins_exactly_one_parallel_enqueue() {
    let td = tempfile::tempdir().expect("tempdir");
    let db = setup_db(td.path()).await;
    let root = td.path().join("project");

    let project_id = seed_project_with_registry(&db, &root).await;
    add_candidate_version(&root, "v1.1", -8.0);

    policy_registry::set_canary_budget(&db, &root, "v1.1", 1)
        .await
        .expect("set canary");

    let first = root.join("audio/parallel-1.wav");
    let second = root.join("audio/parallel-2.wav");
    std::fs::create_dir_all(first.parent().expect("parent")).expect("mkdir audio");

    write_wav_i16(&first, 48_000, 2, 4_800, |idx, _| {
        let t = idx as f64 / 48_000.0;
        (std::f64::consts::TAU * 440.0 * t).sin() * 0.4
    })
    .expect("write wav 1");
    write_wav_i16(&second, 48_000, 2, 4_800, |idx, _| {
        let t = idx as f64 / 48_000.0;
        (std::f64::consts::TAU * 550.0 * t).sin() * 0.4
    })
    .expect("write wav 2");

    let db_one = db.clone();
    let db_two = db.clone();
    let project_one = project_id.clone();
    let project_two = project_id.clone();
    let first_path = first.clone();
    let second_path = second.clone();

    let enqueue_one = tokio::spawn(async move {
        ingest::register_asset_and_enqueue(&db_one, &project_one, &first_path, "mix")
            .await
            .expect("enqueue one")
            .1
    });
    let enqueue_two = tokio::spawn(async move {
        ingest::register_asset_and_enqueue(&db_two, &project_two, &second_path, "mix")
            .await
            .expect("enqueue two")
            .1
    });

    let run_a = enqueue_one.await.expect("join one");
    let run_b = enqueue_two.await.expect("join two");

    let rows = sqlx::query("SELECT id, constitution_version FROM analysis_runs WHERE id IN (?, ?)")
        .bind(&run_a)
        .bind(&run_b)
        .fetch_all(db.pool())
        .await
        .expect("fetch rows");

    let pinned_versions = rows
        .iter()
        .map(|row| row.get::<String, _>("constitution_version"))
        .collect::<Vec<_>>();

    let candidate_count = pinned_versions
        .iter()
        .filter(|version| *version == "v1.1")
        .count();
    assert_eq!(candidate_count, 1);
}

#[tokio::test]
async fn enqueue_fails_fast_when_active_pointer_missing() {
    let td = tempfile::tempdir().expect("tempdir");
    let db = setup_db(td.path()).await;
    let root = td.path().join("project");

    let project_id = seed_project_with_registry(&db, &root).await;

    let active_path = paths::registry_active_path(&root);
    std::fs::remove_file(&active_path).expect("remove active pointer");

    let wav_path = root.join("audio/fail-active.wav");
    std::fs::create_dir_all(wav_path.parent().expect("parent")).expect("mkdir");
    write_wav_i16(&wav_path, 48_000, 2, 2_400, |idx, _| {
        let t = idx as f64 / 48_000.0;
        (std::f64::consts::TAU * 110.0 * t).sin() * 0.2
    })
    .expect("write wav");

    let err = ingest::register_asset_and_enqueue(&db, &project_id, &wav_path, "mix")
        .await
        .expect_err("enqueue should fail");
    assert!(
        err.to_string().contains("active.json missing/unreadable"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn enqueue_rolls_back_when_canary_candidate_missing() {
    let td = tempfile::tempdir().expect("tempdir");
    let db = setup_db(td.path()).await;
    let root = td.path().join("project");

    let project_id = seed_project_with_registry(&db, &root).await;

    let now = util::now_rfc3339();
    sqlx::query(
        r#"
        INSERT INTO policy_canary_state (project_id, candidate_version, remaining_budget, started_at, updated_at)
        VALUES (?, 'v9.9', 1, ?, ?)
        "#,
    )
    .bind(&project_id)
    .bind(&now)
    .bind(&now)
    .execute(db.pool())
    .await
    .expect("insert stale canary");

    let wav_path = root.join("audio/fail-missing-candidate.wav");
    std::fs::create_dir_all(wav_path.parent().expect("parent")).expect("mkdir");
    write_wav_i16(&wav_path, 48_000, 2, 2_400, |idx, _| {
        let t = idx as f64 / 48_000.0;
        (std::f64::consts::TAU * 180.0 * t).sin() * 0.2
    })
    .expect("write wav");

    let err = ingest::register_asset_and_enqueue(&db, &project_id, &wav_path, "mix")
        .await
        .expect_err("enqueue should fail");
    assert!(
        err.to_string()
            .contains("canary candidate constitution missing"),
        "unexpected error: {err}"
    );

    let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM analysis_runs")
        .fetch_one(db.pool())
        .await
        .expect("run count");
    assert_eq!(run_count, 0);

    let remaining_budget: i64 =
        sqlx::query_scalar("SELECT remaining_budget FROM policy_canary_state WHERE project_id=?")
            .bind(&project_id)
            .fetch_one(db.pool())
            .await
            .expect("remaining budget");
    assert_eq!(remaining_budget, 1);
}

#[tokio::test]
async fn worker_meta_contains_pinned_constitution_provenance() {
    let td = tempfile::tempdir().expect("tempdir");
    let db = setup_db(td.path()).await;
    let root = td.path().join("project");

    let project_id = seed_project_with_registry(&db, &root).await;
    add_candidate_version(&root, "v1.1", -12.0);

    policy_registry::set_canary_budget(&db, &root, "v1.1", 1)
        .await
        .expect("set canary");

    let wav_path = root.join("audio/provenance.wav");
    std::fs::create_dir_all(wav_path.parent().expect("parent")).expect("mkdir");
    write_wav_i16(&wav_path, 48_000, 2, 48_000, |idx, _| {
        let t = idx as f64 / 48_000.0;
        (std::f64::consts::TAU * 260.0 * t).sin() * 0.3
    })
    .expect("write wav");

    let (_, run_id) = ingest::register_asset_and_enqueue(&db, &project_id, &wav_path, "mix")
        .await
        .expect("enqueue");

    run_worker_once(db.clone()).await;

    let row = sqlx::query("SELECT status, asset_id FROM analysis_runs WHERE id=?")
        .bind(&run_id)
        .fetch_one(db.pool())
        .await
        .expect("fetch run row");
    assert_eq!(row.get::<String, _>("status"), "done");

    let asset_id = row.get::<String, _>("asset_id");
    let metrics_path = paths::run_dir(&root, &asset_id, &run_id).join("metrics.json");
    let metrics: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&metrics_path).expect("read metrics json"))
            .expect("parse metrics json");

    let candidate_path = paths::registry_constitution_path(&root, "v1.1");
    let expected_hash = util::sha256_file(&candidate_path).expect("hash candidate file");

    assert_eq!(metrics["meta"]["constitution_version"], "v1.1");
    assert_eq!(metrics["meta"]["constitution_hash"], expected_hash);
    let recorded_path = metrics["meta"]["constitution_path"]
        .as_str()
        .expect("meta constitution_path");
    let canonical_recorded = std::fs::canonicalize(recorded_path).expect("canonical recorded path");
    let canonical_candidate =
        std::fs::canonicalize(&candidate_path).expect("canonical candidate path");
    assert_eq!(
        canonical_recorded, canonical_candidate,
        "constitution_path should point to pinned registry candidate"
    );
}

async fn setup_db(temp_root: &Path) -> Db {
    let db_path = temp_root.join("test.db");
    let db_url = format!("sqlite://{}?mode=rwc", db_path.display());
    Db::connect(&db_url).await.expect("connect db")
}

async fn seed_project_with_registry(db: &Db, root: &Path) -> String {
    std::fs::create_dir_all(root.join("SCE")).expect("mkdir SCE");

    let constitution_path = root.join("SCE/constitution.yaml");
    std::fs::write(
        &constitution_path,
        r#"constitution_version: "v1.0"
audio_contract:
  allowed_sample_rates_hz: [44100, 48000]
  allowed_bit_depths: [16, 24]
  max_true_peak_dbtp: -1.0
  clipping_allowed: false
"#,
    )
    .expect("seed constitution");

    let canonical_root = std::fs::canonicalize(root).expect("canonical root");
    let canonical_constitution = std::fs::canonicalize(&constitution_path).expect("canonical c");

    let project_id = Uuid::new_v4().to_string();
    let now = util::now_rfc3339();

    sqlx::query(
        r#"
        INSERT INTO projects (id, name, root_path, constitution_path, watch_paths_json, created_at, updated_at)
        VALUES (?, ?, ?, ?, '[]', ?, ?)
        "#,
    )
    .bind(&project_id)
    .bind("commit6-test")
    .bind(canonical_root.to_string_lossy().to_string())
    .bind(canonical_constitution.to_string_lossy().to_string())
    .bind(&now)
    .bind(&now)
    .execute(db.pool())
    .await
    .expect("insert project");

    policy_registry::init_registry(&canonical_root, "v1.0").expect("init registry");

    project_id
}

fn add_candidate_version(project_root: &Path, version: &str, ceiling: f64) {
    let td = tempfile::tempdir().expect("tempdir for candidate");
    let candidate = td.path().join("candidate.yaml");
    std::fs::write(
        &candidate,
        format!(
            "constitution_version: \"{version}\"\naudio_contract:\n  allowed_sample_rates_hz: [44100, 48000]\n  allowed_bit_depths: [16, 24]\n  max_true_peak_dbtp: {ceiling}\n  clipping_allowed: false\n"
        ),
    )
    .expect("write candidate yaml");

    policy_registry::add_registry_constitution(project_root, version, &candidate, None)
        .expect("add candidate version");
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

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

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
