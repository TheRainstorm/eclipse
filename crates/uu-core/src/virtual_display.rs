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

#[cfg(windows)]
mod install_windows;
#[cfg(windows)]
mod ownership_windows;
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
            let _ownership = manager.lock()?;
            install_windows::install(package_directory)?;
        }
        manager.status()
    }
    #[cfg(not(windows))]
    {
        let _ = package_directory;
        Err(Error::Unsupported)
    }
}

/// Prepare an installed driver explicitly from an elevated source-machine UI.
/// Background hosts use the same provisioning path automatically on creation.
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
    pub(crate) fn validate(self) -> Result<()> {
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
    pub leased: usize,
    pub explanation: String,
    pub install_url: String,
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
    #[error("virtual display operation failed: {0}")]
    Platform(String),
    #[error("administrator permission is required to prepare the virtual display driver")]
    AdministratorRequired,
    #[error("virtual display {id} still needs cleanup: {message}")]
    CleanupPending { id: LeaseId, message: String },
}

pub type Result<T> = std::result::Result<T, Error>;

// Full target identity remains host-private. In particular, a remote caller
// cannot turn an arbitrary adapter/target number into a cleanup capability.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
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
    /// True only after our SetDisplayConfig APPLY succeeded. A validation or
    /// precondition failure never grants permission to deactivate a target.
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
    fn claim(&self, key: &TargetKey) -> Result<Box<dyn Claim>>;
    fn is_claimed(&self, key: &TargetKey) -> Result<bool>;
    /// Supply an unused target without adopting or reconfiguring existing output.
    /// Success means it can be freshly enumerated; failures must own their rollback.
    fn provision(&self) -> Result<()> {
        Err(Error::NoAvailableSlot)
    }
    fn inventory(&self) -> Result<Inventory>;
    fn activate(
        &self,
        key: &TargetKey,
        spec: Spec,
    ) -> std::result::Result<Target, ActivationFailure>;
    fn deactivate(&self, key: &TargetKey) -> Result<()>;
    fn prepare(&self, key: &TargetKey) -> Result<()>;
}

struct Entry {
    _claim: Box<dyn Claim>,
    target: TargetKey,
    display: Display,
}

/// One process-wide owner for manual and authenticated-session display leases.
/// All requests and readbacks are serialized, including failure cleanup.
pub struct Manager {
    backend: Box<dyn Backend>,
    entries: Mutex<BTreeMap<LeaseId, Entry>>,
    sessions: Mutex<BTreeMap<Uuid, Weak<AtomicBool>>>,
    manual_closing: AtomicBool,
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
                })
            })
            .clone()
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
            "Install the signed VirtualDrivers driver explicitly as administrator; installation is never automatic."
        } else if inventory.targets.is_empty() {
            "A supported driver is installed but has no connected monitor target. Creating a display can prepare a disabled Mtt adapter that has no active output; foreground preparation requires administrator permission."
        } else if available_slots == 0 {
            "No unused connected virtual display slot is available. Creating a display attempts to prepare an unused disabled Mtt adapter. Capacity is limited to the driver's configured targets; active adapters are not reloaded and existing active displays are not taken over."
        } else {
            "Unused virtual display slots are available. Removing a lease disconnects only that target from the Windows desktop."
        };
        Ok(Status {
            supported: inventory.supported,
            driver_present: inventory.driver_present,
            available_slots,
            leased: entries.len(),
            explanation: explanation.into(),
            install_url: INSTALL_URL.into(),
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
        Ok(entries
            .values()
            .map(|entry| entry.display.clone())
            .collect())
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
        self.backend.recover()?;
        let mut selected = self.claim_available(&entries)?;
        if selected.is_none() {
            self.backend.provision()?;
            selected = self.claim_available(&entries)?;
        }
        let (target, claim) = selected.ok_or(Error::NoAvailableSlot)?;
        let id = LeaseId(Uuid::new_v4());
        let mut display = Display {
            id,
            owner,
            spec,
            actual: target.actual,
            name: target.name,
            source: target.source,
            active: false,
            cleanup_pending: false,
            last_error: None,
        };
        match self.backend.activate(&target.key, spec) {
            Ok(observed) => {
                display.active = observed.active;
                display.actual = observed.actual;
                display.source = observed.source;
                entries.insert(
                    id,
                    Entry {
                        _claim: claim,
                        target: target.key,
                        display: display.clone(),
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
            return Ok(());
        };
        if owner.is_some_and(|owner| owner != entry.display.owner) {
            return Err(Error::WrongOwner);
        }
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
        let ids: Vec<_> = {
            let entries = self.lock()?;
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
            entries
                .iter()
                .filter_map(|(id, entry)| {
                    (entry.display.owner == Owner::Session(session_id)).then_some(*id)
                })
                .collect()
        };
        let mut first_error = None;
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
            let _ownership = self.lock()?;
            self.manual_closing.store(true, Ordering::Release);
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
