//! User-facing devices, connection routes and shared host settings. Credentials
//! are referenced by private file path; edits never replace the trust database.
use crate::{
    device_trust::{read_private, TrustStore},
    native::{self, SharingConfig},
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
pub use uu_media::encoder::{AmdQuality,LowPower,Tuning as EncoderTuning};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    #[default]
    Auto,
    Hardware,
    Intel,
    Nvidia,
    Amd,
    Software,
}
impl Encoding {
    pub fn native(self) -> native::EncoderPreference {
        match self {
            Self::Auto => native::EncoderPreference::Auto,
            Self::Hardware => native::EncoderPreference::Hardware,
            Self::Intel => native::EncoderPreference::Intel,
            Self::Nvidia => native::EncoderPreference::Nvidia,
            Self::Amd => native::EncoderPreference::Amd,
            Self::Software => native::EncoderPreference::Software,
        }
    }
}

pub use crate::native_quality::RateMode;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sharing {
    pub codec:Option<native::VideoCodec>,
    pub allowed_codecs:Vec<native::VideoCodec>,
    #[serde(skip_serializing)]
    pub max_bitrate_mbps:u32,
    #[serde(default)]
    pub rate_control:RateMode,
    #[serde(skip_serializing)]
    pub peak_bitrate_mbps:u32,
    pub name: String,
    pub monitor: String,
    // Accepted when reading old files; client requests own these values now.
    #[serde(skip_serializing)]
    pub fps: u32,
    #[serde(skip_serializing)]
    pub bitrate_mbps: u32,
    pub encoding: Encoding,
    pub encoder_tuning:uu_media::encoder::Tuning,
    pub control: bool,
    pub audio: bool,
    pub mute_host_audio: bool,
    #[serde(default)]
    pub clipboard: bool,
    pub service_port: u16,
    pub discovery_port: u16,
    pub plugin_services: Vec<String>,
    #[serde(flatten)]
    pub extensions: std::collections::BTreeMap<String,serde_json::Value>,
    pub ip_enabled: bool,
    pub ipv6_enabled: bool,
    // Older files without this preference must not unexpectedly start sharing.
    #[serde(default)]
    pub auto_start: bool,
    /// Kept for older configuration readers. Native display definitions use
    /// the fixed MAX_DISPLAY_LIMIT budget; this value is no longer editable.
    pub virtual_display_limit: u16,
}
impl Default for Sharing {
    fn default() -> Self {
        Self {
            allowed_codecs:vec![native::VideoCodec::H264,native::VideoCodec::Hevc,native::VideoCodec::Av1,native::VideoCodec::Pyrowave],max_bitrate_mbps:100,
            codec:Default::default(),rate_control:RateMode::Vbr,peak_bitrate_mbps:36,
            name: crate::config::local_device_name(),
            monitor: String::new(),
            fps: 60,
            bitrate_mbps: 18,
            encoding: Encoding::Auto,
            encoder_tuning:Default::default(),
            control: true,
            audio: true,
            mute_host_audio: true,
            clipboard: true,
            service_port: crate::native_discovery::SERVICE_PORT,
            discovery_port: crate::native_discovery::DISCOVERY_PORT,
            plugin_services: Vec::new(),
            extensions: Default::default(),
            ip_enabled: true,
            ipv6_enabled: true,
            auto_start: true,
            virtual_display_limit: crate::virtual_display::DEFAULT_DISPLAY_LIMIT,
        }
    }
}
impl Sharing {
    pub fn audio_source(&self)->Option<native::AudioSource> {
        self.audio.then_some(if self.mute_host_audio {native::AudioSource::SystemMuted}else{native::AudioSource::System})
    }
    pub fn native(&self) -> SharingConfig {
        SharingConfig {encoder_tuning:self.encoder_tuning,session:None,video_policy:Some(crate::native_quality::Policy{codecs:self.allowed_codecs.clone(),preferred_codec:self.codec}),codec:self.codec.unwrap_or_default(),rate_control:match self.rate_control {RateMode::Cbr=>uu_media::encoder::RateControl::Cbr,RateMode::Vbr=>uu_media::encoder::RateControl::Vbr{peak_kbps:36_000}},
            source: native::CaptureSource::Desktop {initial_monitor:
                (!self.monitor.is_empty() && self.monitor != crate::native_displays::ALL_MONITORS)
                    .then(||self.monitor.clone())},
            fps: 60,
            bitrate_kbps: 18_000,
            encoder: self.encoding.native(),
            session_limit: None,
            allow_input: self.control,
            audio: self.audio_source(),
            clipboard: None,
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.virtual_display_limit<=crate::virtual_display::MAX_DISPLAY_LIMIT,"virtual display limit exceeds the native catalog budget");
        ensure!(self.plugin_services.len()<=64&&self.extensions.len()<=64,"too many extension settings");
        let mut unique=std::collections::BTreeSet::new();
        for id in &self.plugin_services {
            ensure!(!id.is_empty()&&id.len()<=64&&id.bytes().all(|b|b.is_ascii_lowercase()||b.is_ascii_digit()||b==b'-')&&unique.insert(id),"invalid or duplicate plugin service id");
        }
        ensure!(self.codec.is_none_or(|c|self.allowed_codecs.contains(&c)),"{}", uu_i18n::format!("默认编码须包含在允许的编码中"));
        valid_name(&self.name)?;
        ensure!(
            self.monitor.len() <= 256 && !self.monitor.chars().any(char::is_control),
            "{}", uu_i18n::format!("屏幕名称无效"));
        ensure!(
            self.service_port > 0
                && self.discovery_port > 0,
            "{}", uu_i18n::format!("共享端口无效"));
        self.native().validate()?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    pub id: String,
    pub name: String,
    pub address: String,
    pub token_file: PathBuf,
    pub certificate: Option<PathBuf>,
    #[serde(default)]
    pub host_enabled: bool,
    pub published_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case",deny_unknown_fields)]
pub enum Destination {
    Ip {
        address: String,
        #[serde(default)]
        legacy_device: Option<String>,
    },
    Plugin {
        plugin: String,
        device: String,
    },
    Vps {
        service: String,
        device: String,
    },
    /// Imported legacy configuration is explicitly preserved until upgraded.
    LegacyInternet {
        settings: PathBuf,
    },
    /// Uninstalled extensions retain their original route without interpreting
    /// its schema. Only the owning plugin may migrate it after explicit opt-in.
    #[serde(untagged)]
    Unavailable(serde_json::Value),
}
impl Destination {
    pub fn same_target(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Ip { address: a, .. }, Self::Ip { address: b, .. }) => match (
                crate::native_discovery::endpoint(a),
                crate::native_discovery::endpoint(b),
            ) {
                (Ok(a), Ok(b)) => a.eq_ignore_ascii_case(&b),
                _ => false,
            },
            (Self::Plugin { plugin:a,device:x }, Self::Plugin { plugin:b,device:y }) => a==b&&x==y,
            (Self::Unavailable(a),Self::Unavailable(b))=>a==b,
            (
                Self::Vps {
                    service: a,
                    device: x,
                },
                Self::Vps {
                    service: b,
                    device: y,
                },
            ) => a == b && x == y,
            (Self::LegacyInternet { settings: a }, Self::LegacyInternet { settings: b }) => a == b,
            _ => false,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ip { .. } => uu_i18n::text("IP 直连"),
            Self::Plugin { plugin,.. } => crate::plugins::registry().get(plugin).map(|p|p.descriptor().name).unwrap_or(uu_i18n::text("插件连接服务")),
            Self::Unavailable(_)=>uu_i18n::text("未安装的连接插件"),
            Self::Vps { .. } => "VPS",
            Self::LegacyInternet { .. } => uu_i18n::text("旧版连接服务"),
        }
    }
    pub fn target(&self) -> String {
        match self {
            Self::Ip { address, .. } => address.clone(),
            Self::Plugin { device,.. } | Self::Vps { device, .. } => device.clone(),
            Self::Unavailable(_)=>uu_i18n::text("已保留的插件配置").into(),
            Self::LegacyInternet { .. } => uu_i18n::text("已迁移的旧配置").into(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub id: String,
    pub destination: Destination,
}
impl Route {
    pub fn new(destination: Destination) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            destination,
        }
    }
}

#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    pub id:String,
    pub at:u64,
    #[serde(default)]
    pub pinned:bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub peer: Option<String>,
    pub routes: Vec<Route>,
    pub selected_route: String,
    #[serde(default)]
    pub client_configs: Vec<crate::client_settings::NamedClientSettings>,
    #[serde(default)]
    pub selected_client: Option<String>,
    #[serde(default)]
    pub display_clients: Vec<crate::client_settings::DisplayClientSettings>,
    #[serde(default)]
    pub last_connected:Option<u64>,
    #[serde(default)]
    pub preview:Option<Preview>,
    #[serde(default)]
    pub redirect: Option<String>,
}
impl Profile {
    pub fn new(name: &str, destination: Destination) -> Self {
        let route = Route::new(destination);
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            peer: None,
            selected_route: route.id.clone(),
            client_configs:Vec::new(),selected_client:None,display_clients:Vec::new(),last_connected:None,preview:None,
            routes: vec![route],
            redirect: None,
        }
    }
    pub fn route(&self) -> Result<&Route> {
        self.routes
            .iter()
            .find(|v| v.id == self.selected_route)
            .context(uu_i18n::text("请选择设备连接地址"))
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Book {
    pub version: u32,
    pub sharing: Sharing,
    pub services: Vec<Service>,
    pub devices: Vec<Profile>,
    pub legacy_imported: bool,
}
impl Default for Book {
    fn default() -> Self {
        Self {
            version: 5,
            sharing: Sharing::default(),
            services: Vec::new(),
            devices: Vec::new(),
            legacy_imported: false,
        }
    }
}
fn valid_id(id: &str) -> Result<()> {
    ensure!(uuid::Uuid::parse_str(id).is_ok(), "{}", uu_i18n::format!("配置标识无效"));
    Ok(())
}
fn valid_name(name: &str) -> Result<()> {
    ensure!(
        !name.trim().is_empty()
            && name.chars().count() <= 64
            && !name.chars().any(char::is_control),
        "{}", uu_i18n::format!("请输入有效的名称（最多64字）"));
    Ok(())
}
fn service_device(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 80
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
        "{}", uu_i18n::format!("服务器中的设备名称只能使用字母、数字、短横线或下划线（最多80字节）"));
    Ok(())
}
impl Book {
    pub fn device(&self, id: &str) -> Result<&Profile> {
        let mut current = id;
        for _ in 0..8 {
            let profile = self
                .devices
                .iter()
                .find(|p| p.id == current)
                .context(uu_i18n::text("设备配置已移除"))?;
            match &profile.redirect {
                Some(id) => current = id,
                None => return Ok(profile),
            }
        }
        anyhow::bail!("{}", uu_i18n::format!("设备配置存在循环引用"))
    }
    pub fn service(&self, id: &str) -> Result<&Service> {
        self.services
            .iter()
            .find(|p| p.id == id)
            .context(uu_i18n::text("请先配置连接服务器"))
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 5 && self.devices.len() <= 256 && self.services.len() <= 32,
            "{}", uu_i18n::format!("设备配置版本或数量无效"));
        self.sharing.validate()?;
        let mut ids = std::collections::HashSet::new();
        for service in &self.services {
            valid_id(&service.id)?;
            valid_name(&service.name)?;
            ensure!(
                ids.insert(&service.id)
                    && service.address.len() <= 2048
                    && !service.address.trim().is_empty(),
                "{}", uu_i18n::format!("服务器地址或标识无效"));
            service_device(&service.published_name)?;
            ensure!(
                !service.token_file.as_os_str().is_empty(),
                "{}", uu_i18n::format!("请选择服务器访问凭据文件"));
        }
        let mut ids = std::collections::HashSet::new();
        for profile in &self.devices {
            valid_id(&profile.id)?;
            valid_name(&profile.name)?;
            if let Some(preview)=&profile.preview {valid_id(&preview.id)?;}
            let mut config_ids=std::collections::HashSet::new();
            for config in &profile.client_configs {config.validate()?;ensure!(config_ids.insert(&config.id),"{}", uu_i18n::format!("客户端配置标识重复"));}
            ensure!(profile.selected_client.as_ref().is_none_or(|id|config_ids.contains(id)),"{}", uu_i18n::format!("请选择有效的客户端配置"));
            ensure!(profile.display_clients.len()<=64,"{}",uu_i18n::format!("每台设备最多保存64个显示器客户端配置"));
            let mut display_targets=std::collections::HashSet::new();
            for config in &profile.display_clients {config.validate()?;ensure!(display_targets.insert(&config.target),"{}",uu_i18n::format!("显示器客户端配置标识重复"));}
            ensure!(ids.insert(&profile.id), "{}", uu_i18n::format!("设备配置标识重复"));
            ensure!(
                profile
                    .peer
                    .as_ref()
                    .is_none_or(|id| id.len() == 64 && hex::decode(id).is_ok()),
                "{}", uu_i18n::format!("设备身份指纹无效"));
            if profile.redirect.is_some() {
                self.device(&profile.id)?;
                continue;
            }
            ensure!(
                !profile.routes.is_empty() && profile.routes.len() <= 16,
                "{}", uu_i18n::format!("每台设备需要1至16个连接地址"));
            profile.route()?;
            let mut routes = std::collections::HashSet::new();
            for route in &profile.routes {
                valid_id(&route.id)?;
                ensure!(routes.insert(&route.id), "{}", uu_i18n::format!("连接地址标识重复"));
                match &route.destination {
                    Destination::Ip {
                        address,
                        legacy_device,
                    } => {
                        crate::native_discovery::endpoint(address)?;
                        if let Some(id) = legacy_device {
                            valid_id(id)?;
                        }
                    }
                    Destination::Plugin { plugin,device } => {ensure!(!plugin.is_empty()&&plugin.len()<=64&&plugin.bytes().all(|b|b.is_ascii_lowercase()||b.is_ascii_digit()||b==b'-'),"invalid plugin id");ensure!(
                        !device.trim().is_empty()
                            && device.len() <= 512 && !device.chars().any(char::is_control),
                        "invalid plugin target");},
                    Destination::Unavailable(value)=>ensure!(value["kind"].as_str().is_some_and(|s|!s.is_empty()&&s.len()<=64&&!matches!(s,"ip"|"plugin"|"vps"|"legacy_internet")),"invalid extension route"),
                    Destination::Vps { service, device } => {
                        self.service(service)?;
                        service_device(device)?;
                    }
                    Destination::LegacyInternet { settings } => {
                        ensure!(!settings.as_os_str().is_empty(), "{}", uu_i18n::format!("旧配置路径无效"))
                    }
                }
            }
        }
        Ok(())
    }
}

/// Immutable target snapshot for one stream window. Profile edits cannot
/// redirect an existing window's reconnect to another device.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    pub version: u32,
    pub profile: String,
    pub name: String,
    pub expected_peer: Option<String>,
    pub route: Route,
    pub service: Option<Service>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub client: Option<crate::client_settings::ClientSettings>,
}
impl Launch {
    fn validate(&self) -> Result<()> {
        ensure!(self.version == 2, "{}", uu_i18n::format!("串流配置版本无效"));
        let profile = Profile {
            id: self.profile.clone(),
            name: self.name.clone(),
            peer: self.expected_peer.clone(),
            routes: vec![self.route.clone()],
            selected_route: self.route.id.clone(),
            client_configs:Vec::new(),selected_client:None,display_clients:Vec::new(),last_connected:None,preview:None,
            redirect: None,
        };
        let book = Book {
            devices: vec![profile],
            services: self.service.clone().into_iter().collect(),
            ..Default::default()
        };
        if let Some(client)=&self.client {client.validate()?;}
        book.validate()
    }
    pub fn read(path: &std::path::Path) -> Result<Self> {
        let value: Self = serde_json::from_slice(&read_private(path)?)
            .map_err(|_| anyhow::anyhow!("{}", uu_i18n::format!("串流目标配置格式无效")))?;
        value.validate()?;
        Ok(value)
    }
}

#[derive(Clone)]
pub struct Store {
    directory: PathBuf,
}
// Both authenticated binding and route deduplication retain the canonical
// profile's preferences. Callers establish compatible identities beforehand.
fn merge_profiles(book: &mut Book, current: usize, other: usize, unused: &mut Vec<String>) {
    if book.devices[other].peer.is_none() {
        book.devices[other].peer = book.devices[current].peer.clone();
    }
    let at = book.devices[current].last_connected.take();
    if let Some(at) = at {
        book.devices[other].last_connected =
            Some(book.devices[other].last_connected.unwrap_or(0).max(at));
    }
    if let Some(preview) = book.devices[current].preview.take() {
        if book.devices[other].preview.as_ref().is_none_or(|p| {
            (!p.pinned && preview.pinned)
                || (p.pinned == preview.pinned && !p.pinned && p.at < preview.at)
        }) {
            if let Some(old) = book.devices[other].preview.replace(preview) {
                unused.push(old.id);
            }
        } else {
            unused.push(preview.id);
        }
    }
    let configs = std::mem::take(&mut book.devices[current].client_configs);
    let selection = book.devices[current].selected_client.take();
    for config in configs {
        if !book.devices[other]
            .client_configs
            .iter()
            .any(|c| c.id == config.id)
        {
            book.devices[other].client_configs.push(config);
        }
    }
    if book.devices[other].selected_client.is_none() {
        book.devices[other].selected_client = selection;
    }
    let display_configs = std::mem::take(&mut book.devices[current].display_clients);
    for config in display_configs {
        if !book.devices[other]
            .display_clients
            .iter()
            .any(|c| c.target == config.target)
        {
            book.devices[other].display_clients.push(config);
        }
    }
    let routes = std::mem::take(&mut book.devices[current].routes);
    for route in routes {
        if !book.devices[other]
            .routes
            .iter()
            .any(|r| r.id == route.id || r.destination.same_target(&route.destination))
        {
            book.devices[other].routes.push(route);
        }
    }
    book.devices[current].redirect = Some(book.devices[other].id.clone());
}
impl Store {
    pub fn store_service_credential(&self, id: &str, text: &str) -> Result<PathBuf> {
        valid_id(id)?;
        let text = text.trim();
        ensure!(
            text.len() == 64 && hex::decode(text).is_ok(),
            "{}", uu_i18n::format!("服务器访问令牌应为64位十六进制字符"));
        // Existing stream snapshots retain their own credential version.
        let path = self.directory.join(format!(
            "service-access-{id}-{}.token",
            uuid::Uuid::new_v4()
        ));
        crate::private_file::write_atomic(&path, text.as_bytes())?;
        Ok(path)
    }
    pub fn launch(&self, id: &str) -> Result<Launch> {
        let book = self.read()?;
        let profile = book.device(id)?;
        let route = profile.route()?.clone();
        let service = match &route.destination {
            Destination::Vps { service, .. } => Some(book.service(service)?.clone()),
            _ => None,
        };
        Ok(Launch {
            version: 2,
            profile: profile.id.clone(),
            name: profile.name.clone(),
            expected_peer: profile.peer.clone(),
            route,
            service,
            client:profile.selected_client.as_ref().and_then(|id|profile.client_configs.iter().find(|c|&c.id==id)).map(|c|c.settings.clone()),
        })
    }
    pub fn publish_launch(&self, id: &str) -> Result<PathBuf> {
        let launch = self.launch(id)?;
        self.publish_snapshot(launch)
    }
    pub fn publish_launch_with_defaults(&self,id:&str,defaults:&crate::client_settings::ClientSettings) -> Result<PathBuf> {
        let mut launch=self.launch(id)?;
        if launch.client.is_none() {launch.client=Some(defaults.clone());}
        self.publish_snapshot(launch)
    }
    fn publish_snapshot(&self, launch:Launch) -> Result<PathBuf> {
        launch.validate()?;
        let path = self.directory.join(format!(
            "launch-{}-{}.json",
            launch.profile,
            uuid::Uuid::new_v4()
        ));
        crate::private_file::write_atomic(&path, &serde_json::to_vec(&launch)?)?;
        Ok(path)
    }
    pub fn open(directory: impl Into<PathBuf>) -> Result<Self> {
        let directory = directory.into();
        std::fs::create_dir_all(&directory)?;
        let value = Self { directory };
        value.read()?;
        Ok(value)
    }
    pub fn read(&self) -> Result<Book> {
        let path = self.directory.join("connections.json");
        if !path.try_exists()? {
            return Ok(Book::default());
        }
        let mut value: Book = serde_json::from_slice(&read_private(&path)?)
            .map_err(|_| anyhow::anyhow!("{}", uu_i18n::format!("设备连接配置损坏，原文件已保留")))?;
        if matches!(value.version, 2 | 3) {
            // Move the old default listeners away from Sunshine once. Values
            // saved by current versions, and all custom ports, remain explicit.
            if value.sharing.service_port == 47998 { value.sharing.service_port = crate::native_discovery::SERVICE_PORT; }
            if value.sharing.discovery_port == 47999 { value.sharing.discovery_port = crate::native_discovery::DISCOVERY_PORT; }
            value.version = 4;
        }
        if value.version==4 {value.version=5;}
        for service in &mut value.services {
            for file in std::iter::once(&mut service.token_file).chain(service.certificate.iter_mut()) {
                if file.is_relative() {
                    ensure!(!file.as_os_str().is_empty() && file.components().all(|part| matches!(part, std::path::Component::Normal(_))),
                        "service credential path must remain within its configuration store");
                    *file = self.directory.join(&*file);
                }
            }
        }
        value.validate()?;
        Ok(value)
    }
    pub fn edit(&self, f: impl FnOnce(&mut Book) -> Result<()>) -> Result<()> {
        let _lock = TrustStore::lock_at(&self.directory)?;
        let mut value = self.read()?;
        f(&mut value)?;
        value.validate()?;
        // Managed files move with the store. Callers and launch snapshots see
        // resolved paths; only the persistent record stores relative paths.
        // Explicit files outside the store keep their original location.
        for service in &mut value.services {
            for file in std::iter::once(&mut service.token_file).chain(service.certificate.iter_mut()) {
                if let Ok(relative) = file.strip_prefix(&self.directory) {
                    if !relative.as_os_str().is_empty()
                        && relative.components().all(|part| matches!(part, std::path::Component::Normal(_))) {
                        *file = relative.to_path_buf();
                    }
                }
            }
        }
        let encoded=serde_json::to_vec_pretty(&value)?;
        ensure!(encoded.len()<=1024*1024,"{}", uu_i18n::format!("设备与配置记录总量超过1 MiB，请先移除不再使用的记录"));
        let original=self.directory.join("connections.json");
        if original.exists() {
            let bytes=read_private(&original)?;
            let version=serde_json::from_slice::<serde_json::Value>(&bytes)?["version"].as_u64().unwrap_or(0);
            if matches!(version, 2 | 3 | 4) {
                crate::private_file::write_atomic(&self.directory.join(format!("connections.v{version}-backup-{}.json",uuid::Uuid::new_v4())),&bytes)?;
            }
        }
        crate::private_file::write_atomic(
            &self.directory.join("connections.json"),
            &encoded,
        )?;
        Ok(())
    }
    pub fn sharing(&self, sharing: Sharing) -> Result<()> {
        self.edit(|b| {
            b.sharing = sharing;
            Ok(())
        })
    }
    pub fn save_service(&self, service: Service) -> Result<()> {
        self.edit(|b| {
            if let Some(v) = b.services.iter_mut().find(|v| v.id == service.id) {
                *v = service;
            } else {
                b.services.push(service);
            }
            Ok(())
        })
    }
    pub fn remove_service(&self, id: &str) -> Result<()> {
        self.edit(|book| {
            ensure!(
                !book.devices.iter().any(|p| p.redirect.is_none()
                    && p.routes.iter().any(
                        |r| matches!(&r.destination,Destination::Vps{service,..} if service==id)
                    )),
                "{}", uu_i18n::format!("该服务器仍用于设备连接，请先移除对应的连接地址"));
            book.services.retain(|s| s.id != id);
            Ok(())
        })
    }
    pub fn save_device(&self, profile: Profile) -> Result<()> {
        self.edit(|b| {
            if let Some(v) = b.devices.iter_mut().find(|v| v.id == profile.id) {
                ensure!(v.redirect.is_none() && profile.redirect.is_none(),"{}", uu_i18n::format!("设备已合并，请刷新设备列表后重试"));
                ensure!(
                    v.peer == profile.peer,
                    "{}", uu_i18n::format!("设备身份刚刚更新，请重新打开设备详情后保存"));
                let mut profile=profile;
                profile.client_configs=v.client_configs.clone();
                profile.selected_client=v.selected_client.clone();
                profile.display_clients=v.display_clients.clone();
                profile.last_connected=v.last_connected;profile.preview=v.preview.clone();
                *v = profile;
            } else {
                b.devices.push(profile);
            }
            Ok(())
        })
    }
    /// Add a route without replacing the existing device's name, settings or
    /// identity. Only supply `verified_peer` after authenticating that public
    /// key; directory IDs and matching names are not authentication evidence.
    /// An empty name uses the default label without matching other unnamed hosts.
    pub fn upsert_device(
        &self,
        name: &str,
        mut destination: Destination,
        verified_peer: Option<&str>,
    ) -> Result<String> {
        let explicit_name = !name.trim().is_empty();
        let name = if explicit_name {name.trim()} else {uu_i18n::text("远端设备")};
        valid_name(name)?;
        if let Destination::Ip { address, .. } = &mut destination {
            *address = crate::native_discovery::endpoint(address)?;
        }
        if let Some(peer) = verified_peer {
            ensure!(
                peer.len() == 64 && hex::decode(peer).is_ok(),
                "{}",
                uu_i18n::format!("设备身份指纹无效")
            );
        }
        let mut saved = String::new();
        let mut unused = Vec::new();
        self.edit(|book| {
            let route_matches: Vec<_> = book
                .devices
                .iter()
                .enumerate()
                .filter(|(_, p)| {
                    p.redirect.is_none()
                        && p.routes
                            .iter()
                            .any(|route| route.destination.same_target(&destination))
                })
                .map(|(index, _)| index)
                .collect();
            let known_peers: std::collections::BTreeSet<_> = route_matches
                .iter()
                .filter_map(|&index| book.devices[index].peer.as_deref())
                .chain(verified_peer)
                .collect();
            ensure!(
                known_peers.len() <= 1,
                "{}",
                uu_i18n::format!("设备身份与保存的记录不一致")
            );
            let identity_match = verified_peer.and_then(|peer| {
                book.devices
                    .iter()
                    .position(|p| p.redirect.is_none() && p.peer.as_deref() == Some(peer))
            });
            let known_route = route_matches
                .iter()
                .copied()
                .find(|&index| book.devices[index].peer.is_some());
            // Native hosts normally publish their public-key ID. Use that
            // advertisement only to locate an existing pinned identity, never
            // to create one or replace an authenticated route's expected key.
            let advertised_match = match &destination {
                Destination::Vps { device, .. } if verified_peer.is_none() => book
                    .devices
                    .iter()
                    .position(|p| p.redirect.is_none() && p.peer.as_deref() == Some(device)),
                _ => None,
            };
            let names: Vec<_> = book
                .devices
                .iter()
                .enumerate()
                .filter(|(_, p)| explicit_name && p.redirect.is_none() && p.name.trim().eq_ignore_ascii_case(name))
                .map(|(index, _)| index)
                .collect();
            let name_match = (names.len() == 1).then(|| names[0]).filter(|&index| {
                verified_peer.is_none_or(|peer| {
                    book.devices[index]
                        .peer
                        .as_deref()
                        .is_none_or(|saved| saved == peer)
                })
            });
            let target = identity_match
                .or(known_route)
                .or(advertised_match)
                .or_else(|| route_matches.first().copied())
                .or(name_match)
                .unwrap_or_else(|| {
                    let mut profile = Profile::new(name, destination.clone());
                    profile.peer = verified_peer.map(str::to_owned);
                    book.devices.push(profile);
                    book.devices.len() - 1
                });
            if book.devices[target].peer.is_none() {
                book.devices[target].peer = verified_peer.map(str::to_owned);
            }
            let peer = book.devices[target].peer.clone();
            // A shared target or an already verified public key can collapse
            // old duplicates. Multiple names alone never justify this step.
            let duplicates: Vec<_> = book
                .devices
                .iter()
                .enumerate()
                .filter(|(index, p)| {
                    *index != target
                        && p.redirect.is_none()
                        && (route_matches.contains(index)
                            || peer
                                .as_ref()
                                .is_some_and(|peer| p.peer.as_ref() == Some(peer)))
                })
                .map(|(index, _)| index)
                .collect();
            for source in duplicates {
                ensure!(
                    book.devices[source]
                        .peer
                        .as_ref()
                        .is_none_or(|source| peer.as_ref().is_none_or(|target| source == target)),
                    "{}",
                    uu_i18n::format!("设备身份与保存的记录不一致")
                );
                merge_profiles(book, source, target, &mut unused);
            }
            let profile = &mut book.devices[target];
            if let Some(route) = profile
                .routes
                .iter()
                .find(|route| route.destination.same_target(&destination))
            {
                profile.selected_route = route.id.clone();
            } else {
                let route = Route::new(destination);
                profile.selected_route = route.id.clone();
                profile.routes.push(route);
            }
            saved = profile.id.clone();
            Ok(())
        })?;
        for id in unused {
            let _ = std::fs::remove_file(self.directory.join("previews").join(format!("{id}.jpg")));
        }
        Ok(saved)
    }
    /// Called only after the first decoded image, never when opening a window.
    pub fn record_connected(&self,device:&str,at:u64)->Result<()> {
        self.edit(|book| {
            let id=book.device(device)?.id.clone();
            let profile=book.devices.iter_mut().find(|p|p.id==id).unwrap();
            profile.last_connected=Some(profile.last_connected.unwrap_or(0).max(at));Ok(())
        })
    }
    pub fn record_preview(&self,device:&str,at:u64,jpeg:&[u8])->Result<()> {
        ensure!(!jpeg.is_empty()&&jpeg.len()<=1024*1024,"{}", uu_i18n::format!("设备预览大小无效"));
        let id=uuid::Uuid::new_v4().to_string();
        let path=self.directory.join("previews").join(format!("{id}.jpg"));
        crate::private_file::write_atomic(&path,jpeg)?;
        let mut old=None;let mut retained=false;
        let result=self.edit(|book| {
            let target=book.device(device)?.id.clone();
            let profile=book.devices.iter_mut().find(|p|p.id==target).unwrap();
            if profile.preview.as_ref().is_none_or(|p|!p.pinned && p.at<=at) {
                old=profile.preview.replace(Preview{id:id.clone(),at,pinned:false});retained=true;
            }
            Ok(())
        });
        if result.is_err()||!retained {let _=std::fs::remove_file(path);}
        if result.is_ok() {if let Some(old)=old{let _=std::fs::remove_file(self.directory.join("previews").join(format!("{}.jpg",old.id)));}}
        result
    }
    /// Pin the image the user actually saw. A queued preview writer must not
    /// replace it, and an outdated details window must not pin a different one.
    pub fn pin_preview(&self,device:&str,expected:&str,pinned:bool)->Result<()> {
        self.edit(|book| {
            let target=book.device(device)?.id.clone();
            let profile=book.devices.iter_mut().find(|p|p.id==target).unwrap();
            let preview=profile.preview.as_mut().context(uu_i18n::text("设备还没有截图，请先连接一次"))?;
            ensure!(preview.id==expected,"{}", uu_i18n::format!("设备截图已更新，请重新查看后再固定"));
            preview.pinned=pinned;
            Ok(())
        })
    }
    pub fn preview_bytes(&self,preview:&Preview)->Result<Vec<u8>> {
        valid_id(&preview.id)?;
        Ok(read_private(&self.directory.join("previews").join(format!("{}.jpg",preview.id)))?.to_vec())
    }
    pub fn save_client_config(&self,device:&str,config:crate::client_settings::NamedClientSettings)->Result<()> {
        self.save_config(device,config,false)
    }
    pub fn save_and_select_client_config(&self,device:&str,config:crate::client_settings::NamedClientSettings)->Result<()> {
        self.save_config(device,config,true)
    }
    pub fn display_client_settings(&self,device:&str,target:&str)->Result<Option<crate::client_settings::ClientSettings>> {
        let book=self.read()?;
        let profile=book.device(device)?;
        Ok(profile.display_clients.iter().find(|config|config.target==target).map(|config|config.settings.clone()))
    }
    pub fn save_display_client_settings(&self,device:&str,target:&str,settings:crate::client_settings::ClientSettings)->Result<()> {
        let value=crate::client_settings::DisplayClientSettings{target:target.into(),settings};
        value.validate()?;
        self.edit(|book| {
            let id=book.device(device)?.id.clone();
            let profile=book.devices.iter_mut().find(|p|p.id==id).unwrap();
            if let Some(existing)=profile.display_clients.iter_mut().find(|config|config.target==value.target) {*existing=value;}
            else {profile.display_clients.push(value);}
            Ok(())
        })
    }
    /// The editor explicitly selects an existing ID, or creates a new name.
    /// Resolve that intent under the store lock so a stale editor cannot
    /// overwrite a same-named preset or resurrect a deleted one.
    pub fn save_named_client_config(&self,device:&str,editing:Option<&str>,name:&str,settings:crate::client_settings::ClientSettings)->Result<String> {
        let mut value=crate::client_settings::NamedClientSettings::new(name.trim(),settings);
        if let Some(id)=editing {value.id=id.into();}
        value.validate()?;
        let saved_id=value.id.clone();
        self.edit(|book| {
            let canonical=book.device(device)?.id.clone();
            let profile=book.devices.iter_mut().find(|p|p.id==canonical).unwrap();
            if let Some(id)=editing {
                let existing=profile.client_configs.iter_mut().find(|c|c.id==id)
                    .context(uu_i18n::text("配置已更改或删除，请重新选择后编辑。"))?;
                ensure!(existing.name.trim()==value.name,"{}",uu_i18n::text("配置已更改或删除，请重新选择后编辑。"));
                // Preserve legacy spelling/whitespace and duplicate names;
                // only the explicitly selected stable ID is updated.
                value.name=existing.name.clone();
                *existing=value;
            } else {
                ensure!(!profile.client_configs.iter().any(|c|c.name.trim()==value.name),"{}",uu_i18n::text("已存在同名配置，请从列表选择后编辑。"));
                profile.client_configs.push(value);
            }
            profile.selected_client=Some(saved_id.clone());
            Ok(())
        })?;
        Ok(saved_id)
    }
    fn save_config(&self,device:&str,config:crate::client_settings::NamedClientSettings,select:bool)->Result<()> {
        config.validate()?;
        self.edit(|book| {
            let id=book.device(device)?.id.clone();
            let profile=book.devices.iter_mut().find(|p|p.id==id).unwrap();
            if select {profile.selected_client=Some(config.id.clone());}
            if let Some(existing)=profile.client_configs.iter_mut().find(|c|c.id==config.id) {*existing=config;}
            else {profile.client_configs.push(config);}
            Ok(())
        })
    }
    pub fn select_client_config(&self, device:&str, selected:Option<String>) -> Result<()> {
        self.edit(|book| {
            let id=book.device(device)?.id.clone();
            book.devices.iter_mut().find(|p|p.id==id).unwrap().selected_client=selected;
            Ok(())
        })
    }
    pub fn remove_client_config(&self, device:&str, id:&str) -> Result<()> {
        self.edit(|book| {
            let canonical=book.device(device)?.id.clone();
            let profile=book.devices.iter_mut().find(|p|p.id==canonical).unwrap();
            profile.client_configs.retain(|c|c.id!=id);
            if profile.selected_client.as_deref()==Some(id) {profile.selected_client=None;}
            Ok(())
        })
    }
    pub fn bind(&self, profile: &str, peer: &str) -> Result<()> {
        let mut unused=Vec::new();
        self.edit(|b| {
            let canonical = b.device(profile)?.id.clone();
            let current = b.devices.iter().position(|p| p.id == canonical).unwrap();
            ensure!(
                b.devices[current].peer.as_ref().is_none_or(|v| v == peer),
                "{}", uu_i18n::format!("设备身份与保存的记录不一致"));
            if let Some(other) = b.devices.iter().position(|p| {
                p.id != canonical && p.redirect.is_none() && p.peer.as_deref() == Some(peer)
            }) {
                merge_profiles(b,current,other,&mut unused);
            } else {
                b.devices[current].peer = Some(peer.into());
            }
            Ok(())
        })?;
        for id in unused{let _=std::fs::remove_file(self.directory.join("previews").join(format!("{id}.jpg")));}
        Ok(())
    }
    pub fn forget(&self, id: &str) -> Result<()> {
        self.forget_many(&[id.to_owned()]).map(|_| ())
    }
    pub fn reorder_device(&self, source: &str, target: &str, after: bool) -> Result<()> {
        self.edit(|book| {
            let source = book.device(source)?.id.clone();
            let target = book.device(target)?.id.clone();
            if source == target { return Ok(()); }
            let index = book.devices.iter().position(|p| p.id == source).unwrap();
            let profile = book.devices.remove(index);
            let index = book.devices.iter().position(|p| p.id == target).unwrap();
            book.devices.insert(index + usize::from(after), profile);
            Ok(())
        })
    }
    /// Resolve the complete selection under one lock before removing anything.
    /// Forgetting local records never revokes trust or closes running streams.
    pub fn forget_many(&self, ids: &[String]) -> Result<usize> {
        let mut images=Vec::new();
        let mut removed=0;
        self.edit(|b| {
            let canonical=ids.iter().map(|id|b.device(id).map(|p|p.id.clone()))
                .collect::<Result<std::collections::BTreeSet<_>>>()?;
            removed=canonical.len();
            for profile in b.devices.iter().filter(|p|canonical.contains(&p.id)||p.redirect.as_ref().is_some_and(|id|canonical.contains(id))) {
                if let Some(preview)=&profile.preview {images.push(preview.id.clone());}
            }
            b.devices.retain(|p|!canonical.contains(&p.id)&&!p.redirect.as_ref().is_some_and(|id|canonical.contains(id)));Ok(())
        })?;
        for id in images{let _=std::fs::remove_file(self.directory.join("previews").join(format!("{id}.jpg")));}
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn upsert_fixture() -> (PathBuf, Store) {
        let root=std::env::temp_dir().join(format!("eclipse-device-upsert-{}",uuid::Uuid::new_v4()));
        let store=Store::open(&root).unwrap();
        (root,store)
    }
    fn generated_ip(index:u8) -> Destination {Destination::Ip {address:format!("192.0.2.{index}:48998"),legacy_device:None}}
    #[test]
    fn adding_same_route_or_unique_name_retains_profile_identity_order_and_preferences() {
        let (root,store)=upsert_fixture();
        let first=Profile::new("Another computer",generated_ip(1));
        store.save_device(first.clone()).unwrap();
        let id=store.upsert_device("Generated host",generated_ip(2),None).unwrap();
        let peer="ab".repeat(32);store.bind(&id,&peer).unwrap();
        let preset=crate::client_settings::NamedClientSettings::new("Generated preset",Default::default());
        store.save_client_config(&id,preset.clone()).unwrap();store.select_client_config(&id,Some(preset.id.clone())).unwrap();
        store.record_connected(&id,100).unwrap();
        store.record_preview(&id,100,b"generated-image-bytes").unwrap();
        let before=store.read().unwrap().device(&id).unwrap().clone();
        let added=store.upsert_device(" generated HOST ",generated_ip(3),None).unwrap();
        assert_eq!(id,added);
        assert_eq!(store.upsert_device("New reported name",generated_ip(2),None).unwrap(),id);
        let book=store.read().unwrap();let after=book.device(&id).unwrap();
        assert_eq!(book.devices.iter().map(|p|p.id.as_str()).collect::<Vec<_>>(),vec![first.id.as_str(),id.as_str()]);
        assert_eq!(after.name,before.name);assert_eq!(after.peer,before.peer);
        assert_eq!(after.routes.len(),2);
        assert_eq!(serde_json::to_value(&after.client_configs).unwrap(),serde_json::to_value(&before.client_configs).unwrap());
        assert_eq!(after.selected_client,before.selected_client);
        assert_eq!(after.last_connected,before.last_connected);
        assert_eq!(serde_json::to_value(&after.preview).unwrap(),serde_json::to_value(&before.preview).unwrap());
        assert_eq!(store.launch(&id).unwrap().expected_peer,Some(peer));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn public_directory_id_locates_existing_pin_and_collapses_an_unpaired_duplicate() {
        let (root,store)=upsert_fixture();
        let peer="ac".repeat(32);
        let id=store.upsert_device("Generated host",generated_ip(1),None).unwrap();
        store.bind(&id,&peer).unwrap();
        let service=Service {id:uuid::Uuid::new_v4().to_string(),name:"Generated group".into(),address:"wss://signal.example:9443/v1/signal".into(),
            token_file:"generated.token".into(),certificate:None,host_enabled:false,published_name:"generated-local".into()};
        store.save_service(service.clone()).unwrap();
        let route=Destination::Vps {service:service.id,device:peer.clone()};
        let duplicate=Profile::new("Generated host",route.clone());
        let preset=crate::client_settings::NamedClientSettings::new("Duplicate preset",Default::default());
        store.save_device(duplicate.clone()).unwrap();store.save_client_config(&duplicate.id,preset.clone()).unwrap();store.select_client_config(&duplicate.id,Some(preset.id.clone())).unwrap();
        assert_eq!(store.upsert_device("Generated host",route,None).unwrap(),id);
        let book=store.read().unwrap();
        assert_eq!(book.devices.iter().filter(|p|p.redirect.is_none()).count(),1);
        assert_eq!(book.device(&duplicate.id).unwrap().id,id);
        let canonical=book.device(&id).unwrap();
        assert_eq!(canonical.peer.as_deref(),Some(peer.as_str()));assert_eq!(canonical.routes.len(),2);
        assert_eq!(canonical.client_configs[0].id,preset.id);
        assert_eq!(store.launch(&id).unwrap().expected_peer,Some(peer.clone()));
        // Advertisements locate existing pins; a new advertised ID is unpaired.
        let other_peer="ad".repeat(32);
        let other=store.upsert_device("Other generated host",Destination::Vps {service:book.services[0].id.clone(),device:other_peer},None).unwrap();
        assert!(store.read().unwrap().device(&other).unwrap().peer.is_none());
        let mut custom_service=book.services[0].clone();custom_service.id=uuid::Uuid::new_v4().to_string();store.save_service(custom_service.clone()).unwrap();
        let custom=Destination::Vps {service:custom_service.id,device:peer.clone()};
        let actual_peer="ae".repeat(32);
        let actual=store.upsert_device("Custom publisher",custom.clone(),Some(&actual_peer)).unwrap();
        assert_ne!(actual,id,"a custom advertised name cannot override the authenticated key");
        assert_eq!(store.upsert_device("Custom publisher",custom,None).unwrap(),actual,"an existing authenticated route takes precedence over a directory advertisement");
        assert_eq!(store.launch(&actual).unwrap().expected_peer,Some(actual_peer));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn verified_identity_and_exact_routes_merge_old_records_but_not_conflicting_keys() {
        let (root,store)=upsert_fixture();
        let peer="ba".repeat(32);
        let id=store.upsert_device("First generated name",generated_ip(1),Some(&peer)).unwrap();
        let duplicate=Profile::new("Second generated name",generated_ip(2));
        store.save_device(duplicate.clone()).unwrap();
        assert_eq!(store.upsert_device("Second generated name",generated_ip(2),Some(&peer)).unwrap(),id);
        assert_eq!(store.read().unwrap().device(&duplicate.id).unwrap().id,id);
        let original=std::fs::read(root.join("connections.json")).unwrap();
        assert!(store.upsert_device("Wrong host",generated_ip(1),Some(&"bb".repeat(32))).is_err());
        assert_eq!(std::fs::read(root.join("connections.json")).unwrap(),original);
        let conflict=store.upsert_device("First generated name",generated_ip(3),Some(&"bb".repeat(32))).unwrap();
        assert_ne!(id,conflict,"matching names cannot replace an established identity");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn ambiguous_names_stay_separate_and_exact_duplicate_targets_converge() {
        let (root,store)=upsert_fixture();
        let first=Profile::new("Generated duplicate",generated_ip(1));
        let second=Profile::new("Generated duplicate",generated_ip(2));
        store.save_device(first.clone()).unwrap();store.save_device(second.clone()).unwrap();
        let ambiguous=store.upsert_device("Generated duplicate",generated_ip(3),None).unwrap();
        assert_ne!(ambiguous,first.id);assert_ne!(ambiguous,second.id);
        let duplicate=Profile::new("Saved elsewhere",generated_ip(1));
        store.save_device(duplicate.clone()).unwrap();
        assert_eq!(store.upsert_device("New advertised name",generated_ip(1),None).unwrap(),first.id);
        assert_eq!(store.read().unwrap().device(&duplicate.id).unwrap().id,first.id);
        assert_eq!(store.read().unwrap().devices.iter().filter(|p|p.redirect.is_none()).count(),3);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn concurrent_adds_share_one_canonical_profile_without_losing_routes() {
        let (root,store)=upsert_fixture();
        let barrier=std::sync::Arc::new(std::sync::Barrier::new(2));
        let threads:Vec<_>=(1..=2).map(|index|{let store=store.clone();let barrier=barrier.clone();std::thread::spawn(move||{
            barrier.wait();store.upsert_device("Generated concurrent host",generated_ip(index),None).unwrap()
        })}).collect();
        let ids:Vec<_>=threads.into_iter().map(|thread|thread.join().unwrap()).collect();
        assert_eq!(ids[0],ids[1]);
        let book=store.read().unwrap();assert_eq!(book.devices.len(),1);assert_eq!(book.devices[0].routes.len(),2);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn unnamed_adds_do_not_merge_unrelated_hosts_under_the_default_label() {
        let (root,store)=upsert_fixture();
        let first=store.upsert_device("",generated_ip(1),None).unwrap();
        let second=store.upsert_device("  ",generated_ip(2),None).unwrap();
        assert_ne!(first,second);
        assert_eq!(store.upsert_device("",generated_ip(1),None).unwrap(),first);
        assert_eq!(store.read().unwrap().devices.len(),2);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn device_reorder_and_batch_forget_preserve_fresh_records_and_apply_atomically() {
        let root=std::env::temp_dir().join(format!("eclipse-device-management-{}",uuid::Uuid::new_v4()));
        let store=Store::open(&root).unwrap();
        let profiles:Vec<_>=(1..=3).map(|i|Profile::new(&format!("Device {i}"),Destination::Ip{address:format!("192.0.2.{i}:48998"),legacy_device:None})).collect();
        for profile in &profiles {store.save_device(profile.clone()).unwrap();}
        let mut fresh=profiles[2].clone();fresh.name="Renamed elsewhere".into();store.save_device(fresh).unwrap();
        store.reorder_device(&profiles[0].id,&profiles[2].id,true).unwrap();
        let reopened=Store::open(&root).unwrap();
        let book=reopened.read().unwrap();
        assert_eq!(book.devices.iter().map(|p|p.id.as_str()).collect::<Vec<_>>(),vec![profiles[1].id.as_str(),profiles[2].id.as_str(),profiles[0].id.as_str()]);
        assert_eq!(book.devices[1].name,"Renamed elsewhere");
        let path=root.join("connections.json");let unchanged=std::fs::read(&path).unwrap();
        assert!(store.forget_many(&[profiles[0].id.clone(),uuid::Uuid::new_v4().to_string()]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(),unchanged);
        assert_eq!(store.forget_many(&[profiles[0].id.clone(),profiles[2].id.clone()]).unwrap(),2);
        assert_eq!(reopened.read().unwrap().devices[0].id,profiles[1].id);
        assert_eq!(reopened.read().unwrap().devices.len(),1);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn host_output_mute_defaults_on_for_older_settings_and_opt_out_survives_save() {
        let mut old=serde_json::to_value(Sharing::default()).unwrap();
        old.as_object_mut().unwrap().remove("mute_host_audio");
        let mut sharing:Sharing=serde_json::from_value(old).unwrap();
        assert!(sharing.mute_host_audio);
        assert_eq!(sharing.native().audio,Some(native::AudioSource::SystemMuted));
        sharing.mute_host_audio=false;
        let restored:Sharing=serde_json::from_value(serde_json::to_value(&sharing).unwrap()).unwrap();
        assert!(!restored.mute_host_audio);
        assert_eq!(restored.native().audio,Some(native::AudioSource::System));
        sharing.audio=false;
        assert_eq!(sharing.native().audio,None);
    }
    #[test]
    fn unknown_extension_records_survive_edits_without_becoming_enabled() {
        let root=std::env::temp_dir().join(format!("extension-book-{}",uuid::Uuid::new_v4()));
        let store=Store::open(&root).unwrap();
        let original=serde_json::json!({"kind":"example-addon-route","device":"generated-target","custom":{"revision":2}});
        let destination:Destination=serde_json::from_value(original.clone()).unwrap();
        assert!(matches!(&destination,Destination::Unavailable(_)));
        let profile=Profile::new("Generated extension host",destination);
        store.save_device(profile.clone()).unwrap();
        store.edit(|book|{book.sharing.extensions.insert("example-addon-enabled".into(),serde_json::json!(true));Ok(())}).unwrap();
        let book=store.read().unwrap();
        assert!(book.sharing.plugin_services.is_empty());
        assert_eq!(book.sharing.extensions["example-addon-enabled"],true);
        assert_eq!(serde_json::to_value(&book.device(&profile.id).unwrap().routes[0].destination).unwrap(),original);
        let malformed:Destination=serde_json::from_value(serde_json::json!({"kind":"ip","address":42})).unwrap();
        assert!(store.save_device(Profile::new("Malformed",malformed)).is_err());
        let a=Destination::Plugin{plugin:"example-a".into(),device:"same-device".into()};
        let b=Destination::Plugin{plugin:"example-b".into(),device:"same-device".into()};
        assert!(!a.same_target(&b));
        std::fs::remove_dir_all(root).unwrap();
    }
    use crate::client_settings::{ClientSettings,NamedClientSettings};
    #[test]
    fn v4_encoder_defaults_migrate_without_rewriting_until_save_and_keep_exact_backup() {
        use uu_media::encoder::{AmdQuality,LowPower};
        let root=std::env::temp_dir().join(format!("eclipse-encoder-migration-{}",uuid::Uuid::new_v4()));
        let store=Store::open(&root).unwrap();
        let mut old=serde_json::to_value(Book::default()).unwrap();
        old["version"]=4.into();
        old["sharing"].as_object_mut().unwrap().remove("encoder_tuning");
        old["sharing"]["service_port"]=50123.into();
        let original=serde_json::to_vec(&old).unwrap();
        crate::private_file::write_atomic(&root.join("connections.json"),&original).unwrap();
        let migrated=store.read().unwrap();
        assert_eq!(migrated.version,5);
        assert_eq!(migrated.sharing.encoder_tuning,Default::default());
        assert_eq!(std::fs::read(root.join("connections.json")).unwrap(),original);
        store.edit(|book| {
            book.sharing.encoder_tuning.amd_quality=AmdQuality::Balanced;
            book.sharing.encoder_tuning.intel_target_usage=4;
            book.sharing.encoder_tuning.intel_low_power=LowPower::Off;
            book.sharing.encoder_tuning.nvidia_quality_vs_speed=Some(50);
            Ok(())
        }).unwrap();
        let saved=store.read().unwrap();
        assert_eq!(saved.sharing.service_port,50123);
        assert_eq!(saved.sharing.encoder_tuning,saved.sharing.native().encoder_tuning);
        assert_eq!(saved.sharing.encoder_tuning.amd_quality,AmdQuality::Balanced);
        assert_eq!(saved.sharing.encoder_tuning.intel_target_usage,4);
        assert_eq!(saved.sharing.encoder_tuning.intel_low_power,LowPower::Off);
        assert_eq!(saved.sharing.encoder_tuning.nvidia_quality_vs_speed,Some(50));
        let backups:Vec<_>=std::fs::read_dir(&root).unwrap().flatten().filter(|p|p.file_name().to_string_lossy().starts_with("connections.v4-backup-")).collect();
        assert_eq!(backups.len(),1);
        assert_eq!(std::fs::read(backups[0].path()).unwrap(),original);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn old_default_ports_migrate_once_while_custom_and_remote_addresses_survive() {
        let root = std::env::temp_dir().join(format!("eclipse-port-migration-{}", uuid::Uuid::new_v4()));
        let store = Store::open(&root).unwrap();
        let mut old = Book::default();
        old.version = 3;
        old.sharing.service_port = 47998;
        old.sharing.discovery_port = 47999;
        old.devices.push(Profile::new("Older host", Destination::Ip { address: "192.0.2.10:47998".into(), legacy_device: None }));
        crate::private_file::write_atomic(&root.join("connections.json"), &serde_json::to_vec(&old).unwrap()).unwrap();
        let migrated=store.read().unwrap();
        assert_eq!((migrated.sharing.service_port,migrated.sharing.discovery_port),(48998,48999));
        assert!(matches!(&migrated.devices[0].routes[0].destination, Destination::Ip {address,..} if address.ends_with(":47998")));
        store.edit(|book| {book.sharing.service_port=47998;book.sharing.discovery_port=50123;Ok(())}).unwrap();
        let explicit=store.read().unwrap();
        assert_eq!((explicit.sharing.service_port,explicit.sharing.discovery_port),(47998,50123));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pinned_preview_survives_queued_writes_stale_editors_and_identity_merge() {
        let root=std::env::temp_dir().join(format!("eclipse-pinned-preview-{}",uuid::Uuid::new_v4()));
        let store=Store::open(&root).unwrap();
        let a=Profile::new("First",Destination::Plugin{plugin:"fixture".into(),device:"first".into()});
        let b=Profile::new("Second",Destination::Plugin{plugin:"fixture".into(),device:"second".into()});
        store.save_device(a.clone()).unwrap();store.save_device(b.clone()).unwrap();
        store.record_preview(&b.id,100,b"keep this screenshot").unwrap();
        let image=store.read().unwrap().device(&b.id).unwrap().preview.clone().unwrap();
        store.pin_preview(&b.id,&image.id,true).unwrap();
        store.save_device(b.clone()).unwrap();
        store.record_preview(&b.id,200,b"queued later screenshot").unwrap();
        store.record_preview(&a.id,300,b"newer unpinned screenshot").unwrap();
        let peer="34".repeat(32);store.bind(&a.id,&peer).unwrap();store.bind(&b.id,&peer).unwrap();
        let pinned=store.read().unwrap().device(&b.id).unwrap().preview.clone().unwrap();
        assert!(pinned.pinned);assert_eq!(pinned.id,image.id);
        assert_eq!(store.preview_bytes(&pinned).unwrap(),b"keep this screenshot");
        assert_eq!(std::fs::read_dir(root.join("previews")).unwrap().count(),1);
        store.pin_preview(&b.id,&image.id,false).unwrap();
        store.record_preview(&b.id,400,b"automatic updates resumed").unwrap();
        assert!(store.pin_preview(&b.id,&image.id,true).is_err(),"stale details must not pin an unseen replacement");
        let updated=store.read().unwrap().device(&a.id).unwrap().preview.clone().unwrap();
        assert!(!updated.pinned);assert_eq!(store.preview_bytes(&updated).unwrap(),b"automatic updates resumed");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn successful_history_and_latest_private_preview_follow_verified_identity() {
        let root=std::env::temp_dir().join(format!("eclipse-history-{}",uuid::Uuid::new_v4()));
        let store=Store::open(&root).unwrap();
        let a=Profile::new("First",Destination::Plugin{plugin:"fixture".into(),device:"first".into()});
        let b=Profile::new("Second",Destination::Plugin{plugin:"fixture".into(),device:"second".into()});
        store.save_device(a.clone()).unwrap();store.save_device(b.clone()).unwrap();
        assert!(store.read().unwrap().device(&a.id).unwrap().last_connected.is_none());
        store.record_connected(&a.id,100).unwrap();store.record_connected(&a.id,90).unwrap();
        store.record_preview(&a.id,110,b"bounded generated preview A").unwrap();
        store.record_preview(&a.id,109,b"older preview must not replace A").unwrap();
        store.save_device(a.clone()).unwrap();
        let book=store.read().unwrap();let record=book.device(&a.id).unwrap();assert_eq!(record.last_connected,Some(100));
        assert_eq!(store.preview_bytes(record.preview.as_ref().unwrap()).unwrap(),b"bounded generated preview A");
        store.record_connected(&b.id,200).unwrap();store.record_preview(&b.id,210,b"bounded generated preview B").unwrap();
        let peer="12".repeat(32);store.bind(&a.id,&peer).unwrap();store.bind(&b.id,&peer).unwrap();
        let book=store.read().unwrap();let record=book.device(&b.id).unwrap();assert_eq!(record.id,a.id);assert_eq!(record.last_connected,Some(200));
        assert_eq!(store.preview_bytes(record.preview.as_ref().unwrap()).unwrap(),b"bounded generated preview B");
        assert!(store.preview_bytes(&Preview{id:"../../credentials".into(),at:0,pinned:false}).is_err());
        let preview=record.preview.clone().unwrap();store.forget(&a.id).unwrap();assert!(store.preview_bytes(&preview).is_err());
        assert_eq!(std::fs::read_dir(root.join("previews")).unwrap().count(),0);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn v2_migration_is_read_only_until_save_and_preserves_a_private_exact_backup() {
        let root=std::env::temp_dir().join(format!("eclipse-migrate-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let profile=Profile::new("Generated host",Destination::Plugin{plugin:"fixture".into(),device:"generated-device".into()});
        let mut old=serde_json::to_value(Book{devices:vec![profile.clone()],..Default::default()}).unwrap();
        old["version"]=serde_json::json!(2);
        let device=old["devices"][0].as_object_mut().unwrap();device.remove("client_configs");device.remove("selected_client");device.remove("last_connected");device.remove("preview");
        let original=serde_json::to_vec_pretty(&old).unwrap();
        let path=root.join("connections.json");crate::private_file::write_atomic(&path,&original).unwrap();
        let store=Store::open(&root).unwrap();
        assert_eq!(store.read().unwrap().version,5);
        assert_eq!(std::fs::read(&path).unwrap(),original);
        let config=NamedClientSettings::new("Fast",ClientSettings::default());
        store.save_client_config(&profile.id,config.clone()).unwrap();
        store.select_client_config(&profile.id,Some(config.id)).unwrap();
        let backups:Vec<_>=std::fs::read_dir(&root).unwrap().flatten().filter(|p|p.file_name().to_string_lossy().starts_with("connections.v2-backup-")).collect();
        assert_eq!(backups.len(),1);assert_eq!(std::fs::read(backups[0].path()).unwrap(),original);
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;assert_eq!(std::fs::metadata(backups[0].path()).unwrap().permissions().mode()&0o777,0o600);}
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn named_and_inherited_settings_freeze_at_launch_and_stale_route_edits_keep_presets() {
        let root=std::env::temp_dir().join(format!("eclipse-presets-{}",uuid::Uuid::new_v4()));
        let store=Store::open(&root).unwrap();
        let mut profile=Profile::new("Host",Destination::Plugin{plugin:"fixture".into(),device:"generated-host".into()});
        store.save_device(profile.clone()).unwrap();
        let mut defaults=ClientSettings::default();
        let inherited=store.publish_launch_with_defaults(&profile.id,&defaults).unwrap();defaults.fps=90;
        assert_eq!(Launch::read(&inherited).unwrap().client.unwrap().fps,60);
        let mut named=NamedClientSettings::new("Fast",ClientSettings{fps:120,..Default::default()});
        store.save_client_config(&profile.id,named.clone()).unwrap();store.select_client_config(&profile.id,Some(named.id.clone())).unwrap();
        let frozen=store.publish_launch_with_defaults(&profile.id,&defaults).unwrap();
        named.settings.fps=30;store.save_client_config(&profile.id,named.clone()).unwrap();
        profile.name="Updated host".into();store.save_device(profile.clone()).unwrap();
        assert_eq!(store.read().unwrap().device(&profile.id).unwrap().client_configs[0].settings.fps,30);
        assert_eq!(Launch::read(&frozen).unwrap().client.unwrap().fps,120);
        store.remove_client_config(&profile.id,&named.id).unwrap();
        assert!(store.read().unwrap().device(&profile.id).unwrap().selected_client.is_none());
        assert_eq!(Launch::read(&store.publish_launch_with_defaults(&profile.id,&defaults).unwrap()).unwrap().client.unwrap().fps,90);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn named_editor_intents_keep_ids_fresh_records_and_active_launches_isolated() {
        let root=std::env::temp_dir().join(format!("eclipse-named-editor-{}",uuid::Uuid::new_v4()));
        let store=Store::open(&root).unwrap();
        let other_process=Store::open(&root).unwrap();
        let host=Profile::new("Host",Destination::Plugin{plugin:"fixture".into(),device:"host".into()});
        let other=Profile::new("Other",Destination::Plugin{plugin:"fixture".into(),device:"other".into()});
        store.save_device(host.clone()).unwrap();store.save_device(other.clone()).unwrap();
        let other_preset=NamedClientSettings::new("Office",ClientSettings{fps:90,..Default::default()});
        store.save_and_select_client_config(&other.id,other_preset).unwrap();
        let original=serde_json::to_value(store.read().unwrap()).unwrap();
        let first=store.save_named_client_config(&host.id,None," Office ",ClientSettings{fps:120,..Default::default()}).unwrap();
        let frozen=store.publish_launch_with_defaults(&host.id,&ClientSettings::default()).unwrap();
        let second=other_process.save_named_client_config(&host.id,None,"Travel",ClientSettings{fps:30,..Default::default()}).unwrap();
        let unchanged=std::fs::read(root.join("connections.json")).unwrap();
        assert!(store.save_named_client_config(&host.id,None," Travel ",ClientSettings::default()).is_err(),"a stale name list must not overwrite another process's preset");
        assert_eq!(std::fs::read(root.join("connections.json")).unwrap(),unchanged);
        assert_eq!(store.save_named_client_config(&host.id,Some(&first),"Office",ClientSettings{fps:60,..Default::default()}).unwrap(),first);
        let book=store.read().unwrap();let profile=book.device(&host.id).unwrap();
        assert_eq!(profile.client_configs.len(),2);
        assert_eq!(profile.client_configs.iter().find(|c|c.id==first).unwrap().name,"Office");
        assert_eq!(profile.client_configs.iter().find(|c|c.id==second).unwrap().settings.fps,30);
        assert_eq!(serde_json::to_value(&profile.routes).unwrap(),serde_json::to_value(&host.routes).unwrap());
        assert_eq!(serde_json::to_value(book.device(&other.id).unwrap()).unwrap(),original["devices"][1]);
        assert_eq!(serde_json::to_value(&book.sharing).unwrap(),original["sharing"]);
        assert_eq!(Launch::read(&frozen).unwrap().client.unwrap().fps,120);
        other_process.remove_client_config(&host.id,&first).unwrap();
        assert!(store.read().unwrap().device(&host.id).unwrap().selected_client.is_none());
        let unchanged=std::fs::read(root.join("connections.json")).unwrap();
        assert!(store.save_named_client_config(&host.id,Some(&first),"Office",ClientSettings::default()).is_err(),"saving a removed selected ID must not resurrect it");
        assert_eq!(std::fs::read(root.join("connections.json")).unwrap(),unchanged);
        assert_eq!(Launch::read(&frozen).unwrap().client.unwrap().fps,120);
        assert_eq!(Launch::read(&store.publish_launch_with_defaults(&host.id,&ClientSettings{fps:75,..Default::default()}).unwrap()).unwrap().client.unwrap().fps,75);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn named_editor_preserves_legacy_duplicate_names_and_rejects_stale_renames() {
        let root=std::env::temp_dir().join(format!("eclipse-named-duplicates-{}",uuid::Uuid::new_v4()));
        let store=Store::open(&root).unwrap();
        let host=Profile::new("Host",Destination::Plugin{plugin:"fixture".into(),device:"host".into()});
        store.save_device(host.clone()).unwrap();
        let first=NamedClientSettings::new(" Same ",ClientSettings{fps:90,..Default::default()});
        let mut second=NamedClientSettings::new("Same",ClientSettings{fps:120,..Default::default()});
        store.save_client_config(&host.id,first.clone()).unwrap();store.save_client_config(&host.id,second.clone()).unwrap();
        store.save_named_client_config(&host.id,Some(&second.id)," Same ",ClientSettings{fps:30,..Default::default()}).unwrap();
        let book=store.read().unwrap();let profile=book.device(&host.id).unwrap();
        assert_eq!(profile.client_configs.len(),2);
        assert_eq!(serde_json::to_value(profile.client_configs.iter().find(|v|v.id==first.id).unwrap()).unwrap(),serde_json::to_value(&first).unwrap());
        assert_eq!(profile.client_configs.iter().find(|v|v.id==second.id).unwrap().settings.fps,30);
        assert!(store.save_named_client_config(&host.id,None,"Same",ClientSettings::default()).is_err());
        second.name="Renamed elsewhere".into();store.save_client_config(&host.id,second.clone()).unwrap();
        let unchanged=std::fs::read(root.join("connections.json")).unwrap();
        assert!(store.save_named_client_config(&host.id,Some(&second.id),"Same",ClientSettings::default()).is_err());
        assert_eq!(std::fs::read(root.join("connections.json")).unwrap(),unchanged);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn verified_identity_merges_routes_and_presets_without_merging_similar_names() {
        let root=std::env::temp_dir().join(format!("eclipse-preset-merge-{}",uuid::Uuid::new_v4()));let store=Store::open(&root).unwrap();
        let a=Profile::new("Same name",Destination::Plugin{plugin:"fixture".into(),device:"generated-a".into()});
        let b=Profile::new("Same name",Destination::Plugin{plugin:"fixture".into(),device:"generated-b".into()});
        store.save_device(a.clone()).unwrap();store.save_device(b.clone()).unwrap();
        let ca=NamedClientSettings::new("Office",ClientSettings::default());
        let cb=NamedClientSettings::new("Gaming",ClientSettings{fps:120,..Default::default()});
        store.save_client_config(&a.id,ca).unwrap();store.save_client_config(&b.id,cb.clone()).unwrap();store.select_client_config(&b.id,Some(cb.id.clone())).unwrap();
        assert_ne!(store.read().unwrap().device(&a.id).unwrap().id,store.read().unwrap().device(&b.id).unwrap().id);
        let peer="a".repeat(64);store.bind(&a.id,&peer).unwrap();store.bind(&b.id,&peer).unwrap();
        let book=store.read().unwrap();let merged=book.device(&b.id).unwrap();assert_eq!(merged.id,a.id);assert_eq!(merged.client_configs.len(),2);assert_eq!(merged.selected_client.as_deref(),Some(cb.id.as_str()));
        let saved=std::fs::read(root.join("connections.json")).unwrap();
        assert!(store.save_device(b.clone()).is_err(),"a stale route editor must not resurrect a merged device");
        assert_eq!(std::fs::read(root.join("connections.json")).unwrap(),saved);
        assert_eq!(store.read().unwrap().device(&b.id).unwrap().id,a.id);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn shared_codec_and_rate_policy_survives_storage_and_rejects_invalid_profiles(){
        let initial=Sharing::default();initial.validate().unwrap();
        assert_eq!(initial.rate_control,RateMode::Vbr);
        assert!(initial.clipboard);
        assert!(initial.ipv6_enabled);
        let legacy:Sharing=serde_json::from_value(serde_json::json!({"name":"generated"})).unwrap();
        assert_eq!(legacy.codec,None);assert_eq!(legacy.rate_control,RateMode::Cbr);
        assert!(!legacy.clipboard,"a missing legacy field must not grant clipboard access");
        assert!(legacy.ipv6_enabled,"older configurations retain dual-stack IP sharing by default");
        assert_eq!(legacy.native().video_policy.unwrap().preferred_codec,None);
        let explicit:Sharing=serde_json::from_value(serde_json::json!({"name":"generated","codec":"h264"})).unwrap();
        assert_eq!(explicit.codec,Some(native::VideoCodec::H264));
        let mut sharing=legacy;sharing.codec=Some(native::VideoCodec::Av1);sharing.rate_control=RateMode::Vbr;sharing.bitrate_mbps=20;sharing.peak_bitrate_mbps=35;sharing.ipv6_enabled=false;
        let restored:Sharing=serde_json::from_value(serde_json::to_value(&sharing).unwrap()).unwrap();restored.validate().unwrap();
        assert!(!restored.ipv6_enabled,"an explicitly disabled IPv6 setting must survive storage");
        assert_eq!(restored.native().codec,native::VideoCodec::Av1);assert_eq!(restored.native().rate_control,uu_media::encoder::RateControl::Vbr{peak_kbps:36000});
        sharing.peak_bitrate_mbps=19;sharing.bitrate_mbps=u32::MAX;sharing.fps=1;sharing.max_bitrate_mbps=1;
        sharing.validate().unwrap();
        let wire=serde_json::to_value(&sharing).unwrap();
        for key in ["fps","bitrate_mbps","max_bitrate_mbps","peak_bitrate_mbps"] {assert!(wire.get(key).is_none());}
        assert_eq!(sharing.native().fps,60,"legacy host rate must not cap client requests");
        sharing.encoding=Encoding::Software;assert!(sharing.validate().is_err());
    }
    #[test]
    fn profiles_keep_distinct_routes_and_service_credentials_and_bind_only_verified_identity() {
        let root = std::env::temp_dir().join(format!("eclipse-book-{}", uuid::Uuid::new_v4()));
        let store = Store::open(&root).unwrap();
        let service = Service {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Server".into(),
            address: "signal.example:8443".into(),
            token_file: root.join("server.token"),
            certificate: None,
            host_enabled: false,
            published_name: "desktop".into(),
        };
        store.save_service(service.clone()).unwrap();
        let first = store
            .store_service_credential(&service.id, &"ab".repeat(32))
            .unwrap();
        let next = store
            .store_service_credential(&service.id, &"cd".repeat(32))
            .unwrap();
        assert_ne!(first, next);
        assert_eq!(
            std::fs::read_to_string(first).unwrap(),
            "ab".repeat(32),
            "editing a service cannot replace an existing launch credential"
        );
        assert!(Destination::Ip {
            address: "192.0.2.1".into(),
            legacy_device: None
        }
        .same_target(&Destination::Ip {
            address: "192.0.2.1:48998".into(),
            legacy_device: Some(uuid::Uuid::new_v4().to_string())
        }));
        let a = Profile::new(
            "A",
            Destination::Plugin {plugin:"fixture".into(),
                device: "generated-a".into(),
            },
        );
        let b = Profile::new(
            "B",
            Destination::Vps {
                service: service.id.clone(),
                device: "generated-b".into(),
            },
        );
        store.save_device(a.clone()).unwrap();
        store.save_device(b.clone()).unwrap();
        let snapshot = store.publish_launch(&a.id).unwrap();
        store
            .edit(|book| {
                let profile = book.devices.iter_mut().find(|p| p.id == a.id).unwrap();
                profile.routes[0].destination = Destination::Plugin {plugin:"fixture".into(),
                    device: "new-destination".into(),
                };
                Ok(())
            })
            .unwrap();
        let frozen = Launch::read(&snapshot).unwrap();
        assert!(
            matches!(frozen.route.destination,Destination::Plugin{plugin,device} if plugin=="fixture"&&device=="generated-a")
        );
        let id = "a".repeat(64);
        store.bind(&a.id, &id).unwrap();
        assert!(store.bind(&a.id, &"b".repeat(64)).is_err());
        store.bind(&b.id, &id).unwrap();
        let book = store.read().unwrap();
        assert_eq!(book.device(&b.id).unwrap().id, a.id);
        assert_eq!(book.device(&a.id).unwrap().routes.len(), 2);
        assert_eq!(book.services[0].token_file, service.token_file);
        assert!(
            store.remove_service(&service.id).is_err(),
            "a service referenced by a saved route cannot be removed"
        );
        assert!(
            store.save_device(a).is_err(),
            "stale UI must not erase a freshly verified peer identity"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn per_display_client_settings_roundtrip_without_overwriting_selected_profile() {
        let root=std::env::temp_dir().join(format!("eclipse-display-settings-{}",uuid::Uuid::new_v4()));
        let store=Store::open(&root).unwrap();
        let profile=Profile::new("Generated",Destination::Plugin{plugin:"fixture".into(),device:"generated".into()});
        store.save_device(profile.clone()).unwrap();
        let selected=NamedClientSettings::new("Selected",ClientSettings::default());
        store.save_and_select_client_config(&profile.id,selected.clone()).unwrap();
        let display=ClientSettings{fps:165,follow_remote_fps:true,bitrate:4,custom_mbps:42,..Default::default()};
        store.save_display_client_settings(&profile.id,"display:Generated source",display.clone()).unwrap();
        assert_eq!(store.display_client_settings(&profile.id,"display:Generated source").unwrap(),Some(display.clone()));
        let saved=store.read().unwrap().device(&profile.id).unwrap().clone();
        assert_eq!(saved.selected_client.as_deref(),Some(selected.id.as_str()));
        assert_eq!(saved.client_configs[0].settings,selected.settings);
        let mut stale=saved.clone();stale.display_clients.clear();
        store.save_device(stale).unwrap();
        assert_eq!(store.display_client_settings(&profile.id,"display:Generated source").unwrap(),Some(display));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn moving_a_store_keeps_managed_credentials_and_launches_in_the_new_directory() {
        let root = std::env::temp_dir().join(format!("eclipse-move-profile-{}", uuid::Uuid::new_v4()));
        let original = root.join("old/device-trust");
        let moved = root.join("new/device-trust");
        let store = Store::open(&original).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let token = store.store_service_credential(&id, &"ab".repeat(32)).unwrap();
        let external = root.join("external.pem");
        let service = Service { id:id.clone(), name:"Generated".into(), address:"signal.example:8443".into(),
            token_file:token.clone(), certificate:Some(external.clone()), host_enabled:false, published_name:"Generated".into() };
        store.save_service(service).unwrap();
        let profile = Profile::new("Generated", Destination::Vps { service:id, device:"a".repeat(64) });
        store.save_device(profile.clone()).unwrap();
        let persisted: serde_json::Value = serde_json::from_slice(&std::fs::read(original.join("connections.json")).unwrap()).unwrap();
        assert_eq!(persisted["services"][0]["token_file"], token.file_name().unwrap().to_str().unwrap());
        std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
        std::fs::rename(&original, &moved).unwrap();
        let relocated = Store::open(&moved).unwrap();
        let launch = relocated.launch(&profile.id).unwrap();
        let service = launch.service.unwrap();
        assert_eq!(service.token_file.parent(), Some(moved.as_path()));
        assert_eq!(std::fs::read_to_string(&service.token_file).unwrap(), "ab".repeat(32));
        assert_eq!(service.certificate, Some(external), "user-selected external files must not be redirected");
        assert_eq!(Launch::read(&relocated.publish_launch(&profile.id).unwrap()).unwrap().service.unwrap().token_file, service.token_file);
        let mut invalid = persisted;
        for path in ["", "../outside.token"] {
            invalid["services"][0]["token_file"] = path.into();
            std::fs::write(moved.join("connections.json"), serde_json::to_vec(&invalid).unwrap()).unwrap();
            assert!(relocated.read().is_err());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
