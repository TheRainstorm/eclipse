//! Exact Mtt adapter primitives and disabled-adapter recovery. The separate
//! capacity child owns idle-host count/configuration changes; the legacy
//! enable-only path below never changes XML, restarts Zako, or adopts a screen.

use super::*;
use std::{
    sync::{Mutex, OnceLock},
    time::Instant,
};
use windows_sys::Win32::System::Registry::REG_DWORD;

const SETTLE: Duration = Duration::from_millis(500);
const STARTUP: Duration = Duration::from_secs(10);

#[path = "capacity_windows.rs"]
pub(in crate::virtual_display) mod capacity;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DisabledState {
    global_flags: u32,
    profile: u32,
}

impl DisabledState {
    fn global(self) -> bool {
        self.global_flags & CONFIGFLAG_DISABLED != 0
    }
}

struct Candidate {
    device: SP_DEVINFO_DATA,
    instance: String,
    original: DisabledState,
}

// Keep only portable identities/state across workers. HDEVINFO and its device
// elements belong to the current SetupAPI inventory and are rebuilt on retry.
// A failed rollback must not drop the claims that protect its partial work.
struct PendingRollback {
    instance: String,
    original: DisabledState,
    baseline: Fingerprints,
    claims: BTreeMap<TargetKey, Box<dyn Claim>>,
    failure: String,
}

fn pending() -> &'static Mutex<Option<PendingRollback>> {
    static PENDING: OnceLock<Mutex<Option<PendingRollback>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(None))
}

fn is_mtt(devices: &DeviceSet, device: &SP_DEVINFO_DATA) -> Result<bool> {
    let mut ids = [0u16; 2048];
    win_ok(
        unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                devices.0,
                device,
                SPDRP_HARDWAREID,
                null_mut(),
                ids.as_mut_ptr().cast(),
                size_of_val(&ids) as u32,
                null_mut(),
            )
        },
        "SetupDiGetDeviceRegistryPropertyW(virtual adapter hardware ID)",
    )?;
    Ok(ids.split(|v| *v == 0).filter(|s| !s.is_empty()).any(|id| {
        matches!(
            String::from_utf16_lossy(id).to_ascii_lowercase().as_str(),
            "root\\mttvdd" | "mttvdd"
        )
    }))
}

fn reopen(devices: &DeviceSet, pending: &PendingRollback) -> Result<Candidate> {
    let instance: Vec<u16> = pending.instance.encode_utf16().chain([0]).collect();
    let mut device: SP_DEVINFO_DATA = unsafe { zeroed() };
    device.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
    win_ok(
        unsafe { SetupDiOpenDeviceInfoW(devices.0, instance.as_ptr(), null_mut(), 0, &mut device) },
        "SetupDiOpenDeviceInfoW(pending virtual adapter rollback)",
    )?;
    if devices.instance(&device)? != pending.instance || !is_mtt(devices, &device)? {
        return Err(Error::Platform(
            "pending virtual adapter identity no longer matches the original Mtt instance".into(),
        ));
    }
    Ok(Candidate {
        device,
        instance: pending.instance.clone(),
        original: pending.original,
    })
}

fn win_ok(value: i32, operation: &str) -> Result<()> {
    if value == 0 {
        Err(error(operation, unsafe { GetLastError() }))
    } else {
        Ok(())
    }
}

fn current_profile() -> Result<u32> {
    let mut profiles = [0u32; 64];
    let (mut size, mut active) = (0, u32::MAX);
    win_ok(
        unsafe {
            SetupDiGetHwProfileList(
                profiles.as_mut_ptr(),
                profiles.len() as u32,
                &mut size,
                &mut active,
            )
        },
        "SetupDiGetHwProfileList",
    )?;
    profiles
        .get(active as usize)
        .copied()
        .filter(|_| active < size)
        .ok_or_else(|| Error::Platform("current hardware profile is unavailable".into()))
}

fn disabled_state(devices: &DeviceSet, candidate: &SP_DEVINFO_DATA) -> Result<DisabledState> {
    let profile = current_profile()?;
    let mut global_flags = 0u32;
    let mut kind = 0;
    let mut size = 0;
    win_ok(
        unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                devices.0,
                candidate,
                SPDRP_CONFIGFLAGS,
                &mut kind,
                (&mut global_flags as *mut u32).cast(),
                size_of::<u32>() as u32,
                &mut size,
            )
        },
        "SetupDiGetDeviceRegistryPropertyW(CONFIGFLAGS)",
    )?;
    if kind != REG_DWORD || size != size_of::<u32>() as u32 {
        return Err(Error::Platform(
            "invalid virtual adapter configuration flags".into(),
        ));
    }
    Ok(DisabledState {
        global_flags,
        profile,
    })
}

fn problem(device: &SP_DEVINFO_DATA) -> Result<u32> {
    let (mut status, mut problem) = (0, 0);
    let code = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, device.DevInst, 0) };
    if code == CR_SUCCESS {
        Ok(problem)
    } else {
        Err(error("CM_Get_DevNode_Status", code))
    }
}

fn reboot_pending(devices: &DeviceSet, device: &SP_DEVINFO_DATA) -> Result<bool> {
    let mut parameters: SP_DEVINSTALL_PARAMS_W = unsafe { zeroed() };
    parameters.cbSize = size_of::<SP_DEVINSTALL_PARAMS_W>() as u32;
    win_ok(
        unsafe { SetupDiGetDeviceInstallParamsW(devices.0, device, &mut parameters) },
        "SetupDiGetDeviceInstallParamsW(virtual adapter state)",
    )?;
    Ok(parameters.Flags & (DI_NEEDREBOOT | DI_NEEDRESTART) != 0)
}

fn candidates(devices: &DeviceSet) -> Result<Vec<Candidate>> {
    let mut found = Vec::new();
    for index in 0..4096 {
        let mut device: SP_DEVINFO_DATA = unsafe { zeroed() };
        device.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
        if unsafe { SetupDiEnumDeviceInfo(devices.0, index, &mut device) } == 0 {
            let code = unsafe { GetLastError() };
            return if code == ERROR_NO_MORE_ITEMS {
                Ok(found)
            } else {
                Err(error("SetupDiEnumDeviceInfo(provision)", code))
            };
        }
        if !is_mtt(devices, &device).unwrap_or(false) || problem(&device)? != CM_PROB_DISABLED {
            continue;
        }
        let original = disabled_state(devices, &device)?;
        if !original.global() {
            // Profile-specific disable flags have no supported retrieval API
            // on modern Windows (the old CM_Get_HW_Prof_Flags is deprecated).
            // Do not clear a state whose scope cannot be restored precisely.
            continue;
        }
        if reboot_pending(devices, &device)? {
            return Err(Error::Platform(
                "Windows must restart before preparing the disabled virtual adapter".into(),
            ));
        }
        let instance = devices.instance(&device)?;
        found.push(Candidate {
            device,
            instance,
            original,
        });
    }
    Err(Error::Platform("too many display adapter devices".into()))
}

fn change(devices: &DeviceSet, candidate: &Candidate, enable: bool) -> Result<()> {
    if current_profile()? != candidate.original.profile {
        return Err(Error::Platform(
            "hardware profile changed during virtual adapter preparation".into(),
        ));
    }
    let parameters = SP_PROPCHANGE_PARAMS {
        ClassInstallHeader: SP_CLASSINSTALL_HEADER {
            cbSize: size_of::<SP_CLASSINSTALL_HEADER>() as u32,
            InstallFunction: DIF_PROPERTYCHANGE,
        },
        StateChange: if enable { DICS_ENABLE } else { DICS_DISABLE },
        Scope: DICS_FLAG_GLOBAL,
        HwProfile: 0,
    };
    win_ok(
        unsafe {
            SetupDiSetClassInstallParamsW(
                devices.0,
                &candidate.device,
                &parameters.ClassInstallHeader,
                size_of::<SP_PROPCHANGE_PARAMS>() as u32,
            )
        },
        "SetupDiSetClassInstallParamsW(virtual adapter state)",
    )?;
    // Let class/co-installers participate. SetupDiChangeState is the default
    // handler and must not be called directly by an application.
    let result = win_ok(
        unsafe { SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, devices.0, &candidate.device) },
        "DIF_PROPERTYCHANGE(only the disabled virtual adapter)",
    );
    let clear = win_ok(
        unsafe { SetupDiSetClassInstallParamsW(devices.0, &candidate.device, null(), 0) },
        "clear virtual adapter class-install parameters",
    );
    result.and(clear)
}

fn claim_targets(
    targets: &[Target],
    claims: &mut BTreeMap<TargetKey, Box<dyn Claim>>,
) -> Result<()> {
    for target in targets {
        if !claims.contains_key(&target.key) {
            claims.insert(
                target.key.clone(),
                super::super::ownership_windows::claim(&target.key)?,
            );
        }
    }
    Ok(())
}

fn verify_enable_effects(baseline: &Fingerprints, fresh: &Scene, instance: &str) -> Result<()> {
    use super::super::provision_policy::{check_enabled, Violation};
    let current = fingerprints(&fresh.active, None)?;
    let candidate = current
        .keys()
        .filter(|key| {
            fresh
                .supported_adapters
                .get(&key.adapter)
                .is_some_and(|owner| owner == instance)
        })
        .cloned()
        .collect();
    let Err(violation) = check_enabled(baseline, &current, &candidate) else {
        return Ok(());
    };
    let key = match &violation {
        Violation::ExistingChanged(key) | Violation::ForeignActivated(key) => key,
    };
    let name = fresh
        .active
        .paths
        .iter()
        .find_map(|path| {
            target_key(path)
                .ok()
                .filter(|(found, _)| found == key)
                .map(|(_, name)| name)
        })
        .filter(|name| !name.is_empty());
    let output = name.map_or_else(
        || key.monitor_path.clone(),
        |name| format!("{name} ({})", key.monitor_path),
    );
    let reason = match violation {
        Violation::ExistingChanged(_) => "an existing display changed during virtual adapter preparation",
        Violation::ForeignActivated(_) => "a display outside the prepared adapter became active during virtual adapter preparation",
    };
    Err(Error::Platform(format!("{reason}: {output}; automatic preparation stopped before taking over that display or replaying its layout")))
}

fn rollback(
    devices: &DeviceSet,
    candidate: &Candidate,
    baseline: &Fingerprints,
    claims: &mut BTreeMap<TargetKey, Box<dyn Claim>>,
) -> Result<()> {
    let targets = targets_for_instance(&candidate.instance)?;
    // A previous cooperating owner can still hold the identity of a newly
    // published target. Never disable its adapter until every target is owned.
    claim_targets(&targets, claims)?;
    // Preserve fresh external state, never replay a cached global topology.
    // Disabling our adapter can also naturally undo its extra output effects;
    // exact equality with the original baseline proves that recovery too.
    let mut preserved = active_signature()?;
    for target in &targets {
        preserved.remove(&target.key);
    }
    if disabled_state(devices, &candidate.device)? != candidate.original
        || problem(&candidate.device)? != CM_PROB_DISABLED
    {
        change(devices, candidate, false)?;
    }
    if reboot_pending(devices, &candidate.device)? {
        return Err(Error::Platform(
            "Windows requires a restart to finish the virtual adapter rollback".into(),
        ));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let targets = targets_for_instance(&candidate.instance)?;
        claim_targets(&targets, claims)?;
        if disabled_state(devices, &candidate.device)? == candidate.original
            && problem(&candidate.device)? == CM_PROB_DISABLED
            && !targets.iter().any(|target| target.active)
            && super::super::provision_policy::rollback_matches(
                baseline,
                &preserved,
                &active_signature()?,
            )
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::Platform(
                "original disabled adapter state or preserved active outputs could not be verified"
                    .into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn pending_error(pending: &PendingRollback, rollback: Error) -> Error {
    Error::Platform(format!(
        "{}; exact adapter rollback remains pending and its claims are retained: {rollback}",
        pending.failure
    ))
}

fn recover_pending(
    pending: &mut Option<PendingRollback>,
    restore: impl FnOnce(&mut PendingRollback) -> Result<()>,
) -> Result<()> {
    let Some(previous) = pending.as_mut() else {
        return Ok(());
    };
    if let Err(error) = restore(previous) {
        return Err(pending_error(previous, error));
    }
    pending.take();
    Ok(())
}

/// Common mutation admission, including reuse of an already inactive target.
/// A delayed target from partial preparation must not bypass exact rollback.
/// With no partial mutation this is a no-op for an ordinary frontend.
pub(super) fn recover_pending_only() -> Result<()> {
    if pending()
        .lock()
        .map_err(|_| Error::Platform("virtual adapter rollback state is poisoned".into()))?
        .is_none()
    {
        return Ok(());
    }
    crate::service_admin::windows::require_administrator()
        .map_err(|_| Error::AdministratorRequired)?;
    let _installer =
        MutationLock::acquire_named("Global\\EclipseVirtualDisplayDriverInstaller", 5_000)?;
    let mut pending = pending()
        .lock()
        .map_err(|_| Error::Platform("virtual adapter rollback state is poisoned".into()))?;
    let devices = DeviceSet::displays()?;
    recover_pending(&mut pending, |previous| {
        reopen(&devices, previous).and_then(|candidate| {
            rollback(
                &devices,
                &candidate,
                &previous.baseline,
                &mut previous.claims,
            )
        })
    })
}

/// Called by the desktop worker with its layout mutation lock held.
pub(super) fn prepare() -> Result<()> {
    crate::service_admin::windows::require_administrator()
        .map_err(|_| Error::AdministratorRequired)?;
    let _installer =
        MutationLock::acquire_named("Global\\EclipseVirtualDisplayDriverInstaller", 5_000)?;
    let mut pending = pending()
        .lock()
        .map_err(|_| Error::Platform("virtual adapter rollback state is poisoned".into()))?;
    let devices = DeviceSet::displays()?;
    recover_pending(&mut pending, |previous| {
        reopen(&devices, previous).and_then(|candidate| {
            rollback(
                &devices,
                &candidate,
                &previous.baseline,
                &mut previous.claims,
            )
        })
    })?;
    for candidate in candidates(&devices)? {
        let existing = targets_for_instance(&candidate.instance)?;
        if existing.iter().any(|target| target.active) {
            continue;
        }
        let mut claims = BTreeMap::new();
        match claim_targets(&existing, &mut claims) {
            Err(Error::Claimed) => continue,
            result => result?,
        }
        let before = active_signature()?;
        // Precondition failures are zero-write: do not "roll back" a state
        // changed by another process before our first mutation.
        if disabled_state(&devices, &candidate.device)? != candidate.original
            || problem(&candidate.device)? != CM_PROB_DISABLED
            || targets_for_instance(&candidate.instance)?
                .iter()
                .any(|target| target.active)
        {
            return Err(Error::Platform(
                "virtual adapter state changed before preparation".into(),
            ));
        }
        *pending = Some(PendingRollback {
            instance: candidate.instance.clone(),
            original: candidate.original,
            baseline: before.clone(),
            claims,
            failure: "virtual adapter preparation was interrupted".into(),
        });
        let operation = pending
            .as_mut()
            .expect("registered virtual adapter mutation");
        let result = (|| -> Result<()> {
            change(&devices, &candidate, true)?;
            if reboot_pending(&devices, &candidate.device)? {
                return Err(Error::Platform(
                    "Windows requires a restart to enable the virtual adapter".into(),
                ));
            }
            let deadline = Instant::now() + STARTUP;
            let mut last_keys = BTreeSet::new();
            let mut stable_since = Instant::now();
            loop {
                let fresh = scene()?;
                let targets: Vec<_> = inventory(&fresh)?
                    .targets
                    .into_iter()
                    .filter(|target| {
                        fresh.supported_adapters.get(&target.key.adapter)
                            == Some(&candidate.instance)
                    })
                    .collect();
                claim_targets(&targets, &mut operation.claims)?;
                verify_enable_effects(&before, &fresh, &candidate.instance)?;
                let keys: BTreeSet<_> = targets.iter().map(|target| target.key.clone()).collect();
                if keys != last_keys {
                    last_keys = keys;
                    stable_since = Instant::now();
                }
                if !targets.is_empty() && stable_since.elapsed() >= SETTLE {
                    for target in &targets {
                        deactivate(&target.key)?;
                    }
                    let after = targets_for_instance(&candidate.instance)?;
                    let after_keys: BTreeSet<_> =
                        after.iter().map(|target| target.key.clone()).collect();
                    if after_keys == last_keys
                        && !after.iter().any(|target| target.active)
                        && after.iter().any(|target| target.available)
                    {
                        let enabled = DisabledState {
                            global_flags: candidate.original.global_flags & !CONFIGFLAG_DISABLED,
                            profile: candidate.original.profile,
                        };
                        if disabled_state(&devices, &candidate.device)? != enabled
                            || problem(&candidate.device)? != 0
                        {
                            return Err(Error::Platform(
                                "virtual adapter did not enter the requested enabled state".into(),
                            ));
                        }
                        if active_signature()? != before {
                            return Err(Error::Platform("another display changed while enabling the virtual adapter; its layout was not replayed".into()));
                        }
                        return Ok(());
                    }
                    stable_since = Instant::now();
                }
                if Instant::now() >= deadline {
                    return Err(Error::Platform("the enabled virtual adapter did not provide a stable inactive display within 10 seconds".into()));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })();
        if let Err(failure) = result {
            operation.failure = failure.to_string();
            recover_pending(&mut pending, |previous| {
                rollback(
                    &devices,
                    &candidate,
                    &previous.baseline,
                    &mut previous.claims,
                )
            })?;
            return Err(failure);
        }
        pending.take();
        return Ok(());
    }
    Err(Error::NoAvailableSlot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    struct CountedClaim(Arc<AtomicUsize>);
    impl Claim for CountedClaim {}
    impl Drop for CountedClaim {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn key(target: u32) -> TargetKey {
        TargetKey {
            adapter: (7, 9),
            target,
            monitor_path: format!("generated-pending-target-{target}"),
        }
    }

    #[test]
    fn rollback_retry_retains_original_and_new_claims_until_exact_recovery_succeeds() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let mut claims: BTreeMap<TargetKey, Box<dyn Claim>> = BTreeMap::new();
        claims.insert(key(1), Box::new(CountedClaim(dropped.clone())));
        let original = DisabledState {
            global_flags: CONFIGFLAG_DISABLED | 0x40,
            profile: 11,
        };
        let mut pending = Some(PendingRollback {
            instance: "generated-exact-mtt-instance".into(),
            original,
            baseline: BTreeMap::new(),
            claims,
            failure: "generated activation failure".into(),
        });
        let failed = recover_pending(&mut pending, |previous| {
            assert_eq!(previous.instance, "generated-exact-mtt-instance");
            assert_eq!(previous.original, original);
            previous
                .claims
                .insert(key(2), Box::new(CountedClaim(dropped.clone())));
            Err(Error::Claimed)
        });
        assert!(failed.is_err());
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        assert_eq!(pending.as_ref().unwrap().claims.len(), 2);
        // A later worker must recover the same instance with both claims live.
        recover_pending(&mut pending, |previous| {
            assert_eq!(previous.instance, "generated-exact-mtt-instance");
            assert_eq!(previous.original, original);
            assert_eq!(previous.claims.len(), 2);
            assert_eq!(dropped.load(Ordering::SeqCst), 0);
            Ok(())
        })
        .unwrap();
        assert!(pending.is_none());
        assert_eq!(dropped.load(Ordering::SeqCst), 2);
        recover_pending(&mut pending, |_| panic!("already recovered")).unwrap();
    }
}
