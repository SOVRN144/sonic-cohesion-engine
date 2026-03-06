use crate::{
    analyzer::{self, DecodedAudio, Metrics},
    constitution::{self, Constitution},
    drift::{self, DriftResult},
    gates::{self, GateEvaluation, GateSeverity},
};
use anyhow::{anyhow, bail};
use std::f64::consts::PI;

const MONO_PHONE_FILTERS: [FilterSpec; 3] = [
    FilterSpec::high_pass(220.0, 0.707),
    FilterSpec::low_pass(4500.0, 0.707),
    FilterSpec::peaking(2600.0, 3.0, 0.900),
];

const LAPTOP_SPEAKERS_FILTERS: [FilterSpec; 4] = [
    FilterSpec::high_pass(120.0, 0.707),
    FilterSpec::peaking(280.0, -2.0, 1.000),
    FilterSpec::peaking(3200.0, 2.0, 0.900),
    FilterSpec::low_pass(15000.0, 0.707),
];

const EARBUDS_FILTERS: [FilterSpec; 4] = [
    FilterSpec::high_pass(45.0, 0.707),
    FilterSpec::low_shelf(100.0, 2.0, 0.707),
    FilterSpec::peaking(3000.0, 1.5, 1.000),
    FilterSpec::high_shelf(9000.0, 1.0, 0.707),
];

const CAR_FILTERS: [FilterSpec; 4] = [
    FilterSpec::high_pass(35.0, 0.707),
    FilterSpec::low_shelf(80.0, 3.0, 0.707),
    FilterSpec::peaking(250.0, -1.5, 1.000),
    FilterSpec::peaking(2500.0, 1.0, 1.000),
];

const CLUB_PA_FILTERS: [FilterSpec; 5] = [
    FilterSpec::high_pass(30.0, 0.707),
    FilterSpec::low_shelf(63.0, 4.0, 0.707),
    FilterSpec::peaking(350.0, -1.5, 1.000),
    FilterSpec::peaking(2000.0, 1.5, 0.900),
    FilterSpec::high_shelf(10000.0, -2.0, 0.707),
];

const MONO_COMPAT_FILTERS: [FilterSpec; 0] = [];

const TARGETS: [TargetSpec; 6] = [
    TargetSpec::new("mono_phone", ChannelModel::Mono, 0.0, &MONO_PHONE_FILTERS),
    TargetSpec::new(
        "laptop_speakers",
        ChannelModel::Stereo,
        0.50,
        &LAPTOP_SPEAKERS_FILTERS,
    ),
    TargetSpec::new("earbuds", ChannelModel::Stereo, 0.90, &EARBUDS_FILTERS),
    TargetSpec::new("car", ChannelModel::Stereo, 0.70, &CAR_FILTERS),
    TargetSpec::new("club_pa", ChannelModel::Stereo, 0.85, &CLUB_PA_FILTERS),
    TargetSpec::new("mono_compat", ChannelModel::Mono, 0.0, &MONO_COMPAT_FILTERS),
];

#[derive(Debug, Clone)]
pub(crate) struct RawTargetResult {
    pub(crate) target_id: &'static str,
    pub(crate) is_mono: bool,
    pub(crate) metrics: Metrics,
    pub(crate) gate_evaluation: GateEvaluation,
    pub(crate) drift_result: DriftResult,
    pub(crate) failed_blockers: u32,
    pub(crate) failed_warns: u32,
    pub(crate) translation_risk: u32,
}

#[derive(Debug, Clone, Copy)]
enum ChannelModel {
    Mono,
    Stereo,
}

#[derive(Debug, Clone, Copy)]
enum FilterKind {
    HighPass,
    LowPass,
    Peaking,
    LowShelf,
    HighShelf,
}

#[derive(Debug, Clone, Copy)]
struct FilterSpec {
    kind: FilterKind,
    frequency_hz: f64,
    gain_db: f64,
    q: f64,
}

impl FilterSpec {
    const fn high_pass(frequency_hz: f64, q: f64) -> Self {
        Self {
            kind: FilterKind::HighPass,
            frequency_hz,
            gain_db: 0.0,
            q,
        }
    }

    const fn low_pass(frequency_hz: f64, q: f64) -> Self {
        Self {
            kind: FilterKind::LowPass,
            frequency_hz,
            gain_db: 0.0,
            q,
        }
    }

    const fn peaking(frequency_hz: f64, gain_db: f64, q: f64) -> Self {
        Self {
            kind: FilterKind::Peaking,
            frequency_hz,
            gain_db,
            q,
        }
    }

    const fn low_shelf(frequency_hz: f64, gain_db: f64, q: f64) -> Self {
        Self {
            kind: FilterKind::LowShelf,
            frequency_hz,
            gain_db,
            q,
        }
    }

    const fn high_shelf(frequency_hz: f64, gain_db: f64, q: f64) -> Self {
        Self {
            kind: FilterKind::HighShelf,
            frequency_hz,
            gain_db,
            q,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TargetSpec {
    target_id: &'static str,
    channel_model: ChannelModel,
    stereo_width: f64,
    filters: &'static [FilterSpec],
}

impl TargetSpec {
    const fn new(
        target_id: &'static str,
        channel_model: ChannelModel,
        stereo_width: f64,
        filters: &'static [FilterSpec],
    ) -> Self {
        Self {
            target_id,
            channel_model,
            stereo_width,
            filters,
        }
    }

    fn is_mono(self) -> bool {
        matches!(self.channel_model, ChannelModel::Mono)
    }
}

#[derive(Debug, Clone)]
struct StereoBasis {
    left: Vec<f64>,
    right: Vec<f64>,
}

impl StereoBasis {
    fn source_peak_linear(&self) -> f64 {
        self.left
            .iter()
            .chain(self.right.iter())
            .map(|sample| sample.abs())
            .fold(0.0, f64::max)
    }
}

#[derive(Debug, Clone, Copy)]
struct BiquadCoefficients {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

#[derive(Debug, Default, Clone, Copy)]
struct DirectForm1State {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl DirectForm1State {
    fn process(&mut self, coeffs: BiquadCoefficients, sample: f64) -> f64 {
        let out = coeffs.b0 * sample + coeffs.b1 * self.x1 + coeffs.b2 * self.x2
            - coeffs.a1 * self.y1
            - coeffs.a2 * self.y2;

        self.x2 = self.x1;
        self.x1 = sample;
        self.y2 = self.y1;
        self.y1 = out;
        out
    }
}

pub(crate) fn simulate_targets(
    decoded: &DecodedAudio,
    analyzer_version: &str,
    lossy_source: bool,
    constitution: &Constitution,
) -> anyhow::Result<Vec<RawTargetResult>> {
    let stereo_basis = build_stereo_basis(decoded)?;
    let source_peak = stereo_basis.source_peak_linear();
    let mut results = Vec::with_capacity(TARGETS.len());

    for target in TARGETS {
        let transformed_samples =
            transform_target(&stereo_basis, decoded.sample_rate_hz(), target)?;
        let attenuated = attenuate_to_source_peak(transformed_samples, source_peak);
        let transformed_audio = DecodedAudio::from_samples(decoded.sample_rate_hz(), attenuated)?;
        let metrics =
            analyzer::analyze_decoded(&transformed_audio, analyzer_version, lossy_source)?;
        let availability = constitution::build_target_availability(constitution, &metrics, None);
        let gate_evaluation = gates::evaluate_gates(&metrics, constitution, &availability, None);
        let drift_result = drift::compute_drift(&metrics, constitution, &availability);
        let (failed_blockers, failed_warns) = count_failed_gates(&gate_evaluation);
        let translation_risk =
            (drift_result.drift_score + failed_blockers * 15 + failed_warns * 5).clamp(0, 100);

        results.push(RawTargetResult {
            target_id: target.target_id,
            is_mono: target.is_mono(),
            metrics,
            gate_evaluation,
            drift_result,
            failed_blockers,
            failed_warns,
            translation_risk,
        });
    }

    Ok(results)
}

fn build_stereo_basis(decoded: &DecodedAudio) -> anyhow::Result<StereoBasis> {
    let samples = decoded.samples();
    if samples.is_empty() {
        bail!("decoded audio must contain at least one channel");
    }

    let frame_count = decoded.frame_count();
    let basis = match decoded.channels() {
        0 => bail!("decoded audio channel count is zero"),
        1 => StereoBasis {
            left: samples[0][..frame_count].to_vec(),
            right: samples[0][..frame_count].to_vec(),
        },
        2 => StereoBasis {
            left: samples[0][..frame_count].to_vec(),
            right: samples[1][..frame_count].to_vec(),
        },
        _ => {
            let mut left = vec![0.0; frame_count];
            let mut right = vec![0.0; frame_count];
            let mut even_count = 0usize;
            let mut odd_count = 0usize;

            for (channel_idx, channel) in samples.iter().enumerate() {
                let source = &channel[..frame_count];
                if channel_idx % 2 == 0 {
                    even_count += 1;
                    for (dst, sample) in left.iter_mut().zip(source.iter()) {
                        *dst += *sample;
                    }
                } else {
                    odd_count += 1;
                    for (dst, sample) in right.iter_mut().zip(source.iter()) {
                        *dst += *sample;
                    }
                }
            }

            if even_count == 0 {
                bail!("decoded audio has no even-indexed channels");
            }

            for sample in &mut left {
                *sample /= even_count as f64;
            }

            if odd_count == 0 {
                right.clone_from(&left);
            } else {
                for sample in &mut right {
                    *sample /= odd_count as f64;
                }
            }

            StereoBasis { left, right }
        }
    };

    Ok(basis)
}

fn transform_target(
    basis: &StereoBasis,
    sample_rate_hz: u32,
    target: TargetSpec,
) -> anyhow::Result<Vec<Vec<f64>>> {
    let mut channels = match target.channel_model {
        ChannelModel::Stereo => apply_stereo_width(basis, target.stereo_width),
        ChannelModel::Mono => {
            // Translation Matrix uses arithmetic mono fold-down everywhere, including mono_phone
            // and mono_compat; no equal-power variant is allowed in this operator.
            vec![fold_to_mono(basis)]
        }
    };

    apply_filter_chain(&mut channels, sample_rate_hz, target.filters)?;
    Ok(channels)
}

fn apply_stereo_width(basis: &StereoBasis, width: f64) -> Vec<Vec<f64>> {
    let mut left = Vec::with_capacity(basis.left.len());
    let mut right = Vec::with_capacity(basis.right.len());

    for (&l, &r) in basis.left.iter().zip(basis.right.iter()) {
        let mid = 0.5 * (l + r);
        let side = 0.5 * (l - r);
        left.push(mid + side * width);
        right.push(mid - side * width);
    }

    vec![left, right]
}

fn fold_to_mono(basis: &StereoBasis) -> Vec<f64> {
    basis
        .left
        .iter()
        .zip(basis.right.iter())
        .map(|(&left, &right)| 0.5 * (left + right))
        .collect()
}

fn apply_filter_chain(
    channels: &mut [Vec<f64>],
    sample_rate_hz: u32,
    filters: &[FilterSpec],
) -> anyhow::Result<()> {
    for spec in filters {
        let coeffs = biquad_coefficients(*spec, sample_rate_hz)?;
        for channel in channels.iter_mut() {
            let mut state = DirectForm1State::default();
            for sample in channel.iter_mut() {
                *sample = state.process(coeffs, *sample);
            }
        }
    }

    Ok(())
}

fn attenuate_to_source_peak(mut channels: Vec<Vec<f64>>, source_peak: f64) -> Vec<Vec<f64>> {
    let transformed_peak = peak_linear(&channels);
    if transformed_peak <= source_peak || transformed_peak == 0.0 {
        return channels;
    }

    let scale = source_peak / transformed_peak;
    for channel in &mut channels {
        for sample in channel {
            *sample *= scale;
        }
    }

    channels
}

fn peak_linear(channels: &[Vec<f64>]) -> f64 {
    channels
        .iter()
        .flat_map(|channel| channel.iter())
        .map(|sample| sample.abs())
        .fold(0.0, f64::max)
}

fn count_failed_gates(gate_evaluation: &GateEvaluation) -> (u32, u32) {
    let mut failed_blockers = 0u32;
    let mut failed_warns = 0u32;

    for gate in gate_evaluation.gates.iter().filter(|gate| !gate.pass_fail) {
        match gate.severity {
            GateSeverity::Blocker => failed_blockers += 1,
            GateSeverity::Warn => failed_warns += 1,
            GateSeverity::Info => {}
        }
    }

    (failed_blockers, failed_warns)
}

fn biquad_coefficients(
    spec: FilterSpec,
    sample_rate_hz: u32,
) -> anyhow::Result<BiquadCoefficients> {
    if sample_rate_hz == 0 {
        bail!("sample_rate_hz must be non-zero");
    }
    if !spec.frequency_hz.is_finite() || spec.frequency_hz <= 0.0 {
        bail!("filter frequency must be positive and finite");
    }
    if !spec.q.is_finite() || spec.q <= 0.0 {
        bail!("filter q must be positive and finite");
    }
    if !spec.gain_db.is_finite() {
        bail!("filter gain must be finite");
    }

    let nyquist = sample_rate_hz as f64 / 2.0;
    let frequency_hz = spec.frequency_hz.min((nyquist - 1e-9).max(1e-9));
    let w0 = 2.0 * PI * frequency_hz / sample_rate_hz as f64;
    let cos_w0 = w0.cos();
    let sin_w0 = w0.sin();
    let alpha = sin_w0 / (2.0 * spec.q);

    let coeffs = match spec.kind {
        FilterKind::LowPass => {
            let b0 = (1.0 - cos_w0) / 2.0;
            let b1 = 1.0 - cos_w0;
            let b2 = (1.0 - cos_w0) / 2.0;
            let a0 = 1.0 + alpha;
            let a1 = -2.0 * cos_w0;
            let a2 = 1.0 - alpha;
            normalize_coefficients(b0, b1, b2, a0, a1, a2)?
        }
        FilterKind::HighPass => {
            let b0 = (1.0 + cos_w0) / 2.0;
            let b1 = -(1.0 + cos_w0);
            let b2 = (1.0 + cos_w0) / 2.0;
            let a0 = 1.0 + alpha;
            let a1 = -2.0 * cos_w0;
            let a2 = 1.0 - alpha;
            normalize_coefficients(b0, b1, b2, a0, a1, a2)?
        }
        FilterKind::Peaking => {
            let a = 10f64.powf(spec.gain_db / 40.0);
            let b0 = 1.0 + alpha * a;
            let b1 = -2.0 * cos_w0;
            let b2 = 1.0 - alpha * a;
            let a0 = 1.0 + alpha / a;
            let a1 = -2.0 * cos_w0;
            let a2 = 1.0 - alpha / a;
            normalize_coefficients(b0, b1, b2, a0, a1, a2)?
        }
        FilterKind::LowShelf => shelf_coefficients(spec, cos_w0, sin_w0, false)?,
        FilterKind::HighShelf => shelf_coefficients(spec, cos_w0, sin_w0, true)?,
    };

    Ok(coeffs)
}

fn shelf_coefficients(
    spec: FilterSpec,
    cos_w0: f64,
    sin_w0: f64,
    high: bool,
) -> anyhow::Result<BiquadCoefficients> {
    let a = 10f64.powf(spec.gain_db / 40.0);
    let sqrt_a = a.sqrt();
    let slope = spec.q;
    let alpha = sin_w0 / 2.0 * (((a + 1.0 / a) * (1.0 / slope - 1.0) + 2.0).sqrt());

    let (b0, b1, b2, a0, a1, a2) = if high {
        (
            a * ((a + 1.0) + (a - 1.0) * cos_w0 + 2.0 * sqrt_a * alpha),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0),
            a * ((a + 1.0) + (a - 1.0) * cos_w0 - 2.0 * sqrt_a * alpha),
            (a + 1.0) - (a - 1.0) * cos_w0 + 2.0 * sqrt_a * alpha,
            2.0 * ((a - 1.0) - (a + 1.0) * cos_w0),
            (a + 1.0) - (a - 1.0) * cos_w0 - 2.0 * sqrt_a * alpha,
        )
    } else {
        (
            a * ((a + 1.0) - (a - 1.0) * cos_w0 + 2.0 * sqrt_a * alpha),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0),
            a * ((a + 1.0) - (a - 1.0) * cos_w0 - 2.0 * sqrt_a * alpha),
            (a + 1.0) + (a - 1.0) * cos_w0 + 2.0 * sqrt_a * alpha,
            -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0),
            (a + 1.0) + (a - 1.0) * cos_w0 - 2.0 * sqrt_a * alpha,
        )
    };

    normalize_coefficients(b0, b1, b2, a0, a1, a2)
}

fn normalize_coefficients(
    b0: f64,
    b1: f64,
    b2: f64,
    a0: f64,
    a1: f64,
    a2: f64,
) -> anyhow::Result<BiquadCoefficients> {
    if !a0.is_finite() || a0.abs() < f64::EPSILON {
        return Err(anyhow!("biquad normalization coefficient is invalid"));
    }

    let coeffs = BiquadCoefficients {
        b0: b0 / a0,
        b1: b1 / a0,
        b2: b2 / a0,
        a1: a1 / a0,
        a2: a2 / a0,
    };

    if ![coeffs.b0, coeffs.b1, coeffs.b2, coeffs.a1, coeffs.a2]
        .iter()
        .all(|value| value.is_finite())
    {
        bail!("biquad coefficients must be finite");
    }

    Ok(coeffs)
}

#[cfg(test)]
mod tests {
    use super::{attenuate_to_source_peak, build_stereo_basis, fold_to_mono, simulate_targets};
    use crate::{
        analyzer::{self, DecodedAudio},
        constitution::Constitution,
    };
    use std::{f64::consts::PI, io::Write, path::Path};

    #[test]
    fn stereo_basis_for_multichannel_uses_even_odd_means() {
        let decoded = DecodedAudio::from_samples(
            48_000,
            vec![
                vec![1.0, 2.0],
                vec![10.0, 20.0],
                vec![3.0, 4.0],
                vec![30.0, 40.0],
            ],
        )
        .expect("decoded");

        let basis = build_stereo_basis(&decoded).expect("basis");

        assert_eq!(basis.left, vec![2.0, 3.0]);
        assert_eq!(basis.right, vec![20.0, 30.0]);
    }

    #[test]
    fn mono_fold_down_uses_arithmetic_average() {
        let basis = super::StereoBasis {
            left: vec![1.0, -1.0, 0.5],
            right: vec![0.0, 1.0, -0.5],
        };

        assert_eq!(fold_to_mono(&basis), vec![0.5, 0.0, 0.0]);
    }

    #[test]
    fn attenuation_uses_stereo_basis_source_peak() {
        let source_peak = 0.25;
        let attenuated =
            attenuate_to_source_peak(vec![vec![0.5, -0.5], vec![0.125, -0.125]], source_peak);

        assert!((attenuated[0][0] - 0.25).abs() < 1e-12);
        assert!((attenuated[0][1] + 0.25).abs() < 1e-12);
    }

    #[tokio::test]
    async fn pure_simulation_can_be_exercised_in_memory() {
        let td = tempfile::tempdir().expect("tempdir");
        let wav_path = td.path().join("tone.wav");
        write_wav_i16(&wav_path, 48_000, 2, 48_000, |frame_idx, channel_idx| {
            let t = frame_idx as f64 / 48_000.0;
            let phase = (2.0 * PI * 440.0 * t).sin() * 0.25;
            if channel_idx == 0 {
                phase
            } else {
                phase * 0.8
            }
        })
        .expect("write wav");

        let decoded = analyzer::decode_audio(&wav_path).expect("decode");
        let constitution: Constitution = serde_yaml::from_str(
            r#"
constitution_version: "1.0"
audio_contract:
  allowed_sample_rates_hz: [48000]
  max_true_peak_dbtp: -0.5
  clipping_allowed: false
targets:
  loudness:
    integrated_lufs_range: [-18.0, -10.0]
  dynamics:
    crest_factor_db_range: [3.0, 18.0]
  spectral_balance:
    bands_db:
      hz_20_60: [-12.0, 6.0]
      hz_60_150: [-12.0, 6.0]
      hz_150_500: [-12.0, 6.0]
      hz_500_2000: [-12.0, 6.0]
      hz_2000_8000: [-12.0, 6.0]
      hz_8000_16000: [-12.0, 6.0]
  stereo:
    correlation_min: -1.0
    lr_balance_db_max_abs: 6.0
"#,
        )
        .expect("constitution");

        let results = simulate_targets(&decoded, "TelemetryAnalyzer/1.0.0", false, &constitution)
            .expect("simulate");

        assert_eq!(results.len(), 6);
        assert_eq!(results[0].target_id, "mono_phone");
        assert_eq!(results[0].metrics.channels, 1);
        assert_eq!(results[0].metrics.correlation_min, None);
        assert_eq!(results[5].target_id, "mono_compat");
        assert_eq!(results[5].metrics.channels, 1);
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
                let value = (sample_fn(frame_idx, channel_idx) * 32767.0)
                    .round()
                    .clamp(i16::MIN as f64, i16::MAX as f64) as i16;
                pcm.push(value);
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
}
