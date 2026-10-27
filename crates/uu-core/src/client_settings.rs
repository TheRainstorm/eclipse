//! Client preferences are immutable per connection; host permissions and
//! connection-provider configuration never enter these device presets.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CursorMode {
    #[serde(alias = "local")]
    Both,
    #[default]
    #[serde(alias = "hidden")]
    Remote,
}
impl CursorMode {
    pub const ALL: [Self; 2] = [Self::Remote, Self::Both];
    pub fn label(self) -> &'static str { match self { Self::Both => uu_i18n::text("本地和远端光标"), Self::Remote => uu_i18n::text("仅远端光标") } }
    pub fn local(self) -> bool { self == Self::Both }
    pub fn local_visible(self,remote_available:bool)->bool {self.local()||!remote_available}
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
pub enum CodecPreference { #[default] Auto, H264, Hevc, Av1, Pyrowave }
impl CodecPreference {
    pub const ALL: [Self; 5] = [Self::Auto, Self::H264, Self::Hevc, Self::Av1,Self::Pyrowave];
    pub fn label(self) -> &'static str { match self { Self::Auto => uu_i18n::text("自动协商"), Self::H264 => "H.264", Self::Hevc => "H.265 / HEVC", Self::Av1 => "AV1",Self::Pyrowave=>uu_i18n::text("PyroWave（实验性 / SDR）") } }
}

#[derive(Clone,Copy,Debug,Default,Serialize,Deserialize,PartialEq,Eq)]
#[serde(rename_all="snake_case")]
pub enum HudDetail { #[default] Compact, Detailed }
impl HudDetail {
    pub const ALL:[Self;2]=[Self::Compact,Self::Detailed];
    pub fn label(self)->&'static str {match self {Self::Compact=>uu_i18n::text("精简"),Self::Detailed=>uu_i18n::text("详细")}}
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
    /// None follows the host default; explicit modes affect only this session.
    pub rate_control: Option<crate::native_quality::RateMode>,
    pub volume: u32,
    pub muted: bool,
    pub hud: bool,
    pub hud_detail: HudDetail,
    pub hotkeys: crate::hotkeys::Bindings,
    pub capture_keyboard: bool,
    pub vsync: bool,
    /// Prefer negotiated HDR10 when the current display and decoder support it.
    pub hdr: bool,
    pub software_decoder: bool,
    pub prefer_hevc: bool,
    pub relative_mouse: bool,
    #[serde(alias = "remote_cursor", deserialize_with = "cursor")]
    pub cursor_mode: CursorMode,
    pub clipboard: bool,
    /// Native consent is independent of the compatibility clipboard default.
    pub native_clipboard: bool,
    pub gamepad: bool,
    /// Forward client commands except the configured nested-mode toggle.
    pub nested_shortcuts: bool,
}
impl Default for ClientSettings {
    fn default() -> Self {
        Self { fps:60, display_mode:crate::screen_control::DisplayMode::Preserve,
            bitrate:5,custom_mbps:55,resolution:0,codec:CodecPreference::Auto,rate_control:None,
            volume:100,muted:false,hud:false,hud_detail:HudDetail::Compact,hotkeys:crate::hotkeys::Bindings::default(),capture_keyboard:true,
            vsync:true,hdr:true,software_decoder:false,prefer_hevc:false,relative_mouse:false,
            cursor_mode:CursorMode::Remote,clipboard:true,native_clipboard:false,gamepad:true,nested_shortcuts:false }
    }
}
impl ClientSettings {
    pub fn validate(&self) -> Result<()> {
        self.hotkeys.validate()?;
        ensure!((5..=240).contains(&self.fps) && self.bitrate<=5
            && (1..=1000).contains(&self.custom_mbps) && self.resolution<=3 && self.volume<=100,
            "{}", uu_i18n::format!("客户端分辨率、帧率、码率或音量无效"));
        ensure!(!self.software_decoder || !matches!(self.codec,CodecPreference::Hevc|CodecPreference::Av1|CodecPreference::Pyrowave),
            "{}", uu_i18n::format!("HEVC / AV1 / PyroWave 需要可用的 GPU 解码；请选择自动或 H.264"));
        ensure!(self.codec!=CodecPreference::Pyrowave || self.rate_control!=Some(crate::native_quality::RateMode::Vbr),"{}", uu_i18n::format!("PyroWave 当前只支持 CBR 帧预算，请选择 CBR"));
        Ok(())
    }
    pub fn native_request(&self)->crate::native_quality::Request {
        use uu_proto::media::VideoCodec;
        crate::native_quality::Request { fps:self.fps,
            bounds:match self.resolution {1=>Some([1920,1080]),2=>Some([2560,1440]),3=>Some([3840,2160]),_=>None},
            bitrate_kbps:match self.bitrate {1=>Some(30_000),2=>Some(14_000),3=>Some(8_000),4=>Some(self.custom_mbps*1000),_=>None},
            codec:match self.codec {CodecPreference::Auto=>None,CodecPreference::H264=>Some(VideoCodec::H264),CodecPreference::Hevc=>Some(VideoCodec::Hevc),CodecPreference::Av1=>Some(VideoCodec::Av1),CodecPreference::Pyrowave=>Some(VideoCodec::Pyrowave)} }
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
        ensure!(uuid::Uuid::parse_str(&self.id).is_ok(),"{}", uu_i18n::format!("客户端配置标识无效"));
        ensure!(!self.name.trim().is_empty() && self.name.chars().count()<=64 && !self.name.chars().any(char::is_control),"{}", uu_i18n::format!("请输入有效的配置名称（最多64字）"));
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

#[cfg(test)]
mod bitrate_tests {
    use super::*;
    #[test]
    fn recommended_bitrate_is_default_and_fixed_presets_keep_their_values() {
        let mut s=ClientSettings::default();
        assert_eq!(s.bitrate,5);assert!(s.native_request().bitrate_kbps.is_none());
        for (index,kbps) in [(1,30_000),(2,14_000),(3,8_000),(4,55_000)] {
            s.bitrate=index;assert_eq!(s.native_request().bitrate_kbps,Some(kbps));
        }
        let legacy:ClientSettings=serde_json::from_str(r#"{"bitrate":0}"#).unwrap();
        assert_eq!(legacy.bitrate,0);assert!(legacy.native_request().bitrate_kbps.is_none());
        let selected:ClientSettings=serde_json::from_str(r#"{"bitrate":5,"codec":"pyrowave"}"#).unwrap();
        selected.validate().unwrap();
        assert_eq!(serde_json::from_slice::<ClientSettings>(&serde_json::to_vec(&selected).unwrap()).unwrap(),selected);
    }
}
