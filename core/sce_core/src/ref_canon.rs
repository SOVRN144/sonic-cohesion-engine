use crate::{
    analyzer::{self, Metrics},
    cll, paths, policy_registry,
};
use anyhow::{anyhow, ensure, Context};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

const SCHEMA_VERSION: &str = "1.0";
const REF_ANALYZER_VERSION: &str = "TelemetryAnalyzer/1.0.0";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RefEntry {
    pub content_hash: String,
    pub name: String,
    pub tags: Vec<String>,
    pub profile_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copied_file_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RefListOutput {
    pub schema_version: String,
    pub refs: Vec<RefEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RefShowOutput {
    pub schema_version: String,
    pub content_hash: String,
    pub name: String,
    pub tags: Vec<String>,
    pub profile_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copied_file_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefProfile {
    pub schema_version: String,
    pub content_hash: String,
    pub analyzer_version: String,
    pub metrics: Metrics,
}

impl RefShowOutput {
    fn from_entry(entry: &RefEntry) -> Self {
        Self {
            schema_version: SCHEMA_VERSION.to_string(),
            content_hash: entry.content_hash.clone(),
            name: entry.name.clone(),
            tags: entry.tags.clone(),
            profile_path: entry.profile_path.clone(),
            copied_file_path: entry.copied_file_path.clone(),
        }
    }
}

pub fn add_ref(
    project_root: &Path,
    file_path: &Path,
    name: Option<&str>,
    tags: &str,
    copy_file: bool,
) -> anyhow::Result<RefShowOutput> {
    let sce_hint_dir = project_root.join("SCE");
    fs::create_dir_all(&sce_hint_dir)
        .with_context(|| format!("create ref project directory: {}", sce_hint_dir.display()))?;
    let canonical_root = policy_registry::canonical_project_root(project_root)?;
    let canonical_file = fs::canonicalize(file_path)
        .with_context(|| format!("canonicalize ref file: {}", file_path.display()))?;
    let content_hash = crate::util::sha256_file(&canonical_file)
        .with_context(|| format!("hash ref file: {}", canonical_file.display()))?;
    let profile = reuse_or_analyze(&canonical_root, &canonical_file, &content_hash)?;
    let profile_path = paths::refs_profile_path(&canonical_root, &content_hash);
    policy_registry::write_atomic_json(&profile_path, &profile)?;

    let mut index = load_index(&canonical_root)?;
    let name = normalize_name(name, &canonical_file)?;
    let tags = normalize_tags(tags);
    let copied_file_path = match (copy_file, existing_entry(&index.refs, &content_hash)) {
        (true, _) => Some(copy_ref_file(
            &canonical_root,
            &content_hash,
            &canonical_file,
        )?),
        (false, Some(existing)) => existing.copied_file_path.clone(),
        (false, None) => None,
    };

    let entry = RefEntry {
        content_hash: content_hash.clone(),
        name,
        tags,
        profile_path: profile_path.to_string_lossy().to_string(),
        copied_file_path,
    };

    upsert_entry(&mut index.refs, entry.clone());
    policy_registry::write_atomic_json(&paths::refs_canon_index_path(&canonical_root), &index)?;

    Ok(RefShowOutput::from_entry(&entry))
}

pub fn list_refs(project_root: &Path) -> anyhow::Result<RefListOutput> {
    let canonical_root = policy_registry::canonical_project_root(project_root)?;
    load_index(&canonical_root)
}

pub fn show_ref(project_root: &Path, content_hash: &str) -> anyhow::Result<RefShowOutput> {
    let canonical_root = policy_registry::canonical_project_root(project_root)?;
    let index = load_index(&canonical_root)?;
    let entry = existing_entry(&index.refs, content_hash)
        .ok_or_else(|| anyhow!("reference not found: {content_hash}"))?;
    Ok(RefShowOutput::from_entry(entry))
}

pub fn remove_ref(project_root: &Path, content_hash: &str) -> anyhow::Result<RefShowOutput> {
    let canonical_root = policy_registry::canonical_project_root(project_root)?;
    let mut index = load_index(&canonical_root)?;
    let position = index
        .refs
        .iter()
        .position(|entry| entry.content_hash == content_hash)
        .ok_or_else(|| anyhow!("reference not found: {content_hash}"))?;

    let removed = index.refs.remove(position);
    policy_registry::write_atomic_json(&paths::refs_canon_index_path(&canonical_root), &index)?;

    remove_if_exists(Path::new(&removed.profile_path))?;
    if let Some(copied_file_path) = &removed.copied_file_path {
        remove_if_exists(Path::new(copied_file_path))?;
    }

    Ok(RefShowOutput::from_entry(&removed))
}

pub fn write_operator_output(project_root: &Path) -> anyhow::Result<(RefListOutput, PathBuf)> {
    let canonical_root = policy_registry::canonical_project_root(project_root)?;
    let output = load_index(&canonical_root)?;
    let output_path = paths::ref_canon_operator_output_path(&canonical_root);
    policy_registry::write_atomic_json(&output_path, &output)?;
    Ok((output, output_path))
}

fn reuse_or_analyze(
    project_root: &Path,
    file_path: &Path,
    content_hash: &str,
) -> anyhow::Result<RefProfile> {
    let loaded = if project_root.exists() {
        Some(cll::load_valid_runs(project_root)?)
    } else {
        None
    };
    if let Some(run) = loaded.as_ref().and_then(|loaded| {
        loaded.valid_runs.iter().rfind(|run| {
            run.asset_content_hash == content_hash
                && !run.analyzer_version.trim().is_empty()
                && !run.metrics.analyzer_version.trim().is_empty()
                && run.metrics.analyzer_version == run.analyzer_version
        })
    }) {
        return Ok(RefProfile {
            schema_version: SCHEMA_VERSION.to_string(),
            content_hash: content_hash.to_string(),
            analyzer_version: run.analyzer_version.clone(),
            metrics: run.metrics.clone(),
        });
    }

    let metrics = futures_executor(file_path)?;
    Ok(RefProfile {
        schema_version: SCHEMA_VERSION.to_string(),
        content_hash: content_hash.to_string(),
        analyzer_version: REF_ANALYZER_VERSION.to_string(),
        metrics,
    })
}

fn futures_executor(file_path: &Path) -> anyhow::Result<Metrics> {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| {
            handle
                .block_on(analyzer::analyze_file(file_path, REF_ANALYZER_VERSION))
                .map_err(|err| anyhow!(err.to_string()))
        }),
        Err(_) => {
            let runtime = tokio::runtime::Runtime::new().context("create ref analyzer runtime")?;
            runtime
                .block_on(analyzer::analyze_file(file_path, REF_ANALYZER_VERSION))
                .map_err(|err| anyhow!(err.to_string()))
        }
    }
}

fn load_index(project_root: &Path) -> anyhow::Result<RefListOutput> {
    let index_path = paths::refs_canon_index_path(project_root);
    if !index_path.exists() {
        return Ok(RefListOutput {
            schema_version: SCHEMA_VERSION.to_string(),
            refs: Vec::new(),
        });
    }

    let mut index: RefListOutput = serde_json::from_str(
        &fs::read_to_string(&index_path)
            .with_context(|| format!("read ref index: {}", index_path.display()))?,
    )
    .with_context(|| format!("parse ref index: {}", index_path.display()))?;
    index.schema_version = SCHEMA_VERSION.to_string();
    sort_entries(&mut index.refs);
    Ok(index)
}

fn upsert_entry(entries: &mut Vec<RefEntry>, entry: RefEntry) {
    if let Some(existing) = entries
        .iter_mut()
        .find(|current| current.content_hash == entry.content_hash)
    {
        *existing = entry;
    } else {
        entries.push(entry);
    }
    sort_entries(entries);
}

fn existing_entry<'a>(entries: &'a [RefEntry], content_hash: &str) -> Option<&'a RefEntry> {
    entries
        .iter()
        .find(|entry| entry.content_hash == content_hash)
}

fn sort_entries(entries: &mut [RefEntry]) {
    entries.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.content_hash.cmp(&right.content_hash))
    });
}

fn normalize_name(name: Option<&str>, file_path: &Path) -> anyhow::Result<String> {
    let normalized = name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            file_path
                .file_stem()
                .and_then(|value| value.to_str())
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            file_path
                .file_name()
                .and_then(|value| value.to_str())
                .map(ToOwned::to_owned)
        })
        .ok_or_else(|| anyhow!("unable to derive ref name from {}", file_path.display()))?;

    ensure!(!normalized.trim().is_empty(), "ref name must be non-empty");
    Ok(normalized)
}

fn normalize_tags(tags: &str) -> Vec<String> {
    // Normalize tags by splitting on commas, trimming each token, dropping empties,
    // de-duplicating, then sorting ascending for deterministic storage/output.
    let mut normalized = tags
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn copy_ref_file(
    project_root: &Path,
    content_hash: &str,
    source_path: &Path,
) -> anyhow::Result<String> {
    let file_name = match source_path.extension().and_then(|value| value.to_str()) {
        Some(extension) if !extension.is_empty() => format!("{content_hash}.{extension}"),
        _ => content_hash.to_string(),
    };
    let destination = paths::refs_files_dir(project_root).join(file_name);
    copy_atomic(source_path, &destination)?;
    Ok(destination.to_string_lossy().to_string())
}

fn copy_atomic(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow!("missing destination parent: {}", destination.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create ref copy directory: {}", parent.display()))?;

    let file_name = destination
        .file_name()
        .ok_or_else(|| anyhow!("missing destination file name: {}", destination.display()))?;
    let mut tmp_name = OsString::from(file_name);
    tmp_name.push(format!(".tmp.{}", std::process::id()));
    let tmp_path = parent.join(tmp_name);

    fs::copy(source, &tmp_path).with_context(|| {
        format!(
            "copy ref file {} -> {}",
            source.display(),
            tmp_path.display()
        )
    })?;
    fs::rename(&tmp_path, destination).with_context(|| {
        format!(
            "rename ref temp file {} -> {}",
            tmp_path.display(),
            destination.display()
        )
    })?;
    Ok(())
}

fn remove_if_exists(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove file: {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::{add_ref, list_refs, remove_ref, show_ref, RefProfile};
    use crate::{gates::GateStatus, paths, reports::ReportMeta};
    use serde_json::json;
    use std::{io::Write, path::Path};

    #[test]
    fn tag_normalization_is_deterministic() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        let ref_path = td.path().join("ref.wav");
        write_wav_i16(&ref_path).expect("write wav");

        let added = add_ref(
            &project_root,
            &ref_path,
            Some("Alpha"),
            "  warm ,bright, warm,, punchy ",
            false,
        )
        .expect("add ref");

        assert_eq!(added.tags, vec!["bright", "punchy", "warm"]);
    }

    #[test]
    fn list_and_show_follow_locked_schema_and_order() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        let alpha = td.path().join("alpha.wav");
        let beta = td.path().join("beta.wav");
        write_wav_i16(&alpha).expect("write alpha");
        write_wav_i16(&beta).expect("write beta");

        let beta_entry = add_ref(&project_root, &beta, Some("Beta"), "", false).expect("beta");
        let alpha_entry =
            add_ref(&project_root, &alpha, Some("Alpha"), "warm", false).expect("alpha");

        let listed = list_refs(&project_root).expect("list refs");
        assert_eq!(listed.schema_version, "1.0");
        assert_eq!(listed.refs.len(), 2);
        assert_eq!(listed.refs[0].name, "Alpha");
        assert_eq!(listed.refs[1].name, "Beta");

        let shown = show_ref(&project_root, &alpha_entry.content_hash).expect("show alpha");
        assert_eq!(shown.schema_version, "1.0");
        assert_eq!(shown.content_hash, alpha_entry.content_hash);
        assert_eq!(shown.tags, vec!["warm"]);
        assert!(shown.copied_file_path.is_none());

        let shown_beta = show_ref(&project_root, &beta_entry.content_hash).expect("show beta");
        assert_eq!(shown_beta.name, "Beta");
    }

    #[test]
    fn add_is_idempotent_for_same_content_hash() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        let ref_path = td.path().join("ref.wav");
        write_wav_i16(&ref_path).expect("write wav");

        let first =
            add_ref(&project_root, &ref_path, Some("Alpha"), "warm", false).expect("first add");
        let second =
            add_ref(&project_root, &ref_path, Some("Alpha"), "warm", false).expect("second add");

        assert_eq!(first, second);
        assert_eq!(list_refs(&project_root).expect("list").refs.len(), 1);
    }

    #[test]
    fn add_prefers_latest_matching_run_by_finished_at_then_run_id() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        let ref_path = td.path().join("ref.wav");
        write_wav_i16(&ref_path).expect("write wav");

        let content_hash = crate::util::sha256_file(&ref_path).expect("hash");
        write_matching_run(
            &project_root,
            "asset-a",
            "run-a",
            "2026-03-06T00:00:01.000Z",
            &content_hash,
        );
        write_matching_run(
            &project_root,
            "asset-a",
            "run-b",
            "2026-03-06T00:00:01.000Z",
            &content_hash,
        );

        let added = add_ref(&project_root, &ref_path, Some("Alpha"), "", false).expect("add");
        let profile: RefProfile = serde_json::from_str(
            &std::fs::read_to_string(&added.profile_path).expect("read profile"),
        )
        .expect("parse profile");

        assert_eq!(profile.analyzer_version, "TelemetryAnalyzer/1.0.0");
        assert_eq!(profile.metrics.integrated_lufs, -13.0);
    }

    #[test]
    fn remove_deletes_index_entry() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        let ref_path = td.path().join("ref.wav");
        write_wav_i16(&ref_path).expect("write wav");

        let added = add_ref(&project_root, &ref_path, Some("Alpha"), "", true).expect("add");
        let removed = remove_ref(&project_root, &added.content_hash).expect("remove");

        assert_eq!(removed.content_hash, added.content_hash);
        assert!(list_refs(&project_root).expect("list").refs.is_empty());
        assert!(!Path::new(&removed.profile_path).exists());
        assert!(!Path::new(
            removed
                .copied_file_path
                .as_deref()
                .expect("copied file path must be present")
        )
        .exists());
    }

    fn write_matching_run(
        project_root: &Path,
        asset_id: &str,
        run_id: &str,
        finished_at: &str,
        content_hash: &str,
    ) {
        let run_dir = paths::run_dir(project_root, asset_id, run_id);
        std::fs::create_dir_all(&run_dir).expect("mkdir run");

        let meta = ReportMeta {
            project_id: "project-1".to_string(),
            asset_id: asset_id.to_string(),
            run_id: run_id.to_string(),
            asset_content_hash: content_hash.to_string(),
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

        let metrics = json!({
            "meta": meta,
            "metrics": {
                "analyzer_version": "TelemetryAnalyzer/1.0.0",
                "sample_rate_hz": 48000,
                "channels": 2,
                "frame_count": 48000,
                "duration_seconds": 1.0,
                "sample_peak_linear": 0.5,
                "sample_peak_dbfs": -6.0,
                "clipping_sample_count": 0,
                "rms_dbfs": -12.0,
                "crest_factor_db": 6.0,
                "short_term_rms_series_dbfs": [-12.0],
                "approx_true_peak_dbtp": -2.0,
                "true_peak_dbtp": -1.9,
                "band_energies_db_rel": {
                    "hz_20_60": -4.0,
                    "hz_60_150": -3.0,
                    "hz_150_500": -2.0,
                    "hz_500_2000": -1.0,
                    "hz_2000_8000": -2.0,
                    "hz_8000_16000": -5.0
                },
                "spectral_centroid_hz": 1100.0,
                "correlation_min": 0.7,
                "correlation_mean": 0.8,
                "lr_balance_db": 0.1,
                "lossy_source": false,
                "integrated_lufs": if run_id == "run-a" { -14.0 } else { -13.0 },
                "short_term_lufs_series": [-14.0],
                "tonal_balance_curve": vec![0.0; 30],
                "transient_density": 0.2
            }
        });

        let gates = json!({
            "meta": meta,
            "gate_status": GateStatus::Pass,
            "gates": []
        });

        let drift = json!({
            "meta": meta,
            "drift_score": 10,
            "domain_scores": {
                "loudness": 0.1,
                "dynamics": 0.1,
                "spectral_balance": 0.1,
                "stereo": 0.1
            },
            "drift_vector": []
        });

        std::fs::write(
            run_dir.join("metrics.json"),
            serde_json::to_vec_pretty(&metrics).expect("metrics"),
        )
        .expect("write metrics");
        std::fs::write(
            run_dir.join("gates.json"),
            serde_json::to_vec_pretty(&gates).expect("gates"),
        )
        .expect("write gates");
        std::fs::write(
            run_dir.join("drift.json"),
            serde_json::to_vec_pretty(&drift).expect("drift"),
        )
        .expect("write drift");
    }

    fn write_wav_i16(path: &Path) -> anyhow::Result<()> {
        let sample_rate_hz = 48_000u32;
        let channels = 1u16;
        let frames = 512usize;
        let seed = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("ref")
            .bytes()
            .map(u32::from)
            .sum::<u32>() as f64;
        let mut pcm = Vec::<i16>::with_capacity(frames * channels as usize);
        for frame_idx in 0..frames {
            let value = ((frame_idx as f64 / (12.0 + seed.rem_euclid(5.0))).sin() * 0.25 * 32767.0)
                .round() as i16;
            pcm.push(value);
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
}
