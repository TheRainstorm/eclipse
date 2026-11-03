//! Idle-host capacity preparation for the pinned Mtt driver. Global XML count
//! changes require an adapter restart; they are never performed for a live
//! lease, through the driver's pipe, or by making one Root device per stream.
use super::*;
use crate::virtual_display::{capacity_config, capacity_policy, preparation};
use std::{
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
};
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};

#[path = "capacity_store_windows.rs"]
mod store;
type Checkpoint<'a> = &'a (dyn Fn() -> preparation::Result<()> + Sync);
fn fail(message: impl Into<String>) -> Error {
    Error::Platform(message.into())
}
fn unchanged(error: Error) -> preparation::Failure {
    preparation::Failure::unchanged(error.to_string())
}
fn idle(checkpoint: Checkpoint<'_>) -> Result<()> {
    checkpoint().map_err(|e| fail(e.message))
}

struct Existing {
    instance: String,
    original: DisabledState,
    inf: String,
    config: PathBuf,
    change: capacity_config::Change,
    original_targets: usize,
}
struct Pending {
    existing: Option<Existing>,
    installed: Option<(String, PathBuf)>,
    install_xml: Vec<u8>,
    minimum: u32,
    baseline: Fingerprints,
    database: Fingerprints,
    claims: BTreeMap<TargetKey, Box<dyn Claim>>,
    journal: store::Journal,
    progress: capacity_policy::Progress,
    failure: String,
}
fn pending() -> &'static Mutex<Option<Pending>> {
    static PENDING: OnceLock<Mutex<Option<Pending>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(None))
}
pub(in crate::virtual_display) fn recovery_pending() -> Result<bool> {
    Ok(pending()
        .lock()
        .map_err(|_| fail("capacity recovery state is poisoned"))?
        .is_some()
        || store::pending_exists()?)
}
pub(in crate::virtual_display) fn read_configuration(path: &Path) -> Result<Vec<u8>> {
    store::read(path)
}
pub(in crate::virtual_display) fn replace_configuration(
    path: &Path,
    before: &[u8],
    after: &[u8],
) -> Result<()> {
    store::replace_exact(path, before, after)
}
pub(in crate::virtual_display) fn verify_new_instance(
    baseline: &Fingerprints,
    instance: &str,
) -> Result<()> {
    verify_enable_effects(baseline, &scene()?, instance)
}

fn string_value(key: &str, name: &str) -> Result<String> {
    let key: Vec<_> = key.encode_utf16().chain([0]).collect();
    let name: Vec<_> = name.encode_utf16().chain([0]).collect();
    let mut value = [0u16; 32768];
    let mut bytes = size_of_val(&value) as u32;
    let code = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            value.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if code != 0 {
        return Err(error(
            "read exact Mtt driver/configuration registry value",
            code,
        ));
    }
    let units = bytes as usize / 2;
    if bytes % 2 != 0
        || units == 0
        || units > value.len()
        || value[units - 1] != 0
        || value[..units - 1].contains(&0)
    {
        return Err(fail("Mtt registry string is malformed"));
    }
    Ok(String::from_utf16(&value[..units - 1])
        .map_err(|_| fail("Mtt registry string is not valid UTF-16"))?)
}
fn configuration() -> Result<PathBuf> {
    let directory = PathBuf::from(string_value(
        "SOFTWARE\\MikeTheTech\\VirtualDisplayDriver",
        "VDDPATH",
    )?);
    if !directory.is_absolute() {
        return Err(fail("Mtt configuration path must be absolute"));
    }
    Ok(directory.join("vdd_settings.xml"))
}
fn driver_value(devices: &DeviceSet, device: &SP_DEVINFO_DATA, name: &str) -> Result<String> {
    let mut value = [0u16; 1024];
    win_ok(
        unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                devices.0,
                device,
                SPDRP_DRIVER,
                null_mut(),
                value.as_mut_ptr().cast(),
                size_of_val(&value) as u32,
                null_mut(),
            )
        },
        "read exact Mtt driver registry key",
    )?;
    string_value(
        &format!(
            "SYSTEM\\CurrentControlSet\\Control\\Class\\{}",
            text(&value)
        ),
        name,
    )
}
fn mtt(devices: &DeviceSet) -> Result<Vec<Candidate>> {
    let mut result = Vec::new();
    for index in 0..4096 {
        let mut device: SP_DEVINFO_DATA = unsafe { zeroed() };
        device.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
        if unsafe { SetupDiEnumDeviceInfo(devices.0, index, &mut device) } == 0 {
            let code = unsafe { GetLastError() };
            if code == ERROR_NO_MORE_ITEMS {
                return Ok(result);
            }
            return Err(error("enumerate capacity adapters", code));
        }
        // An unrelated adapter may have no hardware ID. Only an exact Mtt ID
        // creates mutation authority, never a name or a Zako-compatible target.
        if is_mtt(devices, &device)? {
            result.push(Candidate {
                device,
                instance: devices.instance(&device)?,
                original: disabled_state(devices, &device)?,
            });
        }
    }
    Err(fail("capacity adapter inventory exceeds safety bound"))
}
fn open(devices: &DeviceSet, instance: &str, original: DisabledState) -> Result<Candidate> {
    let mut device: SP_DEVINFO_DATA = unsafe { zeroed() };
    device.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
    let name: Vec<_> = instance.encode_utf16().chain([0]).collect();
    win_ok(
        unsafe { SetupDiOpenDeviceInfoW(devices.0, name.as_ptr(), null_mut(), 0, &mut device) },
        "reopen exact capacity adapter",
    )?;
    if !is_mtt(devices, &device)? || devices.instance(&device)? != instance {
        return Err(fail("capacity adapter identity was replaced"));
    }
    Ok(Candidate {
        device,
        instance: instance.into(),
        original,
    })
}
fn verify_driver(
    devices: &DeviceSet,
    candidate: &Candidate,
    expected_inf: Option<&str>,
) -> Result<String> {
    let inf = driver_value(devices, &candidate.device, "InfPath")?;
    if expected_inf.is_some_and(|value| !inf.eq_ignore_ascii_case(value))
        || driver_value(devices, &candidate.device, "DriverVersion")?
            != crate::virtual_display::driver_package().driver_version
    {
        return Err(fail(
            "capacity preparation requires the unchanged pinned Mtt driver identity/version",
        ));
    }
    if Path::new(&inf).file_name().and_then(|v| v.to_str()) != Some(inf.as_str()) {
        return Err(fail("installed Mtt INF is not a published INF filename"));
    }
    let inf_wide: Vec<_> = inf.encode_utf16().chain([0]).collect();
    let mut path = [0u16; 32768];
    win_ok(
        unsafe {
            SetupGetInfDriverStoreLocationW(
                inf_wide.as_ptr(),
                null(),
                null(),
                path.as_mut_ptr(),
                path.len() as u32,
                null_mut(),
            )
        },
        "resolve exact installed Mtt driver store",
    )?;
    let length = path
        .iter()
        .position(|c| *c == 0)
        .ok_or_else(|| fail("Mtt driver store path is not terminated"))?;
    let directory = PathBuf::from(std::ffi::OsString::from_wide(&path[..length]))
        .parent()
        .ok_or_else(|| fail("Mtt driver store path has no parent"))?
        .to_path_buf();
    // The XML may contain user modes and is deliberately excluded. INF, CAT,
    // DLL are bound to the exact signed original package before any restart.
    for expected in &crate::virtual_display::driver_package().files {
        if expected.name == "vdd_settings.xml" {
            continue;
        }
        let bytes = store::read(&directory.join(&expected.name))?;
        if bytes.len() != expected.bytes || hex::encode(Sha256::digest(&bytes)) != expected.sha256 {
            return Err(fail(
                "installed Mtt driver store bytes differ from the pinned signed release",
            ));
        }
    }
    crate::virtual_display::install_windows::verify_catalog(&directory)?;
    Ok(inf)
}

pub(in crate::virtual_display) fn observe() -> Result<preparation::Observation> {
    let devices = DeviceSet::displays()?;
    let candidates = mtt(&devices)?;
    let mut result = preparation::Observation {
        supported: cfg!(target_arch = "x86_64"),
        driver_present: !candidates.is_empty(),
        configured_capacity: None,
        targets: 0,
        available: 0,
        active_driver_targets: 0,
        claimed_targets: 0,
        active_streams: 0,
        authority: crate::service_admin::windows::require_administrator().is_ok(),
        recovery_pending: recovery_pending()?,
    };
    if candidates.len() == 1 {
        let targets = targets_for_instance(&candidates[0].instance)?;
        result.targets = targets
            .len()
            .try_into()
            .map_err(|_| fail("capacity target count overflow"))?;
        for target in targets {
            result.active_driver_targets += u32::from(target.active || target.actual.is_some());
            let claimed = crate::virtual_display::ownership_windows::is_claimed(&target.key)?;
            result.claimed_targets += u32::from(claimed);
            result.available += u32::from(
                target.available && !target.active && target.actual.is_none() && !claimed,
            );
        }
        if let Ok(path) = configuration() {
            if let Ok(bytes) = store::read(&path) {
                if let Ok(change) = capacity_config::expand(&bytes, 1) {
                    result.configured_capacity = Some(change.original_count);
                }
            }
        }
    }
    Ok(result)
}

// Semantic saved-mode identity. Database target totalSize is documented to be
// zero; do not query live HDR as if it were persisted database state. Transient
// mode-array indices and status flags are not configuration ownership.
fn modes(topology: &Topology) -> Result<Fingerprints> {
    let mut result = BTreeMap::new();
    for path in &topology.paths {
        let (key, _) = target_key(path)?;
        let source = topology
            .modes
            .get(unsafe { path.sourceInfo.Anonymous.modeInfoIdx } as usize)
            .filter(|m| m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE)
            .ok_or_else(|| fail("cannot preserve a saved source mode"))?;
        if adapter(source.adapterId) != adapter(path.sourceInfo.adapterId)
            || source.id != path.sourceInfo.id
        {
            return Err(fail("saved source mode identity differs from its path"));
        }
        let source = unsafe { source.Anonymous.sourceMode };
        let target = topology
            .modes
            .get(unsafe { path.targetInfo.Anonymous.modeInfoIdx } as usize)
            .filter(|m| m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_TARGET)
            .ok_or_else(|| fail("cannot preserve a saved target signal"))?;
        if adapter(target.adapterId) != adapter(path.targetInfo.adapterId)
            || target.id != path.targetInfo.id
        {
            return Err(fail("saved target signal identity differs from its path"));
        }
        let signal = unsafe { target.Anonymous.targetMode }.targetVideoSignalInfo;
        let value = vec![
            source.width as i64,
            source.height as i64,
            source.pixelFormat as i64,
            source.position.x as i64,
            source.position.y as i64,
            path.sourceInfo.id as i64,
            path.targetInfo.rotation as i64,
            path.targetInfo.scaling as i64,
            path.targetInfo.refreshRate.Numerator as i64,
            path.targetInfo.refreshRate.Denominator as i64,
            path.targetInfo.scanLineOrdering as i64,
            path.sourceInfo.adapterId.LowPart as i64,
            path.sourceInfo.adapterId.HighPart as i64,
            path.targetInfo.outputTechnology as i64,
            signal.pixelRate as i64,
            signal.hSyncFreq.Numerator as i64,
            signal.hSyncFreq.Denominator as i64,
            signal.vSyncFreq.Numerator as i64,
            signal.vSyncFreq.Denominator as i64,
            signal.activeSize.cx as i64,
            signal.activeSize.cy as i64,
            unsafe { signal.Anonymous.videoStandard } as i64,
            signal.scanLineOrdering as i64,
        ];
        if result.insert(key, value).is_some() {
            return Err(fail("saved topology repeats one target identity"));
        }
    }
    Ok(result)
}
fn database() -> Result<Fingerprints> {
    modes(&query(QDC_DATABASE_CURRENT)?)
}
fn owned_keys(pending: &Pending) -> BTreeSet<TargetKey> {
    pending.claims.keys().cloned().collect()
}
fn database_preserved(pending: &Pending) -> Result<bool> {
    Ok(capacity_policy::unchanged_foreign(
        &pending.database,
        &database()?,
        &owned_keys(pending),
    ))
}
fn json_signature(value: &Fingerprints) -> serde_json::Value {
    serde_json::Value::Array(value.iter().map(|(key,modes)|serde_json::json!({"adapter":key.adapter,"target":key.target,"monitor_path":key.monitor_path,"modes":modes})).collect())
}
fn healthy(devices: &DeviceSet, candidate: &Candidate, enabled: bool) -> Result<()> {
    let state = disabled_state(devices, &candidate.device)?;
    let flags = if enabled {
        candidate.original.global_flags & !CONFIGFLAG_DISABLED
    } else {
        candidate.original.global_flags | CONFIGFLAG_DISABLED
    };
    if state.profile != candidate.original.profile
        || state.global_flags != flags
        || problem(&candidate.device)? != if enabled { 0 } else { CM_PROB_DISABLED }
        || reboot_pending(devices, &candidate.device)?
    {
        return Err(fail(
            "exact capacity adapter state or restart requirement changed",
        ));
    }
    Ok(())
}
fn fresh_idle(
    pending: &mut Pending,
    candidate: &Candidate,
    checkpoint: Checkpoint<'_>,
) -> Result<Vec<Target>> {
    idle(checkpoint)?;
    let targets = targets_for_instance(&candidate.instance)?;
    claim_targets(&targets, &mut pending.claims)?;
    if targets.iter().any(|t| t.active || t.actual.is_some()) {
        return Err(fail(
            "capacity adapter has an active target; reload was refused",
        ));
    }
    Ok(targets)
}
fn inactive(
    pending: &mut Pending,
    devices: &DeviceSet,
    candidate: &Candidate,
    count: usize,
    checkpoint: Checkpoint<'_>,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut keys = BTreeSet::new();
    let mut since = Instant::now();
    loop {
        idle(checkpoint)?;
        let fresh = scene()?;
        let targets = targets_for_instance(&candidate.instance)?;
        claim_targets(&targets, &mut pending.claims)?;
        verify_enable_effects(&pending.baseline, &fresh, &candidate.instance)?;
        let next: BTreeSet<_> = targets.iter().map(|t| t.key.clone()).collect();
        if next != keys {
            keys = next;
            since = Instant::now();
        }
        if targets.len() == count && since.elapsed() >= SETTLE {
            for target in &targets {
                idle(checkpoint)?;
                verify_new_instance(&pending.baseline, &candidate.instance)?;
                // Temporary exact-target removal; pool persistence is a
                // separate, final fresh SAVE after all targets are inactive.
                deactivate(&target.key)?;
            }
            let after = targets_for_instance(&candidate.instance)?;
            if after.iter().map(|t| t.key.clone()).collect::<BTreeSet<_>>() == keys
                && after
                    .iter()
                    .all(|t| !t.active && t.actual.is_none() && t.available)
            {
                healthy(devices, candidate, true)?;
                if active_signature()? != pending.baseline {
                    return Err(fail("an original output changed during capacity preparation; its layout was not replayed"));
                }
                return Ok(());
            }
            since = Instant::now();
        }
        if Instant::now() >= deadline {
            return Err(fail(
                "requested Mtt capacity did not settle into wholly inactive targets",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
fn save_pool(pending: &mut Pending, instance: &str, checkpoint: Checkpoint<'_>) -> Result<()> {
    idle(checkpoint)?;
    if active_signature()? != pending.baseline || !database_preserved(pending)? {
        return Err(fail("active or saved non-owned topology changed before pool persistence; no old layout was replayed"));
    }
    let owned = owned_keys(pending);
    let saved = database()?;
    // An already Mtt-free persistence baseline needs no write at all. Saved
    // preferences can differ from the current resolved mode and remain intact.
    if !saved.keys().any(|key| owned.contains(key)) {
        pending.progress = capacity_policy::Progress::FinalReadback;
        return stable_prepared_readback(pending, checkpoint);
    }
    let mut current = query(QDC_ONLY_ACTIVE_PATHS)?;
    let expected = modes(&current)?;
    let non_owned = capacity_policy::foreign(&saved, &owned);
    if non_owned.len() != expected.len()
        || non_owned.iter().any(|(key, mode)| {
            expected
                .get(key)
                .is_none_or(|active| !capacity_policy::preserve_saved_scaling(mode, active))
        })
    {
        return Err(fail("saved non-Mtt display configuration differs from the current preserved outputs; automatic pool persistence was refused"));
    }
    if targets_for_instance(instance)?
        .iter()
        .any(|t| t.active || t.actual.is_some())
    {
        return Err(fail("pool still has an active target before persistence"));
    }
    // API-defined request for the path's last-saved scaling. No old mode or
    // topology is copied into the fresh current proposal.
    for path in &mut current.paths {
        path.targetInfo.scaling = DISPLAYCONFIG_SCALING_PREFERRED;
    }
    let validate = unsafe {
        SetDisplayConfig(
            current.paths.len() as u32,
            current.paths.as_ptr(),
            current.modes.len() as u32,
            current.modes.as_ptr(),
            SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_VALIDATE,
        )
    };
    if validate != 0 {
        return Err(error("SetDisplayConfig(pool VALIDATE)", validate as u32));
    }
    idle(checkpoint)?;
    if active_signature()? != pending.baseline || database()? != saved {
        return Err(fail(
            "topology changed after pool validation; SAVE was not submitted",
        ));
    }
    let mut fresh = query(QDC_ONLY_ACTIVE_PATHS)?;
    if modes(&fresh)? != expected {
        return Err(fail("current output modes changed before pool SAVE"));
    }
    for path in &mut fresh.paths {
        path.targetInfo.scaling = DISPLAYCONFIG_SCALING_PREFERRED;
    }
    pending.journal.phase("pool_save_submitted")?;
    pending.progress = capacity_policy::Progress::SaveSubmitted;
    let code = unsafe {
        SetDisplayConfig(
            fresh.paths.len() as u32,
            fresh.paths.as_ptr(),
            fresh.modes.len() as u32,
            fresh.modes.as_ptr(),
            SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_APPLY | SDC_SAVE_TO_DATABASE,
        )
    };
    if code != 0 {
        return Err(error(
            "SetDisplayConfig(pool APPLY|SAVE_TO_DATABASE)",
            code as u32,
        ));
    }
    let after = database()?;
    if active_signature()? != pending.baseline
        || !capacity_policy::unchanged_foreign(&pending.database, &after, &owned)
        || after.keys().any(|key| owned.contains(key))
    {
        return Err(fail(
            "saved pool baseline could not be verified; old global database was not replayed",
        ));
    }
    stable_prepared_readback(pending, checkpoint)
}

fn stable_prepared_readback(pending: &mut Pending, checkpoint: Checkpoint<'_>) -> Result<()> {
    idle(checkpoint)?;
    let (instance, config, expected_xml, count, inf, original) =
        if let Some(existing) = &pending.existing {
            (
                existing.instance.clone(),
                existing.config.clone(),
                existing.change.after.clone(),
                existing.change.count,
                Some(existing.inf.clone()),
                Some(existing.original),
            )
        } else {
            let (instance, directory) = pending.installed.clone().ok_or_else(|| {
                fail("new installation identity is unknown during final readback")
            })?;
            (
                instance,
                directory.join("vdd_settings.xml"),
                pending.install_xml.clone(),
                pending.minimum,
                None,
                None,
            )
        };
    let devices = DeviceSet::displays()?;
    let mut candidates = mtt(&devices)?;
    if candidates.len() != 1 || candidates[0].instance != instance {
        return Err(fail(
            "prepared driver instance set changed during final readback",
        ));
    }
    let candidate = candidates.pop().unwrap();
    let inf = verify_driver(&devices, &candidate, inf.as_deref())?;
    let original = original.unwrap_or(candidate.original);
    let started = Instant::now();
    let mut window = capacity_policy::StableReadback::default();
    let mut initial_keys = None;
    let mut final_signature_verified = false;
    loop {
        idle(checkpoint)?;
        let devices = DeviceSet::displays()?;
        let candidates = mtt(&devices)?;
        if candidates.len() != 1 || candidates[0].instance != instance {
            return Err(fail(
                "prepared driver instance set changed during stable readback",
            ));
        }
        let candidate = open(&devices, &instance, original)?;
        healthy(&devices, &candidate, true)?;
        if driver_value(&devices, &candidate.device, "InfPath")? != inf
            || driver_value(&devices, &candidate.device, "DriverVersion")?
                != crate::virtual_display::driver_package().driver_version
            || configuration()? != config
            || store::read(&config)? != expected_xml
        {
            return Err(fail(
                "prepared driver/configuration binding changed during stable readback",
            ));
        }
        let targets = targets_for_instance(&instance)?;
        claim_targets(&targets, &mut pending.claims)?;
        let keys: BTreeSet<_> = targets.iter().map(|target| target.key.clone()).collect();
        let initial = initial_keys.get_or_insert_with(|| keys.clone());
        let saved = database()?;
        let owned = owned_keys(pending);
        let exact = targets.len() == count as usize
            && targets
                .iter()
                .all(|t| t.available && !t.active && t.actual.is_none())
            && keys == *initial
            && active_signature()? == pending.baseline
            && capacity_policy::unchanged_foreign(&pending.database, &saved, &owned)
            && !saved.keys().any(|key| owned.contains(key));
        let done=window.observe(started.elapsed(),exact).map_err(|_|fail("pool/current/saved-foreign readback drifted during the strict stable window; ownership remains retained and no topology was rewritten"))?;
        if done {
            // Rebind actual driver-store bytes/catalog after the observation
            // window too; the cheap per-sample INF/version is not its signature.
            if final_signature_verified {
                return Ok(());
            }
            verify_driver(&devices, &candidate, Some(&inf))?;
            final_signature_verified = true;
            // Signature/revocation checks may block. Take one more complete
            // fresh readback afterwards rather than release a stale proof.
            continue;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn verify_saved_completion(pending: &mut Pending, checkpoint: Checkpoint<'_>) -> Result<()> {
    stable_prepared_readback(pending, checkpoint)?;
    pending
        .journal
        .complete("committed_after_verified_readback")
}

fn compensate(pending: &mut Pending, checkpoint: Checkpoint<'_>) -> Result<()> {
    idle(checkpoint)?;
    if pending.progress.recovery(true, true) == capacity_policy::Recovery::VerifySavedReadback {
        return verify_saved_completion(pending, checkpoint);
    }
    if pending.existing.is_none() {
        let (instance,directory)=pending.installed.as_ref().ok_or_else(||fail("first-install identity remains unknown; no partial device was adopted automatically"))?.clone();
        let devices = DeviceSet::displays()?;
        let candidate = mtt(&devices)?
            .into_iter()
            .find(|candidate| candidate.instance == instance)
            .ok_or_else(|| {
                fail("owned first-install instance is unavailable for exact recovery")
            })?;
        verify_driver(&devices, &candidate, None)?;
        if configuration()? != directory.join("vdd_settings.xml") || !database_preserved(pending)? {
            return Err(fail("new installation configuration or saved foreign outputs changed before compensation"));
        }
        crate::virtual_display::install_windows::rollback_capacity_install(
            &instance,
            &directory,
            &pending.install_xml,
            &pending.baseline,
            checkpoint,
            &mut pending.claims,
        )?;
        if !database_preserved(pending)? {
            return Err(fail(
                "saved non-owned topology changed during first-install compensation",
            ));
        }
        return pending.journal.complete("installation_rolled_back");
    }
    let existing = pending.existing.as_ref().unwrap();
    let devices = DeviceSet::displays()?;
    let candidate = open(&devices, &existing.instance, existing.original)?;
    verify_driver(&devices, &candidate, Some(&existing.inf))?;
    if configuration()? != existing.config || !database_preserved(pending)? {
        return Err(fail(
            "configuration path or saved topology changed; recovery did not overwrite it",
        ));
    }
    let current = store::read(&existing.config)?;
    let restored = capacity_config::rollback_image(&current, &existing.change)?.to_vec();
    let original = existing.original;
    let config = existing.config.clone();
    let original_count = existing.original_targets;
    // The adapter may contain only our submitted new targets during recovery.
    // Reacquire every fresh identity before disabling; never borrow a claim.
    let targets = targets_for_instance(&candidate.instance)?;
    claim_targets(&targets, &mut pending.claims)?;
    idle(checkpoint)?;
    pending.journal.phase("rollback_disable_submitted")?;
    change(&devices, &candidate, false)?;
    healthy(&devices, &candidate, false)?;
    store::replace_exact(&config, &current, &restored)?;
    if !original.global() {
        idle(checkpoint)?;
        pending.journal.phase("rollback_enable_submitted")?;
        change(&devices, &candidate, true)?;
        inactive(pending, &devices, &candidate, original_count, checkpoint)?;
    }
    if disabled_state(&devices, &candidate.device)? != original
        || store::read(&config)? != restored
        || active_signature()? != pending.baseline
        || !database_preserved(pending)?
    {
        return Err(fail("original adapter/configuration or unaffected outputs could not be verified after compensation"));
    }
    pending.journal.complete("rolled_back")
}
pub(in crate::virtual_display) fn recover(checkpoint: Checkpoint<'_>) -> preparation::Result<()> {
    checkpoint()?;
    let _installer =
        MutationLock::acquire_named("Global\\EclipseVirtualDisplayDriverInstaller", 5000)
            .map_err(unchanged)?;
    let mut state = pending()
        .lock()
        .map_err(|_| preparation::Failure::pending("capacity recovery state is poisoned"))?;
    if let Some(transaction) = state.as_mut() {
        compensate(transaction, checkpoint)
            .map_err(|e| preparation::Failure::pending(format!("{}; {e}", transaction.failure)))?;
        state.take();
    } else if store::pending_exists().map_err(unchanged)? {
        return Err(preparation::Failure::pending("an interrupted capacity transaction has a durable journal; ownership was not inherited after process restart, so verified local recovery is required"));
    }
    drop(state);
    recover_pending_only().map_err(unchanged)
}

pub(in crate::virtual_display) fn prepare(
    minimum: u32,
    package: Option<&[(String, Vec<u8>)]>,
    checkpoint: Checkpoint<'_>,
) -> preparation::Result<()> {
    checkpoint()?;
    crate::service_admin::windows::require_administrator()
        .map_err(|e| preparation::Failure::unchanged(e.to_string()))?;
    let _installer =
        MutationLock::acquire_named("Global\\EclipseVirtualDisplayDriverInstaller", 5000)
            .map_err(unchanged)?;
    checkpoint()?;
    if recovery_pending().map_err(unchanged)? {
        return Err(preparation::Failure::pending(
            "an earlier capacity transaction still requires verified recovery",
        ));
    }
    let mut state = pending()
        .lock()
        .map_err(|_| preparation::Failure::pending("capacity recovery state is poisoned"))?;
    let devices = DeviceSet::displays().map_err(unchanged)?;
    let mut candidates = mtt(&devices).map_err(unchanged)?;
    if package.is_some() && !candidates.is_empty() {
        return Err(preparation::Failure::unchanged(
            "first-install intent found an existing Mtt adapter; it was not reloaded",
        ));
    }
    if candidates.len() > 1 {
        return Err(preparation::Failure::unchanged("multiple Mtt adapters share global configuration; automatic capacity changes were refused"));
    }
    let baseline = active_signature().map_err(unchanged)?;
    if baseline.is_empty() {
        return Err(preparation::Failure::unchanged("preserving at least one active original display is required to prepare an inactive pool"));
    }
    let saved = database().map_err(unchanged)?;
    let minimum = minimum.max(preparation::INITIAL_CAPACITY);
    let existing = if let Some(candidate) = candidates.pop() {
        let targets = targets_for_instance(&candidate.instance).map_err(unchanged)?;
        if targets.iter().any(|t| t.active || t.actual.is_some()) {
            return Err(preparation::Failure {
                message: "capacity preparation waits until every Mtt target is inactive".into(),
                outcome: preparation::FailureOutcome::AdmissionChanged,
            });
        }
        if reboot_pending(&devices, &candidate.device).map_err(unchanged)?
            || (!candidate.original.global() && problem(&candidate.device).map_err(unchanged)? != 0)
            || (candidate.original.global()
                && problem(&candidate.device).map_err(unchanged)? != CM_PROB_DISABLED)
        {
            return Err(preparation::Failure::unchanged(
                "Mtt adapter is not healthy or requires a Windows restart",
            ));
        }
        let inf = verify_driver(&devices, &candidate, None).map_err(unchanged)?;
        let config = configuration().map_err(unchanged)?;
        let change = capacity_config::expand(&store::read(&config).map_err(unchanged)?, minimum)
            .map_err(unchanged)?;
        if !candidate.original.global() && targets.len() != change.original_count as usize {
            return Err(preparation::Failure::unchanged("the enabled driver target count does not match its XML; the loaded configuration is unknown and was not restarted automatically"));
        }
        Some(Existing {
            instance: candidate.instance,
            original: candidate.original,
            inf,
            config,
            change,
            original_targets: targets.len(),
        })
    } else {
        None
    };
    let mut claims = BTreeMap::new();
    if let Some(existing) = &existing {
        claim_targets(
            &targets_for_instance(&existing.instance).map_err(unchanged)?,
            &mut claims,
        )
        .map_err(unchanged)?;
    }
    checkpoint()?;
    let original = existing
        .as_ref()
        .map(|e| e.change.before.as_slice())
        .or_else(|| {
            package.and_then(|files| {
                files
                    .iter()
                    .find(|(name, _)| name == "vdd_settings.xml")
                    .map(|(_, bytes)| bytes.as_slice())
            })
        })
        .unwrap_or(&[]);
    let record = serde_json::json!({"instance":existing.as_ref().map(|e|&e.instance),"inf":existing.as_ref().map(|e|&e.inf),"config":existing.as_ref().map(|e|e.config.to_string_lossy()),"count_before":existing.as_ref().map(|e|e.change.original_count),"count_requested":minimum,"active_before":json_signature(&baseline),"database_before":json_signature(&saved),"config_before_sha256":hex::encode(Sha256::digest(original)),"driver_version":crate::virtual_display::driver_package().driver_version});
    let install_xml = if existing.is_none() {
        capacity_config::expand(original, minimum)
            .map_err(unchanged)?
            .after
    } else {
        Vec::new()
    };
    let journal = store::Journal::create(original, record).map_err(|e| {
        if store::pending_exists().unwrap_or(true) {
            preparation::Failure::pending(e.to_string())
        } else {
            unchanged(e)
        }
    })?;
    *state = Some(Pending {
        existing,
        installed: None,
        install_xml,
        minimum,
        baseline,
        database: saved,
        claims,
        journal,
        progress: capacity_policy::Progress::Unchanged,
        failure: "capacity transaction interrupted".into(),
    });
    let transaction = state.as_mut().expect("registered capacity ownership");
    let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||->Result<()> {
        if transaction.existing.is_none() {
            let package=package.ok_or_else(||fail("first driver installation has no verified pinned package"))?;
            transaction.journal.phase("installation_submitted")?;
            transaction.progress=capacity_policy::Progress::AdapterSubmitted;
            let installed = crate::virtual_display::install_windows::install_capacity_on_desktop(
                package, minimum, checkpoint, &mut transaction.claims,
                &mut |instance, directory| {
                    if !instance.is_empty() {
                        transaction.installed = Some((instance.into(), directory.into()));
                    }
                    transaction.journal.record("installation_directory", directory.to_string_lossy().to_string().into())?;
                    if !instance.is_empty() {
                        transaction.journal.record("instance", instance.into())?;
                    }
                    Ok(())
                },
            );
            if let Err(failure)=installed {
                if failure.cleanup_verified {
                    if active_signature()?!=transaction.baseline||!database_preserved(transaction)? {return Err(fail(format!("{}; first-install active/saved configuration compensation is not proven",failure.error)));}
                    transaction.journal.complete("installation_rolled_back")?;
                    return Err(failure.error);
                }
                return Err(fail(format!("{}; first-install ownership recovery remains pending",failure.error)));
            }
            let fresh=mtt(&DeviceSet::displays()?)?;
            if fresh.len()!=1||transaction.installed.as_ref().is_none_or(|(instance,_)|fresh[0].instance!=*instance){return Err(fail("new installation did not publish the exact owned Mtt instance"));}
            save_pool(transaction,&fresh[0].instance,checkpoint)?;
        } else {
            let existing=transaction.existing.as_ref().unwrap();
            let candidate=open(&devices,&existing.instance,existing.original)?;
            let config=existing.config.clone();let before=existing.change.before.clone();let after=existing.change.after.clone();let count=existing.change.count as usize;
            let original=existing.original;
            let targets=fresh_idle(transaction,&candidate,checkpoint)?;
            verify_driver(&devices,&candidate,transaction.existing.as_ref().map(|e|e.inf.as_str()))?;
            if configuration()?!=config||disabled_state(&devices,&candidate.device)?!=original||store::read(&config)?!=before||active_signature()?!=transaction.baseline||database()?!=transaction.database {return Err(fail("last zero-write capacity preflight changed"));}
            if targets.len()==count&&targets.iter().all(|t|t.available)&&!original.global()&&before==after {
                save_pool(transaction,&candidate.instance,checkpoint)?;
            } else {
                transaction.journal.phase("disable_submitted")?;
                idle(checkpoint)?;
                transaction.progress=capacity_policy::Progress::AdapterSubmitted;
                if !original.global(){change(&devices,&candidate,false)?;}
                healthy(&devices,&candidate,false)?;
                if active_signature()?!=transaction.baseline||!database_preserved(transaction)? {return Err(fail("unaffected active/saved outputs changed when disabling Mtt"));}
                idle(checkpoint)?;store::replace_exact(&config,&before,&after)?;
                transaction.journal.phase("configuration_committed")?;
                idle(checkpoint)?;
                transaction.journal.phase("enable_submitted")?;
                change(&devices,&candidate,true)?;
                inactive(transaction,&devices,&candidate,count,checkpoint)?;
                if configuration()?!=config||store::read(&config)?!=after {return Err(fail("Mtt configuration changed during driver startup"));}
                save_pool(transaction,&candidate.instance,checkpoint)?;
            }
        }
        transaction.journal.complete("committed")
    })).unwrap_or_else(|_|Err(fail("capacity transaction panicked; exact recovery is required")));
    match result {
        Ok(()) => {
            state.take();
            Ok(())
        }
        Err(error) => {
            transaction.failure = error.to_string();
            if transaction.progress == capacity_policy::Progress::Unchanged {
                transaction
                    .journal
                    .complete("aborted_before_mutation")
                    .map_err(|e| preparation::Failure::pending(e.to_string()))?;
                state.take();
                return Err(unchanged(error));
            }
            // A fully compensated first-install failure has already archived
            // its marker; do not manufacture another node or a second rollback.
            if transaction.existing.is_none() && !store::pending_exists().map_err(unchanged)? {
                state.take();
                return Err(unchanged(error));
            }
            if transaction.progress.recovery(true, true)
                == capacity_policy::Recovery::VerifySavedReadback
            {
                return Err(preparation::Failure::pending(format!("{error}; final pool readback remains pending; exact claims and journal are retained without another SET/SAVE")));
            }
            match compensate(transaction,checkpoint) {
                Ok(())=>{state.take();Err(unchanged(error))}
                Err(recovery)=>Err(preparation::Failure::pending(format!("{error}; exact capacity recovery remains pending and target claims are retained: {recovery}"))),
            }
        }
    }
}
