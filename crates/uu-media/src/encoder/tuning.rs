//! Driver parameters, independent of client-owned geometry and bitrate.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AmdQuality {
    #[default]
    Speed,
    Balanced,
    Quality,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LowPower {
    #[default]
    Auto,
    On,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Tuning {
    pub amd_quality: AmdQuality,
    /// oneVPL TargetUsage: 1 = best quality, 4 = balanced, 7 = fastest.
    pub intel_target_usage: u8,
    pub intel_low_power: LowPower,
    /// Windows hardware MFT QualityVsSpeed: 0 = fastest, 100 = quality.
    /// None retains the best-effort low-latency default on older drivers.
    pub nvidia_quality_vs_speed: Option<u8>,
}

impl Default for Tuning {
    fn default() -> Self {
        Self { amd_quality:AmdQuality::Speed, intel_target_usage:7,
            intel_low_power:LowPower::Auto, nvidia_quality_vs_speed:None }
    }
}

impl Tuning {
    pub fn validate(self) -> Result<(), super::CodecError> {
        if !(1..=7).contains(&self.intel_target_usage)
            || self.nvidia_quality_vs_speed.is_some_and(|v|v>100) {
            return Err(super::error("Intel Target Usage 必须为 1–7，NVIDIA 画质/速度必须为 0–100"));
        }
        Ok(())
    }
    pub(crate) fn custom_intel(self) -> bool {
        self.intel_target_usage!=7 || self.intel_low_power!=LowPower::Auto
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn driver_tuning_rejects_invalid_values_and_unknown_options() {
        assert_eq!(serde_json::from_str::<Tuning>("{}").unwrap(),Tuning::default());
        for usage in [0,8,255] {
            assert!(Tuning{intel_target_usage:usage,..Default::default()}.validate().is_err());
        }
        assert!(Tuning{nvidia_quality_vs_speed:Some(101),..Default::default()}.validate().is_err());
        assert!(serde_json::from_str::<Tuning>(r#"{"nvidia_preset":4}"#).is_err());
    }
}
