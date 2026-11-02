//! Elevated, pinned first installation of the original signed MttVDD package.
//! No certificate import, signature-policy override, driver-global reload,
//! forced upgrade or removal of an existing device is permitted here.

use super::*;
use std::{
    ffi::OsString,
    io::Write,
    mem::{size_of, zeroed},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::FromRawHandle,
    },
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    time::{Duration, Instant},
};
use windows_sys::{
    core::GUID,
    Win32::{
        Devices::DeviceAndDriverInstallation::*,
        Foundation::{
            GetLastError, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, GENERIC_WRITE,
            INVALID_HANDLE_VALUE,
        },
        Security::WinTrust::*,
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            SECURITY_ATTRIBUTES,
        },
        Storage::FileSystem::{
            CreateDirectoryW, CreateFileW, GetFileAttributesW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL,
            FILE_ATTRIBUTE_REPARSE_POINT, INVALID_FILE_ATTRIBUTES,
        },
        System::{Com::CoTaskMemFree, Registry::*},
        UI::Shell::{FOLDERID_ProgramData, SHGetKnownFolderPath},
    },
};

const CONFIG_KEY: &str = "SOFTWARE\\MikeTheTech\\VirtualDisplayDriver";
const DISPLAY_CLASS: GUID = GUID {
    data1: 0x4d36e968,
    data2: 0xe325,
    data3: 0x11ce,
    data4: [0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18],
};

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}
fn path_wide(value: &Path) -> Vec<u16> {
    value.as_os_str().encode_wide().chain([0]).collect()
}
fn os_error(operation: &str, code: u32) -> Error {
    Error::Platform(format!("{operation}: Windows error {code}"))
}
fn win_ok(value: i32, operation: &str) -> Result<()> {
    if value != 0 {
        Ok(())
    } else {
        Err(os_error(operation, unsafe { GetLastError() }))
    }
}

pub(super) fn install(directory: &Path) -> Result<()> {
    crate::service_admin::windows::require_administrator().map_err(|_| {
        Error::Platform(
            "explicit administrator elevation is required to install the virtual display driver"
                .into(),
        )
    })?;
    #[cfg(not(target_arch = "x86_64"))]
    return Err(Error::Platform(
        "the pinned signed driver package is for x86_64 Windows".into(),
    ));
    #[cfg(target_arch = "x86_64")]
    {
        // Copy bytes, not paths, across the elevation boundary. The original
        // directory may be writable by an unelevated caller; its files are not
        // referenced by SetupAPI after this bounded digest check.
        let payload = read_pinned_package(directory)?;
        super::windows::worker(move || {
            // PnP and the driver's HKLM config are machine-wide even when
            // different interactive console sessions have different CCD locks.
            let _installer = super::windows::MutationLock::acquire_named(
                "Global\\EclipseVirtualDisplayDriverInstaller",
                5_000,
            )?;
            install_on_desktop(payload)
        })?
    }
}

struct ProtectedDirectory {
    path: PathBuf,
    keep: bool,
}
impl ProtectedDirectory {
    fn create(payload: &[(String, Vec<u8>)]) -> Result<Self> {
        let mut output = null_mut();
        let code =
            unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, 0, null_mut(), &mut output) };
        if code < 0 || output.is_null() {
            return Err(os_error("SHGetKnownFolderPath(ProgramData)", code as u32));
        }
        let mut length = 0;
        unsafe {
            while *output.add(length) != 0 {
                length += 1;
            }
        }
        let base = PathBuf::from(OsString::from_wide(unsafe {
            std::slice::from_raw_parts(output, length)
        }));
        unsafe {
            CoTaskMemFree(output.cast());
        }
        let attributes = unsafe { GetFileAttributesW(path_wide(&base).as_ptr()) };
        if attributes == INVALID_FILE_ATTRIBUTES || attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(Error::Platform(
                "ProgramData is unavailable or redirects through a reparse point".into(),
            ));
        }
        let path = base.join(format!("Eclipse-VirtualDisplay-{}", Uuid::new_v4()));
        // The UMDF host may run as LocalService. Read/execute is sufficient;
        // only Administrators and System may change the pinned package/config.
        let sddl =
            wide("O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;GRGX;;;BU)(A;OICI;GRGX;;;LS)");
        let mut descriptor = null_mut();
        win_ok(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    null_mut(),
                )
            },
            "ConvertStringSecurityDescriptorToSecurityDescriptorW",
        )?;
        let descriptor = crate::service_admin::windows::Descriptor(descriptor);
        let security = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        let created = unsafe { CreateDirectoryW(path_wide(&path).as_ptr(), &security) };
        let create_error = unsafe { GetLastError() };
        if created == 0 {
            return Err(os_error(
                "CreateDirectoryW(protected driver directory)",
                create_error,
            ));
        }
        let result = Self { path, keep: false };
        for (name, bytes) in payload {
            // Apply owner and ACL at creation, before a lower-privilege token
            // of the same user could obtain a writer through creator ownership.
            let handle = unsafe {
                CreateFileW(
                    path_wide(&result.path.join(name)).as_ptr(),
                    GENERIC_WRITE,
                    0,
                    &security,
                    CREATE_NEW,
                    FILE_ATTRIBUTE_NORMAL,
                    null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(os_error("CreateFileW(protected driver payload)", unsafe {
                    GetLastError()
                }));
            }
            let mut file = unsafe { std::fs::File::from_raw_handle(handle) };
            file.write_all(bytes)
                .and_then(|_| file.sync_all())
                .map_err(|error| {
                    Error::Platform(format!("write protected driver payload: {error}"))
                })?;
        }
        // Recheck the protected copy before handing it to Windows installers.
        read_pinned_package(&result.path)?;
        Ok(result)
    }
}
impl Drop for ProtectedDirectory {
    fn drop(&mut self) {
        if !self.keep {
            // Delete only the four names we created, then the directory. Never
            // recursively follow content added by another principal.
            for file in &driver_package().files {
                let _ = std::fs::remove_file(self.path.join(&file.name));
            }
            if let Err(error) = std::fs::remove_dir(&self.path) {
                tracing::warn!(%error, "protected virtual display staging directory was retained");
            }
        }
    }
}

fn verify_catalog(directory: &Path) -> Result<()> {
    let path = path_wide(&directory.join("mttvdd.cat"));
    let mut file: WINTRUST_FILE_INFO = unsafe { zeroed() };
    file.cbStruct = size_of::<WINTRUST_FILE_INFO>() as u32;
    file.pcwszFilePath = path.as_ptr();
    let mut trust: WINTRUST_DATA = unsafe { zeroed() };
    trust.cbStruct = size_of::<WINTRUST_DATA>() as u32;
    trust.dwUIChoice = WTD_UI_NONE;
    trust.fdwRevocationChecks = WTD_REVOKE_WHOLECHAIN;
    trust.dwUnionChoice = WTD_CHOICE_FILE;
    trust.Anonymous.pFile = &mut file;
    trust.dwStateAction = WTD_STATEACTION_VERIFY;
    trust.dwProvFlags = WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT | WTD_DISABLE_MD2_MD4;
    trust.dwUIContext = WTD_UICONTEXT_INSTALL;
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let status = unsafe {
        WinVerifyTrust(
            null_mut(),
            &mut action,
            (&mut trust as *mut WINTRUST_DATA).cast(),
        )
    };
    trust.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        WinVerifyTrust(
            null_mut(),
            &mut action,
            (&mut trust as *mut WINTRUST_DATA).cast(),
        );
    }
    if status != 0 {
        return Err(os_error(
            "WinVerifyTrust(pinned driver catalog)",
            status as u32,
        ));
    }
    // Fixed hashes bind the INF and DLL to the exact original release. Windows
    // SetupAPI/NewDev additionally enforce catalog membership and driver policy
    // when staging/binding; no INSTALLFLAG_FORCE or unsigned fallback is used.
    Ok(())
}

struct DevInfoSet(HDEVINFO);
impl Drop for DevInfoSet {
    fn drop(&mut self) {
        unsafe {
            SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}
fn no_existing_mtt_node() -> Result<()> {
    // Include phantom nodes: installation is not an implicit repair/upgrade of
    // a previous installation, even if its monitor is currently disconnected.
    let handle = unsafe { SetupDiGetClassDevsW(&DISPLAY_CLASS, null(), null_mut(), 0) };
    if handle == INVALID_HANDLE_VALUE as isize {
        return Err(os_error("SetupDiGetClassDevsW", unsafe { GetLastError() }));
    }
    let devices = DevInfoSet(handle);
    for index in 0..4096 {
        let mut info: SP_DEVINFO_DATA = unsafe { zeroed() };
        info.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
        if unsafe { SetupDiEnumDeviceInfo(devices.0, index, &mut info) } == 0 {
            let code = unsafe { GetLastError() };
            if code == ERROR_NO_MORE_ITEMS {
                return Ok(());
            }
            return Err(os_error("SetupDiEnumDeviceInfo", code));
        }
        let mut data = [0u16; 2048];
        if unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                devices.0,
                &info,
                SPDRP_HARDWAREID,
                null_mut(),
                data.as_mut_ptr().cast(),
                std::mem::size_of_val(&data) as u32,
                null_mut(),
            )
        } != 0
        {
            if data
                .split(|value| *value == 0)
                .filter(|part| !part.is_empty())
                .any(|part| {
                    matches!(
                        String::from_utf16_lossy(part).to_ascii_lowercase().as_str(),
                        "root\\mttvdd" | "mttvdd"
                    )
                })
            {
                return Err(Error::Platform("an existing MttVDD installation was found; prepare an existing slot or use the official driver manager explicitly".into()));
            }
        }
    }
    Err(Error::Platform(
        "display adapter inventory exceeds safety bound".into(),
    ))
}

struct Configuration {
    key: HKEY,
    value: Vec<u16>,
    keep: bool,
}
impl Configuration {
    fn create(directory: &Path) -> Result<Self> {
        let mut key = null_mut();
        let code = unsafe {
            RegCreateKeyExW(
                HKEY_LOCAL_MACHINE,
                wide(CONFIG_KEY).as_ptr(),
                0,
                null(),
                REG_OPTION_NON_VOLATILE,
                KEY_QUERY_VALUE | KEY_SET_VALUE | KEY_WOW64_64KEY,
                null(),
                &mut key,
                null_mut(),
            )
        };
        if code != 0 {
            return Err(os_error("RegCreateKeyExW(MttVDD)", code));
        }
        let mut config = Self {
            key,
            value: Vec::new(),
            keep: false,
        };
        let mut bytes = 0;
        let code = unsafe {
            RegQueryValueExW(
                key,
                wide("VDDPATH").as_ptr(),
                null(),
                null_mut(),
                null_mut(),
                &mut bytes,
            )
        };
        if code != ERROR_FILE_NOT_FOUND {
            config.keep = true; // Never delete a value we did not create.
            return Err(Error::Platform(
                "an existing MttVDD configuration path was found; it was left unchanged".into(),
            ));
        }
        config.value = path_wide(directory);
        let code = unsafe {
            RegSetValueExW(
                key,
                wide("VDDPATH").as_ptr(),
                0,
                REG_SZ,
                config.value.as_ptr().cast(),
                (config.value.len() * 2) as u32,
            )
        };
        if code != 0 {
            return Err(os_error("RegSetValueExW(MttVDD path)", code));
        }
        Ok(config)
    }
    fn rollback(&mut self) -> Result<()> {
        if self.keep || self.value.is_empty() {
            return Ok(());
        }
        let mut current = vec![0u16; self.value.len() + 1];
        let mut bytes = (current.len() * 2) as u32;
        let mut kind = 0;
        let code = unsafe {
            RegQueryValueExW(
                self.key,
                wide("VDDPATH").as_ptr(),
                null(),
                &mut kind,
                current.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        current.truncate(bytes as usize / 2);
        if code != 0 || kind != REG_SZ || current != self.value {
            return Err(Error::Platform(
                "MttVDD configuration changed externally; rollback did not overwrite it".into(),
            ));
        }
        let code = unsafe { RegDeleteValueW(self.key, wide("VDDPATH").as_ptr()) };
        if code == 0 {
            self.keep = true;
            Ok(())
        } else {
            Err(os_error("RegDeleteValueW(owned VDDPATH)", code))
        }
    }
}
impl Drop for Configuration {
    fn drop(&mut self) {
        if let Err(error) = self.rollback() {
            tracing::warn!(%error, "virtual display configuration rollback remains pending");
        }
        unsafe {
            RegCloseKey(self.key);
        }
    }
}

struct DeviceNode {
    devices: DevInfoSet,
    info: SP_DEVINFO_DATA,
    instance: String,
    registered: bool,
    keep: bool,
}
struct DriverList {
    devices: HDEVINFO,
    info: SP_DEVINFO_DATA,
}
impl Drop for DriverList {
    fn drop(&mut self) {
        unsafe {
            SetupDiDestroyDriverInfoList(self.devices, &self.info, SPDIT_COMPATDRIVER);
        }
    }
}
impl DeviceNode {
    fn create() -> Result<Self> {
        no_existing_mtt_node()?;
        let handle = unsafe { SetupDiCreateDeviceInfoList(&DISPLAY_CLASS, null_mut()) };
        if handle == INVALID_HANDLE_VALUE as isize {
            return Err(os_error("SetupDiCreateDeviceInfoList", unsafe {
                GetLastError()
            }));
        }
        let devices = DevInfoSet(handle);
        let mut info: SP_DEVINFO_DATA = unsafe { zeroed() };
        info.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
        win_ok(
            unsafe {
                SetupDiCreateDeviceInfoW(
                    devices.0,
                    wide("Display").as_ptr(),
                    &DISPLAY_CLASS,
                    wide("Virtual Display Driver").as_ptr(),
                    null_mut(),
                    DICD_GENERATE_ID,
                    &mut info,
                )
            },
            "SetupDiCreateDeviceInfoW",
        )?;
        let hardware = wide("Root\\MttVDD\0"); // REG_MULTI_SZ has two final NULs.
        win_ok(
            unsafe {
                SetupDiSetDeviceRegistryPropertyW(
                    devices.0,
                    &mut info,
                    SPDRP_HARDWAREID,
                    hardware.as_ptr().cast(),
                    (hardware.len() * 2) as u32,
                )
            },
            "SetupDiSetDeviceRegistryPropertyW",
        )?;
        Ok(Self {
            devices,
            info,
            instance: String::new(),
            registered: false,
            keep: false,
        })
    }
    fn register(&mut self) -> Result<()> {
        no_existing_mtt_node()?;
        win_ok(
            unsafe { SetupDiCallClassInstaller(DIF_REGISTERDEVICE, self.devices.0, &self.info) },
            "DIF_REGISTERDEVICE",
        )?;
        self.registered = true;
        let mut instance = [0u16; 512];
        win_ok(
            unsafe {
                SetupDiGetDeviceInstanceIdW(
                    self.devices.0,
                    &self.info,
                    instance.as_mut_ptr(),
                    instance.len() as u32,
                    null_mut(),
                )
            },
            "SetupDiGetDeviceInstanceIdW(new node)",
        )?;
        self.instance = String::from_utf16_lossy(
            &instance[..instance
                .iter()
                .position(|v| *v == 0)
                .unwrap_or(instance.len())],
        )
        .to_ascii_lowercase();
        Ok(())
    }
    fn bind(&mut self, directory: &Path) -> Result<()> {
        let inf = path_wide(&directory.join("MttVDD.inf"));
        let mut destination = [0u16; 512];
        win_ok(
            unsafe {
                SetupCopyOEMInfW(
                    inf.as_ptr(),
                    null(),
                    SPOST_PATH,
                    0,
                    destination.as_mut_ptr(),
                    destination.len() as u32,
                    null_mut(),
                    null_mut(),
                )
            },
            "SetupCopyOEMInfW(signed MttVDD)",
        )?;
        // A hardware-ID-wide update could also bind a node concurrently
        // registered by another installer. Select the one staged INF for our
        // exact newly registered device, then install only that device.
        let mut parameters: SP_DEVINSTALL_PARAMS_W = unsafe { zeroed() };
        parameters.cbSize = size_of::<SP_DEVINSTALL_PARAMS_W>() as u32;
        win_ok(
            unsafe { SetupDiGetDeviceInstallParamsW(self.devices.0, &self.info, &mut parameters) },
            "SetupDiGetDeviceInstallParamsW(new node)",
        )?;
        let length = destination
            .iter()
            .position(|value| *value == 0)
            .ok_or_else(|| Error::Platform("staged driver INF path is not terminated".into()))?;
        if length == 0 || length >= parameters.DriverPath.len() {
            return Err(Error::Platform(
                "staged driver INF path exceeds SetupAPI limits".into(),
            ));
        }
        parameters.DriverPath.fill(0);
        parameters.DriverPath[..length].copy_from_slice(&destination[..length]);
        parameters.Flags |= DI_ENUMSINGLEINF | DI_QUIETINSTALL;
        parameters.FlagsEx |= DI_FLAGSEX_ALLOWEXCLUDEDDRVS;
        win_ok(
            unsafe { SetupDiSetDeviceInstallParamsW(self.devices.0, &self.info, &parameters) },
            "SetupDiSetDeviceInstallParamsW(pinned INF)",
        )?;
        win_ok(
            unsafe {
                SetupDiBuildDriverInfoList(self.devices.0, &mut self.info, SPDIT_COMPATDRIVER)
            },
            "SetupDiBuildDriverInfoList(pinned INF, new node)",
        )?;
        let _list = DriverList {
            devices: self.devices.0,
            info: self.info,
        };
        let mut driver: SP_DRVINFO_DATA_V2_W = unsafe { zeroed() };
        driver.cbSize = size_of::<SP_DRVINFO_DATA_V2_W>() as u32;
        win_ok(
            unsafe {
                SetupDiEnumDriverInfoW(
                    self.devices.0,
                    &self.info,
                    SPDIT_COMPATDRIVER,
                    0,
                    &mut driver,
                )
            },
            "SetupDiEnumDriverInfoW(pinned driver)",
        )?;
        let mut other: SP_DRVINFO_DATA_V2_W = unsafe { zeroed() };
        other.cbSize = size_of::<SP_DRVINFO_DATA_V2_W>() as u32;
        let extra = unsafe {
            SetupDiEnumDriverInfoW(
                self.devices.0,
                &self.info,
                SPDIT_COMPATDRIVER,
                1,
                &mut other,
            )
        };
        let error = unsafe { GetLastError() };
        if extra != 0 || error != ERROR_NO_MORE_ITEMS {
            return Err(Error::Platform(
                "pinned driver does not identify one unambiguous compatible model".into(),
            ));
        }
        let mut reboot = 0;
        win_ok(
            unsafe {
                DiInstallDevice(
                    null_mut(),
                    self.devices.0,
                    &self.info,
                    &driver,
                    0,
                    &mut reboot,
                )
            },
            "DiInstallDevice(pinned driver, only the new node)",
        )?;
        if reboot != 0 {
            return Err(Error::Platform("Windows requires a restart to load the driver; the newly created device will be rolled back".into()));
        }
        Ok(())
    }
    fn rollback(&mut self) -> Result<()> {
        if self.keep || !self.registered {
            return Ok(());
        }
        let mut reboot = 0;
        win_ok(
            unsafe { DiUninstallDevice(null_mut(), self.devices.0, &self.info, 0, &mut reboot) },
            "DiUninstallDevice(only the new node)",
        )?;
        self.registered = false;
        if reboot != 0 {
            return Err(Error::Platform(
                "Windows requires a restart to finish removing the new driver node".into(),
            ));
        }
        Ok(())
    }
}
impl Drop for DeviceNode {
    fn drop(&mut self) {
        if let Err(error) = self.rollback() {
            tracing::warn!(%error, "new virtual display device rollback remains pending");
        }
    }
}

fn install_on_desktop(payload: Vec<(String, Vec<u8>)>) -> Result<()> {
    no_existing_mtt_node()?;
    let before = super::windows::active_signature()?;
    let mut directory = ProtectedDirectory::create(&payload)?;
    verify_catalog(&directory.path)?;
    let mut configuration = Configuration::create(&directory.path)?;
    let mut node = DeviceNode::create()?;
    let installation = (|| -> Result<()> {
        node.register()?;
        node.bind(&directory.path)?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let targets = super::windows::targets_for_instance(&node.instance)?;
            if !targets.is_empty() {
                // Installation itself proves ownership of this exact new PnP
                // instance. No pre-existing adapter's screen is adopted.
                for target in &targets {
                    super::windows::deactivate(&target.key)?;
                }
                let after = super::windows::targets_for_instance(&node.instance)?;
                if after.iter().any(|target| target.active) {
                    return Err(Error::Platform(
                        "new virtual display remained active during slot preparation".into(),
                    ));
                }
                if !after.iter().any(|target| target.available) {
                    return Err(Error::Platform(
                        "new driver has no usable inactive display slot".into(),
                    ));
                }
                if super::windows::active_signature()? != before {
                    return Err(Error::Platform("another display changed during driver installation; old global layout was not replayed".into()));
                }
                break;
            }
            if Instant::now() >= deadline {
                return Err(Error::Platform(
                    "new driver target did not appear within 20 seconds".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    })();
    match installation {
        Ok(()) => {
            node.keep = true;
            configuration.keep = true;
            directory.keep = true;
            Ok(())
        }
        Err(error) => {
            if let Err(rollback) = node.rollback() {
                // Keep the verified config available while Windows still has
                // our node; never delete its files and pretend cleanup passed.
                node.keep = true;
                configuration.keep = true;
                directory.keep = true;
                return Err(Error::Platform(format!(
                    "{error}; rollback remains pending: {rollback}"
                )));
            }
            if let Err(rollback) = configuration.rollback() {
                configuration.keep = true;
                directory.keep = true;
                return Err(Error::Platform(format!(
                    "{error}; configuration rollback remains pending: {rollback}"
                )));
            }
            Err(error)
        }
    }
}
