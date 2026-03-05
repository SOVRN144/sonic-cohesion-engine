use crate::{
    analyzer::{BandEnergies, Metrics},
    constitution::{Constitution, SpectralBandId, TargetAvailability},
    scoring::{corridor_deviation, CorridorDirection},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cmp::Ordering;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DomainScores {
    pub loudness: Option<f64>,
    pub dynamics: Option<f64>,
    pub spectral_balance: Option<f64>,
    pub stereo: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DomainWeightsEffective {
    pub loudness: Option<f64>,
    pub dynamics: Option<f64>,
    pub spectral_balance: Option<f64>,
    pub stereo: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DriftVectorItem {
    pub metric_id: String,
    pub deviation: f64,
    pub weighted_deviation: f64,
    pub direction: String,
    pub evidence: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FixSuggestion {
    pub metric_id: String,
    pub direction: String,
    pub delta: f64,
    pub human_action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DriftResult {
    pub drift_raw: f64,
    pub drift_score: u32,
    pub domain_scores: DomainScores,
    pub domain_weights_effective: DomainWeightsEffective,
    pub drift_vector: Vec<DriftVectorItem>,
    pub fix_list: Vec<FixSuggestion>,
    pub notes: Vec<String>,
}

pub fn compute_drift(
    metrics: &Metrics,
    constitution: &Constitution,
    availability: &TargetAvailability,
) -> DriftResult {
    let mut domain_scores = DomainScores {
        loudness: None,
        dynamics: None,
        spectral_balance: None,
        stereo: None,
    };

    let mut domain_weights_effective = DomainWeightsEffective {
        loudness: None,
        dynamics: None,
        spectral_balance: None,
        stereo: None,
    };

    let mut drift_vector = Vec::<DriftVectorItem>::new();

    let loudness_weight = constitution.priority_weight("loudness_compliance");
    let dynamics_weight = constitution.priority_weight("transient_punch");
    let spectral_weight = (constitution.priority_weight("low_end_translation")
        + constitution.priority_weight("harshness_control"))
        / 2.0;
    let stereo_weight = constitution.priority_weight("width_control");

    if availability.loudness_integrated_lufs.configured
        && availability.loudness_integrated_lufs.applicable
    {
        if let Some(range) = constitution
            .targets
            .as_ref()
            .and_then(|targets| targets.loudness.as_ref())
            .and_then(|loudness| loudness.integrated_lufs_range.as_ref())
        {
            let value = metrics.integrated_lufs;
            let (deviation, direction) = corridor_deviation(value, range.min(), range.max());
            domain_scores.loudness = Some(deviation);
            domain_weights_effective.loudness = Some(loudness_weight);

            if deviation > 0.0 {
                let delta = corridor_delta(value, range.min(), range.max(), direction);
                drift_vector.push(DriftVectorItem {
                    metric_id: "loudness.integrated_lufs".to_string(),
                    deviation,
                    weighted_deviation: deviation * loudness_weight,
                    direction: direction_token(direction).to_string(),
                    evidence: json!({
                        "configured": true,
                        "applicable": true,
                        "value": value,
                        "min": range.min(),
                        "max": range.max(),
                        "delta": delta,
                    }),
                });
            }
        }
    }

    if availability.dynamics_crest_factor.configured
        && availability.dynamics_crest_factor.applicable
    {
        if let Some(range) = constitution
            .targets
            .as_ref()
            .and_then(|targets| targets.dynamics.as_ref())
            .and_then(|dynamics| dynamics.crest_factor_db_range.as_ref())
        {
            let value = metrics.crest_factor_db;
            let (deviation, direction) = corridor_deviation(value, range.min(), range.max());
            domain_scores.dynamics = Some(deviation);
            domain_weights_effective.dynamics = Some(dynamics_weight);

            if deviation > 0.0 {
                let delta = corridor_delta(value, range.min(), range.max(), direction);
                drift_vector.push(DriftVectorItem {
                    metric_id: "dynamics.crest_factor_db".to_string(),
                    deviation,
                    weighted_deviation: deviation * dynamics_weight,
                    direction: direction_token(direction).to_string(),
                    evidence: json!({
                        "configured": true,
                        "applicable": true,
                        "value": value,
                        "min": range.min(),
                        "max": range.max(),
                        "delta": delta,
                    }),
                });
            }
        }
    }

    let spectral_targets = constitution.spectral_band_targets();
    let configured_spectral_bands: Vec<SpectralBandId> = SpectralBandId::ALL
        .iter()
        .copied()
        .filter(|band| availability.spectral_bands.get(*band).configured)
        .collect();

    if !configured_spectral_bands.is_empty() {
        let mut deviations = Vec::new();
        for band in configured_spectral_bands {
            if let Some(range) = spectral_targets.get(band) {
                let value = spectral_band_value(&metrics.band_energies_db_rel, band);
                let (deviation, direction) = corridor_deviation(value, range.min(), range.max());
                deviations.push(deviation);

                if deviation > 0.0 {
                    let delta = corridor_delta(value, range.min(), range.max(), direction);
                    drift_vector.push(DriftVectorItem {
                        metric_id: band.metric_id().to_string(),
                        deviation,
                        weighted_deviation: deviation * spectral_weight,
                        direction: match direction {
                            CorridorDirection::TooLow => "band_cold".to_string(),
                            CorridorDirection::TooHigh => "band_hot".to_string(),
                            CorridorDirection::Inside => "inside".to_string(),
                        },
                        evidence: json!({
                            "configured": true,
                            "applicable": true,
                            "band": band.key(),
                            "value": value,
                            "min": range.min(),
                            "max": range.max(),
                            "delta": delta,
                        }),
                    });
                }
            }
        }

        if !deviations.is_empty() {
            domain_scores.spectral_balance = Some(mean(&deviations));
            domain_weights_effective.spectral_balance = Some(spectral_weight);
        }
    }

    let stereo_configured =
        availability.stereo_correlation.configured || availability.stereo_lr_balance.configured;
    if stereo_configured {
        let mut deviations = Vec::new();

        if availability.stereo_correlation.configured && availability.stereo_correlation.applicable
        {
            if let Some(min_corr) = constitution
                .targets
                .as_ref()
                .and_then(|targets| targets.stereo.as_ref())
                .and_then(|stereo| stereo.correlation_min)
            {
                if let Some(value) = metrics.correlation_min {
                    let (deviation, direction) = corridor_deviation(value, min_corr, 1.0);
                    deviations.push(deviation);

                    if deviation > 0.0 {
                        let delta = corridor_delta(value, min_corr, 1.0, direction);
                        drift_vector.push(DriftVectorItem {
                            metric_id: "stereo.correlation_min".to_string(),
                            deviation,
                            weighted_deviation: deviation * stereo_weight,
                            direction: direction_token(direction).to_string(),
                            evidence: json!({
                                "configured": true,
                                "applicable": true,
                                "value": value,
                                "min": min_corr,
                                "max": 1.0,
                                "delta": delta,
                            }),
                        });
                    }
                }
            }
        }

        if availability.stereo_lr_balance.configured && availability.stereo_lr_balance.applicable {
            if let Some(max_abs) = constitution
                .targets
                .as_ref()
                .and_then(|targets| targets.stereo.as_ref())
                .and_then(|stereo| stereo.lr_balance_db_max_abs)
            {
                if let Some(value_db) = metrics.lr_balance_db {
                    let value_abs = value_db.abs();
                    let (deviation, direction) = corridor_deviation(value_abs, 0.0, max_abs);
                    deviations.push(deviation);

                    if deviation > 0.0 {
                        let delta = corridor_delta(value_abs, 0.0, max_abs, direction);
                        let lean_direction = if value_db >= 0.0 {
                            "lean_left"
                        } else {
                            "lean_right"
                        };
                        drift_vector.push(DriftVectorItem {
                            metric_id: "stereo.lr_balance_db_abs".to_string(),
                            deviation,
                            weighted_deviation: deviation * stereo_weight,
                            direction: lean_direction.to_string(),
                            evidence: json!({
                                "configured": true,
                                "applicable": true,
                                "value": value_db,
                                "value_abs": value_abs,
                                "min": 0.0,
                                "max": max_abs,
                                "delta": delta,
                            }),
                        });
                    }
                }
            }
        }

        if !deviations.is_empty() {
            domain_scores.stereo = Some(mean(&deviations));
            domain_weights_effective.stereo = Some(stereo_weight);
        }
    }

    drift_vector.sort_by(|a, b| {
        b.weighted_deviation
            .partial_cmp(&a.weighted_deviation)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.metric_id.cmp(&b.metric_id))
    });

    let mut weighted_sum = 0.0;
    let mut weight_sum = 0.0;

    for (score, weight) in [
        (domain_scores.loudness, domain_weights_effective.loudness),
        (domain_scores.dynamics, domain_weights_effective.dynamics),
        (
            domain_scores.spectral_balance,
            domain_weights_effective.spectral_balance,
        ),
        (domain_scores.stereo, domain_weights_effective.stereo),
    ] {
        if let (Some(score), Some(weight)) = (score, weight) {
            weighted_sum += score * weight;
            weight_sum += weight;
        }
    }

    let drift_raw = if weight_sum > 0.0 {
        (weighted_sum / weight_sum).clamp(0.0, 1.0)
    } else {
        0.0
    };

    let drift_score = (drift_raw * 100.0).round().clamp(0.0, 100.0) as u32;

    let fix_list = drift_vector
        .iter()
        .take(7)
        .map(|item| {
            let delta = item.evidence["delta"].as_f64().unwrap_or(item.deviation);
            FixSuggestion {
                metric_id: item.metric_id.clone(),
                direction: item.direction.clone(),
                delta,
                human_action: fix_action(&item.metric_id, &item.direction, delta),
            }
        })
        .collect();

    let mut notes = Vec::new();
    if weight_sum == 0.0 {
        notes.push("No constitution targets configured; drift not evaluated.".to_string());
    } else if drift_vector.is_empty() {
        notes.push("Inside constitution corridors; no action required.".to_string());
    }

    DriftResult {
        drift_raw,
        drift_score,
        domain_scores,
        domain_weights_effective,
        drift_vector,
        fix_list,
        notes,
    }
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn corridor_delta(value: f64, min: f64, max: f64, direction: CorridorDirection) -> f64 {
    let low = min.min(max);
    let high = min.max(max);

    match direction {
        CorridorDirection::Inside => 0.0,
        CorridorDirection::TooLow => low - value,
        CorridorDirection::TooHigh => value - high,
    }
}

fn direction_token(direction: CorridorDirection) -> &'static str {
    match direction {
        CorridorDirection::Inside => "inside",
        CorridorDirection::TooLow => "too_low",
        CorridorDirection::TooHigh => "too_high",
    }
}

fn spectral_band_value(bands: &BandEnergies, band: SpectralBandId) -> f64 {
    match band {
        SpectralBandId::Hz20_60 => bands.hz_20_60,
        SpectralBandId::Hz60_150 => bands.hz_60_150,
        SpectralBandId::Hz150_500 => bands.hz_150_500,
        SpectralBandId::Hz500_2000 => bands.hz_500_2000,
        SpectralBandId::Hz2000_8000 => bands.hz_2000_8000,
        SpectralBandId::Hz8000_16000 => bands.hz_8000_16000,
    }
}

fn fix_action(metric_id: &str, direction: &str, delta: f64) -> String {
    match (metric_id, direction) {
        ("loudness.integrated_lufs", "too_low") => {
            format!(
                "Increase integrated loudness by approximately {:.2} LUFS.",
                delta
            )
        }
        ("loudness.integrated_lufs", "too_high") => {
            format!(
                "Reduce integrated loudness by approximately {:.2} LUFS.",
                delta
            )
        }
        ("dynamics.crest_factor_db", "too_low") => {
            format!(
                "Increase crest factor by about {:.2} dB (restore transient contrast).",
                delta
            )
        }
        ("dynamics.crest_factor_db", "too_high") => {
            format!(
                "Reduce crest factor by about {:.2} dB (control transient overshoot).",
                delta
            )
        }
        (metric, "band_hot") if metric.starts_with("spectral.") => {
            format!("Reduce {} by approximately {:.2} dB.", metric, delta)
        }
        (metric, "band_cold") if metric.starts_with("spectral.") => {
            format!("Increase {} by approximately {:.2} dB.", metric, delta)
        }
        ("stereo.correlation_min", "too_low") => {
            format!(
                "Increase mono compatibility; raise correlation by roughly {:.3}.",
                delta
            )
        }
        ("stereo.lr_balance_db_abs", "lean_left") => {
            format!(
                "Reduce left-heavy balance by approximately {:.2} dB.",
                delta
            )
        }
        ("stereo.lr_balance_db_abs", "lean_right") => {
            format!(
                "Reduce right-heavy balance by approximately {:.2} dB.",
                delta
            )
        }
        _ => format!(
            "Adjust {} toward corridor by approximately {:.3}.",
            metric_id, delta
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::compute_drift;
    use crate::{
        analyzer::{BandEnergies, Metrics},
        constitution::{build_target_availability, Constitution},
    };

    #[test]
    fn inside_corridor_gives_zero_deviation() {
        let constitution: Constitution = serde_yaml::from_str(
            r#"
constitution_version: "1.0"
targets:
  loudness:
    integrated_lufs_range: [-16, -12]
"#,
        )
        .expect("constitution");

        let mut metrics = base_metrics();
        metrics.integrated_lufs = -14.0;

        let availability = build_target_availability(&constitution, &metrics, None);
        let drift = compute_drift(&metrics, &constitution, &availability);

        assert_eq!(drift.domain_scores.loudness, Some(0.0));
    }

    #[test]
    fn outside_corridor_generates_deviation_and_fix() {
        let constitution: Constitution = serde_yaml::from_str(
            r#"
constitution_version: "1.0"
targets:
  loudness:
    integrated_lufs_range: [-16, -12]
"#,
        )
        .expect("constitution");

        let mut metrics = base_metrics();
        metrics.integrated_lufs = -20.0;

        let availability = build_target_availability(&constitution, &metrics, None);
        let drift = compute_drift(&metrics, &constitution, &availability);

        assert!(drift.domain_scores.loudness.expect("score") > 0.0);
        assert_eq!(drift.drift_vector[0].metric_id, "loudness.integrated_lufs");
        assert_eq!(drift.drift_vector[0].direction, "too_low");
        assert!(!drift.fix_list.is_empty());
    }

    #[test]
    fn configured_but_not_applicable_stereo_domain_is_excluded() {
        let constitution: Constitution = serde_yaml::from_str(
            r#"
constitution_version: "1.0"
targets:
  stereo:
    correlation_min: 0.0
    lr_balance_db_max_abs: 1.5
"#,
        )
        .expect("constitution");

        let mut metrics = base_metrics();
        metrics.channels = 1;
        metrics.correlation_min = None;
        metrics.lr_balance_db = None;

        let availability = build_target_availability(&constitution, &metrics, None);
        let drift = compute_drift(&metrics, &constitution, &availability);

        assert_eq!(drift.domain_scores.stereo, None);
        assert_eq!(drift.domain_weights_effective.stereo, None);
        assert_eq!(drift.drift_score, 0);
    }

    #[test]
    fn priorities_reorder_top_drift_vector_items() {
        let constitution: Constitution = serde_yaml::from_str(
            r#"
constitution_version: "1.0"
targets:
  loudness:
    integrated_lufs_range: [-16, -12]
  dynamics:
    crest_factor_db_range: [6, 10]
priorities:
  weights:
    loudness_compliance: 0.2
    transient_punch: 3.0
"#,
        )
        .expect("constitution");

        let mut metrics = base_metrics();
        metrics.integrated_lufs = -20.0;
        metrics.crest_factor_db = 2.0;

        let availability = build_target_availability(&constitution, &metrics, None);
        let drift = compute_drift(&metrics, &constitution, &availability);

        assert_eq!(drift.drift_vector[0].metric_id, "dynamics.crest_factor_db");
    }

    #[test]
    fn no_contributing_domains_returns_zero_drift_and_note() {
        let constitution: Constitution = serde_yaml::from_str(
            r#"
constitution_version: "1.0"
"#,
        )
        .expect("constitution");

        let metrics = base_metrics();
        let availability = build_target_availability(&constitution, &metrics, None);
        let drift = compute_drift(&metrics, &constitution, &availability);

        assert_eq!(drift.drift_raw, 0.0);
        assert_eq!(drift.drift_score, 0);
        assert!(drift.fix_list.is_empty());
        assert_eq!(
            drift.notes,
            vec!["No constitution targets configured; drift not evaluated.".to_string()]
        );
    }

    #[test]
    fn loudness_boundaries_are_inclusive_for_drift() {
        let constitution: Constitution = serde_yaml::from_str(
            r#"
constitution_version: "1.0"
targets:
  loudness:
    integrated_lufs_range: [-16, -12]
"#,
        )
        .expect("constitution");

        let mut at_min = base_metrics();
        at_min.integrated_lufs = -16.0;
        let availability_at_min = build_target_availability(&constitution, &at_min, None);
        let drift_at_min = compute_drift(&at_min, &constitution, &availability_at_min);
        assert_eq!(drift_at_min.domain_scores.loudness, Some(0.0));

        let mut at_max = base_metrics();
        at_max.integrated_lufs = -12.0;
        let availability_at_max = build_target_availability(&constitution, &at_max, None);
        let drift_at_max = compute_drift(&at_max, &constitution, &availability_at_max);
        assert_eq!(drift_at_max.domain_scores.loudness, Some(0.0));

        let mut below_min = base_metrics();
        below_min.integrated_lufs = -16.1;
        let availability_below_min = build_target_availability(&constitution, &below_min, None);
        let drift_below_min = compute_drift(&below_min, &constitution, &availability_below_min);
        assert!(
            drift_below_min
                .domain_scores
                .loudness
                .expect("below min score")
                > 0.0
        );

        let mut above_max = base_metrics();
        above_max.integrated_lufs = -11.9;
        let availability_above_max = build_target_availability(&constitution, &above_max, None);
        let drift_above_max = compute_drift(&above_max, &constitution, &availability_above_max);
        assert!(
            drift_above_max
                .domain_scores
                .loudness
                .expect("above max score")
                > 0.0
        );
    }

    fn base_metrics() -> Metrics {
        Metrics {
            analyzer_version: "TelemetryAnalyzer/1.0.0".to_string(),
            sample_rate_hz: 44_100,
            channels: 2,
            frame_count: 44_100,
            duration_seconds: 1.0,
            sample_peak_linear: 0.5,
            sample_peak_dbfs: -6.0,
            clipping_sample_count: 0,
            rms_dbfs: -12.0,
            crest_factor_db: 6.0,
            short_term_rms_series_dbfs: vec![-12.0],
            approx_true_peak_dbtp: -1.2,
            true_peak_dbtp: -1.1,
            band_energies_db_rel: BandEnergies {
                hz_20_60: 0.0,
                hz_60_150: 0.0,
                hz_150_500: 0.0,
                hz_500_2000: 0.0,
                hz_2000_8000: 0.0,
                hz_8000_16000: 0.0,
            },
            spectral_centroid_hz: 1000.0,
            correlation_min: Some(0.8),
            correlation_mean: Some(0.85),
            lr_balance_db: Some(0.0),
            lossy_source: false,
            integrated_lufs: -14.0,
            short_term_lufs_series: vec![-14.0],
            tonal_balance_curve: vec![0.0; 30],
            transient_density: 0.2,
        }
    }
}
