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
    /// Maximum periodic GOP in seconds for hardware encoders. Recovery requests
    /// still force a keyframe immediately. None retains each backend's policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hardware_gop_seconds: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amd_input_queue: Option<u8>,
    /// VBAQ on AVC/HEVC, content-adaptive quantization on AV1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amd_adaptive_quantization: Option<bool>,
    /// Reservoir at stream creation, in frames at the negotiated peak bitrate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amd_vbv_frames: Option<u8>,
    /// oneVPL TargetUsage: 1 = best quality, 4 = balanced, 7 = fastest.
    pub intel_target_usage: u8,
    pub intel_low_power: LowPower,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intel_vbv_frames: Option<u8>,
    /// Applies to Intel's Windows MFT in automatic hardware selection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intel_mf_quality_vs_speed: Option<u8>,
    /// Windows hardware MFT QualityVsSpeed: 0 = fastest, 100 = quality.
    /// None retains the best-effort low-latency default on older drivers.
    pub nvidia_quality_vs_speed: Option<u8>,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            amd_quality: AmdQuality::Speed,
            hardware_gop_seconds: None,
            amd_input_queue: None,
            amd_adaptive_quantization: None,
            amd_vbv_frames: None,
            intel_target_usage: 7,
            intel_low_power: LowPower::Auto,
            intel_vbv_frames: None,
            intel_mf_quality_vs_speed: None,
            nvidia_quality_vs_speed: None,
        }
    }
}

impl Tuning {
    pub fn validate(self) -> Result<(), super::CodecError> {
        if !(1..=7).contains(&self.intel_target_usage) {
            return Err(super::error("Intel Target Usage must be 1–7"));
        }
        if [self.nvidia_quality_vs_speed, self.intel_mf_quality_vs_speed]
            .into_iter()
            .flatten()
            .any(|value| value > 100)
        {
            return Err(super::error("MFT quality/speed must be 0–100"));
        }
        if self
            .hardware_gop_seconds
            .is_some_and(|value| !(1..=60).contains(&value))
        {
            return Err(super::error(
                "hardware keyframe interval must be 1–60 seconds",
            ));
        }
        if self
            .amd_input_queue
            .is_some_and(|value| !(1..=16).contains(&value))
        {
            return Err(super::error("AMF input queue capacity must be 1–16"));
        }
        if [self.intel_vbv_frames, self.amd_vbv_frames]
            .into_iter()
            .flatten()
            .any(|value| !(1..=5).contains(&value))
        {
            return Err(super::error("VBV budget must be 1–5 frames"));
        }
        Ok(())
    }
    pub fn custom_intel(self) -> bool {
        self.intel_target_usage != 7
            || self.intel_low_power != LowPower::Auto
            || self.intel_vbv_frames.is_some()
    }
    pub(crate) fn custom_amd(self) -> bool {
        self.amd_quality != AmdQuality::Speed
            || self.amd_input_queue.is_some()
            || self.amd_adaptive_quantization.is_some()
            || self.amd_vbv_frames.is_some()
    }
    pub(crate) fn gop_frames(self, fps: u32) -> Option<u32> {
        self.hardware_gop_seconds
            .map(|seconds| u32::from(seconds) * fps)
    }
    #[cfg(all(windows, feature = "mf-encode"))]
    pub(super) fn mf_quality(self, vendor: Option<u16>) -> Option<u8> {
        match vendor {
            Some(0x8086) => self.intel_mf_quality_vs_speed,
            Some(0x10de) => self.nvidia_quality_vs_speed,
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_profiles_and_automatic_mft_selection_survive_new_controls() {
        let original = serde_json::json!({
            "amd_quality":"speed","intel_target_usage":7,"intel_low_power":"auto",
            "nvidia_quality_vs_speed":null
        });
        assert_eq!(serde_json::to_value(Tuning::default()).unwrap(), original);
        let mut t: Tuning = serde_json::from_value(original).unwrap();
        t.intel_mf_quality_vs_speed = Some(0);
        t.hardware_gop_seconds = Some(60);
        assert_eq!(t.gop_frames(240), Some(14400));
        assert!(!t.custom_intel());
        assert!(!t.custom_amd());
        t.amd_adaptive_quantization = Some(false);
        t.intel_vbv_frames = Some(3);
        assert!(t.custom_amd() && t.custom_intel());
        assert_eq!(
            serde_json::from_value::<Tuning>(serde_json::to_value(t).unwrap()).unwrap(),
            t
        );
    }

    #[test]
    fn configured_driver_ranges_have_closed_endpoints() {
        for (field, min, max) in [
            ("hardware_gop_seconds", 1, 60),
            ("amd_input_queue", 1, 16),
            ("amd_vbv_frames", 1, 5),
            ("intel_vbv_frames", 1, 5),
            ("intel_mf_quality_vs_speed", 0, 100),
        ] {
            for value in [0, min, max, max + 1, 255] {
                let t: Tuning = serde_json::from_value(serde_json::json!({(field):value})).unwrap();
                assert_eq!(
                    t.validate().is_ok(),
                    (min..=max).contains(&value),
                    "{field}={value}"
                );
            }
        }
    }
    #[test]
    fn driver_tuning_rejects_invalid_values_and_unknown_options() {
        assert_eq!(
            serde_json::from_str::<Tuning>("{}").unwrap(),
            Tuning::default()
        );
        for usage in [0, 8, 255] {
            assert!(Tuning {
                intel_target_usage: usage,
                ..Default::default()
            }
            .validate()
            .is_err());
        }
        assert!(Tuning {
            nvidia_quality_vs_speed: Some(101),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(serde_json::from_str::<Tuning>(r#"{"nvidia_preset":4}"#).is_err());
    }
}
