//! Elevated lifecycle of the original signed MttVDD package and owned devices.
//! No certificate import, signature-policy override, driver-global reload,
//! forced upgrade or removal of an unproven existing device is permitted here.

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
    time::Duration,
};
use windows_sys::{
    core::GUID,
    Win32::{
        Devices::DeviceAndDriverInstallation::*,
        Foundation::{
            GetLastError, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, GENERIC_WRITE,
            INVALID_HANDLE_VALUE, ERROR_NO_SUCH_DEVINST, ERROR_PATH_NOT_FOUND, ERROR_NOT_FOUND,
            ERROR_AUTHENTICODE_PUBLISHER_NOT_TRUSTED, ERROR_AUTHENTICODE_TRUST_NOT_ESTABLISHED,
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
        System::{Com::CoTaskMemFree, Registry::*, SystemInformation::GetSystemWindowsDirectoryW},
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

pub(super) fn verify_catalog(directory: &Path) -> Result<()> {
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

/// A downloaded package is not an installed driver. Resolve the exact pinned
/// package in DriverStore and require a published OEM INF before reporting it
/// ready; no monitor or adapter is created by this check.
pub(super) fn published_driver() -> Result<Option<PathBuf>> {
    // SetupGetInfDriverStoreLocation accepts installed INF names, not an
    // arbitrary downloaded INF. Find the published copy by the pinned bytes.
    let mut system = [0u16; 32768];
    let length = unsafe { GetSystemWindowsDirectoryW(system.as_mut_ptr(), system.len() as u32) } as usize;
    if length == 0 || length >= system.len() { return Err(os_error("locate Windows INF directory", unsafe { GetLastError() })); }
    let inf_directory = PathBuf::from(OsString::from_wide(&system[..length])).join("INF");
    let expected = driver_package().files.iter().find(|file| file.name == "MttVDD.inf").ok_or_else(|| Error::Platform("pinned INF identity unavailable".into()))?;
    let mut published = None;
    for entry in std::fs::read_dir(inf_directory).map_err(|error| Error::Platform(error.to_string()))? {
        let entry = entry.map_err(|error| Error::Platform(error.to_string()))?;
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if !name.starts_with("oem") || !name.ends_with(".inf") { continue; }
        let metadata = entry.metadata().map_err(|error| Error::Platform(error.to_string()))?;
        if !metadata.is_file() || metadata.len() != expected.bytes as u64 { continue; }
        let bytes = std::fs::read(entry.path()).map_err(|error| Error::Platform(error.to_string()))?;
        if hex::encode(Sha256::digest(&bytes)) == expected.sha256 { published = Some(entry.path()); break; }
    }
    let Some(published) = published else { return Ok(None); };
    let mut stored = [0u16; 32768];
    if unsafe { SetupGetInfDriverStoreLocationW(path_wide(&published).as_ptr(), null(), null(), stored.as_mut_ptr(), stored.len() as u32, null_mut()) } == 0 {
        let code = unsafe { GetLastError() };
        return match code {
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND | ERROR_NOT_FOUND => Ok(None),
            _ => Err(os_error("locate pinned driver in DriverStore", code)),
        };
    }
    let stored = PathBuf::from(OsString::from_wide(&stored[..stored.iter().position(|c| *c == 0).unwrap_or(stored.len())]));
    let parent = stored.parent().ok_or_else(|| Error::Platform("driver store directory unavailable".into()))?;
    for expected in driver_package().files.iter().filter(|file| file.name != "vdd_settings.xml") {
        let bytes = std::fs::read(parent.join(&expected.name)).map_err(|error| Error::Platform(error.to_string()))?;
        if bytes.len() != expected.bytes || hex::encode(Sha256::digest(&bytes)) != expected.sha256 {
            return Ok(None);
        }
    }
    Ok(Some(published))
}

struct InstallationUi(i32);
impl Drop for InstallationUi {
    fn drop(&mut self) { unsafe { SetupSetNonInteractiveMode(self.0); } }
}

#[cfg(test)]
mod staging_tests {
    use super::*;
    #[test]
    fn noninteractive_setup_scope_restores_the_callers_ui_policy() {
        let previous = unsafe { SetupGetNonInteractiveMode() };
        {
            let _scope = InstallationUi(unsafe { SetupSetNonInteractiveMode(1) });
            assert_ne!(unsafe { SetupGetNonInteractiveMode() }, 0);
        }
        assert_eq!(unsafe { SetupGetNonInteractiveMode() }, previous);
    }

    #[test]
    #[ignore = "requires the pinned signed driver to have been installed on this host; read-only"]
    fn pinned_driver_readiness_requires_published_inf_and_matching_store_files() {
        let published = published_driver().unwrap().expect("pinned driver has not been installed");
        assert!(published.is_file());
        assert!(published.file_name().unwrap().to_string_lossy().to_ascii_lowercase().starts_with("oem"));
    }
}

/// Explicit installation runs in the user's elevated interactive helper.
/// A System host must never display a publisher prompt on an unseen console.
pub(super) fn stage_signed_driver(directory: &Path) -> Result<PathBuf> {
    if let Some(published) = published_driver()? { return Ok(published); }
    verify_catalog(directory)?;
    let system = crate::service_admin::windows::process_user(unsafe { windows_sys::Win32::System::Threading::GetCurrentProcess() })
        .map_err(|error| Error::Platform(error.to_string()))? == "S-1-5-18";
    let _ui = InstallationUi(unsafe { SetupSetNonInteractiveMode(i32::from(system)) });
    let mut destination = [0u16; 32768];
    let copied = unsafe { SetupCopyOEMInfW(path_wide(&directory.join("MttVDD.inf")).as_ptr(), null(), SPOST_PATH, 0, destination.as_mut_ptr(), destination.len() as u32, null_mut(), null_mut()) };
    if copied == 0 {
        let code = unsafe { GetLastError() };
        if system && matches!(code, ERROR_AUTHENTICODE_TRUST_NOT_ESTABLISHED | ERROR_AUTHENTICODE_PUBLISHER_NOT_TRUSTED) {
            return Err(Error::Platform("首次驱动安装需要在主机桌面确认发布者；请在虚拟显示器页点击安装驱动，完成 Windows 确认后重试。".into()));
        }
        return Err(os_error("SetupCopyOEMInfW(signed MttVDD)", code));
    }
    published_driver()?.ok_or_else(|| Error::Platform("signed driver was not published in DriverStore".into()))
}

struct DevInfoSet(HDEVINFO);
impl Drop for DevInfoSet {
    fn drop(&mut self) {
        unsafe {
            SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}
pub(super) fn no_existing_mtt_node() -> Result<()> {
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
        let read = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                devices.0,
                &info,
                SPDRP_HARDWAREID,
                null_mut(),
                data.as_mut_ptr().cast(),
                std::mem::size_of_val(&data) as u32,
                null_mut(),
            )
        };
        if read == 0 {return Err(os_error("verify installed display hardware identity",unsafe{GetLastError()}));}
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

fn configuration_absent() -> Result<bool> {
    let mut bytes = 0;
    let code = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            wide(CONFIG_KEY).as_ptr(),
            wide("VDDPATH").as_ptr(),
            RRF_RT_ANY,
            null_mut(),
            null_mut(),
            &mut bytes,
        )
    };
    match code {
        ERROR_FILE_NOT_FOUND => Ok(true),
        0 => Ok(false),
        code => Err(os_error("read VDDPATH absence", code)),
    }
}

struct Configuration {
    key: HKEY,
    value: Vec<u16>,
    keep: bool,
}
/// An explicit recovery after uninstall may retire only the exact old registry
/// value. No device is removed and the old installation files remain evidence.
pub(super) fn retire_uninstalled_configuration(directory:&Path) -> Result<()> {
    no_existing_mtt_node()?; // includes phantom devices; absence is not disabled
    if configuration_absent()? {return Ok(());}
    let mut key=null_mut();
    let code=unsafe{RegOpenKeyExW(HKEY_LOCAL_MACHINE,wide(CONFIG_KEY).as_ptr(),0,
        KEY_QUERY_VALUE|KEY_SET_VALUE|KEY_WOW64_64KEY,&mut key)};
    if code!=0{return Err(os_error("open uninstalled driver configuration",code));}
    let mut configuration=Configuration{key,value:path_wide(directory),keep:true};
    // rollback compares the complete current REG_SZ on this same handle.
    configuration.rollback()?;
    if !configuration_absent()? {return Err(Error::Platform("uninstalled driver configuration remains".into()));}
    no_existing_mtt_node()
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
            let failure = os_error("RegSetValueExW(MttVDD path)", code);
            let rollback = config.rollback();
            config.keep = true;
            return Err(match rollback {
                Ok(()) => failure,
                Err(error) => Error::Platform(format!(
                    "{failure}; exact VDDPATH recovery pending: {error}"
                )),
            });
        }
        // Capacity owns explicit compensation. Drop during a panic must not
        // remove a config while its accepted device install may still run.
        config.keep = true;
        Ok(config)
    }
    fn rollback(&mut self) -> Result<()> {
        if self.value.is_empty() {
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
        if !self.keep {
            if let Err(error) = self.rollback() {
                tracing::warn!(%error,"virtual display configuration rollback remains pending");
            }
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
struct RegistryKey(HKEY);
impl Drop for RegistryKey { fn drop(&mut self) { unsafe { RegCloseKey(self.0); } } }
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
        Self::create_named("Virtual Display Driver")
    }
    fn create_named(description: &str) -> Result<Self> {Self::create_identity(description,None)}
    fn create_identity(description:&str,identity:Option<&str>)->Result<Self> {
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
                    wide(identity.unwrap_or("Display")).as_ptr(),
                    &DISPLAY_CLASS,
                    wide(description).as_ptr(),
                    null_mut(),
                    if identity.is_some(){0}else{DICD_GENERATE_ID},
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
    fn refresh_instance(&mut self) -> Result<()> {
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
            "read exact new device instance",
        )?;
        let length = instance
            .iter()
            .position(|v| *v == 0)
            .ok_or_else(|| Error::Platform("new device instance is not terminated".into()))?;
        let instance = String::from_utf16(&instance[..length])
            .map_err(|_| Error::Platform("new device instance is invalid UTF-16".into()))?
            .to_ascii_lowercase();
        if instance.is_empty() || (!self.instance.is_empty() && self.instance != instance) {
            return Err(Error::Platform(
                "new device instance identity changed".into(),
            ));
        }
        self.instance = instance;
        Ok(())
    }
    fn register(&mut self) -> Result<()> {
        no_existing_mtt_node()?;
        self.register_exact()
    }
    fn register_exact(&mut self) -> Result<()> {
        // Best effort before registration; some SetupAPI providers expose the
        // generated ID only after creating the devnode. A failed DIF may still
        // have accepted a registration, so mark submission before invoking it.
        let _ = self.refresh_instance();
        self.registered = true;
        let submitted = win_ok(
            unsafe { SetupDiCallClassInstaller(DIF_REGISTERDEVICE, self.devices.0, &self.info) },
            "DIF_REGISTERDEVICE",
        );
        let identity = self.refresh_instance();
        submitted?;
        identity
    }
    fn bind(&mut self, directory: &Path) -> Result<()> {
        self.bind_exact(directory, None)
    }
    fn bind_exact(&mut self, directory: &Path, group: Option<&str>) -> Result<()> {
        let destination = path_wide(&stage_signed_driver(directory)?);
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
        if group.is_some() { parameters.Flags |= DI_DONOTCALLCONFIGMG; }
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
        if let Some(group) = group {
            win_ok(unsafe { SetupDiSetSelectedDriverW(self.devices.0, &mut self.info, &mut driver) }, "select exact signed driver")?;
            win_ok(unsafe { SetupDiCallClassInstaller(DIF_INSTALLDEVICE, self.devices.0, &self.info) }, "install deferred exact device")?;
            self.set_value("WUDF", "DeviceGroupId", group)?;
            if self.value("WUDF", "DeviceGroupId")? != group { return Err(Error::Platform("UMDF isolation group was not retained".into())); }
            win_ok(unsafe { SetupDiGetDeviceInstallParamsW(self.devices.0, &self.info, &mut parameters) }, "read deferred install flags")?;
            if parameters.Flags & DI_NEEDREBOOT != 0 { return Err(Error::Platform("Windows requires a restart to install this device".into())); }
            parameters.Flags &= !DI_DONOTCALLCONFIGMG;
            win_ok(unsafe { SetupDiSetDeviceInstallParamsW(self.devices.0, &self.info, &parameters) }, "complete exact device settings")?;
            // Go through the class installer for this devnode only. The
            // SetupDiRestartDevices default handler is reserved for class
            // installers and can restart sibling devices after file changes.
            // Use a fresh information set: the install set retains deferred
            // restart state inside SetupAPI even after updating Flags.
            let mut fresh=open_demand_device(&self.instance)?.ok_or(Error::UnknownSlot)?;
            let mut fresh_parameters:SP_DEVINSTALL_PARAMS_W=unsafe{zeroed()};fresh_parameters.cbSize=size_of::<SP_DEVINSTALL_PARAMS_W>() as u32;
            win_ok(unsafe{SetupDiGetDeviceInstallParamsW(fresh.devices.0,&fresh.info,&mut fresh_parameters)},"read fresh device start parameters")?;
            fresh_parameters.Flags &= !DI_DONOTCALLCONFIGMG;
            win_ok(unsafe{SetupDiSetDeviceInstallParamsW(fresh.devices.0,&fresh.info,&fresh_parameters)},"allow exact device start")?;
            let change = SP_PROPCHANGE_PARAMS {
                ClassInstallHeader:SP_CLASSINSTALL_HEADER { cbSize:size_of::<SP_CLASSINSTALL_HEADER>() as u32, InstallFunction:DIF_PROPERTYCHANGE },
                StateChange:DICS_PROPCHANGE, Scope:DICS_FLAG_GLOBAL, HwProfile:0,
            };
            win_ok(unsafe { SetupDiSetClassInstallParamsW(fresh.devices.0, &fresh.info, &change.ClassInstallHeader, size_of::<SP_PROPCHANGE_PARAMS>() as u32) }, "prepare exact device start")?;
            return win_ok(unsafe { SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, fresh.devices.0, &fresh.info) }, "start isolated exact device");
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
    fn hardware_key(&self) -> Result<RegistryKey> {
        let key = unsafe { SetupDiCreateDevRegKeyW(self.devices.0, &self.info, DICS_FLAG_GLOBAL, 0, DIREG_DEV, null(), null()) };
        if key == INVALID_HANDLE_VALUE { return Err(os_error("open exact device hardware key", unsafe { GetLastError() })); }
        Ok(RegistryKey(key))
    }
    fn set_value(&self, subkey:&str, name:&str, value:&str) -> Result<()> {
        let root = self.hardware_key()?;
        let mut key = null_mut();
        let code = unsafe { RegCreateKeyExW(root.0, wide(subkey).as_ptr(), 0, null(), 0, KEY_SET_VALUE|KEY_QUERY_VALUE, null(), &mut key, null_mut()) };
        if code != 0 { return Err(os_error("create exact device setting", code)); }
        let key = RegistryKey(key); let value = wide(value);
        let code = unsafe { RegSetValueExW(key.0, wide(name).as_ptr(), 0, REG_SZ, value.as_ptr().cast(), (value.len()*2) as u32) };
        if code != 0 { return Err(os_error("write exact device setting", code)); }
        Ok(())
    }
    fn value(&self, subkey:&str, name:&str) -> Result<String> {
        let raw=unsafe { SetupDiOpenDevRegKey(self.devices.0,&self.info,DICS_FLAG_GLOBAL,0,DIREG_DEV,KEY_QUERY_VALUE) };
        if raw==INVALID_HANDLE_VALUE { return Err(os_error("read exact hardware key",unsafe{GetLastError()})); }
        let root=RegistryKey(raw);
        let mut value = [0u16;1024]; let mut length = std::mem::size_of_val(&value) as u32;
        let code = unsafe { RegGetValueW(root.0, wide(subkey).as_ptr(), wide(name).as_ptr(), RRF_RT_REG_SZ, null_mut(), value.as_mut_ptr().cast(), &mut length) };
        if code != 0 { return Err(os_error("read exact device setting", code)); }
        let end = value.iter().position(|v| *v == 0).ok_or_else(|| Error::Platform("unterminated device setting".into()))?;
        String::from_utf16(&value[..end]).map_err(|_| Error::Platform("invalid device setting".into()))
    }
    fn rollback(&mut self) -> Result<()> {
        if !self.registered {
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
        if !self.keep {
            if let Err(error) = self.rollback() {
                tracing::warn!(%error,"new virtual display device rollback remains pending");
            }
        }
    }
}

/// Stage the original signed package without publishing a monitor. Every
/// subsequently created isolated adapter reads exactly one monitor from it.
pub(super) fn stage_demand_package(payload:&[(String,Vec<u8>)]) -> Result<PathBuf> {
    let mut directory = ProtectedDirectory::create(payload)?;
    verify_catalog(&directory.path)?;
    stage_signed_driver(&directory.path)?;
    let file = directory.path.join("vdd_settings.xml");
    let before = std::fs::read(&file).map_err(|e|Error::Platform(e.to_string()))?;
    let change = super::capacity_config::exact_count(&before,1)?;
    super::windows::provision::capacity::store::replace_exact(&file,&before,&change.after)?;
    directory.keep = true;
    Ok(directory.path.clone())
}
pub(super) fn demand_configuration(directory:&Path) -> Result<()> {
    let mut current = [0u16;32768]; let mut length=std::mem::size_of_val(&current) as u32;
    let code=unsafe { RegGetValueW(HKEY_LOCAL_MACHINE,wide(CONFIG_KEY).as_ptr(),wide("VDDPATH").as_ptr(),RRF_RT_REG_SZ,null_mut(),current.as_mut_ptr().cast(),&mut length) };
    if code == ERROR_FILE_NOT_FOUND { let _owned=Configuration::create(directory)?; return Ok(()); }
    let expected=path_wide(directory);
    if code != 0 || length as usize != expected.len()*2 || current[..expected.len()] != expected {
        return Err(Error::Platform("MttVDD configuration belongs to another installation; it was preserved".into()));
    }
    Ok(())
}
pub(super) fn current_configuration() -> Result<Option<PathBuf>> {
    let mut value=[0u16;32768]; let mut length=std::mem::size_of_val(&value) as u32;
    let code=unsafe { RegGetValueW(HKEY_LOCAL_MACHINE,wide(CONFIG_KEY).as_ptr(),wide("VDDPATH").as_ptr(),RRF_RT_REG_SZ,null_mut(),value.as_mut_ptr().cast(),&mut length) };
    if code==ERROR_FILE_NOT_FOUND { return Ok(None); }
    if code!=0 { return Err(os_error("read current Mtt configuration",code)); }
    let end=value.iter().position(|c|*c==0).ok_or_else(||Error::Platform("invalid Mtt configuration path".into()))?;
    Ok(Some(PathBuf::from(OsString::from_wide(&value[..end]))))
}
pub(super) fn create_demand_device(directory:&Path, token:&str, id:Uuid, registered:&mut dyn FnMut(&str)->Result<()>) -> Result<String> {
    let description=format!("Eclipse Virtual Display {token}");
    let mut device=DeviceNode::create_identity(&description,Some(&format!("ROOT\\DISPLAY\\{}",id.simple())))?;
    // Durable recovery owns the accepted registration, including an error or
    // process exit before SetupAPI returns its exact instance ID.
    device.keep=true;
    device.register_exact()?;
    registered(&device.instance)?;
    device.set_value("Eclipse","Owner",token)?;
    device.bind_exact(directory,Some(&format!("EclipseVDD-{token}")))?;
    Ok(device.instance.clone())
}
pub(super) fn demand_instances() -> Result<Vec<String>> {
    let handle=unsafe { SetupDiGetClassDevsW(&DISPLAY_CLASS,null(),null_mut(),0) };
    if handle==INVALID_HANDLE_VALUE as isize { return Err(os_error("enumerate Mtt devices",unsafe{GetLastError()})); }
    let devices=DevInfoSet(handle); let mut result=Vec::new();
    for index in 0..4096 {
        let mut info:SP_DEVINFO_DATA=unsafe{zeroed()}; info.cbSize=size_of::<SP_DEVINFO_DATA>() as u32;
        if unsafe { SetupDiEnumDeviceInfo(devices.0,index,&mut info) }==0 {
            let code=unsafe{GetLastError()};
            if code==ERROR_NO_MORE_ITEMS { return Ok(result); }
            return Err(os_error("enumerate exact Mtt node",code));
        }
        let hardware=property(devices.0,&info,SPDRP_HARDWAREID)?;
        if hardware.split('\0').any(|id|matches!(id.to_ascii_lowercase().as_str(),"root\\mttvdd"|"mttvdd")) {
            let mut id=[0u16;512];
            win_ok(unsafe{SetupDiGetDeviceInstanceIdW(devices.0,&info,id.as_mut_ptr(),id.len() as u32,null_mut())},"read Mtt instance")?;
            result.push(String::from_utf16_lossy(&id[..id.iter().position(|c|*c==0).unwrap_or(id.len())]).to_ascii_lowercase());
        }
    }
    Err(Error::Platform("Mtt device inventory exceeds safety bound".into()))
}
fn property(devices:HDEVINFO,info:&SP_DEVINFO_DATA,kind:u32) -> Result<String> {
    let mut value=[0u16;2048];
    win_ok(unsafe{SetupDiGetDeviceRegistryPropertyW(devices,info,kind,null_mut(),value.as_mut_ptr().cast(),std::mem::size_of_val(&value) as u32,null_mut())},"read exact device property")?;
    Ok(String::from_utf16_lossy(&value).trim_end_matches('\0').to_owned())
}
fn open_demand_device(instance:&str) -> Result<Option<DeviceNode>> {
    let handle=unsafe{SetupDiCreateDeviceInfoList(&DISPLAY_CLASS,null_mut())};
    if handle==INVALID_HANDLE_VALUE as isize { return Err(os_error("open exact device set",unsafe{GetLastError()})); }
    let devices=DevInfoSet(handle); let mut info:SP_DEVINFO_DATA=unsafe{zeroed()};info.cbSize=size_of::<SP_DEVINFO_DATA>() as u32;
    if unsafe{SetupDiOpenDeviceInfoW(devices.0,wide(instance).as_ptr(),null_mut(),0,&mut info)}==0 {
        let code=unsafe{GetLastError()};
        if matches!(code,ERROR_NO_SUCH_DEVINST|ERROR_FILE_NOT_FOUND) { return Ok(None); }
        return Err(os_error("open exact virtual device",code));
    }
    let hardware=property(devices.0,&info,SPDRP_HARDWAREID)?;
    if !hardware.split('\0').any(|id|matches!(id.to_ascii_lowercase().as_str(),"root\\mttvdd"|"mttvdd")) { return Err(Error::Platform("recorded device no longer has the original hardware identity".into())); }
    Ok(Some(DeviceNode{devices,info,instance:instance.into(),registered:true,keep:true}))
}
pub(super) fn verify_demand_device(instance:&str,token:&str,ready:bool) -> Result<bool> {
    let Some(device)=open_demand_device(instance)? else { return Ok(false); };
    let owner=device.value("Eclipse","Owner");
    if !owner.as_ref().is_ok_and(|value|value==token) {
        if ready || property(device.devices.0,&device.info,SPDRP_DEVICEDESC)? != format!("Eclipse Virtual Display {token}") {
            return Err(Error::Platform("recorded virtual device ownership changed; device was preserved".into()));
        }
    }
    if ready && device.value("WUDF","DeviceGroupId")? != format!("EclipseVDD-{token}") { return Err(Error::Platform("recorded UMDF isolation group changed; device was preserved".into())); }
    Ok(true)
}
pub(super) fn demand_problem(instance:&str) -> String {
    match open_demand_device(instance) {
        Ok(Some(device))=> {
            let (mut status,mut problem)=(0,0);
            let code=unsafe {CM_Get_DevNode_Status(&mut status,&mut problem,device.info.DevInst,0)};
            format!("instance={instance}, cm={code}, status={status:#x}, problem={problem}, group={:?}",device.value("WUDF","DeviceGroupId"))
        },
        Ok(None)=>format!("instance={instance}, absent"),
        Err(error)=>format!("instance={instance}, {error}"),
    }
}
pub(super) fn find_unbound_demand(token:&str) -> Result<Option<String>> {
    let mut found=None;
    for instance in demand_instances()? {
        let Some(device)=open_demand_device(&instance)? else { continue; };
        if property(device.devices.0,&device.info,SPDRP_DEVICEDESC)?==format!("Eclipse Virtual Display {token}") {
            if found.is_some() { return Err(Error::Platform("interrupted virtual device identity is ambiguous".into())); }
            found=Some(instance);
        }
    }
    Ok(found)
}
/// The pinned Mtt driver stops its blocking pipe listener by connecting to a
/// shared pipe name. With several independent hosts that connection can wake
/// another instance, leaving its own stop waiting for UMDF's 60-second timeout.
/// During an exact owned uninstall, empty anonymous connections wake idle
/// listeners; no driver command or data is ever sent and active clients remain
/// connected. This compatibility path is confined to our isolated Mtt devices.
fn uninstall_isolated_device(device: &mut DeviceNode) -> Result<()> {
    use std::sync::{Arc,atomic::{AtomicBool,Ordering}};
    use windows_sys::Win32::{Foundation::{CloseHandle,GENERIC_READ},Storage::FileSystem::{OPEN_EXISTING,SECURITY_SQOS_PRESENT,SECURITY_ANONYMOUS}};
    let finished=Arc::new(AtomicBool::new(false));
    let stopping=finished.clone();
    let wake=std::thread::Builder::new().name("eclipse-vdd-stop-wakeup".into()).spawn(move||{
        let name=wide(r"\\.\pipe\MTTVirtualDisplayPipe");
        while !stopping.load(Ordering::Acquire) {
            // Anonymous SQOS prevents a substituted local pipe server from
            // impersonating this privileged installer. Never wait for a busy
            // instance or write/read protocol bytes.
            let handle=unsafe{CreateFileW(name.as_ptr(),GENERIC_READ|GENERIC_WRITE,0,null(),OPEN_EXISTING,SECURITY_SQOS_PRESENT|SECURITY_ANONYMOUS,null_mut())};
            if handle!=INVALID_HANDLE_VALUE {unsafe{CloseHandle(handle);}}
            std::thread::park_timeout(Duration::from_millis(20));
        }
    }).map_err(|e|Error::Platform(e.to_string()))?;
    let result=device.rollback();
    finished.store(true,Ordering::Release);wake.thread().unpark();
    wake.join().map_err(|_|Error::Platform("isolated device wakeup worker panicked".into()))?;
    result
}
pub(super) fn remove_demand_device(instance:&str,token:&str,ready:bool) -> Result<()> {
    if !verify_demand_device(instance,token,ready)? { return Ok(()); }
    if let Some(mut device)=open_demand_device(instance)? {
        if device.value("WUDF","DeviceGroupId").is_ok_and(|group|group==format!("EclipseVDD-{token}")) {uninstall_isolated_device(&mut device)?;} else {device.rollback()?;}
    }
    if open_demand_device(instance)?.is_some() { return Err(Error::Platform("exact virtual device removal is not yet complete".into())); }
    Ok(())
}
/// Only the receipt, pinned driver and inactive-target checks in the capacity
/// migration path authorize this legacy operation. Names never authorize it.
pub(super) fn remove_verified_legacy(instance:&str) -> Result<()> {
    if let Some(mut device)=open_demand_device(instance)? {device.rollback()?;}
    if open_demand_device(instance)?.is_some() {return Err(Error::Platform("legacy virtual adapter removal is pending".into()));}
    Ok(())
}

pub(super) struct InstallationFailure {
    pub error: Error,
    pub cleanup_verified: bool,
}

/// Compensate only a node and configuration created by this still-live
/// transaction. A durable journal alone never grants this capability.
pub(super) fn rollback_capacity_install(
    instance: &str,
    directory: &Path,
    expected_xml: &[u8],
    baseline: &super::windows::Fingerprints,
    saved_baseline: &super::windows::Fingerprints,
    checkpoint: &(dyn Fn() -> super::preparation::Result<()> + Sync),
    claims: &mut BTreeMap<TargetKey, Box<dyn Claim>>,
    phase: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    checkpoint().map_err(|e| Error::Platform(e.message))?;
    // Do not remove files an administrator changed after the installation.
    for expected in &driver_package().files {
        let bytes = super::windows::provision::capacity::read_configuration(
            &directory.join(&expected.name),
        )?;
        let owned = if expected.name == "vdd_settings.xml" {
            bytes == expected_xml
        } else {
            bytes.len() == expected.bytes && hex::encode(Sha256::digest(&bytes)) == expected.sha256
        };
        if !owned {
            return Err(Error::Platform(
                "new driver staging files changed; recovery did not overwrite them".into(),
            ));
        }
    }
    let mut key = null_mut();
    let code = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            wide(CONFIG_KEY).as_ptr(),
            0,
            KEY_QUERY_VALUE | KEY_SET_VALUE | KEY_WOW64_64KEY,
            &mut key,
        )
    };
    if code != 0 {
        return Err(os_error(
            "open owned new-driver configuration for recovery",
            code,
        ));
    }
    let mut configuration = Configuration {
        key,
        value: path_wide(directory),
        keep: true,
    };
    // Prove the current value before touching the node. rollback() repeats the
    // exact comparison on the same key before removing only VDDPATH.
    let mut current = vec![0u16; configuration.value.len() + 1];
    let mut bytes = (current.len() * 2) as u32;
    let mut kind = 0;
    let code = unsafe {
        RegQueryValueExW(
            key,
            wide("VDDPATH").as_ptr(),
            null_mut(),
            &mut kind,
            current.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    current.truncate(bytes as usize / 2);
    if code != 0 || kind != REG_SZ || current != configuration.value {
        return Err(Error::Platform(
            "new-driver configuration ownership changed before recovery".into(),
        ));
    }
    let handle = unsafe { SetupDiGetClassDevsW(&DISPLAY_CLASS, null(), null_mut(), DIGCF_PRESENT) };
    if handle == INVALID_HANDLE_VALUE as isize {
        return Err(os_error("open new-driver recovery inventory", unsafe {
            GetLastError()
        }));
    }
    let devices = DevInfoSet(handle);
    let mut info: SP_DEVINFO_DATA = unsafe { zeroed() };
    info.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
    win_ok(
        unsafe {
            SetupDiOpenDeviceInfoW(devices.0, wide(instance).as_ptr(), null_mut(), 0, &mut info)
        },
        "open only the owned new driver instance",
    )?;
    let mut node = DeviceNode {
        devices,
        info,
        instance: instance.into(),
        registered: true,
        keep: true,
    };
    node.refresh_instance()?;
    // Installation failure may itself have moved another output. Cleanup still
    // owns this exact new node, never that other output. Preserve its fresh
    // state instead of requiring/replaying the old global layout to remove us.
    let (preserved, saved) = super::windows::settled_readback(Duration::from_secs(10), checkpoint, || {
        super::windows::provision::capacity::installation_external_state(instance, claims)
    })?;
    checkpoint().map_err(|e| Error::Platform(e.message))?;
    phase("installation_remove_submitted")?;
    node.rollback()?;
    phase("installation_removed")?;
    super::windows::settled_readback(Duration::from_secs(10), checkpoint, || {
        no_existing_mtt_node()?;
        let active = super::windows::active_signature()?;
        let database = super::windows::provision::capacity::database()?;
        if !super::provision_policy::rollback_matches(baseline, &preserved, &active)
            || !super::provision_policy::rollback_matches(saved_baseline, &saved, &database) {
            return Err(Error::Platform("original displays have not settled after new-driver removal; no saved layout was replayed".into()));
        }
        Ok((active, database))
    })?;
    configuration.rollback()?;
    if !configuration_absent()? {
        return Err(Error::Platform(
            "new-driver configuration removal is not verified".into(),
        ));
    }
    // Drop removes only the four fixed, byte-verified files, never a recursive
    // foreign directory tree or unrelated driver-store package.
    drop(ProtectedDirectory {
        path: directory.to_path_buf(),
        keep: false,
    });
    Ok(())
}

pub(super) fn install_capacity_on_desktop(
    payload: &[(String, Vec<u8>)],
    minimum: u32,
    original_active: &super::windows::Fingerprints,
    original_saved: &super::windows::Fingerprints,
    checkpoint: &(dyn Fn() -> super::preparation::Result<()> + Sync),
    claims: &mut BTreeMap<TargetKey, Box<dyn Claim>>,
    created: &mut dyn FnMut(&str, &str, &Path) -> Result<()>,
) -> std::result::Result<(), InstallationFailure> {
    let preflight = (|| -> Result<_> {
        // Revalidate memory inputs at the privileged installer boundary.
        if payload.len() != driver_package().files.len() {
            return Err(Error::Platform("driver payload file count mismatch".into()));
        }
        for expected in &driver_package().files {
            let matches: Vec<_> = payload
                .iter()
                .filter(|(name, _)| name == &expected.name)
                .collect();
            if matches.len() != 1
                || matches[0].1.len() != expected.bytes
                || hex::encode(Sha256::digest(&matches[0].1)) != expected.sha256
            {
                return Err(Error::Platform(
                    "driver payload differs from the pinned official release".into(),
                ));
            }
        }
        checkpoint().map_err(|e| Error::Platform(e.message))?;
        no_existing_mtt_node()?;
        let (before, saved) = super::windows::settled_readback(Duration::from_secs(10), checkpoint, || {
            let active = super::windows::active_signature()?;
            let saved = super::windows::provision::capacity::database()?;
            if &active != original_active || &saved != original_saved {
                return Err(Error::Platform("original displays changed before driver installation; no node was installed".into()));
            }
            Ok((active, saved))
        })?;
        if before.is_empty() {
            return Err(Error::LastActiveOutput);
        }
        if !configuration_absent()? {
            return Err(Error::Platform(
                "an existing Mtt configuration path was found; it was left unchanged".into(),
            ));
        }
        let directory = ProtectedDirectory::create(payload)?;
        verify_catalog(&directory.path)?;
        // Verify the untouched signed payload first, then derive only its XML.
        // XML is runtime user configuration and is not a replacement catalog.
        let path = directory.path.join("vdd_settings.xml");
        let original = std::fs::read(&path).map_err(|e| Error::Platform(e.to_string()))?;
        let change = super::capacity_config::expand(&original, minimum)?;
        super::windows::provision::capacity::replace_configuration(
            &path,
            &change.before,
            &change.after,
        )?;
        // All fallible zero-write setup precedes the first HKLM mutation.
        let node = DeviceNode::create()?;
        Ok((before, saved, directory, node))
    })();
    let (before, saved, mut directory, mut node) = preflight.map_err(|error| InstallationFailure {
        error,
        cleanup_verified: true,
    })?;
    let mut configuration = None;
    // Retain stage on unwinding after a potentially accepted registry/PnP
    // write. Normal errors explicitly prove rollback below before deleting it.
    directory.keep = true;
    node.keep = true;
    let mut stage = "register new adapter";
    let installation = (|| -> Result<()> {
        checkpoint().map_err(|e| Error::Platform(e.message))?;
        created("installation_submitted", "", &directory.path)?;
        configuration = Some(Configuration::create(&directory.path)?);
        node.register()?;
        created("installation_submitted", &node.instance, &directory.path)?;
        checkpoint().map_err(|e| Error::Platform(e.message))?;
        stage = "bind pinned driver";
        node.bind(&directory.path)?;
        stage = "wait for complete monitor identities";
        let targets = super::windows::provision::capacity::ready_instance(
            &before, &node.instance, Some(minimum as usize), Duration::from_secs(45), checkpoint, claims,
        )?;
        stage = "prepare inactive pool";
        super::windows::settled_readback(Duration::from_secs(5), checkpoint, || {
            super::windows::provision::capacity::verify_new_instance(&before, &node.instance)
        })?;
        checkpoint().map_err(|e| Error::Platform(e.message))?;
        super::windows::deactivate_claimed(&targets, claims)?;
        stage = "verify inactive pool";
        super::windows::settled_readback(Duration::from_secs(10), checkpoint, || {
            let after = super::windows::targets_for_instance(&node.instance)?;
            if after.iter().map(|t| &t.key).collect::<std::collections::BTreeSet<_>>() != targets.iter().map(|t| &t.key).collect::<std::collections::BTreeSet<_>>()
                || after.iter().any(|target| target.active || !target.available || target.actual.is_some())
                || super::windows::active_signature()? != before {
                return Err(Error::Platform("new driver inactive slots or original outputs have not settled".into()));
            }
            Ok(after)
        })?;
        Ok(())
    })();
    match installation {
        Ok(()) => {
            node.keep = true;
            if let Some(config) = configuration.as_mut() {
                config.keep = true;
            }
            directory.keep = true;
            Ok(())
        }
        Err(error) => {
            tracing::warn!(%stage,instance=%node.instance,%error,"virtual display first installation failed before cleanup");
            // Accepted PnP changes are not cancelled by a caller timeout. If a
            // authorization changed, retain this exact new node/config and claims.
            if let Err(admission) = checkpoint() {
                if !node.registered {
                    let restored = match configuration.as_mut() {
                        Some(config) => config.rollback(),
                        None => configuration_absent().and_then(|absent| {
                            if absent {
                                Ok(())
                            } else {
                                Err(Error::Platform(
                                    "configuration creation recovery remains pending".into(),
                                ))
                            }
                        }),
                    };
                    if restored.is_ok() && configuration_absent().unwrap_or(false) {
                        directory.keep = false;
                        return Err(InstallationFailure {
                            error,
                            cleanup_verified: true,
                        });
                    }
                }
                node.keep = true;
                if let Some(config) = configuration.as_mut() {
                    config.keep = true;
                }
                directory.keep = true;
                return Err(InstallationFailure {
                    error: Error::Platform(format!(
                        "{error}; idle-host recovery pending: {}",
                        admission.message
                    )),
                    cleanup_verified: false,
                });
            }
            if node.registered {
                let xml=super::capacity_config::expand(
                    &payload.iter().find(|(name,_)|name=="vdd_settings.xml").expect("verified XML payload").1,
                    minimum,
                ).expect("verified capacity configuration").after;
                let rollback=rollback_capacity_install(&node.instance,&directory.path,&xml,&before,&saved,checkpoint,claims,
                    &mut |phase| created(phase, &node.instance, &directory.path));
                return Err(InstallationFailure {
                    cleanup_verified:rollback.is_ok(),
                    error:match rollback {Ok(())=>error,Err(recovery)=>Error::Platform(format!("{error}; exact new-device cleanup remains pending: {recovery}"))},
                });
            }
            let ownership = (|| -> Result<()> {
                if node.registered {
                    node.refresh_instance()?;
                    created("installation_submitted", &node.instance, &directory.path)?;
                    let targets = super::windows::targets_for_instance(&node.instance)?;
                    for target in targets {
                        if !claims.contains_key(&target.key) {
                            claims.insert(
                                target.key.clone(),
                                super::ownership_windows::claim(&target.key)?,
                            );
                        }
                    }
                    super::windows::provision::capacity::verify_new_instance(
                        &before,
                        &node.instance,
                    )?;
                }
                checkpoint().map_err(|e| Error::Platform(e.message))?;
                Ok(())
            })();
            if let Err(recovery) = ownership {
                return Err(InstallationFailure {
                    error: Error::Platform(format!(
                        "{error}; exact new-device ownership recovery pending: {recovery}"
                    )),
                    cleanup_verified: false,
                });
            }
            if let Err(rollback) = node.rollback() {
                // Keep the verified config available while Windows still has
                // our node; never delete its files and pretend cleanup passed.
                node.keep = true;
                if let Some(config) = configuration.as_mut() {
                    config.keep = true;
                }
                directory.keep = true;
                return Err(InstallationFailure {
                    error: Error::Platform(format!(
                        "{error}; rollback remains pending: {rollback}"
                    )),
                    cleanup_verified: false,
                });
            }
            let rollback = match configuration.as_mut() {
                Some(config) => config.rollback(),
                None => configuration_absent().and_then(|absent| {
                    if absent {
                        Ok(())
                    } else {
                        Err(Error::Platform(
                            "VDDPATH remains after failed configuration creation".into(),
                        ))
                    }
                }),
            };
            if let Err(rollback) = rollback {
                return Err(InstallationFailure {
                    error: Error::Platform(format!(
                        "{error}; configuration rollback remains pending: {rollback}"
                    )),
                    cleanup_verified: false,
                });
            }
            let gone = if node.instance.is_empty() {
                !node.registered
            } else {
                super::windows::targets_for_instance(&node.instance)
                    .is_ok_and(|targets| targets.is_empty())
            };
            let verified = gone
                && super::windows::active_signature().is_ok_and(|after| after == before)
                && configuration_absent().unwrap_or(false);
            if verified {
                directory.keep = false;
            }
            Err(InstallationFailure {
                error,
                cleanup_verified: verified,
            })
        }
    }
}

/// A per-instance configuration uses the signed driver's documented XML/EDID
/// inputs. Existing isolated UMDF hosts retain their already loaded settings.
pub(super) fn stage_monitor_configuration(base:&Path,token:Uuid,id:Uuid,name:&str,spec:Spec)->Result<PathBuf> {
    let directory=base.join(format!("monitor-{token}"));
    std::fs::create_dir(&directory).map_err(|e|Error::Platform(e.to_string()))?;
    let xml=std::fs::read(base.join("vdd_settings.xml")).map_err(|e|Error::Platform(e.to_string()))?;
    let xml=String::from_utf8(xml).map_err(|e|Error::Platform(e.to_string()))?;
    let needle="<CustomEdid>false</CustomEdid>";
    if xml.matches(needle).count()!=1 {return Err(Error::Platform("pinned CustomEdid setting is not unique".into()));}
    let mut xml=xml.replace(needle,"<CustomEdid>true</CustomEdid>");
    #[cfg(test)] {
        xml=xml.replace("<logging>false</logging>","<logging>true</logging>").replace("<debuglogging>false</debuglogging>","<debuglogging>true</debuglogging>");
        let sddl=wide("O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;LS)");
        let mut descriptor=null_mut();
        win_ok(unsafe{ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(),SDDL_REVISION_1,&mut descriptor,null_mut())},"fixture log security")?;
        let mut attributes=SECURITY_ATTRIBUTES{nLength:size_of::<SECURITY_ATTRIBUTES>() as u32,lpSecurityDescriptor:descriptor,bInheritHandle:0};
        let created=unsafe{CreateDirectoryW(path_wide(&directory.join("Logs")).as_ptr(),&mut attributes)};
        unsafe{windows_sys::Win32::Foundation::LocalFree(descriptor);}
        win_ok(created,"create own driver fixture log directory")?;
    }
    // Preserve the signed release's tested mode table. Add only the exact
    // requested definition mode; advertising speculative modes can cause
    // IddCxMonitorArrival to reject the entire monitor with STATUS_NOT_SUPPORTED.
    let standard_size=[(800,600),(1366,768),(1920,1080),(2560,1440),(3840,2160)].contains(&(spec.width,spec.height));
    let standard_rate=[30_000,60_000,90_000,120_000,144_000,165_000,244_000].contains(&spec.refresh_millihz);
    let mut extra=String::new();
    let mode=|rate:u32|format!("<resolution><width>{}</width><height>{}</height><refresh_rate>{}</refresh_rate></resolution>",spec.width,spec.height,rate as f64/1000.);
    if !standard_size {extra.push_str(&mode(30_000));}
    if !standard_rate {extra.push_str(&mode(spec.refresh_millihz));}
    if !extra.is_empty() {xml=xml.replace("</resolutions>",&format!("{extra}</resolutions>"));}
    crate::private_file::write_atomic(&directory.join("vdd_settings.xml"),xml.as_bytes()).map_err(|e|Error::Platform(e.to_string()))?;
    crate::private_file::write_atomic(&directory.join("user_edid.bin"),&super::edid::monitor(id,name)).map_err(|e|Error::Platform(e.to_string()))?;
    Ok(directory)
}
pub(super) fn switch_demand_configuration(expected:&Path,next:&Path)->Result<()> {
    if current_configuration()?.as_deref()!=Some(expected) {return Err(Error::Platform("virtual display configuration ownership changed".into()));}
    let mut key=null_mut();
    let code=unsafe{RegOpenKeyExW(HKEY_LOCAL_MACHINE,wide(CONFIG_KEY).as_ptr(),0,KEY_QUERY_VALUE|KEY_SET_VALUE|KEY_WOW64_64KEY,&mut key)};
    if code!=0 {return Err(os_error("open owned display configuration",code));}
    let bytes=path_wide(next);
    let code=unsafe{RegSetValueExW(key,wide("VDDPATH").as_ptr(),0,REG_SZ,bytes.as_ptr().cast(),(bytes.len()*2) as u32)};
    unsafe{RegCloseKey(key);}
    if code!=0 {return Err(os_error("select owned display configuration",code));}
    Ok(())
}
pub(super) fn remove_monitor_configuration(base:&Path,token:Uuid)->Result<()> {
    let directory=base.join(format!("monitor-{token}"));
    if current_configuration()?.as_deref()==Some(directory.as_path()) {switch_demand_configuration(&directory,base)?;}
    if !directory.try_exists().map_err(|e|Error::Platform(e.to_string()))? {return Ok(());}
    let attrs=unsafe{GetFileAttributesW(path_wide(&directory).as_ptr())};
    if attrs==INVALID_FILE_ATTRIBUTES||attrs&FILE_ATTRIBUTE_REPARSE_POINT!=0 {return Err(Error::Platform("owned monitor configuration was redirected".into()));}
    for file in ["vdd_settings.xml","user_edid.bin"] {
        match std::fs::remove_file(directory.join(file)) {Ok(())=>{},Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{},Err(e)=>return Err(Error::Platform(e.to_string()))}
    }
    #[cfg(test)] {
        let logs=directory.join("Logs");
        if logs.exists() {
            for item in std::fs::read_dir(&logs).map_err(|e|Error::Platform(e.to_string()))? {
                let item=item.map_err(|e|Error::Platform(e.to_string()))?;
                let name=item.file_name();let name=name.to_string_lossy();
                if name.starts_with("log_")&&name.ends_with(".txt") {std::fs::remove_file(item.path()).map_err(|e|Error::Platform(e.to_string()))?;}
            }
            std::fs::remove_dir(logs).map_err(|e|Error::Platform(e.to_string()))?;
        }
    }
    std::fs::remove_dir(directory).map_err(|e|Error::Platform(e.to_string()))
}
pub(super) fn rename_demand_monitor(key:&TargetKey,token:&str,name:&str)->Result<()> {
    // Prove adapter ownership before opening a child monitor's registry data.
    let matched=demand_instances()?.into_iter().find(|instance|super::windows::targets_for_instance(instance).is_ok_and(|targets|targets.iter().any(|t|t.key==*key))).ok_or(Error::UnknownSlot)?;
    verify_demand_device(&matched,token,true)?;
    let path=key.monitor_path.strip_prefix(r"\\?\").ok_or(Error::UnknownSlot)?;
    let pieces:Vec<_>=path.split('#').collect();
    if pieces.len()!=4||!pieces[0].eq_ignore_ascii_case("display") {return Err(Error::UnknownSlot);}
    let instance=format!("{}\\{}\\{}",pieces[0],pieces[1],pieces[2]);
    let handle=unsafe{SetupDiCreateDeviceInfoList(null(),null_mut())};
    if handle==INVALID_HANDLE_VALUE as isize {return Err(os_error("open monitor device set",unsafe{GetLastError()}));}
    let devices=DevInfoSet(handle);
    let mut info:SP_DEVINFO_DATA=unsafe{zeroed()};info.cbSize=size_of::<SP_DEVINFO_DATA>() as u32;
    win_ok(unsafe{SetupDiOpenDeviceInfoW(devices.0,wide(&instance).as_ptr(),null_mut(),0,&mut info)},"open exact owned monitor")?;
    let bytes=wide(name);
    win_ok(unsafe{SetupDiSetDeviceRegistryPropertyW(devices.0,&mut info,SPDRP_FRIENDLYNAME,bytes.as_ptr().cast(),(bytes.len()*2) as u32)},"set owned monitor name")?;
    if property(devices.0,&info,SPDRP_FRIENDLYNAME)?!=name {return Err(Error::Platform("Windows monitor name did not match".into()));}
    Ok(())
}

#[cfg(test)]
pub(super) fn monitor_diagnostics(base:&Path,token:Uuid)->String {
    let path=base.join(format!("monitor-{token}")).join("Logs");
    std::fs::read_dir(path).into_iter().flatten().filter_map(|entry|entry.ok()).filter_map(|entry|std::fs::read_to_string(entry.path()).ok()).map(|text|text.lines().rev().take(100).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")).collect::<Vec<_>>().join("\n")
}
