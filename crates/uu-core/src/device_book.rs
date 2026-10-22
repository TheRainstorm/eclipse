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
    Software,
}
impl Encoding {
    pub fn native(self) -> native::EncoderPreference {
        match self {
            Self::Auto => native::EncoderPreference::Auto,
            Self::Hardware => native::EncoderPreference::Hardware,
            Self::Software => native::EncoderPreference::Software,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sharing {
    pub name: String,
    pub monitor: String,
    pub fps: u32,
    pub bitrate_mbps: u32,
    pub encoding: Encoding,
    pub control: bool,
    pub audio: bool,
    pub clipboard: bool,
    pub service_port: u16,
    pub discovery_port: u16,
    pub uu_enabled: bool,
}
impl Default for Sharing {
    fn default() -> Self {
        Self {
            name: "Eclipse".into(),
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
        }
    }
}
impl Sharing {
    pub fn native(&self) -> SharingConfig {
        SharingConfig {
            source: if self.monitor.is_empty() {
                native::CaptureSource::Primary
            } else {
                native::CaptureSource::Monitor(self.monitor.clone())
            },
            fps: self.fps,
            bitrate_kbps: self.bitrate_mbps * 1000,
            encoder: self.encoding.native(),
            session_limit: None,
            allow_input: self.control,
            audio: self.audio.then_some(native::AudioSource::System),
            clipboard: None,
        }
    }
    pub fn validate(&self) -> Result<()> {
        valid_name(&self.name)?;
        ensure!(
            self.monitor.len() <= 256 && !self.monitor.chars().any(char::is_control),
            "屏幕名称无效"
        );
        ensure!(
            (1..=60).contains(&self.fps)
                && (1..=100).contains(&self.bitrate_mbps)
                && self.service_port > 0
                && self.discovery_port > 0,
            "共享帧率、码率或端口无效"
        );
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub peer: Option<String>,
    pub routes: Vec<Route>,
    pub selected_route: String,
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
            version: 2,
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
            self.version == 2 && self.devices.len() <= 256 && self.services.len() <= 32,
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
            valid_name(&service.published_name)?;
        }
        let mut ids = std::collections::HashSet::new();
        for profile in &self.devices {
            valid_id(&profile.id)?;
            valid_name(&profile.name)?;
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
                            && !device.chars().any(char::is_control),
                        "UU 设备编号无效"
                    ),
                    Destination::Vps { service, device } => {
                        self.service(service)?;
                        valid_name(device)?;
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
            redirect: None,
        };
        let book = Book {
            devices: vec![profile],
            services: self.service.clone().into_iter().collect(),
            ..Default::default()
        };
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
        })
    }
    pub fn publish_launch(&self, id: &str) -> Result<PathBuf> {
        let launch = self.launch(id)?;
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
        let value: Book = serde_json::from_slice(&read_private(&path)?)
            .map_err(|_| anyhow::anyhow!("设备连接配置损坏，原文件已保留"))?;
        value.validate()?;
        Ok(value)
    }
    pub fn edit(&self, f: impl FnOnce(&mut Book) -> Result<()>) -> Result<()> {
        let _lock = TrustStore::lock_at(&self.directory)?;
        let mut value = self.read()?;
        f(&mut value)?;
        value.validate()?;
        crate::private_file::write_atomic(
            &self.directory.join("connections.json"),
            &serde_json::to_vec_pretty(&value)?,
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
    pub fn save_device(&self, profile: Profile) -> Result<()> {
        self.edit(|b| {
            if let Some(v) = b.devices.iter_mut().find(|v| v.id == profile.id) {
                ensure!(
                    v.peer == profile.peer,
                    "设备身份刚刚更新，请重新打开设备详情后保存"
                );
                *v = profile;
            } else {
                b.devices.push(profile);
            }
            Ok(())
        })
    }
    pub fn bind(&self, profile: &str, peer: &str) -> Result<()> {
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
                let routes = std::mem::take(&mut b.devices[current].routes);
                for route in routes {
                    if !b.devices[other].routes.iter().any(|r| r.id == route.id) {
                        b.devices[other].routes.push(route);
                    }
                }
                let id = b.devices[other].id.clone();
                b.devices[current].redirect = Some(id);
            } else {
                b.devices[current].peer = Some(peer.into());
            }
            Ok(())
        })
    }
    pub fn forget(&self, id: &str) -> Result<()> {
        self.edit(|b| {
            let canonical = b.device(id)?.id.clone();
            b.devices
                .retain(|p| p.id != canonical && p.redirect.as_deref() != Some(&canonical));
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let snapshot=store.publish_launch(&a.id).unwrap();
        store.edit(|book| {
            let profile=book.devices.iter_mut().find(|p|p.id==a.id).unwrap();
            profile.routes[0].destination=Destination::Uu{device:"new-destination".into()};Ok(())
        }).unwrap();
        let frozen=Launch::read(&snapshot).unwrap();
        assert!(matches!(frozen.route.destination,Destination::Uu{device} if device=="generated-a"));
        let id = "a".repeat(64);
        store.bind(&a.id, &id).unwrap();
        assert!(store.bind(&a.id, &"b".repeat(64)).is_err());
        store.bind(&b.id, &id).unwrap();
        let book = store.read().unwrap();
        assert_eq!(book.device(&b.id).unwrap().id, a.id);
        assert_eq!(book.device(&a.id).unwrap().routes.len(), 2);
        assert_eq!(book.services[0].token_file, service.token_file);
        assert!(
            store.save_device(a).is_err(),
            "stale UI must not erase a freshly verified peer identity"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
