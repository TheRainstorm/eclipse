//! Optional, explicitly installed Windows host service. The SCM parent owns
//! only its console-session child; closing the manager never stops sharing.
mod host;
mod install;
mod supervisor;
mod profile_mirror;
use anyhow::{Context, Result};
use std::{
    path::PathBuf,
    ptr,
    time::{Duration, Instant},
};
use uu_core::{service_admin::windows::*, Config};
use windows_sys::Win32::System::Services::*;

/// The supervisor provisions this exact protected path. Use it when a shell or
/// profile API fails before the regular logger exists; no cross-session handle
/// inheritance or unprivileged caller can provide a privileged log destination.
pub fn record_bootstrap_error(error: &anyhow::Error) {
    use std::io::Write;
    if process_user(unsafe { windows_sys::Win32::System::Threading::GetCurrentProcess() })
        .ok()
        .as_deref()
        != Some("S-1-5-18")
    {
        return;
    }
    let Some(root) = std::env::var_os("ProgramData").map(PathBuf::from) else {
        return;
    };
    let expected = root.join("EclipseRemoteHost/state/uu-remote/log/bootstrap.log");
    let Some(path) = std::env::var_os("ECLIPSE_SERVICE_BOOTSTRAP_LOG").map(PathBuf::from) else {
        return;
    };
    let profiles = root.join("EclipseRemoteHost/profiles");
    let scoped = path.strip_prefix(&profiles).ok().and_then(|p| {
        let id=p.components().next()?.as_os_str().to_str()?;
        (uu_core::service_profile::valid_id(id) && p == std::path::Path::new(id).join("state/eclipse/log/bootstrap.log")).then_some(())
    }).is_some();
    if !path.is_absolute() || (path != expected && !scoped) {
        return;
    }
    let truncate = std::fs::metadata(&path).is_ok_and(|m| m.len() > 1024 * 1024);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(!truncate)
        .truncate(truncate)
        .open(path)
    {
        let _ = writeln!(file, "background startup failed: {error:#}");
    }
}

pub fn init_background_logging() -> Result<()> {
    anyhow::ensure!(
        process_user(unsafe { windows_sys::Win32::System::Threading::GetCurrentProcess() })?
            == "S-1-5-18",
        "后台进程必须由 LocalSystem 服务启动"
    );
    let paths = Paths::discover().context("解析后台安装目录失败")?;
    let installed = paths.manifest().context("读取后台安装记录失败")?;
    anyhow::ensure!(
        std::env::current_exe()?.canonicalize().context("解析后台可执行文件失败")?
            == paths
                .binary_directory(&installed)
                .join(installed.cli_name())
                .canonicalize().context("解析受保护后台安装位置失败")?,
        "后台进程必须从安装位置启动"
    );
    let directory = paths.runtime_config()?.log_dir();
    std::fs::create_dir_all(&directory).context("创建后台日志目录失败")?;
    let path = directory.join("service.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 4 * 1024 * 1024) {
        let _ = std::fs::rename(&path, directory.join("service.previous.log"));
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path).context("打开后台日志文件失败")?;
    tracing_subscriber::fmt()
        .with_env_filter("error,eclipse_cli=info,uu_core=info,uu=info,uu_capture=warn,uu_input=warn,uu_media=warn")
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
        #[arg(long)]
        independent_profile: bool,
    },
    /// Explicitly synchronize enabled plugin binaries/configuration and restart.
    ImportPlugins {
        #[arg(long)]
        source_data: Option<PathBuf>,
    },
    /// Inspect or repair rules for the actual protected service executable.
    Firewall { #[arg(long)] repair:bool },
    Start,
    Stop,
    Restart,
    Status {
        #[arg(long)]
        json:bool,
    },
    /// Remove the service registration; protected pairing/configuration is kept.
    Remove,
    /// Open the local administrator GUI for the installed host.
    Manage,
    #[command(hide = true)]
    Daemon {
        #[arg(long)]
        profile: Option<String>,
    },
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
            independent_profile,
        } => {
            let source = source_data.unwrap_or(config.data_dir.clone());
            let independent = independent_profile || config.profile_dir().is_some();
            tokio::task::spawn_blocking(move || install::install_profile(source, independent)).await??;
            if open_manager {
                manage()?;
            }
            Ok(())
        }
        Command::Firewall {repair} => {
            if repair { require_selected_profile()?; }
            let paths=Paths::discover()?; let installed=paths.manifest()?;
            let sharing=uu_core::device_book::Store::open(paths.runtime_config()?.at("device-trust"))?.read()?.sharing;
            let exe=firewall_program(&paths,&installed)?;
            let report=if repair {uu_core::firewall::ensure(&exe,sharing.service_port,sharing.discovery_port)?} else {uu_core::firewall::inspect(&exe,sharing.service_port,sharing.discovery_port)?};
            println!("{}",serde_json::to_string_pretty(&report)?);Ok(())
        }
        Command::Start => { require_selected_profile()?; start() },
        Command::ImportPlugins { source_data } => {
            let source = source_data.unwrap_or(config.data_dir);
            require_data_profile(&source)?;
            tokio::task::spawn_blocking(move || install::import_plugins(source)).await?
        }
        Command::Stop => { require_selected_profile()?; stop() },
        Command::Restart => {
            require_selected_profile()?;
            stop()?;
            start()
        }
        Command::Status {json} => {
            if json {println!("{}",serde_json::to_string_pretty(&build_status()?)?);return Ok(());}
            println!(
                "{}",
                match status() {
                    Ok(SERVICE_RUNNING) => "后台服务运行中",
                    Ok(SERVICE_STOPPED) => "后台服务已停止",
                    Ok(_) => "后台服务正在转换状态",
                    Err(_) => "后台服务未安装或不可访问",
                }
            );
            if let Ok(build)=build_status() {
                if let Some(running)=build.running {println!("后台运行版本：{} · PID {}",running.label(),build.process_id);}
                else if build.process_id!=0 {println!("后台运行版本：无法读取 · PID {}",build.process_id);}
                if let Some(installed)=build.installed {println!("后台安装版本：{}",installed.label());}
            }
            Ok(())
        }
        Command::Remove => {
            require_administrator()?;
            require_selected_profile()?;
            stop()?;
            let service = open_service(windows_sys::Win32::Storage::FileSystem::DELETE)?;
            winok(unsafe { DeleteService(service.0) })
        }
        Command::Manage => manage(),
        Command::Daemon { profile } => {
            let actual = Paths::discover()?.active_profile()?.map(|p|p.info.id);
            anyhow::ensure!(profile == actual, "Registered host profile does not match protected installation metadata");
            supervisor::dispatch()
        },
        Command::SessionHost {
            session,
            stop_event,
        } => {
            let result = host::run(session, &stop_event).await;
            if let Err(error) = &result {
                tracing::error!(error=%error,"background media process stopped");
            }
            result
        }
    }
}
pub fn start() -> Result<()> {
    require_administrator()?;
    let paths=Paths::discover()?; let installed=paths.manifest()?;
    let sharing=uu_core::device_book::Store::open(paths.runtime_config()?.at("device-trust"))?.read()?.sharing;
    let report=uu_core::firewall::ensure(&firewall_program(&paths,&installed)?,sharing.service_port,sharing.discovery_port)?;
    eprintln!("{}",report.message());
    start_registered()
}
pub(super) fn start_registered() -> Result<()> {
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
    let deadline = Instant::now() + Duration::from_secs(if expected == SERVICE_STOPPED { 95 } else { 25 });
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
    let config = Config::from_env()?;
    let mut command = std::process::Command::new(paths.binary_directory(&installed).join(installed.gui_name()));
    if let Some(profile) = config.profile_dir() { command.env("ECLIPSE_PROFILE_DIR", profile); }
    command.arg("--manage-background-service")
        .spawn()
        .context("无法打开后台服务管理窗口")?;
    Ok(())
}
