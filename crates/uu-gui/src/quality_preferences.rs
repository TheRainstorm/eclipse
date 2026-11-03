//! Friendly shortcuts over the existing client settings draft. No extra state
//! is persisted: advanced edits are reflected as a custom preference.
use eframe::egui;
use uu_core::client_settings::{ClientSettings, CodecPreference};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Preference {
    Fast,
    Balanced,
    Quality,
}
impl Preference {
    const ALL: [Self; 3] = [Self::Fast, Self::Balanced, Self::Quality];
    fn label(self) -> &'static str {
        uu_i18n::text(match self {
            Self::Fast => "极速流畅",
            Self::Balanced => "平衡",
            Self::Quality => "极致画质",
        })
    }
    fn values(self, max_fps: u32) -> (u32, usize, usize) {
        let (resolution, bitrate) = match self {
            Self::Fast => (1, 3), // At most 1080p, 8 Mbps.
            Self::Balanced => (2, 5), // At most 1440p, recommended bitrate.
            Self::Quality => (0, 5), // Remote resolution, recommended bitrate.
        };
        (60.min(max_fps.clamp(5, 240)), resolution, bitrate)
    }
    fn matches(self, value: &ClientSettings, max_fps: u32) -> bool {
        (value.fps, value.resolution, value.bitrate) == self.values(max_fps)
            && value.codec == CodecPreference::Auto
            && !value.prefer_hevc
    }
    fn apply(self, value: &mut ClientSettings, max_fps: u32) {
        (value.fps, value.resolution, value.bitrate) = self.values(max_fps);
        value.codec = CodecPreference::Auto;
        value.prefer_hevc = false;
    }
}

pub(super) fn show(ui: &mut egui::Ui, value: &mut ClientSettings, max_fps: u32) {
    ui.horizontal_wrapped(|ui| {
        for preference in Preference::ALL {
            if ui.selectable_label(preference.matches(value, max_fps), preference.label()).clicked() {
                preference.apply(value, max_fps);
            }
        }
        if !Preference::ALL.into_iter().any(|p| p.matches(value, max_fps)) {
            ui.weak(uu_i18n::text("自定义"));
        }
    });
    ui.add_space(8.);
    ui.weak(uu_i18n::text("流畅优先节省带宽；平衡使用至多 1440p；极致画质保留远端分辨率。均以 60 帧为目标，编码自动协商。"));
    ui.weak(uu_i18n::text("更改后点击保存。分辨率与带宽需求仍受远端和网络条件影响；高级设置可继续微调。"));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preference_updates_only_quality_draft_and_respects_frame_limit() {
        let original = ClientSettings {
            fps: 144, resolution: 3, bitrate: 4, custom_mbps: 79,
            codec: CodecPreference::Hevc, prefer_hevc: true,
            volume: 37, muted: true, vsync: false, hdr: false,
            ..Default::default()
        };
        for preference in Preference::ALL {
            let mut value = original.clone();
            preference.apply(&mut value, 30);
            assert_eq!(value.fps, 30);
            assert!(preference.matches(&value, 30));
            value.validate().unwrap();
            value.fps = original.fps;
            value.resolution = original.resolution;
            value.bitrate = original.bitrate;
            value.codec = original.codec;
            value.prefer_hevc = original.prefer_hevc;
            assert_eq!(value, original, "preferences must preserve input, presentation and audio");
        }
    }
    #[test]
    fn advanced_changes_show_custom_without_overwriting_saved_values() {
        let mut value = ClientSettings::default();
        Preference::Balanced.apply(&mut value, 240);
        assert!(Preference::Balanced.matches(&value, 240));
        value.fps = 120;
        assert!(!Preference::ALL.into_iter().any(|p| p.matches(&value, 240)));
        assert_eq!(value.fps, 120);
    }
}
