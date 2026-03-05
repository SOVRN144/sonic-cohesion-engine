use sce_core::{
    analyzer::{BandEnergies, Metrics},
    constitution::{build_target_availability, Constitution},
    drift::{compute_drift, DriftResult},
    gates::{evaluate_gates, GateEvaluation},
};
use serde_json::Value;

#[test]
fn governance_outputs_are_deterministic_after_normalization() {
    let constitution: Constitution = serde_yaml::from_str(
        r#"
constitution_version: "1.0"
audio_contract:
  allowed_sample_rates_hz: [44100, 48000]
  max_true_peak_dbtp: -1.0
  clipping_allowed: false
targets:
  loudness:
    integrated_lufs_range: [-16.0, -12.0]
  dynamics:
    crest_factor_db_range: [7.0, 16.0]
  spectral_balance:
    bands_db:
      sub_20_60: [-3.0, 3.0]
      low_60_150: [-2.0, 2.0]
      lowmid_150_500: [-2.0, 2.0]
      mid_500_2k: [-2.0, 2.0]
      high_2k_8k: [-3.0, 1.0]
      air_8k_16k: [-4.0, 0.0]
  stereo:
    correlation_min: 0.0
    lr_balance_db_max_abs: 1.5
priorities:
  weights:
    low_end_translation: 1.4
    harshness_control: 1.2
    width_control: 0.9
    transient_punch: 1.0
    loudness_compliance: 1.1
"#,
    )
    .expect("parse constitution");

    let metrics = base_metrics();

    let availability_a = build_target_availability(&constitution, &metrics, Some(24));
    let gates_a = evaluate_gates(&metrics, &constitution, &availability_a, Some(24));
    let drift_a = compute_drift(&metrics, &constitution, &availability_a);

    let availability_b = build_target_availability(&constitution, &metrics, Some(24));
    let gates_b = evaluate_gates(&metrics, &constitution, &availability_b, Some(24));
    let drift_b = compute_drift(&metrics, &constitution, &availability_b);

    let norm_a = normalize_governance(gates_a, drift_a);
    let norm_b = normalize_governance(gates_b, drift_b);

    assert_eq!(norm_a, norm_b);
    assert_eq!(
        serde_json::to_string(&norm_a).expect("json a"),
        serde_json::to_string(&norm_b).expect("json b")
    );
}

fn normalize_governance(
    mut gates: GateEvaluation,
    mut drift: DriftResult,
) -> (GateEvaluation, DriftResult) {
    for gate in &mut gates.gates {
        normalize_gate_evidence(&gate.gate_name, &mut gate.evidence);
    }

    drift.drift_raw = round9(drift.drift_raw);
    round_opt(&mut drift.domain_scores.loudness);
    round_opt(&mut drift.domain_scores.dynamics);
    round_opt(&mut drift.domain_scores.spectral_balance);
    round_opt(&mut drift.domain_scores.stereo);
    round_opt(&mut drift.domain_weights_effective.loudness);
    round_opt(&mut drift.domain_weights_effective.dynamics);
    round_opt(&mut drift.domain_weights_effective.spectral_balance);
    round_opt(&mut drift.domain_weights_effective.stereo);

    for item in &mut drift.drift_vector {
        item.deviation = round9(item.deviation);
        item.weighted_deviation = round9(item.weighted_deviation);
        normalize_drift_vector_evidence(&mut item.evidence);
    }

    for item in &mut drift.fix_list {
        item.delta = round9(item.delta);
    }

    (gates, drift)
}

fn normalize_gate_evidence(gate_name: &str, evidence: &mut Value) {
    match gate_name {
        "G003_TruePeakCeiling" => {
            round_value(evidence, "value_dbtp");
            round_value(evidence, "ceiling_dbtp");
        }
        "W001_StereoCorrelation" => {
            round_value(evidence, "value");
            round_value(evidence, "min");
            round_value(evidence, "max");
            round_value(evidence, "deviation");
        }
        "W002_LRBalance" => {
            round_value(evidence, "value_db");
            round_value(evidence, "value_abs_db");
            round_value(evidence, "min");
            round_value(evidence, "max");
            round_value(evidence, "deviation");
        }
        "W003_SpectralBandOutside" => {
            if let Some(rows) = evidence.get_mut("bands").and_then(Value::as_array_mut) {
                for row in rows {
                    round_value(row, "value_db_rel");
                    round_value(row, "min");
                    round_value(row, "max");
                    round_value(row, "deviation");
                }
            }
        }
        "W004_DynamicsOutside" => {
            round_value(evidence, "value_db");
            round_value(evidence, "min");
            round_value(evidence, "max");
            round_value(evidence, "deviation");
        }
        "W005_LoudnessOutside" => {
            round_value(evidence, "value_lufs");
            round_value(evidence, "min");
            round_value(evidence, "max");
            round_value(evidence, "deviation");
        }
        "I002_TransientProxyRecorded" => {
            round_value(evidence, "value");
        }
        _ => {}
    }
}

fn normalize_drift_vector_evidence(evidence: &mut Value) {
    round_value(evidence, "value");
    round_value(evidence, "value_abs");
    round_value(evidence, "min");
    round_value(evidence, "max");
    round_value(evidence, "delta");
}

fn round_opt(value: &mut Option<f64>) {
    if let Some(inner) = value {
        *inner = round9(*inner);
    }
}

fn round_value(value: &mut Value, key: &str) {
    if let Some(field) = value.get_mut(key) {
        if let Some(number) = field.as_f64() {
            *field = Value::from(round9(number));
        }
    }
}

fn round9(value: f64) -> f64 {
    (value * 1_000_000_000.0).round() / 1_000_000_000.0
}

fn base_metrics() -> Metrics {
    Metrics {
        analyzer_version: "TelemetryAnalyzer/1.0.0".to_string(),
        sample_rate_hz: 44_100,
        channels: 2,
        frame_count: 44_100,
        duration_seconds: 1.0,
        sample_peak_linear: 0.82,
        sample_peak_dbfs: -1.724,
        clipping_sample_count: 12,
        rms_dbfs: -11.2,
        crest_factor_db: 4.6,
        short_term_rms_series_dbfs: vec![-11.2, -10.9, -11.0],
        approx_true_peak_dbtp: -0.8,
        true_peak_dbtp: -0.7,
        band_energies_db_rel: BandEnergies {
            hz_20_60: 3.4,
            hz_60_150: 2.3,
            hz_150_500: -1.5,
            hz_500_2000: 0.2,
            hz_2000_8000: 2.1,
            hz_8000_16000: -4.5,
        },
        spectral_centroid_hz: 1800.0,
        correlation_min: Some(-0.1),
        correlation_mean: Some(0.2),
        lr_balance_db: Some(2.0),
        lossy_source: false,
        integrated_lufs: -19.0,
        short_term_lufs_series: vec![-19.0, -18.8],
        tonal_balance_curve: vec![0.0; 30],
        transient_density: 0.3,
    }
}
