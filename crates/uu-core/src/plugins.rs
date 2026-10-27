//! Connection providers register against engine contracts. The native engine
//! has no dependency on any compatibility provider or its account protocol.
use crate::{internet::Connection, Config};
use anyhow::{bail, ensure, Result};
use std::{collections::BTreeMap, sync::{Arc, OnceLock, RwLock}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
}

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
    async fn join(&self, config: &Config, device: &str, development_relay: bool) -> Result<Connection>;
    async fn host(&self, config: &Config, development_relay: bool) -> Result<Box<dyn Listener>>;
    async fn listen(&self, config: &Config, development_relay: bool) -> Result<Box<dyn Listener>>;
}

#[derive(Default)]
pub struct Registry(RwLock<BTreeMap<&'static str, Arc<dyn ConnectionService>>>);
impl Registry {
    pub fn register(&self, service: Arc<dyn ConnectionService>) -> Result<()> {
        let id = service.descriptor().id;
        ensure!(!id.is_empty() && id.len() <= 64 && id.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'), "invalid plugin id");
        let mut services = self.0.write().unwrap();
        ensure!(!services.contains_key(id), "plugin {id} is already registered");
        services.insert(id, service);
        Ok(())
    }
    pub fn get(&self, id: &str) -> Result<Arc<dyn ConnectionService>> {
        if let Some(service) = self.0.read().unwrap().get(id) { return Ok(service.clone()); }
        bail!("connection plugin '{id}' is not installed or enabled")
    }
    pub fn descriptors(&self) -> Vec<Descriptor> {
        self.0.read().unwrap().values().map(|p| p.descriptor()).collect()
    }
}
pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::default)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture;
    #[async_trait::async_trait]
    impl ConnectionService for Fixture {
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
        registry.register(Arc::new(Fixture)).unwrap();
        assert_eq!(registry.get("fixture").unwrap().descriptor().id, "fixture");
        assert!(registry.register(Arc::new(Fixture)).is_err());
        assert!(registry.get("other").is_err());
    }
}
