//! Original Windows CCD/SetupAPI adapter. No Sunshine implementation is copied.
//! Driver-global commands deliberately have no place in this backend.

use super::*;
use std::{
    collections::{BTreeSet, HashSet},
    mem::{size_of, size_of_val, zeroed},
    ptr::{null, null_mut},
    time::Duration,
};
use windows_sys::{
    core::GUID,
    Win32::{
        Devices::{DeviceAndDriverInstallation::*, Display::*},
        Foundation::{
            CloseHandle, GetLastError, LocalFree, ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_ITEMS,
            HANDLE, INVALID_HANDLE_VALUE, LUID, POINTL, WAIT_ABANDONED, WAIT_OBJECT_0,
            WAIT_TIMEOUT,
        },
        Graphics::Gdi::{DISPLAYCONFIG_PATH_ACTIVE, DISPLAYCONFIG_PATH_MODE_IDX_INVALID},
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            SECURITY_ATTRIBUTES,
        },
        System::Threading::{
            CreateMutexExW, ReleaseMutex, WaitForSingleObject, MUTEX_MODIFY_STATE,
        },
    },
};

#[path = "color_windows.rs"]
mod color_api;
#[path = "provision_windows.rs"]
mod provision;

pub(super) struct Windows;

pub(super) struct MutationLock(HANDLE);
impl MutationLock {
    fn acquire() -> Result<Self> {
        Self::acquire_named("Local\\EclipseVirtualDisplayManager", 5_000)
    }
    pub(super) fn acquire_named(name: &str, timeout: u32) -> Result<Self> {
        let name: Vec<_> = name.encode_utf16().chain([0]).collect();
        let sddl: Vec<_> = super::ownership_windows::MUTEX_SECURITY
            .encode_utf16()
            .chain([0])
            .collect();
        let mut descriptor = null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err(error("virtual display mutex security", unsafe {
                GetLastError()
            }));
        }
        let security = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let handle = unsafe {
            CreateMutexExW(
                &security,
                name.as_ptr(),
                0,
                0x0010_0000 | MUTEX_MODIFY_STATE,
            )
        };
        let create_error = unsafe { GetLastError() };
        unsafe {
            LocalFree(descriptor);
        }
        if handle.is_null() {
            return Err(error("CreateMutexExW(virtual displays)", create_error));
        }
        // Installation can wait for UMDF startup. A contending process fails
        // closed after a bounded wait rather than mutating a stale inventory.
        let state = unsafe { WaitForSingleObject(handle, timeout) };
        if state == WAIT_OBJECT_0 || state == WAIT_ABANDONED {
            return Ok(Self(handle));
        }
        unsafe {
            CloseHandle(handle);
        }
        if state == WAIT_TIMEOUT {
            return Err(Error::Platform(
                "virtual display manager is busy; retry after the current operation".into(),
            ));
        }
        Err(error("waiting for the virtual display owner mutex", state))
    }
}
impl Drop for MutationLock {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

pub(super) fn worker<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    // A GUI thread already owns HWNDs and cannot switch desktops. The service
    // also needs to bind to its authorized interactive desktop, not Session 0.
    std::thread::Builder::new()
        .name("eclipse-virtual-display".into())
        .spawn(move || {
            uu_input::windows_desktop::sync_input_desktop()
                .map_err(|error| Error::Platform(error.to_string()))?;
            let _coordinates = uu_input::windows_desktop::PhysicalCoordinates::enter()
                .map_err(|error| Error::Platform(error.to_string()))?;
            let _serialized = MutationLock::acquire()?;
            Ok(f())
        })
        .map_err(|error| Error::Platform(error.to_string()))?
        .join()
        .map_err(|_| Error::Platform("virtual display worker panicked".into()))?
}

impl Backend for Windows {
    fn recover(&self) -> Result<()> {
        worker(provision::recover_pending_only)?
    }
    fn claim(&self, key: &TargetKey) -> Result<Box<dyn Claim>> {
        super::ownership_windows::claim(key)
    }
    fn is_claimed(&self, key: &TargetKey) -> Result<bool> {
        super::ownership_windows::is_claimed(key)
    }
    fn provision(&self) -> Result<()> {
        worker(provision::prepare)?
    }
    fn inventory(&self) -> Result<Inventory> {
        worker(|| inventory(&scene()?))?
    }
    fn activate(
        &self,
        key: &TargetKey,
        spec: Spec,
    ) -> std::result::Result<Target, ActivationFailure> {
        let key = key.clone();
        worker(move || activate(&key, spec)).unwrap_or_else(|error| {
            Err(ActivationFailure {
                message: error.to_string(),
                activated: false,
            })
        })
    }
    fn deactivate(&self, key: &TargetKey) -> Result<()> {
        let key = key.clone();
        worker(move || deactivate(&key))?
    }
    fn prepare(&self, key: &TargetKey) -> Result<()> {
        let key = key.clone();
        worker(move || deactivate_checked(&key, true))?
    }
}

fn error(operation: &str, code: u32) -> Error {
    Error::Platform(format!("{operation}: Windows error {code}"))
}
fn text(value: &[u16]) -> String {
    String::from_utf16_lossy(&value[..value.iter().position(|v| *v == 0).unwrap_or(value.len())])
}
fn adapter(value: LUID) -> (u32, i32) {
    (value.LowPart, value.HighPart)
}
fn header<T>(kind: i32, adapter_id: LUID, id: u32) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: kind,
        size: size_of::<T>() as u32,
        adapterId: adapter_id,
        id,
    }
}
fn get_info<T>(value: &mut T) -> Result<()> {
    let code = unsafe { DisplayConfigGetDeviceInfo((value as *mut T).cast()) };
    if code == 0 {
        Ok(())
    } else {
        Err(error("DisplayConfigGetDeviceInfo", code as u32))
    }
}

struct DeviceSet(HDEVINFO);
impl DeviceSet {
    fn displays() -> Result<Self> {
        let class = GUID {
            data1: 0x4d36e968,
            data2: 0xe325,
            data3: 0x11ce,
            data4: [0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18],
        };
        let handle = unsafe { SetupDiGetClassDevsW(&class, null(), null_mut(), DIGCF_PRESENT) };
        if handle == INVALID_HANDLE_VALUE as isize {
            Err(error("SetupDiGetClassDevsW", unsafe { GetLastError() }))
        } else {
            Ok(Self(handle))
        }
    }
    fn instance(&self, device: &SP_DEVINFO_DATA) -> Result<String> {
        let mut buffer = [0u16; 512];
        if unsafe {
            SetupDiGetDeviceInstanceIdW(
                self.0,
                device,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                null_mut(),
            )
        } == 0
        {
            return Err(error("SetupDiGetDeviceInstanceIdW", unsafe {
                GetLastError()
            }));
        }
        Ok(text(&buffer).to_ascii_lowercase())
    }
    fn supported_instances(&self) -> Result<HashSet<String>> {
        let mut result = HashSet::new();
        for index in 0..4096 {
            let mut device: SP_DEVINFO_DATA = unsafe { zeroed() };
            device.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
            if unsafe { SetupDiEnumDeviceInfo(self.0, index, &mut device) } == 0 {
                let code = unsafe { GetLastError() };
                if code == ERROR_NO_MORE_ITEMS {
                    return Ok(result);
                }
                return Err(error("SetupDiEnumDeviceInfo", code));
            }
            let mut buffer = [0u16; 2048];
            if unsafe {
                SetupDiGetDeviceRegistryPropertyW(
                    self.0,
                    &device,
                    SPDRP_HARDWAREID,
                    null_mut(),
                    buffer.as_mut_ptr().cast(),
                    size_of_val(&buffer) as u32,
                    null_mut(),
                )
            } == 0
            {
                continue;
            }
            // Exact hardware IDs, never a friendly-name substring such as
            // "Virtual" (which would also claim GameViewer and other drivers).
            let known = buffer
                .split(|v| *v == 0)
                .filter(|part| !part.is_empty())
                .any(|part| {
                    matches!(
                        String::from_utf16_lossy(part).to_ascii_lowercase().as_str(),
                        "root\\mttvdd" | "mttvdd" | "root\\zakovdd" | "zakovdd"
                    )
                });
            if known {
                result.insert(self.instance(&device)?);
            }
        }
        Err(Error::Platform("too many display adapter devices".into()))
    }
    fn interface_instance(&self, path: &[u16]) -> Result<String> {
        let mut interface: SP_DEVICE_INTERFACE_DATA = unsafe { zeroed() };
        interface.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
        if unsafe { SetupDiOpenDeviceInterfaceW(self.0, path.as_ptr(), 0, &mut interface) } == 0 {
            return Err(error("SetupDiOpenDeviceInterfaceW", unsafe {
                GetLastError()
            }));
        }
        let mut required = 0;
        unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                self.0,
                &interface,
                null_mut(),
                0,
                &mut required,
                null_mut(),
            );
        }
        if required < 8 || required > 64 * 1024 {
            return Err(Error::Platform(
                "invalid adapter interface detail size".into(),
            ));
        }
        // u64 allocation preserves alignment for both Win32/Win64 structures.
        let mut storage = vec![0u64; (required as usize).div_ceil(8)];
        let detail = storage
            .as_mut_ptr()
            .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
        unsafe {
            (*detail).cbSize = if cfg!(target_pointer_width = "64") {
                8
            } else {
                6
            };
        }
        let mut device: SP_DEVINFO_DATA = unsafe { zeroed() };
        device.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
        if unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                self.0,
                &interface,
                detail,
                required,
                null_mut(),
                &mut device,
            )
        } == 0
        {
            return Err(error("SetupDiGetDeviceInterfaceDetailW", unsafe {
                GetLastError()
            }));
        }
        self.instance(&device)
    }
}
impl Drop for DeviceSet {
    fn drop(&mut self) {
        unsafe {
            SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

struct Topology {
    paths: Vec<DISPLAYCONFIG_PATH_INFO>,
    modes: Vec<DISPLAYCONFIG_MODE_INFO>,
}
fn query(flags: u32) -> Result<Topology> {
    for _ in 0..4 {
        let (mut paths_count, mut modes_count) = (0, 0);
        let code =
            unsafe { GetDisplayConfigBufferSizes(flags, &mut paths_count, &mut modes_count) };
        if code != 0 {
            return Err(error("GetDisplayConfigBufferSizes", code));
        }
        if paths_count > 4096 || modes_count > 8192 {
            return Err(Error::Platform(
                "display topology exceeds safety bound".into(),
            ));
        }
        let mut paths = vec![unsafe { zeroed() }; paths_count as usize];
        let mut modes = vec![unsafe { zeroed() }; modes_count as usize];
        let code = unsafe {
            QueryDisplayConfig(
                flags,
                &mut paths_count,
                paths.as_mut_ptr(),
                &mut modes_count,
                modes.as_mut_ptr(),
                null_mut(),
            )
        };
        if code == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if code != 0 {
            return Err(error("QueryDisplayConfig", code));
        }
        paths.truncate(paths_count as usize);
        modes.truncate(modes_count as usize);
        return Ok(Topology { paths, modes });
    }
    Err(Error::Platform(
        "display topology changed repeatedly while enumerating".into(),
    ))
}

fn target_key(path: &DISPLAYCONFIG_PATH_INFO) -> Result<(TargetKey, String)> {
    let mut name: DISPLAYCONFIG_TARGET_DEVICE_NAME = unsafe { zeroed() };
    name.header = header::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(
        DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
        path.targetInfo.adapterId,
        path.targetInfo.id,
    );
    get_info(&mut name)?;
    let monitor_path = text(&name.monitorDevicePath).to_ascii_lowercase();
    if monitor_path.is_empty() {
        return Err(Error::Platform("monitor identity is unavailable".into()));
    }
    Ok((
        TargetKey {
            adapter: adapter(path.targetInfo.adapterId),
            target: path.targetInfo.id,
            monitor_path,
        },
        text(&name.monitorFriendlyDeviceName),
    ))
}
fn source_name(path: &DISPLAYCONFIG_PATH_INFO) -> Result<String> {
    let mut name: DISPLAYCONFIG_SOURCE_DEVICE_NAME = unsafe { zeroed() };
    name.header = header::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
        path.sourceInfo.adapterId,
        path.sourceInfo.id,
    );
    get_info(&mut name)?;
    Ok(text(&name.viewGdiDeviceName))
}
fn color(path: &DISPLAYCONFIG_PATH_INFO) -> Result<(bool, bool)> {
    let state = color_api::read(path)?;
    Ok((state.hdr_supported, state.hdr_active))
}
fn set_color(path: &DISPLAYCONFIG_PATH_INFO, hdr: bool) -> Result<()> {
    color_api::set(path, hdr)
}

struct Scene {
    active: Topology,
    all: Topology,
    instances: HashSet<String>,
    supported_adapters: BTreeMap<(u32, i32), String>,
}
fn scene() -> Result<Scene> {
    let devices = DeviceSet::displays()?;
    let instances = devices.supported_instances()?;
    let all = query(QDC_ALL_PATHS)?;
    let active = query(QDC_ONLY_ACTIVE_PATHS)?;
    let mut supported_adapters = BTreeMap::new();
    let mut seen = BTreeSet::new();
    // An adapter can finish publishing between the two CCD queries. Resolve
    // exact PnP ownership for the freshest active set too; absence from the
    // earlier all-path sample must not turn our own adapter into a foreign one.
    for path in all.paths.iter().chain(active.paths.iter()) {
        let id = adapter(path.targetInfo.adapterId);
        if !seen.insert(id) {
            continue;
        }
        let mut name: DISPLAYCONFIG_ADAPTER_NAME = unsafe { zeroed() };
        name.header = header::<DISPLAYCONFIG_ADAPTER_NAME>(
            DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME,
            path.targetInfo.adapterId,
            0,
        );
        if get_info(&mut name).is_ok() {
            if let Ok(instance) = devices.interface_instance(&name.adapterDevicePath) {
                if instances.contains(&instance) {
                    supported_adapters.insert(id, instance);
                }
            }
        }
    }
    Ok(Scene {
        active,
        all,
        instances,
        supported_adapters,
    })
}
fn actual(
    path: &DISPLAYCONFIG_PATH_INFO,
    modes: &[DISPLAYCONFIG_MODE_INFO],
) -> Result<Option<Spec>> {
    if path.flags & DISPLAYCONFIG_PATH_ACTIVE == 0 {
        return Ok(None);
    }
    let index = unsafe { path.sourceInfo.Anonymous.modeInfoIdx };
    let mode = modes
        .get(index as usize)
        .filter(|mode| mode.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE)
        .ok_or_else(|| Error::Platform("active source has no validated source mode".into()))?;
    let source = unsafe { mode.Anonymous.sourceMode };
    let denominator = path.targetInfo.refreshRate.Denominator;
    if denominator == 0 {
        return Err(Error::Platform("active target has no refresh rate".into()));
    }
    let refresh = (u64::from(path.targetInfo.refreshRate.Numerator) * 1000
        + u64::from(denominator / 2))
        / u64::from(denominator);
    Ok(Some(Spec {
        width: source.width,
        height: source.height,
        refresh_millihz: u32::try_from(refresh)
            .map_err(|_| Error::Platform("refresh rate overflow".into()))?,
        hdr: color(path)?.1,
    }))
}
fn inventory(scene: &Scene) -> Result<Inventory> {
    let used: BTreeSet<_> = scene
        .active
        .paths
        .iter()
        .map(|path| (adapter(path.sourceInfo.adapterId), path.sourceInfo.id))
        .collect();
    let mut targets = BTreeMap::<TargetKey, Target>::new();
    for path in &scene.all.paths {
        if !scene
            .supported_adapters
            .contains_key(&adapter(path.targetInfo.adapterId))
            || path.targetInfo.targetAvailable == 0
        {
            continue;
        }
        let Ok((key, name)) = target_key(path) else {
            continue;
        };
        let active_path = scene.active.paths.iter().find(|p| {
            adapter(p.targetInfo.adapterId) == key.adapter && p.targetInfo.id == key.target
        });
        let active = active_path.is_some();
        let selected = active_path.unwrap_or(path);
        let available =
            !active && !used.contains(&(adapter(path.sourceInfo.adapterId), path.sourceInfo.id));
        let entry = targets.entry(key.clone()).or_insert(Target {
            key,
            name,
            source: source_name(selected).unwrap_or_default(),
            active,
            available: false,
            actual: if active {
                actual(selected, &scene.active.modes)?
            } else {
                None
            },
        });
        entry.available |= available;
    }
    Ok(Inventory {
        supported: true,
        driver_present: !scene.instances.is_empty(),
        targets: targets.into_values().collect(),
    })
}

// Compare semantic modes and target identities, not transient mode-array indices
// or driver status flags. A fresh OS layout is never replaced with an old global
// snapshot during cleanup: only our exact target is removed from the fresh set.
pub(super) type Fingerprints = BTreeMap<TargetKey, Vec<i64>>;
pub(super) fn active_signature() -> Result<Fingerprints> {
    fingerprints(&query(QDC_ONLY_ACTIVE_PATHS)?, None)
}
pub(super) fn targets_for_instance(instance: &str) -> Result<Vec<Target>> {
    let scene = scene()?;
    Ok(inventory(&scene)?
        .targets
        .into_iter()
        .filter(|target| {
            scene
                .supported_adapters
                .get(&target.key.adapter)
                .is_some_and(|value| value == instance)
        })
        .collect())
}
fn fingerprints(topology: &Topology, exclude: Option<&TargetKey>) -> Result<Fingerprints> {
    let mut result = BTreeMap::new();
    for path in &topology.paths {
        let (key, _) = target_key(path)?;
        if exclude == Some(&key) {
            continue;
        }
        let index = unsafe { path.sourceInfo.Anonymous.modeInfoIdx };
        let mode = topology
            .modes
            .get(index as usize)
            .filter(|mode| mode.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE)
            .ok_or_else(|| Error::Platform("cannot preserve an active source mode".into()))?;
        let source = unsafe { mode.Anonymous.sourceMode };
        let color = color_api::read(path)?;
        let mut fingerprint = vec![
            i64::from(source.width),
            i64::from(source.height),
            i64::from(source.pixelFormat),
            i64::from(source.position.x),
            i64::from(source.position.y),
            i64::from(path.sourceInfo.id),
            i64::from(path.targetInfo.rotation),
            i64::from(path.targetInfo.scaling),
            i64::from(path.targetInfo.refreshRate.Numerator),
            i64::from(path.targetInfo.refreshRate.Denominator),
            i64::from(color.hdr_active),
        ];
        // WCG changes must remain visible to the original-output guard even
        // though actual.hdr now correctly classifies WCG as SDR.
        fingerprint.extend(color.signature());
        result.insert(key, fingerprint);
    }
    Ok(result)
}
fn apply(topology: &Topology, apply_submitted: &mut bool) -> Result<()> {
    use super::config_apply::{self, Phase};
    config_apply::run(apply_submitted, |phase| {
        let (flag, operation) = match phase {
            Phase::Validate => (SDC_VALIDATE, "SetDisplayConfig(VALIDATE)"),
            Phase::Apply => (SDC_APPLY, "SetDisplayConfig(APPLY)"),
        };
        let code = unsafe {
            SetDisplayConfig(
                topology.paths.len() as u32,
                topology.paths.as_ptr(),
                topology.modes.len() as u32,
                topology.modes.as_ptr(),
                SDC_USE_SUPPLIED_DISPLAY_CONFIG | flag,
            )
        };
        if code != 0 {
            return Err(error(operation, code as u32));
        }
        Ok(())
    })
}

fn activate(key: &TargetKey, spec: Spec) -> std::result::Result<Target, ActivationFailure> {
    let mut apply_submitted = false;
    let result = (|| -> Result<Target> {
        let mut before = scene()?;
        let targets = inventory(&before)?;
        if !targets
            .targets
            .iter()
            .any(|target| &target.key == key && target.available && !target.active)
        {
            return Err(Error::NoAvailableSlot);
        }
        let preserved = fingerprints(&before.active, None)?;
        let used: BTreeSet<_> = before
            .active
            .paths
            .iter()
            .map(|path| (adapter(path.sourceInfo.adapterId), path.sourceInfo.id))
            .collect();
        let mut path = before
            .all
            .paths
            .iter()
            .find(|path| {
                path.targetInfo.targetAvailable != 0
                    && !used.contains(&(adapter(path.sourceInfo.adapterId), path.sourceInfo.id))
                    && target_key(path).is_ok_and(|(candidate, _)| &candidate == key)
            })
            .copied()
            .ok_or(Error::NoAvailableSlot)?;
        let mut right = 0i64;
        for mode in &before.active.modes {
            if mode.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE {
                let source = unsafe { mode.Anonymous.sourceMode };
                right = right.max(i64::from(source.position.x) + i64::from(source.width));
            }
        }
        let x = i32::try_from(right)
            .map_err(|_| Error::Platform("desktop coordinate overflow".into()))?;
        let mut source: DISPLAYCONFIG_MODE_INFO = unsafe { zeroed() };
        source.infoType = DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE;
        source.adapterId = path.sourceInfo.adapterId;
        source.id = path.sourceInfo.id;
        source.Anonymous.sourceMode = DISPLAYCONFIG_SOURCE_MODE {
            width: spec.width,
            height: spec.height,
            pixelFormat: DISPLAYCONFIG_PIXELFORMAT_32BPP,
            position: POINTL { x, y: 0 },
        };
        path.sourceInfo.Anonymous.modeInfoIdx = before.active.modes.len() as u32;
        path.targetInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
        path.targetInfo.refreshRate = DISPLAYCONFIG_RATIONAL {
            Numerator: spec.refresh_millihz,
            Denominator: 1000,
        };
        path.targetInfo.scanLineOrdering = DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE;
        path.flags = DISPLAYCONFIG_PATH_ACTIVE;
        before.active.modes.push(source);
        before.active.paths.push(path);
        // Do not use SDC_ALLOW_CHANGES: it could silently change another screen
        // to make an unsupported mode work. Unsupported requests fail instead.
        apply(&before.active, &mut apply_submitted)?;
        for _ in 0..20 {
            let after = scene()?;
            if fingerprints(&after.active, Some(key))? != preserved {
                return Err(Error::Platform(
                    "another display changed during activation; refusing to claim success".into(),
                ));
            }
            if let Some(path) = after
                .active
                .paths
                .iter()
                .find(|path| target_key(path).is_ok_and(|(candidate, _)| &candidate == key))
            {
                set_color(path, spec.hdr)?;
                // HDR is a separate Windows operation. Verify the other paths
                // again afterwards rather than reusing the pre-HDR snapshot.
                let committed = query(QDC_ONLY_ACTIVE_PATHS)?;
                if fingerprints(&committed, Some(key))? != preserved {
                    return Err(Error::Platform(
                        "another display changed during HDR configuration".into(),
                    ));
                }
                let Some(path) = committed
                    .paths
                    .iter()
                    .find(|path| target_key(path).is_ok_and(|(candidate, _)| &candidate == key))
                else {
                    return Err(Error::Platform(
                        "virtual display disappeared during HDR configuration".into(),
                    ));
                };
                let readback = actual(path, &committed.modes)?;
                if readback.is_some_and(|actual| {
                    actual.width == spec.width
                        && actual.height == spec.height
                        && actual.refresh_millihz.abs_diff(spec.refresh_millihz) <= 1
                        && actual.hdr == spec.hdr
                }) {
                    let source = source_name(path)?;
                    if !source.is_empty() {
                        return Ok(Target {
                            key: key.clone(),
                            name: target_key(path)?.1,
                            source,
                            active: true,
                            available: false,
                            actual: readback,
                        });
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        Err(Error::Platform(
            "virtual display mode/HDR/source readback did not match the request".into(),
        ))
    })();
    result.map_err(|error| ActivationFailure {
        message: format!(
            "requested virtual display {}x{} at {}.{:03} Hz ({}): {error}",
            spec.width,
            spec.height,
            spec.refresh_millihz / 1000,
            spec.refresh_millihz % 1000,
            if spec.hdr { "HDR" } else { "SDR" },
        ),
        activated: apply_submitted,
    })
}

pub(super) fn deactivate(key: &TargetKey) -> Result<()> {
    deactivate_checked(key, false)
}
fn deactivate_checked(key: &TargetKey, require_other_active: bool) -> Result<()> {
    let mut before = scene()?;
    // A numeric ID can be recycled after a driver restart. Never fall back from
    // the full monitor path to a number, a name, or a different driver instance.
    let index = super::config_apply::exact_index(
        before
            .active
            .paths
            .iter()
            .map(|path| target_key(path).map(|(candidate, _)| candidate)),
        key,
    )?;
    let Some(index) = index else {
        return Ok(());
    };
    if require_other_active && before.active.paths.len() <= 1 {
        return Err(Error::LastActiveOutput);
    }
    let preserved = fingerprints(&before.active, Some(key))?;
    before.active.paths.remove(index);
    let mut apply_submitted = false;
    apply(&before.active, &mut apply_submitted)?;
    let after = scene()?;
    if after
        .active
        .paths
        .iter()
        .any(|path| target_key(path).is_ok_and(|(candidate, _)| &candidate == key))
    {
        return Err(Error::Platform(
            "virtual target remained active after removal".into(),
        ));
    }
    if fingerprints(&after.active, None)? != preserved {
        return Err(Error::Platform(
            "another display changed during removal; original layout was not replayed".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn named_owner_mutex_serializes_another_process_without_display_changes() {
        let name = format!("Local\\EclipseVirtualDisplayTest-{}", Uuid::new_v4());
        let owner = MutationLock::acquire_named(&name, 100).unwrap();
        let child = |expected: &str| {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "virtual_display::windows::tests::named_mutex_child",
                    "--nocapture",
                ])
                .env("ECLIPSE_VDD_MUTEX_TEST_NAME", &name)
                .env("ECLIPSE_VDD_MUTEX_TEST_EXPECT", expected)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        };
        child("busy");
        drop(owner);
        child("free");
    }
    #[test]
    fn named_mutex_child() {
        let Ok(name) = std::env::var("ECLIPSE_VDD_MUTEX_TEST_NAME") else {
            return;
        };
        let acquired = MutationLock::acquire_named(&name, 50);
        match std::env::var("ECLIPSE_VDD_MUTEX_TEST_EXPECT").as_deref() {
            Ok("busy") => assert!(acquired.is_err()),
            Ok("free") => assert!(acquired.is_ok()),
            _ => panic!("missing owned mutex test mode"),
        }
    }
}
