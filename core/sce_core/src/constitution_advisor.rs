use crate::{
    cll::{load_valid_runs, select_window},
    diversity::{compute_diversity_sentinel, DiversityAlertLevel, DiversityRunInput},
    paths,
};
use anyhow::{anyhow, Context};
use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

const BASE_QUANTILE_MIN: f64 = 0.10;
const BASE_QUANTILE_MAX: f64 = 0.90;
const BASE_WIDEN_FACTOR: f64 = 0.15;
const DIVERSITY_WARN_WIDEN_FACTOR: f64 = 0.20;

const LUFS_MIN_WIDTH: f64 = 2.0;
const CREST_MIN_WIDTH: f64 = 1.0;
const SPECTRAL_MIN_WIDTH: f64 = 1.5;
const LR_BALANCE_MIN_WIDTH: f64 = 0.5;
const LR_BALANCE_MAX_ABS_FLOOR: f64 = 0.5;
const CORRELATION_MIN_WIDTH: f64 = 0.05;

const MIN_STEREO_RUNS: usize = 2;
const MIN_STEREO_SAMPLES_PER_METRIC: usize = 2;

#[derive(Debug, Clone)]
pub struct SuggestConstitutionSummary {
    pub output_path: PathBuf,
    pub requested_last_n: usize,
    pub selected_run_count: usize,
    pub diversity_warn: bool,
}

#[derive(Debug, Clone, Copy)]
struct Corridor {
    min: f64,
    max: f64,
}

impl Corridor {
    fn new(a: f64, b: f64) -> Self {
        Self {
            min: a.min(b),
            max: a.max(b),
        }
    }

    fn width(self) -> f64 {
        self.max - self.min
    }

    fn midpoint(self) -> f64 {
        (self.min + self.max) / 2.0
    }

    fn widen(self, factor: f64) -> Self {
        let half_width = self.width() / 2.0;
        let expanded_half = half_width * (1.0 + factor);
        let center = self.midpoint();
        Self {
            min: center - expanded_half,
            max: center + expanded_half,
        }
    }

    fn enforce_min_width(self, min_width: f64) -> Self {
        if self.width() >= min_width {
            return self;
        }
        let center = self.midpoint();
        let half = min_width / 2.0;
        Self {
            min: center - half,
            max: center + half,
        }
    }

    fn clamp(self, lower: f64, upper: f64) -> Self {
        let mut min = self.min.clamp(lower, upper);
        let mut max = self.max.clamp(lower, upper);
        if min > max {
            std::mem::swap(&mut min, &mut max);
        }
        Self { min, max }
    }
}

pub fn suggest_constitution(
    project_root: &Path,
    requested_last_n: usize,
    out: Option<&Path>,
) -> anyhow::Result<SuggestConstitutionSummary> {
    let loaded = load_valid_runs(project_root)?;
    let selected_runs = select_window(&loaded.valid_runs, requested_last_n);
    if selected_runs.is_empty() {
        return Err(anyhow!(
            "no valid runs available to suggest constitution corridors"
        ));
    }

    let sce_dir = paths::project_sce_dir(&loaded.canonical_root);
    let constitution_path = sce_dir.join("constitution.yaml");
    let output_path = match out {
        Some(path) => path.to_path_buf(),
        None => sce_dir.join("constitution.suggested.yaml"),
    };

    if paths_equivalent(&output_path, &constitution_path)? {
        return Err(anyhow!(
            "refusing to overwrite active constitution: {}",
            constitution_path.display()
        ));
    }

    let diversity_input = selected_runs
        .iter()
        .map(|run| DiversityRunInput {
            run_id: run.run_id.clone(),
            finished_at: run.finished_at.clone(),
            drift_score: run.drift_score,
            metrics: run.metrics.clone(),
        })
        .collect::<Vec<_>>();
    let diversity = compute_diversity_sentinel(&diversity_input);
    let diversity_warn = diversity
        .diversity_alerts
        .iter()
        .any(|alert| alert.level == DiversityAlertLevel::Warn);

    let integrated_lufs = collect_finite(
        selected_runs
            .iter()
            .map(|run| run.metrics.integrated_lufs)
            .collect::<Vec<_>>(),
    );
    let crest_factor = collect_finite(
        selected_runs
            .iter()
            .map(|run| run.metrics.crest_factor_db)
            .collect::<Vec<_>>(),
    );

    let hz_20_60 = collect_finite(
        selected_runs
            .iter()
            .map(|run| run.metrics.band_energies_db_rel.hz_20_60)
            .collect::<Vec<_>>(),
    );
    let hz_60_150 = collect_finite(
        selected_runs
            .iter()
            .map(|run| run.metrics.band_energies_db_rel.hz_60_150)
            .collect::<Vec<_>>(),
    );
    let hz_150_500 = collect_finite(
        selected_runs
            .iter()
            .map(|run| run.metrics.band_energies_db_rel.hz_150_500)
            .collect::<Vec<_>>(),
    );
    let hz_500_2000 = collect_finite(
        selected_runs
            .iter()
            .map(|run| run.metrics.band_energies_db_rel.hz_500_2000)
            .collect::<Vec<_>>(),
    );
    let hz_2000_8000 = collect_finite(
        selected_runs
            .iter()
            .map(|run| run.metrics.band_energies_db_rel.hz_2000_8000)
            .collect::<Vec<_>>(),
    );
    let hz_8000_16000 = collect_finite(
        selected_runs
            .iter()
            .map(|run| run.metrics.band_energies_db_rel.hz_8000_16000)
            .collect::<Vec<_>>(),
    );

    let loudness_corridor = build_corridor(&integrated_lufs, LUFS_MIN_WIDTH, None, diversity_warn)
        .ok_or_else(|| anyhow!("unable to compute loudness corridor from selected runs"))?;
    let crest_corridor = build_corridor(&crest_factor, CREST_MIN_WIDTH, None, diversity_warn)
        .ok_or_else(|| anyhow!("unable to compute dynamics corridor from selected runs"))?;

    let spectral_hz_20_60 = build_corridor(&hz_20_60, SPECTRAL_MIN_WIDTH, None, diversity_warn)
        .ok_or_else(|| anyhow!("unable to compute spectral corridor for hz_20_60"))?;
    let spectral_hz_60_150 =
        build_corridor(&hz_60_150, SPECTRAL_MIN_WIDTH, None, diversity_warn)
            .ok_or_else(|| anyhow!("unable to compute spectral corridor for hz_60_150"))?;
    let spectral_hz_150_500 = build_corridor(&hz_150_500, SPECTRAL_MIN_WIDTH, None, diversity_warn)
        .ok_or_else(|| anyhow!("unable to compute spectral corridor for hz_150_500"))?;
    let spectral_hz_500_2000 =
        build_corridor(&hz_500_2000, SPECTRAL_MIN_WIDTH, None, diversity_warn)
            .ok_or_else(|| anyhow!("unable to compute spectral corridor for hz_500_2000"))?;
    let spectral_hz_2000_8000 =
        build_corridor(&hz_2000_8000, SPECTRAL_MIN_WIDTH, None, diversity_warn)
            .ok_or_else(|| anyhow!("unable to compute spectral corridor for hz_2000_8000"))?;
    let spectral_hz_8000_16000 =
        build_corridor(&hz_8000_16000, SPECTRAL_MIN_WIDTH, None, diversity_warn)
            .ok_or_else(|| anyhow!("unable to compute spectral corridor for hz_8000_16000"))?;

    let stereo_runs = selected_runs
        .iter()
        .filter(|run| run.metrics.channels >= 2)
        .collect::<Vec<_>>();

    let correlation_values = collect_finite(
        stereo_runs
            .iter()
            .filter_map(|run| run.metrics.correlation_min)
            .collect::<Vec<_>>(),
    );
    let lr_balance_abs_values = collect_finite(
        stereo_runs
            .iter()
            .filter_map(|run| run.metrics.lr_balance_db)
            .map(f64::abs)
            .collect::<Vec<_>>(),
    );

    let enough_stereo_runs = stereo_runs.len() >= MIN_STEREO_RUNS;
    let correlation_min =
        if enough_stereo_runs && correlation_values.len() >= MIN_STEREO_SAMPLES_PER_METRIC {
            build_corridor(
                &correlation_values,
                CORRELATION_MIN_WIDTH,
                Some((-1.0, 1.0)),
                diversity_warn,
            )
            .map(|corridor| corridor.min)
        } else {
            None
        };

    let lr_balance_db_max_abs =
        if enough_stereo_runs && lr_balance_abs_values.len() >= MIN_STEREO_SAMPLES_PER_METRIC {
            build_corridor(
                &lr_balance_abs_values,
                LR_BALANCE_MIN_WIDTH,
                Some((0.0, 6.0)),
                diversity_warn,
            )
            .map(|corridor| corridor.max.max(LR_BALANCE_MAX_ABS_FLOOR).clamp(0.0, 6.0))
        } else {
            None
        };

    let max_true_peak_dbtp = (-1.0_f64).min(0.0);

    let mut notes = vec![
        "Advisory only: review and edit before replacing constitution.yaml.".to_string(),
        "Use corridor ranges to preserve diversity; avoid tightening too early.".to_string(),
        "generated_by: sce_cli suggest-constitution advisory v1".to_string(),
    ];
    if diversity_warn {
        notes.push(
            "Diversity Sentinel detected collapse risk in selected history; corridors were widened by an additional 20% to encourage exploration.".to_string(),
        );
    }

    let document = SuggestionDocument {
        loudness_corridor,
        crest_corridor,
        spectral_hz_20_60,
        spectral_hz_60_150,
        spectral_hz_150_500,
        spectral_hz_500_2000,
        spectral_hz_2000_8000,
        spectral_hz_8000_16000,
        correlation_min,
        lr_balance_db_max_abs,
        max_true_peak_dbtp,
        notes,
    };

    let yaml = render_suggestion_yaml(&document);
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create suggestion output directory: {}", parent.display()))?;
    }
    fs::write(&output_path, yaml.as_bytes())
        .with_context(|| format!("write constitution suggestion: {}", output_path.display()))?;

    Ok(SuggestConstitutionSummary {
        output_path,
        requested_last_n,
        selected_run_count: selected_runs.len(),
        diversity_warn,
    })
}

#[derive(Debug, Clone)]
struct SuggestionDocument {
    loudness_corridor: Corridor,
    crest_corridor: Corridor,
    spectral_hz_20_60: Corridor,
    spectral_hz_60_150: Corridor,
    spectral_hz_150_500: Corridor,
    spectral_hz_500_2000: Corridor,
    spectral_hz_2000_8000: Corridor,
    spectral_hz_8000_16000: Corridor,
    correlation_min: Option<f64>,
    lr_balance_db_max_abs: Option<f64>,
    max_true_peak_dbtp: f64,
    notes: Vec<String>,
}

fn build_corridor(
    values: &[f64],
    min_width_floor: f64,
    clamp: Option<(f64, f64)>,
    diversity_warn: bool,
) -> Option<Corridor> {
    if values.is_empty() {
        return None;
    }

    let p10 = quantile_type7(values, BASE_QUANTILE_MIN)?;
    let p90 = quantile_type7(values, BASE_QUANTILE_MAX)?;
    let mut corridor = Corridor::new(p10, p90).widen(BASE_WIDEN_FACTOR);
    corridor = corridor.enforce_min_width(min_width_floor);
    if let Some((lower, upper)) = clamp {
        corridor = corridor.clamp(lower, upper);
    }

    if diversity_warn {
        corridor = corridor.widen(DIVERSITY_WARN_WIDEN_FACTOR);
        if let Some((lower, upper)) = clamp {
            corridor = corridor.clamp(lower, upper);
        }
    }

    Some(corridor)
}

fn quantile_type7(values: &[f64], p: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if sorted.len() == 1 {
        return Some(sorted[0]);
    }

    let clamped_p = p.clamp(0.0, 1.0);
    let n = sorted.len() as f64;
    let h = (n - 1.0) * clamped_p + 1.0;
    let k = h.floor();
    let gamma = h - k;
    let k_index = (k as usize).saturating_sub(1);
    if k_index >= sorted.len() - 1 {
        return Some(*sorted.last().expect("non-empty"));
    }
    let lower = sorted[k_index];
    let upper = sorted[k_index + 1];
    Some(lower + gamma * (upper - lower))
}

fn collect_finite(values: Vec<f64>) -> Vec<f64> {
    values
        .into_iter()
        .filter(|value| value.is_finite())
        .collect()
}

fn paths_equivalent(left: &Path, right: &Path) -> anyhow::Result<bool> {
    Ok(normalize_for_compare(left)? == normalize_for_compare(right)?)
}

fn normalize_for_compare(path: &Path) -> anyhow::Result<PathBuf> {
    if path.exists() {
        return fs::canonicalize(path)
            .with_context(|| format!("canonicalize path: {}", path.display()));
    }

    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        let cwd = std::env::current_dir().with_context(|| "resolve current directory")?;
        Ok(cwd.join(path))
    }
}

fn render_suggestion_yaml(document: &SuggestionDocument) -> String {
    let mut out = String::new();
    writeln!(&mut out, "constitution_version: \"1.0\"").expect("write");
    writeln!(&mut out, "audio_contract:").expect("write");
    writeln!(
        &mut out,
        "  max_true_peak_dbtp: {}",
        format_db_like(document.max_true_peak_dbtp)
    )
    .expect("write");
    writeln!(&mut out, "targets:").expect("write");
    writeln!(&mut out, "  loudness:").expect("write");
    writeln!(
        &mut out,
        "    integrated_lufs_range: [{}, {}]",
        format_db_like(document.loudness_corridor.min),
        format_db_like(document.loudness_corridor.max)
    )
    .expect("write");
    writeln!(&mut out, "  dynamics:").expect("write");
    writeln!(
        &mut out,
        "    crest_factor_db_range: [{}, {}]",
        format_db_like(document.crest_corridor.min),
        format_db_like(document.crest_corridor.max)
    )
    .expect("write");
    writeln!(&mut out, "  spectral_balance:").expect("write");
    writeln!(&mut out, "    bands_db:").expect("write");
    writeln!(
        &mut out,
        "      hz_20_60: [{}, {}]",
        format_db_like(document.spectral_hz_20_60.min),
        format_db_like(document.spectral_hz_20_60.max)
    )
    .expect("write");
    writeln!(
        &mut out,
        "      hz_60_150: [{}, {}]",
        format_db_like(document.spectral_hz_60_150.min),
        format_db_like(document.spectral_hz_60_150.max)
    )
    .expect("write");
    writeln!(
        &mut out,
        "      hz_150_500: [{}, {}]",
        format_db_like(document.spectral_hz_150_500.min),
        format_db_like(document.spectral_hz_150_500.max)
    )
    .expect("write");
    writeln!(
        &mut out,
        "      hz_500_2000: [{}, {}]",
        format_db_like(document.spectral_hz_500_2000.min),
        format_db_like(document.spectral_hz_500_2000.max)
    )
    .expect("write");
    writeln!(
        &mut out,
        "      hz_2000_8000: [{}, {}]",
        format_db_like(document.spectral_hz_2000_8000.min),
        format_db_like(document.spectral_hz_2000_8000.max)
    )
    .expect("write");
    writeln!(
        &mut out,
        "      hz_8000_16000: [{}, {}]",
        format_db_like(document.spectral_hz_8000_16000.min),
        format_db_like(document.spectral_hz_8000_16000.max)
    )
    .expect("write");

    if document.correlation_min.is_some() || document.lr_balance_db_max_abs.is_some() {
        writeln!(&mut out, "  stereo:").expect("write");
        if let Some(correlation_min) = document.correlation_min {
            writeln!(
                &mut out,
                "    correlation_min: {}",
                format_correlation(correlation_min)
            )
            .expect("write");
        }
        if let Some(lr_balance_db_max_abs) = document.lr_balance_db_max_abs {
            writeln!(
                &mut out,
                "    lr_balance_db_max_abs: {}",
                format_db_like(lr_balance_db_max_abs)
            )
            .expect("write");
        }
    }

    writeln!(&mut out, "notes:").expect("write");
    for note in &document.notes {
        writeln!(&mut out, "  - {}", yaml_quote(note)).expect("write");
    }
    out
}

// Formatting contract for deterministic YAML bytes:
// - LUFS + dB values use format!("{:.2}", value)
// - correlation values use format!("{:.3}", value)
fn format_db_like(value: f64) -> String {
    format!("{:.2}", value)
}

fn format_correlation(value: f64) -> String {
    format!("{:.3}", value)
}

fn yaml_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::quantile_type7;

    #[test]
    fn quantile_type7_n1_returns_single_value() {
        let values = vec![7.0];
        assert_eq!(quantile_type7(&values, 0.10), Some(7.0));
        assert_eq!(quantile_type7(&values, 0.90), Some(7.0));
    }

    #[test]
    fn quantile_type7_n2_interpolates_linearly() {
        let values = vec![0.0, 10.0];
        assert!((quantile_type7(&values, 0.10).expect("q") - 1.0).abs() < 1e-12);
        assert!((quantile_type7(&values, 0.90).expect("q") - 9.0).abs() < 1e-12);
    }

    #[test]
    fn quantile_type7_n3_matches_expected_points() {
        let values = vec![0.0, 10.0, 20.0];
        assert!((quantile_type7(&values, 0.25).expect("q") - 5.0).abs() < 1e-12);
        assert!((quantile_type7(&values, 0.75).expect("q") - 15.0).abs() < 1e-12);
    }

    #[test]
    fn quantile_type7_n4_matches_expected_points() {
        let values = vec![0.0, 10.0, 20.0, 30.0];
        assert!((quantile_type7(&values, 0.10).expect("q") - 3.0).abs() < 1e-12);
        assert!((quantile_type7(&values, 0.90).expect("q") - 27.0).abs() < 1e-12);
    }
}
