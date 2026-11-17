//! Restart-only profile relocation. The helper never resolves the live Config.
use anyhow::{anyhow, bail, Context, Result};
use std::{
    io::{BufRead, Read, Write},
    process::{Child, Command, Stdio},
    sync::{mpsc, Mutex, OnceLock},
    time::Duration,
};
use uu_core::{config::ProfileRelocation, Config};

const ARGUMENT: &str = "--relocate-profile";
const ERROR_ENV: &str = "ECLIPSE_PROFILE_RELOCATION_ERROR";
const REQUEST_LIMIT: u64 = 64 * 1024;
static RESTART_ERROR: OnceLock<Mutex<Option<String>>> = OnceLock::new();

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    relocation: ProfileRelocation,
    parent_pid: u32,
}

/// Until the UI receives READY, dropping this guard cancels only its own helper.
pub(super) struct Prepared {
    child: Option<Child>,
}
impl Prepared {
    pub fn release(mut self) {
        self.child.take();
    }
}
impl Drop for Prepared {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub(super) fn prepare(source: &Config, destination: &std::path::Path) -> Result<Prepared> {
    let relocation = ProfileRelocation::prepare(source, destination)?;
    let payload = serde_json::to_vec(&Request {
        relocation,
        parent_pid: std::process::id(),
    })?;
    if payload.len() as u64 > REQUEST_LIMIT {
        bail!("配置迁移请求过大");
    }
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg(ARGUMENT)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut prepared = Prepared {
        child: Some(command.spawn().context("无法启动配置迁移助手")?),
    };
    let child = prepared.child.as_mut().unwrap();
    let mut input = child.stdin.take().context("配置迁移助手缺少输入管道")?;
    input.write_all(&payload)?;
    drop(input);
    let output = child.stdout.take().context("配置迁移助手缺少确认管道")?;
    let (tx, rx) = mpsc::sync_channel(1);
    let reader = std::thread::Builder::new()
        .name("eclipse-profile-ready".into())
        .spawn(move || {
            let _ = tx.send(read_ready(output));
        })?;
    let ready = rx.recv_timeout(Duration::from_secs(20));
    if ready.is_err() {
        drop(prepared);
        let _ = reader.join();
        bail!("配置迁移助手未及时就绪，当前程序和目录未切换");
    }
    let _ = reader.join();
    ready.unwrap()?;
    Ok(prepared)
}

fn read_ready(reader: impl Read) -> Result<()> {
    let mut line = String::new();
    std::io::BufReader::new(reader.take(4097)).read_line(&mut line)?;
    match line.trim_end() {
        "READY" => Ok(()),
        value if value.starts_with("ERROR:") => bail!("{}", value.trim_start_matches("ERROR:")),
        _ => bail!("配置迁移助手未确认就绪，当前程序继续运行"),
    }
}

fn read_request(reader: impl Read) -> Result<Request> {
    let mut bytes = Vec::new();
    reader.take(REQUEST_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > REQUEST_LIMIT {
        bail!("配置迁移请求过大");
    }
    let request: Request = serde_json::from_slice(&bytes).context("配置迁移请求无效")?;
    if request.parent_pid == 0 || request.parent_pid == std::process::id() {
        bail!("配置迁移请求的原进程无效");
    }
    Ok(request)
}

/// Called before settings, services or any Config::from_env invocation.
pub(super) fn dispatch() -> bool {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(ARGUMENT)) {
        return false;
    }
    let result = if args.next().is_some() {
        Err(anyhow!("配置迁移助手参数无效"))
    } else {
        helper()
    };
    if let Err(error) = result {
        // Pre-READY errors go back to the still-running launcher.
        let _ = writeln!(std::io::stdout(), "ERROR:{error:#}");
    }
    true
}

fn helper() -> Result<()> {
    let request = read_request(std::io::stdin().lock())?;
    let executable = std::env::current_exe()?;
    let parent = Parent::open(request.parent_pid)?;
    writeln!(std::io::stdout(), "READY")?;
    std::io::stdout().flush()?;
    if let Err(error) = parent.wait(Duration::from_secs(120)) {
        show_failure(&format!(
            "配置目录没有迁移：{error:#}。请关闭原程序后重试。"
        ));
        return Ok(());
    }
    let mut command = Command::new(&executable);
    match request.relocation.execute(&executable) {
        Ok(_) => {
            command
                .env_remove("ECLIPSE_PROFILE_DIR")
                .env_remove("ECLIPSE_CONFIG_SOURCE")
                .env_remove(ERROR_ENV);
        }
        Err(error) => {
            request.relocation.source.apply_to_command(&mut command);
            command.env(
                ERROR_ENV,
                format!("配置迁移未完成，已重新打开原配置：{error:#}"),
            );
        }
    }
    if let Err(error) = command.spawn() {
        show_failure(&format!(
            "配置迁移助手无法重新打开 Eclipse：{error}。请手动重新打开程序。"
        ));
    }
    Ok(())
}

fn show_failure(message: &str) {
    eprintln!("{message}");
    let _ = rfd::MessageDialog::new()
        .set_title("Eclipse · 配置迁移")
        .set_description(message)
        .set_level(rfd::MessageLevel::Error)
        .show();
}

/// Consume the restart-only notice before threads or stream children are started.
pub(super) fn capture_restart_error() {
    let error = std::env::var(ERROR_ENV)
        .ok()
        .map(|s| s.chars().take(8192).collect());
    std::env::remove_var(ERROR_ENV);
    let _ = RESTART_ERROR.set(Mutex::new(error));
}
pub(super) fn take_restart_error() -> Option<String> {
    RESTART_ERROR
        .get()
        .and_then(|value| value.lock().ok()?.take())
}

#[cfg(windows)]
struct Parent(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl Parent {
    fn open(pid: u32) -> Result<Self> {
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self(handle))
    }
    fn wait(&self, timeout: Duration) -> Result<()> {
        use windows_sys::Win32::{
            Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
            System::Threading::WaitForSingleObject,
        };
        match unsafe {
            WaitForSingleObject(self.0, timeout.as_millis().min(u32::MAX as u128) as u32)
        } {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_TIMEOUT => bail!("等待原程序退出超时"),
            _ => Err(std::io::Error::last_os_error().into()),
        }
    }
}
#[cfg(windows)]
impl Drop for Parent {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(target_os = "linux")]
struct Parent(std::fs::File);
#[cfg(target_os = "linux")]
impl Parent {
    fn open(pid: u32) -> Result<Self> {
        use std::os::fd::FromRawFd;
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self(unsafe { std::fs::File::from_raw_fd(fd as i32) }))
    }
    fn wait(&self, timeout: Duration) -> Result<()> {
        use std::os::fd::AsRawFd;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let mut descriptor = libc::pollfd {
                fd: self.0.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let result = unsafe {
                libc::poll(
                    &mut descriptor,
                    1,
                    left.as_millis().min(i32::MAX as u128) as i32,
                )
            };
            if result > 0 && descriptor.revents & libc::POLLIN != 0 {
                return Ok(());
            }
            if result == 0 {
                bail!("等待原程序退出超时");
            }
            let error = std::io::Error::last_os_error();
            if result < 0 && error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            bail!("无法等待原程序退出：{error}");
        }
    }
}
#[cfg(not(any(windows, target_os = "linux")))]
struct Parent;
#[cfg(not(any(windows, target_os = "linux")))]
impl Parent {
    fn open(_: u32) -> Result<Self> {
        bail!("当前平台尚不支持配置目录迁移");
    }
    fn wait(&self, _: Duration) -> Result<()> {
        unreachable!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readiness_requires_explicit_confirmation_and_preserves_helper_errors() {
        assert!(read_ready(&b"READY\n"[..]).is_ok());
        assert!(read_ready(&b""[..]).is_err());
        assert!(read_ready(&b"diagnostic output\nREADY\n"[..]).is_err());
        assert!(read_ready(&b"ERROR:other stream windows remain open\n"[..])
            .unwrap_err()
            .to_string()
            .contains("other stream windows"));
    }
    #[test]
    fn helper_rejects_oversized_or_unbound_requests_before_running_any_migration() {
        assert!(read_request(&vec![b' '; REQUEST_LIMIT as usize + 1][..]).is_err());
        assert!(read_request(&b"{}"[..]).is_err());
        let config = Config {
            data_dir: "old/data".into(),
            state_dir: "old/state".into(),
            signaling_host: String::new(),
        };
        let bytes = serde_json::to_vec(&Request {
            relocation: ProfileRelocation {
                source: config,
                destination: "new".into(),
            },
            parent_pid: std::process::id(),
        })
        .unwrap();
        assert!(read_request(bytes.as_slice()).is_err());
    }
    #[test]
    fn a_live_parent_handle_does_not_report_process_exit() {
        let parent = Parent::open(std::process::id()).unwrap();
        assert!(parent.wait(Duration::ZERO).is_err());
    }
}
