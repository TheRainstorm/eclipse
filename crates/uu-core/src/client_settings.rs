//! Client preferences are immutable per connection; host permissions and
//! connection-provider configuration never enter these device presets.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

pub const LOCAL_RESOLUTION:usize=4;
pub const CUSTOM_RESOLUTION:usize=5;
pub const COMMON_FRAME_RATES:[u32;3]=[30,60,120];
pub const OTHER_FRAME_RATES:[u32;4]=[24,90,144,165];

/// Observed viewer monitor, in physical pixels and millihertz. Missing values
/// stay unknown; neither DPI scaling nor a window's client size is a mode.
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub struct LocalDisplay {
    pub size:Option<[u32;2]>,
    pub refresh_millihz:Option<u32>,
}
impl LocalDisplay {
    pub fn fps(self)->Option<u32> {
        self.refresh_millihz.map(|hz|hz/1000).filter(|fps|*fps>=5)
    }
}

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

#[derive(Clone,Copy,Debug,Serialize,Deserialize,PartialEq)]
#[serde(default)]
pub struct HudAppearance {
    /// Fraction of the video area's height; the whole HUD stays in the area.
    pub vertical_position:f32,
    pub opacity:f32,
    pub diagnostic_opacity:f32,
}
impl Default for HudAppearance {
    fn default()->Self {Self {vertical_position:1./30.,opacity:0.75,diagnostic_opacity:0.8}}
}
impl HudAppearance {
    fn validate(&self)->Result<()> {
        ensure!([self.vertical_position,self.opacity,self.diagnostic_opacity].iter().all(|v|(0.0..=1.0).contains(v)),
            "{}",uu_i18n::format!("HUD位置和背景不透明度必须在0到1之间"));
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ClientSettings {
    pub fps: u32,
    /// Follow the confirmed remote monitor's refresh rate. `fps` remains the
    /// last valid rate until a catalog arrives; zero is never a wire sentinel.
    pub follow_remote_fps: bool,
    /// Local mode takes precedence over the legacy remote-mode flag.
    pub follow_local_fps:bool,
    pub limit_fps_to_local:bool,
    // Legacy profile field: do not carry an obsolete exclusive-layout request
    // into a new connection after the control has been retired.
    #[serde(skip_serializing, deserialize_with="preserve_display_layout")]
    pub display_mode: crate::screen_control::DisplayMode,
    pub bitrate: usize,
    pub custom_mbps: u32,
    /// Expected total video budget shared by active displays of one device.
    #[serde(default="default_network_budget")]
    pub network_budget_mbps:u32,
    pub resolution: usize,
    pub custom_resolution:[u32;2],
    pub codec: CodecPreference,
    /// None follows the host default; explicit modes affect only this session.
    pub rate_control: Option<crate::native_quality::RateMode>,
    pub volume: u32,
    pub muted: bool,
    pub hud: bool,
    pub hud_appearance: HudAppearance,
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
fn preserve_display_layout<'de, D: serde::Deserializer<'de>>(d: D) -> Result<crate::screen_control::DisplayMode,D::Error> {
    let _ = crate::screen_control::DisplayMode::deserialize(d)?;
    Ok(crate::screen_control::DisplayMode::Preserve)
}
fn default_network_budget()->u32 {100}
impl Default for ClientSettings {
    fn default() -> Self {
        Self { fps:60, follow_remote_fps:true,follow_local_fps:false,limit_fps_to_local:false, display_mode:crate::screen_control::DisplayMode::Preserve,
            bitrate:6,custom_mbps:55,network_budget_mbps:default_network_budget(),resolution:0,custom_resolution:[1920,1080],codec:CodecPreference::Auto,rate_control:None,
            volume:100,muted:false,hud:false,hud_appearance:HudAppearance::default(),hotkeys:crate::hotkeys::Bindings::default(),capture_keyboard:true,
            vsync:true,hdr:true,software_decoder:false,prefer_hevc:false,relative_mouse:false,
            cursor_mode:CursorMode::Remote,clipboard:true,native_clipboard:false,gamepad:true,nested_shortcuts:false }
    }
}
impl ClientSettings {
    pub fn resolved_fps(&self, remote_refresh:Option<u32>, max_fps:u32)->u32 {
        self.resolved_fps_for_display(remote_refresh,LocalDisplay::default(),max_fps)
    }
    pub fn resolved_fps_for_display(&self,remote_refresh:Option<u32>,local:LocalDisplay,max_fps:u32)->u32 {
        let observed=if self.follow_local_fps {local.fps()} else if self.follow_remote_fps {remote_refresh.filter(|fps|*fps>=5)} else {None};
        let fps=observed.unwrap_or(self.fps);
        let fps=if self.limit_fps_to_local {local.fps().map_or(fps,|limit|fps.min(limit))}else{fps};
        fps.clamp(5,max_fps.clamp(5,240))
    }
    pub fn resolved_bounds(&self,local:LocalDisplay)->Option<[u32;2]> {
        match self.resolution {
            1=>Some([1920,1080]),2=>Some([2560,1440]),3=>Some([3840,2160]),
            LOCAL_RESOLUTION=>local.size.filter(|size|size.iter().all(|n|*n>=2)).map(|size|size.map(|n|n.min(8192)/2*2)),
            CUSTOM_RESOLUTION=>Some(self.custom_resolution),_=>None,
        }
    }
    pub fn resolved_request(&self,remote_refresh:Option<u32>,local:LocalDisplay,max_fps:u32,budget_kbps:Option<u32>)->crate::native_quality::Request {
        let mut request=self.with_budget_kbps(budget_kbps);
        request.fps=self.resolved_fps_for_display(remote_refresh,local,max_fps);
        request.bounds=self.resolved_bounds(local);
        request
    }
    pub fn validate(&self) -> Result<()> {
        self.hotkeys.validate()?;
        self.hud_appearance.validate()?;
        ensure!((5..=240).contains(&self.fps) && self.bitrate<=6
            && (1..=1000).contains(&self.custom_mbps) && (1..=1000).contains(&self.network_budget_mbps) && self.resolution<=CUSTOM_RESOLUTION && self.volume<=100,
            "{}", uu_i18n::format!("客户端分辨率、帧率、码率或音量无效"));
        ensure!(self.custom_resolution.iter().all(|n|(2..=8192).contains(n)&&n%2==0),"{}",uu_i18n::format!("自定义分辨率的宽高须为 2–8192 之间的偶数"));
        ensure!(!self.software_decoder || !matches!(self.codec,CodecPreference::Hevc|CodecPreference::Av1|CodecPreference::Pyrowave),
            "{}", uu_i18n::format!("HEVC / AV1 / PyroWave 需要可用的 GPU 解码；请选择自动或 H.264"));
        ensure!(self.codec!=CodecPreference::Pyrowave || self.rate_control!=Some(crate::native_quality::RateMode::Vbr),"{}", uu_i18n::format!("PyroWave 当前只支持 CBR 帧预算，请选择 CBR"));
        Ok(())
    }
    pub fn native_request(&self)->crate::native_quality::Request {
        use uu_proto::media::VideoCodec;
        crate::native_quality::Request { fps:self.fps,
            bounds:self.resolved_bounds(LocalDisplay::default()),
            bitrate_kbps:match self.bitrate {1=>Some(30_000),2=>Some(14_000),3=>Some(8_000),4=>Some(self.custom_mbps*1000),_=>None},
            codec:match self.codec {CodecPreference::Auto=>None,CodecPreference::H264=>Some(VideoCodec::H264),CodecPreference::Hevc=>Some(VideoCodec::Hevc),CodecPreference::Av1=>Some(VideoCodec::Av1),CodecPreference::Pyrowave=>Some(VideoCodec::Pyrowave)} }
    }
    pub fn hevc_preferred(&self) -> bool {
        !self.software_decoder && (self.codec == CodecPreference::Hevc || (self.codec == CodecPreference::Auto && self.prefer_hevc))
    }
    pub fn with_budget_kbps(&self,kbps:Option<u32>)->crate::native_quality::Request {
        let mut request=self.native_request();
        if self.bitrate==6 {request.bitrate_kbps=kbps.map(|kbps|kbps.clamp(64,crate::native_quality::MAX_BITRATE_KBPS));}
        request
    }
}

/// Allocate a device-wide video budget across active displays.
/// Each item is `(weight, fixed_kbps)`. Fixed targets are reserved first;
/// remaining capacity is divided by weight with a small floor per auto view.
pub fn allocate_bitrate_budget(total_kbps:u32,items:&[(u64,Option<u32>)])->Vec<u32> {
    let max=crate::native_quality::MAX_BITRATE_KBPS;
    let fixed:u64=items.iter().filter_map(|(_,fixed)|*fixed).map(|kbps|u64::from(kbps.clamp(64,max))).sum();
    let remaining=u64::from(total_kbps).saturating_sub(fixed);
    let auto:Vec<_>=items.iter().enumerate().filter(|(_,(_,fixed))|fixed.is_none()).map(|(index,(weight,_))|(index,*weight)).collect();
    let mut result=items.iter().map(|(_,fixed)|fixed.map_or(0,|kbps|kbps.clamp(64,max))).collect::<Vec<_>>();
    if auto.is_empty(){return result;}
    let count=auto.len() as u64;
    let floor=(remaining/count).min(64);
    let distributable=remaining.saturating_sub(floor*count);
    let weight_total:u64=auto.iter().map(|(_,weight)|*weight).sum();
    for (index,weight) in auto {
        let share=floor+if weight_total==0{distributable/count}else{distributable.saturating_mul(weight)/weight_total};
        result[index]=u32::try_from(share).unwrap_or(max).clamp(1,max);
    }
    result
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedClientSettings {
    pub id: String,
    pub name: String,
    pub settings: ClientSettings,
}

/// Settings owned by one stable display target within a device profile.
/// `target` is `display:<source>` for physical views or `virtual:<uuid>` for
/// an explicit virtual-display view, never a transient host track number.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayClientSettings {
    pub target: String,
    pub settings: ClientSettings,
}

impl DisplayClientSettings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.target.is_empty()
                && self.target.len() <= 320
                && !self.target.chars().any(char::is_control),
            "{}",
            uu_i18n::format!("显示器客户端配置标识无效")
        );
        self.settings.validate()
    }
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
mod hud_tests {
    use super::*;
    #[test]
    fn legacy_and_partial_hud_settings_keep_defaults_and_custom_values_roundtrip() {
        let legacy:ClientSettings=serde_json::from_str(r#"{"hud":true,"hud_detail":"detailed","fps":120}"#).unwrap();
        assert!(legacy.hud);assert_eq!(legacy.fps,120);
        assert!(serde_json::to_value(&legacy).unwrap().get("hud_detail").is_none());
        assert_eq!(legacy.hud_appearance,HudAppearance::default());
        let partial:ClientSettings=serde_json::from_str(r#"{"hud_appearance":{"opacity":0.2}}"#).unwrap();
        assert_eq!(partial.hud_appearance.opacity,0.2);
        assert_eq!(partial.hud_appearance.vertical_position,HudAppearance::default().vertical_position);
        let mut custom=legacy;custom.hud_appearance=HudAppearance{vertical_position:1.,opacity:0.,diagnostic_opacity:1.};
        custom.validate().unwrap();
        let restored:ClientSettings=serde_json::from_slice(&serde_json::to_vec(&custom).unwrap()).unwrap();
        assert_eq!(restored,custom);
        for value in [-0.01,1.01,f32::NAN,f32::INFINITY] {
            for field in 0..3 {
                let mut invalid=HudAppearance::default();
                match field {0=>invalid.vertical_position=value,1=>invalid.opacity=value,_=>invalid.diagnostic_opacity=value};
                assert!(invalid.validate().is_err());
            }
        }
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
    fn local_monitor_modes_and_caps_preserve_manual_intent_and_handle_unknown_or_fractional_refresh() {
        let mut settings:ClientSettings=serde_json::from_str(r#"{"fps":165,"follow_remote_fps":false}"#).unwrap();
        assert!(!settings.follow_local_fps&&!settings.limit_fps_to_local);
        let monitor=|hz|LocalDisplay{size:Some([2560,1440]),refresh_millihz:Some(hz)};
        settings.limit_fps_to_local=true;
        assert_eq!(settings.resolved_fps_for_display(Some(240),monitor(60_000),240),60);
        assert_eq!(settings.fps,165);
        assert_eq!(settings.resolved_fps_for_display(None,monitor(144_000),240),144);
        assert_eq!(settings.resolved_fps_for_display(None,monitor(59_940),240),59);
        assert_eq!(settings.resolved_fps_for_display(None,LocalDisplay::default(),240),165);
        settings.limit_fps_to_local=false;
        assert_eq!(settings.resolved_fps_for_display(None,monitor(60_000),240),165);
        settings.follow_local_fps=true;
        assert_eq!(settings.resolved_fps_for_display(Some(240),monitor(120_000),240),120);
        assert_eq!(settings.resolved_fps_for_display(Some(240),LocalDisplay::default(),240),165);
        settings.follow_local_fps=false;settings.follow_remote_fps=true;settings.limit_fps_to_local=true;
        assert_eq!(settings.resolved_fps_for_display(Some(240),monitor(144_000),120),120);
        let restored:ClientSettings=serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert_eq!(restored,settings);
    }
    #[test]
    fn local_and_custom_resolution_resolve_to_valid_wire_bounds_without_changing_source_modes() {
        let mut settings=ClientSettings{resolution:LOCAL_RESOLUTION,follow_local_fps:true,..Default::default()};
        let local=LocalDisplay{size:Some([3072,1920]),refresh_millihz:Some(120_000)};
        let request=settings.resolved_request(Some(240),local,240,Some(20_000));
        assert_eq!(request.bounds,Some([3072,1920]));assert_eq!(request.fps,120);assert_eq!(request.bitrate_kbps,Some(20_000));request.validate().unwrap();
        assert!(settings.resolved_request(None,LocalDisplay::default(),240,None).bounds.is_none());
        let odd=LocalDisplay{size:Some([8193,1439]),..local};
        assert_eq!(settings.resolved_bounds(odd),Some([8192,1438]));
        settings.resolution=CUSTOM_RESOLUTION;settings.custom_resolution=[3440,1440];
        settings.validate().unwrap();assert_eq!(settings.resolved_bounds(local),Some([3440,1440]));
        let restored:ClientSettings=serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert_eq!(restored,settings);
        for size in [[0,1080],[1921,1080],[8194,2160],[1920,3]] {settings.custom_resolution=size;assert!(settings.validate().is_err());}
    }
    #[test]
    fn remote_frame_rate_keeps_valid_fallback_and_obeys_protocol_limits() {
        let mut settings=ClientSettings::default();
        assert!(settings.follow_remote_fps,"new profiles follow the confirmed remote refresh by default");
        settings.follow_remote_fps=false;
        settings.fps=120;
        assert_eq!(settings.resolved_fps(Some(165),240),120);
        settings.follow_remote_fps=true;
        for (refresh,max,expected) in [(Some(165),240,165),(Some(240),144,144),(Some(1),240,120),(Some(0),240,120),(None,240,120),(Some(360),240,240)] {
            assert_eq!(settings.resolved_fps(refresh,max),expected);
        }
        let restored:ClientSettings=serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert!(restored.follow_remote_fps);
        restored.native_request().validate().unwrap();
    }
    #[test]
    fn retired_exclusive_layout_is_migrated_without_reissuing_it() {
        let settings:ClientSettings=serde_json::from_str(r#"{"display_mode":"exclusive_virtual"}"#).unwrap();
        assert_eq!(settings.display_mode,crate::screen_control::DisplayMode::Preserve);
        assert!(serde_json::to_value(settings).unwrap().get("display_mode").is_none());
    }
    #[test]
    fn recommended_bitrate_is_default_and_fixed_presets_keep_their_values() {
        let mut s=ClientSettings::default();
        assert_eq!(s.bitrate,6);assert_eq!(s.network_budget_mbps,100);assert!(s.native_request().bitrate_kbps.is_none());
        assert_eq!(s.with_budget_kbps(Some(12_345)).bitrate_kbps,Some(12_345));
        assert_eq!(s.with_budget_kbps(None).bitrate_kbps,None);
        for (index,kbps) in [(1,30_000),(2,14_000),(3,8_000),(4,55_000)] {
            s.bitrate=index;assert_eq!(s.native_request().bitrate_kbps,Some(kbps));
        }
        let legacy:ClientSettings=serde_json::from_str(r#"{"bitrate":0}"#).unwrap();
        assert_eq!(legacy.bitrate,0);assert!(legacy.native_request().bitrate_kbps.is_none());
        let selected:ClientSettings=serde_json::from_str(r#"{"bitrate":5,"codec":"pyrowave"}"#).unwrap();
        selected.validate().unwrap();
        assert_eq!(serde_json::from_slice::<ClientSettings>(&serde_json::to_vec(&selected).unwrap()).unwrap(),selected);
        let missing:ClientSettings=serde_json::from_str(r#"{"fps":60}"#).unwrap();
        assert_eq!(missing.network_budget_mbps,100);
        let mut invalid=ClientSettings::default();invalid.network_budget_mbps=0;assert!(invalid.validate().is_err());
    }
    #[test]
    fn device_budget_reserves_manual_targets_and_allocates_remaining_by_display_weight() {
        let all=allocate_bitrate_budget(100_000,&[(1,None),(2,None)]);
        assert_eq!(all.len(),2);assert!(all[0]<all[1]);assert!(all.iter().sum::<u32>()<=100_000);
        let mixed=allocate_bitrate_budget(100_000,&[(100,Some(30_000)),(100,None),(200,None)]);
        assert_eq!(mixed[0],30_000);assert!(mixed[1]+mixed[2]<=70_000);assert!(mixed[1]<mixed[2]);
        let disabled=allocate_bitrate_budget(8_000,&[(1,None),(1,None),(1,None)]);
        assert!(disabled.iter().all(|kbps|*kbps>=1));assert!(disabled.iter().sum::<u32>()<=8_000);
    }
}
