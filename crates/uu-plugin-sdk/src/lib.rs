//! Runtime extensions share the engine, UI and async runtime through this DLL.
//!
//! This is a version-matched native SDK, not a stable Rust binary ABI. A bounded
//! manifest and C metadata entry are checked before calling a Rust factory.
//! Installed SDK sources, compiler, target and features must match. Libraries
//! remain loaded for the process lifetime, including after disabling a plugin.
pub use eframe::egui;
pub use tokio;
pub use uu_core as engine;
pub use uu_i18n as i18n;
pub use anyhow;
use anyhow::{bail,Result};
use serde::{Deserialize,Serialize};
use std::sync::Arc;

pub mod loader;
pub mod ui;
pub mod theme;
pub mod runtime;
pub const API_VERSION:u32=1;
pub const SDK_ID:&str=env!("ECLIPSE_PLUGIN_SDK_ID");

#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub api_version:u32,
    pub sdk_id:String,
    pub id:String,
    pub name:String,
    pub version:String,
    pub library:String,
    pub library_sha256:String,
    #[serde(default)]
    pub data_files:Vec<String>,
}
#[derive(Clone)]
pub struct Context {
    pub id:String,
    pub application:engine::Config,
    pub data:engine::Config,
}
impl Context {
    pub fn new(application:&engine::Config,id:&str)->Self {
        let data=engine::Config{data_dir:application.at("plugins").join(id),state_dir:application.state_dir.join("plugins").join(id),signaling_host:application.signaling_host.clone()};
        Self{id:id.into(),application:application.clone(),data}
    }
    pub fn preferences(&self)->Result<engine::plugins::PluginPreferences> {Ok(engine::plugins::Preferences::load(&self.application)?.get(&self.id))}
    pub fn save_preferences(&self,expected:&engine::plugins::PluginPreferences,value:engine::plugins::PluginPreferences)->Result<()> {
        engine::plugins::Preferences::save_plugin(&self.application,&self.id,expected,value)?;Ok(())
    }
}

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum Placement { Navigation, Connections, Settings }
#[derive(Clone,Debug)]
pub struct Page {pub id:String,pub title:String,pub placement:Placement}
pub enum Action {
    /// Identity is public and used for tab reuse. The opaque target can contain
    /// authentication material and is passed only through bounded stdin/IPC.
    Connect {identity:String,target:zeroize::Zeroizing<String>,name:String,view_only:bool},
    RefreshDevices,
    ProviderReset,
}
pub trait PageState {
    fn show(&mut self,ui:&mut egui::Ui,page:&str)->Vec<Action>;
}
#[derive(Clone)]
pub struct ClientSupport {
    pub host_managed_bitrate:bool,
    pub max_fps:u32,
    pub codecs:Vec<engine::native::VideoCodec>,
}
impl Default for ClientSupport {
    fn default()->Self {Self{host_managed_bitrate:false,max_fps:240,codecs:vec![engine::native::VideoCodec::H264,engine::native::VideoCodec::Hevc,engine::native::VideoCodec::Av1,engine::native::VideoCodec::Pyrowave]}}
}
/// Implementations own protocol/account/UI state. The host knows descriptors
/// and engine session contracts, and never identifies a particular provider.
pub trait Plugin:Send+Sync {
    fn client_support(&self)->ClientSupport {ClientSupport::default()}
    /// Called after explicit persisted opt-in, before opening new entrances.
    /// Disabling must not terminate already admitted sessions.
    fn set_enabled(&self,_enabled:bool)->Result<()> {Ok(())}
    fn pages(&self)->Vec<Page> {Vec::new()}
    fn gui(&self)->Result<Box<dyn PageState>> {bail!("plugin has no UI")}
    fn service(&self)->Option<Arc<dyn engine::plugins::ConnectionService>> {None}
    fn client_settings(&self,defaults:engine::client_settings::ClientSettings)->Result<engine::client_settings::ClientSettings> {Ok(defaults)}
    fn save_client(&self,_settings:engine::client_settings::ClientSettings)->Result<()> {bail!("plugin does not store client preferences")}
    fn connect(&self,_target:zeroize::Zeroizing<String>,_client:engine::client_settings::ClientSettings,_stats:Arc<engine::media::MediaStats>)->Result<engine::controller::ControllerHandle> {bail!("plugin has no client")}
    fn cli(&self,_arguments:Vec<String>)->Result<()> {bail!("plugin has no command interface")}
}

/// Only this fixed-size envelope crosses the discovery boundary. The factory
/// uses the matched SDK's Rust ABI and must never run before SDK validation.
#[repr(C)]
pub struct Entry {
    pub api_version:u32,
    pub struct_bytes:u32,
    pub sdk_id:[u8;64],
    pub create:fn(Context)->Result<Box<dyn Plugin>>,
}
pub type EntryPoint=unsafe extern "C" fn()->*const Entry;

pub const fn entry(create:fn(Context)->Result<Box<dyn Plugin>>)->Entry {
    let bytes=SDK_ID.as_bytes();let mut sdk_id=[0;64];let mut i=0;
    while i<64 {sdk_id[i]=bytes[i];i+=1;}
    Entry{api_version:API_VERSION,struct_bytes:std::mem::size_of::<Entry>() as u32,sdk_id,create}
}
