//! Host-local desired displays. Durable intent is separate from live leases;
//! only a recorded virtual target may be reclaimed active after a restart.
use super::*;
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SavedDisplay {
    pub id: Uuid,
    pub spec: Spec,
    pub target: TargetKey,
}
#[derive(Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    version: u8,
    keep_after_stream: bool,
    displays: Vec<SavedDisplay>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            keep_after_stream: false,
            displays: Vec::new(),
        }
    }
}
impl Settings {
    fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(Error::Platform("unsupported saved display policy".into()));
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut targets = std::collections::BTreeSet::new();
        for display in &self.displays {
            display.spec.validate()?;
            if !ids.insert(display.id)
                || !targets.insert((&display.target.monitor_path, display.target.target))
                || display.target.monitor_path.is_empty()
                || display.target.monitor_path.len() > 2048
                || display.target.monitor_path.chars().any(char::is_control)
            {
                return Err(Error::Platform(
                    "saved display identities are invalid or duplicated".into(),
                ));
            }
        }
        Ok(())
    }
}
#[derive(Clone)]
struct File {
    directory: PathBuf,
}
impl File {
    fn read(&self) -> Result<Settings> {
        let path = self.directory.join("virtual-displays.json");
        if !path.try_exists().map_err(platform)? {
            return Ok(Settings::default());
        }
        let data = crate::device_trust::read_private(&path).map_err(platform)?;
        let value: Settings = serde_json::from_slice(&data).map_err(platform)?;
        value.validate()?;
        Ok(value)
    }
    fn edit(&self, change: impl FnOnce(&mut Settings) -> Result<()>) -> Result<()> {
        let _lock = crate::device_trust::TrustStore::lock_at(&self.directory).map_err(platform)?;
        let mut value = self.read()?;
        change(&mut value)?;
        value.validate()?;
        let bytes = serde_json::to_vec_pretty(&value).map_err(platform)?;
        if bytes.len() > 1024 * 1024 {
            return Err(Error::Platform("saved display policy exceeds 1 MiB".into()));
        }
        crate::private_file::write_atomic(&self.directory.join("virtual-displays.json"), &bytes)
            .map_err(platform)
    }
}
fn platform(error: impl std::fmt::Display) -> Error {
    Error::Platform(error.to_string())
}
#[derive(Default)]
pub(super) struct State {
    file: Option<File>,
    keep_after_stream: bool,
    restore_pending: bool,
    waiting_for_capacity: bool,
}

impl Manager {
    pub fn configure_saved_displays(&self, directory: impl AsRef<Path>) -> Result<()> {
        let entries = self.lock()?;
        let file = File {
            directory: directory.as_ref().to_owned(),
        };
        let saved = file.read()?;
        let mut state = self
            .policy
            .lock()
            .map_err(|_| platform("saved display policy is poisoned"))?;
        if state
            .file
            .as_ref()
            .is_some_and(|old| old.directory != file.directory)
            && !entries.is_empty()
        {
            return Err(platform(
                "cannot switch saved-display owner while targets are held",
            ));
        }
        state.file = Some(file);
        state.keep_after_stream = saved.keep_after_stream;
        state.restore_pending = !saved.displays.is_empty();
        state.waiting_for_capacity = false;
        Ok(())
    }
    fn saved_file(&self) -> Result<File> {
        let mut state = self
            .policy
            .lock()
            .map_err(|_| platform("saved display policy is poisoned"))?;
        if state.file.is_none() {
            let file = File {
                directory: crate::Config::from_env()
                    .map_err(platform)?
                    .at("device-trust"),
            };
            let saved = file.read()?;
            state.keep_after_stream = saved.keep_after_stream;
            state.restore_pending = !saved.displays.is_empty();
            state.file = Some(file);
        }
        Ok(state.file.as_ref().unwrap().clone())
    }
    pub fn keep_after_stream(&self) -> Result<bool> {
        self.policy
            .lock()
            .map(|s| s.keep_after_stream)
            .map_err(|_| platform("saved display policy is poisoned"))
    }
    pub fn set_keep_after_stream(&self, keep: bool) -> Result<()> {
        let _entries = self.lock()?;
        if self.manual_closing.load(Ordering::Acquire) {
            return Err(Error::HostClosing);
        }
        self.saved_file()?.edit(|policy| {
            policy.keep_after_stream = keep;
            Ok(())
        })?;
        self.policy
            .lock()
            .map_err(|_| platform("saved display policy is poisoned"))?
            .keep_after_stream = keep;
        Ok(())
    }
    pub fn toggle_keep_after_stream(&self) -> Result<()> {
        let _entries = self.lock()?;
        if self.manual_closing.load(Ordering::Acquire) {
            return Err(Error::HostClosing);
        }
        let mut keep = false;
        self.saved_file()?.edit(|policy| {
            policy.keep_after_stream = !policy.keep_after_stream;
            keep = policy.keep_after_stream;
            Ok(())
        })?;
        self.policy
            .lock()
            .map_err(|_| platform("saved display policy is poisoned"))?
            .keep_after_stream = keep;
        Ok(())
    }
    pub fn set_persistent(&self, id: LeaseId, persistent: bool) -> Result<()> {
        let mut entries = self.lock()?;
        let entry = entries.get_mut(&id).ok_or(Error::UnknownSlot)?;
        let saved = SavedDisplay {
            id: id.0,
            spec: entry.display.spec,
            target: entry.target.clone(),
        };
        self.saved_file()?.edit(|policy| {
            policy.displays.retain(|d| d.id != id.0);
            if persistent {
                policy.displays.push(saved);
            }
            Ok(())
        })?;
        entry.display.persistent = persistent;
        Ok(())
    }
    pub fn create_manual(&self, spec: Spec, persistent: bool) -> Result<Display> {
        let mut display = self.create(Owner::Manual, spec)?;
        if persistent {
            if let Err(error) = self.set_persistent(display.id, true) {
                self.remove(display.id)?;
                return Err(error);
            }
            display.persistent = true;
        }
        Ok(display)
    }
    pub(super) fn forget_saved(&self, id: LeaseId) -> Result<()> {
        self.saved_file()?.edit(|policy| {
            policy.displays.retain(|d| d.id != id.0);
            Ok(())
        })
    }
    pub(super) fn save_disconnected(
        &self,
        entries: &BTreeMap<LeaseId, Entry>,
        session: Uuid,
    ) -> Result<()> {
        self.saved_file()?.edit(|policy| {
            for (id, entry) in entries.iter().filter(|(_, e)| {
                e.display.owner == Owner::Session(session) && !e.display.cleanup_pending
            }) {
                policy.displays.retain(|d| d.id != id.0);
                policy.displays.push(SavedDisplay {
                    id: id.0,
                    spec: entry.display.spec,
                    target: entry.target.clone(),
                });
            }
            Ok(())
        })
    }
    pub fn saved_restore_pending(&self) -> bool {
        self.policy.lock().is_ok_and(|state| {
            state.restore_pending && (!state.waiting_for_capacity || !self.has_queued_preparation())
        })
    }
    /// Startup restores installed targets only. Removing a driver is not
    /// silently undone by an unattended download/reinstallation.
    pub fn restore_saved_displays(&self) -> Result<()> {
        let file = self.saved_file()?;
        let policy = file.read()?;
        self.policy
            .lock()
            .map_err(|_| platform("saved display policy is poisoned"))?
            .restore_pending = false;
        for saved in policy.displays {
            if self.lock()?.contains_key(&LeaseId(saved.id)) {
                continue;
            }
            let display = match self.create_restored(&saved) {
                Ok(display) => display,
                Err(error) => {
                    if self.has_queued_preparation() || matches!(error, Error::Claimed) {
                        let mut state = self
                            .policy
                            .lock()
                            .map_err(|_| platform("saved display policy is poisoned"))?;
                        state.restore_pending = true;
                        state.waiting_for_capacity = !matches!(error, Error::Claimed);
                    }
                    return Err(error);
                }
            };
            let target = self
                .lock()?
                .get(&display.id)
                .ok_or(Error::UnknownSlot)?
                .target
                .clone();
            file.edit(|policy| {
                let record = policy
                    .displays
                    .iter_mut()
                    .find(|r| r.id == saved.id)
                    .ok_or(Error::UnknownSlot)?;
                record.target = target;
                Ok(())
            })?;
        }
        Ok(())
    }
    pub fn close_all(&self) -> Result<()> {
        let ids: Vec<_> = self.lock()?.keys().copied().collect();
        let mut first = None;
        for id in ids {
            if let Err(error) = self.remove(id) {
                if first.is_none() {
                    first = Some(error);
                }
            }
        }
        if let Some(error) = first {
            return Err(error);
        }
        Ok(())
    }
    pub fn reset_displays(&self) -> Result<()> {
        self.close_all()?;
        if self
            .preparation_status()?
            .is_some_and(|s| s.phase == preparation::Phase::RecoveryPending)
        {
            if let Some(status) = self.recover_preparation()? {
                if matches!(
                    status.phase,
                    preparation::Phase::RecoveryPending
                        | preparation::Phase::Failed
                        | preparation::Phase::Preparing
                ) {
                    return Err(Error::Preparation(status));
                }
            }
        }
        let _entries = self.lock()?;
        self.saved_file()?.edit(|policy| {
            *policy = Settings::default();
            Ok(())
        })?;
        let mut state = self
            .policy
            .lock()
            .map_err(|_| platform("saved display policy is poisoned"))?;
        state.keep_after_stream = false;
        state.restore_pending = false;
        state.waiting_for_capacity = false;
        Ok(())
    }
}
