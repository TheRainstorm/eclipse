//! Client preferences are immutable per connection; host permissions and
//! connection-provider configuration never enter these device presets.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CursorMode {
    #[default]
    #[serde(alias = "local")]
    Both,
    Remote,
}
impl CursorMode {
    pub const ALL: [Self; 2] = [Self::Both, Self::Remote];
    pub fn label(self) -> &'static str { match self { Self::Both => "本地和远端光标", Self::Remote => "仅远端光标" } }
    pub fn local(self) -> bool { self == Self::Both }
    pub fn remote(self) -> bool { true }
    pub fn next(self) -> Self { match self { Self::Both => Self::Remote, Self::Remote => Self::Both } }
}
fn cursor<'de, D: serde::Deserializer<'de>>(d: D) -> Result<CursorMode, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Value { Mode(CursorMode), Legacy(bool) }
    Ok(match Value::deserialize(d)? {
        Value::Mode(mode) => mode,
        Value::Legacy(true) => CursorMode::Remote,
        Value::Legacy(false) => CursorMode::Both,
    })
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodecPreference { #[default] Auto, H264, Hevc, Av1 }
impl CodecPreference {
    pub const ALL: [Self; 4] = [Self::Auto, Self::H264, Self::Hevc, Self::Av1];
    pub fn label(self) -> &'static str { match self { Self::Auto => "自动协商", Self::H264 => "H.264", Self::Hevc => "H.265 / HEVC", Self::Av1 => "AV1" } }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ClientSettings {
    pub fps: u32,
    pub display_mode: crate::screen_control::DisplayMode,
    pub bitrate: usize,
    pub custom_mbps: u32,
    pub resolution: usize,
    pub codec: CodecPreference,
    pub volume: u32,
    pub muted: bool,
    pub hud: bool,
    pub capture_keyboard: bool,
    pub vsync: bool,
    pub software_decoder: bool,
    pub prefer_hevc: bool,
    pub relative_mouse: bool,
    #[serde(alias = "remote_cursor", deserialize_with = "cursor")]
    pub cursor_mode: CursorMode,
    pub clipboard: bool,
    /// Native consent is independent of the compatibility clipboard default.
    pub native_clipboard: bool,
    pub gamepad: bool,
    /// Send Ctrl+Alt client command chords to the remote while excluding F12.
    pub nested_shortcuts: bool,
}
impl Default for ClientSettings {
    fn default() -> Self {
        Self { fps:60, display_mode:crate::screen_control::DisplayMode::Preserve,
            bitrate:0,custom_mbps:55,resolution:0,codec:CodecPreference::Auto,
            volume:100,muted:false,hud:false,capture_keyboard:true,
            vsync:true,software_decoder:false,prefer_hevc:false,relative_mouse:false,
            cursor_mode:CursorMode::Both,clipboard:true,native_clipboard:false,gamepad:true,nested_shortcuts:false }
    }
}
impl ClientSettings {
    pub fn validate(&self) -> Result<()> {
        ensure!((5..=240).contains(&self.fps) && self.bitrate<=4
            && (1..=1000).contains(&self.custom_mbps) && self.resolution<=3 && self.volume<=100,
            "客户端分辨率、帧率、码率或音量无效");
        ensure!(!self.software_decoder || !matches!(self.codec,CodecPreference::Hevc|CodecPreference::Av1),
            "HEVC / AV1 需要可用的硬件解码；请选择自动或 H.264");
        Ok(())
    }
    pub fn native_request(&self)->crate::native_quality::Request {
        use uu_proto::media::VideoCodec;
        crate::native_quality::Request { fps:self.fps,
            bounds:match self.resolution {1=>Some([1920,1080]),2=>Some([2560,1440]),3=>Some([3840,2160]),_=>None},
            bitrate_kbps:match self.bitrate {1=>Some(30_000),2=>Some(14_000),3=>Some(8_000),4=>Some(self.custom_mbps*1000),_=>None},
            codec:match self.codec {CodecPreference::Auto=>None,CodecPreference::H264=>Some(VideoCodec::H264),CodecPreference::Hevc=>Some(VideoCodec::Hevc),CodecPreference::Av1=>Some(VideoCodec::Av1)} }
    }
    pub fn hevc_preferred(&self) -> bool {
        !self.software_decoder && (self.codec == CodecPreference::Hevc || (self.codec == CodecPreference::Auto && self.prefer_hevc))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedClientSettings {
    pub id: String,
    pub name: String,
    pub settings: ClientSettings,
}
impl NamedClientSettings {
    pub fn new(name: impl Into<String>, settings: ClientSettings) -> Self {
        Self { id:uuid::Uuid::new_v4().to_string(),name:name.into(),settings }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(uuid::Uuid::parse_str(&self.id).is_ok(),"客户端配置标识无效");
        ensure!(!self.name.trim().is_empty() && self.name.chars().count()<=64 && !self.name.chars().any(char::is_control),"请输入有效的配置名称（最多64字）");
        self.settings.validate()
    }
}

#[cfg(test)]
mod clipboard_tests {
    use super::*;
    #[test]
    fn native_clipboard_consent_is_independent_and_survives_client_profile_roundtrip() {
        let mut settings:ClientSettings=serde_json::from_str(r#"{"clipboard":true}"#).unwrap();
        assert!(settings.clipboard);assert!(!settings.native_clipboard);
        settings.native_clipboard=true;
        let restored:ClientSettings=serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert!(restored.native_clipboard);assert_eq!(restored,settings);
    }
}
