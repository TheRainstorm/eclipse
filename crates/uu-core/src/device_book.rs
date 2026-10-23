//! User-facing devices, connection routes and shared host settings. Credentials
//! are referenced by private file path; edits never replace the trust database.
use crate::{
    device_trust::{read_private, TrustStore},
    native::{self, SharingConfig},
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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

#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum RateMode {#[default] Cbr,Vbr}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sharing {
    pub codec:Option<native::VideoCodec>,
    pub allowed_codecs:Vec<native::VideoCodec>,
    #[serde(skip_serializing)]
    pub max_bitrate_mbps:u32,
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
    pub control: bool,
    pub audio: bool,
    pub clipboard: bool,
    pub service_port: u16,
    pub discovery_port: u16,
    pub uu_enabled: bool,
    pub ip_enabled: bool,
    pub auto_start: bool,
}
impl Default for Sharing {
    fn default() -> Self {
        Self {
            allowed_codecs:vec![native::VideoCodec::H264,native::VideoCodec::Hevc,native::VideoCodec::Av1],max_bitrate_mbps:100,
            codec:Default::default(),rate_control:Default::default(),peak_bitrate_mbps:36,
            name: crate::config::local_device_name(),
            monitor: String::new(),
            fps: 60,
            bitrate_mbps: 18,
            encoding: Encoding::Auto,
            control: true,
            audio: true,
            clipboard: false,
            service_port: crate::native_discovery::SERVICE_PORT,
            discovery_port: crate::native_discovery::DISCOVERY_PORT,
            uu_enabled: false,
            ip_enabled: true,
            auto_start: false,
        }
    }
}
impl Sharing {
    pub fn native(&self) -> SharingConfig {
        SharingConfig {session:None,video_policy:Some(crate::native_quality::Policy{codecs:self.allowed_codecs.clone(),preferred_codec:self.codec}),codec:self.codec.unwrap_or_default(),rate_control:match self.rate_control {RateMode::Cbr=>uu_media::encoder::RateControl::Cbr,RateMode::Vbr=>uu_media::encoder::RateControl::Vbr{peak_kbps:36_000}},
            source: if self.monitor == crate::native_displays::ALL_MONITORS {
                native::CaptureSource::Desktop
            } else if self.monitor.is_empty() {
                native::CaptureSource::Primary
            } else {
                native::CaptureSource::Monitor(self.monitor.clone())
            },
            fps: 60,
            bitrate_kbps: 18_000,
            encoder: self.encoding.native(),
            session_limit: None,
            allow_input: self.control,
            audio: self.audio.then_some(native::AudioSource::System),
            clipboard: None,
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.codec.is_none_or(|c|self.allowed_codecs.contains(&c)),"默认编码须包含在允许的编码中");
        valid_name(&self.name)?;
        ensure!(
            self.monitor.len() <= 256 && !self.monitor.chars().any(char::is_control),
            "屏幕名称无效"
        );
        ensure!(
            self.service_port > 0
                && self.discovery_port > 0,
            "共享端口无效"
        );
        self.native().validate()?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Destination {
    Ip {
        address: String,
        #[serde(default)]
        legacy_device: Option<String>,
    },
    Uu {
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
            (Self::Uu { device: a }, Self::Uu { device: b }) => a == b,
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
            Self::Ip { .. } => "IP 直连",
            Self::Uu { .. } => "UU",
            Self::Vps { .. } => "VPS",
            Self::LegacyInternet { .. } => "旧版连接服务",
        }
    }
    pub fn target(&self) -> String {
        match self {
            Self::Ip { address, .. } => address.clone(),
            Self::Uu { device } | Self::Vps { device, .. } => device.clone(),
            Self::LegacyInternet { .. } => "已迁移的旧配置".into(),
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
pub struct Preview {pub id:String,pub at:u64}
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
            client_configs:Vec::new(),selected_client:None,last_connected:None,preview:None,
            routes: vec![route],
            redirect: None,
        }
    }
    pub fn route(&self) -> Result<&Route> {
        self.routes
            .iter()
            .find(|v| v.id == self.selected_route)
            .context("请选择设备连接地址")
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
            version: 4,
            sharing: Sharing::default(),
            services: Vec::new(),
            devices: Vec::new(),
            legacy_imported: false,
        }
    }
}
fn valid_id(id: &str) -> Result<()> {
    ensure!(uuid::Uuid::parse_str(id).is_ok(), "配置标识无效");
    Ok(())
}
fn valid_name(name: &str) -> Result<()> {
    ensure!(
        !name.trim().is_empty()
            && name.chars().count() <= 64
            && !name.chars().any(char::is_control),
        "请输入有效的名称（最多64字）"
    );
    Ok(())
}
fn service_device(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 80
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
        "服务器中的设备名称只能使用字母、数字、短横线或下划线（最多80字节）"
    );
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
                .context("设备配置已移除")?;
            match &profile.redirect {
                Some(id) => current = id,
                None => return Ok(profile),
            }
        }
        anyhow::bail!("设备配置存在循环引用")
    }
    pub fn service(&self, id: &str) -> Result<&Service> {
        self.services
            .iter()
            .find(|p| p.id == id)
            .context("请先配置连接服务器")
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 4 && self.devices.len() <= 256 && self.services.len() <= 32,
            "设备配置版本或数量无效"
        );
        self.sharing.validate()?;
        let mut ids = std::collections::HashSet::new();
        for service in &self.services {
            valid_id(&service.id)?;
            valid_name(&service.name)?;
            ensure!(
                ids.insert(&service.id)
                    && service.address.len() <= 2048
                    && !service.address.trim().is_empty(),
                "服务器地址或标识无效"
            );
            service_device(&service.published_name)?;
            ensure!(
                !service.token_file.as_os_str().is_empty(),
                "请选择服务器访问凭据文件"
            );
        }
        let mut ids = std::collections::HashSet::new();
        for profile in &self.devices {
            valid_id(&profile.id)?;
            valid_name(&profile.name)?;
            if let Some(preview)=&profile.preview {valid_id(&preview.id)?;}
            let mut config_ids=std::collections::HashSet::new();
            for config in &profile.client_configs {config.validate()?;ensure!(config_ids.insert(&config.id),"客户端配置标识重复");}
            ensure!(profile.selected_client.as_ref().is_none_or(|id|config_ids.contains(id)),"请选择有效的客户端配置");
            ensure!(ids.insert(&profile.id), "设备配置标识重复");
            ensure!(
                profile
                    .peer
                    .as_ref()
                    .is_none_or(|id| id.len() == 64 && hex::decode(id).is_ok()),
                "设备身份指纹无效"
            );
            if profile.redirect.is_some() {
                self.device(&profile.id)?;
                continue;
            }
            ensure!(
                !profile.routes.is_empty() && profile.routes.len() <= 16,
                "每台设备需要1至16个连接地址"
            );
            profile.route()?;
            let mut routes = std::collections::HashSet::new();
            for route in &profile.routes {
                valid_id(&route.id)?;
                ensure!(routes.insert(&route.id), "连接地址标识重复");
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
                    Destination::Uu { device } => ensure!(
                        !device.trim().is_empty()
                            && device.len() <= 128
                            && device
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
                        "UU 设备编号无效"
                    ),
                    Destination::Vps { service, device } => {
                        self.service(service)?;
                        service_device(device)?;
                    }
                    Destination::LegacyInternet { settings } => {
                        ensure!(!settings.as_os_str().is_empty(), "旧配置路径无效")
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
        ensure!(self.version == 2, "串流配置版本无效");
        let profile = Profile {
            id: self.profile.clone(),
            name: self.name.clone(),
            peer: self.expected_peer.clone(),
            routes: vec![self.route.clone()],
            selected_route: self.route.id.clone(),
            client_configs:Vec::new(),selected_client:None,last_connected:None,preview:None,
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
            .map_err(|_| anyhow::anyhow!("串流目标配置格式无效"))?;
        value.validate()?;
        Ok(value)
    }
}

#[derive(Clone)]
pub struct Store {
    directory: PathBuf,
}
impl Store {
    pub fn store_service_credential(&self, id: &str, text: &str) -> Result<PathBuf> {
        valid_id(id)?;
        let text = text.trim();
        ensure!(
            text.len() == 64 && hex::decode(text).is_ok(),
            "服务器访问令牌应为64位十六进制字符"
        );
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
            .map_err(|_| anyhow::anyhow!("设备连接配置损坏，原文件已保留"))?;
        if matches!(value.version, 2 | 3) {
            // Move the old default listeners away from Sunshine once. Values
            // saved by current versions, and all custom ports, remain explicit.
            if value.sharing.service_port == 47998 { value.sharing.service_port = crate::native_discovery::SERVICE_PORT; }
            if value.sharing.discovery_port == 47999 { value.sharing.discovery_port = crate::native_discovery::DISCOVERY_PORT; }
            value.version = 4;
        }
        value.validate()?;
        Ok(value)
    }
    pub fn edit(&self, f: impl FnOnce(&mut Book) -> Result<()>) -> Result<()> {
        let _lock = TrustStore::lock_at(&self.directory)?;
        let mut value = self.read()?;
        f(&mut value)?;
        value.validate()?;
        let encoded=serde_json::to_vec_pretty(&value)?;
        ensure!(encoded.len()<=1024*1024,"设备与配置记录总量超过1 MiB，请先移除不再使用的记录");
        let original=self.directory.join("connections.json");
        if original.exists() {
            let bytes=read_private(&original)?;
            let version=serde_json::from_slice::<serde_json::Value>(&bytes)?["version"].as_u64().unwrap_or(0);
            if matches!(version, 2 | 3) {
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
                "该服务器仍用于设备连接，请先移除对应的连接地址"
            );
            book.services.retain(|s| s.id != id);
            Ok(())
        })
    }
    pub fn save_device(&self, profile: Profile) -> Result<()> {
        self.edit(|b| {
            if let Some(v) = b.devices.iter_mut().find(|v| v.id == profile.id) {
                ensure!(v.redirect.is_none() && profile.redirect.is_none(),"设备已合并，请刷新设备列表后重试");
                ensure!(
                    v.peer == profile.peer,
                    "设备身份刚刚更新，请重新打开设备详情后保存"
                );
                let mut profile=profile;
                profile.client_configs=v.client_configs.clone();
                profile.selected_client=v.selected_client.clone();
                profile.last_connected=v.last_connected;profile.preview=v.preview.clone();
                *v = profile;
            } else {
                b.devices.push(profile);
            }
            Ok(())
        })
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
        ensure!(!jpeg.is_empty()&&jpeg.len()<=1024*1024,"设备预览大小无效");
        let id=uuid::Uuid::new_v4().to_string();
        let path=self.directory.join("previews").join(format!("{id}.jpg"));
        crate::private_file::write_atomic(&path,jpeg)?;
        let mut old=None;let mut retained=false;
        let result=self.edit(|book| {
            let target=book.device(device)?.id.clone();
            let profile=book.devices.iter_mut().find(|p|p.id==target).unwrap();
            if profile.preview.as_ref().is_none_or(|p|p.at<=at) {
                old=profile.preview.replace(Preview{id:id.clone(),at});retained=true;
            }
            Ok(())
        });
        if result.is_err()||!retained {let _=std::fs::remove_file(path);}
        if result.is_ok() {if let Some(old)=old{let _=std::fs::remove_file(self.directory.join("previews").join(format!("{}.jpg",old.id)));}}
        result
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
                "设备身份与保存的记录不一致"
            );
            if let Some(other) = b.devices.iter().position(|p| {
                p.id != canonical && p.redirect.is_none() && p.peer.as_deref() == Some(peer)
            }) {
                let at=b.devices[current].last_connected.take();
                if let Some(at)=at {b.devices[other].last_connected=Some(b.devices[other].last_connected.unwrap_or(0).max(at));}
                if let Some(preview)=b.devices[current].preview.take() {
                    if b.devices[other].preview.as_ref().is_none_or(|p|p.at<preview.at){if let Some(old)=b.devices[other].preview.replace(preview){unused.push(old.id);}}else{unused.push(preview.id);}
                }
                let configs=std::mem::take(&mut b.devices[current].client_configs);
                let selection=b.devices[current].selected_client.take();
                for config in configs {if !b.devices[other].client_configs.iter().any(|c|c.id==config.id) {b.devices[other].client_configs.push(config);}}
                if b.devices[other].selected_client.is_none() {b.devices[other].selected_client=selection;}
                let routes = std::mem::take(&mut b.devices[current].routes);
                for route in routes {
                    if !b.devices[other]
                        .routes
                        .iter()
                        .any(|r| r.id == route.id || r.destination.same_target(&route.destination))
                    {
                        b.devices[other].routes.push(route);
                    }
                }
                let id = b.devices[other].id.clone();
                b.devices[current].redirect = Some(id);
            } else {
                b.devices[current].peer = Some(peer.into());
            }
            Ok(())
        })?;
        for id in unused{let _=std::fs::remove_file(self.directory.join("previews").join(format!("{id}.jpg")));}
        Ok(())
    }
    pub fn forget(&self, id: &str) -> Result<()> {
        let mut images=Vec::new();
        self.edit(|b| {
            let canonical=b.device(id)?.id.clone();
            for profile in b.devices.iter().filter(|p|p.id==canonical||p.redirect.as_deref()==Some(&canonical)) {
                if let Some(preview)=&profile.preview {images.push(preview.id.clone());}
            }
            b.devices.retain(|p|p.id!=canonical&&p.redirect.as_deref()!=Some(&canonical));Ok(())
        })?;
        for id in images{let _=std::fs::remove_file(self.directory.join("previews").join(format!("{id}.jpg")));}
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_settings::{ClientSettings,NamedClientSettings};
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
    fn successful_history_and_latest_private_preview_follow_verified_identity() {
        let root=std::env::temp_dir().join(format!("eclipse-history-{}",uuid::Uuid::new_v4()));
        let store=Store::open(&root).unwrap();
        let a=Profile::new("First",Destination::Uu{device:"first".into()});
        let b=Profile::new("Second",Destination::Uu{device:"second".into()});
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
        assert!(store.preview_bytes(&Preview{id:"../../credentials".into(),at:0}).is_err());
        let preview=record.preview.clone().unwrap();store.forget(&a.id).unwrap();assert!(store.preview_bytes(&preview).is_err());
        assert_eq!(std::fs::read_dir(root.join("previews")).unwrap().count(),0);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn v2_migration_is_read_only_until_save_and_preserves_a_private_exact_backup() {
        let root=std::env::temp_dir().join(format!("eclipse-migrate-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let profile=Profile::new("Generated host",Destination::Uu{device:"generated-device".into()});
        let mut old=serde_json::to_value(Book{devices:vec![profile.clone()],..Default::default()}).unwrap();
        old["version"]=serde_json::json!(2);
        let device=old["devices"][0].as_object_mut().unwrap();device.remove("client_configs");device.remove("selected_client");device.remove("last_connected");device.remove("preview");
        let original=serde_json::to_vec_pretty(&old).unwrap();
        let path=root.join("connections.json");crate::private_file::write_atomic(&path,&original).unwrap();
        let store=Store::open(&root).unwrap();
        assert_eq!(store.read().unwrap().version,4);
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
        let mut profile=Profile::new("Host",Destination::Uu{device:"generated-host".into()});
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
    fn verified_identity_merges_routes_and_presets_without_merging_similar_names() {
        let root=std::env::temp_dir().join(format!("eclipse-preset-merge-{}",uuid::Uuid::new_v4()));let store=Store::open(&root).unwrap();
        let a=Profile::new("Same name",Destination::Uu{device:"generated-a".into()});
        let b=Profile::new("Same name",Destination::Uu{device:"generated-b".into()});
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
        let legacy:Sharing=serde_json::from_value(serde_json::json!({"name":"generated"})).unwrap();
        assert_eq!(legacy.codec,None);assert_eq!(legacy.rate_control,RateMode::Cbr);
        assert_eq!(legacy.native().video_policy.unwrap().preferred_codec,None);
        let explicit:Sharing=serde_json::from_value(serde_json::json!({"name":"generated","codec":"h264"})).unwrap();
        assert_eq!(explicit.codec,Some(native::VideoCodec::H264));
        let mut sharing=legacy;sharing.codec=Some(native::VideoCodec::Av1);sharing.rate_control=RateMode::Vbr;sharing.bitrate_mbps=20;sharing.peak_bitrate_mbps=35;
        let restored:Sharing=serde_json::from_value(serde_json::to_value(&sharing).unwrap()).unwrap();restored.validate().unwrap();
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
            token_file: "server.token".into(),
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
            Destination::Uu {
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
                profile.routes[0].destination = Destination::Uu {
                    device: "new-destination".into(),
                };
                Ok(())
            })
            .unwrap();
        let frozen = Launch::read(&snapshot).unwrap();
        assert!(
            matches!(frozen.route.destination,Destination::Uu{device} if device=="generated-a")
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
}
