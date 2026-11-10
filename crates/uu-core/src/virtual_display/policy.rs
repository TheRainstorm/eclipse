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
    #[serde(default="default_true")]
    pub enabled:bool,
    #[serde(default="default_true")]
    pub persistent:bool,
    #[serde(default="default_true")]
    pub restore_layout:bool,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub position:Option<Position>,
}
impl SavedDisplay {
    pub(super) fn disconnected(&self)->Display {
        Display{id:LeaseId(self.id),owner:Owner::Manual,spec:self.spec,actual:None,name:self.name.clone(),source:String::new(),active:false,persistent:self.persistent,enabled:self.enabled,restore_layout:self.restore_layout,cleanup_pending:false,last_error:self.enabled.then(||uu_i18n::text("已请求启用，但尚未连接到 Windows；请重试启用或查看诊断详情。").into())}
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
        if !matches!(self.version,1|2) {
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
                || (display.enabled && !display.target.monitor_path.is_empty() && !targets.insert((&display.target.monitor_path, display.target.target)))
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
        self.edit(|policy|{if let Some(old)=policy.displays.iter_mut().find(|d|d.id==display.id) {*old=display;}else{policy.displays.push(display);}Ok(())})
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
        value.version=2;
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
    pub(super) fn ensure_catalog(&self)->Result<()> {
        if self.backend.on_demand()&&self.policy.lock().map_err(|_|platform("saved display policy is poisoned"))?.file.is_none() {
            let directory=crate::Config::from_env().map_err(platform)?.at("device-trust");
            self.configure_saved_displays(directory)?;
        }
        Ok(())
    }
    /// Adding a definition is metadata only. No driver, target or claim is reserved.
    pub fn add_display(&self)->Result<Display> {
        self.ensure_catalog()?;
        let entries=self.lock()?;
        if self.manual_closing.load(Ordering::Acquire) {return Err(Error::HostClosing);}
        if self.saved_records()?.len()>=usize::from(MAX_DISPLAY_LIMIT) {return Err(Error::LimitReached{limit:MAX_DISPLAY_LIMIT});}
        let record=SavedDisplay{id:Uuid::new_v4(),spec:Spec::default(),name:self.next_display_name(&entries)?,target:TargetKey{adapter:(0,0),target:0,monitor_path:String::new()},enabled:false,persistent:true,restore_layout:true,position:None};
        self.saved_file()?.save(record.clone())?;
        Ok(record.disconnected())
    }
    pub fn set_enabled(&self,id:LeaseId,enabled:bool)->Result<Display> {
        if !enabled {
            self.remove_owned(id,None,false)?;
        } else {
            let record={
                let mut entries=self.lock()?;
                let record=self.saved_records()?.into_iter().find(|r|r.id==id.0);
                if let Some(entry)=entries.get(&id) {
                    if entry.display.cleanup_pending {return Err(Error::CleanupPending{id,message:entry.display.last_error.clone().unwrap_or_default()});}
                    let current=self.backend.inventory()?.targets.into_iter().find(|t|t.key==entry.target).ok_or(Error::UnknownSlot)?;
                    if current.active {return Ok(entry.display.clone());}
                    let existing:Vec<_>=entries.values().map(|e|(e.target.clone(),e.display.spec)).collect();
                    let position=record.as_ref().filter(|r|r.restore_layout).and_then(|r|r.position);
                    let target=self.backend.reconnect(&entry.target,entry.display.spec,position,&existing)?;
                    let entry=entries.get_mut(&id).unwrap();
                    entry.display.active=target.active;entry.display.actual=target.actual;entry.display.source=target.source;
                    return Ok(entry.display.clone());
                }
                self.saved_records()?.into_iter().find(|r|r.id==id.0).ok_or(Error::UnknownSlot)?
            };
            self.create_restored(&record)?;
        }
        self.list()?.into_iter().find(|d|d.id==id).ok_or(Error::UnknownSlot)
    }
    pub fn set_restore_layout(&self,id:LeaseId,restore:bool)->Result<()> {
        let mut entries=self.lock()?;
        self.saved_file()?.edit(|policy| {
            policy.displays.iter_mut().find(|d|d.id==id.0).ok_or(Error::UnknownSlot)?.restore_layout=restore;
            Ok(())
        })?;
        if let Some(entry)=entries.get_mut(&id) {entry.display.restore_layout=restore;}
        Ok(())
    }
    pub(super) fn mark_disabled(&self,id:LeaseId,position:Option<Position>)->Result<()> {
        if self.policy.lock().map_err(|_|platform("saved display policy is poisoned"))?.file.is_none() {return Ok(());}
        self.saved_file()?.edit(|policy| {
            if let Some(record)=policy.displays.iter_mut().find(|d|d.id==id.0) {
                record.enabled=false;
                if position.is_some() {record.position=position;}
            }
            Ok(())
        })
    }
    pub(super) fn remember_position(&self,id:LeaseId,position:Option<Position>)->Result<()> {
        if position.is_none() {return Ok(());}
        if self.policy.lock().map_err(|_|platform("saved display policy is poisoned"))?.file.is_none() {return Ok(());}
        self.saved_file()?.edit(|policy| {
            if let Some(record)=policy.displays.iter_mut().find(|d|d.id==id.0) {record.position=position;}
            Ok(())
        })
    }
    pub(super) fn saved_records(&self)->Result<Vec<SavedDisplay>> {
        let file=self.policy.lock().map_err(|_|platform("saved display policy is poisoned"))?.file.clone();
        file.map(|file|file.read().map(|policy|policy.displays)).unwrap_or_else(||Ok(Vec::new()))
    }
    pub(super) fn allocated_count(&self,entries:&BTreeMap<LeaseId,Entry>)->Result<usize> {
        Ok(Self::allocated_count_from_records(entries, &self.saved_records()?))
    }
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
        if state.file.as_ref().is_none_or(|old|old.directory!=file.directory) {
            self.backend.configure_store(&file.directory,&saved.displays.iter().filter(|d|!d.target.monitor_path.is_empty()).map(|d|d.target.clone()).collect::<Vec<_>>())?;
            // A temporary screen never reconnects just because the host starts.
            file.edit(|policy|{for display in &mut policy.displays {if !display.persistent {display.enabled=false;}}Ok(())})?;
        }
        state.file = Some(file);
        state.keep_after_stream = saved.keep_after_stream;
        state.restore_pending = !saved.displays.is_empty();
        state.waiting_for_capacity = false;
        self.limit.store(MAX_DISPLAY_LIMIT,Ordering::Release);
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
        let file=self.saved_file()?;
        file.edit(|policy| {
            let saved=policy.displays.iter_mut().find(|d|d.id==id.0).ok_or(Error::UnknownSlot)?;
            saved.persistent=persistent;
            Ok(())
        })?;
        let Some(entry)=entries.get_mut(&id) else {return Ok(());};
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
        if name.is_empty() {return Err(platform(uu_i18n::text("请填写虚拟显示器名称")));}
        let mut entries=self.lock()?;
        if self.manual_closing.load(Ordering::Acquire) {return Err(Error::HostClosing);}
        let Some(entry)=entries.get_mut(&id) else {
            let mut saved=self.saved_records()?.into_iter().find(|s|s.id==id.0).ok_or(Error::UnknownSlot)?;
            saved.name=name;saved.spec=spec;
            self.saved_file()?.save(saved.clone())?;
            if saved.enabled {self.policy.lock().map_err(|_|platform("saved display policy is poisoned"))?.restore_pending=true;}
            return Ok(saved.disconnected());
        };
        if entry.display.cleanup_pending {return Err(Error::CleanupPending{id,message:entry.display.last_error.clone().unwrap_or_default()});}
        let saved_record=self.saved_records()?.into_iter().find(|r|r.id==id.0);
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
        self.backend.rename(&entry.target,&next_name)?;
        if let Some(mut record)=saved_record {
            record.spec=spec;record.name=next_name.clone();
            if let Some(position)=self.backend.position(&entry.target)? {record.position=Some(position);}
            if let Err(error)=self.saved_file()?.save(record) {
                if mode_changed {let _=self.backend.reconfigure(&entry.target,current.actual.unwrap());}
                return Err(error);
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
        if self.policy.lock().map_err(|_|platform("saved display policy is poisoned"))?.file.is_none() {return Ok(());}
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
                    enabled:true,persistent:true,restore_layout:entry.display.restore_layout,position:self.backend.position(&entry.target)?,
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
        for saved in policy.displays.into_iter().filter(|d|d.enabled&&d.persistent) {
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
            if let Err(error) = self.remove_owned(id,None,false) {
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
