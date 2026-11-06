//! Host-local desired displays. Durable intent is separate from live leases;
//! only a recorded virtual target may be reclaimed active after a restart.
use super::*;
use std::path::{Path, PathBuf};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SavedDisplay {
    pub id: Uuid,
    pub spec: Spec,
    pub target: TargetKey,
    #[serde(default,skip_serializing_if="String::is_empty")]
    pub name: String,
}
impl SavedDisplay {
    pub(super) fn disconnected(&self)->Display {
        Display{id:LeaseId(self.id),owner:Owner::Manual,spec:self.spec,actual:None,name:if self.name.is_empty(){uu_i18n::text("持久虚拟显示器").into()}else{self.name.clone()},source:String::new(),active:false,persistent:true,cleanup_pending:false,last_error:Some(uu_i18n::text("等待已保存的虚拟显示器恢复；请检查驱动及可用槽位").into())}
    }
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
        if self.displays.len()>usize::from(MAX_DISPLAY_LIMIT) {
            return Err(Error::Platform("saved display policy exceeds the native display catalog budget".into()));
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut targets = std::collections::BTreeSet::new();
        for display in &self.displays {
            display.spec.validate()?;
            validate_name(&display.name)?;
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
fn validate_name(name:&str)->Result<()> {
    if name.chars().count()>64 || name.chars().any(char::is_control) {return Err(platform(uu_i18n::text("虚拟显示器名称最多64字且不能包含控制字符")));}
    Ok(())
}
#[derive(Clone)]
pub(super) struct File {
    directory: PathBuf,
}
impl File {
    pub(super) fn save(&self,display:SavedDisplay)->Result<()> {
        self.edit(|policy|{policy.displays.retain(|d|d.id!=display.id);policy.displays.push(display);Ok(())})
    }
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

impl State {
    pub(super) fn directory(&self)->Option<PathBuf> {self.file.as_ref().map(|file|file.directory.clone())}
}
impl Manager {
    pub(super) fn saved_records(&self)->Result<Vec<SavedDisplay>> {
        let file=self.policy.lock().map_err(|_|platform("saved display policy is poisoned"))?.file.clone();
        file.map(|file|file.read().map(|policy|policy.displays)).unwrap_or_else(||Ok(Vec::new()))
    }
    pub(super) fn allocated_count(&self,entries:&BTreeMap<LeaseId,Entry>)->Result<usize> {
        Ok(entries.len()+self.saved_records()?.iter().filter(|saved|!entries.contains_key(&LeaseId(saved.id))).count())
    }
    pub fn configure_saved_displays(&self, directory: impl AsRef<Path>) -> Result<()> {
        let entries = self.lock()?;
        let file = File {
            directory: directory.as_ref().to_owned(),
        };
        let saved = file.read()?;
        let limit=crate::device_book::Store::open(&file.directory).and_then(|store|store.read()).map_err(platform)?.sharing.virtual_display_limit;
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
        self.limit.store(limit,Ordering::Release);
        Ok(())
    }
    pub(super) fn saved_file(&self) -> Result<File> {
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
        let Some(entry)=entries.get_mut(&id) else {
            if !persistent && self.saved_records()?.iter().any(|s|s.id==id.0) {return self.forget_saved(id);}
            return Err(Error::UnknownSlot);
        };
        let saved = SavedDisplay {
            id: id.0,
            spec: entry.display.spec,
            target: entry.target.clone(),
            name: entry.display.name.clone(),
        };
        self.saved_file()?.edit(|policy| {
            policy.displays.retain(|d| d.id != id.0);
            if persistent {
                policy.displays.push(saved);
            }
            Ok(())
        })?;
        entry.display.persistent = persistent;
        entry.explicit_temporary = !persistent;
        Ok(())
    }
    /// Names are metadata. Mode changes require an idle host because changing
    /// source geometry retires DXGI capture; session admission observes the
    /// editing flag before the activity snapshot, closing the check/apply race.
    pub fn configure_display(&self,id:LeaseId,name:String,spec:Spec)->Result<Display> {
        self.configure_display_inner(id,name,spec,false)
    }
    fn configure_display_inner(&self,id:LeaseId,name:String,spec:Spec,restore_mode:bool)->Result<Display> {
        spec.validate()?;
        let name=name.trim().to_owned();validate_name(&name)?;
        let mut entries=self.lock()?;
        if self.manual_closing.load(Ordering::Acquire) {return Err(Error::HostClosing);}
        let Some(entry)=entries.get_mut(&id) else {
            let mut saved=self.saved_records()?.into_iter().find(|s|s.id==id.0).ok_or(Error::UnknownSlot)?;
            saved.name=name;saved.spec=spec;
            self.saved_file()?.save(saved.clone())?;
            self.policy.lock().map_err(|_|platform("saved display policy is poisoned"))?.restore_pending=true;
            return Ok(saved.disconnected());
        };
        if entry.display.cleanup_pending {return Err(Error::CleanupPending{id,message:entry.display.last_error.clone().unwrap_or_default()});}
        let previous=entry.display.clone();
        let current=self.backend.inventory()?.targets.into_iter().find(|target|target.key==entry.target).ok_or(Error::UnknownSlot)?;
        // Actual HDR/mode may differ because of OS policy or an external mode
        // change. A name-only save must not turn that observation into a new
        // topology request, especially while this host is streaming.
        let mode_changed=previous.spec!=spec || (restore_mode&&current.actual!=Some(spec));
        let _edit=if mode_changed {
            if self.editing_mode.swap(true,Ordering::AcqRel) {return Err(platform("another virtual display edit is running"));}
            struct Edit<'a>(&'a AtomicBool);
            impl Drop for Edit<'_> {fn drop(&mut self){self.0.store(false,Ordering::Release);}}
            let guard=Edit(&self.editing_mode);
            if self.preparation_streams(None)!=0 {return Err(platform(uu_i18n::text("修改虚拟显示器规格前，请先结束此主机上的串流")));}
            Some(guard)
        }else{None};
        let observed=if mode_changed {
            let old=current.actual.ok_or_else(||platform("virtual display has no confirmed current mode"))?;
            match self.backend.reconfigure(&entry.target,spec) {
                Ok(target)=>Some(target),
                Err(failure)=>{
                    let mut message=failure.message;
                    if failure.activated {
                        if let Err(rollback)=self.backend.reconfigure(&entry.target,old) {message.push_str(&format!("; rollback: {}",rollback.message));}
                    }
                    entry.display.last_error=Some(message.clone());
                    return Err(platform(message));
                }
            }
        }else{None};
        let next_name=if name.is_empty(){current.name}else{name};
        if previous.persistent {
            let record=SavedDisplay{id:id.0,spec,target:entry.target.clone(),name:next_name.clone()};
            let file=self.saved_file()?;
            if let Err(error)=file.save(record.clone()) {
                // Atomic replacement may have completed before an IO error.
                // Confirm the exact record before rolling hardware back.
                if !file.read().is_ok_and(|p|p.displays.iter().any(|d|d==&record)) {
                    let mut message=error.to_string();
                    if mode_changed {
                        if let Err(rollback)=self.backend.reconfigure(&entry.target,current.actual.unwrap()) {message.push_str(&format!("; rollback: {}",rollback.message));}
                    }
                    entry.display.last_error=Some(message.clone());
                    return Err(platform(message));
                }
            }
        }
        entry.display.name=next_name;
        entry.display.spec=spec;
        entry.display.last_error=None;
        if let Some(target)=observed {
            entry.display.actual=target.actual;entry.display.active=target.active;entry.display.source=target.source;
        } else {
            entry.display.actual=current.actual;entry.display.active=current.active;entry.display.source=current.source;
        }
        Ok(entry.display.clone())
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
                e.display.owner == Owner::Session(session) && !e.display.cleanup_pending && !e.explicit_temporary
            }) {
                policy.displays.retain(|d| d.id != id.0);
                policy.displays.push(SavedDisplay {
                    id: id.0,
                    spec: entry.display.spec,
                    target: entry.target.clone(),
                    name: entry.display.name.clone(),
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
            if display.actual!=Some(saved.spec) {
                // Startup explicitly reconciles the exact saved target's mode;
                // ordinary metadata edits never imply that recovery action.
                self.configure_display_inner(display.id,display.name.clone(),saved.spec,true)?;
            }
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
        let mut ids:std::collections::BTreeSet<_>=self.lock()?.keys().copied().collect();
        ids.extend(self.saved_records()?.into_iter().map(|saved|LeaseId(saved.id)));
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
