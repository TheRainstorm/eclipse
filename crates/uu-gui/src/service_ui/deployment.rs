//! Complete-package deployment and idle admission, executed off the UI thread.
use uu_core::service_admin::{
    windows::{elevated_exit_code, request_owner, status},
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
) -> Result<DeploymentOutcome, String> {
    (|| -> anyhow::Result<DeploymentOutcome> {
        let exe = std::env::current_exe()?;
        uu_core::service_admin::package::verify(
            exe.parent()
                .ok_or_else(|| anyhow::anyhow!("application directory unavailable"))?,
            env!("UU_BUILD_COMMIT"),
        )?;
        // Recheck immediately before an automatic restart. A host
        // with temporary displays or an accepted preparation is
        // still in use even when its stream count reaches zero.
        if automatic && !automatic_restart_ready()? {
            return Ok(DeploymentOutcome::Deferred);
        }
        let config = uu_core::Config::from_env()?;
        if matches!(deployment, Deployment::Install) {
            uu_core::service_profile::prepare_source(&config)?;
        }
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
            Ok(10) => Ok(DeploymentOutcome::Deferred),
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
