//! Optional protocol plugin. The engine does not depend on this crate.
pub mod api;
pub mod protocol;
pub mod selfloop;
pub use selfloop::SelfLoopConfig;
pub mod error;
pub mod assistance;
pub mod config;
pub mod connection;
mod ice;
pub mod controller;
pub mod media;
pub mod sign;
pub mod signal;
pub mod signing_key;
pub mod socketio;
mod compatibility_signing;
mod registration;

pub use api::{ClientState, QrLogin, UuApi};
pub use sign::ClientIdentity;
pub use signal::SignalClient;
pub use uu_core::{Config, capabilities, direct_ice, display_frames, session, connection_info, rtx, shared_host, native, install_crypto_provider};
pub mod control_wire;
pub mod screen_control;
pub mod cursor;
pub mod clipboard;
mod private_file { pub use uu_core::plugin_backend::write_private_file as write_atomic; }
mod congestion { pub use uu_core::plugin_backend::configure_congestion as configure; }
mod srtp_policy { pub use uu_core::plugin_backend::configure_srtp as configure; }
mod network_probe;

/// The build feature can exclude every network entry point. The process
/// override also applies to the CLI/background host and its child processes.
pub fn available() -> bool {
    cfg!(feature = "enabled") && std::env::var("ECLIPSE_UU_PLUGIN").as_deref() != Ok("0")
}
pub fn require_enabled() -> anyhow::Result<()> {
    anyhow::ensure!(available(), "UU compatibility plugin is not installed or enabled");
    Ok(())
}
pub fn register() -> anyhow::Result<()> {
    #[cfg(feature = "enabled")]
    if available() { uu_core::plugins::registry().register(std::sync::Arc::new(Service))?; }
    Ok(())
}
struct Service;
#[async_trait::async_trait]
impl uu_core::plugins::ConnectionService for Service {
    fn descriptor(&self) -> uu_core::plugins::Descriptor {
        uu_core::plugins::Descriptor { id: "uu", name: "UU compatibility", version: env!("CARGO_PKG_VERSION") }
    }
    async fn join(&self, config: &Config, device: &str, relay: bool) -> anyhow::Result<uu_core::internet::Connection> {
        connection::join(config, device, relay).await
    }
    async fn host(&self, config: &Config, relay: bool) -> anyhow::Result<Box<dyn uu_core::plugins::Listener>> {
        Ok(Box::new(connection::Host::online(config, relay).await?))
    }
    async fn listen(&self, config: &Config, relay: bool) -> anyhow::Result<Box<dyn uu_core::plugins::Listener>> {
        Ok(Box::new(connection::Listener::online(config, relay).await?))
    }
}
#[async_trait::async_trait]
impl uu_core::plugins::Listener for connection::Host {
    async fn accept(&mut self) -> anyhow::Result<uu_core::internet::Connection> { self.accept().await }
    async fn close(&mut self) { self.close().await; }
}
#[async_trait::async_trait]
impl uu_core::plugins::Listener for connection::Listener {
    async fn accept(&mut self) -> anyhow::Result<uu_core::internet::Connection> { self.accept().await }
    async fn close(&mut self) { self.close().await; }
}
