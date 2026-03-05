use crate::{
    analyzer::{BandEnergies, Metrics},
    constitution::{Constitution, SpectralBandId, TargetAvailability, TargetState},
    scoring::{corridor_deviation, CorridorDirection},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const GATE_FORMAT_SAMPLE_RATE: &str = "G001_FormatSampleRate";
pub const GATE_FORMAT_BIT_DEPTH: &str = "G002_BitDepth";
pub const GATE_TRUE_PEAK_CEILING: &str = "G003_TruePeakCeiling";
pub const GATE_HARD_CLIPPING: &str = "G004_HardClipping";
pub const GATE_STEREO_CORRELATION: &str = "W001_StereoCorrelation";
pub const GATE_LR_BALANCE: &str = "W002_LRBalance";
pub const GATE_SPECTRAL_BAND_OUTSIDE: &str = "W003_SpectralBandOutside";
pub const GATE_DYNAMICS_OUTSIDE: &str = "W004_DynamicsOutside";
pub const GATE_LOUDNESS_OUTSIDE: &str = "W005_LoudnessOutside";
pub const GATE_TONAL_CURVE_RECORDED: &str = "I001_TonalCurveRecorded";
pub const GATE_TRANSIENT_PROXY_RECORDED: &str = "I002_TransientProxyRecorded";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GateSeverity {
    Blocker,
    Warn,
    Info,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GateStatus {
    Fail,
    Warn,
    Pass,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateResult {
    pub gate_name: String,
    pub severity: GateSeverity,
    pub pass_fail: bool,
    pub evidence: Value,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateEvaluation {
    pub gate_status: GateStatus,
    pub gates: Vec<GateResult>,
}

pub fn evaluate_gates(
    metrics: &Metrics,
    constitution: &Constitution,
    availability: &TargetAvailability,
    asset_bit_depth: Option<i64>,
) -> GateEvaluation {
    let mut gates = Vec::new();

    gates.push(evaluate_sample_rate(
        metrics,
        constitution,
        availability.sample_rate,
    ));
    gates.push(evaluate_bit_depth(
        constitution,
        availability.bit_depth,
        asset_bit_depth,
    ));
    gates.push(evaluate_true_peak(
        metrics,
        constitution,
        availability.true_peak_ceiling,
    ));
    gates.push(evaluate_clipping(
        metrics,
        constitution,
        availability.clipping,
    ));
    gates.push(evaluate_stereo_correlation(
        metrics,
        constitution,
        availability.stereo_correlation,
    ));
    gates.push(evaluate_lr_balance(
        metrics,
        constitution,
        availability.stereo_lr_balance,
    ));
    gates.push(evaluate_spectral(
        metrics,
        constitution,
        &availability.spectral_bands,
    ));
    gates.push(evaluate_dynamics(
        metrics,
        constitution,
        availability.dynamics_crest_factor,
    ));
    gates.push(evaluate_loudness(
        metrics,
        constitution,
        availability.loudness_integrated_lufs,
    ));
    gates.push(info_gate(
        GATE_TONAL_CURVE_RECORDED,
        json!({
            "configured": true,
            "applicable": availability.tonal_curve.applicable,
            "point_count": metrics.tonal_balance_curve.len(),
        }),
        if availability.tonal_curve.applicable {
            "tonal curve recorded"
        } else {
            "tonal curve unavailable"
        },
    ));
    gates.push(info_gate(
        GATE_TRANSIENT_PROXY_RECORDED,
        json!({
            "configured": true,
            "applicable": availability.transient_proxy.applicable,
            "value": metrics.transient_density,
        }),
        if availability.transient_proxy.applicable {
            "transient proxy recorded"
        } else {
            "transient proxy unavailable"
        },
    ));

    let gate_status = compute_gate_status(&gates);

    GateEvaluation { gate_status, gates }
}

fn evaluate_sample_rate(
    metrics: &Metrics,
    constitution: &Constitution,
    state: TargetState,
) -> GateResult {
    if !state.configured {
        return info_not_configured(GATE_FORMAT_SAMPLE_RATE);
    }

    let allowed = constitution
        .audio_contract
        .as_ref()
        .and_then(|audio| audio.allowed_sample_rates_hz.as_ref())
        .cloned()
        .unwrap_or_default();

    let pass = allowed.contains(&metrics.sample_rate_hz);

    GateResult {
        gate_name: GATE_FORMAT_SAMPLE_RATE.to_string(),
        severity: GateSeverity::Blocker,
        pass_fail: pass,
        evidence: json!({
            "configured": true,
            "value_hz": metrics.sample_rate_hz,
            "allowed_hz": allowed,
        }),
        rationale: if pass {
            "sample rate is allowed".to_string()
        } else {
            "sample rate is outside allowed list".to_string()
        },
    }
}

fn evaluate_bit_depth(
    constitution: &Constitution,
    state: TargetState,
    asset_bit_depth: Option<i64>,
) -> GateResult {
    if !state.configured {
        return info_not_configured(GATE_FORMAT_BIT_DEPTH);
    }

    if !state.applicable {
        return info_not_applicable(GATE_FORMAT_BIT_DEPTH);
    }

    let allowed = constitution
        .audio_contract
        .as_ref()
        .and_then(|audio| audio.allowed_bit_depths.as_ref())
        .cloned()
        .unwrap_or_default();

    let value = asset_bit_depth.unwrap_or_default();
    let pass = allowed.contains(&value);

    GateResult {
        gate_name: GATE_FORMAT_BIT_DEPTH.to_string(),
        severity: GateSeverity::Blocker,
        pass_fail: pass,
        evidence: json!({
            "configured": true,
            "applicable": true,
            "value_bits": value,
            "allowed_bits": allowed,
        }),
        rationale: if pass {
            "bit depth is allowed".to_string()
        } else {
            "bit depth is outside allowed list".to_string()
        },
    }
}

fn evaluate_true_peak(
    metrics: &Metrics,
    constitution: &Constitution,
    state: TargetState,
) -> GateResult {
    if !state.configured {
        return info_not_configured(GATE_TRUE_PEAK_CEILING);
    }

    let ceiling = constitution
        .audio_contract
        .as_ref()
        .and_then(|audio| audio.max_true_peak_dbtp)
        .unwrap_or(0.0);

    let (value_dbtp, metric_source) = if metrics.true_peak_dbtp.is_finite() {
        (metrics.true_peak_dbtp, "true_peak_dbtp")
    } else {
        (metrics.approx_true_peak_dbtp, "approx_true_peak_dbtp")
    };

    let pass = value_dbtp <= ceiling;

    GateResult {
        gate_name: GATE_TRUE_PEAK_CEILING.to_string(),
        severity: GateSeverity::Blocker,
        pass_fail: pass,
        evidence: json!({
            "configured": true,
            "value_dbtp": value_dbtp,
            "ceiling_dbtp": ceiling,
            "metric_source": metric_source,
        }),
        rationale: if pass {
            "true peak is within ceiling".to_string()
        } else {
            "true peak exceeds ceiling".to_string()
        },
    }
}

fn evaluate_clipping(
    metrics: &Metrics,
    constitution: &Constitution,
    state: TargetState,
) -> GateResult {
    if !state.configured {
        return info_not_configured(GATE_HARD_CLIPPING);
    }

    let clipping_allowed = constitution
        .audio_contract
        .as_ref()
        .and_then(|audio| audio.clipping_allowed)
        .unwrap_or(true);

    let pass = clipping_allowed || metrics.clipping_sample_count == 0;

    GateResult {
        gate_name: GATE_HARD_CLIPPING.to_string(),
        severity: GateSeverity::Blocker,
        pass_fail: pass,
        evidence: json!({
            "configured": true,
            "clipping_allowed": clipping_allowed,
            "clipping_sample_count": metrics.clipping_sample_count,
        }),
        rationale: if pass {
            "clipping contract satisfied".to_string()
        } else {
            "hard clipping not allowed and clipping samples detected".to_string()
        },
    }
}

fn evaluate_stereo_correlation(
    metrics: &Metrics,
    constitution: &Constitution,
    state: TargetState,
) -> GateResult {
    if !state.configured {
        return info_not_configured(GATE_STEREO_CORRELATION);
    }

    if !state.applicable {
        return info_not_applicable(GATE_STEREO_CORRELATION);
    }

    let min_corr = constitution
        .targets
        .as_ref()
        .and_then(|targets| targets.stereo.as_ref())
        .and_then(|stereo| stereo.correlation_min)
        .unwrap_or(-1.0);

    let value = metrics.correlation_min.unwrap_or(0.0);
    let (deviation, direction) = corridor_deviation(value, min_corr, 1.0);
    let pass = deviation == 0.0;

    GateResult {
        gate_name: GATE_STEREO_CORRELATION.to_string(),
        severity: GateSeverity::Warn,
        pass_fail: pass,
        evidence: json!({
            "configured": true,
            "applicable": true,
            "value": value,
            "min": min_corr,
            "max": 1.0,
            "deviation": deviation,
            "direction": direction_token(direction),
        }),
        rationale: if pass {
            "stereo correlation is within corridor".to_string()
        } else {
            "stereo correlation is outside corridor".to_string()
        },
    }
}

fn evaluate_lr_balance(
    metrics: &Metrics,
    constitution: &Constitution,
    state: TargetState,
) -> GateResult {
    if !state.configured {
        return info_not_configured(GATE_LR_BALANCE);
    }

    if !state.applicable {
        return info_not_applicable(GATE_LR_BALANCE);
    }

    let max_abs = constitution
        .targets
        .as_ref()
        .and_then(|targets| targets.stereo.as_ref())
        .and_then(|stereo| stereo.lr_balance_db_max_abs)
        .unwrap_or(0.0);

    let signed_value = metrics.lr_balance_db.unwrap_or(0.0);
    let value_abs = signed_value.abs();
    let (deviation, direction) = corridor_deviation(value_abs, 0.0, max_abs);
    let pass = deviation == 0.0;

    let lr_direction = if pass {
        "inside"
    } else if signed_value >= 0.0 {
        "lean_left"
    } else {
        "lean_right"
    };

    GateResult {
        gate_name: GATE_LR_BALANCE.to_string(),
        severity: GateSeverity::Warn,
        pass_fail: pass,
        evidence: json!({
            "configured": true,
            "applicable": true,
            "value_db": signed_value,
            "value_abs_db": value_abs,
            "min": 0.0,
            "max": max_abs,
            "deviation": deviation,
            "direction": lr_direction,
            "corridor_direction": direction_token(direction),
        }),
        rationale: if pass {
            "L/R balance is within corridor".to_string()
        } else {
            "L/R balance exceeds corridor".to_string()
        },
    }
}

fn evaluate_spectral(
    metrics: &Metrics,
    constitution: &Constitution,
    state: &crate::constitution::SpectralBandAvailability,
) -> GateResult {
    let bands = constitution.spectral_band_targets();
    if bands.configured_count() == 0 {
        return info_not_configured(GATE_SPECTRAL_BAND_OUTSIDE);
    }

    let mut rows = Vec::new();
    let mut violations = 0usize;

    for band in SpectralBandId::ALL {
        let target_state = state.get(band);
        if !target_state.configured {
            continue;
        }

        let range = bands.get(band).expect("configured spectral band has range");
        let value = spectral_band_value(&metrics.band_energies_db_rel, band);
        let (deviation, direction) = corridor_deviation(value, range.min(), range.max());
        if deviation > 0.0 {
            violations += 1;
        }

        rows.push(json!({
            "band": band.key(),
            "value_db_rel": value,
            "min": range.min(),
            "max": range.max(),
            "deviation": deviation,
            "direction": match direction {
                CorridorDirection::Inside => "inside",
                CorridorDirection::TooLow => "band_cold",
                CorridorDirection::TooHigh => "band_hot",
            },
        }));
    }

    let pass = violations == 0;

    GateResult {
        gate_name: GATE_SPECTRAL_BAND_OUTSIDE.to_string(),
        severity: GateSeverity::Warn,
        pass_fail: pass,
        evidence: json!({
            "configured": true,
            "configured_band_count": rows.len(),
            "violating_band_count": violations,
            "bands": rows,
        }),
        rationale: if pass {
            "spectral bands are within configured corridors".to_string()
        } else {
            "one or more spectral bands are outside configured corridors".to_string()
        },
    }
}

fn evaluate_dynamics(
    metrics: &Metrics,
    constitution: &Constitution,
    state: TargetState,
) -> GateResult {
    if !state.configured {
        return info_not_configured(GATE_DYNAMICS_OUTSIDE);
    }

    let range = constitution
        .targets
        .as_ref()
        .and_then(|targets| targets.dynamics.as_ref())
        .and_then(|dynamics| dynamics.crest_factor_db_range.as_ref())
        .expect("configured dynamics target has range");

    let value = metrics.crest_factor_db;
    let (deviation, direction) = corridor_deviation(value, range.min(), range.max());
    let pass = deviation == 0.0;

    GateResult {
        gate_name: GATE_DYNAMICS_OUTSIDE.to_string(),
        severity: GateSeverity::Warn,
        pass_fail: pass,
        evidence: json!({
            "configured": true,
            "value_db": value,
            "min": range.min(),
            "max": range.max(),
            "deviation": deviation,
            "direction": direction_token(direction),
        }),
        rationale: if pass {
            "crest factor is within corridor".to_string()
        } else {
            "crest factor is outside corridor".to_string()
        },
    }
}

fn evaluate_loudness(
    metrics: &Metrics,
    constitution: &Constitution,
    state: TargetState,
) -> GateResult {
    if !state.configured {
        return info_not_configured(GATE_LOUDNESS_OUTSIDE);
    }

    let range = constitution
        .targets
        .as_ref()
        .and_then(|targets| targets.loudness.as_ref())
        .and_then(|loudness| loudness.integrated_lufs_range.as_ref())
        .expect("configured loudness target has range");

    let value = metrics.integrated_lufs;
    let (deviation, direction) = corridor_deviation(value, range.min(), range.max());
    let pass = deviation == 0.0;

    GateResult {
        gate_name: GATE_LOUDNESS_OUTSIDE.to_string(),
        severity: GateSeverity::Warn,
        pass_fail: pass,
        evidence: json!({
            "configured": true,
            "value_lufs": value,
            "min": range.min(),
            "max": range.max(),
            "deviation": deviation,
            "direction": direction_token(direction),
        }),
        rationale: if pass {
            "integrated loudness is within corridor".to_string()
        } else {
            "integrated loudness is outside corridor".to_string()
        },
    }
}

fn compute_gate_status(gates: &[GateResult]) -> GateStatus {
    if gates
        .iter()
        .any(|gate| gate.severity == GateSeverity::Blocker && !gate.pass_fail)
    {
        GateStatus::Fail
    } else if gates
        .iter()
        .any(|gate| gate.severity == GateSeverity::Warn && !gate.pass_fail)
    {
        GateStatus::Warn
    } else {
        GateStatus::Pass
    }
}

fn info_not_configured(gate_name: &str) -> GateResult {
    info_gate(
        gate_name,
        json!({"configured": false}),
        "target not configured",
    )
}

fn info_not_applicable(gate_name: &str) -> GateResult {
    info_gate(
        gate_name,
        json!({"configured": true, "applicable": false}),
        "configured target not applicable",
    )
}

fn info_gate(gate_name: &str, evidence: Value, rationale: &str) -> GateResult {
    GateResult {
        gate_name: gate_name.to_string(),
        severity: GateSeverity::Info,
        pass_fail: true,
        evidence,
        rationale: rationale.to_string(),
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

#[cfg(test)]
mod tests {
    use super::{
        evaluate_gates, GateSeverity, GATE_FORMAT_SAMPLE_RATE, GATE_HARD_CLIPPING,
        GATE_LOUDNESS_OUTSIDE, GATE_STEREO_CORRELATION, GATE_TRUE_PEAK_CEILING,
    };
    use crate::{
        analyzer::{BandEnergies, Metrics},
        constitution::{build_target_availability, Constitution},
    };

    #[test]
    fn clipping_disallowed_and_detected_fails_blocker() {
        let constitution: Constitution = serde_yaml::from_str(
            r#"
constitution_version: "1.0"
audio_contract:
  clipping_allowed: false
"#,
        )
        .expect("constitution");

        let mut metrics = base_metrics();
        metrics.clipping_sample_count = 12;

        let availability = build_target_availability(&constitution, &metrics, None);
        let evaluation = evaluate_gates(&metrics, &constitution, &availability, None);

        let clipping_gate = evaluation
            .gates
            .iter()
            .find(|gate| gate.gate_name == GATE_HARD_CLIPPING)
            .expect("clipping gate");

        assert_eq!(clipping_gate.severity, GateSeverity::Blocker);
        assert!(!clipping_gate.pass_fail);
    }

    #[test]
    fn sample_rate_outside_allowed_fails_blocker() {
        let constitution: Constitution = serde_yaml::from_str(
            r#"
constitution_version: "1.0"
audio_contract:
  allowed_sample_rates_hz: [44100]
"#,
        )
        .expect("constitution");

        let mut metrics = base_metrics();
        metrics.sample_rate_hz = 48_000;

        let availability = build_target_availability(&constitution, &metrics, None);
        let evaluation = evaluate_gates(&metrics, &constitution, &availability, None);

        let sample_rate_gate = evaluation
            .gates
            .iter()
            .find(|gate| gate.gate_name == GATE_FORMAT_SAMPLE_RATE)
            .expect("sample rate gate");

        assert!(!sample_rate_gate.pass_fail);
    }

    #[test]
    fn true_peak_falls_back_to_approx_when_unavailable() {
        let constitution: Constitution = serde_yaml::from_str(
            r#"
constitution_version: "1.0"
audio_contract:
  max_true_peak_dbtp: -1.0
"#,
        )
        .expect("constitution");

        let mut metrics = base_metrics();
        metrics.true_peak_dbtp = f64::NAN;
        metrics.approx_true_peak_dbtp = 0.2;

        let availability = build_target_availability(&constitution, &metrics, None);
        let evaluation = evaluate_gates(&metrics, &constitution, &availability, None);

        let peak_gate = evaluation
            .gates
            .iter()
            .find(|gate| gate.gate_name == GATE_TRUE_PEAK_CEILING)
            .expect("true peak gate");

        assert!(!peak_gate.pass_fail);
        assert_eq!(peak_gate.evidence["metric_source"], "approx_true_peak_dbtp");
    }

    #[test]
    fn missing_targets_emit_configured_false_info() {
        let constitution: Constitution = serde_yaml::from_str(
            r#"
constitution_version: "1.0"
"#,
        )
        .expect("constitution");

        let metrics = base_metrics();
        let availability = build_target_availability(&constitution, &metrics, None);
        let evaluation = evaluate_gates(&metrics, &constitution, &availability, None);

        let loudness_gate = evaluation
            .gates
            .iter()
            .find(|gate| gate.gate_name == GATE_LOUDNESS_OUTSIDE)
            .expect("loudness gate");

        assert!(loudness_gate.pass_fail);
        assert_eq!(loudness_gate.severity, GateSeverity::Info);
        assert_eq!(loudness_gate.evidence["configured"], false);
    }

    #[test]
    fn stereo_configured_on_mono_is_not_applicable() {
        let constitution: Constitution = serde_yaml::from_str(
            r#"
constitution_version: "1.0"
targets:
  stereo:
    correlation_min: 0.0
"#,
        )
        .expect("constitution");

        let mut metrics = base_metrics();
        metrics.channels = 1;
        metrics.correlation_min = None;

        let availability = build_target_availability(&constitution, &metrics, None);
        let evaluation = evaluate_gates(&metrics, &constitution, &availability, None);

        let stereo_gate = evaluation
            .gates
            .iter()
            .find(|gate| gate.gate_name == GATE_STEREO_CORRELATION)
            .expect("stereo correlation gate");

        assert!(stereo_gate.pass_fail);
        assert_eq!(stereo_gate.severity, GateSeverity::Info);
        assert_eq!(stereo_gate.evidence["configured"], true);
        assert_eq!(stereo_gate.evidence["applicable"], false);
    }

    #[test]
    fn loudness_corridor_boundaries_are_inclusive() {
        let constitution: Constitution = serde_yaml::from_str(
            r#"
constitution_version: "1.0"
targets:
  loudness:
    integrated_lufs_range: [-16.0, -12.0]
"#,
        )
        .expect("constitution");

        let mut at_min = base_metrics();
        at_min.integrated_lufs = -16.0;
        let availability_at_min = build_target_availability(&constitution, &at_min, None);
        let evaluation_at_min = evaluate_gates(&at_min, &constitution, &availability_at_min, None);
        let gate_at_min = evaluation_at_min
            .gates
            .iter()
            .find(|gate| gate.gate_name == GATE_LOUDNESS_OUTSIDE)
            .expect("loudness gate at min");
        assert!(gate_at_min.pass_fail);

        let mut at_max = base_metrics();
        at_max.integrated_lufs = -12.0;
        let availability_at_max = build_target_availability(&constitution, &at_max, None);
        let evaluation_at_max = evaluate_gates(&at_max, &constitution, &availability_at_max, None);
        let gate_at_max = evaluation_at_max
            .gates
            .iter()
            .find(|gate| gate.gate_name == GATE_LOUDNESS_OUTSIDE)
            .expect("loudness gate at max");
        assert!(gate_at_max.pass_fail);

        let mut below_min = base_metrics();
        below_min.integrated_lufs = -16.1;
        let availability_below_min = build_target_availability(&constitution, &below_min, None);
        let evaluation_below_min =
            evaluate_gates(&below_min, &constitution, &availability_below_min, None);
        let gate_below_min = evaluation_below_min
            .gates
            .iter()
            .find(|gate| gate.gate_name == GATE_LOUDNESS_OUTSIDE)
            .expect("loudness gate below min");
        assert!(!gate_below_min.pass_fail);

        let mut above_max = base_metrics();
        above_max.integrated_lufs = -11.9;
        let availability_above_max = build_target_availability(&constitution, &above_max, None);
        let evaluation_above_max =
            evaluate_gates(&above_max, &constitution, &availability_above_max, None);
        let gate_above_max = evaluation_above_max
            .gates
            .iter()
            .find(|gate| gate.gate_name == GATE_LOUDNESS_OUTSIDE)
            .expect("loudness gate above max");
        assert!(!gate_above_max.pass_fail);
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
