//! Explicit local management and per-stream temporary display controls.
use eframe::egui;
use std::{
    collections::BTreeSet,
    sync::mpsc,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LocalOwner {
    Foreground,
    Background,
}
impl LocalOwner {
    fn label(self) -> &'static str {
        match self {
            Self::Foreground => uu_i18n::text("前台 · 当前 Eclipse 进程"),
            Self::Background => uu_i18n::text("后台 · Windows 服务"),
        }
    }
    fn lifetime(self) -> &'static str {
        match self {
            Self::Foreground => uu_i18n::text("手动屏由当前 Eclipse 进程持有；停止前台共享或关闭此弹窗不移除，退出 Eclipse 时清理。"),
            Self::Background => uu_i18n::text("手动屏由后台共享进程持有；关闭管理窗口不影响，后台进程正常退出时清理。"),
        }
    }
}

#[derive(Debug)]
struct LocalFailure {
    message: String,
    submitted_unknown: bool,
    retained_lease: Option<uu_core::virtual_display::LeaseId>,
}
fn local_error(error: &anyhow::Error) -> LocalFailure {
    let message = if error.downcast_ref::<uu_core::virtual_display::Error>().is_some() {
        uu_core::native_virtual_display::error_message(error)
    } else {
        format!("{error:#}")
    };
    let retained_lease = match error.downcast_ref::<uu_core::virtual_display::Error>() {
        Some(uu_core::virtual_display::Error::CleanupPending { id, .. }) => Some(*id),
        _ => None,
    };
    LocalFailure { message, retained_lease, submitted_unknown: error.downcast_ref::<uu_core::service_admin::SubmittedUnknown>().is_some() }
}

fn administrator_required(owner: LocalOwner) -> bool {
    #[cfg(windows)]
    return owner == LocalOwner::Background
        && uu_core::service_admin::windows::require_administrator().is_err();
    #[cfg(not(windows))]
    { let _ = owner; false }
}
use uu_core::{
    native_virtual_display::{Client, LocalRequest, Request, Response},
    virtual_display::{Display, Owner, Slot, Spec, Status},
};

#[derive(Default)]
pub struct State {
    pub open: bool,
    local_owner: Option<LocalOwner>,
    local_selector: bool,
    foreground_displays: BTreeSet<uu_core::virtual_display::LeaseId>,
    spec: Spec,
    status: Option<Status>,
    displays: Vec<Display>,
    slots: Vec<Slot>,
    preparing_slot: Option<Slot>,
    worker: Option<mpsc::Receiver<Result<Response, LocalFailure>>>,
    worker_owner: Option<LocalOwner>,
    uncertain_owners: BTreeSet<LocalOwner>,
    pending: Option<(u64, Instant)>,
    pending_timed_out: bool,
    pub source: Option<(String, Instant)>,
    notice: String,
    notice_changed: bool,
}
impl State {
    pub fn reset_remote(&mut self) {
        self.open = false;
        self.local_owner = None;
        self.local_selector = false;
        self.pending = None;
        self.pending_timed_out = false;
        self.source = None;
        self.status = None;
        self.displays.clear();
        self.slots.clear();
        self.preparing_slot = None;
        self.notice.clear();
        self.notice_changed = false;
    }
    pub fn owns_foreground_displays(&self) -> bool {
        !self.foreground_displays.is_empty()
            || self.uncertain_owners.contains(&LocalOwner::Foreground)
    }
    fn busy(&self) -> bool {
        self.worker.is_some() || self.pending.is_some()
    }
    fn local_mutations_blocked(&self) -> bool {
        self.local_owner.is_some_and(|owner| self.uncertain_owners.contains(&owner))
    }
    fn message(&mut self, message: String) {
        self.notice = message;
        self.notice_changed = true;
    }
    pub fn take_notice(&mut self) -> Option<String> {
        std::mem::take(&mut self.notice_changed).then(|| self.notice.clone())
    }
    pub fn open_local_selector(&mut self) {
        self.open = true;
        if !self.busy() {
            self.local_selector = true;
        }
    }
    fn select_local_owner(&mut self, owner: LocalOwner) -> bool {
        if self.busy() {
            return false;
        }
        if self.local_owner != Some(owner) {
            self.status = None;
            self.displays.clear();
            self.slots.clear();
            self.preparing_slot = None;
            self.notice.clear();
        }
        self.local_owner = Some(owner);
        true
    }
    pub fn open_local(&mut self, ctx: &egui::Context, owner: LocalOwner) {
        self.open = true;
        if !self.select_local_owner(owner) {
            return;
        }
        self.local_selector = false;
        self.local(ctx, Request::Inspect.into());
    }
    fn local(&mut self, ctx: &egui::Context, request: LocalRequest) {
        if self.busy() {
            return;
        }
        let Some(owner) = self.local_owner else { return; };
        if self.local_mutations_blocked()
            && !matches!(&request, LocalRequest::Manage { request: Request::Inspect })
        {
            return;
        }
        #[cfg(windows)]
        if administrator_required(owner) {
            self.administrator(ctx);
            return;
        }
        let context = ctx.clone();
        let (send, receive) = mpsc::channel();
        self.worker = Some(receive);
        self.worker_owner = Some(owner);
        std::thread::spawn(move || {
            let result = local_request(request, owner).map_err(|error| local_error(&error));
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
        self.worker_owner = Some(LocalOwner::Background);
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
            .map_err(|error| local_error(&error));
            let _ = send.send(result);
            context.request_repaint();
        });
    }
    #[cfg(windows)]
    fn prepare_driver(&mut self, ctx: &egui::Context) {
        if self.busy() || self.local_mutations_blocked() { return; }
        let Some(owner) = self.local_owner else { return; };
        let context = ctx.clone();
        let (send, receive) = mpsc::channel();
        self.worker = Some(receive);
        self.worker_owner = Some(owner);
        self.message(uu_i18n::text("正在请求管理员准备虚拟显示器…").into());
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<Response> {
                uu_core::service_admin::windows::elevated(
                    &std::env::current_exe()?, "--prepare-virtual-display-driver",
                )?;
                local_request(Request::Inspect.into(), owner)
            })().map_err(|error| local_error(&error));
            let _ = send.send(result);
            context.request_repaint();
        });
    }
    #[cfg(windows)]
    fn install_driver(&mut self, ctx: &egui::Context) {
        if self.busy() || self.local_mutations_blocked() {
            return;
        }
        let context = ctx.clone();
        let (send, receive) = mpsc::channel();
        self.worker = Some(receive);
        self.worker_owner = self.local_owner;
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
            .map_err(|error| local_error(&error));
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
                self.pending_timed_out = false;
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
    fn receive(&mut self, response: Response, owner: Option<LocalOwner>) {
        if matches!(&response, Response::Snapshot { .. }) {
            if let Some(owner) = owner {
                self.uncertain_owners.remove(&owner);
            }
        }
        if owner == Some(LocalOwner::Foreground) {
            match &response {
                Response::Snapshot { displays, .. } => self.foreground_displays = displays.iter().map(|display| display.id).collect(),
                Response::Created { display } => { self.foreground_displays.insert(display.id); },
                Response::Removed { id } => { self.foreground_displays.remove(id); },
                Response::Error { .. } => {},
            }
        }
        // A controller reset may finish an old local worker. Preserve lease
        // ownership bookkeeping without applying its result to another scope.
        if owner.is_some() && owner != self.local_owner { return; }
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
                    let owner = self.worker_owner.take();
                    match result {
                        Ok(response) => self.receive(response, owner),
                        Err(error) => {
                            if owner == Some(LocalOwner::Foreground) {
                                if let Some(id) = error.retained_lease { self.foreground_displays.insert(id); }
                            }
                            if error.submitted_unknown {
                                if let Some(owner) = owner {
                                    self.uncertain_owners.insert(owner);
                                }
                            }
                            if owner == self.local_owner { self.message(error.message); }
                        },
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.worker = None;
                    let owner = self.worker_owner.take();
                    if let Some(owner) = owner { self.uncertain_owners.insert(owner); }
                    if owner == self.local_owner {
                        self.message(uu_i18n::text("虚拟显示器管理线程已停止").into());
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some((id, started)) = self.pending {
            if let Some(response) = client.and_then(|client| client.take_response(id)) {
                self.pending = None;
                self.pending_timed_out = false;
                self.receive(response, None);
            } else if started.elapsed() > uu_core::native_virtual_display::OPERATION_TIMEOUT
                && !self.pending_timed_out
            {
                // A timeout does not cancel the host operation. Keep its ID
                // and the admission gate until the late reply or disconnect.
                self.pending_timed_out = true;
                self.message(
                    uu_i18n::text(
                        "虚拟显示器操作尚未确认，正在等待回执；确认前不能再次操作，断开连接仍会清理临时屏",
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
        let mutations_blocked = client.is_none() && self.local_mutations_blocked();
        let mut open = self.open;
        let mut command = None;
        let mut prepare = None;
        let mut selected_owner = None;
        #[cfg(windows)]
        let mut administrator = false;
        #[cfg(windows)]
        let mut install_driver = false;
        #[cfg(windows)]
        let mut prepare_driver = false;
        egui::Window::new(uu_i18n::text("虚拟显示器"))
            .id(egui::Id::new((
                "virtual-display-manager",
                ctx.viewport_id(),
            )))
            .open(&mut open)
            .collapsible(false)
            .default_width(480.)
            .show(ctx, |ui| {
                if client.is_none() && self.local_selector {
                    ui.strong(uu_i18n::text("选择管理对象"));
                    ui.horizontal_wrapped(|ui| {
                        for owner in [LocalOwner::Foreground, LocalOwner::Background] {
                            if ui.add_enabled(!busy, egui::SelectableLabel::new(self.local_owner == Some(owner), owner.label())).clicked() {
                                selected_owner = Some(owner);
                            }
                        }
                    });
                    if busy { ui.weak(uu_i18n::text("操作完成前，管理对象保持不变。")); }
                    ui.separator();
                }
                if client.is_some() {
                    ui.strong(uu_i18n::text("管理对象：本串流连接"));
                    ui.weak(uu_i18n::text("这里创建的临时屏在本串流连接断开后自动移除。"));
                } else if let Some(owner) = self.local_owner {
                    ui.strong(uu_i18n::format!("管理对象：{}", owner.label()));
                    ui.weak(owner.lifetime());
                    if administrator_required(owner) {
                        ui.label(uu_i18n::text("后台管理需要管理员权限；在独立管理窗口中查看与操作，不会切换到前台共享。"));
                        #[cfg(windows)]
                        if ui.add_enabled(!busy, egui::Button::new(uu_i18n::text("以管理员打开后台管理"))).clicked() { administrator = true; }
                        if !self.notice.is_empty() { ui.label(&self.notice); }
                        return;
                    }
                } else {
                    ui.label(uu_i18n::text("请选择前台进程或后台服务；两者的虚拟屏分别管理。"));
                    return;
                }
                ui.add_space(8.);
                if mutations_blocked {
                    ui.weak(uu_i18n::text("上一次操作结果未知，请刷新当前管理对象的列表后再操作。"));
                }
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
                ui.checkbox(&mut self.spec.hdr, "HDR");
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !busy && !mutations_blocked,
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
                        uu_i18n::text("驱动没有可用虚拟屏，请在源机管理窗口准备驱动后重试")
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
                                    !busy && !mutations_blocked,
                                    egui::Button::new(uu_i18n::text("安装签名虚拟显示器驱动")),
                                )
                                .clicked()
                        {
                            install_driver = true;
                        }
                    }
                    #[cfg(windows)]
                    if client.is_none() && status.driver_present && status.available_slots == 0 {
                        if ui.add_enabled(!busy && !mutations_blocked, egui::Button::new(uu_i18n::text("准备虚拟显示器（管理员）")))
                            .on_hover_text(uu_i18n::text("准备已安装的虚拟显示设备，完成后刷新当前管理对象的列表。"))
                            .clicked() { prepare_driver = true; }
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
                                        !busy && !mutations_blocked,
                                        egui::Button::new(uu_i18n::text("停用现有虚拟屏…")),
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
                            if ui.add_enabled(!busy && !mutations_blocked,egui::Button::new(uu_i18n::text("确认停用"))).clicked() {
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
                                        !busy && !mutations_blocked,
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
        if let Some(owner) = selected_owner {
            if self.select_local_owner(owner) {
                self.local(ctx, Request::Inspect.into());
            }
        }
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
        #[cfg(windows)]
        if prepare_driver {
            self.prepare_driver(ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unconfirmed_remote_work_blocks_retries_until_reset_and_new_connections_drop_old_selection() {
        let mut state = State::default();
        state.pending = Some((1, Instant::now() - uu_core::native_virtual_display::OPERATION_TIMEOUT - Duration::from_secs(1)));
        state.poll(None);
        assert_eq!(state.pending.map(|pending| pending.0), Some(1));
        assert!(state.busy() && state.pending_timed_out);
        assert!(!state.notice.is_empty());
        assert!(state.take_notice().is_some());
        state.poll(None);
        assert!(state.take_notice().is_none(), "timeout is reported once while awaiting the same operation");
        state.open = true;
        state.pending = Some((2, Instant::now()));
        state.source = Some(("generated old source".into(), Instant::now()));
        state.spec.width = 2560;
        state.reset_remote();
        assert!(!state.open);
        assert!(state.pending.is_none() && state.source.is_none());
        assert!(!state.pending_timed_out);
        assert!(state.displays.is_empty() && state.status.is_none());
        assert_eq!(state.spec.width, 2560);
    }
    #[test]
    fn a_busy_local_operation_cannot_be_rebound_to_another_host_process() {
        let (send, receive) = mpsc::channel::<Result<Response, LocalFailure>>();
        let mut state = State {
            local_owner: Some(LocalOwner::Background),
            worker: Some(receive),
            ..Default::default()
        };
        state.open_local(&egui::Context::default(), LocalOwner::Foreground);
        assert_eq!(state.local_owner, Some(LocalOwner::Background));
        drop(send);
        state.poll(None);
        assert!(state.worker.is_none());
    }

    #[test]
    fn tray_selector_requires_an_explicit_owner_before_querying_a_process() {
        let mut state = State::default();
        state.open_local_selector();
        assert!(state.open && state.local_selector);
        assert_eq!(state.local_owner, None);
        assert!(!state.busy());
        state.local(&egui::Context::default(), Request::Inspect.into());
        assert!(state.worker.is_none(), "opening the tray selector cannot silently query the foreground host");
    }

    fn display() -> Display {
        Display {
            id: uu_core::virtual_display::LeaseId(uuid::Uuid::new_v4()), owner: Owner::Manual,
            spec: Spec::default(), actual: None,
            name: "Generated virtual display".into(), source: "generated-source".into(),
            active: true, cleanup_pending: false, last_error: None,
        }
    }
    fn snapshot(displays: Vec<Display>) -> Response {
        Response::Snapshot {
            status: Status { supported: true, driver_present: true, available_slots: 1,
                leased: displays.len(), explanation: String::new(), install_url: String::new() },
            displays, slots: vec![],
        }
    }
    #[test]
    fn changing_management_owner_clears_visible_inventory_but_preserves_foreground_ownership() {
        let foreground = display();
        let background = display();
        let mut state = State::default();
        assert!(state.select_local_owner(LocalOwner::Foreground));
        state.receive(snapshot(vec![foreground.clone()]), Some(LocalOwner::Foreground));
        assert!(state.owns_foreground_displays());
        assert!(state.select_local_owner(LocalOwner::Background));
        assert!(state.status.is_none() && state.displays.is_empty() && state.slots.is_empty());
        state.receive(snapshot(vec![background.clone()]), Some(LocalOwner::Background));
        assert_eq!(state.displays, vec![background]);
        state.receive(Response::Removed { id: foreground.id }, Some(LocalOwner::Background));
        assert!(state.owns_foreground_displays(), "a background reply cannot release a foreground lease");
        state.receive(Response::Removed { id: foreground.id }, Some(LocalOwner::Foreground));
        assert!(!state.owns_foreground_displays());
    }
    #[test]
    fn closed_local_window_finishes_its_operation_and_controller_reset_keeps_only_lease_bookkeeping() {
        let foreground = display();
        let (send, receive) = mpsc::channel();
        let mut state = State { open: true, local_owner: Some(LocalOwner::Foreground),
            worker: Some(receive), worker_owner: Some(LocalOwner::Foreground), ..Default::default() };
        state.open = false;
        assert!(!state.select_local_owner(LocalOwner::Background));
        state.reset_remote();
        send.send(Ok(Response::Created { display: foreground.clone() })).unwrap();
        state.poll(None);
        assert!(state.owns_foreground_displays(), "late success still keeps the launcher cleanup/tray entry alive");
        assert!(state.displays.is_empty() && state.status.is_none() && state.source.is_none());
        assert!(state.notice.is_empty());
        let (send, receive) = mpsc::channel();
        state.worker = Some(receive);
        state.worker_owner = Some(LocalOwner::Foreground);
        send.send(Err(local_error(&anyhow::anyhow!("old owner error")))).unwrap();
        state.poll(None);
        assert!(state.notice.is_empty(), "a previous local scope cannot overwrite the remote notice");
        let (send, receive) = mpsc::channel();
        state.worker = Some(receive);
        state.worker_owner = Some(LocalOwner::Foreground);
        drop(send);
        state.poll(None);
        assert!(state.notice.is_empty(), "an old worker disconnect cannot overwrite the remote notice");
    }
    #[test]
    fn submitted_unknown_requires_a_successful_inspect_of_that_same_owner_before_mutating_again() {
        let (send, receive) = mpsc::channel();
        let mut state = State { local_owner: Some(LocalOwner::Foreground),
            worker: Some(receive), worker_owner: Some(LocalOwner::Foreground), ..Default::default() };
        let error = anyhow::Error::new(uu_core::service_admin::SubmittedUnknown("generated lost reply".into()))
            .context("generated transport context");
        send.send(Err(local_error(&error))).unwrap();
        state.poll(None);
        assert!(!state.busy(), "Inspect must remain usable after an unknown result");
        assert!(state.local_mutations_blocked());
        let ctx = egui::Context::default();
        for request in [Request::Create { spec: Spec::default() }, Request::Remove { id: display().id }] {
            state.local(&ctx, request.into());
            assert!(state.worker.is_none());
        }
        state.local(&ctx, LocalRequest::PrepareSlot { id: "generated-slot".into() });
        assert!(state.worker.is_none());
        assert!(state.select_local_owner(LocalOwner::Background));
        assert!(!state.local_mutations_blocked(), "other owners have independent mutation gates");
        state.receive(snapshot(vec![]), Some(LocalOwner::Background));
        assert!(state.select_local_owner(LocalOwner::Foreground));
        assert!(state.local_mutations_blocked(), "an unrelated owner's Inspect cannot confirm the previous operation");
        state.receive(Response::Error { message: "inspection failed".into() }, Some(LocalOwner::Foreground));
        assert!(state.local_mutations_blocked());
        state.reset_remote();
        assert!(state.select_local_owner(LocalOwner::Foreground));
        assert!(state.local_mutations_blocked(), "closing or resetting UI cannot forget an unconfirmed local operation");
        state.receive(snapshot(vec![]), Some(LocalOwner::Foreground));
        assert!(!state.local_mutations_blocked());
    }
    #[test]
    fn failed_foreground_create_with_pending_cleanup_keeps_its_tray_and_shutdown_ownership() {
        let id = display().id;
        let (send, receive) = mpsc::channel();
        let mut state = State { local_owner: Some(LocalOwner::Foreground),
            worker: Some(receive), worker_owner: Some(LocalOwner::Foreground), ..Default::default() };
        let error = anyhow::Error::new(uu_core::virtual_display::Error::CleanupPending { id, message: "generated rollback failure".into() });
        send.send(Err(local_error(&error))).unwrap();
        state.reset_remote();
        state.poll(None);
        assert!(state.owns_foreground_displays(), "a failed Create can still own a target that needs cleanup");
        assert!(state.foreground_displays.contains(&id));
        assert!(state.select_local_owner(LocalOwner::Foreground));
        state.receive(snapshot(vec![]), Some(LocalOwner::Foreground));
        assert!(!state.owns_foreground_displays());
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

fn local_request(request: LocalRequest, owner: LocalOwner) -> anyhow::Result<Response> {
    if owner == LocalOwner::Foreground {
        return uu_core::native_virtual_display::execute_local_owner(request);
    }
    #[cfg(windows)]
    {
        uu_core::service_admin::windows::require_administrator()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        use anyhow::Context as _;
        match runtime
            .block_on(uu_core::service_admin::windows::request(
                uu_core::service_admin::Request::VirtualDisplay { request },
            ))
            .context(uu_i18n::text(
                "无法读取后台虚拟显示器管理响应",
            ))? {
            uu_core::service_admin::Response::VirtualDisplay { response } => Ok(response),
            uu_core::service_admin::Response::Error { message } => anyhow::bail!(message),
            _ => anyhow::bail!(
                "{}",
                uu_i18n::text("后台返回了不匹配的管理响应，请在管理员窗口查看运行版本")
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
