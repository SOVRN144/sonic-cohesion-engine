use anyhow::{anyhow, bail, Context};
use ebur128::{EbuR128, Mode};
use rustfft::{num_complex::Complex, FftPlanner};
use serde::{Deserialize, Serialize};
use std::{f64::consts::PI, fs::File, path::Path};
use symphonia::core::{
    audio::{AudioBufferRef, SampleBuffer},
    codecs::{DecoderOptions, CODEC_TYPE_NULL},
    errors::Error as SymphoniaError,
    formats::FormatOptions,
    io::MediaSourceStream,
    meta::MetadataOptions,
    probe::Hint,
};

pub const EPS: f64 = 1e-12;
const FFT_SIZE: usize = 4096;
const FFT_HOP: usize = 1024;
const STEREO_WINDOW: usize = 4096;
const STEREO_HOP: usize = 4096;
const RMS_WINDOW_MS: u32 = 400;
const RMS_HOP_MS: u32 = 100;
const LUFS_WINDOW_MS: u32 = 3000;
const LUFS_HOP_MS: u32 = 1000;
const TRANSIENT_RMS_JUMP_DB: f64 = 6.0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BandEnergies {
    pub hz_20_60: f64,
    pub hz_60_150: f64,
    pub hz_150_500: f64,
    pub hz_500_2000: f64,
    pub hz_2000_8000: f64,
    pub hz_8000_16000: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metrics {
    pub analyzer_version: String,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub frame_count: u64,
    pub duration_seconds: f64,
    pub sample_peak_linear: f64,
    pub sample_peak_dbfs: f64,
    pub clipping_sample_count: u64,
    pub rms_dbfs: f64,
    pub crest_factor_db: f64,
    pub short_term_rms_series_dbfs: Vec<f64>,
    pub approx_true_peak_dbtp: f64,
    pub true_peak_dbtp: f64,
    pub band_energies_db_rel: BandEnergies,
    pub spectral_centroid_hz: f64,
    pub correlation_min: Option<f64>,
    pub correlation_mean: Option<f64>,
    pub lr_balance_db: Option<f64>,
    pub lossy_source: bool,
    pub integrated_lufs: f64,
    pub short_term_lufs_series: Vec<f64>,
    pub tonal_balance_curve: Vec<f64>,
    pub transient_density: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum AnalyzerError {
    #[error("{0}")]
    Decode(String),
    #[error("{0}")]
    Analysis(String),
}

#[derive(Debug)]
struct DecodedAudio {
    sample_rate_hz: u32,
    channels: u16,
    samples: Vec<Vec<f64>>,
}

struct SpectralMetrics {
    band_energies_db_rel: BandEnergies,
    spectral_centroid_hz: f64,
    tonal_balance_curve: Vec<f64>,
}

struct StereoMetrics {
    correlation_min: Option<f64>,
    correlation_mean: Option<f64>,
    lr_balance_db: Option<f64>,
}

pub async fn analyze_file(path: &Path, analyzer_version: &str) -> Result<Metrics, AnalyzerError> {
    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("bin")
        .to_ascii_lowercase();

    let decoded = decode_audio(path).map_err(|err| AnalyzerError::Decode(err.to_string()))?;
    compute_metrics(&decoded, analyzer_version, &extension)
        .map_err(|err| AnalyzerError::Analysis(err.to_string()))
}

fn decode_audio(path: &Path) -> anyhow::Result<DecodedAudio> {
    let source = File::open(path).with_context(|| format!("open file {}", path.display()))?;
    let stream = MediaSourceStream::new(Box::new(source), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .with_context(|| format!("probe format for {}", path.display()))?;

    let mut format = probed.format;

    let (track_id, codec_params) = {
        let track = format
            .default_track()
            .ok_or_else(|| anyhow!("no default audio track"))?;
        (track.id, track.codec_params.clone())
    };

    if codec_params.codec == CODEC_TYPE_NULL {
        bail!("unsupported codec");
    }

    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .with_context(|| "create decoder")?;

    let mut sample_rate_hz = codec_params.sample_rate;
    let mut channels = codec_params.channels.map(|ch| ch.count() as u16);
    let mut samples: Vec<Vec<f64>> = Vec::new();
    let mut decoded_any = false;

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => bail!("decoder reset required"),
            Err(err) => return Err(anyhow!("read packet: {err}")),
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymphoniaError::DecodeError(err)) => bail!("{err}"),
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => bail!("decoder reset required"),
            Err(err) => return Err(anyhow!("decode packet: {err}")),
        };

        let spec = *decoded.spec();
        sample_rate_hz.get_or_insert(spec.rate);
        let current_channels = spec.channels.count() as u16;
        if let Some(existing_channels) = channels {
            if existing_channels != current_channels {
                bail!(
                    "channel count changed during decode: {existing_channels} -> {current_channels}"
                );
            }
        } else {
            channels = Some(current_channels);
        }

        if samples.is_empty() {
            samples = vec![Vec::new(); current_channels as usize];
        }

        append_decoded(decoded, current_channels as usize, &mut samples)?;
        decoded_any = true;
    }

    if !decoded_any {
        bail!("no decodable audio frames");
    }

    let sample_rate_hz = sample_rate_hz.ok_or_else(|| anyhow!("missing sample_rate"))?;
    let channels = channels.ok_or_else(|| anyhow!("missing channel count"))?;
    if channels == 0 {
        bail!("decoded channel count is zero");
    }

    Ok(DecodedAudio {
        sample_rate_hz,
        channels,
        samples,
    })
}

fn append_decoded(
    decoded: AudioBufferRef<'_>,
    channels: usize,
    out: &mut [Vec<f64>],
) -> anyhow::Result<()> {
    let spec = *decoded.spec();
    let mut sample_buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
    // Symphonia handles integer PCM normalization during conversion.
    sample_buf.copy_interleaved_ref(decoded);
    let interleaved = sample_buf.samples();

    if !interleaved.len().is_multiple_of(channels) {
        bail!(
            "decoded interleaved sample count {} not divisible by channels {}",
            interleaved.len(),
            channels
        );
    }

    for frame in interleaved.chunks_exact(channels) {
        for (ch_idx, sample) in frame.iter().enumerate() {
            out[ch_idx].push(*sample as f64);
        }
    }

    Ok(())
}

fn compute_metrics(
    decoded: &DecodedAudio,
    analyzer_version: &str,
    extension: &str,
) -> anyhow::Result<Metrics> {
    let channel_count = decoded.channels as usize;
    if channel_count == 0 {
        bail!("decoded channel_count is zero");
    }

    let frame_count = decoded
        .samples
        .iter()
        .map(|channel| channel.len())
        .min()
        .unwrap_or(0);

    let duration_seconds = if decoded.sample_rate_hz == 0 {
        0.0
    } else {
        frame_count as f64 / decoded.sample_rate_hz as f64
    };

    let mut sample_peak_linear = 0.0;
    let mut clipping_sample_count = 0u64;
    let mut sum_sq = 0.0;

    for channel in &decoded.samples {
        for &sample in &channel[..frame_count] {
            let abs_sample = sample.abs();
            if abs_sample > sample_peak_linear {
                sample_peak_linear = abs_sample;
            }
            if abs_sample >= 1.0 {
                clipping_sample_count += 1;
            }
            sum_sq += sample * sample;
        }
    }

    let total_samples = (frame_count * channel_count).max(1) as f64;
    let rms_linear = (sum_sq / total_samples).sqrt();
    let sample_peak_dbfs = linear_to_db(sample_peak_linear);
    let rms_dbfs = linear_to_db(rms_linear);
    let crest_factor_db = (sample_peak_dbfs - rms_dbfs).max(0.0);

    let short_term_rms_series_dbfs = short_term_rms_series(
        &decoded.samples,
        frame_count,
        decoded.sample_rate_hz,
        RMS_WINDOW_MS,
        RMS_HOP_MS,
    );

    let approx_true_peak_dbtp =
        linear_to_db(approx_true_peak_linear(&decoded.samples, frame_count));

    let (integrated_lufs, true_peak_dbtp, short_term_lufs_series) =
        compute_lufs_and_true_peak(&decoded.samples, frame_count, decoded.sample_rate_hz)
            .context("compute LUFS/true peak")?;

    let spectral = compute_spectral_metrics(&decoded.samples, frame_count, decoded.sample_rate_hz);
    let stereo = compute_stereo_metrics(&decoded.samples, frame_count);
    let transient_density =
        compute_transient_density(&short_term_rms_series_dbfs, duration_seconds);

    Ok(Metrics {
        analyzer_version: analyzer_version.to_string(),
        sample_rate_hz: decoded.sample_rate_hz,
        channels: decoded.channels,
        frame_count: frame_count as u64,
        duration_seconds,
        sample_peak_linear,
        sample_peak_dbfs,
        clipping_sample_count,
        rms_dbfs,
        crest_factor_db,
        short_term_rms_series_dbfs,
        approx_true_peak_dbtp,
        true_peak_dbtp,
        band_energies_db_rel: spectral.band_energies_db_rel,
        spectral_centroid_hz: spectral.spectral_centroid_hz,
        correlation_min: stereo.correlation_min,
        correlation_mean: stereo.correlation_mean,
        lr_balance_db: stereo.lr_balance_db,
        lossy_source: extension == "mp3",
        integrated_lufs,
        short_term_lufs_series,
        tonal_balance_curve: spectral.tonal_balance_curve,
        transient_density,
    })
}

fn compute_lufs_and_true_peak(
    samples: &[Vec<f64>],
    frame_count: usize,
    sample_rate_hz: u32,
) -> anyhow::Result<(f64, f64, Vec<f64>)> {
    let channels = samples.len();
    let mut integrated_lufs = -70.0;
    let mut true_peak_dbtp = linear_to_db(0.0);

    if frame_count > 0 {
        let mut meter = EbuR128::new(
            channels as u32,
            sample_rate_hz,
            Mode::I | Mode::S | Mode::TRUE_PEAK,
        )
        .map_err(|err| anyhow!("init ebur128 meter: {err}"))?;

        feed_meter_interleaved(&mut meter, samples, frame_count)?;

        if let Ok(value) = meter.loudness_global() {
            integrated_lufs = sanitize_finite(value, -70.0);
        }

        let mut true_peak_linear: f64 = 0.0;
        for ch_idx in 0..channels {
            if let Ok(channel_peak) = meter.true_peak(ch_idx as u32) {
                true_peak_linear = true_peak_linear.max(channel_peak.abs());
            }
        }
        true_peak_dbtp = linear_to_db(sanitize_finite(true_peak_linear, 0.0));
    }

    let short_term_lufs_series = short_term_lufs_series(samples, frame_count, sample_rate_hz)?;

    Ok((integrated_lufs, true_peak_dbtp, short_term_lufs_series))
}

fn feed_meter_interleaved(
    meter: &mut EbuR128,
    samples: &[Vec<f64>],
    frame_count: usize,
) -> anyhow::Result<()> {
    const CHUNK_FRAMES: usize = 4096;

    let mut interleaved = Vec::<f64>::with_capacity(CHUNK_FRAMES * samples.len());

    let mut offset = 0usize;
    while offset < frame_count {
        let frames = (frame_count - offset).min(CHUNK_FRAMES);
        interleaved.clear();

        for frame_idx in 0..frames {
            let source_idx = offset + frame_idx;
            for channel in samples {
                interleaved.push(channel[source_idx]);
            }
        }

        meter
            .add_frames_f64(&interleaved)
            .map_err(|err| anyhow!("ebur128 add_frames_f64: {err}"))?;

        offset += frames;
    }

    Ok(())
}

fn short_term_lufs_series(
    samples: &[Vec<f64>],
    frame_count: usize,
    sample_rate_hz: u32,
) -> anyhow::Result<Vec<f64>> {
    let channels = samples.len();
    let window = ms_to_frames(sample_rate_hz, LUFS_WINDOW_MS);
    let hop = ms_to_frames(sample_rate_hz, LUFS_HOP_MS);
    let mut values = Vec::new();

    let mut interleaved = Vec::<f64>::with_capacity(window * channels);
    let mut start = 0usize;
    let limit = frame_count.max(1);

    while start < limit {
        let mut meter = EbuR128::new(channels as u32, sample_rate_hz, Mode::S)
            .map_err(|err| anyhow!("init short-term LUFS meter: {err}"))?;

        interleaved.clear();
        for frame_idx in 0..window {
            let idx = start + frame_idx;
            for channel in samples {
                let sample = if idx < frame_count { channel[idx] } else { 0.0 };
                interleaved.push(sample);
            }
        }

        meter
            .add_frames_f64(&interleaved)
            .map_err(|err| anyhow!("short-term LUFS add frames: {err}"))?;

        let short_term = meter
            .loudness_shortterm()
            .map(|value| sanitize_finite(value, -70.0))
            .unwrap_or(-70.0);
        values.push(short_term);

        start += hop;
    }

    Ok(values)
}

fn compute_spectral_metrics(
    samples: &[Vec<f64>],
    frame_count: usize,
    sample_rate_hz: u32,
) -> SpectralMetrics {
    let mut planner = FftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);

    let hann: Vec<f64> = (0..FFT_SIZE)
        .map(|idx| {
            let ratio = idx as f64 / (FFT_SIZE as f64 - 1.0);
            0.5 - 0.5 * (2.0 * PI * ratio).cos()
        })
        .collect();

    let mut spectrum = vec![Complex::new(0.0, 0.0); FFT_SIZE];
    let mut total_power = 0.0;
    let mut spectral_centroid_num = 0.0;
    let mut spectral_centroid_den = 0.0;

    let static_bands = [
        (20.0, 60.0),
        (60.0, 150.0),
        (150.0, 500.0),
        (500.0, 2000.0),
        (2000.0, 8000.0),
        (8000.0, 16000.0),
    ];
    let mut static_band_power = [0.0_f64; 6];

    let tonal_bands: Vec<(f64, f64)> = (0..30)
        .map(|i| {
            let center = 20.0 * 2.0_f64.powf(i as f64 / 3.0);
            let low = center / 2.0_f64.powf(1.0 / 6.0);
            let high = center * 2.0_f64.powf(1.0 / 6.0);
            (low, high)
        })
        .collect();
    let mut tonal_power = vec![0.0_f64; tonal_bands.len()];

    let mut start = 0usize;
    let limit = frame_count.max(1);
    let channels = samples.len() as f64;

    while start < limit {
        for (bin_idx, bin) in spectrum.iter_mut().enumerate() {
            let idx = start + bin_idx;
            let mono = if idx < frame_count {
                let mut sum = 0.0;
                for channel in samples {
                    sum += channel[idx];
                }
                sum / channels
            } else {
                0.0
            };
            *bin = Complex::new(mono * hann[bin_idx], 0.0);
        }

        fft.process(&mut spectrum);

        for (bin_idx, value) in spectrum.iter().enumerate().take(FFT_SIZE / 2 + 1) {
            let freq = bin_idx as f64 * sample_rate_hz as f64 / FFT_SIZE as f64;
            let power = value.norm_sqr();

            if (20.0..20000.0).contains(&freq) {
                total_power += power;
                spectral_centroid_num += power * freq;
                spectral_centroid_den += power;
            }

            for (band_idx, (low, high)) in static_bands.iter().enumerate() {
                if *low <= freq && freq < *high {
                    static_band_power[band_idx] += power;
                }
            }

            for (band_idx, (low, high)) in tonal_bands.iter().enumerate() {
                if *low <= freq && freq < *high {
                    tonal_power[band_idx] += power;
                }
            }
        }

        start += FFT_HOP;
    }

    let band_energies_db_rel = BandEnergies {
        hz_20_60: power_ratio_to_db(static_band_power[0], total_power),
        hz_60_150: power_ratio_to_db(static_band_power[1], total_power),
        hz_150_500: power_ratio_to_db(static_band_power[2], total_power),
        hz_500_2000: power_ratio_to_db(static_band_power[3], total_power),
        hz_2000_8000: power_ratio_to_db(static_band_power[4], total_power),
        hz_8000_16000: power_ratio_to_db(static_band_power[5], total_power),
    };

    let tonal_balance_curve = tonal_power
        .iter()
        .map(|band_power| power_ratio_to_db(*band_power, total_power))
        .collect();

    let spectral_centroid_hz = if spectral_centroid_den < EPS {
        0.0
    } else {
        spectral_centroid_num / spectral_centroid_den
    };

    SpectralMetrics {
        band_energies_db_rel,
        spectral_centroid_hz,
        tonal_balance_curve,
    }
}

fn compute_stereo_metrics(samples: &[Vec<f64>], frame_count: usize) -> StereoMetrics {
    if samples.len() < 2 {
        return StereoMetrics {
            correlation_min: None,
            correlation_mean: None,
            lr_balance_db: None,
        };
    }

    let left = &samples[0];
    let right = &samples[1];

    let mut left_energy = 0.0;
    let mut right_energy = 0.0;
    for idx in 0..frame_count {
        left_energy += left[idx] * left[idx];
        right_energy += right[idx] * right[idx];
    }

    let lr_balance_db = Some(10.0 * ((left_energy + EPS) / (right_energy + EPS)).log10());

    let mut corr_values = Vec::new();
    let mut start = 0usize;
    let limit = frame_count.max(1);

    while start < limit {
        let mut mean_l = 0.0;
        let mut mean_r = 0.0;
        for i in 0..STEREO_WINDOW {
            let idx = start + i;
            let l = if idx < frame_count { left[idx] } else { 0.0 };
            let r = if idx < frame_count { right[idx] } else { 0.0 };
            mean_l += l;
            mean_r += r;
        }
        mean_l /= STEREO_WINDOW as f64;
        mean_r /= STEREO_WINDOW as f64;

        let mut num = 0.0;
        let mut var_l = 0.0;
        let mut var_r = 0.0;
        for i in 0..STEREO_WINDOW {
            let idx = start + i;
            let l = if idx < frame_count { left[idx] } else { 0.0 };
            let r = if idx < frame_count { right[idx] } else { 0.0 };
            let dl = l - mean_l;
            let dr = r - mean_r;
            num += dl * dr;
            var_l += dl * dl;
            var_r += dr * dr;
        }

        let denom = var_l * var_r;
        if denom >= EPS {
            let corr = num / denom.sqrt();
            corr_values.push(corr);
        }

        start += STEREO_HOP;
    }

    let (correlation_min, correlation_mean) = if corr_values.is_empty() {
        (None, None)
    } else {
        let mut min_value = f64::INFINITY;
        let mut sum = 0.0;
        for value in &corr_values {
            min_value = min_value.min(*value);
            sum += *value;
        }
        (Some(min_value), Some(sum / corr_values.len() as f64))
    };

    StereoMetrics {
        correlation_min,
        correlation_mean,
        lr_balance_db,
    }
}

fn short_term_rms_series(
    samples: &[Vec<f64>],
    frame_count: usize,
    sample_rate_hz: u32,
    window_ms: u32,
    hop_ms: u32,
) -> Vec<f64> {
    let channels = samples.len();
    let window = ms_to_frames(sample_rate_hz, window_ms);
    let hop = ms_to_frames(sample_rate_hz, hop_ms);

    let mut output = Vec::new();
    let mut start = 0usize;
    let limit = frame_count.max(1);

    while start < limit {
        let mut sum_sq = 0.0;
        for frame_idx in 0..window {
            let idx = start + frame_idx;
            for channel in samples {
                let sample = if idx < frame_count { channel[idx] } else { 0.0 };
                sum_sq += sample * sample;
            }
        }

        let norm = (window * channels).max(1) as f64;
        let rms_linear = (sum_sq / norm).sqrt();
        output.push(linear_to_db(rms_linear));

        start += hop;
    }

    output
}

fn approx_true_peak_linear(samples: &[Vec<f64>], frame_count: usize) -> f64 {
    let mut max_peak: f64 = 0.0;

    for channel in samples {
        if frame_count == 0 {
            continue;
        }

        for idx in 0..frame_count {
            let current = channel[idx];
            max_peak = max_peak.max(current.abs());

            if idx + 1 < frame_count {
                let next = channel[idx + 1];
                let delta = next - current;
                let q1 = current + delta * 0.25;
                let q2 = current + delta * 0.5;
                let q3 = current + delta * 0.75;
                max_peak = max_peak.max(q1.abs()).max(q2.abs()).max(q3.abs());
            }
        }
    }

    max_peak
}

fn compute_transient_density(short_term_rms_series_dbfs: &[f64], duration_seconds: f64) -> f64 {
    // This is a deterministic proxy metric. It can later be replaced by
    // onset-detection (e.g. spectral flux) without changing existing telemetry fields.
    if duration_seconds <= EPS || short_term_rms_series_dbfs.len() < 2 {
        return 0.0;
    }

    let mut transient_count = 0u64;
    for idx in 1..short_term_rms_series_dbfs.len() {
        if short_term_rms_series_dbfs[idx] - short_term_rms_series_dbfs[idx - 1]
            >= TRANSIENT_RMS_JUMP_DB
        {
            transient_count += 1;
        }
    }

    transient_count as f64 / duration_seconds
}

fn linear_to_db(value: f64) -> f64 {
    20.0 * value.abs().max(EPS).log10()
}

fn power_ratio_to_db(numerator: f64, denominator: f64) -> f64 {
    10.0 * ((numerator + EPS) / (denominator + EPS)).log10()
}

fn ms_to_frames(sample_rate_hz: u32, milliseconds: u32) -> usize {
    ((sample_rate_hz as u64 * milliseconds as u64) / 1000)
        .max(1)
        .try_into()
        .unwrap_or(usize::MAX)
}

fn sanitize_finite(value: f64, fallback: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::{analyze_file, BandEnergies, Metrics};
    use serde::{Deserialize, Serialize};
    use std::{f64::consts::PI, io::Write, path::Path};

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    struct NormalizedBandEnergies {
        hz_20_60: f64,
        hz_60_150: f64,
        hz_150_500: f64,
        hz_500_2000: f64,
        hz_2000_8000: f64,
        hz_8000_16000: f64,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    struct NormalizedMetrics {
        analyzer_version: String,
        sample_rate_hz: u32,
        channels: u16,
        frame_count: u64,
        duration_seconds: f64,
        sample_peak_linear: f64,
        sample_peak_dbfs: f64,
        clipping_sample_count: u64,
        rms_dbfs: f64,
        crest_factor_db: f64,
        short_term_rms_series_dbfs: Vec<f64>,
        approx_true_peak_dbtp: f64,
        true_peak_dbtp: f64,
        band_energies_db_rel: NormalizedBandEnergies,
        spectral_centroid_hz: f64,
        correlation_min: Option<f64>,
        correlation_mean: Option<f64>,
        lr_balance_db: Option<f64>,
        lossy_source: bool,
        integrated_lufs: f64,
        short_term_lufs_series: Vec<f64>,
        tonal_balance_curve: Vec<f64>,
        transient_density: f64,
    }

    #[tokio::test]
    async fn wav_sine_metrics_sanity() {
        let td = tempfile::tempdir().expect("tempdir");
        let wav_path = td.path().join("sine.wav");
        let sample_rate = 48_000u32;
        let channels = 2u16;
        let frames = sample_rate as usize;

        write_wav_i16(
            &wav_path,
            sample_rate,
            channels,
            frames,
            |frame_idx, _channel_idx| {
                let t = frame_idx as f64 / sample_rate as f64;
                (2.0 * PI * 440.0 * t).sin() * 0.5
            },
        )
        .expect("write wav");

        let metrics = analyze_file(&wav_path, "TelemetryAnalyzer/1.0.0")
            .await
            .expect("analyze wav");

        assert_eq!(metrics.sample_rate_hz, sample_rate);
        assert_eq!(metrics.channels, channels);
        assert_eq!(metrics.frame_count, frames as u64);
        assert!((metrics.duration_seconds - 1.0).abs() < 1e-6);
        assert!((metrics.sample_peak_linear - 0.5).abs() < 0.03);
        assert!(metrics.rms_dbfs < -7.0 && metrics.rms_dbfs > -12.0);
        assert!(metrics.crest_factor_db >= 0.0);
        assert!(!metrics.short_term_rms_series_dbfs.is_empty());
        assert_eq!(metrics.tonal_balance_curve.len(), 30);
        assert!(metrics.transient_density.is_finite());
        assert!(metrics.transient_density >= 0.0);
        assert!(!metrics.lossy_source);
    }

    #[tokio::test]
    async fn determinism_uses_struct_and_json_equality() {
        let td = tempfile::tempdir().expect("tempdir");
        let wav_path = td.path().join("determinism.wav");
        let sample_rate = 44_100u32;
        let channels = 2u16;
        let frames = sample_rate as usize;

        write_wav_i16(
            &wav_path,
            sample_rate,
            channels,
            frames,
            |frame_idx, channel_idx| {
                let t = frame_idx as f64 / sample_rate as f64;
                let base = (2.0 * PI * 220.0 * t).sin() * 0.4;
                if channel_idx == 0 {
                    base
                } else {
                    base * 0.8
                }
            },
        )
        .expect("write wav");

        let a = analyze_file(&wav_path, "TelemetryAnalyzer/1.0.0")
            .await
            .expect("analyze first");
        let b = analyze_file(&wav_path, "TelemetryAnalyzer/1.0.0")
            .await
            .expect("analyze second");

        let norm_a = normalize_metrics(&a);
        let norm_b = normalize_metrics(&b);

        assert_eq!(norm_a, norm_b);
        let json_a = serde_json::to_string(&norm_a).expect("json a");
        let json_b = serde_json::to_string(&norm_b).expect("json b");
        assert_eq!(json_a, json_b);
    }

    #[tokio::test]
    async fn short_file_stft_padding_is_stable() {
        let td = tempfile::tempdir().expect("tempdir");
        let wav_path = td.path().join("short.wav");
        let sample_rate = 48_000u32;

        write_wav_i16(&wav_path, sample_rate, 2, 256, |idx, _| {
            let t = idx as f64 / sample_rate as f64;
            (2.0 * PI * 1000.0 * t).sin() * 0.2
        })
        .expect("write wav");

        let metrics = analyze_file(&wav_path, "TelemetryAnalyzer/1.0.0")
            .await
            .expect("analyze short wav");

        assert!(metrics.spectral_centroid_hz.is_finite());
        assert!(metrics.band_energies_db_rel.hz_20_60.is_finite());
        assert!(metrics.band_energies_db_rel.hz_60_150.is_finite());
        assert!(metrics.band_energies_db_rel.hz_150_500.is_finite());
    }

    #[tokio::test]
    async fn mono_path_has_none_stereo_fields() {
        let td = tempfile::tempdir().expect("tempdir");
        let wav_path = td.path().join("mono.wav");
        let sample_rate = 48_000u32;

        write_wav_i16(
            &wav_path,
            sample_rate,
            1,
            sample_rate as usize,
            |frame_idx, _| {
                let t = frame_idx as f64 / sample_rate as f64;
                (2.0 * PI * 440.0 * t).sin() * 0.3
            },
        )
        .expect("write wav");

        let metrics = analyze_file(&wav_path, "TelemetryAnalyzer/1.0.0")
            .await
            .expect("analyze mono wav");

        assert_eq!(metrics.correlation_min, None);
        assert_eq!(metrics.correlation_mean, None);
        assert_eq!(metrics.lr_balance_db, None);
    }

    #[tokio::test]
    async fn silence_is_deterministic_and_finite() {
        let td = tempfile::tempdir().expect("tempdir");
        let wav_path = td.path().join("silence.wav");
        let sample_rate = 48_000u32;

        write_wav_i16(
            &wav_path,
            sample_rate,
            2,
            sample_rate as usize,
            |_frame_idx, _| 0.0,
        )
        .expect("write wav");

        let metrics = analyze_file(&wav_path, "TelemetryAnalyzer/1.0.0")
            .await
            .expect("analyze silence wav");

        assert!(metrics.sample_peak_linear.is_finite());
        assert!(metrics.sample_peak_dbfs.is_finite());
        assert!(metrics.rms_dbfs.is_finite());
        assert!(metrics.crest_factor_db.is_finite());
        assert!(metrics.integrated_lufs.is_finite());
        assert!(metrics.true_peak_dbtp.is_finite());
        assert!(metrics.approx_true_peak_dbtp.is_finite());
        assert!(metrics.spectral_centroid_hz.is_finite());
        assert!(metrics.transient_density.is_finite());
        assert!(metrics
            .short_term_rms_series_dbfs
            .iter()
            .all(|value| value.is_finite()));
        assert!(metrics
            .short_term_lufs_series
            .iter()
            .all(|value| value.is_finite()));
        assert!(metrics
            .tonal_balance_curve
            .iter()
            .all(|value| value.is_finite()));
        assert_eq!(metrics.correlation_min, None);
        assert_eq!(metrics.correlation_mean, None);
        assert!(metrics.lr_balance_db.expect("lr balance").is_finite());
    }

    #[tokio::test]
    async fn generated_sweep_covers_spectral_path() {
        let td = tempfile::tempdir().expect("tempdir");
        let wav_path = td.path().join("sweep.wav");
        let sample_rate = 48_000u32;
        let duration_seconds = 5.0;
        let frames = (sample_rate as f64 * duration_seconds) as usize;

        let mut phase = 0.0f64;
        write_wav_i16(
            &wav_path,
            sample_rate,
            2,
            frames,
            |frame_idx, channel_idx| {
                let progress = frame_idx as f64 / frames as f64;
                let frequency = 20.0 * 1000.0_f64.powf(progress);
                phase += (2.0 * PI * frequency) / sample_rate as f64;
                let channel_gain = if channel_idx == 0 { 0.6 } else { 0.5 };
                (phase.sin() * channel_gain).clamp(-1.0, 1.0)
            },
        )
        .expect("write sweep wav");

        let metrics = analyze_file(&wav_path, "TelemetryAnalyzer/1.0.0")
            .await
            .expect("analyze sweep wav");

        assert_eq!(metrics.tonal_balance_curve.len(), 30);
        assert!(metrics.spectral_centroid_hz > 0.0);
        assert!(metrics.transient_density.is_finite());
        assert!(metrics.transient_density >= 0.0);
    }

    fn normalize_metrics(metrics: &Metrics) -> NormalizedMetrics {
        NormalizedMetrics {
            analyzer_version: metrics.analyzer_version.clone(),
            sample_rate_hz: metrics.sample_rate_hz,
            channels: metrics.channels,
            frame_count: metrics.frame_count,
            duration_seconds: round_to(metrics.duration_seconds),
            sample_peak_linear: round_to(metrics.sample_peak_linear),
            sample_peak_dbfs: round_to(metrics.sample_peak_dbfs),
            clipping_sample_count: metrics.clipping_sample_count,
            rms_dbfs: round_to(metrics.rms_dbfs),
            crest_factor_db: round_to(metrics.crest_factor_db),
            short_term_rms_series_dbfs: metrics
                .short_term_rms_series_dbfs
                .iter()
                .map(|v| round_to(*v))
                .collect(),
            approx_true_peak_dbtp: round_to(metrics.approx_true_peak_dbtp),
            true_peak_dbtp: round_to(metrics.true_peak_dbtp),
            band_energies_db_rel: normalize_band_energies(&metrics.band_energies_db_rel),
            spectral_centroid_hz: round_to(metrics.spectral_centroid_hz),
            correlation_min: metrics.correlation_min.map(round_to),
            correlation_mean: metrics.correlation_mean.map(round_to),
            lr_balance_db: metrics.lr_balance_db.map(round_to),
            lossy_source: metrics.lossy_source,
            integrated_lufs: round_to(metrics.integrated_lufs),
            short_term_lufs_series: metrics
                .short_term_lufs_series
                .iter()
                .map(|v| round_to(*v))
                .collect(),
            tonal_balance_curve: metrics
                .tonal_balance_curve
                .iter()
                .map(|v| round_to(*v))
                .collect(),
            transient_density: round_to(metrics.transient_density),
        }
    }

    fn normalize_band_energies(bands: &BandEnergies) -> NormalizedBandEnergies {
        NormalizedBandEnergies {
            hz_20_60: round_to(bands.hz_20_60),
            hz_60_150: round_to(bands.hz_60_150),
            hz_150_500: round_to(bands.hz_150_500),
            hz_500_2000: round_to(bands.hz_500_2000),
            hz_2000_8000: round_to(bands.hz_2000_8000),
            hz_8000_16000: round_to(bands.hz_8000_16000),
        }
    }

    fn round_to(value: f64) -> f64 {
        (value * 1_000_000_000.0).round() / 1_000_000_000.0
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
}
