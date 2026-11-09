//! Original Windows CCD/SetupAPI adapter. No Sunshine implementation is copied.
//! Driver pipe commands are never used. Idle capacity preparation owns its
//! separate signed-package/configuration transaction through SetupAPI.

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
        Graphics::Gdi::{DISPLAYCONFIG_PATH_ACTIVE, DISPLAYCONFIG_PATH_MODE_IDX_INVALID, DISPLAYCONFIG_PATH_SUPPORT_VIRTUAL_MODE},
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
#[path = "driver_download_windows.rs"]
mod driver_download;
#[path = "provision_windows.rs"]
pub(super) mod provision;
#[path = "demand_windows.rs"]
mod demand;

#[derive(Default)]
pub(super) struct Windows { scope:Mutex<Option<(String,BTreeSet<TargetKey>)>> }

pub(super) struct MutationLock(HANDLE);
impl MutationLock {
    fn acquire() -> Result<Self> {
        Self::acquire_named("Global\\EclipseVirtualDisplayManager.v2", 5_000)
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

// Capacity admission borrows a host activity checkpoint. Join a scoped worker
// before returning so that the callback cannot outlive its Manager/host gates.
fn scoped_worker<T: Send>(f: impl FnOnce() -> T + Send) -> Result<T> {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("eclipse-vdd-capacity".into())
            .spawn_scoped(scope, move || {
                uu_input::windows_desktop::sync_input_desktop()
                    .map_err(|e| Error::Platform(e.to_string()))?;
                let _coordinates = uu_input::windows_desktop::PhysicalCoordinates::enter()
                    .map_err(|e| Error::Platform(e.to_string()))?;
                let _serialized = MutationLock::acquire()?;
                Ok(f())
            })
            .map_err(|e| Error::Platform(e.to_string()))?
            .join()
            .map_err(|_| Error::Platform("capacity worker panicked".into()))?
    })
}

impl Backend for Windows {
    fn on_demand(&self) -> bool { true }
    fn configure_store(&self, directory:&std::path::Path, saved:&[TargetKey]) -> Result<()> {
        let scope=crate::service_profile::directory_key(directory).map_err(|e|Error::Platform(e.to_string()))?;
        *self.scope.lock().map_err(|_|Error::Platform("display owner is poisoned".into()))? = Some((scope.clone(),saved.iter().cloned().collect()));
        if crate::service_admin::windows::require_administrator().is_ok() {
            scoped_worker(move||demand::reconcile_owner(&scope))??;
            if let Err(error)=scoped_worker(demand::retire_idle_pool)? {tracing::warn!(%error,"legacy virtual display pool cleanup is waiting for its owner");}
        }
        Ok(())
    }
    fn prepare_on_demand(&self, package:Option<&[(String,Vec<u8>)]>) -> Result<()> {
        scoped_worker(demand::recover_uninstalled_legacy)??;
        if package.is_none() && scoped_worker(demand::prepared)?? { return Ok(()); }
        let owned;
        let package = if let Some(package) = package { package } else {
            owned = driver_download::download().map_err(|e|Error::Platform(e.message))?;
            &owned
        };
        scoped_worker(|| demand::prepare(package))?
    }
    fn allocate(&self, id:LeaseId) -> Result<Option<(Target,Box<dyn Claim>)>> {
        if !scoped_worker(demand::prepared)?? {self.prepare_on_demand(None)?;}
        let (scope,saved) = {
            let mut selected=self.scope.lock().map_err(|_|Error::Platform("display owner is poisoned".into()))?;
            if selected.is_none() {
                let directory=crate::Config::from_env().map_err(|e|Error::Platform(e.to_string()))?.at("device-trust");
                *selected=Some((crate::service_profile::directory_key(&directory).map_err(|e|Error::Platform(e.to_string()))?,BTreeSet::new()));
            }
            selected.as_ref().unwrap().clone()
        };
        scoped_worker(|| demand::create(&scope,id,&saved))?.map(Some)
    }
    fn recover(&self) -> Result<()> {
        Ok(())
    }
    fn preparation_authorized(&self) -> bool {
        crate::service_admin::windows::require_administrator().is_ok()
    }
    fn preparation_observation(&self) -> Result<Option<preparation::Observation>> {
        Ok(None)
    }
    fn download_capacity_driver(&self) -> preparation::Result<Vec<(String, Vec<u8>)>> {
        driver_download::download()
    }
    fn prepare_capacity(
        &self,
        minimum: u32,
        package: Option<&[(String, Vec<u8>)]>,
        checkpoint: &(dyn Fn() -> preparation::Result<()> + Sync),
    ) -> preparation::Result<()> {
        scoped_worker(|| provision::capacity::prepare(minimum, package, checkpoint))
            .map_err(|error| preparation::Failure::unchanged(error.to_string()))?
    }
    fn recover_capacity(
        &self,
        checkpoint: &(dyn Fn() -> preparation::Result<()> + Sync),
    ) -> preparation::Result<()> {
        scoped_worker(|| provision::capacity::recover(checkpoint))
            .map_err(|error| preparation::Failure::pending(error.to_string()))?
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
        if crate::service_admin::windows::require_administrator().is_err() {return Ok(Inventory{supported:true,driver_present:false,targets:Vec::new()});}
        let scope=self.scope.lock().map_err(|_|Error::Platform("display owner is poisoned".into()))?.as_ref().map(|(id,_)|id.clone());
        worker(move||demand::inventory(scope.as_deref()))?
    }
    fn activate(
        &self,
        key: &TargetKey,
        spec: Spec,
    ) -> std::result::Result<Target, ActivationFailure> {
        let key = key.clone();
        worker(move || configure_owned_mode(&key, spec, true)).unwrap_or_else(|error| {
            Err(ActivationFailure {
                message: error.to_string(),
                activated: false,
            })
        })
    }
    fn deactivate(&self, key: &TargetKey) -> Result<()> {
        let key = key.clone();
        worker(move || demand::remove(&key))?
    }
    fn reconfigure(&self,key:&TargetKey,spec:Spec)->std::result::Result<Target,ActivationFailure> {
        let key=key.clone();
        worker(move||configure_owned_mode(&key,spec,false)).unwrap_or_else(|error|Err(ActivationFailure{message:error.to_string(),activated:false}))
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
        let query_flags = flags | QDC_VIRTUAL_MODE_AWARE;
        let code =
            unsafe { GetDisplayConfigBufferSizes(query_flags, &mut paths_count, &mut modes_count) };
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
        let mut database_topology = 0;
        let topology_id = if flags == QDC_DATABASE_CURRENT {
            &mut database_topology as *mut i32
        } else {
            null_mut()
        };
        let code = unsafe {
            QueryDisplayConfig(
                query_flags,
                &mut paths_count,
                paths.as_mut_ptr(),
                &mut modes_count,
                modes.as_mut_ptr(),
                topology_id,
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

// Windows virtual clones keep one real source per adapter. Legacy CCD queries
// can expose synthetic routes instead, hiding the exact new IDD target.
fn source_mode_index(path: &DISPLAYCONFIG_PATH_INFO) -> u32 {
    let packed = unsafe { path.sourceInfo.Anonymous.modeInfoIdx };
    if path.flags & DISPLAYCONFIG_PATH_SUPPORT_VIRTUAL_MODE != 0 { packed >> 16 } else { packed }
}
fn target_mode_index(path: &DISPLAYCONFIG_PATH_INFO) -> u32 {
    let packed = unsafe { path.targetInfo.Anonymous.modeInfoIdx };
    if path.flags & DISPLAYCONFIG_PATH_SUPPORT_VIRTUAL_MODE != 0 { packed >> 16 } else { packed }
}
fn set_source_mode_index(path: &mut DISPLAYCONFIG_PATH_INFO, index: usize) -> Result<()> {
    let index = u32::try_from(index).map_err(|_| Error::Platform("too many display modes".into()))?;
    path.sourceInfo.Anonymous.modeInfoIdx = if path.flags & DISPLAYCONFIG_PATH_SUPPORT_VIRTUAL_MODE != 0 {
        if index >= 0xffff {return Err(Error::Platform("too many virtual display modes".into()));}
        (index << 16) | 0xffff // no clone group; this path has its own source mode
    } else { index };
    Ok(())
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
    let index = source_mode_index(path);
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

fn fingerprint_change(before: &Fingerprints, after: &Fingerprints, key: &TargetKey) -> String {
    let Some(current) = after.get(key) else {
        return "original full monitor identity is temporarily unavailable or has been replaced".into();
    };
    let Some(previous) = before.get(key) else { return "output was absent before preparation".into(); };
    let fields = ["width", "height", "pixel format", "x", "y", "source ID", "rotation", "scaling",
        "refresh numerator", "refresh denominator", "HDR active", "color API", "color flags",
        "active color mode", "color encoding", "bits per channel"];
    fields.iter().zip(previous.iter().zip(current)).filter_map(|(name, (old, new))| {
        (old != new).then(|| format!("{name}: {old} -> {new}"))
    }).collect::<Vec<_>>().join(", ")
}

// QueryDisplayConfig and monitor interfaces are asynchronous during PnP. Wait
// only by reading; writes remain outside this helper and still use fresh exact
// identity and unchanged-original checks at their submission boundary.
pub(super) fn settled_readback<T: Clone + Eq>(
    timeout: Duration,
    checkpoint: &(dyn Fn() -> super::preparation::Result<()> + Sync),
    mut read: impl FnMut() -> Result<T>,
) -> Result<T> {
    let start = std::time::Instant::now();
    let mut settling = super::capacity_policy::Settling::default();
    loop {
        checkpoint().map_err(|e| Error::Platform(e.message))?;
        let last = match read() {
            Ok(value) => {
                if settling.observe(start.elapsed(), Some(&value)) { return Ok(value); }
                "complete display observations are still changing".to_owned()
            }
            Err(error) => {
                settling.observe(start.elapsed(), None);
                error.to_string()
            }
        };
        if start.elapsed() >= timeout {
            return Err(Error::Platform(format!("display state did not settle within {} seconds: {last}", timeout.as_secs())));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

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
    fingerprints_with_color(topology, exclude, false)
}
fn mode_fingerprints(topology: &Topology, exclude: Option<&TargetKey>) -> Result<Fingerprints> {
    fingerprints_with_color(topology, exclude, true)
}
fn fingerprints_with_color(topology: &Topology, exclude: Option<&TargetKey>, mode_change:bool) -> Result<Fingerprints> {
    let mut result = BTreeMap::new();
    for path in &topology.paths {
        let (key, _) = target_key(path)?;
        if exclude == Some(&key) {
            continue;
        }
        let index = source_mode_index(path);
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
        fingerprint.extend(if mode_change {color.mode_signature()} else {color.signature()});
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
                SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_VIRTUAL_MODE_AWARE | flag,
            )
        };
        if code != 0 {
            return Err(error(operation, code as u32));
        }
        Ok(())
    })
}

/// Supply an independent source for an explicitly owned target. Existing
/// outputs retain their paths/modes, including the source of a cloned sibling.
fn plan_owned_mode(mut active: Topology, all: &Topology, selected: Option<usize>, key: &TargetKey, spec: Spec, extend_right: bool) -> Result<Topology> {
    let identity = |path:&DISPLAYCONFIG_PATH_INFO| (adapter(path.sourceInfo.adapterId),path.sourceInfo.id);
    let used:BTreeSet<_> = active.paths.iter().enumerate().filter(|(i,_)|Some(*i)!=selected).map(|(_,path)|identity(path)).collect();
    let current = selected.map(|i|active.paths[i]);
    let cloned = current.is_some_and(|path| {
        if used.contains(&identity(&path)) {return true;}
        let Some(mode)=active.modes.get(source_mode_index(&path) as usize) else{return false;};
        let position=unsafe{mode.Anonymous.sourceMode.position};
        active.paths.iter().enumerate().filter(|(i,_)|Some(*i)!=selected).any(|(_,p)| {
            active.modes.get(source_mode_index(p) as usize).is_some_and(|m| {
                let other=unsafe{m.Anonymous.sourceMode.position};other.x==position.x&&other.y==position.y
            })
        })
    });
    let mut path = if let Some(path)=current.filter(|p|!used.contains(&identity(p))) {path} else {
        all.paths.iter().find(|p|p.targetInfo.targetAvailable!=0 && !used.contains(&identity(p)) && target_key(p).is_ok_and(|(candidate,_)|&candidate==key)).copied().ok_or(Error::NoAvailableSlot)?
    };
    let mut right=0i64;
    for (_,other) in active.paths.iter().enumerate().filter(|(i,_)|Some(*i)!=selected) {
        let mode=active.modes.get(source_mode_index(other) as usize).ok_or(Error::UnknownSlot)?;
        if mode.infoType!=DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE {return Err(Error::UnknownSlot);}
        let source=unsafe{mode.Anonymous.sourceMode};
        right=right.max(i64::from(source.position.x)+i64::from(source.width));
    }
    let position = if !extend_right && !cloned {
        current.and_then(|p|active.modes.get(source_mode_index(&p) as usize)).map(|m|unsafe{m.Anonymous.sourceMode.position})
    }else{None}.unwrap_or(POINTL{x:i32::try_from(right).map_err(|_|Error::Platform("desktop coordinate overflow".into()))?,y:0});
    let mut source:DISPLAYCONFIG_MODE_INFO=unsafe{zeroed()};
    source.infoType=DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE;source.adapterId=path.sourceInfo.adapterId;source.id=path.sourceInfo.id;
    source.Anonymous.sourceMode=DISPLAYCONFIG_SOURCE_MODE{width:spec.width,height:spec.height,pixelFormat:DISPLAYCONFIG_PIXELFORMAT_32BPP,position};
    // Reuse this source's entry when it is not shared. CCD requires one entry
    // per source identity even when an old mode becomes unused by this path.
    let index=active.modes.iter().position(|m|m.infoType==DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE && adapter(m.adapterId)==identity(&path).0 && m.id==path.sourceInfo.id);
    let index=match index {Some(i)=>{active.modes[i]=source;i},None=>{let i=active.modes.len();active.modes.push(source);i}};
    set_source_mode_index(&mut path,index)?;
    path.targetInfo.Anonymous.modeInfoIdx=DISPLAYCONFIG_PATH_MODE_IDX_INVALID; // recalculate target and desktop-image modes
    path.targetInfo.refreshRate=DISPLAYCONFIG_RATIONAL{Numerator:spec.refresh_millihz,Denominator:1000};
    path.targetInfo.scanLineOrdering=DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE;
    path.flags |= DISPLAYCONFIG_PATH_ACTIVE;
    if let Some(i)=selected {active.paths[i]=path;} else {active.paths.push(path);}
    Ok(active)
}

fn configure_owned_mode(key:&TargetKey,spec:Spec,creation:bool)->std::result::Result<Target,ActivationFailure> {
    // A new PnP monitor may become active between enumeration and this call
    // when Windows applies its saved connected-display configuration. The
    // exact owned claim permits both states; observe it afresh under the lock.
    let observed=(||->Result<_>{let owned=demand::owned_keys(key)?;let active=scene()?.active.paths.iter().any(|p|target_key(p).is_ok_and(|(candidate,_)|&candidate==key));Ok((owned,active))})();
    let (owned,active)=observed.map_err(|error|ActivationFailure{message:error.to_string(),activated:true})?;
    let target=configure_mode(key,spec,active,creation)?;
    let result=provision::capacity::persist_owned_live(&owned);
    result.map(|()|target).map_err(|error|ActivationFailure{message:error.to_string(),activated:true})
}
fn configure_mode(key: &TargetKey, spec: Spec, existing:bool, extend_right:bool) -> std::result::Result<Target, ActivationFailure> {
    let mut apply_submitted = false;
    let result = (|| -> Result<Target> {
        let before = scene()?;
        let targets = inventory(&before)?;
        if !targets
            .targets
            .iter()
            .any(|target| &target.key == key && (extend_right || if existing {target.active}else{target.available && !target.active}))
        {
            return Err(Error::NoAvailableSlot);
        }
        let preserved = mode_fingerprints(&before.active, if existing {Some(key)}else{None})?;
        let selected = before.active.paths.iter().position(|path| target_key(path).is_ok_and(|(candidate,_)| &candidate == key));
        let proposal = plan_owned_mode(before.active, &before.all, selected, key, spec, extend_right)?;
        // Do not use SDC_ALLOW_CHANGES: it could silently change another screen
        // to make an unsupported mode work. Unsupported requests fail instead.
        apply(&proposal, &mut apply_submitted)?;
        for _ in 0..20 {
            let after = scene()?;
            let observed = mode_fingerprints(&after.active, Some(key))?;
            if observed != preserved {
                let changes=preserved.keys().filter(|key|observed.get(*key)!=preserved.get(*key)).map(|key|format!("{}: {}",key.monitor_path,fingerprint_change(&preserved,&observed,key))).collect::<Vec<_>>().join("; ");
                return Err(Error::Platform(format!("another display changed during activation; refusing to claim success: {changes}")));
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
                if mode_fingerprints(&committed, Some(key))? != preserved {
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

pub(super) fn deactivate(key: &TargetKey, movable: &BTreeSet<TargetKey>) -> Result<()> {
    deactivate_keys(&BTreeSet::from([key.clone()]), false, true, movable)
}
fn deactivate_checked(key: &TargetKey, require_other_active: bool) -> Result<()> {
    deactivate_keys(&BTreeSet::from([key.clone()]), require_other_active, false, &BTreeSet::new())
}

/// Pool setup owns every target in this batch, including its inactive siblings.
/// Submit one topology without the entire batch, so no temporary sibling layout
/// can be normalized by Windows between individual removals.
pub(super) fn deactivate_claimed(
    targets: &[Target],
    claims: &BTreeMap<TargetKey, Box<dyn Claim>>,
) -> Result<()> {
    let keys:BTreeSet<_>=targets.iter().map(|target|target.key.clone()).collect();
    if keys.iter().any(|key|!claims.contains_key(key)) {return Err(Error::Claimed);}
    deactivate_keys(&keys, true, false, &BTreeSet::new())
}

fn deactivate_keys(keys: &BTreeSet<TargetKey>, require_other_active: bool, mode_change:bool, movable:&BTreeSet<TargetKey>) -> Result<()> {
    let mut before = scene()?;
    // A numeric ID can be recycled after a driver restart. Never fall back from
    // the full monitor path to a number, a name, or a different driver instance.
    let retained = super::config_apply::retained_indices(
        before
            .active
            .paths
            .iter()
            .map(|path| target_key(path).map(|(candidate, _)| candidate)),
        keys,
    )?;
    if retained.len() == before.active.paths.len() {
        return Ok(());
    }
    if require_other_active && retained.is_empty() {
        return Err(Error::LastActiveOutput);
    }
    let fingerprint:fn(&Topology,Option<&TargetKey>)->Result<Fingerprints>=if mode_change {mode_fingerprints} else {fingerprints};
    let preserved = super::capacity_policy::foreign(&fingerprint(&before.active, None)?, keys);
    before.active.paths = retained.iter().map(|index|before.active.paths[*index]).collect();
    let mut apply_submitted = false;
    apply(&before.active, &mut apply_submitted)?;
    let mut readback = || {
        let after = query(QDC_ONLY_ACTIVE_PATHS)?;
        // Reading every identity is required to prove absence. A retiring
        // monitor with no interface yet must wait, never masquerade as removed.
        if super::config_apply::retained_indices(after.paths.iter().map(|path| target_key(path).map(|(candidate,_)|candidate)), keys)?.len() != after.paths.len() {
            return Err(Error::Platform("virtual target remained active after removal".into()));
        }
        let mut observed=fingerprint(&after, None)?;
        // Windows may close the gap between our surviving siblings. Their
        // modes, color intent and full identities still have to be unchanged;
        // unrelated outputs never receive this positional allowance.
        for key in movable {
            if let (Some(expected),Some(actual))=(preserved.get(key),observed.get_mut(key)) {
                actual[3]=expected[3];actual[4]=expected[4];
            }
        }
        if observed != preserved {
            let changes=preserved.keys().filter(|key|observed.get(*key)!=preserved.get(*key))
                .map(|key|format!("{}: {}",key.monitor_path,fingerprint_change(&preserved,&observed,key))).collect::<Vec<_>>().join("; ");
            return Err(Error::Platform(format!("another display changed during removal; original layout was not replayed: {changes}")));
        }
        Ok(())
    };
    if readback().is_err() {
        settled_readback(Duration::from_secs(5), &|| Ok(()), &mut readback)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn virtual_modes_use_packed_indices_and_clear_only_the_owned_clone_group() {
        let mut path:DISPLAYCONFIG_PATH_INFO=unsafe{zeroed()};
        path.flags=DISPLAYCONFIG_PATH_SUPPORT_VIRTUAL_MODE;
        path.sourceInfo.Anonymous.modeInfoIdx=(7<<16)|3;
        path.targetInfo.Anonymous.modeInfoIdx=(11<<16)|5;
        assert_eq!(source_mode_index(&path),7);assert_eq!(target_mode_index(&path),11);
        set_source_mode_index(&mut path,9).unwrap();
        assert_eq!(source_mode_index(&path),9);
        assert_eq!(unsafe{path.sourceInfo.Anonymous.modeInfoIdx}&0xffff,0xffff);
        assert_eq!(target_mode_index(&path),11);
        path.flags=0;set_source_mode_index(&mut path,13).unwrap();
        assert_eq!(source_mode_index(&path),13);
    }
    #[test]
    fn auto_enabled_virtual_clone_moves_right_but_editing_an_extended_display_preserves_its_position() {
        let mut paths=Vec::new();let mut modes=Vec::new();
        for (id,width,x,y) in [(1,3072,0,0),(2,1920,0,0)] {
            let mut path:DISPLAYCONFIG_PATH_INFO=unsafe{zeroed()};
            path.flags=DISPLAYCONFIG_PATH_ACTIVE|DISPLAYCONFIG_PATH_SUPPORT_VIRTUAL_MODE;
            path.sourceInfo.adapterId=LUID{LowPart:id,HighPart:0};path.sourceInfo.id=0;
            set_source_mode_index(&mut path,modes.len()).unwrap();
            let mut mode:DISPLAYCONFIG_MODE_INFO=unsafe{zeroed()};mode.infoType=DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE;
            mode.adapterId=path.sourceInfo.adapterId;mode.id=0;
            mode.Anonymous.sourceMode=DISPLAYCONFIG_SOURCE_MODE{width,height:1080,pixelFormat:DISPLAYCONFIG_PIXELFORMAT_32BPP,position:POINTL{x,y}};
            paths.push(path);modes.push(mode);
        }
        let empty=Topology{paths:Vec::new(),modes:Vec::new()};
        let key=TargetKey{adapter:(2,0),target:256,monitor_path:"generated-owned-target".into()};
        let spec=Spec{width:1920,height:1080,refresh_millihz:60000,hdr:false};
        let planned=plan_owned_mode(Topology{paths,modes},&empty,Some(1),&key,spec,true).unwrap();
        let physical=unsafe{planned.modes[0].Anonymous.sourceMode};let added=unsafe{planned.modes[1].Anonymous.sourceMode};
        assert_eq!((physical.width,physical.position.x,physical.position.y),(3072,0,0));
        assert_eq!((added.position.x,added.position.y),(3072,0));assert_eq!(planned.modes.len(),2);
        let edited=plan_owned_mode(planned,&empty,Some(1),&key,Spec{width:1280,..spec},false).unwrap();
        let source=unsafe{edited.modes[1].Anonymous.sourceMode};
        assert_eq!((source.width,source.position.x,source.position.y),(1280,3072,0));
    }
    #[test]
    fn a_pool_batch_requires_every_exact_claim_before_querying_or_mutating_the_desktop() {
        struct TestClaim;
        impl Claim for TestClaim {}
        let targets:Vec<_>=(0..2).map(|id|Target {
            key:TargetKey{adapter:(0,0),target:id,monitor_path:format!("generated-unpublished-pool-{}",Uuid::new_v4())},
            name:String::new(),source:String::new(),active:true,available:false,actual:Some(Spec::default()),
        }).collect();
        let mut claims:BTreeMap<TargetKey,Box<dyn Claim>>=BTreeMap::new();
        claims.insert(targets[0].key.clone(),Box::new(TestClaim));
        assert!(matches!(deactivate_claimed(&targets,&claims),Err(Error::Claimed)));
    }
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
