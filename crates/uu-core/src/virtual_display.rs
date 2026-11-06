//! Ownership of virtual targets activated by this host process.
//!
//! These operations block and belong on a worker, not the GUI/event-loop thread.
//! Activation requires an unclaimed, connected but inactive target. A backend
//! may provision capacity in its own exact-resource transaction before a fresh
//! inventory; an existing active output is never adopted. Target claims remain
//! live across temporary external deactivation and failed cleanup.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fmt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
};
use uuid::Uuid;

#[cfg(any(windows, test))]
mod capacity_config;
#[cfg(any(windows, test))]
mod capacity_policy;
#[cfg(any(windows, test))]
mod capacity_recovery;
#[cfg(any(windows, test))]
mod color_state;
#[cfg(any(windows, test))]
mod config_apply;
#[cfg(windows)]
mod install_windows;
#[cfg(windows)]
mod ownership_windows;
pub mod preparation;
mod policy;
#[cfg(any(windows, test))]
mod provision_policy;
#[cfg(windows)]
mod windows;

pub const INSTALL_URL: &str = "https://github.com/VirtualDrivers/Virtual-Display-Driver/releases";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DriverFile {
    pub name: String,
    pub bytes: usize,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DriverPackage {
    pub project: String,
    pub release: String,
    pub driver_version: String,
    pub architecture: String,
    pub license: String,
    pub source_url: String,
    pub url: String,
    pub zip_sha256: String,
    pub files: Vec<DriverFile>,
}

pub fn driver_package() -> &'static DriverPackage {
    static PACKAGE: OnceLock<DriverPackage> = OnceLock::new();
    PACKAGE.get_or_init(|| {
        serde_json::from_str(include_str!("virtual_display/driver-package.json"))
            .expect("embedded virtual display package manifest")
    })
}

#[cfg(any(windows, test))]
fn read_pinned_package(directory: &std::path::Path) -> Result<Vec<(String, Vec<u8>)>> {
    use std::io::Read;
    let mut files = Vec::new();
    for expected in &driver_package().files {
        let file = std::fs::File::open(directory.join(&expected.name))
            .map_err(|_| Error::Platform(format!("missing driver file {}", expected.name)))?;
        let mut data = Vec::with_capacity(expected.bytes);
        file.take(expected.bytes as u64 + 1)
            .read_to_end(&mut data)
            .map_err(|_| Error::Platform(format!("cannot read driver file {}", expected.name)))?;
        if data.len() != expected.bytes || hex::encode(Sha256::digest(&data)) != expected.sha256 {
            return Err(Error::Platform(format!(
                "driver file {} does not match the pinned official release",
                expected.name
            )));
        }
        files.push((expected.name.clone(), data));
    }
    Ok(files)
}

/// Explicit elevated first installation. The caller owns download, extraction
/// and the UAC prompt; this function never elevates itself or imports trust roots.
pub fn install_signed(package_directory: &std::path::Path) -> Result<Status> {
    #[cfg(windows)]
    {
        let manager = Manager::shared();
        {
            let ownership = manager.lock()?;
            if manager
                .backend
                .preparation_observation()?
                .is_some_and(|observed| observed.driver_present)
            {
                return Err(Error::Platform("an installed driver pool must be prepared through the running background management service".into()));
            }
            manager
                .fresh_install_admission()
                .map_err(|failure| Error::Platform(failure.message))?;
            let package = read_pinned_package(package_directory)?;
            let provider = CapacityProvider {
                manager: &manager,
                leases: ownership.len() as u32,
                initial: None,
                package: Some(&package),
            };
            let status = manager
                .preparation
                .request(&provider, u32::from(manager.limit.load(Ordering::Acquire)).max(1))
                .map_err(|failure| Error::Platform(failure.message))?;
            if status.phase != preparation::Phase::Ready {
                return Err(Error::Preparation(status));
            }
        }
        manager.status()
    }
    #[cfg(not(windows))]
    {
        let _ = package_directory;
        Err(Error::Unsupported)
    }
}

/// Explicit management request for an idle driver pool. Reconfiguration belongs
/// to the protected running host and its session gate; an elevated standalone
/// frontend cannot infer whether that host has active capture sessions.
pub fn prepare() -> Result<()> {
    Manager::shared().prepare_driver()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LeaseId(pub Uuid);

impl fmt::Display for LeaseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Owner {
    Manual,
    /// The authenticated connection ID, never a media epoch or a device name.
    Session(Uuid),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spec {
    pub width: u32,
    pub height: u32,
    pub refresh_millihz: u32,
    pub hdr: bool,
}

impl Default for Spec {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            refresh_millihz: 60_000,
            hdr: false,
        }
    }
}

impl Spec {
    pub fn validate(self) -> Result<()> {
        if !(320..=16_384).contains(&self.width)
            || !(200..=16_384).contains(&self.height)
            || !(1_000..=1_000_000).contains(&self.refresh_millihz)
        {
            return Err(Error::InvalidSpec);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub supported: bool,
    pub driver_present: bool,
    pub available_slots: usize,
    /// Live resources currently held by this manager.
    pub leased: usize,
    /// Durable allocations, including persisted displays waiting for recovery.
    #[serde(default)]
    pub allocated: usize,
    #[serde(default)]
    pub keep_after_stream: bool,
    #[serde(default="default_display_limit")]
    pub limit: u16,
    #[serde(default)]
    pub supports_lifetimes: bool,
    #[serde(default)]
    pub supports_configuration: bool,
    pub explanation: String,
    pub install_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation: Option<preparation::Status>,
}

impl Status {
    /// Older peers do not know the durable allocation counter.
    pub fn allocation_count(&self)->usize {self.allocated.max(self.leased)}
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Display {
    pub id: LeaseId,
    pub owner: Owner,
    pub spec: Spec,
    pub actual: Option<Spec>,
    pub name: String,
    /// The actual GDI source name, matching `native::monitors().name`.
    pub source: String,
    pub active: bool,
    #[serde(default,skip_serializing_if="is_false")]
    pub persistent: bool,
    /// A failed removal retains ownership; callers can retry the same ID.
    pub cleanup_pending: bool,
    pub last_error: Option<String>,
}

/// Host-local preparation inventory. These IDs are not remote control
/// capabilities. Only an explicit host-local management action may prepare one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    pub id: String,
    pub name: String,
    pub source: String,
    pub active: bool,
    pub leased: bool,
    pub actual: Option<Spec>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("virtual displays are only supported on Windows")]
    Unsupported,
    #[error("install a supported signed virtual display driver first")]
    DriverMissing,
    #[error("no unused connected virtual display slot is available; existing active displays are never taken over")]
    NoAvailableSlot,
    #[error("virtual display preparation: {0:?}")]
    Preparation(preparation::Status),
    #[error("this virtual display is already leased by another host process")]
    Claimed,
    #[error("invalid virtual display size or refresh rate")]
    InvalidSpec,
    #[error("this virtual display belongs to another session or was created manually")]
    WrongOwner,
    #[error("the authenticated connection is closed or has no display owner guard")]
    SessionClosed,
    #[error("the host is stopping and no longer accepts manual virtual displays")]
    HostClosing,
    #[error("the selected virtual display slot is unavailable or already leased")]
    UnknownSlot,
    #[error("the last active desktop display cannot be prepared as a virtual display slot")]
    LastActiveOutput,
    #[error("the host virtual display limit ({limit}) has been reached")]
    LimitReached { limit:u16 },
    #[error("virtual display operation failed: {0}")]
    Platform(String),
    #[error("administrator permission is required to prepare the virtual display driver")]
    AdministratorRequired,
    #[error("virtual display {id} still needs cleanup: {message}")]
    CleanupPending { id: LeaseId, message: String },
}

pub type Result<T> = std::result::Result<T, Error>;
pub const DEFAULT_DISPLAY_LIMIT:u16=2;
/// Current native display catalog and management envelopes contain at most 16 outputs.
pub const MAX_DISPLAY_LIMIT:u16=16;
pub fn default_display_limit()->u16 {DEFAULT_DISPLAY_LIMIT}
fn is_false(value:&bool)->bool{!*value}

// Full target identity remains host-private. In particular, a remote caller
// cannot turn an arbitrary adapter/target number into a cleanup capability.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct TargetKey {
    adapter: (u32, i32),
    target: u32,
    monitor_path: String,
}

impl TargetKey {
    fn slot_id(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(self.adapter.0.to_le_bytes());
        digest.update(self.adapter.1.to_le_bytes());
        digest.update(self.target.to_le_bytes());
        digest.update(self.monitor_path.as_bytes());
        hex::encode(digest.finalize())
    }
}

#[derive(Clone, Debug)]
struct Target {
    key: TargetKey,
    name: String,
    source: String,
    active: bool,
    available: bool,
    actual: Option<Spec>,
}

struct Inventory {
    supported: bool,
    driver_present: bool,
    targets: Vec<Target>,
}

struct ActivationFailure {
    message: String,
    /// True once our SetDisplayConfig APPLY was submitted, even if it failed.
    /// A validation or precondition failure does not authorize cleanup; after
    /// submission, ownership lasts until exact target cleanup is verified.
    activated: bool,
}

/// Retained through activation, rollback and removal, including inactive periods.
/// Dropping this guard releases only this process's claim on the exact target.
trait Claim: Send + Sync {}

trait Backend: Send + Sync {
    /// Finish this process's earlier partial mutation before admitting another.
    /// Read-only inventory/status calls do not trigger recovery.
    fn recover(&self) -> Result<()> {
        Ok(())
    }
    fn recover_capacity(
        &self,
        checkpoint: &(dyn Fn() -> preparation::Result<()> + Sync),
    ) -> preparation::Result<()> {
        checkpoint()?;
        self.recover()
            .map_err(|error| preparation::Failure::unchanged(error.to_string()))
    }
    fn claim(&self, key: &TargetKey) -> Result<Box<dyn Claim>>;
    fn is_claimed(&self, key: &TargetKey) -> Result<bool>;
    /// Supply an unused target without adopting or reconfiguring existing output.
    /// Success means it can be freshly enumerated; failures must own their rollback.
    fn provision(&self) -> Result<()> {
        Err(Error::NoAvailableSlot)
    }
    fn inventory(&self) -> Result<Inventory>;
    fn preparation_observation(&self) -> Result<Option<preparation::Observation>> {
        Ok(None)
    }
    fn preparation_authorized(&self) -> bool {
        false
    }
    fn download_capacity_driver(&self) -> preparation::Result<Vec<(String, Vec<u8>)>> {
        Err(preparation::Failure::unchanged(
            "driver preparation is unavailable on this platform",
        ))
    }
    fn prepare_capacity(
        &self,
        _minimum: u32,
        _package: Option<&[(String, Vec<u8>)]>,
        _checkpoint: &(dyn Fn() -> preparation::Result<()> + Sync),
    ) -> preparation::Result<()> {
        Err(preparation::Failure::unchanged(
            "driver capacity preparation is unavailable on this platform",
        ))
    }
    fn activate(
        &self,
        key: &TargetKey,
        spec: Spec,
    ) -> std::result::Result<Target, ActivationFailure>;
    /// Changes only an already claimed target, retaining its desktop position.
    fn reconfigure(&self, _key:&TargetKey, _spec:Spec)->std::result::Result<Target,ActivationFailure> {
        Err(ActivationFailure{message:"display mode editing is unavailable".into(),activated:false})
    }
    fn deactivate(&self, key: &TargetKey) -> Result<()>;
    fn prepare(&self, key: &TargetKey) -> Result<()>;
}

struct Entry {
    _claim: Box<dyn Claim>,
    target: TargetKey,
    display: Display,
    explicit_temporary: bool,
}

/// One process-wide owner for manual and authenticated-session display leases.
/// All requests and readbacks are serialized, including failure cleanup.
pub struct Manager {
    backend: Box<dyn Backend>,
    entries: Mutex<BTreeMap<LeaseId, Entry>>,
    sessions: Mutex<BTreeMap<Uuid, Weak<AtomicBool>>>,
    manual_closing: AtomicBool,
    preparation: preparation::Coordinator,
    preparation_owner: AtomicBool,
    editing_mode: AtomicBool,
    hosts: Mutex<Vec<Weak<crate::shared_host::SessionGate>>>,
    policy: Mutex<policy::State>,
    limit: std::sync::atomic::AtomicU16,
}

impl Manager {
    pub fn shared() -> Arc<Self> {
        static MANAGER: OnceLock<Arc<Manager>> = OnceLock::new();
        MANAGER
            .get_or_init(|| {
                #[cfg(windows)]
                let backend: Box<dyn Backend> = Box::new(windows::Windows);
                #[cfg(not(windows))]
                let backend: Box<dyn Backend> = Box::new(Unsupported);
                Arc::new(Self {
                    backend,
                    entries: Mutex::new(BTreeMap::new()),
                    sessions: Mutex::new(BTreeMap::new()),
                    manual_closing: AtomicBool::new(false),
                    preparation: preparation::Coordinator::default(),
                    preparation_owner: AtomicBool::new(false),
                    editing_mode: AtomicBool::new(false),
                    hosts: Mutex::default(),
                    policy: Mutex::default(),
                    limit: std::sync::atomic::AtomicU16::new(DEFAULT_DISPLAY_LIMIT),
                })
            })
            .clone()
    }

    /// Only the protected service-session entry calls this after verifying its
    /// LocalSystem identity, console session, stop event and installed path.
    /// A foreground helper must never impersonate a running host's idle state.
    pub fn enable_service_preparation(&self) -> Result<()> {
        #[cfg(windows)]
        {
            crate::service_admin::windows::require_administrator()
                .map_err(|_| Error::AdministratorRequired)?;
            let user = crate::service_admin::windows::process_user(unsafe {
                windows_sys::Win32::System::Threading::GetCurrentProcess()
            })
            .map_err(|e| Error::Platform(e.to_string()))?;
            if user != "S-1-5-18" {
                return Err(Error::AdministratorRequired);
            }
            self.preparation_owner.store(true, Ordering::Release);
            Ok(())
        }
        #[cfg(not(windows))]
        {
            Err(Error::Unsupported)
        }
    }
    /// The standalone first-install helper has no running host's activity gate.
    /// Never infer that another host is idle from this process's empty sessions.
    fn fresh_install_admission(&self) -> preparation::Result<()> {
        if self.preparation_owner.load(Ordering::Acquire) {
            return Ok(());
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::ERROR_SERVICE_DOES_NOT_EXIST;
            use windows_sys::Win32::System::Services::SERVICE_STOPPED;
            match crate::service_admin::windows::status() {
                Ok(SERVICE_STOPPED)=>Ok(()),
                Err(error) if error.downcast_ref::<std::io::Error>().and_then(std::io::Error::raw_os_error)==Some(ERROR_SERVICE_DOES_NOT_EXIST as i32)=>Ok(()),
                Ok(_)=>Err(preparation::Failure::unchanged("the background service is active; prepare virtual displays through its administrator IPC")),
                Err(error)=>Err(preparation::Failure::unchanged(format!("cannot verify that the background service is stopped: {error:#}"))),
            }
        }
        #[cfg(not(windows))]
        {
            Err(preparation::Failure::unchanged(
                "standalone driver installation is only supported on Windows",
            ))
        }
    }
    pub(crate) fn register_host(&self, gate: &Arc<crate::shared_host::SessionGate>) {
        if let Ok(mut hosts) = self.hosts.lock() {
            hosts.retain(|host| host.strong_count() != 0);
            let gate = Arc::downgrade(gate);
            if !hosts.iter().any(|host| host.ptr_eq(&gate)) {
                hosts.push(gate);
            }
        }
    }
    fn preparation_streams(&self, initial: Option<&str>) -> u32 {
        self.hosts
            .lock()
            .map(|hosts| {
                hosts
                    .iter()
                    .filter_map(Weak::upgrade)
                    .map(|gate| gate.preparation_activity(initial))
                    .fold(0u32, u32::saturating_add)
            })
            .unwrap_or(u32::MAX)
    }
    pub(crate) fn preparation_in_flight(&self) -> bool {
        self.preparation.in_flight() || self.editing_mode.load(Ordering::Acquire)
    }
    pub(crate) fn has_queued_preparation(&self) -> bool {
        self.preparation.queued()
    }
    pub fn preparation_status(&self) -> Result<Option<preparation::Status>> {
        let entries = self.lock()?;
        if self.backend.preparation_observation()?.is_none() {
            return Ok(None);
        }
        self.preparation
            .inspect(&CapacityProvider {
                manager: self,
                leases: entries.len() as u32,
                initial: None,
                package: None,
            })
            .map(Some)
            .map_err(|failure| Error::Platform(failure.message))
    }
    pub fn resume_preparation(&self) -> Result<Option<preparation::Status>> {
        let entries = self.lock()?;
        if self.manual_closing.load(Ordering::Acquire) {
            return Err(Error::HostClosing);
        }
        if self.backend.preparation_observation()?.is_none() {
            return Ok(None);
        }
        let provider=CapacityProvider {
            manager:self,leases:entries.len() as u32,initial:None,package:None,
        };
        let result=if self.preparation.recovery_queued() {
            self.preparation.recover(&provider,||self.backend.recover_capacity(&||preparation::Provider::host_idle(&provider)))
        } else {self.preparation.resume(&provider)};
        result
            .map(Some)
            .map_err(|failure| Error::Platform(failure.message))
    }
    pub fn recover_preparation(&self) -> Result<Option<preparation::Status>> {
        let entries = self.lock()?;
        if self.manual_closing.load(Ordering::Acquire) {return Err(Error::HostClosing);}
        if self.backend.preparation_observation()?.is_none() {
            return Ok(None);
        }
        let provider = CapacityProvider {
            manager: self,
            leases: entries.len() as u32,
            initial: None,
            package: None,
        };
        self.preparation.queue_recovery();
        self.preparation
            .recover(&provider, || {
                self.backend
                    .recover_capacity(&|| preparation::Provider::host_idle(&provider))
            })
            .map(Some)
            .map_err(|failure| Error::Platform(failure.message))
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, BTreeMap<LeaseId, Entry>>> {
        self.entries
            .lock()
            .map_err(|_| Error::Platform("display manager ownership state is poisoned".into()))
    }

    fn prepare_driver(&self) -> Result<()> {
        let _ownership = self.lock()?;
        if self.manual_closing.load(Ordering::Acquire) {
            return Err(Error::HostClosing);
        }
        if self.backend.preparation_observation()?.is_some() {
            let status = self
                .preparation
                .request(
                    &CapacityProvider {
                        manager: self,
                        leases: _ownership.len() as u32,
                        initial: None,
                        package: None,
                    },
                    u32::from(self.limit.load(Ordering::Acquire)).max(1),
                )
                .map_err(|failure| Error::Platform(failure.message))?;
            if status.phase == preparation::Phase::Ready {
                return Ok(());
            }
            return Err(Error::Preparation(status));
        }
        self.backend.recover()?;
        let inventory = self.backend.inventory()?;
        for target in inventory.targets {
            if target.available && !target.active && !self.backend.is_claimed(&target.key)? {
                return Ok(());
            }
        }
        self.backend.provision()
    }

    pub fn status(&self) -> Result<Status> {
        let entries = self.lock()?;
        let inventory = self.backend.inventory()?;
        let mut available_slots = 0;
        for target in &inventory.targets {
            if target.available
                && !target.active
                && !entries.values().any(|entry| entry.target == target.key)
                && !self.backend.is_claimed(&target.key)?
            {
                available_slots += 1;
            }
        }
        let explanation = if !inventory.supported {
            "Virtual displays are only supported on Windows."
        } else if !inventory.driver_present {
            "Creating a virtual display can prepare the pinned signed driver through an authorized management service. A foreground application without that service requires local administrator approval."
        } else if inventory.targets.is_empty() {
            "The driver has no unused connected target. Preparation reserves an inactive pool while the host is idle; existing streams and displays are preserved."
        } else if available_slots == 0 {
            "No unused virtual display slot is available. Additional capacity can be prepared when the host is idle. Active streams are never interrupted to reload a driver, and the initial reservation is not a session limit."
        } else {
            "Unused virtual display slots are available. Removing a lease disconnects only that target from the Windows desktop."
        };
        Ok(Status {
            supported: inventory.supported,
            driver_present: inventory.driver_present,
            available_slots,
            leased: entries.len(),
            allocated:self.allocated_count(&entries)?,
            keep_after_stream:self.keep_after_stream()?,
            limit:self.limit.load(Ordering::Acquire),
            supports_lifetimes:true,
            supports_configuration:true,
            explanation: explanation.into(),
            install_url: INSTALL_URL.into(),
            preparation: if self.backend.preparation_observation()?.is_some() {
                Some(
                    self.preparation
                        .inspect(&CapacityProvider {
                            manager: self,
                            leases: entries.len() as u32,
                            initial: None,
                            package: None,
                        })
                        .map_err(|failure| Error::Platform(failure.message))?,
                )
            } else {
                None
            },
        })
    }

    pub fn list(&self) -> Result<Vec<Display>> {
        let mut entries = self.lock()?;
        let inventory = self.backend.inventory()?;
        for entry in entries.values_mut() {
            let target = inventory
                .targets
                .iter()
                .find(|target| target.key == entry.target);
            entry.display.active = target.is_some_and(|target| target.active);
            entry.display.actual = target.and_then(|target| target.actual);
            if let Some(target) = target {
                entry.display.source.clone_from(&target.source);
            }
        }
        let mut result:Vec<_>=entries
            .values()
            .map(|entry| entry.display.clone())
            .collect();
        // A removed driver must not hide durable intent. These records can be
        // renamed, edited or deleted without recreating a driver unattended.
        for saved in self.saved_records()? {
            if !entries.contains_key(&LeaseId(saved.id)) {
                result.push(saved.disconnected());
            }
        }
        Ok(result)
    }

    pub fn slots(&self) -> Result<Vec<Slot>> {
        let entries = self.lock()?;
        self.backend
            .inventory()?
            .targets
            .into_iter()
            .map(|target| {
                let leased = entries.values().any(|entry| entry.target == target.key)
                    || self.backend.is_claimed(&target.key)?;
                Ok(Slot {
                    id: target.key.slot_id(),
                    name: target.name,
                    source: target.source,
                    active: target.active,
                    leased,
                    actual: target.actual,
                })
            })
            .collect()
    }

    /// Explicit host-local preparation: the user selects this exact existing
    /// virtual screen and agrees to disconnect it for future Eclipse leases.
    /// Never call automatically, and never expose via the streaming control
    /// channel. Remote `create` continues to require an inactive slot.
    pub fn prepare_slot(&self, id: &str) -> Result<()> {
        let entries = self.lock()?;
        if self.manual_closing.load(Ordering::Acquire) {
            return Err(Error::HostClosing);
        }
        self.backend.recover()?;
        let target = self
            .backend
            .inventory()?
            .targets
            .into_iter()
            .find(|target| {
                target.key.slot_id() == id
                    && !entries.values().any(|entry| entry.target == target.key)
            })
            .ok_or(Error::UnknownSlot)?;
        let _claim = self.backend.claim(&target.key)?;
        self.backend.prepare(&target.key)
    }

    pub fn create(&self, owner: Owner, spec: Spec) -> Result<Display> {
        self.create_with_context(owner, spec, None)
    }
    /// Policy limits new allocations, never removes existing or saved targets.
    pub fn set_limit(&self,limit:u16)->Result<()> {
        if limit>MAX_DISPLAY_LIMIT {return Err(Error::Platform("virtual display limit exceeds the native catalog budget".into()));}
        let _entries=self.lock()?;
        self.limit.store(limit,Ordering::Release);
        Ok(())
    }
    /// Remote management changes the same canonical service setting used by
    /// the host settings page, rather than an in-memory value that disappears
    /// when the background service restarts.
    pub fn set_limit_persisted(&self,limit:u16)->Result<()> {
        let directory=self.policy.lock().map_err(|_| Error::Platform("saved display policy is poisoned".into()))?.directory();
        if let Some(directory)=directory {
            crate::device_book::Store::open(directory).and_then(|store|store.edit(|book|{
                book.sharing.virtual_display_limit=limit;
                Ok(())
            })).map_err(|error|Error::Platform(error.to_string()))?;
        }
        self.set_limit(limit)
    }
    pub fn create_for_session(&self,owner:Uuid,spec:Spec,persistent:bool,context:Option<&str>)->Result<Display> {
        self.create_target(Owner::Session(owner),spec,context,None,Some(persistent))
    }
    pub fn create_with_context(
        &self,
        owner: Owner,
        spec: Spec,
        initial_session: Option<&str>,
    ) -> Result<Display> {
        self.create_target(owner,spec,initial_session,None,None)
    }
    fn create_restored(&self,saved:&policy::SavedDisplay)->Result<Display> {
        self.create_target(Owner::Manual,saved.spec,None,Some(saved),None)
    }
    fn create_target(&self,owner:Owner,spec:Spec,initial_session:Option<&str>,saved:Option<&policy::SavedDisplay>,retention:Option<bool>)->Result<Display> {
        spec.validate()?;
        let mut entries = self.lock()?;
        if owner == Owner::Manual && self.manual_closing.load(Ordering::Acquire) {
            return Err(Error::HostClosing);
        }
        if let Owner::Session(id) = owner {
            let sessions = self
                .sessions
                .lock()
                .map_err(|_| Error::Platform("session ownership state is poisoned".into()))?;
            if !sessions
                .get(&id)
                .and_then(Weak::upgrade)
                .is_some_and(|closed| !closed.load(Ordering::Acquire))
            {
                return Err(Error::SessionClosed);
            }
        }
        let limit=self.limit.load(Ordering::Acquire);
        if saved.is_none() && self.allocated_count(&entries)? >=usize::from(limit) {return Err(Error::LimitReached{limit});}
        let preparation_before = self.backend.preparation_observation()?;
        if self.preparation.recovery_pending()
            || preparation_before.is_some_and(|state| state.recovery_pending)
        {
            // Recovery is a separate explicit action. Create must not retire an
            // interrupted installation and silently start another one.
            let status = self
                .preparation
                .inspect(&CapacityProvider {
                    manager: self,
                    leases: entries.len() as u32,
                    initial: initial_session,
                    package: None,
                })
                .map_err(|failure| Error::Platform(failure.message))?;
            if status.phase==preparation::Phase::RecoveryPending {return Err(Error::Preparation(status));}
        }
        self.backend.recover()?;
        let capacity = self.backend.preparation_observation()?;
        // Reuse an already safe slot immediately, even during another stream.
        // Pool expansion is an independent idle-only intent, never a condition
        // for consuming existing capacity.
        let restored=if let Some(saved)=saved {
            let targets=self.backend.inventory()?.targets;
            let matching:Vec<_>=targets.into_iter().filter(|t|t.key.monitor_path==saved.target.monitor_path&&t.key.target==saved.target.target).collect();
            if matching.len()>1{return Err(Error::Platform("saved virtual target is ambiguous".into()));}
            if let Some(target)=matching.into_iter().next() {
                if !target.available || entries.values().any(|e|e.target==target.key){return Err(Error::Claimed);}
                let claim=self.backend.claim(&target.key)?;
                let target=self.backend.inventory()?.targets.into_iter().find(|t|t.key==target.key).ok_or(Error::UnknownSlot)?;
                Some((target,claim))
            }else{None}
        }else{None};
        let mut selected = if restored.is_some(){restored}else{match self.claim_available(&entries) {
            Err(Error::DriverMissing) if saved.is_some()=>return Err(Error::DriverMissing),
            Err(Error::DriverMissing) if capacity.is_some()=>None,
            result=>result?,
        }};
        if selected.is_none() {
            if let Some(capacity) = capacity {
                if saved.is_some()&&!capacity.driver_present{return Err(Error::DriverMissing);}
                // Reserve only the configured creation limit. Existing pools
                // are reused without growth; capacity is never a hidden batch.
                let minimum = u32::from(limit).max(1);
                let status = self
                    .preparation
                    .request(
                        &CapacityProvider {
                            manager: self,
                            leases: entries.len() as u32,
                            initial: initial_session,
                            package: None,
                        },
                        minimum,
                    )
                    .map_err(|failure| Error::Platform(failure.message))?;
                if status.phase != preparation::Phase::Ready {
                    return Err(Error::Preparation(status));
                }
                selected = self.claim_available(&entries)?;
            } else {
                self.backend.provision()?;
                selected = self.claim_available(&entries)?;
            }
        }
        let (target, claim) = selected.ok_or(Error::NoAvailableSlot)?;
        let id = LeaseId(saved.map_or_else(Uuid::new_v4,|s|s.id));
        let mut display = Display {
            id,
            owner,
            spec,
            actual: target.actual,
            name: saved.filter(|s|!s.name.is_empty()).map_or_else(||target.name.clone(),|s|s.name.clone()),
            source: target.source.clone(),
            active: false,
            persistent:saved.is_some(),
            cleanup_pending: false,
            last_error: None,
        };
        let activation=if saved.is_some()&&target.active {Ok(target.clone())}else{self.backend.activate(&target.key,spec)};
        match activation {
            Ok(observed) => {
                display.active = observed.active;
                display.actual = observed.actual;
                display.source = observed.source;
                if retention==Some(true) {
                    let record=policy::SavedDisplay{id:id.0,spec,target:target.key.clone(),name:display.name.clone()};
                    let saved=self.saved_file().and_then(|file|file.save(record));
                    if let Err(error)=saved {
                        if let Err(cleanup)=self.backend.deactivate(&target.key) {
                            display.cleanup_pending=true;
                            let message=format!("{error}; rollback: {cleanup}");
                            display.last_error=Some(message.clone());
                            entries.insert(id,Entry{_claim:claim,target:target.key,display,explicit_temporary:false});
                            return Err(Error::CleanupPending{id,message});
                        }
                        return Err(error);
                    }
                    display.persistent=true;
                }
                entries.insert(
                    id,
                    Entry {
                        _claim: claim,
                        target: target.key,
                        display: display.clone(),
                        explicit_temporary:retention==Some(false),
                    },
                );
                Ok(display)
            }
            Err(failure) => {
                if !failure.activated {
                    return Err(Error::Platform(failure.message));
                }
                match self.backend.deactivate(&target.key) {
                    Ok(()) => Err(Error::Platform(failure.message)),
                    Err(cleanup) => {
                        display.active = true;
                        display.cleanup_pending = true;
                        let message = format!("{}; rollback: {cleanup}", failure.message);
                        display.last_error = Some(message.clone());
                        entries.insert(
                            id,
                            Entry {
                                _claim: claim,
                                target: target.key,
                                display,
                                explicit_temporary:false,
                            },
                        );
                        Err(Error::CleanupPending { id, message })
                    }
                }
            }
        }
    }

    fn claim_available(
        &self,
        entries: &BTreeMap<LeaseId, Entry>,
    ) -> Result<Option<(Target, Box<dyn Claim>)>> {
        let inventory = self.backend.inventory()?;
        if !inventory.supported {
            return Err(Error::Unsupported);
        }
        if !inventory.driver_present {
            return Err(Error::DriverMissing);
        }
        for target in inventory.targets {
            if !target.available
                || target.active
                || entries.values().any(|entry| entry.target == target.key)
            {
                continue;
            }
            match self.backend.claim(&target.key) {
                Ok(claim) => return Ok(Some((target, claim))),
                // A different process may claim after our inventory. Try other
                // targets; errors establishing ownership still fail closed.
                Err(Error::Claimed) => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(None)
    }

    pub fn remove(&self, id: LeaseId) -> Result<()> {
        self.remove_owned(id, None)
    }

    /// The authenticated remote control path must use this ownership check.
    pub fn remove_session(&self, session_id: Uuid, id: LeaseId) -> Result<()> {
        self.remove_owned(id, Some(Owner::Session(session_id)))
    }

    fn remove_owned(&self, id: LeaseId, owner: Option<Owner>) -> Result<()> {
        let mut entries = self.lock()?;
        let Some(entry) = entries.get_mut(&id) else {
            if owner.is_none() && self.saved_records()?.iter().any(|saved|saved.id==id.0) {self.forget_saved(id)?;}
            return Ok(());
        };
        if owner.is_some_and(|owner| owner != entry.display.owner) {
            return Err(Error::WrongOwner);
        }
        if entry.display.persistent {self.forget_saved(id)?;entry.display.persistent=false;}
        match self.backend.deactivate(&entry.target) {
            Ok(()) => {
                entries.remove(&id);
                Ok(())
            }
            Err(error) => {
                let message = error.to_string();
                entry.display.cleanup_pending = true;
                entry.display.last_error = Some(message.clone());
                Err(Error::CleanupPending { id, message })
            }
        }
    }

    pub fn release_session(&self, session_id: Uuid) -> Result<()> {
        let mut persistence_error=None;
        let ids: Vec<_> = {
            let mut entries = self.lock()?;
            // Closing and activation share the ownership lock. An in-flight
            // create either completes before this snapshot or sees closed;
            // it cannot publish a new lease after disconnect cleanup.
            if let Some(closed) = self
                .sessions
                .lock()
                .map_err(|_| Error::Platform("session ownership state is poisoned".into()))?
                .get(&session_id)
                .and_then(Weak::upgrade)
            {
                closed.store(true, Ordering::Release);
            }
            let keep=self.keep_after_stream()?&&!self.manual_closing.load(Ordering::Acquire);
            let saved=if keep&&entries.values().any(|entry|entry.display.owner==Owner::Session(session_id)&&!entry.display.cleanup_pending) {match self.save_disconnected(&entries,session_id){Ok(())=>true,Err(error)=>{persistence_error=Some(error);false}}}else{false};
            for entry in entries.values_mut().filter(|e|e.display.owner==Owner::Session(session_id)) {
                if (entry.display.persistent||(saved&&!entry.explicit_temporary))&&!entry.display.cleanup_pending {
                    entry.display.owner=Owner::Manual;entry.display.persistent=true;
                }
            }
            entries
                .iter()
                .filter_map(|(id, entry)| {
                    (entry.display.owner == Owner::Session(session_id)).then_some(*id)
                })
                .collect()
        };
        let mut first_error = persistence_error;
        for id in ids {
            if let Err(error) = self.remove_session(session_id, id) {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Called after the host has stopped accepting local management work.
    /// Failed removals remain pending and can be retried before process exit.
    pub fn release_manual(&self) -> Result<()> {
        let ids: Vec<_> = self
            .lock()?
            .iter()
            .filter_map(|(id, entry)| (entry.display.owner == Owner::Manual).then_some(*id))
            .collect();
        let mut first_error = None;
        for id in ids {
            if let Err(error) = self.remove_owned(id, Some(Owner::Manual)) {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Final process shutdown: retire queued manual creates before cleanup.
    /// This is intentionally permanent for this Manager, unlike release_manual.
    pub fn shutdown_manual(&self) -> Result<()> {
        {
            let mut _ownership = self.lock()?;
            self.manual_closing.store(true, Ordering::Release);
            // Persisted targets intentionally outlive this process. A later
            // host must reacquire the recorded target under a fresh claim.
            _ownership.retain(|_,entry|entry.display.owner!=Owner::Manual||!entry.display.persistent);
        }
        self.release_manual()
    }

    pub fn session_guard(self: &Arc<Self>, session_id: Uuid) -> SessionGuard {
        let closed = match self.sessions.lock() {
            Ok(mut sessions) => {
                sessions.retain(|_, value| value.strong_count() > 0);
                if let Some(existing) = sessions.get(&session_id).and_then(Weak::upgrade) {
                    existing
                } else {
                    let closed = Arc::new(AtomicBool::new(false));
                    sessions.insert(session_id, Arc::downgrade(&closed));
                    closed
                }
            }
            Err(_) => Arc::new(AtomicBool::new(true)),
        };
        SessionGuard {
            manager: self.clone(),
            session_id,
            _closed: closed,
        }
    }
}

struct CapacityProvider<'a> {
    manager: &'a Manager,
    leases: u32,
    initial: Option<&'a str>,
    package: Option<&'a [(String, Vec<u8>)]>,
}
impl preparation::Provider for CapacityProvider<'_> {
    type Package = Vec<(String, Vec<u8>)>;
    fn observe(&self) -> preparation::Result<preparation::Observation> {
        let mut state = self
            .manager
            .backend
            .preparation_observation()
            .map_err(|error| preparation::Failure::unchanged(error.to_string()))?
            .ok_or_else(|| preparation::Failure::unchanged("driver preparation is unavailable"))?;
        state.authority &= self.manager.preparation_owner.load(Ordering::Acquire)
            || (self.package.is_some()
                && !state.driver_present
                && self.manager.fresh_install_admission().is_ok());
        state.claimed_targets = state.claimed_targets.max(self.leases);
        state.active_streams = state
            .active_streams
            .saturating_add(self.manager.preparation_streams(self.initial));
        Ok(state)
    }
    fn download_pinned(&self) -> preparation::Result<Self::Package> {
        match self.package {
            Some(package) => Ok(package.to_vec()),
            None => self.manager.backend.download_capacity_driver(),
        }
    }
    fn host_idle(&self) -> preparation::Result<()> {
        if self.package.is_some() && !self.manager.preparation_owner.load(Ordering::Acquire) {
            self.manager.fresh_install_admission()?;
        }
        if !(self.manager.preparation_owner.load(Ordering::Acquire) || self.package.is_some())
            || !self.manager.backend.preparation_authorized()
            || self.leases != 0
            || self.manager.preparation_streams(self.initial) != 0
        {
            return Err(preparation::Failure {
                message: "host authorization or stream state changed during driver preparation"
                    .into(),
                outcome: preparation::FailureOutcome::AdmissionChanged,
            });
        }
        Ok(())
    }
    fn apply(
        &self,
        minimum: u32,
        package: Option<&Self::Package>,
        checkpoint: &(dyn Fn() -> preparation::Result<()> + Sync),
    ) -> preparation::Result<()> {
        self.manager
            .backend
            .prepare_capacity(minimum, package.map(Vec::as_slice), checkpoint)
    }
}

/// Host connection owner, independent of video/capture restarts. The normal
/// shutdown should explicitly release on its blocking worker before Drop.
pub struct SessionGuard {
    manager: Arc<Manager>,
    session_id: Uuid,
    _closed: Arc<AtomicBool>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if let Err(error) = self.manager.release_session(self.session_id) {
            tracing::warn!(%error, "virtual display session cleanup remains pending");
        }
    }
}

#[cfg(not(windows))]
struct Unsupported;

#[cfg(not(windows))]
impl Backend for Unsupported {
    fn claim(&self, _: &TargetKey) -> Result<Box<dyn Claim>> {
        Err(Error::Unsupported)
    }
    fn is_claimed(&self, _: &TargetKey) -> Result<bool> {
        Err(Error::Unsupported)
    }
    fn inventory(&self) -> Result<Inventory> {
        Ok(Inventory {
            supported: false,
            driver_present: false,
            targets: Vec::new(),
        })
    }
    fn activate(&self, _: &TargetKey, _: Spec) -> std::result::Result<Target, ActivationFailure> {
        Err(ActivationFailure {
            message: Error::Unsupported.to_string(),
            activated: false,
        })
    }
    fn deactivate(&self, _: &TargetKey) -> Result<()> {
        Err(Error::Unsupported)
    }
    fn prepare(&self, _: &TargetKey) -> Result<()> {
        Err(Error::Unsupported)
    }
}

#[cfg(test)]
mod tests;
