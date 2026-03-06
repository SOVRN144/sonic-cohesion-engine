use crate::{
    cll::{load_valid_runs, select_window},
    constitution::{self, Constitution},
    drift, gates, paths,
    storage::Db,
    util,
};
use anyhow::{anyhow, bail, ensure, Context};
use fs2::FileExt;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sqlx::FromRow;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    path::{Path, PathBuf},
};

pub const IMPACT_SCHEMA_VERSION: &str = "1.0";
pub const ACTIVE_CONSTITUTION_VERSION: &str = "active";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ActivePointer {
    pub version: String,
    pub hash: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub struct RegistryIndex {
    pub versions: Vec<RegistryVersionEntry>,
    pub active_history: Vec<RegistryActiveHistoryEntry>,
    pub thrash_warnings: Vec<RegistryThrashWarningEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct RegistryVersionEntry {
    pub version: String,
    pub hash: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct RegistryActiveHistoryEntry {
    pub version: String,
    pub hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct RegistryThrashWarningEntry {
    pub window_seconds: u64,
    pub toggles: u64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct CanaryStateProjection {
    pub project_id: String,
    pub candidate_version: String,
    pub remaining_budget: i64,
    pub started_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ImpactReport {
    pub schema_version: String,
    pub project_root: String,
    pub candidate_version: String,
    pub candidate_hash: String,
    pub requested_last_n: u64,
    pub selected_run_count: u64,
    pub new_blockers_count: u64,
    pub new_fail_count: u64,
    pub new_fail_share: f64,
    pub gate_status_delta: GateStatusDelta,
    pub drift_delta: DriftDelta,
    pub top_newly_failing_gates: Vec<GateFailCount>,
    pub risk_level: RiskLevel,
    pub risk_reasons: Vec<RiskReason>,
    pub per_run: Vec<ImpactPerRun>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct GateStatusDelta {
    pub pass: i64,
    pub warn: i64,
    pub fail: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct DriftDelta {
    pub mean: f64,
    pub median: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct GateFailCount {
    pub gate_name: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ImpactPerRun {
    pub run_id: String,
    pub finished_at: String,
    pub baseline_gate_status: ImpactGateStatus,
    pub candidate_gate_status: ImpactGateStatus,
    pub baseline_drift_score: u32,
    pub candidate_drift_score: u32,
    pub drift_delta: f64,
    pub new_fail: bool,
    pub new_blocker: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum ImpactGateStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RiskReason {
    ActivationGuardTriggered,
}

#[derive(Debug, Clone)]
pub struct RegistryWriteResult {
    pub version: String,
    pub hash: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct InitResult {
    pub active: ActivePointer,
    pub source_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ActivateResult {
    pub active: ActivePointer,
}

#[derive(Debug, Clone)]
pub struct ImpactResult {
    pub report: ImpactReport,
    pub output_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct StatusResult {
    pub canonical_project_root: PathBuf,
    pub active: Option<ActivePointer>,
    pub index: RegistryIndex,
    pub canary_state: Option<PolicyCanaryStateRow>,
    pub version_warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryLockMode {
    Shared,
    Exclusive,
}

pub struct RegistryLock {
    _file: std::fs::File,
}

impl RegistryLock {
    pub fn acquire(project_root: &Path, mode: RegistryLockMode) -> anyhow::Result<Self> {
        let registry_dir = paths::registry_dir(project_root);
        std::fs::create_dir_all(&registry_dir)
            .with_context(|| format!("create registry dir: {}", registry_dir.display()))?;

        let lock_path = paths::registry_lock_path(project_root);
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("open registry lock file: {}", lock_path.display()))?;

        match mode {
            RegistryLockMode::Shared => file.lock_shared(),
            RegistryLockMode::Exclusive => file.lock_exclusive(),
        }
        .with_context(|| {
            format!(
                "acquire {} registry lock: {}",
                if mode == RegistryLockMode::Shared {
                    "shared"
                } else {
                    "exclusive"
                },
                lock_path.display()
            )
        })?;

        Ok(Self { _file: file })
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct PolicyCanaryStateRow {
    pub project_id: String,
    pub candidate_version: String,
    pub remaining_budget: i64,
    pub started_at: String,
    pub updated_at: String,
}

#[derive(Debug, FromRow)]
pub struct CanaryConsumeResult {
    pub candidate_version: String,
    pub remaining_budget: i64,
}

impl Drop for RegistryLock {
    fn drop(&mut self) {
        let _ = self._file.unlock();
    }
}

impl From<gates::GateStatus> for ImpactGateStatus {
    fn from(value: gates::GateStatus) -> Self {
        match value {
            gates::GateStatus::Pass => Self::Pass,
            gates::GateStatus::Warn => Self::Warn,
            gates::GateStatus::Fail => Self::Fail,
        }
    }
}

pub fn round6(x: f64) -> f64 {
    debug_assert!(x.is_finite(), "impact float must be finite");
    let y = (x * 1_000_000.0).round() / 1_000_000.0;
    if y == -0.0 {
        0.0
    } else {
        y
    }
}

pub fn canonical_project_root(project_root: &Path) -> anyhow::Result<PathBuf> {
    if project_root.exists() {
        std::fs::canonicalize(project_root)
            .with_context(|| format!("canonicalize project root: {}", project_root.display()))
    } else if project_root.is_absolute() {
        Ok(project_root.to_path_buf())
    } else {
        let cwd = std::env::current_dir().with_context(|| "resolve current directory")?;
        Ok(cwd.join(project_root))
    }
}

pub fn read_active_pointer(project_root: &Path) -> anyhow::Result<ActivePointer> {
    let active_path = paths::registry_active_path(project_root);
    let pointer: ActivePointer = read_json(&active_path)
        .with_context(|| format!("read active pointer: {}", active_path.display()))?;

    ensure!(
        !pointer.version.trim().is_empty(),
        "active pointer version must be non-empty: {}",
        active_path.display()
    );
    ensure!(
        !pointer.hash.trim().is_empty(),
        "active pointer hash must be non-empty: {}",
        active_path.display()
    );
    ensure!(
        !pointer.path.trim().is_empty(),
        "active pointer path must be non-empty: {}",
        active_path.display()
    );

    Ok(pointer)
}

pub fn write_atomic_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    write_atomic_raw(path, &bytes)
}

pub fn write_atomic_bytes(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut normalized = bytes.to_vec();
    if !normalized.ends_with(b"\n") {
        normalized.push(b'\n');
    }
    write_atomic_raw(path, &normalized)
}

fn write_atomic_raw(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("missing parent directory for {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create parent directory: {}", parent.display()))?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("missing file name for {}", path.display()))?;

    let tmp_name = format!("{file_name}.tmp.{}", std::process::id());
    let tmp_path = parent.join(tmp_name);

    std::fs::write(&tmp_path, bytes)
        .with_context(|| format!("write temp file: {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "rename temp file {} -> {}",
            tmp_path.display(),
            path.display()
        )
    })?;

    Ok(())
}

pub fn init_registry(project_root: &Path, version: &str) -> anyhow::Result<InitResult> {
    let version = normalize_version(version)?;
    let canonical_root = canonical_project_root(project_root)?;
    let _lock = RegistryLock::acquire(&canonical_root, RegistryLockMode::Exclusive)?;

    let source_path = paths::project_sce_dir(&canonical_root).join("constitution.yaml");
    let source_bytes = std::fs::read(&source_path)
        .with_context(|| format!("read legacy constitution: {}", source_path.display()))?;

    let write_result = write_registry_constitution(
        &canonical_root,
        version,
        &source_bytes,
        Some("imported from constitution.yaml".to_string()),
    )?;

    let active = ActivePointer {
        version: write_result.version.clone(),
        hash: write_result.hash.clone(),
        path: write_result.path.to_string_lossy().to_string(),
    };

    let mut index = load_registry_index(&canonical_root)?;
    index.active_history.push(RegistryActiveHistoryEntry {
        version: active.version.clone(),
        hash: active.hash.clone(),
        at: Some(util::now_rfc3339()),
    });

    write_atomic_json(&paths::registry_active_path(&canonical_root), &active)?;
    write_atomic_json(&paths::registry_index_path(&canonical_root), &index)?;

    Ok(InitResult {
        active,
        source_path,
    })
}

pub fn add_registry_constitution(
    project_root: &Path,
    version: &str,
    source_path: &Path,
    note: Option<String>,
) -> anyhow::Result<RegistryWriteResult> {
    let version = normalize_version(version)?;
    let canonical_root = canonical_project_root(project_root)?;
    let _lock = RegistryLock::acquire(&canonical_root, RegistryLockMode::Exclusive)?;

    let source_bytes = std::fs::read(source_path)
        .with_context(|| format!("read constitution source bytes: {}", source_path.display()))?;

    write_registry_constitution(&canonical_root, version, &source_bytes, note)
}

pub fn activate_registry_version(
    project_root: &Path,
    version: &str,
) -> anyhow::Result<ActivateResult> {
    let version = normalize_version(version)?;
    let canonical_root = canonical_project_root(project_root)?;
    let _lock = RegistryLock::acquire(&canonical_root, RegistryLockMode::Exclusive)?;

    let (path, hash) = load_registry_version_hash(&canonical_root, version)?;
    let active = ActivePointer {
        version: version.to_string(),
        hash: hash.clone(),
        path: path.to_string_lossy().to_string(),
    };

    let mut index = load_registry_index(&canonical_root)?;
    upsert_index_version(&mut index, version, &hash, &path.to_string_lossy(), None)?;
    index.active_history.push(RegistryActiveHistoryEntry {
        version: version.to_string(),
        hash,
        at: Some(util::now_rfc3339()),
    });

    write_atomic_json(&paths::registry_active_path(&canonical_root), &active)?;
    write_atomic_json(&paths::registry_index_path(&canonical_root), &index)?;

    Ok(ActivateResult { active })
}

pub fn build_shadow_impact(
    project_root: &Path,
    version: &str,
    last_n: usize,
) -> anyhow::Result<ImpactResult> {
    let version = normalize_version(version)?;
    let canonical_root = canonical_project_root(project_root)?;
    let _lock = RegistryLock::acquire(&canonical_root, RegistryLockMode::Shared)?;

    let candidate_path = paths::registry_constitution_path(&canonical_root, version);
    let (candidate_constitution, candidate_hash) =
        constitution::load_constitution_and_hash(&candidate_path).with_context(|| {
            format!(
                "load candidate constitution from registry: {}",
                candidate_path.display()
            )
        })?;

    let loaded = load_valid_runs(&canonical_root)?;
    let selected_runs = select_window(&loaded.valid_runs, last_n);

    let mut per_run = Vec::<ImpactPerRun>::new();
    let mut baseline_counts = GateStatusAccumulator::default();
    let mut candidate_counts = GateStatusAccumulator::default();
    let mut newly_failing_gates = BTreeMap::<String, u64>::new();

    let mut new_fail_count: u64 = 0;
    let mut new_blockers_count: u64 = 0;

    for run in &selected_runs {
        let baseline_gate_status: ImpactGateStatus = run.gate_status.into();
        baseline_counts.add(baseline_gate_status);

        let availability =
            constitution::build_target_availability(&candidate_constitution, &run.metrics, None);
        let candidate_gate_eval =
            gates::evaluate_gates(&run.metrics, &candidate_constitution, &availability, None);
        let candidate_drift =
            drift::compute_drift(&run.metrics, &candidate_constitution, &availability);

        let candidate_gate_status: ImpactGateStatus = candidate_gate_eval.gate_status.into();
        candidate_counts.add(candidate_gate_status);

        let new_fail = baseline_gate_status != ImpactGateStatus::Fail
            && candidate_gate_status == ImpactGateStatus::Fail;
        let new_blocker = new_fail;

        if new_fail {
            new_fail_count += 1;
        }
        if new_blocker {
            new_blockers_count += 1;
        }

        update_newly_failing_gates(run, &candidate_gate_eval, &mut newly_failing_gates);

        let drift_delta_raw = candidate_drift.drift_score as f64 - run.drift_score as f64;
        debug_assert!(
            drift_delta_raw.is_finite(),
            "per-run drift_delta must be finite"
        );

        let drift_delta = round6(drift_delta_raw);
        debug_assert!(
            drift_delta.is_finite(),
            "rounded per-run drift_delta must be finite"
        );

        per_run.push(ImpactPerRun {
            run_id: run.run_id.clone(),
            finished_at: run.finished_at.clone(),
            baseline_gate_status,
            candidate_gate_status,
            baseline_drift_score: run.drift_score,
            candidate_drift_score: candidate_drift.drift_score,
            drift_delta,
            new_fail,
            new_blocker,
        });
    }

    per_run.sort_by(|left, right| {
        left.finished_at
            .cmp(&right.finished_at)
            .then(left.run_id.cmp(&right.run_id))
    });

    let selected_run_count = per_run.len() as u64;
    let new_fail_share = if selected_run_count == 0 {
        0.0
    } else {
        round6(new_fail_count as f64 / selected_run_count as f64)
    };
    debug_assert!(new_fail_share.is_finite(), "new_fail_share must be finite");

    let drift_values = per_run
        .iter()
        .map(|run| run.drift_delta)
        .collect::<Vec<_>>();
    let drift_mean = round6(mean(&drift_values));
    let drift_median = round6(median(&drift_values));
    debug_assert!(drift_mean.is_finite(), "drift mean must be finite");
    debug_assert!(drift_median.is_finite(), "drift median must be finite");

    let mut top_newly_failing_gates = newly_failing_gates
        .into_iter()
        .map(|(gate_name, count)| GateFailCount { gate_name, count })
        .collect::<Vec<_>>();
    top_newly_failing_gates.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then(left.gate_name.cmp(&right.gate_name))
    });

    let mut risk_reasons = BTreeSet::<RiskReason>::new();
    let risk_level = if new_fail_share > 0.80 {
        risk_reasons.insert(RiskReason::ActivationGuardTriggered);
        RiskLevel::High
    } else {
        RiskLevel::Low
    };
    let mut risk_reasons = risk_reasons.into_iter().collect::<Vec<_>>();
    risk_reasons.sort();

    let report = ImpactReport {
        schema_version: IMPACT_SCHEMA_VERSION.to_string(),
        project_root: canonical_root.to_string_lossy().to_string(),
        candidate_version: version.to_string(),
        candidate_hash,
        requested_last_n: last_n as u64,
        selected_run_count,
        new_blockers_count,
        new_fail_count,
        new_fail_share,
        gate_status_delta: GateStatusDelta {
            pass: candidate_counts.pass as i64 - baseline_counts.pass as i64,
            warn: candidate_counts.warn as i64 - baseline_counts.warn as i64,
            fail: candidate_counts.fail as i64 - baseline_counts.fail as i64,
        },
        drift_delta: DriftDelta {
            mean: drift_mean,
            median: drift_median,
        },
        top_newly_failing_gates,
        risk_level,
        risk_reasons,
        per_run,
    };

    let output_path = paths::registry_shadow_impact_path(&canonical_root, version);
    write_atomic_json(&output_path, &report)?;

    Ok(ImpactResult {
        report,
        output_path,
    })
}

pub fn build_impact(
    project_root: &Path,
    version: &str,
    last_n: usize,
) -> anyhow::Result<ImpactResult> {
    build_shadow_impact(project_root, version, last_n)
}

pub async fn set_canary_budget(
    db: &Db,
    project_root: &Path,
    version: &str,
    budget_runs: u64,
) -> anyhow::Result<PolicyCanaryStateRow> {
    let version = normalize_version(version)?;
    let canonical_root = canonical_project_root(project_root)?;
    let _lock = RegistryLock::acquire(&canonical_root, RegistryLockMode::Exclusive)?;

    let _ = load_registry_version_hash(&canonical_root, version)?;

    let project_id = find_project_id_for_root(db, &canonical_root).await?;
    let now = util::now_rfc3339();
    let remaining_budget = i64::try_from(budget_runs).context("budget-runs exceeds i64")?;

    sqlx::query(
        r#"
        INSERT INTO policy_canary_state (project_id, candidate_version, remaining_budget, started_at, updated_at)
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT(project_id)
        DO UPDATE SET
            candidate_version = excluded.candidate_version,
            remaining_budget = excluded.remaining_budget,
            started_at = excluded.started_at,
            updated_at = excluded.updated_at
        "#,
    )
    .bind(&project_id)
    .bind(version)
    .bind(remaining_budget)
    .bind(&now)
    .bind(&now)
    .execute(db.pool())
    .await?;

    let state = fetch_policy_canary_state(db, &project_id)
        .await?
        .ok_or_else(|| anyhow!("canary state row missing immediately after upsert"))?;

    if let Err(err) = write_canary_projection(&canonical_root, &state) {
        tracing::warn!(
            project_id = %state.project_id,
            version = %state.candidate_version,
            error = ?err,
            "failed to update canary projection; db state remains authoritative"
        );
    }

    Ok(state)
}

pub async fn fetch_policy_canary_state(
    db: &Db,
    project_id: &str,
) -> anyhow::Result<Option<PolicyCanaryStateRow>> {
    let row = sqlx::query_as::<_, PolicyCanaryStateRow>(
        r#"
        SELECT project_id, candidate_version, remaining_budget, started_at, updated_at
        FROM policy_canary_state
        WHERE project_id=?
        "#,
    )
    .bind(project_id)
    .fetch_optional(db.pool())
    .await?;

    Ok(row)
}

pub fn write_canary_projection(
    project_root: &Path,
    state: &PolicyCanaryStateRow,
) -> anyhow::Result<PathBuf> {
    let projection = CanaryStateProjection {
        project_id: state.project_id.clone(),
        candidate_version: state.candidate_version.clone(),
        remaining_budget: state.remaining_budget,
        started_at: state.started_at.clone(),
        updated_at: state.updated_at.clone(),
    };

    let projection_path =
        paths::registry_canary_projection_path(project_root, &state.candidate_version);
    write_atomic_json(&projection_path, &projection)?;
    Ok(projection_path)
}

pub async fn consume_canary_budget(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    project_id: &str,
    now: &str,
) -> anyhow::Result<Option<CanaryConsumeResult>> {
    let consumed: Option<CanaryConsumeResult> = sqlx::query_as(
        r#"
        UPDATE policy_canary_state
        SET remaining_budget = remaining_budget - 1, updated_at = ?
        WHERE project_id = ? AND remaining_budget > 0
        RETURNING candidate_version, remaining_budget
        "#,
    )
    .bind(now)
    .bind(project_id)
    .fetch_optional(&mut **tx)
    .await?;

    Ok(consumed)
}

pub fn ensure_registry_version_exists(
    project_root: &Path,
    version: &str,
) -> anyhow::Result<PathBuf> {
    let version = normalize_version(version)?;
    let path = paths::registry_constitution_path(project_root, version);
    if !path.exists() {
        bail!(
            "registry constitution version not found: {} ({})",
            version,
            path.display()
        );
    }
    Ok(path)
}

pub fn resolve_worker_constitution(
    project_root: &Path,
    legacy_constitution_path: &Path,
    pinned_version: &str,
) -> anyhow::Result<ResolvedConstitution> {
    let pinned = pinned_version.trim();
    ensure!(
        !pinned.is_empty(),
        "pinned constitution_version cannot be empty"
    );

    if pinned != ACTIVE_CONSTITUTION_VERSION {
        let resolved_path = ensure_registry_version_exists(project_root, pinned)?;
        let (constitution, constitution_hash) =
            constitution::load_constitution_and_hash(&resolved_path)
                .with_context(|| format!("load pinned constitution {}", resolved_path.display()))?;

        return Ok(ResolvedConstitution {
            constitution,
            constitution_version: pinned.to_string(),
            constitution_hash,
            constitution_path: resolved_path.to_string_lossy().to_string(),
        });
    }

    let registry_dir = paths::registry_dir(project_root);
    let active_path = paths::registry_active_path(project_root);

    if active_path.exists() {
        let pointer = read_active_pointer(project_root)?;
        let pointer_path = resolve_pointer_path(project_root, &pointer.path);
        let (constitution, constitution_hash) =
            constitution::load_constitution_and_hash(&pointer_path)
                .with_context(|| format!("load active constitution {}", pointer_path.display()))?;

        return Ok(ResolvedConstitution {
            constitution,
            constitution_version: pointer.version,
            constitution_hash,
            constitution_path: pointer_path.to_string_lossy().to_string(),
        });
    }

    if !registry_dir.exists() {
        let (constitution, constitution_hash) = constitution::load_constitution_and_hash(
            legacy_constitution_path,
        )
        .with_context(|| {
            format!(
                "load legacy constitution {}",
                legacy_constitution_path.display()
            )
        })?;

        let version = if constitution.constitution_version.trim().is_empty() {
            ACTIVE_CONSTITUTION_VERSION.to_string()
        } else {
            constitution.constitution_version.clone()
        };

        return Ok(ResolvedConstitution {
            constitution,
            constitution_version: version,
            constitution_hash,
            constitution_path: legacy_constitution_path.to_string_lossy().to_string(),
        });
    }

    bail!(
        "active.json missing/unreadable for legacy run resolution: {}",
        active_path.display()
    )
}

#[derive(Debug, Clone)]
pub struct ResolvedConstitution {
    pub constitution: Constitution,
    pub constitution_version: String,
    pub constitution_hash: String,
    pub constitution_path: String,
}

pub async fn status(db: &Db, project_root: &Path) -> anyhow::Result<StatusResult> {
    let canonical_root = canonical_project_root(project_root)?;
    let _lock = RegistryLock::acquire(&canonical_root, RegistryLockMode::Shared)?;

    let active_path = paths::registry_active_path(&canonical_root);
    let active = if active_path.exists() {
        Some(read_active_pointer(&canonical_root)?)
    } else {
        None
    };

    let index = load_registry_index(&canonical_root)?;

    let project_id = find_project_id_for_root(db, &canonical_root).await?;
    let canary_state = fetch_policy_canary_state(db, &project_id).await?;

    let mut version_warnings = Vec::<String>::new();
    for entry in &index.versions {
        if !is_release_version_pattern(&entry.version) {
            version_warnings.push(format!(
                "version '{}' does not match expected pattern ^v\\d+\\.\\d+(\\.\\d+)?$",
                entry.version
            ));
        }
    }

    if let Some(active) = &active {
        if !is_release_version_pattern(&active.version) {
            version_warnings.push(format!(
                "active version '{}' does not match expected pattern ^v\\d+\\.\\d+(\\.\\d+)?$",
                active.version
            ));
        }
    }

    Ok(StatusResult {
        canonical_project_root: canonical_root,
        active,
        index,
        canary_state,
        version_warnings,
    })
}

fn normalize_version(version: &str) -> anyhow::Result<&str> {
    let trimmed = version.trim();
    ensure!(!trimmed.is_empty(), "version must be non-empty");
    Ok(trimmed)
}

fn write_registry_constitution(
    project_root: &Path,
    version: &str,
    source_bytes: &[u8],
    note: Option<String>,
) -> anyhow::Result<RegistryWriteResult> {
    validate_registry_admission(source_bytes)?;

    let target_path = paths::registry_constitution_path(project_root, version);
    write_atomic_bytes(&target_path, source_bytes)?;

    let read_1 = std::fs::read(&target_path)
        .with_context(|| format!("read stored constitution #1: {}", target_path.display()))?;
    let hash_1 = util::sha256_bytes(&read_1);

    let read_2 = std::fs::read(&target_path)
        .with_context(|| format!("read stored constitution #2: {}", target_path.display()))?;
    let hash_2 = util::sha256_bytes(&read_2);

    if read_1 != read_2 {
        bail!(
            "stored constitution read mismatch path={} len1={} len2={} hash1={} hash2={}",
            target_path.display(),
            read_1.len(),
            read_2.len(),
            hash_prefix(&hash_1),
            hash_prefix(&hash_2),
        );
    }

    validate_registry_admission(&read_1)?;

    let mut index = load_registry_index(project_root)?;
    upsert_index_version(
        &mut index,
        version,
        &hash_1,
        &target_path.to_string_lossy(),
        note,
    )?;

    write_atomic_json(&paths::registry_index_path(project_root), &index)?;

    Ok(RegistryWriteResult {
        version: version.to_string(),
        hash: hash_1,
        path: target_path,
    })
}

fn load_registry_index(project_root: &Path) -> anyhow::Result<RegistryIndex> {
    let index_path = paths::registry_index_path(project_root);
    if !index_path.exists() {
        return Ok(RegistryIndex::default());
    }

    read_json(&index_path).with_context(|| format!("read index JSON: {}", index_path.display()))
}

fn upsert_index_version(
    index: &mut RegistryIndex,
    version: &str,
    hash: &str,
    path: &str,
    note: Option<String>,
) -> anyhow::Result<()> {
    if let Some(existing) = index
        .versions
        .iter_mut()
        .find(|entry| entry.version == version)
    {
        if existing.hash != hash {
            bail!(
                "version {} already exists with different hash (existing={}, new={})",
                version,
                existing.hash,
                hash
            );
        }
        existing.path = path.to_string();
        if existing.note.is_none() {
            existing.note = note;
        }
    } else {
        index.versions.push(RegistryVersionEntry {
            version: version.to_string(),
            hash: hash.to_string(),
            path: path.to_string(),
            added_at: Some(util::now_rfc3339()),
            note,
        });
    }

    index
        .versions
        .sort_by(|left, right| left.version.cmp(&right.version));
    Ok(())
}

fn load_registry_version_hash(
    project_root: &Path,
    version: &str,
) -> anyhow::Result<(PathBuf, String)> {
    let path = ensure_registry_version_exists(project_root, version)?;
    let bytes = std::fs::read(&path)
        .with_context(|| format!("read registry constitution: {}", path.display()))?;
    let hash = util::sha256_bytes(&bytes);
    Ok((path, hash))
}

fn validate_registry_admission(bytes: &[u8]) -> anyhow::Result<()> {
    let constitution: Constitution = serde_yaml::from_slice(bytes)
        .with_context(|| "parse constitution YAML for registry admission")?;

    ensure!(
        !constitution.constitution_version.trim().is_empty(),
        "constitution_version must be non-empty"
    );
    ensure!(
        constitution.audio_contract.is_some(),
        "audio_contract must be present"
    );

    Ok(())
}

fn hash_prefix(hash: &str) -> String {
    hash.chars().take(8).collect::<String>()
}

fn read_json<T: DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read JSON file: {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("parse JSON file: {}", path.display()))
}

fn resolve_pointer_path(project_root: &Path, pointer_path: &str) -> PathBuf {
    let pointer = PathBuf::from(pointer_path);
    if pointer.is_absolute() {
        pointer
    } else {
        paths::registry_dir(project_root).join(pointer)
    }
}

fn update_newly_failing_gates(
    baseline_run: &crate::cll::ValidRun,
    candidate_gate_eval: &gates::GateEvaluation,
    counts: &mut BTreeMap<String, u64>,
) {
    let baseline_map = baseline_run
        .gates
        .iter()
        .map(|gate| (gate.gate_name.as_str(), gate.pass_fail))
        .collect::<BTreeMap<_, _>>();

    for gate in candidate_gate_eval
        .gates
        .iter()
        .filter(|gate| !gate.pass_fail)
    {
        let baseline_pass = baseline_map
            .get(gate.gate_name.as_str())
            .copied()
            .unwrap_or(true);
        if baseline_pass {
            *counts.entry(gate.gate_name.clone()).or_default() += 1;
        }
    }
}

#[derive(Default)]
struct GateStatusAccumulator {
    pass: u64,
    warn: u64,
    fail: u64,
}

impl GateStatusAccumulator {
    fn add(&mut self, status: ImpactGateStatus) {
        match status {
            ImpactGateStatus::Pass => self.pass += 1,
            ImpactGateStatus::Warn => self.warn += 1,
            ImpactGateStatus::Fail => self.fail += 1,
        }
    }
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }

    let sum = values.iter().copied().sum::<f64>();
    sum / values.len() as f64
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }

    let mut sorted = values.to_vec();
    sorted.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));

    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    }
}

fn is_release_version_pattern(version: &str) -> bool {
    let Some(rest) = version.strip_prefix('v') else {
        return false;
    };

    let parts = rest.split('.').collect::<Vec<_>>();
    if parts.len() != 2 && parts.len() != 3 {
        return false;
    }

    parts
        .iter()
        .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()))
}

async fn find_project_id_for_root(db: &Db, project_root: &Path) -> anyhow::Result<String> {
    #[derive(Debug, FromRow)]
    struct ProjectIdRow {
        id: String,
    }

    let canonical_root = canonical_project_root(project_root)?;
    let root_str = canonical_root.to_string_lossy().to_string();

    let project = sqlx::query_as::<_, ProjectIdRow>("SELECT id FROM projects WHERE root_path=?")
        .bind(root_str)
        .fetch_optional(db.pool())
        .await?
        .ok_or_else(|| {
            anyhow!(
                "project not found for root_path {}",
                canonical_root.to_string_lossy()
            )
        })?;

    Ok(project.id)
}

#[cfg(test)]
mod tests {
    use super::{
        build_impact, init_registry, round6, write_atomic_bytes, write_atomic_json,
        ACTIVE_CONSTITUTION_VERSION,
    };
    use crate::{
        analyzer::{BandEnergies, Metrics},
        cll::ProjectTrends,
        drift::{DomainScores, DriftVectorItem},
        gates::{GateResult, GateSeverity, GateStatus},
        paths,
        reports::ReportMeta,
    };
    use serde::Serialize;
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn round6_normalizes_negative_zero() {
        assert_eq!(round6(-0.0), 0.0);
        assert_eq!(round6(1.23456789), 1.234568);
    }

    #[test]
    fn atomic_writers_append_newline() {
        let td = tempfile::tempdir().expect("tempdir");
        let bytes_path = td.path().join("a.yaml");
        let json_path = td.path().join("b.json");

        write_atomic_bytes(&bytes_path, b"hello").expect("write bytes");
        assert_eq!(std::fs::read(&bytes_path).expect("read bytes"), b"hello\n");

        #[derive(Serialize)]
        struct Payload {
            value: &'static str,
        }

        write_atomic_json(&json_path, &Payload { value: "x" }).expect("write json");
        let json_bytes = std::fs::read(&json_path).expect("read json");
        assert!(json_bytes.ends_with(b"\n"));
    }

    #[test]
    fn impact_is_byte_stable_and_schema_locked() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        std::fs::create_dir_all(project_root.join("SCE")).expect("mkdir");

        std::fs::write(
            project_root.join("SCE/constitution.yaml"),
            r#"constitution_version: "v1.0"
audio_contract:
  max_true_peak_dbtp: -1.0
"#,
        )
        .expect("seed constitution");

        write_run(
            &project_root,
            "asset-a",
            "run-1",
            "2026-03-06T00:00:01.000Z",
            GateStatus::Pass,
            10,
            base_metrics(),
        );
        write_run(
            &project_root,
            "asset-a",
            "run-2",
            "2026-03-06T00:00:02.000Z",
            GateStatus::Warn,
            20,
            base_metrics(),
        );

        let init = init_registry(&project_root, "v1.0").expect("init");
        assert_ne!(init.active.version, ACTIVE_CONSTITUTION_VERSION);

        let add_path = td.path().join("candidate.yaml");
        std::fs::write(
            &add_path,
            r#"constitution_version: "v1.1"
audio_contract:
  max_true_peak_dbtp: -9.0
"#,
        )
        .expect("write candidate");

        super::add_registry_constitution(&project_root, "v1.1", &add_path, None).expect("add");

        let first = build_impact(&project_root, "v1.1", 50).expect("impact 1");
        let first_bytes = std::fs::read(&first.output_path).expect("read first");

        let second = build_impact(&project_root, "v1.1", 50).expect("impact 2");
        let second_bytes = std::fs::read(&second.output_path).expect("read second");

        assert_eq!(first_bytes, second_bytes);
        let report: serde_json::Value = serde_json::from_slice(&first_bytes).expect("parse");
        assert_eq!(report["schema_version"], "1.0");
        assert!(report.get("per_run").is_some());
    }

    #[test]
    fn trends_schema_unchanged_by_registry_codepaths() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        std::fs::create_dir_all(project_root.join("SCE")).expect("mkdir");

        std::fs::write(
            project_root.join("SCE/constitution.yaml"),
            r#"constitution_version: "v1.0"
audio_contract:
  max_true_peak_dbtp: -1.0
"#,
        )
        .expect("seed constitution");

        write_run(
            &project_root,
            "asset-a",
            "run-1",
            "2026-03-06T00:00:01.000Z",
            GateStatus::Pass,
            10,
            base_metrics(),
        );

        let summary = crate::cll::generate_project_trends(&project_root, 25).expect("trends");
        let value: ProjectTrends = serde_json::from_str(
            &std::fs::read_to_string(summary.output_path).expect("read trends json"),
        )
        .expect("parse trends typed");

        assert_eq!(value.meta.window.selected_run_count, 1);
    }

    fn write_run(
        project_root: &Path,
        asset_id: &str,
        run_id: &str,
        finished_at: &str,
        gate_status: GateStatus,
        drift_score: u32,
        metrics: Metrics,
    ) {
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
            created_at: finished_at.to_string(),
            started_at: finished_at.to_string(),
            finished_at: finished_at.to_string(),
        };

        std::fs::write(
            run_dir.join("metrics.json"),
            serde_json::to_vec_pretty(&json!({ "meta": meta, "metrics": metrics }))
                .expect("metrics json"),
        )
        .expect("write metrics");

        std::fs::write(
            run_dir.join("gates.json"),
            serde_json::to_vec_pretty(&json!({
                "meta": meta,
                "gate_status": gate_status,
                "gates": vec![GateResult {
                    gate_name: "G003_TruePeakCeiling".to_string(),
                    severity: GateSeverity::Blocker,
                    pass_fail: gate_status != GateStatus::Fail,
                    evidence: json!({"configured": true}),
                    rationale: "test".to_string(),
                }],
            }))
            .expect("gates json"),
        )
        .expect("write gates");

        std::fs::write(
            run_dir.join("drift.json"),
            serde_json::to_vec_pretty(&json!({
                "meta": meta,
                "drift_raw": (drift_score as f64) / 100.0,
                "drift_score": drift_score,
                "domain_scores": DomainScores {
                    loudness: Some(0.1),
                    dynamics: Some(0.2),
                    spectral_balance: Some(0.3),
                    stereo: Some(0.4),
                },
                "domain_weights_effective": {
                    "loudness": 1.0,
                    "dynamics": 1.0,
                    "spectral_balance": 1.0,
                    "stereo": 1.0,
                },
                "drift_vector": vec![DriftVectorItem {
                    metric_id: "loudness.integrated_lufs".to_string(),
                    deviation: 0.1,
                    weighted_deviation: 0.1,
                    direction: "too_low".to_string(),
                    evidence: json!({"delta": 0.4}),
                }],
                "fix_list": Vec::<serde_json::Value>::new(),
                "notes": Vec::<String>::new(),
            }))
            .expect("drift json"),
        )
        .expect("write drift");
    }

    fn base_metrics() -> Metrics {
        Metrics {
            analyzer_version: "TelemetryAnalyzer/1.0.0".to_string(),
            sample_rate_hz: 48_000,
            channels: 2,
            frame_count: 48_000,
            duration_seconds: 1.0,
            sample_peak_linear: 0.5,
            sample_peak_dbfs: -6.0,
            clipping_sample_count: 0,
            rms_dbfs: -10.0,
            crest_factor_db: 4.0,
            short_term_rms_series_dbfs: vec![-10.0],
            approx_true_peak_dbtp: -3.0,
            true_peak_dbtp: -2.0,
            band_energies_db_rel: BandEnergies {
                hz_20_60: -8.0,
                hz_60_150: -6.0,
                hz_150_500: -4.0,
                hz_500_2000: -3.0,
                hz_2000_8000: -2.0,
                hz_8000_16000: -1.0,
            },
            spectral_centroid_hz: 1200.0,
            correlation_min: Some(0.8),
            correlation_mean: Some(0.9),
            lr_balance_db: Some(0.1),
            lossy_source: false,
            integrated_lufs: -14.0,
            short_term_lufs_series: vec![-14.0],
            tonal_balance_curve: vec![0.0; 30],
            transient_density: 0.5,
        }
    }
}
