//! Complete-package deployment and idle admission, executed off the UI thread.
use uu_core::service_admin::{
    windows::{build_status, elevated_exit_code, request_owner, status, wide, Handle, BuildStatus},
    deployment::Package,
    Request, Response,
};
use windows_sys::Win32::System::Services::SERVICE_RUNNING;

#[derive(Clone, Copy)]
pub(super) enum Deployment {
    Install,
    ImportPlugins,
}
pub(super) enum DeploymentOutcome {
    Applied,
    Cancelled,
    Deferred,
    WaitingForInstallation,
    Superseded,
}
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) enum Stage { Waiting, #[default] Applying }
pub(super) enum Event { Stage(Stage), Done(Result<DeploymentOutcome, String>) }

pub(super) fn package() -> Option<&'static Package> {
    static PACKAGE: std::sync::OnceLock<Option<Package>> = std::sync::OnceLock::new();
    PACKAGE.get_or_init(|| std::env::current_exe().ok().and_then(|exe| Package::read(exe.parent()?).ok())
        .filter(|package| package.revision == env!("UU_BUILD_COMMIT"))).as_ref()
}
pub(super) fn automatic_update_needed(build: Option<&BuildStatus>, package: Option<&Package>) -> bool {
    let (Some(build), Some(package)) = (build, package) else { return false; };
    if package.built_at_utc.is_none() { return false; }
    let mut differs = false;
    for version in build.installed.iter().chain(build.running.iter()) {
        if version.revision != package.revision {
            // Ordinary clients may not read protected version.json. Unknown
            // installed time permits one candidate; the elevated owner repeats
            // the strict check using protected metadata before any replacement.
            if version.built_at_utc.is_some() && !package.is_later_than(&version.revision, version.built_at_utc.as_deref()) { return false; }
            differs = true;
        }
    }
    differs
}
struct UpdateOwner { handle: Handle, waited: bool }
impl Drop for UpdateOwner {
    fn drop(&mut self) { unsafe { windows_sys::Win32::System::Threading::ReleaseMutex(self.handle.0); } }
}
impl UpdateOwner {
    fn acquire(mut progress: impl FnMut(Stage)) -> anyhow::Result<Option<Self>> {
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let user = uu_core::service_admin::windows::process_user(unsafe { GetCurrentProcess() })?;
        Self::acquire_named(&format!("Local\\EclipseRemoteHost.update.{user}"), 120_000, &mut progress)
    }
    fn acquire_named(name: &str, timeout: u32, mut progress: impl FnMut(Stage)) -> anyhow::Result<Option<Self>> {
        use windows_sys::Win32::{Foundation::*, System::Threading::*};
        let handle = Handle(unsafe { CreateMutexW(std::ptr::null(), 0, wide(name).as_ptr()) });
        anyhow::ensure!(!handle.0.is_null(), "{}", std::io::Error::last_os_error());
        let mut result = unsafe { WaitForSingleObject(handle.0, 0) };
        let waited = result == WAIT_TIMEOUT;
        if waited { progress(Stage::Waiting); result = unsafe { WaitForSingleObject(handle.0, timeout) }; }
        if result == WAIT_TIMEOUT { return Ok(None); }
        anyhow::ensure!(result == WAIT_OBJECT_0 || result == WAIT_ABANDONED, "{}", std::io::Error::last_os_error());
        Ok(Some(Self { handle, waited }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn update_owner_times_out_without_stealing_and_releases_its_thread_ownership() {
        let name=format!("Local\\Eclipse.OwnedUpdateTest.{}",uuid::Uuid::new_v4());
        let owner=UpdateOwner::acquire_named(&name,100, |_|{}).unwrap().unwrap();
        assert!(!owner.waited);
        let other=name.clone();
        assert!(std::thread::spawn(move || UpdateOwner::acquire_named(&other,20, |stage|assert_eq!(stage,Stage::Waiting)).unwrap().is_none()).join().unwrap());
        drop(owner);
        assert!(!UpdateOwner::acquire_named(&name,100, |_|panic!("released owner must be available")).unwrap().unwrap().waited);
    }
    #[test]
    fn update_owner_waits_for_another_process_then_rechecks_without_a_second_owner() {
        use std::io::{BufRead, Read};
        let name=format!("Local\\Eclipse.OwnedUpdateTest.{}",uuid::Uuid::new_v4());
        let owner=UpdateOwner::acquire_named(&name,100, |_|{}).unwrap().unwrap();
        let mut child=std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","service_ui::deployment::tests::owned_update_mutex_child","--ignored","--nocapture"])
            .env("ECLIPSE_OWNED_UPDATE_MUTEX",&name)
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
        let mut reader=std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line=String::new();
        loop {
            line.clear(); assert_ne!(reader.read_line(&mut line).unwrap(),0,"owned child never reported its wait");
            if line.contains("OWNED_UPDATE_WAITING") {break;}
        }
        drop(owner);
        let mut tail=String::new(); reader.read_to_string(&mut tail).unwrap();
        let output=child.wait_with_output().unwrap();
        assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
        assert!(tail.contains("OWNED_UPDATE_ACQUIRED"));
        assert!(!UpdateOwner::acquire_named(&name,100, |_|{}).unwrap().unwrap().waited);
    }
    #[test]
    #[ignore = "owned cross-process helper, only invoked with a newly generated mutex name"]
    fn owned_update_mutex_child() {
        use std::io::Write;
        let name=std::env::var("ECLIPSE_OWNED_UPDATE_MUTEX").expect("owned mutex name required");
        assert!(name.starts_with("Local\\Eclipse.OwnedUpdateTest."));
        let owner=UpdateOwner::acquire_named(&name,5_000, |_|{
            println!("OWNED_UPDATE_WAITING");std::io::stdout().flush().unwrap();
        }).unwrap().unwrap();
        assert!(owner.waited);
        println!("OWNED_UPDATE_ACQUIRED");
    }
}
pub(super) fn automatic_restart_ready() -> anyhow::Result<bool> {
    if status()? != SERVICE_RUNNING {
        return Ok(true);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = request_owner(Request::Status).await?;
        if !matches!(host,Response::Status{ref snapshot,..} if snapshot.connections.is_empty()) {
            return Ok(false);
        }
        let displays = request_owner(Request::VirtualDisplay {
            request: uu_core::native_virtual_display::Request::Inspect.into(),
        })
        .await?;
        Ok(automatic_restart_safe(&displays))
    })
}
pub(super) fn automatic_restart_safe(response: &Response) -> bool {
    matches!(response,Response::VirtualDisplay{response:uu_core::native_virtual_display::Response::Snapshot{displays,status,..}}
        if displays.iter().all(|d|(!d.enabled||d.persistent)&&!d.cleanup_pending)
            && status.preparation.as_ref().is_none_or(|p|!crate::virtual_display_ui::preparation_active(p)
                && p.phase!=uu_core::virtual_display::preparation::Phase::RecoveryPending))
}
impl Deployment {
    fn action(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::ImportPlugins => "import-plugins",
        }
    }
}

pub(super) fn execute(
    deployment: Deployment,
    automatic: bool,
    mut progress: impl FnMut(Stage),
) -> Result<DeploymentOutcome, String> {
    (|| -> anyhow::Result<DeploymentOutcome> {
        let Some(owner) = UpdateOwner::acquire(&mut progress)? else { return Ok(DeploymentOutcome::WaitingForInstallation); };
        let exe = std::env::current_exe()?;
        uu_core::service_admin::package::verify(
            exe.parent()
                .ok_or_else(|| anyhow::anyhow!("application directory unavailable"))?,
            env!("UU_BUILD_COMMIT"),
        )?;
        let config = uu_core::Config::from_env()?;
        let expected = uu_core::service_profile::directory_key(&config.data_dir)?;
        let build = build_status().ok();
        if let (Some(build), Some(package)) = (&build, package()) {
            let own = build.profile.as_ref() == Some(&expected);
            if own && build.state == SERVICE_RUNNING
                && build.installed.as_ref().is_some_and(|v|v.revision == package.revision)
                && build.running.as_ref().is_some_and(|v|v.revision == package.revision)
                && matches!(deployment, Deployment::Install) && (owner.waited || automatic) {
                return Ok(DeploymentOutcome::Applied);
            }
            if automatic && (!own || matches!(deployment, Deployment::Install) && !automatic_update_needed(Some(build), Some(package))) {
                return Ok(DeploymentOutcome::Superseded);
            }
        }
        // Recheck immediately before an automatic restart. A host
        // with temporary displays or an accepted preparation is
        // still in use even when its stream count reaches zero.
        if automatic && !automatic_restart_ready()? {
            return Ok(DeploymentOutcome::Deferred);
        }
        if matches!(deployment, Deployment::Install) {
            uu_core::service_profile::prepare_source(&config)?;
        }
        progress(Stage::Applying);
        match elevated_exit_code(
            &exe,
            &format!(
                "--background-service-action {} --source-data \"{}\"{}",
                deployment.action(),
                config.data_dir.display(),
                if automatic { " --defer-if-busy" } else { "" }
            ),
        ) {
            Ok(0) => Ok(DeploymentOutcome::Applied),
            Ok(code) if code == uu_core::service_admin::deployment::DEFERRED_HOST as u32 => Ok(DeploymentOutcome::Deferred),
            Ok(code) if code == uu_core::service_admin::deployment::DEFERRED_INSTALLATION as u32 => Ok(DeploymentOutcome::WaitingForInstallation),
            Ok(code) if code == uu_core::service_admin::deployment::SUPERSEDED as u32 => Ok(DeploymentOutcome::Superseded),
            Ok(code) => anyhow::bail!("Eclipse update helper exited with status {code}"),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.raw_os_error() == Some(1223)) =>
            {
                Ok(DeploymentOutcome::Cancelled)
            }
            Err(error) => Err(error),
        }
    })()
    .map_err(|e| format!("{e:#}"))
}
