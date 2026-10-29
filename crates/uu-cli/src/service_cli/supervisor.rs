//! SCM owns a System child in the configured user's console session. The
//! launcher retains privileges; the media child keeps normal path traversal only.
use super::*;
use anyhow::{ensure, Context};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use windows_sys::Win32::{
    Foundation::*,
    Security::*,
    System::{
        JobObjects::*, RemoteDesktop::*, SystemInformation::GetSystemWindowsDirectoryW,
        Threading::*,
    },
};
static STOP: AtomicBool = AtomicBool::new(false);
static STATUS: AtomicIsize = AtomicIsize::new(0);
#[cfg(test)]
pub(super) mod system_probe;

fn report(state: u32, code: u32) {
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        },
        dwWin32ExitCode: code,
        dwServiceSpecificExitCode: if code == ERROR_SERVICE_SPECIFIC_ERROR {
            1
        } else {
            0
        },
        dwCheckPoint: if state == SERVICE_START_PENDING || state == SERVICE_STOP_PENDING {
            1
        } else {
            0
        },
        dwWaitHint: 10000,
    };
    let handle = STATUS.load(Ordering::Acquire) as SERVICE_STATUS_HANDLE;
    if !handle.is_null() {
        unsafe {
            SetServiceStatus(handle, &status);
        }
    }
}
unsafe extern "system" fn control(
    control: u32,
    _event: u32,
    _data: *mut core::ffi::c_void,
    _context: *mut core::ffi::c_void,
) -> u32 {
    if control == SERVICE_CONTROL_STOP || control == SERVICE_CONTROL_SHUTDOWN {
        STOP.store(true, Ordering::Release);
        report(SERVICE_STOP_PENDING, 0);
    }
    0
}
unsafe extern "system" fn main(_argc: u32, _argv: *mut *mut u16) {
    let handle =
        RegisterServiceCtrlHandlerExW(wide(SERVICE_NAME).as_ptr(), Some(control), ptr::null());
    if handle.is_null() {
        return;
    }
    STATUS.store(handle as isize, Ordering::Release);
    report(SERVICE_START_PENDING, 0);
    let result = run();
    if let Err(error) = &result {
        tracing::error!(error=%error,"background service supervisor stopped");
    }
    report(
        SERVICE_STOPPED,
        if result.is_ok() {
            0
        } else {
            ERROR_SERVICE_SPECIFIC_ERROR
        },
    );
}
pub fn dispatch() -> Result<()> {
    let name = wide(SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_ptr().cast_mut(),
            lpServiceProc: Some(main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: ptr::null_mut(),
            lpServiceProc: None,
        },
    ];
    winok(unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) })
        .context("请通过 Windows 服务管理器启动后台服务")
}
fn enable(name: &str) -> Result<()> {
    let mut token = ptr::null_mut();
    winok(unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    })?;
    let token = Handle(token);
    enable_token(token.0, name)
}
fn enable_token(token: HANDLE, name: &str) -> Result<()> {
    let mut luid: LUID = unsafe { std::mem::zeroed() };
    winok(unsafe { LookupPrivilegeValueW(ptr::null(), wide(name).as_ptr(), &mut luid) })?;
    let privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    winok(unsafe {
        AdjustTokenPrivileges(token, 0, &privileges, 0, ptr::null_mut(), ptr::null_mut())
    })?;
    ensure!(
        unsafe { GetLastError() } != ERROR_NOT_ALL_ASSIGNED,
        "后台服务缺少必要的会话启动权限"
    );
    Ok(())
}
fn media_privileges(token: HANDLE) -> Result<()> {
    winok(unsafe {
        AdjustTokenPrivileges(token, 1, ptr::null(), 0, ptr::null_mut(), ptr::null_mut())
    })?;
    // This is enabled for ordinary Windows users too. Disabling it breaks
    // canonical path resolution on otherwise accessible installed executables.
    // Process/session-launch privileges remain disabled in the media child.
    enable_token(token, "SeChangeNotifyPrivilege")
}
fn owner_session(installed: &Installed) -> Result<u32> {
    let session = unsafe { WTSGetActiveConsoleSessionId() };
    if session == u32::MAX || session == 0 {
        anyhow::bail!("等待活动控制台会话");
    }
    let mut token = ptr::null_mut();
    winok(unsafe { WTSQueryUserToken(session, &mut token) }).context("查询控制台用户身份失败")?;
    let token = Handle(token);
    ensure!(
        token_user(token.0)? == installed.owner_sid,
        "等待服务所有者登录控制台"
    );
    Ok(session)
}
struct Child {
    session: u32,
    process: Handle,
    stop: Handle,
    _job: Handle,
}
impl Drop for Child {
    fn drop(&mut self) {
        unsafe {
            SetEvent(self.stop.0);
            WaitForSingleObject(self.process.0, 5000);
        } /* job closes after graceful deadline */
    }
}
fn system_environment(token: HANDLE) -> Result<Vec<(Vec<u16>, Vec<u16>)>> {
    use windows_sys::Win32::System::Environment::{
        CreateEnvironmentBlock, DestroyEnvironmentBlock,
    };
    let mut block = ptr::null_mut();
    winok(unsafe { CreateEnvironmentBlock(&mut block, token, 0) })
        .context("创建 System 用户的完整环境失败")?;
    struct Block(*mut core::ffi::c_void);
    impl Drop for Block {
        fn drop(&mut self) {
            unsafe {
                DestroyEnvironmentBlock(self.0);
            }
        }
    }
    ensure!(!block.is_null(), "Windows 未返回环境块");
    let block = Block(block);
    // The API owns a double-NUL-terminated UTF-16 block. Preserve its variables
    // and non-ASCII values; do not reconstruct required profile/drive aliases.
    let mut entries: Vec<(Vec<u16>, Vec<u16>)> = Vec::new();
    let mut offset = 0usize;
    unsafe {
        let data = block.0.cast::<u16>();
        while *data.add(offset) != 0 {
            let start = offset;
            while *data.add(offset) != 0 {
                offset += 1;
                ensure!(offset < 1024 * 1024, "Windows 环境块过大");
            }
            let line = std::slice::from_raw_parts(data.add(start), offset - start);
            offset += 1;
            let separator = line
                .iter()
                .enumerate()
                .skip(1)
                .find(|(_, value)| **value == b'=' as u16)
                .map(|(at, _)| at)
                .context("Windows 环境项无效")?;
            let key = String::from_utf16_lossy(&line[..separator]).to_ascii_uppercase();
            if !key.starts_with("UU_") && !key.starts_with("XDG_") {
                entries.push((line[..separator].to_vec(), line[separator + 1..].to_vec()));
            }
        }
    }
    Ok(entries)
}
fn environment(paths: &Paths, token: HANDLE) -> Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Globalization::CompareStringOrdinal;
    let mut entries = system_environment(token)?;
    let mut buffer = vec![0u16; 32768];
    let size = unsafe { GetSystemWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
    ensure!(
        size > 0 && (size as usize) < buffer.len(),
        "无法确定 Windows 系统目录"
    );
    use std::os::windows::ffi::OsStringExt;
    let windows = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..size as usize]));
    let temp = paths.root.join("temp");
    super::install::directory(&temp)?;
    for (key, value) in [
        ("PATH", windows.join("System32")),
        ("TEMP", temp.clone()),
        ("TMP", temp),
        ("XDG_DATA_HOME", paths.root.join("data")),
        ("XDG_STATE_HOME", paths.root.join("state")),
        (
            "ECLIPSE_SERVICE_BOOTSTRAP_LOG",
            paths.config().log_dir().join("bootstrap.log"),
        ),
    ] {
        entries.retain(|(candidate, _)| {
            !String::from_utf16_lossy(candidate).eq_ignore_ascii_case(key)
        });
        entries.push((
            key.encode_utf16().collect(),
            value.as_os_str().encode_wide().collect(),
        ));
    }
    entries.sort_by(|(left, _), (right, _)| {
        match unsafe {
            CompareStringOrdinal(
                left.as_ptr(),
                left.len() as i32,
                right.as_ptr(),
                right.len() as i32,
                1,
            )
        } {
            1 => std::cmp::Ordering::Less,
            3 => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        }
    });
    let mut result = Vec::new();
    for (key, value) in entries {
        result.extend(key);
        result.push(b'=' as u16);
        result.extend(value);
        result.push(0);
    }
    result.push(0);
    Ok(result)
}

fn launch(paths: &Paths, installed: &Installed, session: u32) -> Result<Child> {
    launch_entry(paths, installed, session, Entry::SessionHost)
}
enum Entry {
    SessionHost,
    #[cfg(test)]
    Generated(std::path::PathBuf),
}
fn launch_entry(paths: &Paths, installed: &Installed, session: u32, entry: Entry) -> Result<Child> {
    let mut token = ptr::null_mut();
    winok(unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY,
            &mut token,
        )
    })?;
    let original = Handle(token);
    let mut token = ptr::null_mut();
    winok(unsafe {
        DuplicateTokenEx(
            original.0,
            TOKEN_ALL_ACCESS,
            ptr::null(),
            SecurityImpersonation,
            TokenPrimary,
            &mut token,
        )
    })?;
    let token = Handle(token);
    winok(unsafe {
        SetTokenInformation(token.0, TokenSessionId, (&session as *const u32).cast(), 4)
    })?;
    media_privileges(token.0)?;
    let descriptor = Descriptor::administrators()?;
    let attributes = descriptor.attributes();
    let event_name = format!("Global\\EclipseRemoteHost.stop.{}", uuid::Uuid::new_v4());
    let stop = Handle(unsafe { CreateEventW(&attributes, 1, 0, wide(&event_name).as_ptr()) });
    ensure!(!stop.0.is_null(), "无法创建后台停止事件");
    let job = Handle(unsafe { CreateJobObjectW(ptr::null(), ptr::null()) });
    ensure!(!job.0.is_null(), "无法创建后台进程容器");
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    winok(unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    })?;
    let directory = paths.binary_directory(installed);
    let executable = directory.join(installed.cli_name());
    let arguments = match entry {
        Entry::SessionHost => format!("service session-host --session {session} --stop-event {event_name}"),
        #[cfg(test)]
        Entry::Generated(invitation) => format!("native-host --synthetic --encoder hardware --fps 30 --max-seconds 15 --invitation-file \"{}\"",invitation.display()),
    };
    let mut command = wide(format!("\"{}\" {arguments}", executable.display()));
    let mut desktop = wide("winsta0\\default");
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of_val(&startup) as u32;
    startup.lpDesktop = desktop.as_mut_ptr();
    let environment = environment(paths, token.0)?;
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    winok(unsafe {
        CreateProcessAsUserW(
            token.0,
            wide(executable.as_os_str()).as_ptr(),
            command.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW | CREATE_SUSPENDED,
            environment.as_ptr().cast(),
            wide(directory.as_os_str()).as_ptr(),
            &startup,
            &mut process,
        )
    })?;
    let process_handle = Handle(process.hProcess);
    let thread = Handle(process.hThread);
    if unsafe { AssignProcessToJobObject(job.0, process_handle.0) } == 0 {
        unsafe {
            TerminateProcess(process_handle.0, 1);
        }
        return Err(std::io::Error::last_os_error()).context("无法接管后台子进程");
    }
    if unsafe { ResumeThread(thread.0) } == u32::MAX {
        return Err(std::io::Error::last_os_error()).context("无法启动后台媒体进程");
    }
    Ok(Child {
        session,
        process: process_handle,
        stop,
        _job: job,
    })
}
fn run() -> Result<()> {
    ensure!(
        process_user(unsafe { GetCurrentProcess() })? == "S-1-5-18",
        "后台服务必须由 LocalSystem 运行"
    );
    let paths = Paths::discover()?;
    let installed = paths.manifest()?;
    ensure!(
        std::env::current_exe()?.canonicalize()?
            == paths
                .binary_directory(&installed)
                .join(installed.cli_name())
                .canonicalize()?,
        "后台服务必须从受保护的安装位置运行"
    );
    for name in [
        "SeTcbPrivilege",
        "SeAssignPrimaryTokenPrivilege",
        "SeIncreaseQuotaPrivilege",
    ] {
        enable(name)?;
    }
    report(SERVICE_RUNNING, 0);
    let mut child: Option<Child> = None;
    let mut retry_at = Instant::now();
    let mut waiting = None;
    while !STOP.load(Ordering::Acquire) {
        let session = match owner_session(&installed) {
            Ok(session) => {
                waiting = None;
                Some(session)
            }
            Err(error) => {
                let reason = format!("{error:#}");
                if waiting.as_ref() != Some(&reason) {
                    tracing::info!(%reason,"background console host waiting");
                    waiting = Some(reason);
                }
                None
            }
        };
        if child.as_ref().is_some_and(|child| {
            Some(child.session) != session
                || unsafe { WaitForSingleObject(child.process.0, 0) } == WAIT_OBJECT_0
        }) {
            if let Some(child) = &child {
                let mut code = 0;
                if unsafe { GetExitCodeProcess(child.process.0, &mut code) } != 0
                    && code != STILL_ACTIVE as u32
                {
                    tracing::info!(code, "background media process exited");
                }
            }
            child.take();
            retry_at = Instant::now() + Duration::from_secs(2);
        }
        if child.is_none() && Instant::now() >= retry_at {
            if let Some(session) = session {
                match launch(&paths, &installed, session) {
                    Ok(next) => {
                        tracing::info!(session, "background console host started");
                        child = Some(next);
                    }
                    Err(error) => {
                        tracing::error!(error=%error,"background console host launch failed")
                    }
                }
                retry_at = Instant::now() + Duration::from_secs(3);
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    drop(child);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_token_keeps_only_normal_windows_traversal_enabled() {
        let mut original = ptr::null_mut();
        winok(unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_QUERY,
                &mut original,
            )
        })
        .unwrap();
        let original = Handle(original);
        let mut token = ptr::null_mut();
        winok(unsafe {
            DuplicateTokenEx(
                original.0,
                TOKEN_QUERY | TOKEN_ADJUST_PRIVILEGES,
                ptr::null(),
                SecurityImpersonation,
                TokenPrimary,
                &mut token,
            )
        })
        .unwrap();
        let token = Handle(token);
        media_privileges(token.0).unwrap();
        let mut length = 0;
        unsafe {
            GetTokenInformation(token.0, TokenPrivileges, ptr::null_mut(), 0, &mut length);
        }
        assert!(length > 0 && length < 65536);
        let mut storage = vec![
            0usize;
            (length as usize + std::mem::size_of::<usize>() - 1)
                / std::mem::size_of::<usize>()
        ];
        winok(unsafe {
            GetTokenInformation(
                token.0,
                TokenPrivileges,
                storage.as_mut_ptr().cast(),
                length,
                &mut length,
            )
        })
        .unwrap();
        let privileges = unsafe { &*storage.as_ptr().cast::<TOKEN_PRIVILEGES>() };
        let values = unsafe {
            std::slice::from_raw_parts(
                privileges.Privileges.as_ptr(),
                privileges.PrivilegeCount as usize,
            )
        };
        let mut expected: LUID = unsafe { std::mem::zeroed() };
        winok(unsafe {
            LookupPrivilegeValueW(
                ptr::null(),
                wide("SeChangeNotifyPrivilege").as_ptr(),
                &mut expected,
            )
        })
        .unwrap();
        let enabled: Vec<_> = values
            .iter()
            .filter(|v| v.Attributes & SE_PRIVILEGE_ENABLED != 0)
            .collect();
        assert_eq!(enabled.len(), 1);
        assert_eq!(
            (enabled[0].Luid.LowPart, enabled[0].Luid.HighPart),
            (expected.LowPart, expected.HighPart)
        );
    }
    #[test]
    fn native_environment_preserves_windows_directory_expansion_variables() {
        let environment = system_environment(ptr::null_mut()).unwrap();
        for key in [
            "SystemDrive",
            "SystemRoot",
            "ProgramData",
            "ALLUSERSPROFILE",
        ] {
            assert!(
                environment
                    .iter()
                    .any(
                        |(name, value)| String::from_utf16_lossy(name).eq_ignore_ascii_case(key)
                            && !value.is_empty()
                    ),
                "missing required Windows directory expansion input: {key}"
            );
        }
    }
}
