//! Explicit local management and per-stream temporary display controls.
#[path = "virtual_display_preparation.rs"]
mod preparation_ui;
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
            Self::Foreground => uu_i18n::text(
                "手动屏由当前 Eclipse 进程持有；停止前台共享或关闭此弹窗不移除，退出 Eclipse 时清理。",
            ),
            Self::Background => uu_i18n::text(
                "手动屏由后台共享进程持有；关闭管理窗口不影响，后台进程正常退出时清理。",
            ),
        }
    }
}

#[derive(Debug)]
struct LocalFailure {
    message: String,
    submitted_unknown: bool,
    retained_lease: Option<uu_core::virtual_display::LeaseId>,
    preparation: Option<uu_core::virtual_display::preparation::Status>,
    details: String,
}
fn local_error(error: &anyhow::Error) -> LocalFailure {
    use uu_core::virtual_display::Error;
    let message = match error.downcast_ref::<Error>() {
        Some(Error::Preparation(status)) => {
            uu_core::native_virtual_display::preparation_message(status)
        }
        Some(Error::CleanupPending { .. }) => {
            uu_i18n::text("虚拟副屏尚未完成清理，请保留本次操作并查看诊断详情。").into()
        }
        Some(Error::Platform(_)) | None => {
            uu_i18n::text("虚拟副屏操作未完成，请检查后台状态或查看诊断详情。").into()
        }
        _ => uu_core::native_virtual_display::error_message(error),
    };
    let retained_lease = match error.downcast_ref::<uu_core::virtual_display::Error>() {
        Some(uu_core::virtual_display::Error::CleanupPending { id, .. }) => Some(*id),
        _ => None,
    };
    let preparation = match error.downcast_ref::<Error>() {
        Some(Error::Preparation(status)) => Some(status.clone()),
        _ => None,
    };
    LocalFailure {
        message,
        retained_lease,
        preparation,
        details: format!("{error:#}"),
        submitted_unknown: error
            .downcast_ref::<uu_core::service_admin::SubmittedUnknown>()
            .is_some(),
    }
}

fn administrator_required(owner: LocalOwner) -> bool {
    #[cfg(windows)]
    return owner == LocalOwner::Background
        && uu_core::service_admin::windows::require_administrator().is_err();
    #[cfg(not(windows))]
    {
        let _ = owner;
        false
    }
}
use uu_core::{
    native_virtual_display::{Client, LocalRequest, Request, Response},
    virtual_display::{Display, Owner, Slot, Spec, Status},
};

#[derive(Clone, Copy)]
enum Startup {
    Waiting(Instant),
    Finished,
}

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
    worker_started: Option<Instant>,
    worker_timed_out: bool,
    worker_preparation: bool,
    uncertain_owners: BTreeSet<LocalOwner>,
    pending: Option<(u64, Instant)>,
    pending_timed_out: bool,
    pub source: Option<(String, Instant)>,
    source_timed_out: bool,
    notice: String,
    notice_changed: bool,
    details: String,
    preparation: Option<uu_core::virtual_display::preparation::Status>,
    background_setup_requested: bool,
    next_preparation_check: Option<Instant>,
    remote_inventory_uncertain: bool,
    remote_refresh_requested: bool,
    remote_refresh_attempted: bool,
    startup: Option<Startup>,
    initial_timed_out: bool,
    remote_create_supported: bool,
    fixed_source: Option<String>,
    create_requested: bool,
}
impl State {
    pub(super) fn spec(&self) -> Spec {
        self.spec
    }
    pub(super) fn prepare_startup(&mut self, spec: Spec) {
        self.spec = spec;
        self.startup = Some(Startup::Waiting(Instant::now()));
        self.initial_timed_out = false;
        self.message(uu_i18n::text("正在创建临时虚拟副屏…").into());
    }
    fn poll_startup(&mut self) {
        let Some(Startup::Waiting(started)) = self.startup else {
            return;
        };
        if !self.initial_timed_out
            && started.elapsed() > uu_core::native_virtual_display::OPERATION_TIMEOUT
        {
            self.initial_timed_out = true;
            self.message(uu_i18n::text("虚拟副屏操作尚未确认，正在等待回执；确认前不能再次操作，断开连接仍会清理临时屏").into());
        }
    }
    pub(super) fn startup_failed(&mut self, message: String) {
        self.startup = Some(Startup::Finished);
        self.message(message);
    }
    fn take_create_request(&mut self) -> bool {
        std::mem::take(&mut self.create_requested)
    }
    fn request_background_setup(&mut self) {
        if self.busy() {
            return;
        }
        self.background_setup_requested = true;
        self.open = false;
    }
    fn take_background_setup_request(&mut self) -> bool {
        std::mem::take(&mut self.background_setup_requested)
    }
    fn take_remote_refresh_request(&mut self) -> bool {
        if self.busy() || !self.remote_inventory_uncertain || !self.remote_refresh_requested {
            return false;
        }
        self.remote_refresh_requested = false;
        self.remote_refresh_attempted = true;
        true
    }
    fn preparation_refresh_due(&self, now: Instant) -> bool {
        self.open
            && !self.busy()
            && !self.local_mutations_blocked()
            && self
                .preparation
                .as_ref()
                .is_some_and(|s| s.queued && preparation_ui::active(s))
            && self.next_preparation_check.is_some_and(|at| now >= at)
    }
    pub fn reset_remote(&mut self) {
        self.open = false;
        self.local_owner = None;
        self.local_selector = false;
        self.pending = None;
        self.pending_timed_out = false;
        self.source = None;
        self.source_timed_out = false;
        self.status = None;
        self.displays.clear();
        self.slots.clear();
        self.preparing_slot = None;
        self.preparation = None;
        self.details.clear();
        self.background_setup_requested = false;
        self.next_preparation_check = None;
        self.remote_inventory_uncertain = false;
        self.remote_refresh_requested = false;
        self.remote_refresh_attempted = false;
        self.notice.clear();
        self.notice_changed = false;
        self.startup = None;
        self.initial_timed_out = false;
        self.remote_create_supported = false;
        self.fixed_source = None;
        self.create_requested = false;
    }
    pub fn owns_foreground_displays(&self) -> bool {
        !self.foreground_displays.is_empty()
            || self.uncertain_owners.contains(&LocalOwner::Foreground)
    }
    fn busy(&self) -> bool {
        self.worker.is_some() || self.pending.is_some()
    }
    fn local_mutations_blocked(&self) -> bool {
        self.local_owner
            .is_some_and(|owner| self.uncertain_owners.contains(&owner))
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
            self.details.clear();
            self.preparation = None;
            self.next_preparation_check = None;
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
        let Some(owner) = self.local_owner else {
            return;
        };
        if self.local_mutations_blocked()
            && !matches!(
                &request,
                LocalRequest::Manage {
                    request: Request::Inspect
                }
            )
        {
            return;
        }
        if administrator_required(owner)
            || (owner == LocalOwner::Foreground
                && matches!(&request, LocalRequest::Prepare | LocalRequest::Recover))
        {
            self.request_background_setup();
            return;
        }
        let preparing = matches!(&request, LocalRequest::Prepare | LocalRequest::Recover);
        self.message(
            if preparing {
                uu_i18n::text("正在准备虚拟副屏，请等待本次操作完成。")
            } else {
                uu_i18n::text("正在读取或更新虚拟副屏状态…")
            }
            .into(),
        );
        self.start_worker(ctx, owner, preparing, move || local_request(request, owner));
    }
    fn start_worker(
        &mut self,
        ctx: &egui::Context,
        owner: LocalOwner,
        preparing: bool,
        work: impl FnOnce() -> anyhow::Result<Response> + Send + 'static,
    ) {
        if self.busy() {
            return;
        }
        let context = ctx.clone();
        let (send, receive) = mpsc::channel();
        self.worker = Some(receive);
        self.worker_owner = Some(owner);
        self.worker_started = Some(Instant::now());
        self.worker_timed_out = false;
        self.worker_preparation = preparing;
        std::thread::spawn(move || {
            let result = work().map_err(|error| local_error(&error));
            let _ = send.send(result);
            context.request_repaint();
        });
    }
    fn remote(&mut self, client: &Client, request: Request) {
        if self.busy() || (self.remote_inventory_uncertain && !matches!(&request, Request::Inspect))
        {
            return;
        }
        let inspect = matches!(&request, Request::Inspect);
        match client.request(request) {
            Ok(id) => {
                self.pending = Some((id, Instant::now()));
                self.pending_timed_out = false;
                if !inspect {
                    self.remote_refresh_attempted = false;
                }
                self.message(uu_i18n::text("正在管理虚拟副屏…").into());
            }
            Err(error) => self.message(error.to_string()),
        }
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
        if matches!(&response, Response::Error { .. }) {
            if let Some(owner) = owner {
                self.uncertain_owners.insert(owner);
            }
        }
        if owner == Some(LocalOwner::Foreground) {
            match &response {
                Response::Snapshot { displays, .. } => {
                    self.foreground_displays = displays.iter().map(|display| display.id).collect()
                }
                Response::Created { display } => {
                    self.foreground_displays.insert(display.id);
                }
                Response::Removed { id } => {
                    self.foreground_displays.remove(id);
                }
                Response::Error { .. } => {}
            }
        }
        // A controller reset may finish an old local worker. Preserve lease
        // ownership bookkeeping without applying its result to another scope.
        if owner.is_some() && owner != self.local_owner {
            return;
        }
        match response {
            Response::Snapshot {
                status,
                displays,
                slots,
            } => {
                if owner.is_none() {
                    self.remote_inventory_uncertain = false;
                    self.remote_refresh_requested = false;
                    self.remote_refresh_attempted = false;
                }
                self.preparation = status.preparation.clone();
                self.next_preparation_check = self
                    .preparation
                    .as_ref()
                    .filter(|s| s.queued && preparation_ui::active(s))
                    .map(|_| Instant::now() + Duration::from_secs(1));
                self.status = Some(status);
                self.displays = displays;
                self.slots = slots;
                self.message(String::new());
                self.details.clear();
            }
            Response::Created { display } => {
                if self.startup.is_some() {
                    self.startup = Some(Startup::Finished);
                }
                if matches!(display.owner, Owner::Session(_)) {
                    self.source = Some((display.source.clone(), Instant::now()));
                    self.source_timed_out = false;
                }
                self.displays.retain(|d| d.id != display.id);
                self.displays.push(display);
                self.message(uu_i18n::text("虚拟副屏已创建").into());
                // Capacity changed. Ask explicitly before the next creation.
                self.status = None;
                self.preparation = None;
                self.next_preparation_check = None;
                self.details.clear();
            }
            Response::Removed { id } => {
                self.displays.retain(|d| d.id != id);
                self.status = None;
                self.preparation = None;
                self.next_preparation_check = None;
                self.details.clear();
                self.message(uu_i18n::text("虚拟副屏已移除").into());
            }
            Response::Error { message } => {
                if self.startup.is_some() {
                    self.startup = Some(Startup::Finished);
                }
                self.status = None;
                self.preparation = None;
                self.next_preparation_check = None;
                if owner.is_none() {
                    self.remote_inventory_uncertain = true;
                    self.remote_refresh_requested = !self.remote_refresh_attempted;
                }
                self.details = message;
                self.message(uu_i18n::text("虚拟副屏操作未完成，请刷新确认当前状态。").into());
            }
        }
    }
    pub fn poll(&mut self, client: Option<&Client>) {
        self.poll_startup();
        if let Some(receiver) = &self.worker {
            match receiver.try_recv() {
                Ok(result) => {
                    self.worker = None;
                    self.worker_started = None;
                    self.worker_timed_out = false;
                    self.worker_preparation = false;
                    let owner = self.worker_owner.take();
                    match result {
                        Ok(response) => self.receive(response, owner),
                        Err(error) => {
                            if owner == Some(LocalOwner::Foreground) {
                                if let Some(id) = error.retained_lease {
                                    self.foreground_displays.insert(id);
                                }
                            }
                            if error.submitted_unknown {
                                if let Some(owner) = owner {
                                    self.uncertain_owners.insert(owner);
                                }
                            }
                            if owner == self.local_owner {
                                self.details = error.details;
                                if let Some(preparation) = error.preparation {
                                    self.next_preparation_check = (preparation.queued
                                        && preparation_ui::active(&preparation))
                                    .then(|| Instant::now() + Duration::from_secs(1));
                                    self.preparation = Some(preparation);
                                }
                                self.message(error.message);
                            }
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.worker = None;
                    self.worker_started = None;
                    self.worker_timed_out = false;
                    self.worker_preparation = false;
                    let owner = self.worker_owner.take();
                    if let Some(owner) = owner {
                        self.uncertain_owners.insert(owner);
                    }
                    if owner == self.local_owner {
                        self.message(uu_i18n::text("虚拟副屏管理线程已停止").into());
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.worker.is_some()
            && self.worker_owner == self.local_owner
            && !self.worker_timed_out
            && self
                .worker_started
                .is_some_and(|at| at.elapsed() > uu_core::native_virtual_display::OPERATION_TIMEOUT)
        {
            self.worker_timed_out = true;
            self.message(
                uu_i18n::text(
                    "操作仍在等待后台确认，请保留本次请求；等待提示不会取消准备，也不会自动重试。",
                )
                .into(),
            );
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
                        "虚拟副屏操作尚未确认，正在等待回执；确认前不能再次操作，断开连接仍会清理临时屏",
                    )
                    .into(),
                );
            }
        }
        if !self.source_timed_out
            && self
                .source
                .as_ref()
                .is_some_and(|(_, started)| started.elapsed() > Duration::from_secs(12))
        {
            self.source_timed_out = true;
            if self.startup.is_some() {
                // A late catalog may still confirm this exact created target.
                self.message(
                    uu_i18n::text("仍在等待临时虚拟屏进入显示目录，可关闭此标签取消").into(),
                );
            } else {
                self.source = None;
                self.message(
                    uu_i18n::text("虚拟屏已创建，但显示目录尚未更新，请重新选择显示器").into(),
                );
            }
        }
    }
    pub fn show(&mut self, ctx: &egui::Context, client: Option<&Client>) {
        if !self.open {
            return;
        }
        if let Some(client) = client {
            if self.take_remote_refresh_request() {
                self.remote(client, Request::Inspect);
            }
        }
        if self.preparation_refresh_due(Instant::now()) {
            self.next_preparation_check = None;
            if let Some(client) = client {
                self.remote(client, Request::Inspect);
            } else {
                self.local(ctx, Request::Inspect.into());
            }
        }
        let busy = self.busy();
        let mutations_blocked = if client.is_some() {
            self.remote_inventory_uncertain
        } else {
            self.local_mutations_blocked()
        };
        let background_unavailable = client.is_none()
            && self.local_owner == Some(LocalOwner::Background)
            && self.status.is_none()
            && self.preparation.is_none()
            && !self.details.is_empty();
        let mut open = self.open;
        let mut command = None;
        let mut prepare = None;
        let mut selected_owner = None;
        let mut preparation_action = None;
        egui::Window::new(uu_i18n::text("虚拟副屏"))
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
                        ui.label(uu_i18n::text("请在服务管理中准备后台，再创建虚拟副屏；已有副屏不会因打开设置而断开。"));
                        if ui.add_enabled(!busy,egui::Button::new(uu_i18n::text("打开后台准备"))).clicked(){preparation_action=Some(preparation_ui::Action::BackgroundSetup);}
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
                            !busy && !mutations_blocked && !background_unavailable && (client.is_none() || self.remote_create_supported)
                                && self.status.as_ref().is_none_or(|s|s.supported)
                                && preparation_ui::can_create(self.preparation.as_ref(),self.preparation.as_ref().map_or_else(||self.status.as_ref().map_or(0,|s|s.available_slots),|s|s.available as usize),self.local_owner==Some(LocalOwner::Foreground)&&client.is_none()),
                            egui::Button::new(uu_i18n::text("新建虚拟副屏")),
                        )
                        .on_disabled_hover_text(uu_i18n::text("远端未提供虚拟副屏管理，请更新后台并授予管理服务权限"))
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
                        if self.worker_preparation {ui.weak(uu_i18n::text("正在准备虚拟副屏，请等待本次操作完成。"));}
                        ctx.request_repaint_after(Duration::from_millis(100));
                    }
                });
                if let Some(preparation)=&self.preparation {
                    ui.label(uu_i18n::format!("空闲副屏：{}",preparation.available));
                    if let Some(status)=&self.status {ui.weak(uu_i18n::format!("已占用：{}",status.leased));}
                    ui.weak(uu_core::native_virtual_display::preparation_message(preparation));
                    if preparation_ui::active(preparation) {
                        ui.weak(uu_i18n::text("已登记的准备会在空闲时继续；无需重复点击，也不会中断正在使用的副屏。"));
                        if let Some(at)=self.next_preparation_check {ctx.request_repaint_after(at.saturating_duration_since(Instant::now()));}
                    }
                    if client.is_none() {
                        if let Some(action)=preparation_ui::action(preparation,self.local_owner==Some(LocalOwner::Foreground)) {
                            let label=match action {
                                preparation_ui::Action::Prepare=>uu_i18n::text("准备虚拟副屏"),
                                preparation_ui::Action::Recover=>uu_i18n::text("检查并恢复准备"),
                                preparation_ui::Action::BackgroundSetup=>uu_i18n::text("打开后台准备"),
                            };
                            if ui.add_enabled(!busy&&!mutations_blocked,egui::Button::new(label)).clicked(){preparation_action=Some(action);}
                        }
                    }
                } else if let Some(status) = &self.status {
                    ui.label(uu_i18n::format!(
                        "可用虚拟屏槽位：{}",
                        status.available_slots
                    ));
                    ui.weak(if !status.supported {
                        uu_i18n::text("虚拟副屏仅支持 Windows")
                    } else if !status.driver_present {
                        uu_i18n::text("源机尚未安装支持的签名虚拟副屏驱动")
                    } else if status.available_slots == 0 {
                        uu_i18n::text("驱动没有可用虚拟屏，请在源机管理窗口准备驱动后重试")
                    } else {
                        uu_i18n::text(
                            "已准备好空闲虚拟屏，可以创建；移除仅断开所属目标的桌面输出。",
                        )
                    });
                    if !status.driver_present {
                        ui.hyperlink_to(
                            uu_i18n::text("下载签名虚拟副屏驱动"),
                            uu_core::virtual_display::INSTALL_URL,
                        );

                    }
                    #[cfg(windows)]
                    if client.is_none() && status.driver_present && status.available_slots == 0 {
                        if ui.add_enabled(!busy && !mutations_blocked, egui::Button::new(uu_i18n::text("打开后台准备")))
                            .on_hover_text(uu_i18n::text("准备已安装的虚拟显示设备，完成后刷新当前管理对象的列表。"))
                            .clicked() {preparation_action=Some(preparation_ui::Action::BackgroundSetup);}
                    }
                    if client.is_none()&&!status.driver_present {
                        ui.weak(uu_i18n::text("更新或启用源机后台后，可自动准备虚拟副屏。"));
                        if ui.add_enabled(!busy&&!mutations_blocked,egui::Button::new(uu_i18n::text("打开后台准备"))).clicked(){preparation_action=Some(preparation_ui::Action::BackgroundSetup);}
                    }
                }
                if background_unavailable {
                    ui.weak(uu_i18n::text("请在服务管理中准备后台，再创建虚拟副屏；已有副屏不会因打开设置而断开。"));
                    if ui.add_enabled(!busy,egui::Button::new(uu_i18n::text("打开后台准备"))).clicked(){preparation_action=Some(preparation_ui::Action::BackgroundSetup);}
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
                                let fixed_current=client.is_some() && self.fixed_source.as_ref()==Some(&display.source);
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
                                        !busy && !mutations_blocked && !fixed_current,
                                        egui::Button::new(if display.cleanup_pending {
                                            uu_i18n::text("重试移除")
                                        } else {
                                            uu_i18n::text("移除")
                                        }),
                                    )
                                    .on_disabled_hover_text(uu_i18n::text("关闭此串流标签即可断开并移除它的临时虚拟屏"))
                                    .clicked()
                                {
                                    command = Some(Request::Remove { id: display.id });
                                }
                            });
                            if let Some(error) = &display.last_error {
                                ui.push_id(display.id.0,|ui|{ui.collapsing(uu_i18n::text("诊断详情"),|ui|{ui.monospace(error);});});
                            }
                        }
                    });
                if !self.notice.is_empty() {
                    ui.label(&self.notice);
                }
                let provider_error=self.preparation.as_ref().and_then(|s|s.error.as_deref());
                let explanation=self.status.as_ref().map(|s|s.explanation.as_str()).filter(|s|!s.is_empty());
                if !self.details.is_empty()||provider_error.is_some()||explanation.is_some(){
                    ui.collapsing(uu_i18n::text("诊断详情"),|ui|{
                        if !self.details.is_empty(){ui.monospace(&self.details);}
                        if let Some(error)=provider_error{if error!=self.details{ui.monospace(error);}}
                        if let Some(explanation)=explanation{ui.monospace(explanation);}
                    });
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
                if matches!(command, Request::Create { .. }) {
                    self.create_requested = true;
                } else {
                    self.remote(client, command);
                }
            } else {
                self.local(ctx, command.into());
            }
        }
        if let Some(id) = prepare {
            self.local(ctx, LocalRequest::PrepareSlot { id });
        }
        if let Some(action) = preparation_action {
            match action {
                preparation_ui::Action::Prepare => self.local(ctx, LocalRequest::Prepare),
                preparation_ui::Action::Recover => self.local(ctx, LocalRequest::Recover),
                preparation_ui::Action::BackgroundSetup => self.request_background_setup(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn preparation(
        phase: uu_core::virtual_display::preparation::Phase,
        queued: bool,
    ) -> uu_core::virtual_display::preparation::Status {
        uu_core::virtual_display::preparation::Status {
            phase,
            configured_capacity: Some(16),
            targets: 1,
            available: 0,
            requested_capacity: 16,
            queued,
            error: None,
        }
    }
    #[test]
    fn local_sixty_second_notice_keeps_the_worker_and_accepts_a_late_snapshot() {
        let (send, receive) = mpsc::channel();
        let mut state = State {
            local_owner: Some(LocalOwner::Background),
            worker_owner: Some(LocalOwner::Background),
            worker: Some(receive),
            worker_started: Some(Instant::now() - Duration::from_secs(61)),
            worker_preparation: true,
            ..Default::default()
        };
        state.poll(None);
        assert!(state.worker.is_some() && state.worker_timed_out);
        assert!(state.take_notice().is_some());
        state.poll(None);
        assert!(state.take_notice().is_none());
        send.send(Ok(snapshot(vec![]))).unwrap();
        state.poll(None);
        assert!(!state.busy() && state.worker_started.is_none() && !state.worker_timed_out);
        assert_eq!(state.status.as_ref().unwrap().available_slots, 1);
    }
    #[test]
    fn preparation_polling_requires_a_visible_accepted_intent_and_never_overlaps_a_request() {
        use uu_core::virtual_display::preparation::Phase;
        let mut state = State {
            open: true,
            preparation: Some(preparation(Phase::WaitingForIdle, false)),
            next_preparation_check: Some(Instant::now() - Duration::from_secs(1)),
            ..Default::default()
        };
        assert!(
            !state.preparation_refresh_due(Instant::now()),
            "a read-only busy state is not an accepted request"
        );
        state.preparation.as_mut().unwrap().queued = true;
        assert!(state.preparation_refresh_due(Instant::now()));
        state.pending = Some((7, Instant::now()));
        assert!(!state.preparation_refresh_due(Instant::now()));
        state.pending = None;
        state.open = false;
        assert!(!state.preparation_refresh_due(Instant::now()));
        state.open = true;
        state.preparation.as_mut().unwrap().phase = Phase::RecoveryPending;
        assert!(
            !state.preparation_refresh_due(Instant::now()),
            "recovery is explicit, never a polling side effect"
        );
    }
    #[test]
    fn background_setup_is_an_intent_and_keeps_foreground_lease_ownership() {
        let display = display();
        let mut state = State {
            open: true,
            local_owner: Some(LocalOwner::Foreground),
            ..Default::default()
        };
        state.foreground_displays.insert(display.id);
        state.request_background_setup();
        assert!(!state.open && state.worker.is_none() && state.owns_foreground_displays());
        assert!(state.take_background_setup_request());
        assert!(!state.take_background_setup_request());
        state.pending = Some((9, Instant::now()));
        state.request_background_setup();
        assert!(
            !state.take_background_setup_request(),
            "submitted work keeps its current owner"
        );
    }
    #[test]
    fn provider_failure_is_kept_in_details_without_becoming_the_main_notice() {
        use uu_core::virtual_display::preparation::Phase;
        let raw = "generated_backend_failure_012345";
        let mut prep = preparation(Phase::Failed, false);
        prep.error = Some(raw.into());
        let error = local_error(&uu_core::virtual_display::Error::Preparation(prep.clone()).into());
        assert!(!error.message.contains(raw));
        assert!(error.details.contains(raw));
        assert_eq!(error.preparation, Some(prep));
    }
    #[test]
    fn background_response_errors_require_that_same_owners_snapshot_before_another_mutation() {
        use uu_core::virtual_display::preparation::Phase;
        let mut state = State {
            local_owner: Some(LocalOwner::Background),
            ..Default::default()
        };
        state.receive(snapshot(vec![]), Some(LocalOwner::Background));
        state.preparation = Some(preparation(Phase::Ready, false));
        state.receive(
            Response::Error {
                message: "generated background recovery is pending".into(),
            },
            Some(LocalOwner::Background),
        );
        assert!(
            state.local_mutations_blocked()
                && state.status.is_none()
                && state.preparation.is_none()
        );
        assert!(state.details.contains("generated background recovery"));
        let ctx = egui::Context::default();
        for request in [
            LocalRequest::Prepare,
            Request::Create {
                spec: Spec::default(),
            }
            .into(),
        ] {
            state.local(&ctx, request);
            assert!(state.worker.is_none());
        }
        state.receive(snapshot(vec![]), Some(LocalOwner::Foreground));
        assert!(
            state.local_mutations_blocked(),
            "another host's inventory cannot settle this operation"
        );
        state.receive(snapshot(vec![]), Some(LocalOwner::Background));
        assert!(!state.local_mutations_blocked());
    }
    #[test]
    fn remote_errors_invalidate_old_ready_inventory_until_one_read_only_refresh_confirms_it() {
        use uu_core::virtual_display::preparation::Phase;
        let mut state = State::default();
        state.receive(snapshot(vec![]), None);
        state.preparation = Some(preparation(Phase::Ready, false));
        state.receive(
            Response::Error {
                message: "generated host recovery is pending".into(),
            },
            None,
        );
        assert!(
            state.remote_inventory_uncertain
                && state.status.is_none()
                && state.preparation.is_none()
        );
        state.remote(
            &Client::default(),
            Request::Create {
                spec: Spec::default(),
            },
        );
        assert!(
            state.pending.is_none(),
            "old capacity never admits a new mutation after an error"
        );
        assert!(state.take_remote_refresh_request());
        assert!(!state.take_remote_refresh_request());
        state.receive(
            Response::Error {
                message: "generated inspect failure".into(),
            },
            None,
        );
        assert!(
            !state.take_remote_refresh_request(),
            "a failed Inspect must not create a retry loop"
        );
        state.receive(snapshot(vec![]), None);
        assert!(!state.remote_inventory_uncertain);
    }
    #[test]
    fn virtual_view_startup_waits_for_management_and_never_replays_submitted_creation() {
        let mut state = State::default();
        let spec = Spec {
            width: 2560,
            ..Default::default()
        };
        state.prepare_startup(spec);
        state.take_notice();
        state.poll(None);
        assert!(matches!(state.startup, Some(Startup::Waiting(_))));
        assert!(state.pending.is_none());
        // SDP owns the sole preparation request. A UI timeout cannot replay it
        // through the later management channel or discard a late reply.
        state.startup = Some(Startup::Waiting(
            Instant::now()
                - uu_core::native_virtual_display::OPERATION_TIMEOUT
                - Duration::from_secs(1),
        ));
        state.poll(None);
        state.poll(None);
        assert!(state.pending.is_none() && state.initial_timed_out);
        assert!(matches!(state.startup, Some(Startup::Waiting(_))));
        let mut created = display();
        created.owner = Owner::Session(uuid::Uuid::new_v4());
        state.receive(Response::Created { display: created }, None);
        assert!(matches!(state.startup, Some(Startup::Finished)));
        state.source.as_mut().unwrap().1 = Instant::now() - Duration::from_secs(13);
        state.poll(None);
        assert!(
            state.source.is_some(),
            "late exact catalogs can still bind this owned source"
        );
        state.take_notice();
        state.poll(None);
        assert!(state.take_notice().is_none());
        state.reset_remote();
        assert!(state.startup.is_none() && state.source.is_none());
        state.prepare_startup(spec);
        assert!(
            matches!(state.startup, Some(Startup::Waiting(_))),
            "only a new controller admits a new startup"
        );
    }
    #[test]
    fn unconfirmed_remote_work_blocks_retries_until_reset_and_new_connections_drop_old_selection() {
        let mut state = State::default();
        state.pending = Some((
            1,
            Instant::now()
                - uu_core::native_virtual_display::OPERATION_TIMEOUT
                - Duration::from_secs(1),
        ));
        state.poll(None);
        assert_eq!(state.pending.map(|pending| pending.0), Some(1));
        assert!(state.busy() && state.pending_timed_out);
        assert!(!state.notice.is_empty());
        assert!(state.take_notice().is_some());
        state.poll(None);
        assert!(
            state.take_notice().is_none(),
            "timeout is reported once while awaiting the same operation"
        );
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
        assert!(
            state.worker.is_none(),
            "opening the tray selector cannot silently query the foreground host"
        );
    }

    fn display() -> Display {
        Display {
            id: uu_core::virtual_display::LeaseId(uuid::Uuid::new_v4()),
            owner: Owner::Manual,
            spec: Spec::default(),
            actual: None,
            name: "Generated virtual display".into(),
            source: "generated-source".into(),
            active: true,
            cleanup_pending: false,
            last_error: None,
        }
    }
    fn snapshot(displays: Vec<Display>) -> Response {
        Response::Snapshot {
            status: Status {
                supported: true,
                driver_present: true,
                available_slots: 1,
                leased: displays.len(),
                explanation: String::new(),
                install_url: String::new(),
                preparation: None,
            },
            displays,
            slots: vec![],
        }
    }
    #[test]
    fn changing_management_owner_clears_visible_inventory_but_preserves_foreground_ownership() {
        let foreground = display();
        let background = display();
        let mut state = State::default();
        assert!(state.select_local_owner(LocalOwner::Foreground));
        state.receive(
            snapshot(vec![foreground.clone()]),
            Some(LocalOwner::Foreground),
        );
        assert!(state.owns_foreground_displays());
        assert!(state.select_local_owner(LocalOwner::Background));
        assert!(state.status.is_none() && state.displays.is_empty() && state.slots.is_empty());
        state.receive(
            snapshot(vec![background.clone()]),
            Some(LocalOwner::Background),
        );
        assert_eq!(state.displays, vec![background]);
        state.receive(
            Response::Removed { id: foreground.id },
            Some(LocalOwner::Background),
        );
        assert!(
            state.owns_foreground_displays(),
            "a background reply cannot release a foreground lease"
        );
        state.receive(
            Response::Removed { id: foreground.id },
            Some(LocalOwner::Foreground),
        );
        assert!(!state.owns_foreground_displays());
    }
    #[test]
    fn closed_local_window_finishes_its_operation_and_controller_reset_keeps_only_lease_bookkeeping(
    ) {
        let foreground = display();
        let (send, receive) = mpsc::channel();
        let mut state = State {
            open: true,
            local_owner: Some(LocalOwner::Foreground),
            worker: Some(receive),
            worker_owner: Some(LocalOwner::Foreground),
            ..Default::default()
        };
        state.open = false;
        assert!(!state.select_local_owner(LocalOwner::Background));
        state.reset_remote();
        send.send(Ok(Response::Created {
            display: foreground.clone(),
        }))
        .unwrap();
        state.poll(None);
        assert!(
            state.owns_foreground_displays(),
            "late success still keeps the launcher cleanup/tray entry alive"
        );
        assert!(state.displays.is_empty() && state.status.is_none() && state.source.is_none());
        assert!(state.notice.is_empty());
        let (send, receive) = mpsc::channel();
        state.worker = Some(receive);
        state.worker_owner = Some(LocalOwner::Foreground);
        send.send(Err(local_error(&anyhow::anyhow!("old owner error"))))
            .unwrap();
        state.poll(None);
        assert!(
            state.notice.is_empty(),
            "a previous local scope cannot overwrite the remote notice"
        );
        let (send, receive) = mpsc::channel();
        state.worker = Some(receive);
        state.worker_owner = Some(LocalOwner::Foreground);
        drop(send);
        state.poll(None);
        assert!(
            state.notice.is_empty(),
            "an old worker disconnect cannot overwrite the remote notice"
        );
    }
    #[test]
    fn submitted_unknown_requires_a_successful_inspect_of_that_same_owner_before_mutating_again() {
        let (send, receive) = mpsc::channel();
        let mut state = State {
            local_owner: Some(LocalOwner::Foreground),
            worker: Some(receive),
            worker_owner: Some(LocalOwner::Foreground),
            ..Default::default()
        };
        let error = anyhow::Error::new(uu_core::service_admin::SubmittedUnknown(
            "generated lost reply".into(),
        ))
        .context("generated transport context");
        send.send(Err(local_error(&error))).unwrap();
        state.poll(None);
        assert!(
            !state.busy(),
            "Inspect must remain usable after an unknown result"
        );
        assert!(state.local_mutations_blocked());
        let ctx = egui::Context::default();
        for request in [
            Request::Create {
                spec: Spec::default(),
            },
            Request::Remove { id: display().id },
        ] {
            state.local(&ctx, request.into());
            assert!(state.worker.is_none());
        }
        state.local(
            &ctx,
            LocalRequest::PrepareSlot {
                id: "generated-slot".into(),
            },
        );
        assert!(state.worker.is_none());
        assert!(state.select_local_owner(LocalOwner::Background));
        assert!(
            !state.local_mutations_blocked(),
            "other owners have independent mutation gates"
        );
        state.receive(snapshot(vec![]), Some(LocalOwner::Background));
        assert!(state.select_local_owner(LocalOwner::Foreground));
        assert!(
            state.local_mutations_blocked(),
            "an unrelated owner's Inspect cannot confirm the previous operation"
        );
        state.receive(
            Response::Error {
                message: "inspection failed".into(),
            },
            Some(LocalOwner::Foreground),
        );
        assert!(state.local_mutations_blocked());
        state.reset_remote();
        assert!(state.select_local_owner(LocalOwner::Foreground));
        assert!(
            state.local_mutations_blocked(),
            "closing or resetting UI cannot forget an unconfirmed local operation"
        );
        state.receive(snapshot(vec![]), Some(LocalOwner::Foreground));
        assert!(!state.local_mutations_blocked());
    }
    #[test]
    fn failed_foreground_create_with_pending_cleanup_keeps_its_tray_and_shutdown_ownership() {
        let id = display().id;
        let (send, receive) = mpsc::channel();
        let mut state = State {
            local_owner: Some(LocalOwner::Foreground),
            worker: Some(receive),
            worker_owner: Some(LocalOwner::Foreground),
            ..Default::default()
        };
        let error = anyhow::Error::new(uu_core::virtual_display::Error::CleanupPending {
            id,
            message: "generated rollback failure".into(),
        });
        send.send(Err(local_error(&error))).unwrap();
        state.reset_remote();
        state.poll(None);
        assert!(
            state.owns_foreground_displays(),
            "a failed Create can still own a target that needs cleanup"
        );
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
            .context(uu_i18n::text("无法读取后台虚拟副屏管理响应"))?
        {
            uu_core::service_admin::Response::VirtualDisplay { response } => Ok(response),
            uu_core::service_admin::Response::Error { message } => anyhow::bail!(message),
            _ => anyhow::bail!(
                "{}",
                uu_i18n::text("后台返回了不匹配的管理响应，请在管理员窗口查看运行版本")
            ),
        }
    }
    #[cfg(not(windows))]
    anyhow::bail!("{}", uu_i18n::text("虚拟副屏仅支持 Windows"))
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
        if self.virtual_display.remote_inventory_uncertain {
            self.virtual_display.open = true;
            self.displays.notice = uu_i18n::text("虚拟副屏操作未完成，请刷新确认当前状态。").into();
            return;
        }
        if !self
            .controller
            .as_ref()
            .is_some_and(|handle| handle.stats.native_displays.initial_supported())
        {
            self.displays.notice =
                uu_i18n::text("远端未提供虚拟副屏管理，请更新后台并授予管理服务权限").into();
            return;
        }
        self.release_all_keys();
        self.stream_menu.open = false;
        self.virtual_display.open = false;
        if let Some(tabs) = &self.workspace_tabs {
            tabs.actions
                .borrow_mut()
                .push(crate::stream_workspace::Action::CreateDisplay(tabs.active));
            return;
        }
        // The uncommon standalone entry uses the same launch ownership rule.
        if let Some(origin) = self.stream_target.as_ref() {
            let target = crate::sessions::StreamTarget {
                connection: crate::sessions::ConnectionTarget::NativeVirtual {
                    connection: Box::new(origin.connection.base().clone()),
                    virtual_display: crate::sessions::VirtualDisplayTarget::new(
                        self.virtual_display.spec(),
                    ),
                },
                alias: origin.alias.clone(),
                view_only: origin.view_only,
            };
            let result = std::env::current_exe()
                .map_err(anyhow::Error::from)
                .and_then(|exe| {
                    self.sessions
                        .open(&exe, &target, self.platform.origin())
                        .map_err(anyhow::Error::from)
                });
            if let Err(error) = result {
                self.displays.notice = error.to_string();
            }
        }
    }
    pub(super) fn poll_virtual_display_state(&mut self) {
        if self.disconnecting || self.exit_requested {
            return;
        }
        self.virtual_display.fixed_source =
            if self.virtual_display.open && self.is_fixed_display_view() {
                self.visible_remote_screens()
                    .selected()
                    .map(|screen| screen.display_name.clone())
            } else {
                None
            };
        let stats = self.controller.as_ref().map(|handle| handle.stats.clone());
        self.virtual_display.remote_create_supported = stats
            .as_ref()
            .is_some_and(|stats| stats.native_displays.initial_supported());
        if let Some(result) = stats
            .as_ref()
            .and_then(|stats| stats.native_displays.take_initial_result())
        {
            match result {
                Ok(reply) => {
                    if let Some(display) = reply.virtual_display {
                        self.virtual_display
                            .receive(Response::Created { display }, None);
                    }
                }
                Err(message) => self.virtual_display.startup_failed(message),
            }
        }
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
                let source = source.clone();
                self.virtual_display.source = None;
                if self.displays.awaiting_virtual() {
                    self.displays
                        .bind_created_virtual(crate::sessions::DisplayTarget { id, source });
                } else {
                    self.select_remote_screen(id);
                }
            }
        }
        if self.stream_target.is_some() {
            if let Some(notice) = self.virtual_display.take_notice() {
                self.displays.notice = notice;
            }
        }
    }
    pub(super) fn poll_virtual_displays(&mut self, ctx: &egui::Context) {
        self.poll_virtual_display_state();
        let stats = self.controller.as_ref().map(|handle| handle.stats.clone());
        self.virtual_display
            .show(ctx, stats.as_ref().map(|stats| &stats.virtual_displays));
        if self.virtual_display.take_background_setup_request() {
            self.select_workspace_page(crate::workspace_ui::Page::Services);
        }
        if self.virtual_display.take_create_request() {
            self.create_remote_virtual_display();
        }
    }
}
