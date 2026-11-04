//! Tray commands use the same host-local management path as the display window.
//! The native callback queues work; downloads, UAC and driver APIs stay off UI.
use crate::tray::DisplayAction;
use crate::virtual_display_ui::{local_request, LocalOwner};
use std::sync::mpsc;
use uu_core::{
    native_virtual_display::{LocalRequest, Request, Response},
    virtual_display::Spec,
};

#[derive(Default)]
pub(super) struct State {
    worker: Option<mpsc::Receiver<anyhow::Result<Option<bool>>>>,
    uncertain: bool,
    startup_started: bool,
}
impl State {
    pub fn busy(&self) -> bool {
        self.worker.is_some() || self.uncertain
    }
    pub fn start(
        &mut self,
        ctx: &eframe::egui::Context,
        action: DisplayAction,
        spec: Spec,
        background: bool,
    ) {
        if self.busy() {
            return;
        }
        let (send, receive) = mpsc::channel();
        self.worker = Some(receive);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = run(action, spec, background).map(Some);
            let _ = send.send(result);
            ctx.request_repaint();
        });
    }
    pub fn restore_on_start(&mut self, ctx: &eframe::egui::Context) {
        if self.startup_started || self.worker.is_some() {
            return;
        }
        self.startup_started = true;
        #[cfg(windows)]
        {
            let (send, receive) = mpsc::channel();
            self.worker = Some(receive);
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let result = (|| -> anyhow::Result<Option<bool>> {
                    use windows_sys::Win32::Foundation::ERROR_SERVICE_DOES_NOT_EXIST;
                    match uu_core::service_admin::windows::status() {
                        Ok(_) => return Ok(None), // installed background owns boot recovery
                        Err(error)
                            if error
                                .downcast_ref::<std::io::Error>()
                                .and_then(|e| e.raw_os_error())
                                == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) => {}
                        Err(error) => return Err(error),
                    }
                    let config = uu_core::Config::from_env()?;
                    let manager = uu_core::virtual_display::Manager::shared();
                    manager.configure_saved_displays(config.at("device-trust"))?;
                    manager.restore_saved_displays()?;
                    Ok(Some(manager.keep_after_stream()?))
                })();
                let _ = send.send(result);
                ctx.request_repaint();
            });
        }
    }
    pub fn poll(&mut self) -> Option<anyhow::Result<Option<bool>>> {
        let received = self.worker.as_ref()?.try_recv();
        match received {
            Ok(result) => {
                self.worker = None;
                if result.as_ref().err().is_some_and(|e| {
                    e.downcast_ref::<uu_core::service_admin::SubmittedUnknown>()
                        .is_some()
                }) {
                    self.uncertain = true;
                }
                Some(result)
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.worker = None;
                self.uncertain = true;
                Some(Err(anyhow::anyhow!(
                    "{}",
                    uu_i18n::text("虚拟副屏管理线程已停止")
                )))
            }
            Err(mpsc::TryRecvError::Empty) => None,
        }
    }
}
fn keep(response: Response) -> anyhow::Result<bool> {
    match response {
        Response::Snapshot { status, .. } => Ok(status.keep_after_stream),
        Response::Error { message } => anyhow::bail!(message),
        _ => anyhow::bail!("unexpected display management response"),
    }
}
pub(super) fn execute(
    action: DisplayAction,
    spec: Spec,
    owner: LocalOwner,
) -> anyhow::Result<bool> {
    let request = match action {
        DisplayAction::Create => LocalRequest::CreateManual {
            spec,
            persistent: true,
        },
        DisplayAction::Close => LocalRequest::CloseAll,
        DisplayAction::Reset => LocalRequest::ResetDisplays,
        DisplayAction::ToggleKeep => LocalRequest::ToggleKeepAfterStream,
    };
    let response = local_request(request, owner)?;
    match response {
        Response::Created { .. } => keep(local_request(Request::Inspect.into(), owner)?),
        other => keep(other),
    }
}
fn run(action: DisplayAction, spec: Spec, background: bool) -> anyhow::Result<bool> {
    #[cfg(windows)]
    {
        if background {
            if uu_core::service_admin::windows::require_administrator().is_err() {
                let exe = std::env::current_exe()?;
                uu_core::service_admin::package::verify(
                    exe.parent()
                        .ok_or_else(|| anyhow::anyhow!("application directory unavailable"))?,
                    env!("UU_BUILD_COMMIT"),
                )?;
                let args = format!(
                    "--tray-display-action {} {} {} {} {}",
                    action.verb(),
                    spec.width,
                    spec.height,
                    spec.refresh_millihz,
                    u8::from(spec.hdr)
                );
                return match uu_core::service_admin::windows::elevated_exit_code(&exe, &args)? {
                    10 => Ok(false),
                    11 => Ok(true),
                    12 => Err(uu_core::service_admin::SubmittedUnknown(
                        uu_i18n::text("虚拟副屏操作未完成，请刷新确认当前状态。").into(),
                    )
                    .into()),
                    code => anyhow::bail!(
                        "{}",
                        uu_i18n::format!("后台服务管理未成功（退出码 {code}）")
                    ),
                };
            }
            return execute(action, spec, LocalOwner::Background);
        }
    }
    let _ = background;
    let manager = uu_core::virtual_display::Manager::shared();
    manager.configure_saved_displays(uu_core::Config::from_env()?.at("device-trust"))?;
    let result = execute(action, spec, LocalOwner::Foreground);
    #[cfg(windows)]
    if action == DisplayAction::Create
        && result.as_ref().err().is_some_and(|error| {
            use uu_core::virtual_display::{preparation::Phase, Error};
            matches!(
                error.downcast_ref::<Error>(),
                Some(Error::DriverMissing)
                    | Some(Error::Preparation(
                        uu_core::virtual_display::preparation::Status {
                            phase: Phase::NeedsDriver | Phase::NeedsElevation,
                            ..
                        }
                    ))
            )
        })
    {
        // The first attempt failed before activation. Reuse the fixed signed
        // first-install helper, then retry this single create after its reply.
        let package = crate::virtual_display_install::download()?;
        let exe = std::env::current_exe()?;
        uu_core::service_admin::windows::elevated(
            &exe,
            &format!(
                "--install-virtual-display-driver \"{}\"",
                package.directory.display()
            ),
        )?;
        return execute(action, spec, LocalOwner::Foreground);
    }
    result
}
