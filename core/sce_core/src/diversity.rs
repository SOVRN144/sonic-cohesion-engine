use crate::analyzer::{Metrics, EPS};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::BTreeMap;

const FEATURE_IDS: [&str; 14] = [
    "loudness.integrated_lufs",
    "dynamics.crest_factor_db",
    "spectral.hz_20_60",
    "spectral.hz_60_150",
    "spectral.hz_150_500",
    "spectral.hz_500_2000",
    "spectral.hz_2000_8000",
    "spectral.hz_8000_16000",
    "spectral.centroid_hz",
    "stereo.correlation_mean",
    "stereo.lr_balance_db",
    "tonal.mean_30",
    "tonal.std_30",
    "transient.density",
];

const KEY_FEATURE_IDS: [&str; 6] = [
    "loudness.integrated_lufs",
    "dynamics.crest_factor_db",
    "spectral.hz_20_60",
    "spectral.hz_2000_8000",
    "stereo.correlation_mean",
    "stereo.lr_balance_db",
];

const STEREO_KEY_FEATURE_IDS: [&str; 2] = ["stereo.correlation_mean", "stereo.lr_balance_db"];
const MIN_DIVERSITY_HISTORY: usize = 5;

const IMPUTED_FEATURE_IDS: [&str; 4] = [
    "stereo.correlation_mean",
    "stereo.lr_balance_db",
    "tonal.mean_30",
    "tonal.std_30",
];

#[derive(Debug, Clone)]
pub struct DiversityRunInput {
    pub run_id: String,
    pub finished_at: String,
    pub drift_score: u32,
    pub metrics: Metrics,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DiversityAlertLevel {
    Info,
    Warn,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiversityAlert {
    pub level: DiversityAlertLevel,
    #[serde(rename = "type")]
    pub alert_type: String,
    pub evidence: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimeSpan {
    pub min: String,
    pub max: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeatureMissingCount {
    pub baseline_missing: usize,
    pub recent_missing: usize,
    pub total_missing: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StereoKeyFeatureEvaluation {
    pub feature_id: String,
    pub baseline_present_count: usize,
    pub recent_present_count: usize,
    pub counted: bool,
    pub exclusion_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeyFeatureEvaluation {
    pub m: usize,
    pub majority_threshold: usize,
    pub features: Vec<StereoKeyFeatureEvaluation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VarianceRatioEntry {
    pub feature_id: String,
    pub baseline_var: f64,
    pub recent_var: f64,
    pub ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DistanceCollapse {
    pub baseline_median: f64,
    pub recent_median: f64,
    pub ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiversitySentinelOutput {
    pub baseline_run_count: usize,
    pub recent_run_count: usize,
    pub baseline_span_finished_at: Option<TimeSpan>,
    pub recent_span_finished_at: Option<TimeSpan>,
    pub feature_imputation_value: BTreeMap<String, f64>,
    pub feature_missing_counts: BTreeMap<String, FeatureMissingCount>,
    pub feature_applicability_notes: Vec<String>,
    pub key_feature_evaluation: KeyFeatureEvaluation,
    pub variance_ratios: Vec<VarianceRatioEntry>,
    pub distance_collapse: DistanceCollapse,
    pub zero_drift_share: f64,
    pub diversity_alerts: Vec<DiversityAlert>,
}

impl DiversitySentinelOutput {
    pub fn warn_alert_count(&self) -> usize {
        self.diversity_alerts
            .iter()
            .filter(|alert| alert.level == DiversityAlertLevel::Warn)
            .count()
    }

    pub fn info_alert_count(&self) -> usize {
        self.diversity_alerts
            .iter()
            .filter(|alert| alert.level == DiversityAlertLevel::Info)
            .count()
    }
}

#[derive(Debug, Clone, Copy)]
struct FeatureValue {
    value: f64,
    present: bool,
}

pub fn compute_diversity_sentinel(selected_runs: &[DiversityRunInput]) -> DiversitySentinelOutput {
    let selected_count = selected_runs.len();
    let m = selected_count.min(10);
    let majority_threshold = if m == 0 { 0 } else { m.div_ceil(2) };

    let baseline_runs = &selected_runs[..m];
    let recent_runs = if m == 0 {
        &selected_runs[0..0]
    } else {
        &selected_runs[selected_count - m..]
    };

    let baseline_span_finished_at = span_from_runs(baseline_runs);
    let recent_span_finished_at = span_from_runs(recent_runs);

    let feature_records: Vec<BTreeMap<String, FeatureValue>> = selected_runs
        .iter()
        .map(|run| extract_feature_values(&run.metrics))
        .collect();

    let baseline_records = &feature_records[..m];
    let recent_records = if m == 0 {
        &feature_records[0..0]
    } else {
        &feature_records[selected_count - m..]
    };

    let feature_imputation_value: BTreeMap<String, f64> = IMPUTED_FEATURE_IDS
        .iter()
        .map(|id| ((*id).to_string(), 0.0))
        .collect();

    let feature_missing_counts: BTreeMap<String, FeatureMissingCount> = FEATURE_IDS
        .iter()
        .map(|feature_id| {
            let baseline_missing = baseline_records
                .iter()
                .filter(|record| !record[&feature_id.to_string()].present)
                .count();
            let recent_missing = recent_records
                .iter()
                .filter(|record| !record[&feature_id.to_string()].present)
                .count();
            let total_missing = feature_records
                .iter()
                .filter(|record| !record[&feature_id.to_string()].present)
                .count();

            (
                (*feature_id).to_string(),
                FeatureMissingCount {
                    baseline_missing,
                    recent_missing,
                    total_missing,
                },
            )
        })
        .collect();

    let mut feature_applicability_notes = vec![
        "stereo.correlation_mean may be absent for mono runs; missing values are imputed to 0.0"
            .to_string(),
        "stereo.lr_balance_db may be absent for mono runs; missing values are imputed to 0.0"
            .to_string(),
        "tonal.mean_30 and tonal.std_30 require a valid 30-band tonal_balance_curve; missing values are imputed to 0.0"
            .to_string(),
    ];

    let baseline_stats: BTreeMap<String, (f64, f64)> = FEATURE_IDS
        .iter()
        .map(|feature_id| {
            let values: Vec<f64> = baseline_records
                .iter()
                .map(|record| record[&feature_id.to_string()].value)
                .collect();
            let mean = arithmetic_mean(&values);
            let std = population_std_dev(&values);
            ((*feature_id).to_string(), (mean, std))
        })
        .collect();

    let baseline_vectors = build_normalized_vectors(baseline_records, &baseline_stats);
    let recent_vectors = build_normalized_vectors(recent_records, &baseline_stats);

    let baseline_median_distance = median_pairwise_distance(&baseline_vectors);
    let recent_median_distance = median_pairwise_distance(&recent_vectors);
    let distance_collapse_ratio = recent_median_distance / (baseline_median_distance + EPS);

    let variance_ratios: Vec<VarianceRatioEntry> = FEATURE_IDS
        .iter()
        .map(|feature_id| {
            let baseline_values: Vec<f64> = baseline_records
                .iter()
                .map(|record| record[&feature_id.to_string()].value)
                .collect();
            let recent_values: Vec<f64> = recent_records
                .iter()
                .map(|record| record[&feature_id.to_string()].value)
                .collect();

            let baseline_var = population_variance(&baseline_values);
            let recent_var = population_variance(&recent_values);
            let ratio = recent_var / (baseline_var + EPS);

            VarianceRatioEntry {
                feature_id: (*feature_id).to_string(),
                baseline_var,
                recent_var,
                ratio,
            }
        })
        .collect();

    let ratio_lookup: BTreeMap<String, f64> = variance_ratios
        .iter()
        .map(|entry| (entry.feature_id.clone(), entry.ratio))
        .collect();

    let zero_drift_share = if selected_count == 0 {
        0.0
    } else {
        selected_runs
            .iter()
            .filter(|run| run.drift_score <= 1)
            .count() as f64
            / selected_count as f64
    };

    let median_variance_ratio = median(
        &variance_ratios
            .iter()
            .map(|entry| entry.ratio)
            .collect::<Vec<_>>(),
    );

    let stereo_feature_counts: Vec<StereoKeyFeatureEvaluation> = STEREO_KEY_FEATURE_IDS
        .iter()
        .map(|feature_id| {
            let baseline_present_count = baseline_records
                .iter()
                .filter(|record| record[&feature_id.to_string()].present)
                .count();
            let recent_present_count = recent_records
                .iter()
                .filter(|record| record[&feature_id.to_string()].present)
                .count();

            let counted = m > 0
                && baseline_present_count >= majority_threshold
                && recent_present_count >= majority_threshold;

            let exclusion_reason = if counted {
                None
            } else if m == 0 {
                Some("insufficient runs for stereo feature applicability".to_string())
            } else if baseline_present_count < majority_threshold
                && recent_present_count < majority_threshold
            {
                Some("not present in majority of baseline and recent windows".to_string())
            } else if baseline_present_count < majority_threshold {
                Some("not present in majority of baseline window".to_string())
            } else {
                Some("not present in majority of recent window".to_string())
            };

            StereoKeyFeatureEvaluation {
                feature_id: (*feature_id).to_string(),
                baseline_present_count,
                recent_present_count,
                counted,
                exclusion_reason,
            }
        })
        .collect();

    let key_feature_evaluation = KeyFeatureEvaluation {
        m,
        majority_threshold,
        features: stereo_feature_counts.clone(),
    };

    let stereo_counted_lookup: BTreeMap<String, bool> = stereo_feature_counts
        .iter()
        .map(|entry| (entry.feature_id.clone(), entry.counted))
        .collect();

    let key_feature_hits = KEY_FEATURE_IDS
        .iter()
        .filter(|feature_id| {
            if STEREO_KEY_FEATURE_IDS.contains(feature_id)
                && !stereo_counted_lookup
                    .get::<str>(*feature_id)
                    .copied()
                    .unwrap_or(false)
            {
                return false;
            }
            ratio_lookup.get::<str>(*feature_id).copied().unwrap_or(1.0) < 0.5
        })
        .count();

    let enough_for_stats = m >= 2;

    if !enough_for_stats {
        feature_applicability_notes.push(
            "insufficient runs for diversity statistics (requires at least 2 selected runs)"
                .to_string(),
        );
    }

    let mut diversity_alerts = Vec::new();
    let small_n_guard = selected_count < MIN_DIVERSITY_HISTORY || m < MIN_DIVERSITY_HISTORY;
    if small_n_guard {
        diversity_alerts.push(DiversityAlert {
            level: DiversityAlertLevel::Info,
            alert_type: "insufficient_history".to_string(),
            evidence: json!({
                "message": "insufficient history for diversity inference",
                "selected_run_count": selected_count,
                "m_used": m,
                "minimum_required": MIN_DIVERSITY_HISTORY,
            }),
        });
    } else {
        // Threshold rationale (Commit 4 v0):
        // - zero_drift_share >= 0.8 identifies sustained near-zero drift pressure.
        // - distance_collapse_ratio <= 0.4 indicates substantial reduction in sound-space spread.
        // - median_variance_ratio <= 0.3 indicates broad variance compression across features.
        // Combined together, these prioritize monoculture-risk signals while avoiding over-triggering.
        let warn_condition = enough_for_stats
            && zero_drift_share >= 0.8
            && distance_collapse_ratio <= 0.4
            && median_variance_ratio <= 0.3;

        let info_condition =
            enough_for_stats && (distance_collapse_ratio <= 0.6 || key_feature_hits >= 2);

        if warn_condition {
            diversity_alerts.push(DiversityAlert {
                level: DiversityAlertLevel::Warn,
                alert_type: "homogenization_risk".to_string(),
                evidence: json!({
                    "message": "risk of homogenization / metric conditioning detected; explore constitution corridor edges instead of center convergence",
                    "zero_drift_share": zero_drift_share,
                    "distance_collapse_ratio": distance_collapse_ratio,
                    "median_variance_ratio": median_variance_ratio,
                }),
            });
        } else if info_condition {
            diversity_alerts.push(DiversityAlert {
                level: DiversityAlertLevel::Info,
                alert_type: "homogenization_watch".to_string(),
                evidence: json!({
                    "message": "early signal of homogenization / metric conditioning; explore corridor edges before tightening targets",
                    "distance_collapse_ratio": distance_collapse_ratio,
                    "key_feature_hits": key_feature_hits,
                }),
            });
        }
    }

    DiversitySentinelOutput {
        baseline_run_count: m,
        recent_run_count: m,
        baseline_span_finished_at,
        recent_span_finished_at,
        feature_imputation_value,
        feature_missing_counts,
        feature_applicability_notes,
        key_feature_evaluation,
        variance_ratios,
        distance_collapse: DistanceCollapse {
            baseline_median: baseline_median_distance,
            recent_median: recent_median_distance,
            ratio: distance_collapse_ratio,
        },
        zero_drift_share,
        diversity_alerts,
    }
}

fn span_from_runs(runs: &[DiversityRunInput]) -> Option<TimeSpan> {
    if runs.is_empty() {
        None
    } else {
        Some(TimeSpan {
            min: runs.first().expect("has first").finished_at.clone(),
            max: runs.last().expect("has last").finished_at.clone(),
        })
    }
}

fn extract_feature_values(metrics: &Metrics) -> BTreeMap<String, FeatureValue> {
    let mut values = BTreeMap::<String, FeatureValue>::new();

    values.insert(
        "loudness.integrated_lufs".to_string(),
        present_from_value(metrics.integrated_lufs),
    );
    values.insert(
        "dynamics.crest_factor_db".to_string(),
        present_from_value(metrics.crest_factor_db),
    );
    values.insert(
        "spectral.hz_20_60".to_string(),
        present_from_value(metrics.band_energies_db_rel.hz_20_60),
    );
    values.insert(
        "spectral.hz_60_150".to_string(),
        present_from_value(metrics.band_energies_db_rel.hz_60_150),
    );
    values.insert(
        "spectral.hz_150_500".to_string(),
        present_from_value(metrics.band_energies_db_rel.hz_150_500),
    );
    values.insert(
        "spectral.hz_500_2000".to_string(),
        present_from_value(metrics.band_energies_db_rel.hz_500_2000),
    );
    values.insert(
        "spectral.hz_2000_8000".to_string(),
        present_from_value(metrics.band_energies_db_rel.hz_2000_8000),
    );
    values.insert(
        "spectral.hz_8000_16000".to_string(),
        present_from_value(metrics.band_energies_db_rel.hz_8000_16000),
    );
    values.insert(
        "spectral.centroid_hz".to_string(),
        present_from_value(metrics.spectral_centroid_hz),
    );
    values.insert(
        "stereo.correlation_mean".to_string(),
        present_from_optional(metrics.correlation_mean),
    );
    values.insert(
        "stereo.lr_balance_db".to_string(),
        present_from_optional(metrics.lr_balance_db),
    );

    let tonal_stats = tonal_mean_std(&metrics.tonal_balance_curve);
    values.insert(
        "tonal.mean_30".to_string(),
        present_from_optional(tonal_stats.map(|stats| stats.0)),
    );
    values.insert(
        "tonal.std_30".to_string(),
        present_from_optional(tonal_stats.map(|stats| stats.1)),
    );

    values.insert(
        "transient.density".to_string(),
        present_from_value(metrics.transient_density),
    );

    values
}

fn tonal_mean_std(curve: &[f64]) -> Option<(f64, f64)> {
    if curve.len() != 30 || curve.iter().any(|value| !value.is_finite()) {
        return None;
    }

    let mean = arithmetic_mean(curve);
    let std = population_std_dev(curve);
    Some((mean, std))
}

fn present_from_value(value: f64) -> FeatureValue {
    if value.is_finite() {
        FeatureValue {
            value,
            present: true,
        }
    } else {
        FeatureValue {
            value: 0.0,
            present: false,
        }
    }
}

fn present_from_optional(value: Option<f64>) -> FeatureValue {
    match value {
        Some(inner) if inner.is_finite() => FeatureValue {
            value: inner,
            present: true,
        },
        _ => FeatureValue {
            value: 0.0,
            present: false,
        },
    }
}

fn build_normalized_vectors(
    records: &[BTreeMap<String, FeatureValue>],
    baseline_stats: &BTreeMap<String, (f64, f64)>,
) -> Vec<Vec<f64>> {
    records
        .iter()
        .map(|record| {
            FEATURE_IDS
                .iter()
                .map(|feature_id| {
                    let x = record[&feature_id.to_string()].value;
                    let (mean_baseline, std_baseline) = baseline_stats[&feature_id.to_string()];
                    // z = (x - mean_baseline) / (std_baseline + EPS)
                    (x - mean_baseline) / (std_baseline + EPS)
                })
                .collect()
        })
        .collect()
}

fn median_pairwise_distance(vectors: &[Vec<f64>]) -> f64 {
    if vectors.len() < 2 {
        return 0.0;
    }

    let mut distances = Vec::new();
    for i in 0..vectors.len() {
        for j in (i + 1)..vectors.len() {
            let sum_sq = vectors[i]
                .iter()
                .zip(vectors[j].iter())
                .map(|(a, b)| {
                    let diff = a - b;
                    diff * diff
                })
                .sum::<f64>();
            distances.push(sum_sq.sqrt());
        }
    }

    median(&distances)
}

fn arithmetic_mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn population_variance(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }

    let mean = arithmetic_mean(values);
    values
        .iter()
        .map(|value| {
            let delta = value - mean;
            delta * delta
        })
        .sum::<f64>()
        / values.len() as f64
}

fn population_std_dev(values: &[f64]) -> f64 {
    population_variance(values).sqrt()
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }

    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));

    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::{
        compute_diversity_sentinel, median, population_variance, DiversityAlertLevel,
        DiversityRunInput,
    };
    use crate::analyzer::{BandEnergies, Metrics};

    #[test]
    fn population_variance_returns_zero_for_n_lt_two() {
        assert_eq!(population_variance(&[]), 0.0);
        assert_eq!(population_variance(&[1.0]), 0.0);
    }

    #[test]
    fn median_even_count_is_average_of_middle_values() {
        let value = median(&[1.0, 9.0, 3.0, 5.0]);
        assert_eq!(value, 4.0);
    }

    #[test]
    fn collapsed_vectors_emit_warn_alert() {
        let runs = (0..10)
            .map(|idx| DiversityRunInput {
                run_id: format!("run-{idx:02}"),
                finished_at: format!("2026-03-06T00:00:{idx:02}.000Z"),
                drift_score: 0,
                metrics: base_metrics(),
            })
            .collect::<Vec<_>>();

        let output = compute_diversity_sentinel(&runs);
        assert!(output
            .diversity_alerts
            .iter()
            .any(|alert| alert.level == DiversityAlertLevel::Warn));
    }

    #[test]
    fn small_n_emits_single_insufficient_history_info() {
        let runs = (0..2)
            .map(|idx| DiversityRunInput {
                run_id: format!("run-{idx:02}"),
                finished_at: format!("2026-03-06T00:00:{idx:02}.000Z"),
                drift_score: 10,
                metrics: base_metrics(),
            })
            .collect::<Vec<_>>();

        let output = compute_diversity_sentinel(&runs);
        assert_eq!(output.diversity_alerts.len(), 1);
        let alert = &output.diversity_alerts[0];
        assert_eq!(alert.level, DiversityAlertLevel::Info);
        assert_eq!(alert.alert_type, "insufficient_history");
        assert_eq!(
            alert.evidence["message"].as_str(),
            Some("insufficient history for diversity inference")
        );
        assert_eq!(alert.evidence["selected_run_count"].as_u64(), Some(2));
        assert_eq!(alert.evidence["m_used"].as_u64(), Some(2));
        assert_eq!(alert.evidence["minimum_required"].as_u64(), Some(5));
    }

    #[test]
    fn stereo_key_features_excluded_when_not_present_in_majority() {
        let mut runs = Vec::new();
        for idx in 0..6 {
            let mut metrics = base_metrics();
            metrics.correlation_mean = None;
            metrics.lr_balance_db = None;
            metrics.drift_safe_set(idx);
            runs.push(DiversityRunInput {
                run_id: format!("run-{idx:02}"),
                finished_at: format!("2026-03-06T00:00:{idx:02}.000Z"),
                drift_score: 10,
                metrics,
            });
        }

        let output = compute_diversity_sentinel(&runs);
        for feature in output.key_feature_evaluation.features {
            assert!(!feature.counted);
            assert!(feature.exclusion_reason.is_some());
        }
    }

    #[test]
    fn tonal_fields_present_when_curve_valid() {
        let run = DiversityRunInput {
            run_id: "run-01".to_string(),
            finished_at: "2026-03-06T00:00:01.000Z".to_string(),
            drift_score: 10,
            metrics: base_metrics(),
        };

        let output = compute_diversity_sentinel(&[run.clone(), run]);
        let mean_missing = output
            .feature_missing_counts
            .get("tonal.mean_30")
            .expect("tonal mean count");
        let std_missing = output
            .feature_missing_counts
            .get("tonal.std_30")
            .expect("tonal std count");

        assert_eq!(mean_missing.total_missing, 0);
        assert_eq!(std_missing.total_missing, 0);
    }

    trait DriftSafeSet {
        fn drift_safe_set(&mut self, idx: usize);
    }

    impl DriftSafeSet for Metrics {
        fn drift_safe_set(&mut self, idx: usize) {
            self.integrated_lufs = -14.0 + idx as f64 * 0.25;
            self.crest_factor_db = 7.0 + idx as f64 * 0.2;
            self.spectral_centroid_hz = 1200.0 + idx as f64 * 30.0;
            self.band_energies_db_rel.hz_20_60 = -4.0 + idx as f64 * 0.3;
            self.band_energies_db_rel.hz_2000_8000 = -2.5 + idx as f64 * 0.2;
        }
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
            rms_dbfs: -12.0,
            crest_factor_db: 8.0,
            short_term_rms_series_dbfs: vec![-12.0],
            approx_true_peak_dbtp: -2.0,
            true_peak_dbtp: -1.8,
            band_energies_db_rel: BandEnergies {
                hz_20_60: -4.0,
                hz_60_150: -3.0,
                hz_150_500: -2.0,
                hz_500_2000: -1.0,
                hz_2000_8000: -2.5,
                hz_8000_16000: -5.0,
            },
            spectral_centroid_hz: 1400.0,
            correlation_min: Some(0.6),
            correlation_mean: Some(0.75),
            lr_balance_db: Some(0.1),
            lossy_source: false,
            integrated_lufs: -14.0,
            short_term_lufs_series: vec![-14.0],
            tonal_balance_curve: vec![0.0; 30],
            transient_density: 0.3,
        }
    }
}
