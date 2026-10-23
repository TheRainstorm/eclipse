//! Optional, explicitly installed Windows host service. The SCM parent owns
//! only its console-session child; closing the manager never stops sharing.
mod host;
mod install;
mod supervisor;
use anyhow::{Context, Result};
use std::{
    path::PathBuf,
    ptr,
    time::{Duration, Instant},
};
use uu_core::{service_admin::windows::*, Config};
use windows_sys::Win32::System::Services::*;

pub fn init_background_logging() -> Result<()> {
    anyhow::ensure!(
        process_user(unsafe { windows_sys::Win32::System::Threading::GetCurrentProcess() })?
            == "S-1-5-18",
        "后台进程必须由 LocalSystem 服务启动"
    );
    let paths = Paths::discover()?;
    let installed = paths.manifest()?;
    anyhow::ensure!(
        std::env::current_exe()?.canonicalize()?
            == paths
                .binary_directory(&installed)
                .join("uu.exe")
                .canonicalize()?,
        "后台进程必须从安装位置启动"
    );
    let directory = paths.config().log_dir();
    std::fs::create_dir_all(&directory)?;
    let path = directory.join("service.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 4 * 1024 * 1024) {
        let _ = std::fs::rename(&path, directory.join("service.previous.log"));
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    tracing_subscriber::fmt()
        .with_env_filter(
            "error,uu_core=info,uu_cli=info,uu_capture=warn,uu_input=warn,uu_media=warn",
        )
        .with_target(false)
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(log))
        .try_init()
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(())
}

#[derive(clap::Subcommand, Debug)]
pub enum Command {
    /// Copy the verified package and migrate this user's host settings once.
    Install {
        #[arg(long)]
        source_data: Option<PathBuf>,
        #[arg(long)]
        open_manager: bool,
    },
    /// Apply this user's current UU login state to the protected host profile.
    ImportAccount {
        #[arg(long)]
        source_data: Option<PathBuf>,
    },
    Start,
    Stop,
    Restart,
    Status,
    /// Remove the service registration; protected pairing/configuration is kept.
    Remove,
    /// Open the local administrator GUI for the installed host.
    Manage,
    #[command(hide = true)]
    Daemon,
    #[command(hide = true)]
    SessionHost {
        #[arg(long)]
        session: u32,
        #[arg(long)]
        stop_event: String,
    },
}
pub async fn run(command: Command, config: Config) -> Result<()> {
    match command {
        Command::Install {
            source_data,
            open_manager,
        } => {
            install::install(source_data.unwrap_or(config.data_dir))?;
            if open_manager {
                manage()?;
            }
            Ok(())
        }
        Command::Start => start(),
        Command::ImportAccount { source_data } => {
            install::import_account(source_data.unwrap_or(config.data_dir))
        }
        Command::Stop => stop(),
        Command::Restart => {
            stop()?;
            start()
        }
        Command::Status => {
            println!(
                "{}",
                match status() {
                    Ok(SERVICE_RUNNING) => "后台服务运行中",
                    Ok(SERVICE_STOPPED) => "后台服务已停止",
                    Ok(_) => "后台服务正在转换状态",
                    Err(_) => "后台服务未安装或不可访问",
                }
            );
            Ok(())
        }
        Command::Remove => {
            require_administrator()?;
            stop()?;
            let service = open_service(windows_sys::Win32::Storage::FileSystem::DELETE)?;
            winok(unsafe { DeleteService(service.0) })
        }
        Command::Manage => manage(),
        Command::Daemon => supervisor::dispatch(),
        Command::SessionHost {
            session,
            stop_event,
        } => host::run(session, &stop_event).await,
    }
}
pub fn start() -> Result<()> {
    require_administrator()?;
    let service = open_service(SERVICE_START | SERVICE_QUERY_STATUS)?;
    let result = unsafe { StartServiceW(service.0, 0, ptr::null()) };
    if result == 0
        && std::io::Error::last_os_error().raw_os_error()
            != Some(windows_sys::Win32::Foundation::ERROR_SERVICE_ALREADY_RUNNING as i32)
    {
        winok(result)?;
    }
    wait(SERVICE_RUNNING)
}
pub fn stop() -> Result<()> {
    require_administrator()?;
    if matches!(status(), Ok(SERVICE_STOPPED)) {
        return Ok(());
    }
    let service = open_service(SERVICE_STOP | SERVICE_QUERY_STATUS)?;
    let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
    let result = unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) };
    if result == 0
        && std::io::Error::last_os_error().raw_os_error()
            != Some(windows_sys::Win32::Foundation::ERROR_SERVICE_NOT_ACTIVE as i32)
    {
        winok(result)?;
    }
    wait(SERVICE_STOPPED)
}
fn wait(expected: u32) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        if status()? == expected {
            return Ok(());
        }
        anyhow::ensure!(Instant::now() < deadline, "等待后台服务状态转换超时");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn manage() -> Result<()> {
    require_administrator()?;
    let paths = Paths::discover()?;
    let installed = paths.manifest()?;
    std::process::Command::new(paths.binary_directory(&installed).join("uu-gui.exe"))
        .arg("--manage-background-service")
        .spawn()
        .context("无法打开后台服务管理窗口")?;
    Ok(())
}
