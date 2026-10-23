//! SCM owns a System child in the configured user's console session. The
//! launcher retains privileges; the media child starts with privileges disabled.
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
        AdjustTokenPrivileges(token.0, 0, &privileges, 0, ptr::null_mut(), ptr::null_mut())
    })?;
    ensure!(
        unsafe { GetLastError() } != ERROR_NOT_ALL_ASSIGNED,
        "后台服务缺少必要的会话启动权限"
    );
    Ok(())
}
fn owner_session(installed: &Installed) -> Option<u32> {
    let session = unsafe { WTSGetActiveConsoleSessionId() };
    if session == u32::MAX || session == 0 {
        return None;
    }
    let mut token = ptr::null_mut();
    if unsafe { WTSQueryUserToken(session, &mut token) } == 0 {
        return None;
    }
    let token = Handle(token);
    (token_user(token.0).ok().as_deref() == Some(installed.owner_sid.as_str())).then_some(session)
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
fn environment(paths: &Paths) -> Result<Vec<u16>> {
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
    let mut pairs = [
        ("SystemRoot", windows.clone()),
        ("windir", windows.clone()),
        ("TEMP", temp.clone()),
        ("TMP", temp),
        ("USERPROFILE", windows.join("System32/config/systemprofile")),
        ("ProgramData", paths.root.parent().unwrap().to_path_buf()),
        (
            "ProgramFiles",
            paths.programs.parent().unwrap().to_path_buf(),
        ),
        ("PATH", windows.join("System32")),
    ];
    pairs.sort_by_key(|(key, _)| key.to_ascii_lowercase());
    let mut result = Vec::new();
    for (key, value) in pairs {
        result.extend(wide(format!("{key}={}", value.display())));
    }
    result.push(0);
    Ok(result)
}
fn launch(paths: &Paths, installed: &Installed, session: u32) -> Result<Child> {
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
    winok(unsafe {
        AdjustTokenPrivileges(token.0, 1, ptr::null(), 0, ptr::null_mut(), ptr::null_mut())
    })?;
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
    let executable = directory.join("uu.exe");
    let mut command = wide(format!(
        "\"{}\" service session-host --session {session} --stop-event {event_name}",
        executable.display()
    ));
    let mut desktop = wide("winsta0\\default");
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of_val(&startup) as u32;
    startup.lpDesktop = desktop.as_mut_ptr();
    let environment = environment(paths)?;
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
                .join("uu.exe")
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
    while !STOP.load(Ordering::Acquire) {
        let session = owner_session(&installed);
        if child.as_ref().is_some_and(|child| {
            Some(child.session) != session
                || unsafe { WaitForSingleObject(child.process.0, 0) } == WAIT_OBJECT_0
        }) {
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
