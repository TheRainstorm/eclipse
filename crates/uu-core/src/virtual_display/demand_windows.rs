//! One monitor per independently hosted, signed Mtt device. No target pool is
//! published, and deleting a lease uninstalls only its recorded PnP instance.
//! All entry points run under the global display lifecycle mutex.
use super::*;
use crate::virtual_display::{capacity_config, install_windows as driver};
use provision::capacity::{self, store};
use std::{
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    directory: Option<PathBuf>,
    records: Vec<Record>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    scope: String,
    lease: Uuid,
    token: Uuid,
    instance: Option<String>,
    target: Option<TargetKey>,
    ready: bool,
}
fn fail(message: impl Into<String>) -> Error {
    Error::Platform(message.into())
}
fn path() -> Result<PathBuf> {
    Ok(store::root()?.join("on-demand.json"))
}
fn load() -> Result<State> {
    let path = path()?;
    if !path.try_exists().map_err(|e| fail(e.to_string()))? {
        return Ok(State::default());
    }
    let state: State =
        serde_json::from_slice(&store::read(&path)?).map_err(|e| fail(e.to_string()))?;
    if state.records.len() > 128
        || state
            .records
            .iter()
            .any(|r| !crate::service_profile::valid_id(&r.scope))
    {
        return Err(fail("invalid virtual device ownership inventory"));
    }
    let mut tokens = HashSet::new();
    let mut identities = HashSet::new();
    for record in &state.records {
        if !tokens.insert(record.token) || !identities.insert((record.scope.as_str(), record.lease))
        {
            return Err(fail("duplicated virtual device ownership identity"));
        }
    }
    Ok(state)
}
fn save(state: &State) -> Result<()> {
    crate::private_file::write_atomic(
        &path()?,
        &serde_json::to_vec_pretty(state).map_err(|e| fail(e.to_string()))?,
    )
    .map_err(|e| fail(e.to_string()))
}
fn verify_package(directory: &Path) -> Result<()> {
    for expected in &driver_package().files {
        let bytes = store::read(&directory.join(&expected.name))?;
        if expected.name == "vdd_settings.xml" {
            let original = capacity_config::exact_count(&bytes, 1)?;
            if original.original_count != 1 {
                return Err(fail(
                    "on-demand driver configuration no longer specifies one monitor",
                ));
            }
        } else if bytes.len() != expected.bytes
            || hex::encode(Sha256::digest(&bytes)) != expected.sha256
        {
            return Err(fail(
                "prepared driver differs from the original signed package",
            ));
        }
    }
    Ok(())
}
pub(super) fn prepared() -> Result<bool> {
    let state = load()?;
    match state.directory {
        Some(directory) => {
            verify_package(&directory)?;
            Ok(true)
        }
        None => Ok(false),
    }
}
pub(super) fn recover_uninstalled_legacy() -> Result<()> {
    if capacity::recovery_pending()? {
        driver::no_existing_mtt_node().map_err(|_| {
            fail("旧版驱动操作尚未完成，关联设备已保留；请先在源机核对该操作后重试。")
        })?;
        // A proven absent adapter needs no stream interruption: the legacy
        // recovery verifies its original package and unchanged live desktop,
        // retires only the exact stale value and archives its journal.
        capacity::recover(&|| Ok(())).map_err(|error| fail(error.message))?;
    }
    Ok(())
}
pub(super) fn prepare(package: &[(String, Vec<u8>)]) -> Result<()> {
    crate::service_admin::windows::require_administrator()
        .map_err(|_| Error::AdministratorRequired)?;
    let mut state = load()?;
    if let Some(directory) = &state.directory {
        verify_package(directory)?;
        return Ok(());
    }
    // Stage, verify and retain the signed package; publish no PnP device.
    let directory = driver::stage_demand_package(package)?;
    state.directory = Some(directory);
    save(&state)
}
pub(super) fn retire_idle_pool() -> Result<()> {
    let Some(directory) = driver::current_configuration()? else {
        return Ok(());
    };
    if load()?.directory.as_deref() == Some(&directory) {
        return Ok(());
    }
    if capacity::legacy_owned(&directory)?.is_empty() {
        return Ok(());
    }
    capacity::retire_legacy_pool(&directory, &BTreeSet::new())?;
    driver::retire_uninstalled_configuration(&directory)
}
fn recover_record(record: &Record) -> Result<()> {
    let token = record.token.to_string();
    let instance = match &record.instance {
        Some(instance) => Some(instance.clone()),
        None => driver::find_unbound_demand(&token)?,
    };
    if let Some(instance) = instance {
        driver::remove_demand_device(&instance, &token, record.ready)?;
    }
    if let Some(base)=load()?.directory {driver::remove_monitor_configuration(&base,record.token)?;}
    Ok(())
}
pub(super) fn reconcile_owner(scope: &str) -> Result<()> {
    let mut state = load()?;
    let records: Vec<_> = state
        .records
        .iter()
        .filter(|r| r.scope == scope)
        .cloned()
        .collect();
    for record in records {
        let _claim = record
            .target
            .as_ref()
            .map(|key| super::super::ownership_windows::claim(key))
            .transpose()?;
        recover_record(&record)?;
        state.records.retain(|r| r.token != record.token);
        save(&state)?;
    }
    Ok(())
}
/// Retire only the old Eclipse-created pool. External adapters have no matching
/// first-install receipt. Active/claimed outputs wait for their owning host.
fn select_configuration(
    state: &State,
    directory: &Path,
    saved: &BTreeSet<TargetKey>,
) -> Result<()> {
    let current = driver::current_configuration()?;
    if current.as_deref() == Some(directory) {
        return driver::demand_configuration(directory);
    }
    if let Some(current) = current {
        let owned = capacity::legacy_owned(&current)?;
        let installed = driver::demand_instances()?;
        if installed.iter().any(|id| !owned.contains(id)) {
            return Err(fail(
                "检测到其它安装管理的 Mtt 虚拟显示器，未修改它的配置；请先在原管理程序中移除。",
            ));
        }
        if !installed.is_empty() {
            capacity::retire_legacy_pool(&current, saved)?;
        }
        if owned.is_empty() && state.directory.as_deref() != Some(&current) {
            return Err(fail("已有 Mtt 配置不属于 Eclipse，未覆盖该配置。"));
        }
        // Device absence includes phantom nodes; compare the exact registry
        // value again before clearing only the stale, proven-owned pathname.
        driver::retire_uninstalled_configuration(&current)?;
    } else {
        let owned: BTreeSet<_> = state
            .records
            .iter()
            .filter_map(|r| r.instance.as_ref())
            .collect();
        if driver::demand_instances()?
            .iter()
            .any(|instance| !owned.contains(instance))
        {
            return Err(fail("已有 Mtt 适配器缺少可确认的配置归属，未接管该设备。"));
        }
    }
    driver::demand_configuration(directory)
}
pub(super) fn create_configured(scope:&str,id:LeaseId,saved:&BTreeSet<TargetKey>,name:&str,spec:Spec,position:Option<Position>,existing:&[(TargetKey,Spec)])->Result<(Target,Box<dyn Claim>)> {
    let before=snapshot()?;
    let mut desired=owned_placements(&before.scene.active,existing)?;
    let (target,claim)=create_named(scope,id,saved,name,spec)?;
    desired.push((target.key.clone(),spec,position));
    match configure_owned_set(&desired,Some(&before),None) {
        Ok(())=>Ok((inventory(Some(scope))?.targets.into_iter().find(|t|t.key==target.key&&t.active).ok_or(Error::UnknownSlot)?,claim)),
        Err(error)=>{
            mark_pending(&target.key)?;
            match remove_with_context(&target.key,Some((&before,&desired[..desired.len()-1]))) {Ok(())=>Err(error),Err(cleanup)=>Err(fail(format!("{error}; cleanup: {cleanup}")))}
        }
    }
}
pub(super) fn create_named(scope:&str,id:LeaseId,saved:&BTreeSet<TargetKey>,name:&str,spec:Spec)->Result<(Target,Box<dyn Claim>)> {
    crate::service_admin::windows::require_administrator()
        .map_err(|_| Error::AdministratorRequired)?;
    recover_uninstalled_legacy()?;
    let mut state = load()?;
    if state.records.iter().any(|r| r.scope == scope && !r.ready) {
        // A failed submission is recovered exactly before another allocation;
        // failure keeps the record and never manufactures an extra adapter.
        let pending: Vec<_> = state
            .records
            .iter()
            .filter(|r| r.scope == scope && !r.ready)
            .cloned()
            .collect();
        for record in pending {
            recover_record(&record)?;
            state.records.retain(|r| r.token != record.token);
            save(&state)?;
        }
    }
    if let Some(record) = state
        .records
        .iter()
        .find(|r| r.scope == scope && r.lease == id.0)
        .cloned()
    {
        // A service crash can leave its device behind. An extant claim means
        // that another live owner still holds it and must not be interrupted.
        let _claim = record
            .target
            .as_ref()
            .map(|key| super::super::ownership_windows::claim(key))
            .transpose()?;
        recover_record(&record)?;
        state.records.retain(|r| r.token != record.token);
        save(&state)?;
    }
    let directory = match &state.directory {
        Some(directory) => directory.clone(),
        None => return Err(Error::DriverMissing),
    };
    verify_package(&directory)?;
    let current=driver::current_configuration()?;
    if let Some(record)=state.records.iter().find(|r|current.as_ref()==Some(&directory.join(format!("monitor-{}",r.token)))) {
        driver::switch_demand_configuration(&directory.join(format!("monitor-{}",record.token)),&directory)?;
    }
    select_configuration(&state, &directory, saved)?;
    let token = Uuid::new_v4();
    state.records.push(Record {
        scope: scope.into(),
        lease: id.0,
        token,
        instance: None,
        target: None,
        ready: false,
    });
    save(&state)?;
    let result = (|| -> Result<(Target, Box<dyn Claim>)> {
        let configuration=driver::stage_monitor_configuration(&directory,token,id.0,name,spec)?;
        driver::switch_demand_configuration(&directory,&configuration)?;
        let instance =
            driver::create_demand_device(&directory, &token.to_string(), id.0, &mut |instance| {
                state
                    .records
                    .iter_mut()
                    .find(|r| r.token == token)
                    .unwrap()
                    .instance = Some(instance.into());
                save(&state)
            })?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last_observation = String::new();
        let target = loop {
            let targets = targets_for_instance(&instance);
            if let Err(error) = &targets {last_observation=error.to_string();}
            if let Ok(mut targets) = targets {
                last_observation=format!("{} matching targets",targets.len());
                if targets.len() > 1 {
                    return Err(fail("isolated driver published more than one monitor"));
                }
                if let Some(target) = targets.pop() {
                    // This newly registered device already has an exact
                    // connected monitor identity. A free legacy source slot
                    // is a topology decision, not a driver-readiness test.
                    break target;
                }
            }
            if Instant::now() >= deadline {
                #[cfg(test)] eprintln!("Driver initialization log: {}",driver::monitor_diagnostics(&directory,token));
                #[cfg(test)] {
                    let observation=scene()?;
                    eprintln!("pending adapter {:?}; targets {:?}",observation.supported_adapters,super::inventory(&observation).map(|v|v.targets));
                }
                return Err(fail(format!(
                    "新虚拟显示器未在十秒内就绪，正在清理本次设备。{}; enumeration: {last_observation}",
                    driver::demand_problem(&instance)
                )));
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let claim = super::super::ownership_windows::claim(&target.key)?;
        driver::verify_demand_device(&instance, &token.to_string(), true)?;
        let record = state.records.iter_mut().find(|r| r.token == token).unwrap();
        record.target = Some(target.key.clone());
        record.ready = true;
        save(&state)?;
        driver::switch_demand_configuration(&configuration,&directory)?;
        driver::rename_demand_monitor(&target.key,&token.to_string(),name)?;
        Ok((target, claim))
    })();
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            let record = state
                .records
                .iter()
                .find(|r| r.token == token)
                .unwrap()
                .clone();
            match recover_record(&record) {
                Ok(()) => {
                    state.records.retain(|r| r.token != token);
                    save(&state)?;
                    Err(error)
                }
                Err(cleanup) => Err(fail(format!(
                    "{error}; 本次设备尚未完成精确清理，已停止继续创建：{cleanup}"
                ))),
            }
        }
    }
}
pub(super) fn mark_pending(key:&TargetKey)->Result<()> {
    let mut state=load()?;
    state.records.iter_mut().find(|r|r.target.as_ref()==Some(key)).ok_or(Error::UnknownSlot)?.ready=false;
    save(&state)
}
pub(super) fn rename(key:&TargetKey,name:&str)->Result<()> {
    let state=load()?;
    let record=state.records.iter().find(|r|r.target.as_ref()==Some(key)).ok_or(Error::UnknownSlot)?;
    driver::rename_demand_monitor(key,&record.token.to_string(),name)
}
pub(super) fn remove(key:&TargetKey)->Result<()> {remove_with_context(key,None)}
fn remove_with_context(key: &TargetKey,context:Option<(&DisplaySnapshot,&[(TargetKey,Spec,Option<Position>)])>) -> Result<()> {
    #[cfg(test)] let timer=Instant::now();
    let mut state = load()?;
    let record=state.records.iter().find(|r|r.target.as_ref()==Some(key)).cloned();
    let Some(record)=record else {
        // A preceding uninstall can have committed its ledger deletion before
        // a later preference-cleanup error. Absence is idempotent only after
        // freshly proving that this adapter no longer exists in CCD.
        if !scene()?.supported_adapters.contains_key(&key.adapter) {return Ok(());}
        return Err(Error::UnknownSlot);
    };
    let before=snapshot()?;
    let survivors:Vec<_>=inventory(Some(&record.scope))?.targets.into_iter().filter(|t|t.key!=*key&&t.active).filter_map(|t|t.actual.map(|spec|(t.key,spec))).collect();
    let survivors=owned_placements(&before.scene.active,&survivors)?;
    if let Some(instance)=&record.instance {
        if driver::verify_demand_device(instance,&record.token.to_string(),record.ready)? {
            // Retire the path while Windows still knows its current topology.
            // Uninstalling a live IDD directly can replay a cached clone mode
            // for a surviving sibling even after a successful extension.
            let mut detached=before.scene.active.clone();
            detached.paths.retain(|path|target_key(path).is_ok_and(|(k,_)|k!=*key));
            if !detached.paths.is_empty() {
                let mut submitted=false;
                if let Err(error)=apply(&detached,&mut submitted) {tracing::warn!(%error,"owned monitor detach will finish through exact device removal");}
            }
            #[cfg(test)] eprintln!("remove detached {:?}",timer.elapsed());
        }
    }
    recover_record(&record)?;
    #[cfg(test)] eprintln!("remove uninstalled {:?}",timer.elapsed());
    // Windows can select an older mode for the new connected combination.
    // Restore only this scope's surviving, exactly owned display specifications
    // from immediately before this removal; never replay a global layout.
    let (baseline,survivors)=context.unwrap_or((&before,&survivors));
    configure_owned_set(survivors,Some(baseline),Some(key))?;
    state.records.retain(|r| r.token != record.token);
    save(&state)?;
    if state.records.is_empty() && driver::demand_instances()?.is_empty() {
        if let Some(directory) = &state.directory {
            driver::retire_uninstalled_configuration(directory)?;
        }
    }
    Ok(())
}
pub(super) fn inventory(scope: Option<&str>) -> Result<Inventory> {
    let state = load()?;
    let mut targets = Vec::new();
    for record in state
        .records
        .iter()
        .filter(|r| r.ready && scope.is_some_and(|s| s == r.scope))
    {
        if let Some(instance) = &record.instance {
            if driver::verify_demand_device(instance, &record.token.to_string(), true)? {
                targets.extend(targets_for_instance(instance)?);
            }
        }
    }
    Ok(Inventory {
        supported: true,
        driver_present: state.directory.is_some(),
        targets,
    })
}
pub(super) fn scope_for(key:&TargetKey)->Result<String> {
    load()?.records.into_iter().find(|r|r.target.as_ref()==Some(key)).map(|r|r.scope).ok_or(Error::UnknownSlot)
}
pub(super) fn owned_keys(key: &TargetKey) -> Result<BTreeSet<TargetKey>> {
    let state = load()?;
    let scope = &state
        .records
        .iter()
        .find(|r| r.ready && r.target.as_ref() == Some(key))
        .ok_or(Error::UnknownSlot)?
        .scope;
    Ok(state
        .records
        .iter()
        .filter(|r| r.ready && r.scope == *scope)
        .filter_map(|r| r.target.clone())
        .collect())
}

#[cfg(test)]
mod live_tests {
    use super::*;
    #[test]
    #[ignore = "explicit administrator fixture; creates and removes only its own devices"]
    fn signed_driver_hotplug_preserves_sibling_and_original_displays() {
        let package = std::env::var_os("ECLIPSE_VDD_TEST_PACKAGE")
            .expect("explicit isolated driver package required");
        let package = read_pinned_package(Path::new(&package)).unwrap();
        worker(move|| {
            crate::service_admin::windows::require_administrator().unwrap();
            let original_instances=driver::demand_instances().unwrap();
            let initial=load().unwrap();
            let original_records=serde_json::to_value(&initial.records).unwrap();
            let initial_file=std::fs::read(path().unwrap()).ok();
            let previous_configuration=driver::current_configuration().unwrap();
            let original=active_signature().unwrap();assert!(!original.is_empty());
            let scope=hex::encode(Sha256::digest(Uuid::new_v4().as_bytes()));
            let result=(|| -> Result<()> {
                let timer=Instant::now();
                prepare(&package)?;
                eprintln!("prepared {:?}",timer.elapsed());
                if driver::demand_instances()?!=original_instances {return Err(fail("preparation published an unexpected device"));}
                let spec=Spec{width:1920,height:1080,refresh_millihz:60000,hdr:false};
                let first_id=LeaseId(Uuid::new_v4());
                let (first,first_claim)=create_configured(&scope,first_id,&BTreeSet::new(),"Athena Luna",spec,None,&[])?;
                eprintln!("first allocated {:?}",timer.elapsed());
                if first.name!="Athena Luna" {return Err(fail(format!("per-display EDID name was not published: {}",first.name)));}
                eprintln!("first configured {:?}",timer.elapsed());
                if driver::demand_instances()?.len()!=original_instances.len()+1 {return Err(fail("first create did not publish exactly one adapter"));}
                let (second,second_claim)=create_configured(&scope,LeaseId(Uuid::new_v4()),&BTreeSet::new(),"Apollo Sirius",spec,None,&[(first.key.clone(),spec)])?;
                eprintln!("second allocated {:?}",timer.elapsed());
                if second.name!="Apollo Sirius" {return Err(fail(format!("second EDID identity was not published: {}",second.name)));}
                eprintln!("second configured {:?}",timer.elapsed());
                if !inventory(Some(&scope))?.targets.iter().any(|t|t.key==first.key&&t.active) {return Err(fail("creating the second device deactivated the first"));}
                if driver::demand_instances()?.len()!=original_instances.len()+2 {return Err(fail("second create did not publish exactly two adapters"));}
                // Reproduce a Windows virtual clone using only our two test
                // outputs. User displays retain their full current modes.
                let mut topology=query(QDC_ONLY_ACTIVE_PATHS)?;
                let first_path=*topology.paths.iter().find(|p|target_key(p).is_ok_and(|(k,_)|k==first.key)).ok_or(Error::UnknownSlot)?;
                let second_index=topology.paths.iter().position(|p|target_key(p).is_ok_and(|(k,_)|k==second.key)).ok_or(Error::UnknownSlot)?;
                let second_path=topology.paths[second_index];
                if first_path.flags&DISPLAYCONFIG_PATH_SUPPORT_VIRTUAL_MODE==0 || second_path.flags&DISPLAYCONFIG_PATH_SUPPORT_VIRTUAL_MODE==0 {return Err(fail("fixture requires Windows virtual-clone support"));}
                let first_source=unsafe{topology.modes[source_mode_index(&first_path) as usize].Anonymous.sourceMode};
                let mut second_source=unsafe{topology.modes[source_mode_index(&second_path) as usize].Anonymous.sourceMode};
                if second_source.position.x!=first_source.position.x+spec.width as i32 || second_source.position.y!=0 {return Err(fail("a new display did not extend to the desktop right edge"));}
                second_source.position=first_source.position;
                topology.modes[source_mode_index(&second_path) as usize].Anonymous.sourceMode=second_source;
                topology.paths[second_index].targetInfo.Anonymous.modeInfoIdx=DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
                let mut submitted=false;apply(&topology,&mut submitted)?;
                let cloned=query(QDC_ONLY_ACTIVE_PATHS)?;
                let clone=cloned.paths.iter().find(|p|target_key(p).is_ok_and(|(k,_)|k==second.key)).ok_or(Error::UnknownSlot)?;
                let clone_source=unsafe{cloned.modes[source_mode_index(clone) as usize].Anonymous.sourceMode};
                if clone_source.position.x!=first_source.position.x || clone_source.position.y!=first_source.position.y {return Err(fail("fixture did not enter the reported clone state"));}
                configure_owned_mode(&second.key,spec,true).map_err(|e|fail(e.message))?;
                eprintln!("clone extended {:?}",timer.elapsed());
                let extended=query(QDC_ONLY_ACTIVE_PATHS)?;
                let path=extended.paths.iter().find(|p|target_key(p).is_ok_and(|(k,_)|k==second.key)).ok_or(Error::UnknownSlot)?;
                let source=unsafe{extended.modes[source_mode_index(path) as usize].Anonymous.sourceMode};
                if source.position.x!=first_source.position.x+spec.width as i32 || source.position.y!=0 {return Err(fail("virtual clone was not split and saved at the right edge"));}
                eprintln!("removing first {:?}",timer.elapsed());
                let removing=Instant::now();
                remove(&first.key)?;drop(first_claim);remove(&first.key)?;
                eprintln!("first removed {:?}",timer.elapsed());
                if removing.elapsed()>=Duration::from_secs(10) {return Err(fail("owned removal waited for the driver stop timeout"));}
                let settled=Instant::now()+Duration::from_secs(5);
                loop {
                    let remaining=inventory(Some(&scope))?;
                    if remaining.targets.len()==1 && remaining.targets[0].key==second.key && remaining.targets[0].active
                        && remaining.targets[0].actual.is_some_and(|s|s.width==spec.width&&s.height==spec.height&&s.refresh_millihz.abs_diff(spec.refresh_millihz)<=1&&s.hdr==spec.hdr) {break;}
                    if Instant::now()>=settled {return Err(fail(format!("removing one device interrupted its sibling: expected {:?}, observed {:?}",second.key,remaining.targets)));}
                    std::thread::sleep(Duration::from_millis(100));
                }
                remove(&second.key)?;drop(second_claim);
                eprintln!("second removed {:?}",timer.elapsed());
                // Re-enable the same logical identity above the primary, then
                // add two siblings and remove in the opposite order.
                let above=Position{x:0,y:-(spec.height as i32)};
                let (restored,restored_claim)=create_configured(&scope,first_id,&BTreeSet::new(),"Athena Luna",spec,Some(above),&[])?;
                if position_of(&query(QDC_ONLY_ACTIVE_PATHS)?,&restored.key)?!=Some(above) {return Err(fail("saved position above primary was not restored"));}
                let expected=format!("root\\display\\{}",first_id.0.simple());
                if load()?.records.iter().find(|r|r.lease==first_id.0).and_then(|r|r.instance.as_ref())!=Some(&expected) {return Err(fail("re-enable did not keep its logical PnP identity"));}
                rename(&restored.key,"办公副屏 Atlas")?;
                let sibling_spec=Spec{width:1920,height:1200,refresh_millihz:59_940,hdr:false};
                let (sibling,sibling_claim)=create_configured(&scope,LeaseId(Uuid::new_v4()),&BTreeSet::new(),"Artemis Vega",sibling_spec,None,&[(restored.key.clone(),spec)])?;
                if position_of(&query(QDC_ONLY_ACTIVE_PATHS)?,&restored.key)?!=Some(above) {return Err(fail("new sibling moved the screen above primary"));}
                let before=snapshot()?;
                let existing=[(restored.key.clone(),spec),(sibling.key.clone(),sibling_spec)];
                let mut desired=owned_placements(&before.scene.active,&existing)?;
                let third_spec=Spec{width:1280,height:720,..spec};
                let (third,third_claim)=create_named(&scope,LeaseId(Uuid::new_v4()),&BTreeSet::new(),"Zeus Orion",third_spec)?;
                // Simulate Windows disabling an existing sibling during PnP.
                deactivate(&restored.key,&owned_keys(&third.key)?)?;
                desired.push((third.key.clone(),third_spec,None));
                configure_owned_set(&desired,Some(&before),None)?;
                if position_of(&query(QDC_ONLY_ACTIVE_PATHS)?,&restored.key)?!=Some(above) {return Err(fail("hotplug did not reconnect the previously enabled screen"));}
                if inventory(Some(&scope))?.targets.iter().filter(|t|t.active).count()!=3 {return Err(fail("third display left an enabled sibling disconnected"));}
                remove(&third.key)?;drop(third_claim);
                remove(&sibling.key)?;drop(sibling_claim);
                remove(&restored.key)?;drop(restored_claim);
                let (right,right_claim)=create_configured(&scope,first_id,&BTreeSet::new(),"Athena Luna",spec,None,&[])?;
                if position_of(&query(QDC_ONLY_ACTIVE_PATHS)?,&right.key)?.is_none_or(|p|p.y!=0||p.x<=0) {return Err(fail("restore off did not extend to primary right"));}
                remove(&right.key)?;drop(right_claim);
                eprintln!("identity, both removal orders, saved position, restore off and third screen verified {:?}",timer.elapsed());
                if driver::demand_instances()?!=original_instances {return Err(fail("deleted devices left disconnected adapter nodes"));}
                let deadline=Instant::now()+Duration::from_secs(10);
                loop {
                    if active_signature()?==original {break;}
                    if Instant::now()>=deadline {return Err(fail("original physical/external display layout did not remain intact"));}
                    std::thread::sleep(Duration::from_millis(100));
                }
                Ok(())
            })();
            // Clean exact resources even when an assertion above fails. Never
            // restore a desktop snapshot or remove unrelated adapters.
            let cleanup=reconcile_owner(&scope);
            let final_state=load().unwrap();
            if cleanup.is_ok() && final_state.records.is_empty() && driver::demand_instances().unwrap().is_empty() {
                if let Some(directory)=&final_state.directory {
                    if driver::current_configuration().unwrap().as_deref()==Some(directory) {driver::retire_uninstalled_configuration(directory).unwrap();}
                }
                if let Some(directory)=previous_configuration {driver::demand_configuration(&directory).unwrap();}
                match initial_file {
                    Some(bytes)=>crate::private_file::write_atomic(&path().unwrap(),&bytes).unwrap(),
                    None=>{if path().unwrap().exists(){std::fs::remove_file(path().unwrap()).unwrap();}},
                }
                if initial.directory.is_none() {
                    if let Some(directory)=&final_state.directory {
                        for file in &driver_package().files {std::fs::remove_file(directory.join(&file.name)).unwrap();}
                        std::fs::remove_dir(directory).unwrap();
                    }
                }
            }
            assert!(cleanup.is_ok(),"fixture cleanup pending: {cleanup:?}");
            assert_eq!(serde_json::to_value(&load().unwrap().records).unwrap(),original_records,"fixture changed an existing display owner");
            assert!(result.is_ok(),"signed driver lifecycle failed: {result:?}");
        }).unwrap();
        // Exercise the public owner command path and manager state against the
        // signed driver too, outside the lifecycle lock acquired by each call.
        let original=worker(||Ok::<_,Error>((active_signature()?,serde_json::to_value(load()?.records).map_err(|e|fail(e.to_string()))?))).unwrap().unwrap();
        let directory=std::env::temp_dir().join(format!("eclipse-display-definitions-{}",Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let manager=Manager::shared();manager.configure_saved_displays(&directory).unwrap();
        use crate::native_virtual_display::{execute_local_owner,Request,Response};
        let command=|request:Request|execute_local_owner(request.into()).map_err(|e|fail(e.to_string()));
        let result=(||->Result<()> {
            let Response::Created{display}=command(Request::Add)? else{return Err(fail("definition command returned no record"));};
            if display.active||display.enabled {return Err(fail("adding a definition published a device"));}
            let spec=Spec{width:1920,height:1200,refresh_millihz:59_940,hdr:false};
            command(Request::Configure{id:display.id,name:"Athena Luna".into(),spec})?;
            command(Request::SetEnabled{id:display.id,enabled:true})?;
            command(Request::SetPersistent{id:display.id,persistent:false})?;
            let active=manager.list()?.remove(0);
            if !active.active||active.actual!=Some(spec) {return Err(fail("public enable did not connect the configured mode"));}
            let a=Uuid::new_v4();let b=Uuid::new_v4();let guard_a=manager.session_guard(a);let guard_b=manager.session_guard(b);
            manager.track_session_source(a,&active.source)?;manager.track_session_source(b,&active.source)?;
            manager.release_session(a)?;drop(guard_a);
            if !manager.list()?[0].active {return Err(fail("temporary screen was disabled while another viewer used it"));}
            manager.release_session(b)?;drop(guard_b);
            let disabled=manager.list()?.remove(0);
            if disabled.enabled||disabled.active||disabled.id!=display.id {return Err(fail("last disconnect failed to retain a disabled definition"));}
            let owner=Uuid::new_v4();let guard=manager.session_guard(owner);
            let reused=manager.create_for_session(owner,spec,false,None)?;
            if reused.id!=display.id {return Err(fail("temporary reconnection created another definition"));}
            manager.release_session(owner)?;drop(guard);
            command(Request::Remove{id:display.id})?;
            if !manager.list()?.is_empty() {return Err(fail("deleting the disabled definition left a record"));}
            eprintln!("public Add/configure/enable, final-viewer disable and temporary reuse verified");
            Ok(())
        })();
        let cleanup=manager.close_all();let shutdown=manager.shutdown_manual();
        assert!(cleanup.is_ok()&&shutdown.is_ok(),"definition fixture cleanup pending: {cleanup:?}, {shutdown:?}");
        let observed=worker(||Ok::<_,Error>((active_signature()?,serde_json::to_value(load()?.records).map_err(|e|fail(e.to_string()))?))).unwrap().unwrap();
        assert_eq!(observed,original,"manager fixture changed an existing display or owner");
        std::fs::remove_dir_all(directory).unwrap();
        assert!(result.is_ok(),"public definition lifecycle failed: {result:?}");
    }
}
