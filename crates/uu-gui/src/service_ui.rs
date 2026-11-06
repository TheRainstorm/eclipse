//! Windows GUI manages the one installed host. All host configuration and trust
//! mutations are service RPCs; dropping this frontend never stops the service.
use eframe::egui;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};
use uu_core::{
    device_book::Sharing,
    device_trust::{Permissions, PublicDevice},
    host_admin,
    service_admin::{windows::*, PluginRequest, PluginSnapshot, PluginInfo, Request, Response, TrustRequest, TrustSnapshot},
};
use windows_sys::Win32::System::Services::{SERVICE_RUNNING, SERVICE_STOPPED};
mod connections;

#[derive(Default)]
pub(super) struct Entry {
    manager: Option<Manager>,
    requested_section: Option<u8>,
    deployment: Option<Deployment>,
    job: Option<mpsc::Receiver<Result<bool, String>>>,
    feedback: super::ui_feedback::Feedback,
}
impl Entry {
    pub fn poll(&mut self, ctx: &egui::Context, _host: &mut super::sharing_ui::HostUi) -> bool {
        if self.manager.is_none() {
            match Manager::new() {
                Ok(manager) => self.manager = Some(manager),
                Err(error) => self.feedback.error(super::ui_feedback::Scope::Services(0), error.to_string()),
            }
        }
        if let Some(manager) = &mut self.manager {
            if let Some(section)=self.requested_section.take() {manager.section=section;}
            manager.poll();
        }
        let mut installed = false;
        if let Some(job) = &self.job {
            if let Ok(result) = job.try_recv() {
                self.job = None;
                match result {
                    Ok(true) => {
                        installed = true;
                        self.feedback.success(
                            super::ui_feedback::Scope::Services(0),
                            uu_i18n::text("服务程序操作已完成"),
                        );
                        if let Some(manager) = &mut self.manager {
                            manager.submit(Job::Refresh);
                        }
                    }
                    Ok(false) => self.feedback.report(
                        super::ui_feedback::Scope::Services(0),
                        super::ui_feedback::Kind::Info,
                        uu_i18n::text("已取消系统授权，服务尚未更新"),
                    ),
                    Err(error) => self.feedback.error(
                        super::ui_feedback::Scope::Services(0),
                        error,
                    ),
                }
            }
        }
        if let Some(deployment) = self.deployment.take() {
            let (send, receive) = mpsc::channel();
            self.job = Some(receive);
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let result = (|| -> anyhow::Result<bool> {
                    let exe = std::env::current_exe()?;
                    uu_core::service_admin::package::verify(
                        exe.parent()
                            .ok_or_else(|| anyhow::anyhow!("application directory unavailable"))?,
                        env!("UU_BUILD_COMMIT"),
                    )?;
                    let config = uu_core::Config::from_env()?;
                    match elevated(
                        &exe,
                        &format!(
                            "--background-service-action {} --source-data \"{}\"",
                            deployment.action(),
                            config.data_dir.display()
                        ),
                    ) {
                        Ok(()) => Ok(true),
                        Err(error)
                            if error
                                .downcast_ref::<std::io::Error>()
                                .is_some_and(|e| e.raw_os_error() == Some(1223)) =>
                        {
                            Ok(false)
                        }
                        Err(error) => Err(error),
                    }
                })()
                .map_err(|e| format!("{e:#}"));
                let _ = send.send(result);
                ctx.request_repaint();
            });
        }
        self.feedback.tick(ctx);
        ctx.request_repaint_after(Duration::from_millis(500));
        installed
    }
    pub fn keep_displays(&self) -> Option<bool> {
        self.manager.as_ref().and_then(|m| m.keep_displays)
    }
    pub fn password_configured(&self) -> Option<bool> {
        self.manager
            .as_ref()
            .filter(|m| m.snapshot.is_some())
            .and_then(|m| m.trust.as_ref())
            .map(|t| t.password_permissions.is_some())
    }
    pub fn open_pairing(&mut self) {
        self.select_section(4);
    }
    fn select_section(&mut self,section:u8) {
        self.requested_section=Some(section);
        if let Some(manager) = &mut self.manager {
            manager.section = section;
            self.requested_section=None;
        }
    }
    pub fn open_virtual_displays(&mut self, _ctx: &egui::Context) {
        self.select_section(6);
    }
    pub fn online(&self) -> bool {
        self.manager.as_ref().is_some_and(|m| m.ready)
    }
    pub fn toggle(&mut self) {
        if let Some(manager) = &mut self.manager {
            if !manager.checked {
                return;
            }
            if manager.state == 0 {
                self.deployment = Some(Deployment::Install);
            } else if manager.state == SERVICE_RUNNING {
                manager.confirm = Some(Confirmation::Stop);
            } else if manager.state == SERVICE_STOPPED {
                manager.submit(Job::Control(true));
            }
        }
    }
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        host: &mut super::sharing_ui::HostUi,
        available: bool,
    ) -> bool {
        let installed = self.poll(ui.ctx(), host);
        if self.manager.is_none() {
            ui.weak(uu_i18n::text("后台服务管理未就绪"));
            self.feedback
                .show(ui, super::ui_feedback::Scope::Services(0));
            return installed;
        }
        if let Some(manager) = &mut self.manager {
            manager.show_tabs(ui);
        }
        let section = self.manager.as_ref().map_or(0, |manager| manager.section);
        if section == 0 {
            self.show_status_card(ui, available);
        }
        if let Some(manager) = &mut self.manager {
            manager.show_body(ui);
        }
        installed
    }

    fn show_status_card(&mut self, ui: &mut egui::Ui, available: bool) {
        let busy = self.job.is_some() || self.manager.as_ref().is_some_and(|manager| manager.busy);
        let frame = crate::theme::card(ui.visuals());
        let width = (ui.available_width()
            - frame.inner_margin.sum().x
            - frame.outer_margin.sum().x
            - 2. * frame.stroke.width)
            .max(1.);
        frame.show(ui, |ui| {
            ui.set_width(width);
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            ui.horizontal_wrapped(|ui| {
                ui.strong(uu_i18n::text("Eclipse 服务"));
                let online = self.online();
                ui.colored_label(
                    if online {
                        crate::theme::palette(ui.visuals()).success
                    } else {
                        crate::theme::palette(ui.visuals()).warning
                    },
                    if online {
                        uu_i18n::text("在线")
                    } else {
                        uu_i18n::text("后台服务未启动或尚未就绪")
                    },
                );
                if busy {
                    ui.spinner();
                }
            });
            ui.weak(uu_i18n::text(
                "开机自动运行；关闭此窗口后继续提供串流。配置和配对统一保存在服务中。",
            ));
            if let Some(manager) = &mut self.manager {
                let outdated = manager
                    .build
                    .as_ref()
                    .and_then(|b| b.installed.as_ref())
                    .is_some_and(|v| v.revision != env!("UU_BUILD_COMMIT"));
                if let Some(build) = &manager.build {
                    ui.small(crate::build_info::label())
                        .on_hover_ui(|ui| version_labels(ui, build));
                }
                ui.horizontal_wrapped(|ui| {
                    if manager.state == 0 {
                        if ui
                            .add_enabled(
                                available && !busy && manager.checked,
                                crate::theme::primary_button(uu_i18n::text("启用服务")),
                            )
                            .clicked()
                        {
                            self.deployment = Some(Deployment::Install);
                        }
                    } else {
                        if ui
                            .add_enabled(
                                !busy,
                                crate::theme::secondary_button(if outdated {
                                    uu_i18n::text("应用程序更新")
                                } else {
                                    uu_i18n::text("重新部署服务")
                                }),
                            )
                            .clicked()
                        {
                            manager.confirm = Some(Confirmation::Update);
                        }
                        if ui
                            .add_enabled(
                                !busy
                                    && !manager.busy
                                    && matches!(manager.state, SERVICE_RUNNING | SERVICE_STOPPED),
                                crate::theme::secondary_button(
                                    if manager.state == SERVICE_STOPPED {
                                        uu_i18n::text("启动服务")
                                    } else {
                                        uu_i18n::text("停止服务")
                                    },
                                ),
                            )
                            .clicked()
                        {
                            if manager.state == SERVICE_STOPPED {
                                manager.submit(Job::Control(true));
                            } else {
                                manager.confirm = Some(Confirmation::Stop);
                            }
                        }
                    }
                });
                if manager.section == 5 && manager.state != 0 {
                    if ui
                        .add_enabled(
                            !busy,
                            crate::theme::secondary_button(uu_i18n::text("导入服务插件")),
                        )
                        .on_hover_text(uu_i18n::text(
                            "将当前已启用插件及其声明的配置、账号复制到服务；导入代码需要系统授权。",
                        ))
                        .clicked()
                    {
                        manager.confirm = Some(Confirmation::ImportPlugins);
                    }
                }
                if outdated {
                    ui.colored_label(
                        crate::theme::palette(ui.visuals()).warning,
                        uu_i18n::text("新程序包尚未应用到服务。更新会保留同一份配置与配对。"),
                    );
                }
                if let Some(confirm) = manager.confirm {
                    ui.separator();
                    if let Some(snapshot) = &manager.snapshot {
                        ui.label(uu_i18n::format!(
                            "此操作会断开当前 {} 个串流会话。",
                            snapshot.connections.len()
                        ));
                    } else {
                        ui.label(uu_i18n::text(
                            "无法确认当前连接数；此操作可能会断开正在进行的串流。",
                        ));
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add(crate::theme::primary_button(uu_i18n::text("确认继续")))
                            .clicked()
                        {
                            manager.confirm = None;
                            match confirm {
                                Confirmation::Update => self.deployment = Some(Deployment::Install),
                                Confirmation::ImportPlugins => {
                                    self.deployment = Some(Deployment::ImportPlugins)
                                }
                                Confirmation::Stop => manager.submit(Job::Control(false)),
                                Confirmation::Restart => manager.save(true),
                            }
                        }
                        if ui
                            .add(crate::theme::secondary_button(uu_i18n::text("取消")))
                            .clicked()
                        {
                            manager.confirm = None;
                        }
                    });
                }
            }
            self.feedback
                .show(ui, super::ui_feedback::Scope::Services(0));
            if let Some(manager) = &self.manager {
                if !manager.connection_error.is_empty() {
                    ui.colored_label(
                        crate::theme::palette(ui.visuals()).danger,
                        &manager.connection_error,
                    );
                }
            }
        });
        ui.add_space(12.);
    }
}
fn section_card(ui: &mut egui::Ui, title: &str, contents: impl FnOnce(&mut egui::Ui)) {
    let frame = crate::theme::card(ui.visuals());
    let width = (ui.available_width()
        - frame.inner_margin.sum().x
        - frame.outer_margin.sum().x
        - 2. * frame.stroke.width)
        .max(1.);
    frame.show(ui, |ui| {
        ui.set_width(width);
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
        ui.strong(title);
        ui.add_space(8.);
        contents(ui);
    });
    ui.add_space(12.);
}
fn version_labels(ui: &mut egui::Ui, build: &BuildStatus) {
    if let Some(running) = &build.running {
        ui.label(uu_i18n::format!(
            "后台运行版本：{} · PID {}",
            running.label(),
            build.process_id
        ))
        .on_hover_text(&running.revision);
    } else if build.process_id != 0 {
        ui.label(uu_i18n::text("运行版本暂不可读"));
    }
    if let Some(installed) = &build.installed {
        ui.label(uu_i18n::format!("后台安装版本：{}", installed.label()))
            .on_hover_text(&installed.revision);
    }
    ui.weak(crate::build_info::label());
}
#[derive(Clone, Copy)]
enum Deployment {
    Install,
    ImportPlugins,
}
impl Deployment {
    fn action(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::ImportPlugins => "import-plugins",
        }
    }
}
#[derive(Clone, Copy)]
enum Confirmation {
    Update,
    ImportPlugins,
    Stop,
    Restart,
}
enum Job {
    Refresh,
    Control(bool),
    Rpc(Request),
}
impl Job {
    /// Background refresh has no completion event; locking the whole service
    /// form for it leaves stale busy state after a successful deployment.
    fn foreground(&self) -> bool {
        !matches!(self, Self::Refresh)
    }
}
enum Event {
    State {
        state: u32,
        build: Option<BuildStatus>,
        response: Result<Response, String>,
    },
    Done(Result<Response, String>),
    Trust {
        completed: bool,
        result: Result<Response, String>,
    },
    Connections {
        completed: bool,
        result: Result<Response, String>,
    },
    Plugins {
        completed: bool,
        result: Result<Response, String>,
    },
}
struct PluginDraft {
    expected:uu_core::plugins::PluginPreferences,
    value:uu_core::plugins::PluginPreferences,
    options:String,
    dirty:bool,
}
impl PluginDraft {
    fn new(value:uu_core::plugins::PluginPreferences)->Self {
        let options=serde_json::to_string_pretty(&value.options).unwrap_or_else(|_| "{}".into());
        Self{expected:value.clone(),value,options,dirty:false}
    }
}
struct Manager {
    virtual_display: super::virtual_display_ui::State,
    sender: mpsc::SyncSender<Job>,
    events: mpsc::Receiver<Event>,
    stop: Arc<AtomicBool>,
    state: u32,
    build: Option<BuildStatus>,
    ready: bool,
    checked: bool,
    connection_error: String,
    keep_displays: Option<bool>,
    snapshot: Option<host_admin::Snapshot>,
    trust: Option<TrustSnapshot>,
    plugins:Option<PluginSnapshot>,
    plugin_drafts:HashMap<String,PluginDraft>,
    service_connections: connections::State,
    endpoints: Vec<(String, String)>,
    pending: Vec<uu_core::service_admin::Pairing>,
    draft: Sharing,
    revision: String,
    dirty: bool,
    busy: bool,
    section: u8,
    feedback: super::ui_feedback::Feedback,
    job_section: Option<u8>,
    codes: HashMap<String, zeroize::Zeroizing<String>>,
    permissions: HashMap<String, Permissions>,
    pairing: ServicePairing,
    confirm: Option<Confirmation>,
}
impl Drop for Manager {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}
impl Manager {
    fn new() -> anyhow::Result<Self> {
        let (sender, jobs) = mpsc::sync_channel(8);
        let (send, events) = mpsc::sync_channel(16);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(r) => r,
                Err(e) => {
                    let _ = send.send(Event::Done(Err(e.to_string())));
                    return;
                }
            };
            let mut trust_at = Instant::now();
            let mut plugins_at = Instant::now();
            while !stopped.load(Ordering::Acquire) {
                match jobs.recv_timeout(Duration::from_millis(750)) {
                    Ok(Job::Refresh) => {}
                    Ok(Job::Control(start)) => {
                        let result = control_owner(start)
                            .map(|_| Response::Applied)
                            .map_err(|e| format!("{e:#}"));
                        let _ = send.send(Event::Done(result));
                    }
                    Ok(Job::Rpc(request)) => {
                        let trust = matches!(request, Request::Trust { .. });
                        let connections = matches!(request, Request::Connections { .. });
                        let plugins = matches!(request, Request::Plugins { .. });
                        let result = runtime
                            .block_on(request_remote(request))
                            .map_err(|e| format!("{e:#}"));
                        let _ = send.send(if trust {
                            Event::Trust {
                                completed: true,
                                result,
                            }
                        } else if connections {
                            Event::Connections {
                                completed: true,
                                result,
                            }
                        } else if plugins {
                            Event::Plugins {
                                completed: true,
                                result,
                            }
                        } else {
                            Event::Done(result)
                        });
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                let build = build_status().ok();
                let state = build
                    .as_ref()
                    .map(|b| b.state)
                    .unwrap_or_else(|| match status() {
                        Ok(state) => state,
                        Err(error)
                            if error.downcast_ref::<std::io::Error>().is_some_and(|e| {
                                e.raw_os_error()
                                    == Some(
                                        windows_sys::Win32::Foundation::ERROR_SERVICE_DOES_NOT_EXIST
                                            as i32,
                                    )
                            }) =>
                        {
                            0
                        }
                        Err(_) => u32::MAX,
                    });
                let response = if state == SERVICE_RUNNING {
                    runtime
                        .block_on(request_remote(Request::Status))
                        .map_err(|e| format!("{e:#}"))
                } else {
                    Err(uu_i18n::text("后台服务未启动；启动服务后读取和修改同一份配置。").into())
                };
                if send
                    .send(Event::State {
                        state,
                        build,
                        response,
                    })
                    .is_err()
                {
                    break;
                }
                if state == SERVICE_RUNNING && Instant::now() >= trust_at {
                    let result = runtime
                        .block_on(request_remote(Request::Trust {
                            request: TrustRequest::Inspect,
                        }))
                        .map_err(|e| format!("{e:#}"));
                    if send
                        .send(Event::Trust {
                            completed: false,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    trust_at = Instant::now() + Duration::from_secs(5);
                    let result = runtime
                        .block_on(request_remote(Request::Connections {
                            request: uu_core::service_admin::connections::Request::Inspect,
                        }))
                        .map_err(|e| format!("{e:#}"));
                    if send
                        .send(Event::Connections {
                            completed: false,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                if state == SERVICE_RUNNING && Instant::now() >= plugins_at {
                    let result = runtime
                        .block_on(request_remote(Request::Plugins{request:PluginRequest::Inspect}))
                        .map_err(|e| format!("{e:#}"));
                    if send.send(Event::Plugins{completed:false,result}).is_err(){break;}
                    plugins_at=Instant::now()+Duration::from_secs(5);
                }
            }
        });
        Ok(Self::empty(sender, events, stop))
    }
    fn empty(
        sender: mpsc::SyncSender<Job>,
        events: mpsc::Receiver<Event>,
        stop: Arc<AtomicBool>,
    ) -> Self {
        Self {
            virtual_display: Default::default(),
            sender,
            events,
            stop,
            state: 0,
            build: None,
            ready: false,
            checked: false,
            connection_error: String::new(),
            keep_displays: None,
            snapshot: None,
            trust: None,
            plugins:None,
            plugin_drafts:HashMap::new(),
            service_connections: Default::default(),
            endpoints: vec![],
            pending: vec![],
            draft: Sharing::default(),
            revision: String::new(),
            dirty: false,
            busy: false,
            section: 0,
            feedback: Default::default(),
            job_section: None,
            codes: HashMap::new(),
            permissions: HashMap::new(),
            pairing: Default::default(),
            confirm: None,
        }
    }
    fn submit(&mut self, job: Job) {
        let foreground = job.foreground();
        if foreground && self.busy {
            return;
        }
        match self.sender.try_send(job) {
            Ok(()) => {
                if foreground {
                    self.busy = true;
                    self.job_section = Some(self.section);
                }
            }
            Err(error) => self.feedback.error(
                super::ui_feedback::Scope::Services(self.section),
                error.to_string(),
            ),
        }
    }
    fn completion_scope(&mut self) -> super::ui_feedback::Scope {
        super::ui_feedback::Scope::Services(self.job_section.take().unwrap_or(self.section))
    }
    fn report_completion(&mut self, result: Result<String, String>) {
        let scope = self.completion_scope();
        match result {
            Ok(text) => self.feedback.success(scope, text),
            Err(text) => self.feedback.error(scope, text),
        }
    }
    fn report_info(&mut self, text: impl Into<String>) {
        let scope = self.completion_scope();
        self.feedback
            .report(scope, super::ui_feedback::Kind::Info, text);
    }
    fn poll(&mut self) {
        loop {
            let event = match self.events.try_recv() {
                Ok(event) => event,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.ready = false;
                    self.keep_displays = None;
                    self.snapshot = None;
                    self.busy = false;
                    self.connection_error = uu_i18n::text("后台管理工作线程已结束").into();
                    break;
                }
            };
            match event {
                Event::State {
                    state,
                    build,
                    response,
                } => {
                    self.state = state;
                    self.build = build;
                    self.checked = true;
                    match response {
                        Ok(Response::Status {
                            ready,
                            keep_virtual_displays,
                            endpoints,
                            pending,
                            snapshot,
                        }) => {
                            self.connection_error.clear();
                            self.keep_displays = keep_virtual_displays;
                            self.ready = ready;
                            self.endpoints = endpoints;
                            self.pending = pending;
                            self.codes
                                .retain(|id, _| self.pending.iter().any(|p| p.id == *id));
                            self.permissions
                                .retain(|id, _| self.pending.iter().any(|p| p.id == *id));
                            if !self.dirty {
                                if let Some(server) = &snapshot.server {
                                    self.draft = server.saved.clone();
                                    self.revision = server.revision.clone();
                                }
                            }
                            self.snapshot = Some(snapshot);
                        }
                        Ok(Response::Error { message }) | Err(message) => {
                            self.ready = false;
                            self.keep_displays = None;
                            self.snapshot = None;
                            self.service_connections.snapshot = None;
                            self.pending.clear();
                            self.endpoints.clear();
                            self.connection_error = message;
                        }
                        _ => {
                            self.ready = false;
                            self.snapshot = None;
                        }
                    }
                }
                Event::Connections { completed, result } => {
                    if completed {
                        self.busy = false;
                    }
                    if let Some(notice) = self.service_connections.apply(completed, result) {
                        if completed {
                            self.report_completion(notice);
                        }
                    }
                }
                Event::Plugins { completed, result } => {
                    if completed {self.busy=false;}
                    match result {
                        Ok(Response::Plugins{snapshot})=>{
                            self.apply_plugin_snapshot(snapshot);
                            if completed {self.report_completion(Ok(uu_i18n::text("后台插件设置已保存，服务已重启").into()));}
                        }
                        Ok(Response::Error{message})|Err(message)=>if completed{self.report_completion(Err(message));},
                        _=>{},
                    }
                }
                Event::Done(result) => {
                    self.busy = false;
                    match result {
                        Ok(Response::Error { message }) | Err(message) => self.report_completion(Err(message)),
                        Ok(Response::Admin {
                            response: host_admin::Response::Error { message },
                        }) => self.report_completion(Err(message)),
                        Ok(Response::Admin {
                            response: host_admin::Response::Saved { .. },
                        }) => {
                            self.dirty = false;
                            self.report_completion(Ok(uu_i18n::text(
                                "已保存到服务。媒体默认值用于新连接；显示器上限立即生效。",
                            ).into()));
                        }
                        Ok(Response::Firewall { message }) => self.report_info(message),
                        _ => self.report_completion(Ok(uu_i18n::text("操作已完成").into())),
                    }
                }
                Event::Trust { completed, result } => {
                    if completed {
                        self.busy = false;
                    }
                    match result {
                        Ok(Response::Trust { snapshot }) => {
                            self.trust = Some(snapshot);
                            if completed {
                                if self.pairing.password_submitted {
                                    self.pairing.password.clear();
                                }
                                self.pairing.password_submitted = false;
                                self.report_completion(Ok(uu_i18n::text("操作已完成").into()));
                            }
                        }
                        Ok(Response::Error { message }) | Err(message) => {
                            if completed {
                                self.pairing.password_submitted = false;
                                self.report_completion(Err(message));
                            }
                        }
                        _ => {}
                    }
                    // Periodic trust inspection is not a completion for another job.
                }
            }
        }
    }
    fn save(&mut self, restart: bool) {
        if self.snapshot.is_none() || self.busy {
            return;
        }
        self.submit(Job::Rpc(Request::Admin {
            request: host_admin::Request::Save {
                revision: self.revision.clone(),
                settings: self.draft.clone(),
                restart,
            },
        }));
    }
    fn apply_plugin_snapshot(&mut self,snapshot:PluginSnapshot) {
        let ids=snapshot.plugins.iter().map(|plugin|plugin.id.clone()).collect::<std::collections::HashSet<_>>();
        for plugin in &snapshot.plugins {
            match self.plugin_drafts.get_mut(&plugin.id) {
                Some(draft) if draft.dirty=>{},
                Some(draft)=>{
                    if draft.expected!=plugin.preferences {*draft=PluginDraft::new(plugin.preferences.clone());}
                }
                None=>{self.plugin_drafts.insert(plugin.id.clone(),PluginDraft::new(plugin.preferences.clone()));}
            }
        }
        self.plugin_drafts.retain(|id,_|ids.contains(id));
        self.plugins=Some(snapshot);
    }
    fn plugins_ui(&mut self,ui:&mut egui::Ui,enabled:bool) {
        ui.add_space(12.);
        crate::theme::card(ui.visuals()).show(ui,|ui|{
            ui.horizontal_wrapped(|ui|{
                ui.strong(uu_i18n::text("后台插件"));
                if ui.add_enabled(enabled,crate::theme::secondary_button(uu_i18n::text("重新扫描后台插件"))).clicked(){
                    self.submit(Job::Rpc(Request::Plugins{request:PluginRequest::Rescan}));
                }
            });
            ui.weak(uu_i18n::text("这里读写 Windows 后台服务自己的插件开关和选项；不会读取或覆盖当前用户的前台插件配置。保存后共享服务会重启。"));
            let Some(snapshot)=self.plugins.clone() else{ui.weak(uu_i18n::text("正在读取后台插件…"));return;};
            ui.small(snapshot.directory);
            if snapshot.plugins.is_empty(){ui.label(uu_i18n::text("后台尚未安装插件。可先导入服务插件，再重新扫描。"));}
            let mut save=None;
            for plugin in snapshot.plugins {
                ui.push_id(("service-plugin",&plugin.id),|ui|{
                    egui::Frame::group(ui.style()).inner_margin(12.).show(ui,|ui|{
                        ui.horizontal_wrapped(|ui|{
                            ui.strong(&plugin.name);ui.label(format!("v{}",plugin.version));
                            if !plugin.compatible{ui.colored_label(crate::theme::palette(ui.visuals()).danger,uu_i18n::text("与当前后台 SDK 不匹配"));}
                        });
                        if !plugin.error.is_empty(){ui.colored_label(crate::theme::palette(ui.visuals()).danger,&plugin.error);}
                        let draft=self.plugin_drafts.entry(plugin.id.clone()).or_insert_with(||PluginDraft::new(plugin.preferences.clone()));
                        let mut changed=false;
                        changed|=ui.add_enabled_ui(enabled&&plugin.compatible,|ui|crate::theme::switch(ui,&mut draft.value.enabled,uu_i18n::text("在后台启用"))).inner.changed();
                        ui.collapsing(uu_i18n::text("高级选项（JSON）"),|ui|{
                            changed|=ui.add(egui::TextEdit::multiline(&mut draft.options).desired_rows(3).code_editor()).changed();
                        });
                        if changed{draft.dirty=true;}
                        if draft.dirty {
                            ui.horizontal_wrapped(|ui|{
                                if ui.add_enabled(enabled&&plugin.compatible,crate::theme::primary_button(uu_i18n::text("保存并重启服务"))).clicked(){
                                    match serde_json::from_str::<serde_json::Map<String,serde_json::Value>>(&draft.options) {
                                        Ok(options)=>{draft.value.options=options;save=Some((plugin.id.clone(),draft.expected.clone(),draft.value.clone()));},
                                        Err(error)=>self.feedback.error(
                                            super::ui_feedback::Scope::Services(5),
                                            uu_i18n::format!("插件选项不是有效 JSON：{error}"),
                                        ),
                                    }
                                }
                                if ui.add(crate::theme::secondary_button(uu_i18n::text("还原"))).clicked(){*draft=PluginDraft::new(draft.expected.clone());}
                            });
                            ui.weak(uu_i18n::text("保存会按后台插件自己的校验规则应用选项，并重启共享服务以更新主机入口。"));
                        }
                    });
                });
            }
            if let Some((id,expected,value))=save {
                self.plugin_drafts.get_mut(&id).map(|draft|draft.dirty=false);
                self.submit(Job::Rpc(Request::Plugins{request:PluginRequest::Save{id,expected,value}}));
            }
        });
    }
    fn show_tabs(&mut self, ui: &mut egui::Ui) {
        self.virtual_display.poll(None);
        ui.add_space(10.);
        ui.horizontal_wrapped(|ui| {
            for (tab, label) in [
                (0, "服务管理"),
                (1, "输入"),
                (2, "音频 / 视频"),
                (3, "编码器"),
                (4, "配对设置"),
                (5, "连接服务"),
                (6, "虚拟显示器"),
            ] {
                crate::theme::tab_value(ui, &mut self.section, tab, uu_i18n::text(label));
            }
        });
        ui.add_space(10.);
    }
    fn show_body(&mut self, ui: &mut egui::Ui) {
        self.feedback.tick(ui.ctx());
        self.feedback
            .show(ui, super::ui_feedback::Scope::Services(self.section));
        let enabled = self.snapshot.is_some() && !self.busy;
        match self.section {
            0 => {
                section_card(ui, uu_i18n::text("连接入口"), |ui| {
                    for (name, state) in &self.endpoints {
                        ui.label(format!("{name} · {state}"));
                    }
                    if self.endpoints.is_empty() {
                        ui.weak(uu_i18n::text("当前没有可用的连接入口。"));
                    }
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add_enabled(
                                enabled,
                                crate::theme::secondary_button(uu_i18n::text("检查防火墙")),
                            )
                            .clicked()
                        {
                            self.submit(Job::Rpc(Request::Firewall { repair: false }));
                        }
                        if ui
                            .add_enabled(
                                enabled,
                                crate::theme::secondary_button(uu_i18n::text("修复防火墙")),
                            )
                            .clicked()
                        {
                            self.submit(Job::Rpc(Request::Firewall { repair: true }));
                        }
                    });
                });
                section_card(ui, uu_i18n::text("会话"), |ui| self.connections(ui));
            }
            1..=4 => {
                let title = match self.section {
                    1 => uu_i18n::text("输入"),
                    2 => uu_i18n::text("音频 / 视频"),
                    3 => uu_i18n::text("编码器"),
                    _ => uu_i18n::text("配对设置"),
                };
                section_card(ui, title, |ui| {
                    ui.add_enabled_ui(enabled, |ui| {
                        let before = self.draft.clone();
                        match self.section {
                            1..=3 => {
                                let monitors = self
                                    .snapshot
                                    .as_ref()
                                    .and_then(|s| s.server.as_ref())
                                    .map(|s| s.monitors.clone())
                                    .unwrap_or_default();
                                super::server_config_ui::edit(
                                    ui,
                                    &mut self.draft,
                                    match self.section {
                                        1 => 2,
                                        2 => 0,
                                        _ => 1,
                                    },
                                    &monitors,
                                    false,
                                    true,
                                    false,
                                );
                            }
                            4 => {
                                super::server_config_ui::device_name(ui, &mut self.draft, true);
                                if let Some(snapshot) = &self.trust {
                                    let mut commands = vec![];
                                    self.pairing.show(ui, snapshot, &mut commands);
                                    if let Some(request) = commands.pop() {
                                        self.submit(Job::Rpc(Request::Trust { request }));
                                    }
                                }
                            }
                            _ => unreachable!(),
                        }
                        self.dirty |= self.draft != before;
                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .add_enabled(
                                    self.dirty,
                                    crate::theme::primary_button(uu_i18n::text("保存设置")),
                                )
                                .clicked()
                            {
                                self.save(false);
                            }
                            if ui
                                .add_enabled(
                                    self.dirty,
                                    crate::theme::secondary_button(uu_i18n::text("还原修改")),
                                )
                                .clicked()
                            {
                                self.dirty = false;
                                if let Some(server) =
                                    self.snapshot.as_ref().and_then(|s| s.server.as_ref())
                                {
                                    self.draft = server.saved.clone();
                                    self.revision = server.revision.clone();
                                }
                            }
                            if ui
                                .add(crate::theme::secondary_button(uu_i18n::text(
                                    "保存并重启服务",
                                )))
                                .clicked()
                            {
                                self.confirm = Some(Confirmation::Restart);
                            }
                        });
                    });
                });
            }
            5 => {
                section_card(ui, uu_i18n::text("连接服务"), |ui| {
                    ui.add_enabled_ui(enabled, |ui| {
                        let before = self.draft.clone();
                        crate::theme::switch(
                            ui,
                            &mut self.draft.ip_enabled,
                            uu_i18n::text("IP 直连"),
                        );
                        ui.horizontal_wrapped(|ui| {
                            ui.label(uu_i18n::text("服务端口"));
                            crate::theme::number_input(
                                ui,
                                egui::DragValue::new(&mut self.draft.service_port).range(1..=65535),
                            );
                            ui.label(uu_i18n::text("发现端口"));
                            crate::theme::number_input(
                                ui,
                                egui::DragValue::new(&mut self.draft.discovery_port)
                                    .range(1..=65535),
                            );
                        });
                        for provider in uu_core::plugins::registry().descriptors() {
                            let mut selected = self
                                .draft
                                .plugin_services
                                .iter()
                                .any(|id| id == provider.id);
                            if crate::theme::switch(ui, &mut selected, provider.name).changed() {
                                self.draft.plugin_services.retain(|id| id != provider.id);
                                if selected {
                                    self.draft.plugin_services.push(provider.id.into());
                                }
                            }
                        }
                        self.dirty |= self.draft != before;
                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .add_enabled(
                                    self.dirty,
                                    crate::theme::primary_button(uu_i18n::text("保存设置")),
                                )
                                .clicked()
                            {
                                self.save(false);
                            }
                            if ui
                                .add_enabled(
                                    self.dirty,
                                    crate::theme::secondary_button(uu_i18n::text("还原修改")),
                                )
                                .clicked()
                            {
                                self.dirty = false;
                                if let Some(server) =
                                    self.snapshot.as_ref().and_then(|s| s.server.as_ref())
                                {
                                    self.draft = server.saved.clone();
                                    self.revision = server.revision.clone();
                                }
                            }
                            if ui
                                .add(crate::theme::secondary_button(uu_i18n::text(
                                    "保存并重启服务",
                                )))
                                .clicked()
                            {
                                self.confirm = Some(Confirmation::Restart);
                            }
                        });
                        if self.dirty
                            || self.draft
                                != self
                                    .snapshot
                                    .as_ref()
                                    .and_then(|s| s.server.as_ref())
                                    .map(|s| s.active.clone())
                                    .unwrap_or_default()
                        {
                            ui.weak(uu_i18n::text(
                                "名称、端口和连接入口变更需要重启服务；重启会断开当前串流。",
                            ));
                        }
                    });
                });
                if let Some(request) = self.service_connections.show(ui, enabled) {
                    self.submit(Job::Rpc(Request::Connections { request }));
                }
                self.plugins_ui(ui, enabled);
            }
            6 => {
                section_card(ui, uu_i18n::text("虚拟显示器"), |ui| {
                    if self.snapshot.is_some() {
                        let service_busy = self.busy;
                        ui.add_enabled_ui(!service_busy, |ui| {
                            self.virtual_display.show_background_inline(ui);
                        });
                    } else {
                        ui.weak(uu_i18n::text("启动服务后可在这里管理虚拟显示器。"));
                    }
                });
            }
            _ => {}
        }
        for pending in self.pending.clone() {
            crate::theme::card(ui.visuals()).show(ui, |ui| {
                ui.strong(uu_i18n::format!("配对请求 · {}", pending.device.name));
                ui.monospace(pending.device.id());
                let code = self.codes.entry(pending.id.clone()).or_default();
                ui.add(
                    crate::theme::Input::new(&mut **code)
                        .char_limit(8)
                        .hint_text(uu_i18n::text("控制端显示的 8 位配对码")),
                );
                let permission = self
                    .permissions
                    .entry(pending.id.clone())
                    .or_insert(Permissions::CONTROL);
                ui.horizontal_wrapped(|ui| {
                    super::sharing_ui::permissions(ui, permission);
                });
                let permission = *permission;
                let code = code.to_string();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !self.busy && pending.waiting && code.len() == 8,
                            crate::theme::primary_button(uu_i18n::text("批准配对")),
                        )
                        .clicked()
                    {
                        self.submit(Job::Rpc(Request::Approve {
                            id: pending.id.clone(),
                            code,
                            permissions: permission,
                        }));
                    }
                    if ui
                        .add_enabled(
                            !self.busy,
                            crate::theme::secondary_button(uu_i18n::text("拒绝")),
                        )
                        .clicked()
                    {
                        self.submit(Job::Rpc(Request::Reject {
                            id: pending.id.clone(),
                        }));
                    }
                });
            });
        }
    }
    fn connections(&mut self, ui: &mut egui::Ui) {
        if let Some(snapshot) = self.snapshot.clone() {
            if snapshot.connections.is_empty() {
                ui.weak(uu_i18n::text("当前没有连接的会话。设备连接后，可在这里查看和管理会话。"));
                return;
            }
            ui.strong(uu_i18n::format!(
                "已连接 {} 个会话",
                snapshot.connections.len()
            ));
            for connection in snapshot.connections {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("{} · {}", connection.name, connection.status));
                    let mut permission = connection.permissions;
                    if super::sharing_ui::permissions(ui, &mut permission) {
                        self.submit(Job::Rpc(Request::Admin {
                            request: host_admin::Request::Permissions {
                                peer: connection.peer,
                                permissions: permission,
                            },
                        }));
                    }
                    if ui
                        .add(crate::theme::secondary_button(uu_i18n::text("断开此会话")))
                        .clicked()
                    {
                        self.submit(Job::Rpc(Request::Admin {
                            request: host_admin::Request::Disconnect {
                                session: connection.id,
                            },
                        }));
                    }
                });
            }
        } else {
            ui.weak(uu_i18n::text("连接服务后显示当前会话。"));
        }
    }
}
async fn request_remote(request: Request) -> anyhow::Result<Response> {
    uu_core::service_admin::windows::request_owner(request).await
}
struct Standalone {
    entry: Entry,
    host: super::sharing_ui::HostUi,
    appearance: super::theme::PreferenceWatch,
}
impl eframe::App for Standalone {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.appearance.poll(ctx);
        ctx.request_repaint_after(Duration::from_millis(500));
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.entry.show(ui, &mut self.host, true);
            });
        });
    }
}
struct ServicePairing {
    loaded: bool,
    method: u8,
    password_submitted: bool,
    password: zeroize::Zeroizing<String>,
    permissions: Permissions,
    key: String,
    name: String,
    key_permissions: Permissions,
}
impl Default for ServicePairing {
    fn default() -> Self {
        Self {
            loaded: false,
            method: 0,
            password_submitted: false,
            password: Default::default(),
            permissions: Permissions::CONTROL,
            key: String::new(),
            name: String::new(),
            key_permissions: Permissions::CONTROL,
        }
    }
}
impl ServicePairing {
    fn show(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &TrustSnapshot,
        commands: &mut Vec<TrustRequest>,
    ) {
        if !self.loaded {
            self.permissions = snapshot
                .password_permissions
                .unwrap_or(Permissions::CONTROL);
            self.loaded = true;
        }
        ui.add_space(8.);
        ui.horizontal_wrapped(|ui| {
            crate::theme::tab_value(ui, &mut self.method, 0, uu_i18n::text("密码配对"));
            crate::theme::tab_value(ui, &mut self.method, 1, uu_i18n::text("公钥配对"));
        });
        if self.method == 0 {
            ui.label(if snapshot.password_permissions.is_some() {
                uu_i18n::text("已设置安全连接密码")
            } else {
                uu_i18n::text("尚未设置安全连接密码")
            });
            ui.add(
                crate::theme::Input::new(&mut *self.password)
                    .password(true)
                    .char_limit(256)
                    .hint_text(uu_i18n::text("新的安全连接密码 · 8–256 字节")),
            );
            ui.horizontal_wrapped(|ui| {
                super::sharing_ui::permissions(ui, &mut self.permissions);
            });
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        (8..=256).contains(&self.password.len()),
                        crate::theme::secondary_button(uu_i18n::text("保存安全连接密码")),
                    )
                    .clicked()
                {
                    commands.push(TrustRequest::Password {
                        password: Some(self.password.clone()),
                        permissions: self.permissions,
                    });
                    self.password_submitted = true;
                }
                if ui
                    .add_enabled(
                        snapshot.password_permissions.is_some(),
                        crate::theme::secondary_button(uu_i18n::text("移除安全连接密码")),
                    )
                    .clicked()
                {
                    commands.push(TrustRequest::Password {
                        password: None,
                        permissions: self.permissions,
                    });
                }
            });
        } else {
            ui.add_space(10.);
            ui.strong(uu_i18n::text("预先授权控制端公钥"));
            ui.add(
                crate::theme::Input::new(&mut self.name)
                    .char_limit(64)
                    .hint_text(uu_i18n::text("控制端名称")),
            );
            ui.add(
                crate::theme::Input::multiline(&mut self.key)
                    .char_limit(8192)
                    .desired_rows(2)
                    .hint_text(uu_i18n::text("粘贴 Eclipse 或 SSH Ed25519 公钥")),
            );
            ui.horizontal_wrapped(|ui| {
                super::sharing_ui::permissions(ui, &mut self.key_permissions);
            });
            if ui
                .add_enabled(
                    PublicDevice::from_public_key(&self.key, &self.name).is_ok(),
                    crate::theme::secondary_button(uu_i18n::text("添加授权公钥")),
                )
                .clicked()
            {
                if let Ok(device) = PublicDevice::from_public_key(&self.key, &self.name) {
                    commands.push(TrustRequest::Authorize {
                        device,
                        permissions: self.key_permissions,
                    });
                }
            }
            ui.collapsing(uu_i18n::text("本机公钥"), |ui| {
                ui.monospace(snapshot.device.id());
                if ui
                    .add(crate::theme::secondary_button(uu_i18n::text(
                        "复制本机公钥",
                    )))
                    .clicked()
                {
                    ui.ctx().copy_text(snapshot.device.export_public_key());
                }
            });
        }
        ui.add_space(10.);
        ui.strong(uu_i18n::text("已授权设备"));
        for peer in snapshot
            .peers
            .iter()
            .filter(|p| !p.revoked && p.inbound.is_some())
        {
            ui.horizontal_wrapped(|ui| {
                ui.label(&peer.device.name);
                let mut permission = peer.inbound.unwrap();
                if super::sharing_ui::permissions(ui, &mut permission) {
                    commands.push(TrustRequest::Authorize {
                        device: peer.device.clone(),
                        permissions: permission,
                    });
                }
                if ui
                    .add(crate::theme::secondary_button(uu_i18n::text("撤销授权")))
                    .clicked()
                {
                    commands.push(TrustRequest::Revoke {
                        peer: peer.device.id(),
                    });
                }
            });
        }
    }
}
pub(super) fn run() -> eframe::Result {
    run_manager(false)
}
fn run_manager(virtual_displays: bool) -> eframe::Result {
    let mut manager = Manager::new().map_err(|error| eframe::Error::AppCreation(error.into()))?;
    eframe::run_native(
        uu_i18n::text("Eclipse · 后台服务管理"),
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([850., 760.])
                .with_min_inner_size([640., 480.]),
            ..Default::default()
        },
        Box::new(move |cc| {
            super::setup_style(&cc.egui_ctx);
            let appearance = super::theme::PreferenceWatch::load_current(&cc.egui_ctx);
            if virtual_displays {
                manager.section = 6;
            }
            Ok(Box::new(Standalone {
                entry: Entry {
                    manager: Some(manager),
                    ..Default::default()
                },
                host: Default::default(),
                appearance,
            }))
        }),
    )
}

/// Called on the sharing startup worker, never on the rendering thread.
pub(super) fn prepare_foreground_firewall(
    service: u16,
    discovery: u16,
) -> anyhow::Result<Option<String>> {
    let exe = std::env::current_exe()?;
    if let Ok(before) = uu_core::firewall::inspect(&exe, service, discovery) {
        if before.rules.as_array().is_some_and(|r| !r.is_empty()) {
            return Ok(Some(before.message()));
        }
    }
    // Some organization policies also restrict ordinary-user inspection. The
    // elevated helper rechecks before deciding whether any rule may be created.
    if require_administrator().is_ok() {
        return Ok(Some(
            uu_core::firewall::ensure(&exe, service, discovery)?.message(),
        ));
    }
    elevated(
        &exe,
        &format!("--configure-streaming-firewall {service} {discovery}"),
    )?;
    let after = uu_core::firewall::inspect(&exe, service, discovery)?;
    anyhow::ensure!(
        after.rules.as_array().is_some_and(|r| !r.is_empty()),
        "Firewall configuration was not visible after administrator repair"
    );
    Ok(Some(format!(
        "{}\n{}",
        uu_i18n::text("已由管理员配置防火墙"),
        after.message()
    )))
}

/// Each UAC action is launched through this GUI version. Management does not
/// depend on an adjacent CLI; install/import still use a verified complete set.
pub(super) fn dispatch() -> Option<eframe::Result> {
    let tray_args: Vec<_> = std::env::args_os().skip(1).collect();
    if tray_args.len() == 6 && tray_args[0] == "--tray-display-action" {
        let result = (|| -> anyhow::Result<bool> {
            require_administrator()?;
            let exe = std::env::current_exe()?;
            uu_core::service_admin::package::verify(
                exe.parent()
                    .ok_or_else(|| anyhow::anyhow!("application directory unavailable"))?,
                env!("UU_BUILD_COMMIT"),
            )?;
            let action = tray_args[1]
                .to_str()
                .and_then(crate::tray::DisplayAction::parse)
                .ok_or_else(|| anyhow::anyhow!("unknown tray display action"))?;
            let number = |i: usize| -> anyhow::Result<u32> {
                Ok(tray_args[i]
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("invalid display number"))?
                    .parse()?)
            };
            let hdr = number(5)?;
            anyhow::ensure!(hdr <= 1, "invalid HDR flag");
            let spec = uu_core::virtual_display::Spec {
                width: number(2)?,
                height: number(3)?,
                refresh_millihz: number(4)?,
                hdr: hdr == 1,
            };
            spec.validate()?;
            crate::tray_displays::execute(
                action,
                spec,
                crate::virtual_display_ui::LocalOwner::Background,
            )
        })();
        match result {
            Ok(keep) => std::process::exit(10 + u32::from(keep) as i32),
            Err(error) => {
                let unknown = error
                    .downcast_ref::<uu_core::service_admin::SubmittedUnknown>()
                    .is_some();
                show_error(&format!("{error:#}"));
                std::process::exit(if unknown { 12 } else { 1 });
            }
        }
    }
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 3 && args[0] == "--configure-streaming-firewall" {
        let result = (|| -> anyhow::Result<()> {
            require_administrator()?;
            let exe = std::env::current_exe()?;
            uu_core::service_admin::package::verify(
                exe.parent()
                    .ok_or_else(|| anyhow::anyhow!("cannot determine package directory"))?,
                env!("UU_BUILD_COMMIT"),
            )?;
            let service = args[1]
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid service port"))?
                .parse::<u16>()?;
            let discovery = args[2]
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid discovery port"))?
                .parse::<u16>()?;
            uu_core::firewall::ensure(&exe, service, discovery)?;
            Ok(())
        })();
        if let Err(error) = result {
            show_error(&format!("Firewall: {error:#}"));
            std::process::exit(1);
        }
        return Some(Ok(()));
    }
    if args == ["--prepare-virtual-display-driver"] {
        let result = (|| -> anyhow::Result<()> {
            require_administrator()?;
            let exe = std::env::current_exe()?;
            uu_core::service_admin::package::verify(
                exe.parent()
                    .ok_or_else(|| anyhow::anyhow!("{}", uu_i18n::format!("无法确定安装包目录")))?,
                env!("UU_BUILD_COMMIT"),
            )?;
            uu_core::virtual_display::prepare()?;
            Ok(())
        })();
        if let Err(error) = result {
            let error = format!("{error:#}");
            show_error(&uu_i18n::format!("准备虚拟显示器失败：{error}"));
            std::process::exit(1);
        }
        return Some(Ok(()));
    }
    if args.len() == 2 && args[0] == "--install-virtual-display-driver" {
        let result =
            super::virtual_display_install::install_downloaded(std::path::Path::new(&args[1]));
        if let Err(error) = result {
            show_error(&uu_i18n::format!("安装虚拟显示器驱动失败：{error}"));
            std::process::exit(1);
        }
        return Some(Ok(()));
    }
    if args == ["--manage-background-service"] || args == ["--manage-virtual-displays"] {
        let result = run_manager(args == ["--manage-virtual-displays"]);
        if let Err(error) = &result {
            show_error(&uu_i18n::format!("无法打开后台服务管理：{error}"));
        }
        return Some(result);
    }
    let virtual_displays = args == ["--open-virtual-display-manager"];
    let opening = args == ["--open-background-service-manager"] || virtual_displays;
    let action = if args.len() == 4
        && args[0] == "--background-service-action"
        && args[2] == "--source-data"
    {
        args[1]
            .to_str()
            .filter(|s| matches!(*s, "install" | "import-plugins"))
    } else {
        None
    };
    if !opening && action.is_none() {
        return None;
    }
    let result = (|| -> anyhow::Result<()> {
        let exe = std::env::current_exe()?;
        if opening {
            std::process::Command::new(exe)
                .arg(if virtual_displays {
                    "--manage-virtual-displays"
                } else {
                    "--manage-background-service"
                })
                .spawn()?;
        } else if let Some(action) = action {
            require_administrator()?;
            use std::os::windows::process::CommandExt;
            uu_core::service_admin::package::verify(
                exe.parent()
                    .ok_or_else(|| anyhow::anyhow!("{}", uu_i18n::format!("无法确定安装包目录")))?,
                env!("UU_BUILD_COMMIT"),
            )?;
            let mut command = std::process::Command::new(exe.with_file_name("eclipse-cli.exe"));
            command
                .args(["service", action, "--source-data"])
                .arg(&args[3]);
            let output = command.creation_flags(0x08000000).output()?;
            anyhow::ensure!(
                output.status.success(),
                "{}",
                uu_i18n::format!(
                    "后台服务操作失败（{}）：\n{}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                )
            );
        }
        Ok(())
    })();
    if let Err(error) = result {
        show_error(&format!("{error:#}"));
        std::process::exit(1);
    }
    Some(Ok(()))
}
fn show_error(message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(message).as_ptr(),
            wide(uu_i18n::text("Eclipse · 后台服务")).as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (
        Manager,
        mpsc::SyncSender<Event>,
        mpsc::Receiver<Job>,
        Arc<AtomicBool>,
    ) {
        let (sender, jobs) = mpsc::sync_channel(8);
        let (send, events) = mpsc::sync_channel(16);
        let stop = Arc::new(AtomicBool::new(false));
        (
            Manager::empty(sender, events, stop.clone()),
            send,
            jobs,
            stop,
        )
    }
    fn state(ready: bool, settings: Sharing) -> Event {
        Event::State {
            state: SERVICE_RUNNING,
            build: None,
            response: Ok(Response::Status {
                ready,
                keep_virtual_displays: Some(true),
                endpoints: vec![],
                pending: vec![],
                snapshot: host_admin::Snapshot {
                    own_permissions: Permissions::CONTROL,
                    connections: vec![],
                    server: Some(host_admin::ServerSettings {
                        active: settings.clone(),
                        saved: settings,
                        revision: "a".repeat(64),
                        monitors: vec![],
                        portal: false,
                    }),
                },
            }),
        }
    }
    #[test]
    fn tray_management_selection_survives_service_manager_initialization() {
        let ctx=egui::Context::default();let mut entry=Entry::default();
        entry.open_virtual_displays(&ctx);
        assert_eq!(entry.requested_section,Some(6));
        let (manager,_send,_jobs,_stop)=fixture();
        entry.manager=Some(manager);
        entry.poll(&ctx,&mut Default::default());
        assert_eq!(entry.manager.as_ref().unwrap().section,6);
        assert!(entry.requested_section.is_none());
        entry.open_pairing();
        assert_eq!(entry.manager.as_ref().unwrap().section,4);
    }
    #[test]
    fn service_readiness_and_lost_connection_control_the_online_state() {
        let (manager, send, _jobs, _stop) = fixture();
        let mut entry = Entry {
            manager: Some(manager),
            ..Default::default()
        };
        send.send(state(false, Sharing::default())).unwrap();
        entry.manager.as_mut().unwrap().poll();
        assert!(
            !entry.online(),
            "SCM running alone is not a ready streaming service"
        );
        send.send(state(true, Sharing::default())).unwrap();
        entry.manager.as_mut().unwrap().poll();
        assert!(entry.online());
        assert_eq!(entry.keep_displays(), Some(true));
        send.send(Event::State {
            state: SERVICE_STOPPED,
            build: None,
            response: Err("generated stopped service".into()),
        })
        .unwrap();
        entry.manager.as_mut().unwrap().poll();
        assert!(!entry.online());
        assert_eq!(entry.keep_displays(), None);
        assert!(
            entry.manager.as_ref().unwrap().snapshot.is_none(),
            "offline configuration must never be replaced with a local user's host configuration"
        );
    }
    #[test]
    fn service_refresh_preserves_drafts_and_conflicts_while_closing_only_retires_the_gui_worker() {
        let (mut manager, send, jobs, stop) = fixture();
        send.send(state(true, Sharing::default())).unwrap();
        manager.poll();
        manager.draft.audio = false;
        manager.dirty = true;
        let mut elsewhere = Sharing::default();
        elsewhere.name = "Another manager".into();
        send.send(state(true, elsewhere)).unwrap();
        manager.poll();
        assert!(!manager.draft.audio);
        assert_ne!(manager.draft.name, "Another manager");
        manager.save(false);
        let Job::Rpc(Request::Admin {
            request:
                host_admin::Request::Save {
                    revision,
                    settings,
                    restart,
                },
        }) = jobs.try_recv().unwrap()
        else {
            panic!("save must go to the service")
        };
        assert_eq!(revision, "a".repeat(64));
        assert!(!settings.audio);
        assert!(!restart);
        assert!(manager.busy);
        send.send(Event::Trust {
            completed: false,
            result: Err("generated periodic inspection".into()),
        })
        .unwrap();
        manager.poll();
        assert!(
            manager.busy,
            "a periodic inspection is not a save completion"
        );
        send.send(Event::Done(Err("generated revision conflict".into())))
            .unwrap();
        manager.poll();
        assert!(manager.dirty);
        assert!(!manager.busy);
        send.send(state(true, Sharing::default())).unwrap();
        manager.poll();
        let message = manager
            .feedback
            .current(&crate::ui_feedback::Scope::Services(0))
            .expect("completion message");
        assert_eq!(message.text, "generated revision conflict");
        assert_eq!(message.kind, crate::ui_feedback::Kind::Error);
        drop(manager);
        assert!(stop.load(Ordering::Acquire));
        assert!(
            matches!(jobs.try_recv(), Err(mpsc::TryRecvError::Disconnected)),
            "closing a GUI must not enqueue stop or restart"
        );
    }
    #[test]
    fn service_failures_keep_their_type_and_original_tab_without_matching_message_words() {
        let message = "generated revision conflict".to_owned();
        let events = [
            Event::Done(Err(message.clone())),
            Event::Done(Ok(Response::Error { message: message.clone() })),
            Event::Done(Ok(Response::Admin {
                response: host_admin::Response::Error { message: message.clone() },
            })),
            Event::Plugins { completed: true, result: Err(message.clone()) },
            Event::Plugins { completed: true, result: Ok(Response::Error { message: message.clone() }) },
            Event::Trust { completed: true, result: Err(message.clone()) },
            Event::Trust { completed: true, result: Ok(Response::Error { message: message.clone() }) },
            Event::Connections { completed: true, result: Err(message.clone()) },
            Event::Connections { completed: true, result: Ok(Response::Error { message: message.clone() }) },
        ];
        for event in events {
            let (mut manager, send, _jobs, _stop) = fixture();
            manager.busy = true;
            manager.dirty = true;
            manager.job_section = Some(2);
            manager.section = 6;
            send.send(event).unwrap();
            manager.poll();
            assert!(!manager.busy);
            assert!(manager.dirty);
            let notice = manager.feedback.current(&crate::ui_feedback::Scope::Services(2)).unwrap();
            assert_eq!(notice.text, message);
            assert_eq!(notice.kind, crate::ui_feedback::Kind::Error);
            assert!(manager.feedback.current(&crate::ui_feedback::Scope::Services(6)).is_none());
        }
    }
    #[test]
    fn background_refresh_never_locks_service_controls() {
        let (mut manager, _send, jobs, _stop) = fixture();
        manager.submit(Job::Refresh);
        assert!(!manager.busy, "refresh has no completion event and must stay background");
        assert!(matches!(jobs.try_recv(), Ok(Job::Refresh)));

        manager.busy = true;
        manager.submit(Job::Refresh);
        assert!(manager.busy, "a background refresh must not clear a real foreground operation");
        assert!(matches!(jobs.try_recv(), Ok(Job::Refresh)));
    }
    #[test]
    fn firewall_result_stays_on_its_own_tab_and_is_informational() {
        let (mut manager, send, jobs, _stop) = fixture();
        manager.section = 0;
        manager.submit(Job::Rpc(Request::Firewall { repair: false }));
        assert!(matches!(jobs.try_recv(), Ok(Job::Rpc(Request::Firewall { repair: false }))));
        manager.section = 5;
        send.send(Event::Done(Ok(Response::Firewall {
            message: "已检查防火墙：Generated result".into(),
        })))
        .unwrap();
        manager.poll();
        let message = manager
            .feedback
            .current(&crate::ui_feedback::Scope::Services(0))
            .expect("firewall result belongs to the service-management tab");
        assert_eq!(message.kind, crate::ui_feedback::Kind::Info);
        assert!(
            manager
                .feedback
                .current(&crate::ui_feedback::Scope::Services(5))
                .is_none(),
            "a firewall result must not appear above another service tab"
        );
    }
    #[test]
    fn unified_service_controls_and_long_notices_fit_narrow_columns() {
        for width in [340., 560.] {
            let (mut manager, send, _jobs, _stop) = fixture();
            send.send(state(true, Sharing::default())).unwrap();
            manager.poll();
            manager.feedback.error(
                crate::ui_feedback::Scope::Services(0),
                "Generated long service detail ".repeat(16),
            );
            let mut entry = Entry {
                manager: Some(manager),
                ..Default::default()
            };
            let mut host = super::super::sharing_ui::HostUi::default();
            let ctx = egui::Context::default();
            crate::theme::apply_appearance(
                &ctx,
                egui::ThemePreference::Dark,
                crate::theme::COLOR_THEMES[0],
            );
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 1000.),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let right = ui.max_rect().right();
                        entry.show(ui, &mut host, true);
                        assert!(ui.min_rect().right() <= right + 1.);
                    });
                },
            );
            let text = output
                .shapes
                .iter()
                .filter_map(|shape| {
                    if let egui::Shape::Text(text) = &shape.shape {
                        Some(text.galley.text())
                    } else {
                        None
                    }
                })
                .collect::<String>();
            for action in ["重新部署服务", "停止服务", "虚拟显示器"] {
                assert!(
                    text.contains(uu_i18n::text(action)),
                    "missing service control: {action}"
                );
            }
        }
    }

    fn drawn_text(output: &egui::FullOutput) -> String {
        fn visit(shape: &egui::Shape, output: &mut String) {
            match shape {
                egui::Shape::Text(text) => output.push_str(text.galley.text()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        visit(shape, output);
                    }
                }
                _ => {}
            }
        }
        let mut text = String::new();
        for shape in &output.shapes {
            visit(&shape.shape, &mut text);
        }
        text
    }
    fn draw_service(entry: &mut Entry, host: &mut super::super::sharing_ui::HostUi, ctx: &egui::Context) -> String {
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000., 1400.))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    entry.show(ui, host, true);
                });
            },
        );
        drawn_text(&output)
    }
    #[test]
    fn service_status_card_is_only_rendered_in_the_service_management_tab() {
        let (mut manager, send, _jobs, _stop) = fixture();
        send.send(state(false, Sharing::default())).unwrap();
        manager.poll();
        manager.section = 0;
        let mut entry = Entry { manager: Some(manager), ..Default::default() };
        let mut host = super::super::sharing_ui::HostUi::default();
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let service = draw_service(&mut entry, &mut host, &ctx);
        assert!(service.contains(uu_i18n::text("Eclipse 服务")));
        assert!(service.contains(uu_i18n::text("后台服务未启动或尚未就绪")));
        entry.manager.as_mut().unwrap().section = 1;
        let input = draw_service(&mut entry, &mut host, &ctx);
        assert!(input.contains(uu_i18n::text("服务管理")));
        assert!(!input.contains(uu_i18n::text("Eclipse 服务")));
        assert!(!input.contains(uu_i18n::text("后台服务未启动或尚未就绪")));
    }

    struct Harness {
        manager: Manager,
        send: mpsc::SyncSender<Event>,
        jobs: mpsc::Receiver<Job>,
        ctx: egui::Context,
        labels: Vec<(String, egui::Pos2)>,
    }
    impl Harness {
        fn new() -> Self {
            let (mut manager, send, jobs, _) = fixture();
            send.send(state(true, Sharing::default())).unwrap();
            send.send(Event::Trust {
                completed: false,
                result: Ok(Response::Trust {
                    snapshot: TrustSnapshot {
                        device: PublicDevice {
                            name: "Generated service".into(),
                            public_key: "ab".repeat(32),
                        },
                        peers: vec![],
                        password_permissions: None,
                    },
                }),
            })
            .unwrap();
            send.send(Event::Connections {
                completed: false,
                result: Ok(Response::Connections {
                    snapshot: uu_core::service_admin::connections::Snapshot {
                        revision: "service-revision".into(),
                        entries: vec![],
                        restart_required: false,
                    },
                }),
            })
            .unwrap();
            manager.poll();
            let ctx = egui::Context::default();
            crate::theme::apply(&ctx);
            Self {
                manager,
                send,
                jobs,
                ctx,
                labels: vec![],
            }
        }
        fn frame(&mut self, events: Vec<egui::Event>) {
            self.manager.poll();
            let output = self.ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000., 1400.),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| { self.manager.show_tabs(ui); self.manager.show_body(ui); });
                },
            );
            fn visit(shape: &egui::Shape, labels: &mut Vec<(String, egui::Pos2)>) {
                match shape {
                    egui::Shape::Text(t) => labels.push((
                        t.galley.text().into(),
                        t.pos + t.galley.rect.center().to_vec2(),
                    )),
                    egui::Shape::Vec(shapes) => {
                        for shape in shapes {
                            visit(shape, labels);
                        }
                    }
                    _ => {}
                }
            }
            self.labels.clear();
            for shape in output.shapes {
                visit(&shape.shape, &mut self.labels);
            }
        }
        fn has(&self, label: &str) -> bool {
            self.labels.iter().any(|(text, _)| text == label)
        }
        fn click(&mut self, label: &str) {
            self.frame(vec![]);
            self.frame(vec![]);
            let pos = self
                .labels
                .iter()
                .find(|(text, _)| text == label)
                .unwrap_or_else(|| panic!("missing service control: {label}"))
                .1;
            let event = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            self.frame(vec![egui::Event::PointerMoved(pos), event(true)]);
            self.frame(vec![event(false)]);
            self.frame(vec![]);
        }
    }
    #[test]
    fn service_tabs_keep_authority_and_pairing_forms_in_their_own_sections() {
        let mut h = Harness::new();
        h.frame(vec![]);
        h.frame(vec![]);
        assert!(!h.has("IP 直连"));
        h.click("连接服务");
        assert!(h.has("IP 直连"));
        assert!(h.has("＋ 添加服务器"));
        h.click("配对设置");
        assert!(h.has("设备名称"));
        assert!(h.has("密码配对"));
        assert!(!h.has("添加授权公钥"));
        h.manager.pairing.password = zeroize::Zeroizing::new("generated-password".into());
        h.click("公钥配对");
        assert!(h.has("添加授权公钥"));
        assert!(!h.has("保存安全连接密码"));
        assert_eq!(&*h.manager.pairing.password, "generated-password");
        h.click("输入");
        assert!(!h.has("密码配对"));
        assert!(!h.has("＋ 添加服务器"));
        h.click("虚拟显示器");
        assert!(h.has("虚拟显示器上限"));
        assert!(h.has("新建虚拟显示器"));
        assert!(!h.has("管理虚拟显示器"));
        assert!(!h.has("管理对象"));
    }
    #[test]
    fn empty_session_list_keeps_the_section_and_explains_the_empty_state() {
        let mut h = Harness::new();
        h.frame(vec![]);
        assert!(h.has("会话"));
        assert!(h.has("当前没有连接的会话。设备连接后，可在这里查看和管理会话。"));
        assert!(!h.has("已连接 0 个会话"));
    }
    #[test]
    fn service_connection_form_queues_only_owner_rpc_and_keeps_failed_fields() {
        let mut h = Harness::new();
        h.click("连接服务");
        h.click("＋ 添加服务器");
        h.click("保存服务器");
        assert!(h.has("请填写服务器访问令牌"));
        assert!(matches!(h.jobs.try_recv(), Err(mpsc::TryRecvError::Empty)));
        h.send
            .send(Event::Connections {
                completed: true,
                result: Err("generated backend error".into()),
            })
            .unwrap();
        h.frame(vec![]);
        assert!(h.has("保存服务器"));
        assert!(h.has("generated backend error"));
        h.click("取消");
        assert!(!h.has("保存服务器"));
    }
    #[test]
    fn periodic_plugin_snapshots_do_not_overwrite_an_unsaved_service_draft() {
        let (mut manager,_,_,_)=fixture();
        let initial=uu_core::plugins::PluginPreferences{enabled:false,options:Default::default()};
        manager.apply_plugin_snapshot(PluginSnapshot{directory:"generated".into(),plugins:vec![PluginInfo{id:"fixture".into(),name:"Fixture".into(),version:"1".into(),enabled:false,compatible:true,error:String::new(),preferences:initial.clone()}]});
        let draft=manager.plugin_drafts.get_mut("fixture").unwrap();draft.value.enabled=true;draft.dirty=true;
        let remote=uu_core::plugins::PluginPreferences{enabled:false,options:serde_json::from_str(r#"{"remote":true}"#).unwrap()};
        manager.apply_plugin_snapshot(PluginSnapshot{directory:"generated".into(),plugins:vec![PluginInfo{id:"fixture".into(),name:"Fixture".into(),version:"1".into(),enabled:false,compatible:true,error:String::new(),preferences:remote.clone()}]});
        let draft=manager.plugin_drafts.get("fixture").unwrap();assert!(draft.dirty&&draft.value.enabled);assert_eq!(draft.expected,initial);
        manager.plugin_drafts.get_mut("fixture").unwrap().dirty=false;
        manager.apply_plugin_snapshot(PluginSnapshot{directory:"generated".into(),plugins:vec![PluginInfo{id:"fixture".into(),name:"Fixture".into(),version:"1".into(),enabled:false,compatible:true,error:String::new(),preferences:remote.clone()}]});
        assert_eq!(manager.plugin_drafts.get("fixture").unwrap().expected,remote);
    }
}
