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
pub(super) fn create(
    scope: &str,
    id: LeaseId,
    saved: &BTreeSet<TargetKey>,
) -> Result<(Target, Box<dyn Claim>)> {
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
        let instance =
            driver::create_demand_device(&directory, &token.to_string(), &mut |instance| {
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
pub(super) fn remove(key: &TargetKey) -> Result<()> {
    let mut state = load()?;
    let record = state
        .records
        .iter()
        .find(|r| r.target.as_ref() == Some(key))
        .cloned()
        .ok_or(Error::UnknownSlot)?;
    let survivors:Vec<_>=inventory(Some(&record.scope))?.targets.into_iter().filter(|t|t.key!=*key&&t.active).filter_map(|t|t.actual.map(|spec|(t.key,spec))).collect();
    if let Some(instance)=&record.instance {
        if driver::verify_demand_device(instance,&record.token.to_string(),record.ready)? {
            // Retire the path while Windows still knows its current topology.
            // Uninstalling a live IDD directly can replay a cached clone mode
            // for a surviving sibling even after a successful extension.
            let owned:BTreeSet<_>=state.records.iter().filter(|r|r.ready&&r.scope==record.scope&&r.token!=record.token).filter_map(|r|r.target.clone()).collect();
            deactivate(key,&owned)?;
            let live=mode_fingerprints(&query(QDC_ONLY_ACTIVE_PATHS)?,None)?;
            if owned.iter().any(|key|live.contains_key(key)) {
                capacity::persist_detached_live(&owned,key)?;
            }
        }
    }
    recover_record(&record)?;
    // Windows can select an older mode for the new connected combination.
    // Restore only this scope's surviving, exactly owned display specifications
    // from immediately before this removal; never replay a global layout.
    for (key,spec) in survivors {
        let target=inventory(Some(&record.scope))?.targets.into_iter().find(|t|t.key==key).ok_or(Error::UnknownSlot)?;
        let unchanged=target.active && target.actual.is_some_and(|s|s.width==spec.width&&s.height==spec.height&&s.refresh_millihz.abs_diff(spec.refresh_millihz)<=1&&s.hdr==spec.hdr);
        if !unchanged {configure_owned_mode(&key,spec,true).map_err(|e|fail(format!("remaining owned monitor mode recovery is pending: {}",e.message)))?;}
    }
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
                let (first,first_claim)=create(&scope,LeaseId(Uuid::new_v4()),&BTreeSet::new())?;
                let spec=Spec{width:1920,height:1080,refresh_millihz:60000,hdr:false};
                eprintln!("first allocated {:?}",timer.elapsed());
                configure_owned_mode(&first.key,spec,true).map_err(|e|fail(e.message))?;
                eprintln!("first configured {:?}",timer.elapsed());
                if driver::demand_instances()?.len()!=original_instances.len()+1 {return Err(fail("first create did not publish exactly one adapter"));}
                let (second,second_claim)=create(&scope,LeaseId(Uuid::new_v4()),&BTreeSet::new())?;
                eprintln!("second allocated {:?}",timer.elapsed());
                configure_owned_mode(&second.key,spec,true).map_err(|e|fail(e.message))?;
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
                remove(&first.key)?;drop(first_claim);
                eprintln!("first removed {:?}",timer.elapsed());
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
    }
}
