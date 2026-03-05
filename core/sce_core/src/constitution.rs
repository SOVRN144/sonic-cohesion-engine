use crate::analyzer::Metrics;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Constitution {
    pub constitution_version: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub intent: Option<serde_yaml::Value>,
    #[serde(default)]
    pub audio_contract: Option<AudioContract>,
    #[serde(default)]
    pub targets: Option<Targets>,
    #[serde(default)]
    pub priorities: Option<Priorities>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AudioContract {
    #[serde(default)]
    pub allowed_sample_rates_hz: Option<Vec<u32>>,
    #[serde(default)]
    pub allowed_bit_depths: Option<Vec<i64>>,
    #[serde(default)]
    pub max_true_peak_dbtp: Option<f64>,
    #[serde(default)]
    pub clipping_allowed: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Targets {
    #[serde(default)]
    pub loudness: Option<LoudnessTargets>,
    #[serde(default)]
    pub dynamics: Option<DynamicsTargets>,
    #[serde(default)]
    pub spectral_balance: Option<SpectralBalanceTargets>,
    #[serde(default)]
    pub stereo: Option<StereoTargets>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LoudnessTargets {
    #[serde(default)]
    pub integrated_lufs_range: Option<RangeF64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DynamicsTargets {
    #[serde(default)]
    pub crest_factor_db_range: Option<RangeF64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SpectralBalanceTargets {
    #[serde(default)]
    pub bands_db: Option<BTreeMap<String, RangeF64>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StereoTargets {
    #[serde(default)]
    pub correlation_min: Option<f64>,
    #[serde(default)]
    pub lr_balance_db_max_abs: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Priorities {
    #[serde(default)]
    pub weights: Option<BTreeMap<String, f64>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct RangeF64(pub [f64; 2]);

impl RangeF64 {
    pub fn min(&self) -> f64 {
        self.0[0].min(self.0[1])
    }

    pub fn max(&self) -> f64 {
        self.0[0].max(self.0[1])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SpectralBandId {
    Hz20_60,
    Hz60_150,
    Hz150_500,
    Hz500_2000,
    Hz2000_8000,
    Hz8000_16000,
}

impl SpectralBandId {
    pub const ALL: [Self; 6] = [
        Self::Hz20_60,
        Self::Hz60_150,
        Self::Hz150_500,
        Self::Hz500_2000,
        Self::Hz2000_8000,
        Self::Hz8000_16000,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::Hz20_60 => "hz_20_60",
            Self::Hz60_150 => "hz_60_150",
            Self::Hz150_500 => "hz_150_500",
            Self::Hz500_2000 => "hz_500_2000",
            Self::Hz2000_8000 => "hz_2000_8000",
            Self::Hz8000_16000 => "hz_8000_16000",
        }
    }

    pub fn metric_id(self) -> &'static str {
        match self {
            Self::Hz20_60 => "spectral.hz_20_60",
            Self::Hz60_150 => "spectral.hz_60_150",
            Self::Hz150_500 => "spectral.hz_150_500",
            Self::Hz500_2000 => "spectral.hz_500_2000",
            Self::Hz2000_8000 => "spectral.hz_2000_8000",
            Self::Hz8000_16000 => "spectral.hz_8000_16000",
        }
    }

    pub fn from_alias(alias: &str) -> Option<Self> {
        let normalized = alias
            .trim()
            .to_ascii_lowercase()
            .replace(['-', ' ', '/'], "_");

        match normalized.as_str() {
            "hz_20_60" | "sub_20_60" | "low_sub" => Some(Self::Hz20_60),
            "hz_60_150" | "low_60_150" | "sub" => Some(Self::Hz60_150),
            "hz_150_500" | "lowmid_150_500" | "low_mid_150_500" | "low_mid" => {
                Some(Self::Hz150_500)
            }
            "hz_500_2000" | "mid_500_2k" | "mid_500_2000" | "mid" => Some(Self::Hz500_2000),
            "hz_2000_8000" | "high_2k_8k" | "presence" => Some(Self::Hz2000_8000),
            "hz_8000_16000" | "air_8k_16k" | "air" => Some(Self::Hz8000_16000),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SpectralBandTargetsNormalized {
    pub hz_20_60: Option<RangeF64>,
    pub hz_60_150: Option<RangeF64>,
    pub hz_150_500: Option<RangeF64>,
    pub hz_500_2000: Option<RangeF64>,
    pub hz_2000_8000: Option<RangeF64>,
    pub hz_8000_16000: Option<RangeF64>,
}

impl SpectralBandTargetsNormalized {
    pub fn set(&mut self, band: SpectralBandId, range: RangeF64) {
        match band {
            SpectralBandId::Hz20_60 => self.hz_20_60 = Some(range),
            SpectralBandId::Hz60_150 => self.hz_60_150 = Some(range),
            SpectralBandId::Hz150_500 => self.hz_150_500 = Some(range),
            SpectralBandId::Hz500_2000 => self.hz_500_2000 = Some(range),
            SpectralBandId::Hz2000_8000 => self.hz_2000_8000 = Some(range),
            SpectralBandId::Hz8000_16000 => self.hz_8000_16000 = Some(range),
        }
    }

    pub fn get(&self, band: SpectralBandId) -> Option<&RangeF64> {
        match band {
            SpectralBandId::Hz20_60 => self.hz_20_60.as_ref(),
            SpectralBandId::Hz60_150 => self.hz_60_150.as_ref(),
            SpectralBandId::Hz150_500 => self.hz_150_500.as_ref(),
            SpectralBandId::Hz500_2000 => self.hz_500_2000.as_ref(),
            SpectralBandId::Hz2000_8000 => self.hz_2000_8000.as_ref(),
            SpectralBandId::Hz8000_16000 => self.hz_8000_16000.as_ref(),
        }
    }

    pub fn configured_count(&self) -> usize {
        SpectralBandId::ALL
            .iter()
            .filter(|band| self.get(**band).is_some())
            .count()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TargetState {
    pub configured: bool,
    pub applicable: bool,
}

impl TargetState {
    pub fn new(configured: bool, applicable: bool) -> Self {
        Self {
            configured,
            applicable,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpectralBandAvailability {
    pub hz_20_60: TargetState,
    pub hz_60_150: TargetState,
    pub hz_150_500: TargetState,
    pub hz_500_2000: TargetState,
    pub hz_2000_8000: TargetState,
    pub hz_8000_16000: TargetState,
}

impl SpectralBandAvailability {
    pub fn get(&self, band: SpectralBandId) -> TargetState {
        match band {
            SpectralBandId::Hz20_60 => self.hz_20_60,
            SpectralBandId::Hz60_150 => self.hz_60_150,
            SpectralBandId::Hz150_500 => self.hz_150_500,
            SpectralBandId::Hz500_2000 => self.hz_500_2000,
            SpectralBandId::Hz2000_8000 => self.hz_2000_8000,
            SpectralBandId::Hz8000_16000 => self.hz_8000_16000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetAvailability {
    pub sample_rate: TargetState,
    pub bit_depth: TargetState,
    pub true_peak_ceiling: TargetState,
    pub clipping: TargetState,
    pub loudness_integrated_lufs: TargetState,
    pub dynamics_crest_factor: TargetState,
    pub spectral_bands: SpectralBandAvailability,
    pub stereo_correlation: TargetState,
    pub stereo_lr_balance: TargetState,
    pub tonal_curve: TargetState,
    pub transient_proxy: TargetState,
}

impl Constitution {
    pub fn spectral_band_targets(&self) -> SpectralBandTargetsNormalized {
        let mut normalized = SpectralBandTargetsNormalized::default();

        let maybe_bands = self
            .targets
            .as_ref()
            .and_then(|targets| targets.spectral_balance.as_ref())
            .and_then(|spectral| spectral.bands_db.as_ref());

        if let Some(bands) = maybe_bands {
            for (key, value) in bands {
                if let Some(band_id) = SpectralBandId::from_alias(key) {
                    normalized.set(band_id, value.clone());
                }
            }
        }

        normalized
    }

    pub fn priority_weight(&self, key: &str) -> f64 {
        self.priorities
            .as_ref()
            .and_then(|priorities| priorities.weights.as_ref())
            .and_then(|weights| weights.get(key).copied())
            .unwrap_or(1.0)
    }
}

pub fn build_target_availability(
    constitution: &Constitution,
    metrics: &Metrics,
    asset_bit_depth: Option<i64>,
) -> TargetAvailability {
    let audio = constitution.audio_contract.as_ref();
    let targets = constitution.targets.as_ref();
    let spectral_targets = constitution.spectral_band_targets();

    let sample_rate_configured = audio
        .and_then(|a| a.allowed_sample_rates_hz.as_ref())
        .map(|v| !v.is_empty())
        .unwrap_or(false);

    let bit_depth_configured = audio
        .and_then(|a| a.allowed_bit_depths.as_ref())
        .map(|v| !v.is_empty())
        .unwrap_or(false);

    let true_peak_configured = audio.and_then(|a| a.max_true_peak_dbtp).is_some();
    let clipping_configured = audio.and_then(|a| a.clipping_allowed).is_some();

    let loudness_configured = targets
        .and_then(|t| t.loudness.as_ref())
        .and_then(|l| l.integrated_lufs_range.as_ref())
        .is_some();

    let dynamics_configured = targets
        .and_then(|t| t.dynamics.as_ref())
        .and_then(|d| d.crest_factor_db_range.as_ref())
        .is_some();

    let stereo_correlation_configured = targets
        .and_then(|t| t.stereo.as_ref())
        .and_then(|s| s.correlation_min)
        .is_some();

    let stereo_balance_configured = targets
        .and_then(|t| t.stereo.as_ref())
        .and_then(|s| s.lr_balance_db_max_abs)
        .is_some();

    let stereo_correlation_applicable = metrics.channels >= 2 && metrics.correlation_min.is_some();
    let stereo_balance_applicable = metrics.channels >= 2 && metrics.lr_balance_db.is_some();

    TargetAvailability {
        sample_rate: TargetState::new(sample_rate_configured, true),
        bit_depth: TargetState::new(bit_depth_configured, asset_bit_depth.is_some()),
        true_peak_ceiling: TargetState::new(true_peak_configured, true),
        clipping: TargetState::new(clipping_configured, true),
        loudness_integrated_lufs: TargetState::new(loudness_configured, true),
        dynamics_crest_factor: TargetState::new(dynamics_configured, true),
        spectral_bands: SpectralBandAvailability {
            hz_20_60: TargetState::new(spectral_targets.hz_20_60.is_some(), true),
            hz_60_150: TargetState::new(spectral_targets.hz_60_150.is_some(), true),
            hz_150_500: TargetState::new(spectral_targets.hz_150_500.is_some(), true),
            hz_500_2000: TargetState::new(spectral_targets.hz_500_2000.is_some(), true),
            hz_2000_8000: TargetState::new(spectral_targets.hz_2000_8000.is_some(), true),
            hz_8000_16000: TargetState::new(spectral_targets.hz_8000_16000.is_some(), true),
        },
        stereo_correlation: TargetState::new(
            stereo_correlation_configured,
            stereo_correlation_applicable,
        ),
        stereo_lr_balance: TargetState::new(stereo_balance_configured, stereo_balance_applicable),
        tonal_curve: TargetState::new(true, !metrics.tonal_balance_curve.is_empty()),
        transient_proxy: TargetState::new(true, metrics.transient_density.is_finite()),
    }
}

pub fn load_constitution(path: &Path) -> anyhow::Result<Constitution> {
    let text = std::fs::read_to_string(path)?;
    let constitution: Constitution = serde_yaml::from_str(&text)?;
    Ok(constitution)
}

pub fn load_constitution_and_hash(path: &Path) -> anyhow::Result<(Constitution, String)> {
    let bytes = std::fs::read(path)?;
    let hash = crate::util::sha256_bytes(&bytes);
    let constitution: Constitution = serde_yaml::from_slice(&bytes)?;
    Ok((constitution, hash))
}

#[cfg(test)]
mod tests {
    use super::{Constitution, SpectralBandId};

    #[test]
    fn normalizes_band_aliases() {
        let yaml = r#"
constitution_version: "1.0"
targets:
  spectral_balance:
    bands_db:
      low_sub: [-3.0, 3.0]
      sub: [-2.0, 2.0]
      low_mid: [-2.0, 2.0]
      mid: [-2.0, 2.0]
      presence: [-3.0, 1.0]
      air: [-4.0, 0.0]
"#;

        let constitution: Constitution = serde_yaml::from_str(yaml).expect("parse");
        let normalized = constitution.spectral_band_targets();

        assert!(normalized.get(SpectralBandId::Hz20_60).is_some());
        assert!(normalized.get(SpectralBandId::Hz60_150).is_some());
        assert!(normalized.get(SpectralBandId::Hz150_500).is_some());
        assert!(normalized.get(SpectralBandId::Hz500_2000).is_some());
        assert!(normalized.get(SpectralBandId::Hz2000_8000).is_some());
        assert!(normalized.get(SpectralBandId::Hz8000_16000).is_some());
    }
}
