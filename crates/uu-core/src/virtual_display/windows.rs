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
        worker(move || configure_owned_mode(&key, spec)).unwrap_or_else(|error| {
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
        worker(move||configure_owned_mode(&key,spec)).unwrap_or_else(|error|Err(ActivationFailure{message:error.to_string(),activated:false}))
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
        let mut database_topology = 0;
        let topology_id = if flags == QDC_DATABASE_CURRENT {
            &mut database_topology as *mut i32
        } else {
            null_mut()
        };
        let code = unsafe {
            QueryDisplayConfig(
                flags,
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

fn configure_owned_mode(key:&TargetKey,spec:Spec)->std::result::Result<Target,ActivationFailure> {
    // A new PnP monitor may become active between enumeration and this call
    // when Windows applies its saved connected-display configuration. The
    // exact owned claim permits both states; observe it afresh under the lock.
    let observed=(||->Result<_>{let owned=demand::owned_keys(key)?;let active=scene()?.active.paths.iter().any(|p|target_key(p).is_ok_and(|(candidate,_)|&candidate==key));Ok((owned,active))})();
    let (owned,active)=observed.map_err(|error|ActivationFailure{message:error.to_string(),activated:true})?;
    let target=configure_mode(key,spec,active)?;
    let result=provision::capacity::persist_owned_live(&owned);
    result.map(|()|target).map_err(|error|ActivationFailure{message:error.to_string(),activated:true})
}
fn configure_mode(key: &TargetKey, spec: Spec, existing:bool) -> std::result::Result<Target, ActivationFailure> {
    let mut apply_submitted = false;
    let result = (|| -> Result<Target> {
        let mut before = scene()?;
        let targets = inventory(&before)?;
        if !targets
            .targets
            .iter()
            .any(|target| &target.key == key && if existing {target.active}else{target.available && !target.active})
        {
            return Err(Error::NoAvailableSlot);
        }
        let preserved = fingerprints(&before.active, if existing {Some(key)}else{None})?;
        if existing {
            let index=super::config_apply::exact_index(before.active.paths.iter().map(|path|target_key(path).map(|(key,_)|key)),key)?.ok_or(Error::UnknownSlot)?;
            let path=before.active.paths[index];
            // A cloned source cannot be edited without changing another output.
            if before.active.paths.iter().enumerate().any(|(other,p)|other!=index && p.sourceInfo.id==path.sourceInfo.id && adapter(p.sourceInfo.adapterId)==adapter(path.sourceInfo.adapterId)) {
                return Err(Error::Platform("detach the cloned virtual output before changing its mode".into()));
            }
            let source_index=unsafe{path.sourceInfo.Anonymous.modeInfoIdx} as usize;
            let mode=before.active.modes.get_mut(source_index).ok_or(Error::UnknownSlot)?;
            if mode.infoType!=DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE || mode.id!=path.sourceInfo.id || adapter(mode.adapterId)!=adapter(path.sourceInfo.adapterId) {return Err(Error::UnknownSlot);}
            let mut source=unsafe{mode.Anonymous.sourceMode};
            source.width=spec.width;source.height=spec.height;
            mode.Anonymous.sourceMode=source;
            let path=&mut before.active.paths[index];
            path.targetInfo.Anonymous.modeInfoIdx=DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
            path.targetInfo.refreshRate=DISPLAYCONFIG_RATIONAL{Numerator:spec.refresh_millihz,Denominator:1000};
            path.targetInfo.scanLineOrdering=DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE;
        }else{
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
        }
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
    deactivate_keys(&BTreeSet::from([key.clone()]), require_other_active)
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
    deactivate_keys(&keys, true)
}

fn deactivate_keys(keys: &BTreeSet<TargetKey>, require_other_active: bool) -> Result<()> {
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
    let preserved = super::capacity_policy::foreign(&fingerprints(&before.active, None)?, keys);
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
        let observed=fingerprints(&after, None)?;
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
