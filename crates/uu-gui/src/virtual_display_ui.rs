//! Explicit local management and per-stream temporary display controls.
#[path = "virtual_display_preparation.rs"]
mod preparation_ui;
use eframe::egui;
use std::{
    collections::{BTreeMap,BTreeSet},
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
                "临时屏随当前进程退出清理；持续保存的屏幕会保留，并在下次启动时恢复。",
            ),
            Self::Background => uu_i18n::text(
                "由后台服务管理；持续保存的屏幕会保留，并在后台开机启动时恢复。",
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
    {let _=owner;return false;}
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
#[derive(Clone,PartialEq,Eq)]
struct DisplayEditor { id:uu_core::virtual_display::LeaseId, name:String, spec:Spec }
impl From<&Display> for DisplayEditor {
    fn from(display:&Display)->Self {Self{id:display.id,name:display.name.clone(),spec:display.spec}}
}

#[derive(Default)]
pub struct State {
    pub open: bool,
    local_owner: Option<LocalOwner>,
    local_initial_refresh: bool,
    local_selector: bool,
    foreground_displays: BTreeSet<uu_core::virtual_display::LeaseId>,
    spec: Spec,
    persistent_new:Option<bool>,
    status: Option<Status>,
    displays: Vec<Display>,
    editors: BTreeMap<uu_core::virtual_display::LeaseId,DisplayEditor>,
    submitted_editor: Option<uu_core::virtual_display::LeaseId>,
    limit_draft: Option<u16>,
    next_inventory_check: Option<Instant>,
    slots: Vec<Slot>,
    worker: Option<mpsc::Receiver<Result<Response, LocalFailure>>>,
    worker_owner: Option<LocalOwner>,
    worker_started: Option<Instant>,
    worker_timed_out: bool,
    worker_preparation: bool,
    inspect_inflight: bool,
    deferred_request: Option<LocalRequest>,
    uncertain_owners: BTreeSet<LocalOwner>,
    pending: Option<(u64, Instant)>,
    pending_timed_out: bool,
    pub source: Option<(String, Instant)>,
    source_timed_out: bool,
    notice: String,
    notice_changed: bool,
    details_open_requested: bool,
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
    #[cfg(test)]
    pub(super) fn generated_preview()->Self {
        let displays=[("办公副屏",true,Spec::default()),("游戏副屏",false,Spec{width:2560,height:1440,refresh_millihz:120_000,hdr:true})]
            .into_iter().enumerate().map(|(index,(name,persistent,spec))|Display {
                id:uu_core::virtual_display::LeaseId(uuid::Uuid::from_u128(index as u128+1)),owner:Owner::Manual,
                spec,actual:Some(spec),name:name.into(),source:format!("generated-source-{index}"),active:true,persistent,cleanup_pending:false,last_error:None,
            }).collect();
        let mut state=Self{local_owner:Some(LocalOwner::Background),..Default::default()};
        state.receive(Response::Snapshot {
            status:Status{supported:true,driver_present:true,available_slots:14,leased:2,allocated:2,keep_after_stream:false,limit:2,supports_lifetimes:true,supports_configuration:true,explanation:String::new(),install_url:String::new(),preparation:None},
            displays,slots:Vec::new(),
        },Some(LocalOwner::Background));
        state
    }
    #[cfg(test)]
    pub(super) fn show_generated_preview(&mut self,ui:&mut egui::Ui) {
        // The isolated visual probe never polls or calls a real service.
        self.contents(ui,&ui.ctx().clone(),None,true);
    }
    pub(super) fn new_is_persistent(&self)->bool {self.persistent_new.unwrap_or(false)}
    pub(super) fn spec(&self) -> Spec {
        self.spec
    }
    pub(super) fn prepare_startup(&mut self, spec: Spec) {
        self.prepare_startup_with_lifetime(spec,false);
    }
    pub(super) fn prepare_startup_with_lifetime(&mut self, spec: Spec, persistent:bool) {
        self.spec = spec;
        self.startup = Some(Startup::Waiting(Instant::now()));
        self.initial_timed_out = false;
        self.message(uu_i18n::text(if persistent {"正在创建持久虚拟显示器…"}else{"正在创建临时虚拟副屏…"}).into());
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
        self.local_initial_refresh = false;
        self.pending = None;
        self.deferred_request = None;
        self.pending_timed_out = false;
        self.source = None;
        self.source_timed_out = false;
        self.status = None;
        self.displays.clear();
        self.editors.clear();self.submitted_editor=None;self.limit_draft=None;self.next_inventory_check=None;
        self.slots.clear();
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
            #[cfg(windows)] {self.local_selector=false;self.select_local_owner(LocalOwner::Background);self.local_initial_refresh=true;}
            #[cfg(not(windows))] {self.local_selector = true;}
        }
    }
    fn select_local_owner(&mut self, owner: LocalOwner) -> bool {
        if self.busy() {
            return false;
        }
        if self.local_owner != Some(owner) {
            self.deferred_request = None;
            self.status = None;
            self.displays.clear();
            self.editors.clear();self.submitted_editor=None;self.limit_draft=None;self.next_inventory_check=None;
            self.slots.clear();
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
            if self.inspect_inflight && !matches!(&request, LocalRequest::Manage { request: Request::Inspect }) {
                self.deferred_request.get_or_insert(request);
            }
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
        self.inspect_inflight=matches!(&request,LocalRequest::Manage{request:Request::Inspect});
        if !self.inspect_inflight {
            self.details.clear();
            self.message(
                if preparing {
                    uu_i18n::text("正在准备虚拟副屏，请等待本次操作完成。")
                } else {
                    uu_i18n::text("正在读取或更新虚拟副屏状态…")
                }.into(),
            );
        }
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
        if self.busy() && self.inspect_inflight && !matches!(&request, Request::Inspect) {
            self.deferred_request.get_or_insert(request.into());
            return;
        }
        if self.busy() || (self.remote_inventory_uncertain && !matches!(&request, Request::Inspect))
        {
            return;
        }
        let inspect = matches!(&request, Request::Inspect);
        match client.request(request) {
            Ok(id) => {
                self.pending = Some((id, Instant::now()));
                self.inspect_inflight=inspect;
                self.pending_timed_out = false;
                if !inspect {
                    self.remote_refresh_attempted = false;
                    self.details.clear();
                    self.message(uu_i18n::text("正在管理虚拟副屏…").into());
                }
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
                if self.preparation.as_ref().is_some_and(|s| s.error.is_some()) && !self.inspect_inflight {
                    self.details_open_requested = true;
                }
                self.next_preparation_check = self
                    .preparation
                    .as_ref()
                    .filter(|s| s.queued && preparation_ui::active(s))
                    .map(|_| Instant::now() + Duration::from_secs(1));
                if self.limit_draft==Some(status.limit) {self.limit_draft=None;}
                self.status = Some(status);
                if !self.inspect_inflight {
                    if let Some(id)=self.submitted_editor.take() {
                        if self.editors.get(&id).is_some_and(|draft|displays.iter().any(|display|display.id==draft.id && display.spec==draft.spec && (draft.name.trim().is_empty() || display.name==draft.name.trim()))) {
                            self.editors.remove(&id);
                        }
                    }
                }
                self.editors.retain(|id,_|displays.iter().any(|display|display.id==*id));
                self.displays = displays;
                self.slots = slots;
                self.next_inventory_check=Some(Instant::now()+Duration::from_secs(2));
                if !self.inspect_inflight {
                    self.message(String::new());
                    self.details.clear();
                }
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
                self.next_inventory_check=Some(Instant::now());
            }
            Response::Removed { id } => {
                self.displays.retain(|d| d.id != id);
                self.editors.remove(&id);
                self.status = None;
                self.preparation = None;
                self.next_preparation_check = None;
                self.details.clear();
                self.message(uu_i18n::text("虚拟副屏已移除").into());
                self.next_inventory_check=Some(Instant::now());
            }
            Response::Error { message } => {
                self.submitted_editor=None;
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
                self.details_open_requested = true;
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
                                self.details_open_requested = true;
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
                    self.inspect_inflight=false;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.worker = None;
                    self.inspect_inflight=false;
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
                self.inspect_inflight=false;
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
    fn prepare_interaction(&mut self, ctx: &egui::Context, client: Option<&Client>) {
        if !self.busy() {
            if let Some(request) = self.deferred_request.take() {
                if let Some(client) = client {
                    if let LocalRequest::Manage { request } = request { self.remote(client, request); }
                } else { self.local(ctx, request); }
            }
        }
        if client.is_none() && self.local_initial_refresh && !self.busy() {
            self.local_initial_refresh=false;
            self.local(ctx,Request::Inspect.into());
        }
        if !self.busy() && self.next_inventory_check.is_some_and(|at|Instant::now()>=at) {
            self.next_inventory_check=None;
            if let Some(client)=client {self.remote(client,Request::Inspect);}
            else {self.local(ctx,Request::Inspect.into());}
        }
        if let Some(at)=self.next_inventory_check {ctx.request_repaint_after(at.saturating_duration_since(Instant::now()));}
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
    }
    pub fn show_background_inline(&mut self, ui: &mut egui::Ui) {
        self.open = true;
        if self.local_owner != Some(LocalOwner::Background) {
            if !self.select_local_owner(LocalOwner::Background) {
                return;
            }
            self.local_initial_refresh = true;
        }
        let ctx = ui.ctx().clone();
        self.prepare_interaction(&ctx, None);
        self.contents(ui, &ctx, None, true);
    }
    pub fn show(&mut self, ctx: &egui::Context, client: Option<&Client>) {
        if !self.open {
            return;
        }
        self.prepare_interaction(ctx, client);
        let mut open = self.open;
        egui::Window::new(uu_i18n::text("虚拟副屏"))
            .id(egui::Id::new(("virtual-display-manager", ctx.viewport_id())))
            .open(&mut open)
            .collapsible(false)
            .default_width(480.)
            .show(ctx, |ui| self.contents(ui, ctx, client, false));
        self.open = open;
    }
    fn contents(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        client: Option<&Client>,
        embedded: bool,
    ) {
        let busy = (self.busy() && !self.inspect_inflight) || self.deferred_request.is_some();
        let can_edit=!busy;
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
        let mut command = None;
        let mut local_command = None;
        let mut selected_owner = None;
        let mut preparation_action = None;
        if client.is_none() && self.local_selector && !embedded {
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
            ui.strong(uu_i18n::text("虚拟显示器"));
            ui.weak(uu_i18n::text("临时屏在所属串流断开时回收；持久屏保留并在开机恢复。"));
        } else if let Some(owner) = self.local_owner {
            if embedded {
                ui.weak(uu_i18n::text("临时屏随所属串流或服务退出回收；持久屏保留并在开机恢复。"));
            } else {
                ui.strong(uu_i18n::format!("管理对象：{}", owner.label()));
                ui.weak(owner.lifetime());
            }
            if administrator_required(owner) {
                ui.label(uu_i18n::text("请在服务管理中准备后台，再创建虚拟副屏；已有副屏不会因打开设置而断开。"));
                if ui.add_enabled(!busy,crate::theme::secondary_button(uu_i18n::text("打开后台准备"))).clicked(){preparation_action=Some(preparation_ui::Action::BackgroundSetup);}
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
        if let Some(status)=&self.status {
            ui.horizontal_wrapped(|ui|{
                ui.label(uu_i18n::text("虚拟显示器上限"));
                let mut limit=self.limit_draft.unwrap_or(status.limit);
                ui.add_enabled_ui(can_edit,|ui| {
                    if crate::theme::number_input(ui,egui::DragValue::new(&mut limit).range(0..=uu_core::virtual_display::MAX_DISPLAY_LIMIT)).changed() {
                        self.limit_draft=(limit!=status.limit).then_some(limit);
                    }
                    if ui.add_enabled(self.limit_draft.is_some()&&!busy&&!mutations_blocked,crate::theme::primary_button(uu_i18n::text("保存上限"))).clicked() {
                        command=Some(Request::SetLimit{limit});
                    }
                });
                ui.weak(uu_i18n::format!("/ {} 个",uu_core::virtual_display::MAX_DISPLAY_LIMIT));
            });
            ui.weak(uu_i18n::text("默认 2 个；临时和持久虚拟显示器共同计数，0 表示禁止新建。降低上限不会关闭已有显示器。"));
            let persistent=self.displays.iter().filter(|display|display.persistent).count();
            let temporary=self.displays.len()-persistent;
            let available=status.available_slots;
            let allocated=status.allocation_count();let limit=status.limit;
            let remaining=available.min(usize::from(limit).saturating_sub(allocated));
            ui.horizontal_wrapped(|ui|{
                ui.weak(uu_i18n::format!("已创建：{allocated} / {limit}"));
                ui.weak(uu_i18n::format!("持久：{persistent} · 临时：{temporary}"));
                ui.weak(uu_i18n::format!("可新建：{remaining}"));
            });
        } else {
            ui.horizontal(|ui| {ui.label(uu_i18n::text("虚拟显示器上限"));ui.weak("—");});
        }
        ui.add_space(8.);
        egui::CollapsingHeader::new(uu_i18n::text("创建设置")).id_salt("virtual-display-create").default_open(self.displays.is_empty()).show(ui,|ui| {
            ui.add_enabled_ui(can_edit,|ui| {
                egui::ScrollArea::horizontal().id_salt("virtual-display-create-row").show(ui,|ui|ui.horizontal(|ui| {
                    crate::virtual_display_presets::display_spec_row(ui,&mut self.spec);
                    if client.is_none() || self.status.as_ref().is_some_and(|s|s.supports_lifetimes) {
                        crate::theme::compact_switch(ui,self.persistent_new.get_or_insert(client.is_none()),uu_i18n::text("持久保存，开机恢复"));
                    }
                }));
            });
        });
        ui.horizontal_wrapped(|ui| {
            let can_create=!busy && !mutations_blocked && !background_unavailable && (client.is_none() || self.remote_create_supported)
                && self.status.as_ref().is_some_and(|s|s.supported && (!s.supports_lifetimes||s.allocation_count()<usize::from(s.limit)))
                && preparation_ui::can_create(self.preparation.as_ref(),self.status.as_ref().map_or(0,|s|s.available_slots),self.local_owner==Some(LocalOwner::Foreground)&&client.is_none());
            if ui.add_enabled(can_create,crate::theme::primary_button(uu_i18n::text("新建虚拟显示器"))).clicked() {
                command=Some(Request::Create{spec:self.spec});
            }
            ui.weak(uu_i18n::text(if self.persistent_new.unwrap_or(client.is_none()) {"持久虚拟显示器"}else{"临时虚拟显示器"}));
            if ui.add_enabled(!busy,crate::theme::secondary_button(uu_i18n::text("刷新"))).clicked() {command=Some(Request::Inspect);}
            if busy {ui.spinner();ctx.request_repaint_after(Duration::from_millis(100));}
        });
        if let Some(preparation)=&self.preparation {
            if self.status.as_ref().is_some_and(|s|s.available_slots>0)
                && preparation_ui::can_create(Some(preparation),1,false)
            {ui.weak(uu_i18n::text("已准备好空闲虚拟屏，可以创建；移除仅断开所属目标的桌面输出。"));}
            else {ui.weak(uu_core::native_virtual_display::preparation_message(preparation));}
            if preparation_ui::active(preparation) {
                ui.weak(uu_i18n::text("已登记的准备会在空闲时继续；无需重复点击，也不会中断正在使用的副屏。"));
                if let Some(at)=self.next_preparation_check {ctx.request_repaint_after(at.saturating_duration_since(Instant::now()));}
            }
            if client.is_none() {
                if let Some(action)=preparation_ui::action(preparation,self.local_owner==Some(LocalOwner::Foreground)) {
                    let label=match action {
                        preparation_ui::Action::Prepare=>if embedded {uu_i18n::text("准备虚拟显示器")}else{uu_i18n::text("准备虚拟副屏")},
                        preparation_ui::Action::Recover=>uu_i18n::text("修复未完成的驱动操作"),
                        preparation_ui::Action::BackgroundSetup=>if embedded {uu_i18n::text("准备虚拟显示器")}else{uu_i18n::text("打开后台准备")},
                    };
                    if ui.add_enabled(!busy&&!mutations_blocked,crate::theme::secondary_button(label)).clicked(){preparation_action=Some(action);}
                    if action==preparation_ui::Action::Recover {
                        ui.weak(uu_i18n::text("核对上次日志、驱动、配置及显示状态，确认操作已完成或撤销本次未完成变更；驱动已卸载时核验后封存旧记录。不会重装驱动、新建屏幕或恢复历史桌面布局。"));
                    } else if action==preparation_ui::Action::Prepare {
                        ui.weak(uu_i18n::text("准备驱动容量以当前创建上限为准；新建一次只激活一块屏幕。首次安装会短暂枚举驱动目标。"));
                    }
                }
            }
        } else if let Some(status) = &self.status {
            ui.weak(if !status.supported {
                uu_i18n::text("虚拟副屏仅支持 Windows")
            } else if !status.driver_present {
                uu_i18n::text("源机尚未安装支持的签名虚拟副屏驱动")
            } else if status.supports_lifetimes&&status.allocation_count()>=usize::from(status.limit) {
                uu_i18n::text("已达到创建上限；提高上限或删除已有显示器后可继续创建。")
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
                let label = if embedded {
                    uu_i18n::text("准备虚拟显示器")
                } else {
                    uu_i18n::text("打开后台准备")
                };
                if ui.add_enabled(!busy && !mutations_blocked, crate::theme::secondary_button(label))
                    .on_hover_text(uu_i18n::text("准备已安装的虚拟显示设备，完成后刷新当前管理对象的列表。"))
                    .clicked() {preparation_action=Some(if embedded {preparation_ui::Action::Prepare} else {preparation_ui::Action::BackgroundSetup});}
            }
            if client.is_none()&&!status.driver_present {
                ui.weak(if embedded {
                    uu_i18n::text("更新或启用服务后，可自动准备虚拟显示器。")
                } else {
                    uu_i18n::text("更新或启用源机后台后，可自动准备虚拟副屏。")
                });
                let label = if embedded {
                    uu_i18n::text("准备虚拟显示器")
                } else {
                    uu_i18n::text("打开后台准备")
                };
                if ui.add_enabled(!busy&&!mutations_blocked,crate::theme::secondary_button(label)).clicked(){preparation_action=Some(if embedded {preparation_ui::Action::Prepare} else {preparation_ui::Action::BackgroundSetup});}
            }
        }
        if background_unavailable {
            ui.weak(if embedded {
                uu_i18n::text("请先准备虚拟显示器驱动；已有显示器不会因打开设置而断开。")
            } else {
                uu_i18n::text("请在服务管理中准备后台，再创建虚拟副屏；已有副屏不会因打开设置而断开。")
            });
            let label = if embedded {
                uu_i18n::text("准备虚拟显示器")
            } else {
                uu_i18n::text("打开后台准备")
            };
            if ui.add_enabled(!busy,crate::theme::secondary_button(label)).clicked(){preparation_action=Some(if embedded {preparation_ui::Action::Prepare} else {preparation_ui::Action::BackgroundSetup});}
        }
        ui.separator();
        ui.strong(uu_i18n::text("已创建的显示器"));
        if self.displays.is_empty() {
            ui.weak(uu_i18n::text("还没有由 Eclipse 创建的虚拟显示器。可以在上方创建持久或临时显示器。"));
        }
        let displays=self.displays.clone();
        let mut save_edit=None;
        let mut list = egui::ScrollArea::vertical();
        if !embedded {
            list = list.max_height(300.);
        }
        let client_is_none=client.is_none();
        let supports_lifetimes=self.status.as_ref().is_some_and(|s|s.supports_lifetimes);
        let supports_configuration=self.status.as_ref().is_some_and(|s|s.supports_configuration);
        let row_width=ui.available_width().max(1000.);
        egui::ScrollArea::horizontal().id_salt("virtual-display-property-rows").auto_shrink([false,true]).show(ui,|ui| {
          ui.set_min_width(row_width);
          list.show(ui, |ui| {
            for display in &displays {
                let fixed_current=client.is_some() && self.fixed_source.as_ref()==Some(&display.source);
                let original=DisplayEditor::from(display);
                let mut draft=self.editors.get(&display.id).cloned().unwrap_or_else(||original.clone());
                let mut cancel=false;
                ui.push_id(display.id.0,|ui|{
                    egui::Frame::group(ui.style()).show(ui,|ui|{
                        ui.set_min_width((row_width-20.).max(0.));
                        ui.horizontal(|ui| {
                            ui.add_enabled_ui(can_edit&&supports_configuration,|ui| {
                                ui.label(uu_i18n::text("名称"));
                                ui.add(crate::theme::Input::new(&mut draft.name).desired_width(148.).char_limit(64));
                                crate::virtual_display_presets::display_spec_row(ui,&mut draft.spec);
                            });
                            let dirty=draft!=original;
                            let mut persistent=display.persistent;
                            if ui.add_enabled_ui(!busy&&!mutations_blocked&&(client_is_none||supports_lifetimes),|ui|crate::theme::compact_switch(ui,&mut persistent,uu_i18n::text("持久"))).inner.changed() {
                                if client.is_some(){command=Some(Request::SetPersistent{id:display.id,persistent});}
                                else {local_command=Some(LocalRequest::SetPersistent{id:display.id,persistent});}
                            }
                            if ui.add_enabled(dirty&&supports_configuration&&!busy&&!mutations_blocked,crate::theme::primary_button(uu_i18n::text("保存")))
                                .on_hover_text(uu_i18n::text("名称可立即修改；规格变更需先结束此主机的串流。")).clicked() {
                                save_edit=Some(draft.clone());
                            }
                            if dirty && ui.add_enabled(!busy,crate::theme::secondary_button(uu_i18n::text("还原修改"))).clicked() {cancel=true;}
                            if ui.add_enabled(!busy&&!mutations_blocked&&!fixed_current,crate::theme::secondary_button(if display.cleanup_pending {uu_i18n::text("重试移除")}else{uu_i18n::text("删除")}))
                                .on_disabled_hover_text(uu_i18n::text("关闭此串流标签即可断开并移除它的临时虚拟屏")).clicked() {
                                command=Some(Request::Remove{id:display.id});
                            }
                            let status=ui.weak(if display.active {uu_i18n::text("已启用")}else{uu_i18n::text("等待恢复")});
                            if let Some(actual)=display.actual {
                                status.on_hover_text(uu_i18n::format!("实际模式：{} × {} · {} Hz · {}",actual.width,actual.height,actual.refresh_millihz as f64/1000.,if actual.hdr {"HDR"}else{"SDR"}));
                            }
                        });
                        if !supports_configuration {
                            ui.weak(uu_i18n::text("当前后台尚不支持属性编辑，请更新并重启后台服务。"));
                        }
                        if let Some(error)=&display.last_error {
                            ui.collapsing(uu_i18n::text("诊断详情"),|ui|{ui.monospace(error);});
                        }
                    });
                });
                if cancel||draft==original {self.editors.remove(&display.id);}
                else {self.editors.insert(display.id,draft);}
            }
          });
        });
        if let Some(draft)=save_edit {
            self.submitted_editor=Some(draft.id);
            command=Some(Request::Configure{id:draft.id,name:draft.name,spec:draft.spec});
        }
        if !self.notice.is_empty() {
            ui.label(&self.notice);
        }
        let provider_error=self.preparation.as_ref().and_then(|s|s.error.as_deref());
        // Typed preparation states already have localized explanations above.
        // The generic legacy text may describe a different installed driver.
        let explanation=self.status.as_ref().filter(|s|s.preparation.is_none()).map(|s|s.explanation.as_str()).filter(|s|!s.is_empty());
        if !self.details.is_empty()||provider_error.is_some()||explanation.is_some(){
            egui::CollapsingHeader::new(uu_i18n::text("诊断详情"))
                .id_salt("virtual-display-details")
                .open(std::mem::take(&mut self.details_open_requested).then_some(true))
                .default_open(provider_error.is_some()||!self.details.is_empty()).show(ui,|ui|{
                if ui.add(crate::theme::secondary_button(uu_i18n::text("复制详情"))).clicked() {
                    let mut lines=vec![self.notice.as_str(),self.details.as_str()];
                    if let Some(error)=provider_error {if error!=self.details {lines.push(error);}}
                    if let Some(explanation)=explanation {lines.push(explanation);}
                    let report=lines.into_iter().filter(|line|!line.is_empty()).collect::<Vec<_>>().join("\n\n");
                    ui.output_mut(|output|output.copied_text=report);
                }
                if !self.details.is_empty(){ui.monospace(&self.details);}
                if let Some(error)=provider_error{if error!=self.details{ui.monospace(error);}}
                if let Some(explanation)=explanation{ui.monospace(explanation);}
            });
        }
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
                let command=match command {
                    Request::Create{spec}=>LocalRequest::CreateManual{spec,persistent:self.persistent_new.unwrap_or(true)},
                    Request::SetLimit{limit}=>LocalRequest::SetLimit{limit},
                    other=>other.into(),
                };
                self.local(ctx, command);
            }
        }
        if let Some(command)=local_command{self.local(ctx,command);}
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
        assert!(state.preparation_refresh_due(Instant::now()),"an explicitly queued recovery only polls status");
        state.preparation.as_mut().unwrap().queued=false;
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
    fn tray_entry_uses_the_service_on_windows_and_defers_io_until_rendering() {
        let mut state = State::default();
        state.open_local_selector();
        #[cfg(windows)] {
            assert!(state.open && !state.local_selector);
            assert_eq!(state.local_owner,Some(LocalOwner::Background));
            assert!(state.local_initial_refresh);
            assert!(state.worker.is_none(),"the callback selects the one service without doing IPC on the UI callback");
        }
        #[cfg(not(windows))] {
        assert!(state.open && state.local_selector);
        assert_eq!(state.local_owner, None);
        assert!(!state.busy());
        state.local(&egui::Context::default(), Request::Inspect.into());
        assert!(
            state.worker.is_none(),
            "opening the tray selector cannot silently query the foreground host"
        );
        }
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
            persistent:false,
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
                allocated: displays.len(),
                keep_after_stream:false,limit:uu_core::virtual_display::DEFAULT_DISPLAY_LIMIT,supports_lifetimes:true,supports_configuration:true,
                explanation: String::new(),
                install_url: String::new(),
                preparation: None,
            },
            displays,
            slots: vec![],
        }
    }
    #[test]
    fn embedded_service_manager_has_no_owner_selector_and_lists_displays() {
        let mut display = display();
        display.persistent = true;
        display.name = "Generated desk".into();
        let mut state = State {
            open: true,
            local_owner: Some(LocalOwner::Background),
            ..Default::default()
        };
        state.receive(snapshot(vec![display]), Some(LocalOwner::Background));
        let ctx = egui::Context::default();
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900., 1000.),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    state.show_background_inline(ui);
                });
            },
        );
        fn collect(shape: &egui::Shape, texts: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => texts.push(text.galley.text().into()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, texts);
                    }
                }
                _ => {}
            }
        }
        let mut texts = Vec::new();
        for shape in output.shapes {
            collect(&shape.shape, &mut texts);
        }
        assert!(texts.iter().any(|text| text == "新建虚拟显示器"));
        assert!(texts.iter().any(|text| text == "虚拟显示器上限"));
        assert!(texts.iter().any(|text| text == "已创建：1 / 2"));
        assert!(texts.iter().any(|text| text.contains("Generated desk")));
        assert!(!texts.iter().any(|text| text == "选择管理对象"));
        assert!(!texts.iter().any(|text| text.starts_with("管理对象：")));
    }
    #[test]
    fn inventory_refresh_preserves_each_display_draft_and_confirms_only_the_saved_one() {
        let first=display();let second=display();
        let mut state=State{local_owner:Some(LocalOwner::Background),..Default::default()};
        state.receive(snapshot(vec![first.clone(),second.clone()]),Some(LocalOwner::Background));
        for (item,name) in [(&first,"First edit"),(&second,"Second edit")] {
            state.editors.insert(item.id,DisplayEditor{name:name.into(),..DisplayEditor::from(item)});
        }
        state.limit_draft=Some(5);
        state.receive(snapshot(vec![first.clone(),second.clone()]),Some(LocalOwner::Background));
        assert_eq!(state.editors.len(),2);
        assert_eq!(state.limit_draft,Some(5));
        let mut applied=first.clone();applied.name="First edit".into();
        state.submitted_editor=Some(first.id);
        state.receive(snapshot(vec![applied,second.clone()]),Some(LocalOwner::Background));
        assert!(!state.editors.contains_key(&first.id));
        assert_eq!(state.editors[&second.id].name,"Second edit");
        let created=display();
        state.receive(Response::Created{display:created.clone()},Some(LocalOwner::Background));
        assert!(state.next_inventory_check.is_some_and(|at|at<=Instant::now()),"creation schedules a read-only inventory refresh");
        state.receive(Response::Removed{id:second.id},Some(LocalOwner::Background));
        assert!(!state.editors.contains_key(&second.id));
        assert!(state.next_inventory_check.is_some_and(|at|at<=Instant::now()),"deletion updates capacity automatically");
    }
    fn inventory_frame(state:&mut State,ctx:&egui::Context,events:Vec<egui::Event>,width:f32)->Vec<(String,egui::Rect)> {
        inventory_frame_output(state,ctx,events,width).0
    }
    fn inventory_frame_output(state:&mut State,ctx:&egui::Context,events:Vec<egui::Event>,width:f32)->(Vec<(String,egui::Rect)>,String) {
        let output=ctx.run(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,1200.))),events,..Default::default()},|ctx| {
            egui::CentralPanel::default().show(ctx,|ui| {
                let right=ui.max_rect().right();state.show_background_inline(ui);
                assert!(ui.min_rect().right()<=right+1.,"horizontal properties stay inside the page viewport");
            });
        });
        fn collect(shape:&egui::Shape,texts:&mut Vec<(String,egui::Rect)>) {
            match shape {
                egui::Shape::Text(text)=>texts.push((text.galley.text().into(),text.galley.rect.translate(text.pos.to_vec2()))),
                egui::Shape::Vec(shapes)=>for shape in shapes {collect(shape,texts);},
                _=>{},
            }
        }
        let mut texts=Vec::new();for shape in output.shapes {collect(&shape.shape,&mut texts);}(texts,output.platform_output.copied_text)
    }
    #[test]
    fn manager_only_lists_owned_displays_even_when_external_virtual_outputs_exist() {
        let mut state=State::generated_preview();
        state.slots.push(Slot{id:"external-fixture".into(),name:"External fixture display".into(),source:"external-source".into(),active:true,leased:false,actual:Some(Spec::default())});
        let ctx=egui::Context::default();
        let texts=inventory_frame(&mut state,&ctx,vec![],1280.);
        assert!(!texts.iter().any(|(text,_)|text=="External fixture display"||text==uu_i18n::text("系统现有的虚拟显示器")||text==uu_i18n::text("停用现有虚拟屏…")));
        assert_eq!(state.displays.len(),2);
    }
    #[test]
    fn copy_details_contains_the_retained_error_after_refresh() {
        let mut state=State{open:true,local_owner:Some(LocalOwner::Background),details:"Generated precise driver failure".into(),..Default::default()};
        let ctx=egui::Context::default();
        let texts=inventory_frame(&mut state,&ctx,vec![],1280.);
        let point=texts.iter().find(|(text,_)|text=="复制详情").unwrap().1.center();
        let button=|pressed|egui::Event::PointerButton{pos:point,button:egui::PointerButton::Primary,pressed,modifiers:egui::Modifiers::NONE};
        inventory_frame(&mut state,&ctx,vec![egui::Event::PointerMoved(point),button(true)],1280.);
        let (_,copied)=inventory_frame_output(&mut state,&ctx,vec![button(false)],1280.);
        assert_eq!(copied,"Generated precise driver failure");
    }
    #[test]
    fn inventory_refresh_keeps_operation_errors_and_page_geometry() {
        let mut state=State{open:true,local_owner:Some(LocalOwner::Background),..Default::default()};
        state.receive(snapshot(vec![display()]),Some(LocalOwner::Background));
        state.message("Generated operation failed".into());
        state.details="Generated exact driver failure".into();
        let ctx=egui::Context::default();
        let before=inventory_frame(&mut state,&ctx,vec![],1280.);
        let (send,receive)=mpsc::channel();
        state.worker=Some(receive);state.worker_owner=Some(LocalOwner::Background);state.inspect_inflight=true;
        let during=inventory_frame(&mut state,&ctx,vec![],1280.);
        send.send(Ok(snapshot(vec![display()]))).unwrap();
        state.poll(None);
        let after=inventory_frame(&mut state,&ctx,vec![],1280.);
        assert_eq!(state.notice,"Generated operation failed");
        assert_eq!(state.details,"Generated exact driver failure");
        for label in ["Generated operation failed","新建虚拟显示器","诊断详情"] {
            let rect=|texts:&Vec<(String,egui::Rect)>|texts.iter().find(|(s,_)|s==label).unwrap().1;
            assert_eq!(rect(&before),rect(&during),"refresh changed {label}");
            assert_eq!(rect(&before),rect(&after),"readback changed {label}");
        }
    }
    #[test]
    fn explicit_action_waits_for_inventory_read_without_losing_the_request() {
        let mut state=State{local_owner:Some(LocalOwner::Background),..Default::default()};
        let (_send,receive)=mpsc::channel();state.worker=Some(receive);state.inspect_inflight=true;
        state.local(&egui::Context::default(),Request::Create{spec:Spec::default()}.into());
        assert!(matches!(state.deferred_request,Some(LocalRequest::Manage{request:Request::Create{..}})));
        state.reset_remote();
        assert!(state.deferred_request.is_none(),"an action cannot follow a reset to another owner");
    }
    #[test]
    fn display_properties_are_visible_and_remain_editable_during_read_only_refresh() {
        for width in [360.,1280.] {
            let first=display();let mut second=display();second.name="Second generated screen".into();second.persistent=true;
            let mut state=State{local_owner:Some(LocalOwner::Background),..Default::default()};
            state.receive(snapshot(vec![first.clone(),second]),Some(LocalOwner::Background));
            // Keep a synthetic Inspect in flight. UI edits must still work;
            // no test action may reach the real local management service.
            let (_send,receive)=mpsc::channel();state.worker=Some(receive);state.inspect_inflight=true;
            let ctx=egui::Context::default();
            let texts=inventory_frame(&mut state,&ctx,vec![],width);
            let labels=["名称","分辨率","刷新率","HDR","持久","保存","删除"];
            if width>=1000. {
                for label in labels {
                    assert_eq!(texts.iter().filter(|(text,_)|text==label).count(),2,"{label} at {width}: {texts:?}");
                }
                for row in 0..2 {
                    let centers:Vec<_>=labels.iter().map(|label|texts.iter().filter(|(text,_)|text==label).nth(row).unwrap().1.center().y).collect();
                    let top=centers.iter().copied().fold(f32::INFINITY,f32::min);
                    let bottom=centers.iter().copied().fold(f32::NEG_INFINITY,f32::max);
                    assert!(bottom-top<10.,"one horizontal property row per display: {centers:?}");
                }
            }
            assert_eq!(texts.iter().filter(|(text,_)|text=="虚拟显示器上限").count(),1);
            assert!(!texts.iter().any(|(text,_)|text=="编辑"));
            let point=texts.iter().find(|(text,_)|text==&first.name).unwrap().1.center();
            let button=|pressed|egui::Event::PointerButton{pos:point,button:egui::PointerButton::Primary,pressed,modifiers:egui::Modifiers::NONE};
            inventory_frame(&mut state,&ctx,vec![egui::Event::PointerMoved(point),button(true)],width);
            inventory_frame(&mut state,&ctx,vec![button(false)],width);
            inventory_frame(&mut state,&ctx,vec![egui::Event::Key{key:egui::Key::A,physical_key:Some(egui::Key::A),pressed:true,repeat:false,modifiers:egui::Modifiers{ctrl:true,command:true,..Default::default()}},egui::Event::Text("Edited during refresh".into())],width);
            assert_eq!(state.editors[&first.id].name,"Edited during refresh");
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
    fn display_editor_is_owner_scoped_and_keeps_failed_drafts() {
        let generated=display();
        let draft=DisplayEditor{id:generated.id,name:"Generated desk".into(),spec:generated.spec};
        let mut state=State{local_owner:Some(LocalOwner::Foreground),editors:BTreeMap::from([(generated.id,draft)]),submitted_editor:Some(generated.id),..Default::default()};
        assert!(state.select_local_owner(LocalOwner::Background));
        assert!(state.editors.is_empty()&&state.submitted_editor.is_none(),"switching management ownership discards a stale draft");
        state.local_owner=Some(LocalOwner::Foreground);
        state.editors.insert(generated.id,DisplayEditor{id:generated.id,name:"Generated desk".into(),spec:generated.spec});state.submitted_editor=Some(generated.id);
        state.receive(Response::Error{message:"generated failure".into()},Some(LocalOwner::Foreground));
        assert!(state.editors.contains_key(&generated.id)&&state.submitted_editor.is_none(),"a failed save keeps the draft for correction");
        let mut edited=generated.clone();edited.name="Generated desk".into();
        state.submitted_editor=Some(generated.id);
        state.receive(snapshot(vec![edited]),Some(LocalOwner::Foreground));
        assert!(state.editors.is_empty(),"a matching authoritative snapshot acknowledges the saved editor");
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

pub(super) fn local_request(request: LocalRequest, owner: LocalOwner) -> anyhow::Result<Response> {
    if owner == LocalOwner::Foreground && !cfg!(windows) {
        return uu_core::native_virtual_display::execute_local_owner(request);
    }
    #[cfg(windows)]
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        use anyhow::Context as _;
        match runtime
            .block_on(uu_core::service_admin::windows::request_owner(
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
    pub(super) fn remove_remote_virtual_display(&mut self,id:uu_core::virtual_display::LeaseId) {
        if let Some(handle)=&self.controller {
            self.virtual_display.open=true;
            self.virtual_display.remote(&handle.stats.virtual_displays,Request::Remove{id});
        }
    }
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
                .push(if self.virtual_display.new_is_persistent(){crate::stream_workspace::Action::CreatePersistentDisplayWithSpec {session:tabs.active,spec:self.virtual_display.spec()}}
                    else{crate::stream_workspace::Action::CreateDisplayWithSpec {session:tabs.active,spec:self.virtual_display.spec()}});
            return;
        }
        // The uncommon standalone entry uses the same launch ownership rule.
        if let Some(origin) = self.stream_target.as_ref() {
            let target = crate::sessions::StreamTarget {
                connection: crate::sessions::ConnectionTarget::NativeVirtual {
                    connection: Box::new(origin.connection.base().clone()),
                    virtual_display: crate::sessions::VirtualDisplayTarget{persistent:self.virtual_display.new_is_persistent(),..crate::sessions::VirtualDisplayTarget::new(self.virtual_display.spec())},
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
        if let Some((_,displays))=stats.as_ref().and_then(|stats|stats.virtual_displays.snapshot()) {
            if let Some(source)=self.fixed_display_target() {
                if let Some(display)=displays.iter().find(|display|display.source==source.source) {
                    self.displays.persistent_virtual=display.persistent;
                }
            }
        }
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
                    self.displays.persistent_virtual=self.virtual_display.displays.iter().find(|display|display.source==source).is_some_and(|display|display.persistent);
                    self.displays
                        .bind_created_virtual(crate::sessions::DisplayTarget { id, source });
                } else {
                    self.select_native_display(id);
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
