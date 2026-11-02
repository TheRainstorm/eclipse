//! Explicit local management and per-stream temporary display controls.
use eframe::egui;
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};
use uu_core::{
    native_virtual_display::{Client, LocalRequest, Request, Response},
    virtual_display::{Display, Owner, Slot, Spec, Status},
};

#[derive(Default)]
pub struct State {
    pub open: bool,
    foreground: bool,
    spec: Spec,
    status: Option<Status>,
    displays: Vec<Display>,
    slots: Vec<Slot>,
    preparing_slot: Option<Slot>,
    worker: Option<mpsc::Receiver<Result<Response, String>>>,
    pending: Option<(u64, Instant)>,
    pub source: Option<(String, Instant)>,
    notice: String,
    notice_changed: bool,
}
impl State {
    pub fn reset_remote(&mut self) {
        self.open = false;
        self.pending = None;
        self.source = None;
        self.status = None;
        self.displays.clear();
        self.slots.clear();
        self.notice.clear();
        self.notice_changed = false;
    }
    pub fn owns_foreground_displays(&self) -> bool {
        self.foreground && !self.displays.is_empty()
    }
    fn busy(&self) -> bool {
        self.worker.is_some() || self.pending.is_some()
    }
    fn message(&mut self, message: String) {
        self.notice = message;
        self.notice_changed = true;
    }
    pub fn take_notice(&mut self) -> Option<String> {
        std::mem::take(&mut self.notice_changed).then(|| self.notice.clone())
    }
    pub fn open_local(&mut self, ctx: &egui::Context, foreground: bool) {
        self.open = true;
        if self.busy() {
            return;
        }
        if self.foreground != foreground {
            self.status = None;
            self.displays.clear();
            self.slots.clear();
            self.preparing_slot = None;
        }
        self.foreground = foreground;
        self.local(ctx, Request::Inspect.into());
    }
    fn local(&mut self, ctx: &egui::Context, request: LocalRequest) {
        if self.busy() {
            return;
        }
        let foreground = self.foreground;
        let context = ctx.clone();
        let (send, receive) = mpsc::channel();
        self.worker = Some(receive);
        std::thread::spawn(move || {
            let result = local_request(request, foreground)
                .map_err(|error| uu_core::native_virtual_display::error_message(&error));
            let _ = send.send(result);
            context.request_repaint();
        });
    }
    #[cfg(windows)]
    fn administrator(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        let context = ctx.clone();
        let (send, receive) = mpsc::channel();
        self.worker = Some(receive);
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<Response> {
                uu_core::service_admin::windows::elevated(
                    &std::env::current_exe()?,
                    "--open-virtual-display-manager",
                )?;
                Ok(Response::Error {
                    message: uu_i18n::text("已打开后台虚拟显示器管理窗口").into(),
                })
            })()
            .map_err(|error| error.to_string());
            let _ = send.send(result);
            context.request_repaint();
        });
    }
    #[cfg(windows)]
    fn install_driver(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        let context = ctx.clone();
        let (send, receive) = mpsc::channel();
        self.worker = Some(receive);
        self.message(uu_i18n::text("正在下载并校验签名驱动，随后请求管理员安装…").into());
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<Response> {
                let download = super::virtual_display_install::download()?;
                let directory = download
                    .directory
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("driver package path is not Unicode"))?;
                anyhow::ensure!(
                    !directory.contains('"') && !directory.ends_with('\\'),
                    "invalid driver package path"
                );
                let arguments = format!("--install-virtual-display-driver \"{directory}\"");
                uu_core::service_admin::windows::elevated(&std::env::current_exe()?, &arguments)?;
                Ok(Response::Error {
                    message: uu_i18n::text("驱动已安装，请刷新虚拟显示器列表").into(),
                })
            })()
            .map_err(|error| error.to_string());
            let _ = send.send(result);
            context.request_repaint();
        });
    }
    fn remote(&mut self, client: &Client, request: Request) {
        if self.busy() {
            return;
        }
        match client.request(request) {
            Ok(id) => {
                self.pending = Some((id, Instant::now()));
                self.message(uu_i18n::text("正在管理虚拟显示器…").into());
            }
            Err(error) => self.message(error.to_string()),
        }
    }
    pub fn create_remote(&mut self, client: &Client) {
        self.remote(client, Request::Create { spec: self.spec });
    }
    pub fn open_remote(&mut self, client: &Client) {
        self.open = true;
        self.remote(client, Request::Inspect);
    }
    fn receive(&mut self, response: Response) {
        match response {
            Response::Snapshot {
                status,
                displays,
                slots,
            } => {
                self.status = Some(status);
                self.displays = displays;
                self.slots = slots;
                self.message(String::new());
            }
            Response::Created { display } => {
                if matches!(display.owner, Owner::Session(_)) {
                    self.source = Some((display.source.clone(), Instant::now()));
                }
                self.displays.retain(|d| d.id != display.id);
                self.displays.push(display);
                self.message(uu_i18n::text("虚拟显示器已创建").into());
                // Capacity changed. Ask explicitly before the next creation.
                self.status = None;
            }
            Response::Removed { id } => {
                self.displays.retain(|d| d.id != id);
                self.status = None;
                self.message(uu_i18n::text("虚拟显示器已移除").into());
            }
            Response::Error { message } => self.message(message),
        }
    }
    pub fn poll(&mut self, client: Option<&Client>) {
        if let Some(receiver) = &self.worker {
            match receiver.try_recv() {
                Ok(result) => {
                    self.worker = None;
                    match result {
                        Ok(response) => self.receive(response),
                        Err(error) => self.message(error),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.worker = None;
                    self.message(uu_i18n::text("虚拟显示器管理线程已停止").into());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some((id, started)) = self.pending {
            if let Some(response) = client.and_then(|client| client.take_response(id)) {
                self.pending = None;
                self.receive(response);
            } else if started.elapsed() > Duration::from_secs(20) {
                self.pending = None;
                self.message(
                    uu_i18n::text(
                        "虚拟显示器操作结果未知，请刷新列表后再操作；连接结束仍会清理临时屏",
                    )
                    .into(),
                );
            }
        }
        if self
            .source
            .as_ref()
            .is_some_and(|(_, started)| started.elapsed() > Duration::from_secs(12))
        {
            self.source = None;
            self.message(
                uu_i18n::text("虚拟屏已创建，但显示目录尚未更新，请重新选择显示器").into(),
            );
        }
    }
    pub fn show(&mut self, ctx: &egui::Context, client: Option<&Client>) {
        if !self.open {
            return;
        }
        let busy = self.busy();
        let mut open = self.open;
        let mut command = None;
        let mut prepare = None;
        #[cfg(windows)]
        let mut administrator = false;
        #[cfg(windows)]
        let mut install_driver = false;
        egui::Window::new(uu_i18n::text("虚拟显示器"))
            .id(egui::Id::new((
                "virtual-display-manager",
                ctx.viewport_id(),
            )))
            .open(&mut open)
            .collapsible(false)
            .default_width(480.)
            .show(ctx, |ui| {
                ui.label(if client.is_some() {
                    uu_i18n::text("这里创建的临时屏在本串流连接断开后自动移除。")
                } else if self.foreground {
                    uu_i18n::text("正在管理本机前台共享进程的虚拟显示器。")
                } else {
                    uu_i18n::text("正在管理已安装的后台服务的虚拟显示器。")
                });
                if client.is_none() {
                    ui.weak(uu_i18n::text(
                        "手动创建的屏幕保留到主动移除或所属共享进程正常退出。",
                    ));
                }
                ui.add_space(8.);
                ui.horizontal(|ui| {
                    ui.label(uu_i18n::text("分辨率"));
                    ui.add(egui::DragValue::new(&mut self.spec.width).range(320..=16384));
                    ui.label("×");
                    ui.add(egui::DragValue::new(&mut self.spec.height).range(200..=16384));
                    let mut fps = self.spec.refresh_millihz / 1000;
                    if ui
                        .add(egui::DragValue::new(&mut fps).range(1..=1000).suffix(" Hz"))
                        .changed()
                    {
                        self.spec.refresh_millihz = fps * 1000;
                    }
                });
                #[cfg(windows)]
                if client.is_none()
                    && !self.foreground
                    && ui
                        .add_enabled(
                            !self.busy(),
                            egui::Button::new(uu_i18n::text("以管理员管理后台服务")),
                        )
                        .clicked()
                {
                    administrator = true;
                }
                ui.checkbox(&mut self.spec.hdr, "HDR");
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !self.busy(),
                            egui::Button::new(uu_i18n::text("新建虚拟显示器")),
                        )
                        .clicked()
                    {
                        command = Some(Request::Create { spec: self.spec });
                    }
                    if ui
                        .add_enabled(!self.busy(), egui::Button::new(uu_i18n::text("刷新")))
                        .clicked()
                    {
                        command = Some(Request::Inspect);
                    }
                    if self.busy() {
                        ui.spinner();
                        ctx.request_repaint_after(Duration::from_millis(100));
                    }
                });
                if let Some(status) = &self.status {
                    ui.label(uu_i18n::format!(
                        "可用虚拟屏槽位：{}",
                        status.available_slots
                    ));
                    ui.weak(if !status.supported {
                        uu_i18n::text("虚拟显示器仅支持 Windows")
                    } else if !status.driver_present {
                        uu_i18n::text("源机尚未安装支持的签名虚拟显示器驱动")
                    } else if status.available_slots == 0 {
                        uu_i18n::text("没有空闲虚拟屏，请在源机托盘中准备一个虚拟屏槽位")
                    } else {
                        uu_i18n::text(
                            "已准备好空闲虚拟屏，可以创建；移除仅断开所属目标的桌面输出。",
                        )
                    });
                    if !status.driver_present {
                        ui.hyperlink_to(
                            uu_i18n::text("下载签名虚拟显示器驱动"),
                            uu_core::virtual_display::INSTALL_URL,
                        );
                        #[cfg(windows)]
                        if client.is_none()
                            && ui
                                .add_enabled(
                                    !self.busy(),
                                    egui::Button::new(uu_i18n::text("安装签名虚拟显示器驱动")),
                                )
                                .clicked()
                        {
                            install_driver = true;
                        }
                    }
                }
                ui.separator();
                if client.is_none() {
                    for slot in &self.slots {
                        if slot.leased {
                            continue;
                        }
                        ui.horizontal(|ui| {
                            ui.label(format!("{} · {}", slot.name, slot.source));
                            if slot.active {
                                if ui
                                    .add_enabled(
                                        !busy,
                                        egui::Button::new(uu_i18n::text("提供为空闲虚拟屏")),
                                    )
                                    .clicked()
                                {
                                    self.preparing_slot = Some(slot.clone());
                                }
                            } else {
                                ui.weak(uu_i18n::text("空闲虚拟屏"));
                            }
                        });
                    }
                    if let Some(slot) = self.preparing_slot.clone() {
                        ui.label(uu_i18n::format!(
                            "将断开 {} 的桌面输出，供 Eclipse 临时创建使用。其它显示器保持不变。",
                            slot.source
                        ));
                        ui.horizontal(|ui| {
                            if ui.button(uu_i18n::text("确认提供")).clicked() {
                                prepare = Some(slot.id);
                                self.preparing_slot = None;
                            }
                            if ui.button(uu_i18n::text("取消")).clicked() {
                                self.preparing_slot = None;
                            }
                        });
                    }
                }
                egui::ScrollArea::vertical()
                    .max_height(300.)
                    .show(ui, |ui| {
                        for display in &self.displays {
                            ui.horizontal(|ui| {
                                ui.label(format!(
                                    "{} · {}×{} · {:.3} Hz",
                                    display.name,
                                    display.spec.width,
                                    display.spec.height,
                                    display.spec.refresh_millihz as f32 / 1000.
                                ));
                                ui.weak(if matches!(display.owner, Owner::Manual) {
                                    uu_i18n::text("手动创建")
                                } else {
                                    uu_i18n::text("连接临时屏")
                                });
                                if ui
                                    .add_enabled(
                                        !self.busy(),
                                        egui::Button::new(if display.cleanup_pending {
                                            uu_i18n::text("重试移除")
                                        } else {
                                            uu_i18n::text("移除")
                                        }),
                                    )
                                    .clicked()
                                {
                                    command = Some(Request::Remove { id: display.id });
                                }
                            });
                            if let Some(error) = &display.last_error {
                                ui.colored_label(crate::theme::DANGER, error);
                            }
                        }
                    });
                if !self.notice.is_empty() {
                    ui.label(&self.notice);
                }
            });
        self.open = open;
        if let Some(command) = command {
            if let Some(client) = client {
                self.remote(client, command);
            } else {
                self.local(ctx, command.into());
            }
        }
        if let Some(id) = prepare {
            self.local(ctx, LocalRequest::PrepareSlot { id });
        }
        #[cfg(windows)]
        if administrator {
            self.administrator(ctx);
        }
        #[cfg(windows)]
        if install_driver {
            self.install_driver(ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_remote_work_expires_without_a_controller_and_new_connections_drop_old_selection() {
        let mut state = State::default();
        state.pending = Some((1, Instant::now() - Duration::from_secs(21)));
        state.poll(None);
        assert!(state.pending.is_none());
        assert!(!state.notice.is_empty());
        state.open = true;
        state.pending = Some((2, Instant::now()));
        state.source = Some(("generated old source".into(), Instant::now()));
        state.spec.width = 2560;
        state.reset_remote();
        assert!(!state.open);
        assert!(state.pending.is_none() && state.source.is_none());
        assert!(state.displays.is_empty() && state.status.is_none());
        assert_eq!(state.spec.width, 2560);
    }
    #[test]
    fn a_busy_local_operation_cannot_be_rebound_to_another_host_process() {
        let (send, receive) = mpsc::channel::<Result<Response, String>>();
        let mut state = State {
            foreground: false,
            worker: Some(receive),
            ..Default::default()
        };
        state.open_local(&egui::Context::default(), true);
        assert!(!state.foreground);
        drop(send);
        state.poll(None);
        assert!(state.worker.is_none());
    }
}

#[derive(Default)]
pub struct BackgroundHost {
    running: bool,
    next: Option<Instant>,
    worker: Option<mpsc::Receiver<bool>>,
}
impl BackgroundHost {
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        if let Some(receiver) = &self.worker {
            match receiver.try_recv() {
                Ok(running) => {
                    self.running = running;
                    self.worker = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.running = false;
                    self.worker = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        #[cfg(windows)]
        if self.worker.is_none() && self.next.is_none_or(|at| Instant::now() >= at) {
            self.next = Some(Instant::now() + Duration::from_secs(5));
            let (send, receive) = mpsc::channel();
            self.worker = Some(receive);
            let context = ctx.clone();
            std::thread::spawn(move || {
                let running = uu_core::service_admin::windows::status().is_ok_and(|state| {
                    state == windows_sys::Win32::System::Services::SERVICE_RUNNING
                });
                let _ = send.send(running);
                context.request_repaint();
            });
        }
        if self.worker.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else if let Some(next) = self.next {
            // An idle launcher still needs to discover an independently
            // started/stopped service so its tray entry follows the host.
            ctx.request_repaint_after(next.saturating_duration_since(Instant::now()));
        }
        self.running
    }
}

fn local_request(request: LocalRequest, foreground: bool) -> anyhow::Result<Response> {
    if foreground {
        return uu_core::native_virtual_display::execute_local_owner(request);
    }
    #[cfg(windows)]
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        use anyhow::Context as _;
        match runtime
            .block_on(uu_core::service_admin::windows::request(
                uu_core::service_admin::Request::VirtualDisplay { request },
            ))
            .context(uu_i18n::text(
                "无法管理后台虚拟显示器，请确认后台已更新并正在运行；需要管理员权限",
            ))? {
            uu_core::service_admin::Response::VirtualDisplay { response } => Ok(response),
            uu_core::service_admin::Response::Error { message } => anyhow::bail!(message),
            _ => anyhow::bail!(
                "{}",
                uu_i18n::text("后台尚不支持虚拟显示器管理，请更新后台服务")
            ),
        }
    }
    #[cfg(not(windows))]
    anyhow::bail!("{}", uu_i18n::text("虚拟显示器仅支持 Windows"))
}

impl crate::App {
    pub(super) fn manage_remote_virtual_displays(&mut self) {
        self.release_all_keys();
        self.stream_menu.open = false;
        if let Some(handle) = &self.controller {
            self.virtual_display
                .open_remote(&handle.stats.virtual_displays);
        }
    }
    pub(super) fn create_remote_virtual_display(&mut self) {
        self.release_all_keys();
        self.stream_menu.open = false;
        if let Some(handle) = &self.controller {
            self.virtual_display
                .create_remote(&handle.stats.virtual_displays);
        }
    }
    pub(super) fn poll_virtual_displays(&mut self, ctx: &egui::Context) {
        let stats = self.controller.as_ref().map(|handle| handle.stats.clone());
        self.virtual_display
            .poll(stats.as_ref().map(|stats| &stats.virtual_displays));
        if let Some((source, _)) = &self.virtual_display.source {
            let catalog = self.remote_screens();
            let mut matching = catalog
                .screens
                .iter()
                .filter(|screen| &screen.display_name == source);
            if let Some(screen) = matching.next().filter(|_| matching.next().is_none()) {
                let id = screen.id;
                self.virtual_display.source = None;
                self.select_remote_screen(id);
            }
        }
        if self.stream_target.is_some() {
            if let Some(notice) = self.virtual_display.take_notice() {
                self.displays.notice = notice;
            }
        }
        self.virtual_display
            .show(ctx, stats.as_ref().map(|stats| &stats.virtual_displays));
    }
}
