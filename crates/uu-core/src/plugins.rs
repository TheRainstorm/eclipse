//! Connection providers register against engine contracts. The native engine
//! has no dependency on any compatibility provider or its account protocol.
use crate::{internet::Connection, Config};
use anyhow::{bail, ensure, Result};
use std::{collections::BTreeMap, sync::{Arc, OnceLock, RwLock}};

mod preferences;
pub use preferences::{Preferences, PluginPreferences};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
}
pub struct RemoteDevice {pub id:String,pub name:String,pub online:Option<bool>}

/// A provider may host another wire protocol without weakening the native
/// device-authentication handshake. It receives session ownership, not native
/// administrative credentials or a fabricated public-key pairing.
pub struct HostContext {
    pub sharing:crate::native::SharingConfig,
    pub sessions:Arc<crate::shared_host::SessionGate>,
    pub stop:tokio::sync::watch::Receiver<bool>,
    pub progress:tokio::sync::watch::Sender<crate::native::HostStatus>,
    pub clipboard:Option<crate::native_lan::ClipboardFactory>,
}
#[async_trait::async_trait]
pub trait ProtocolSession: Send {
    async fn serve(self:Box<Self>, context:HostContext)->Result<crate::native::HostReport>;
}
pub enum Incoming {
    Native(Connection),
    Protocol(Box<dyn ProtocolSession>),
}

#[async_trait::async_trait]
pub trait Listener: Send {
    async fn accept(&mut self) -> Result<Connection>;
    async fn accept_session(&mut self)->Result<Incoming> {self.accept().await.map(Incoming::Native)}
    async fn close(&mut self);
}

#[async_trait::async_trait]
pub trait ConnectionService: Send + Sync {
    fn descriptor(&self) -> Descriptor;
    /// Whether to offer this provider for native routes and account settings.
    fn visible(&self)->bool {true}
    /// An explicitly enabled protocol module may require its shared transport
    /// even when its native connection entrance is not selected.
    fn host_required(&self)->bool {false}
    async fn join(&self, config: &Config, device: &str, development_relay: bool) -> Result<Connection>;
    async fn host(&self, config: &Config, development_relay: bool) -> Result<Box<dyn Listener>>;
    async fn listen(&self, config: &Config, development_relay: bool) -> Result<Box<dyn Listener>>;
    async fn devices(&self,_config:&Config)->Result<Vec<RemoteDevice>> {bail!("connection service has no device catalog")}
}

#[derive(Default)]
pub struct Registry {
    services:RwLock<BTreeMap<&'static str,Registration>>,
    generation:std::sync::atomic::AtomicU64,
}
struct Registration {service:Arc<dyn ConnectionService>,enabled:bool}
impl Registry {
    pub fn register(&self, service: Arc<dyn ConnectionService>) -> Result<()> {
        let id = service.descriptor().id;
        ensure!(!id.is_empty() && id.len() <= 64 && id.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'), "invalid plugin id");
        let mut services = self.services.write().unwrap();
        ensure!(!services.contains_key(id), "plugin {id} is already registered");
        services.insert(id, Registration{service,enabled:false});
        self.invalidate();
        Ok(())
    }
    pub fn get(&self, id: &str) -> Result<Arc<dyn ConnectionService>> {
        if let Some(record) = self.services.read().unwrap().get(id).filter(|r|r.enabled) { return Ok(record.service.clone()); }
        bail!("connection plugin '{id}' is not installed or enabled")
    }
    pub fn descriptors(&self) -> Vec<Descriptor> {
        self.services.read().unwrap().values().filter(|r|r.enabled&&r.service.visible()).map(|p| p.service.descriptor()).collect()
    }
    pub fn host_services(&self,preferences:&Preferences,configured:&[String])->Vec<Descriptor> {
        self.services.read().unwrap().values().filter(|r|r.enabled).filter_map(|r| {
            let descriptor=r.service.descriptor();
            (preferences.get(descriptor.id).enabled&&(r.service.host_required()||configured.iter().any(|id|id==descriptor.id))).then_some(descriptor)
        }).collect()
    }
    pub fn set_enabled(&self,id:&str,enabled:bool)->Result<()> {
        let mut services=self.services.write().unwrap();
        let record=services.get_mut(id).ok_or_else(||anyhow::anyhow!("connection plugin '{id}' is not installed"))?;
        if record.enabled!=enabled {record.enabled=enabled;self.invalidate();}Ok(())
    }
    pub fn invalidate(&self){self.generation.fetch_add(1,std::sync::atomic::Ordering::AcqRel);}
    pub fn generation(&self)->u64{self.generation.load(std::sync::atomic::Ordering::Acquire)}
}
pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::default)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {protocol_only:bool}
    #[async_trait::async_trait]
    impl ConnectionService for Fixture {
        fn visible(&self)->bool {!self.protocol_only}
        fn host_required(&self)->bool {self.protocol_only}
        fn descriptor(&self) -> Descriptor { Descriptor { id: "fixture", name: "Fixture", version: "1" } }
        async fn join(&self, _: &Config, _: &str, _: bool) -> Result<Connection> { bail!("fixture") }
        async fn host(&self, _: &Config, _: bool) -> Result<Box<dyn Listener>> { bail!("fixture") }
        async fn listen(&self, _: &Config, _: bool) -> Result<Box<dyn Listener>> { bail!("fixture") }
    }
    #[test]
    fn providers_are_explicit_and_duplicate_registration_is_rejected() {
        let registry = Registry::default();
        assert!(registry.descriptors().is_empty());
        assert!(registry.get("fixture").is_err());
        registry.register(Arc::new(Fixture{protocol_only:false})).unwrap();
        assert!(registry.get("fixture").is_err());
        registry.set_enabled("fixture",true).unwrap();
        assert_eq!(registry.get("fixture").unwrap().descriptor().id, "fixture");
        assert!(registry.register(Arc::new(Fixture{protocol_only:false})).is_err());
        assert!(registry.get("other").is_err());
        let running=registry.get("fixture").unwrap();
        registry.set_enabled("fixture",false).unwrap();
        assert!(registry.get("fixture").is_err());
        assert!(registry.descriptors().is_empty());
        assert_eq!(running.descriptor().id,"fixture");
    }
    #[test]
    fn standalone_protocol_host_requires_plugin_opt_in_without_exposing_native_routes() {
        let registry=Registry::default();registry.register(Arc::new(Fixture{protocol_only:true})).unwrap();
        let mut preferences=Preferences::default();
        assert!(registry.host_services(&preferences,&[]).is_empty());registry.set_enabled("fixture",true).unwrap();
        assert!(registry.descriptors().is_empty());assert!(registry.get("fixture").is_ok());
        assert!(registry.host_services(&preferences,&[]).is_empty(),"an installed protocol cannot bypass persisted plugin opt-in");
        preferences.plugins.insert("fixture".into(),PluginPreferences{enabled:true,..Default::default()});
        assert_eq!(registry.host_services(&preferences,&[]).len(),1,"explicit protocol module publishes independently of native entrance selection");
        registry.set_enabled("fixture",false).unwrap();assert!(registry.host_services(&preferences,&[]).is_empty());
    }
}
