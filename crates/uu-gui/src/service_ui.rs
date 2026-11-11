//! Windows GUI manages the one installed host. All host configuration and trust
//! mutations are service RPCs; dropping this frontend never stops the service.
use eframe::egui;
use std::{
    collections::HashMap,
    sync::mpsc,
    time::{Duration, Instant},
};
use uu_core::{
    device_book::Sharing,
    device_trust::{Permissions, PublicDevice},
    host_admin,
    service_admin::{windows::*, PluginRequest, PluginSnapshot, Request, Response, TrustRequest, TrustSnapshot},
};
use windows_sys::Win32::System::Services::{SERVICE_RUNNING, SERVICE_STOPPED};
#[cfg(test)]
use std::sync::{atomic::{AtomicBool, Ordering}, Arc};
mod connections;
mod deployment;
mod worker;
use deployment::{automatic_restart_ready, Deployment, DeploymentOutcome};
#[cfg(test)]
use deployment::automatic_restart_safe;
use worker::{Event, Job};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
enum Section {
    #[default]
    Management = 0,
    Input = 1,
    AudioVideo = 2,
    Encoder = 3,
    Pairing = 4,
    Connections = 5,
    VirtualDisplays = 6,
}
impl Section {
    fn scope(self) -> crate::ui_feedback::Scope { crate::ui_feedback::Scope::Services(self as u8) }
}

#[derive(Default)]
pub(super) struct Entry {
    manager: Option<Manager>,
    requested_section: Option<Section>,
    deployment: Option<Deployment>,
    job: Option<mpsc::Receiver<Result<DeploymentOutcome, String>>>,
    feedback: super::ui_feedback::Feedback,
    loaded_plugins: Vec<uu_plugin_sdk::Manifest>,
    plugin_preferences: Option<uu_core::plugins::Preferences>,
    automatic_update_attempted: bool,
    automatic_plugin_attempts: std::collections::BTreeSet<String>,
    automatic_sync_enabled: bool,
    automatic_job: bool,
    retry_after: Option<Instant>,
    deployment_error: String,
}
impl Entry {
    pub fn set_loaded_plugins(&mut self,plugins:Vec<uu_plugin_sdk::Manifest>,preferences:Option<uu_core::plugins::Preferences>,automatic_sync:bool) {
        self.loaded_plugins=plugins;
        self.plugin_preferences=preferences;
        // A test/portable profile must not silently replace the machine-wide
        // host or publish its fresh plugin preferences to the existing host.
        self.automatic_sync_enabled=automatic_sync;
    }
    pub fn poll(&mut self, ctx: &egui::Context, _host: &mut super::sharing_ui::HostUi) -> bool {
        if self.manager.is_none() {
            match Manager::new() {
                Ok(manager) => self.manager = Some(manager),
                Err(error) => self.feedback.error(super::ui_feedback::Scope::Services(0), error.to_string()),
            }
        }
        if let Some(manager) = &mut self.manager {
            manager.loaded_plugins=self.loaded_plugins.clone();
            if let Some(section)=self.requested_section.take() {manager.section=section;}
            manager.poll();
            if let Some(deployment)=manager.deployment.take() {self.deployment=Some(deployment);}
        }
        if !cfg!(test) && self.automatic_sync_enabled && self.job.is_none() && self.deployment.is_none()
            && self.retry_after.is_none_or(|at|Instant::now()>=at) {
            self.schedule_automatic_sync();
        }
        let mut installed = false;
        if let Some(job) = &self.job {
            let completed=match job.try_recv() {
                Ok(result)=>Some(result),
                Err(mpsc::TryRecvError::Empty)=>None,
                Err(mpsc::TryRecvError::Disconnected)=>Some(Err("Eclipse update worker exited without a completion result".into())),
            };
            if let Some(result) = completed {
                self.job = None;
                match result {
                    Ok(DeploymentOutcome::Applied) => {
                        installed = true;
                        self.feedback.success(
                            super::ui_feedback::Scope::Services(0),
                            uu_i18n::text("服务程序操作已完成"),
                        );
                        if let Some(manager) = &mut self.manager {
                            manager.submit(Job::Refresh);
                        }
                    }
                    Ok(DeploymentOutcome::Cancelled) => self.feedback.report(
                        super::ui_feedback::Scope::Services(0),
                        super::ui_feedback::Kind::Info,
                        uu_i18n::text("已取消系统授权，服务尚未更新"),
                    ),
                    Ok(DeploymentOutcome::Deferred) => {
                        self.automatic_update_attempted=false;
                        self.automatic_plugin_attempts.clear();
                        self.retry_after=Some(Instant::now()+Duration::from_secs(5));
                        self.feedback.report(super::ui_feedback::Scope::Services(0),super::ui_feedback::Kind::Progress,
                            uu_i18n::text("更新正在等待串流结束或临时显示器关闭；未完成的显示器操作也需要先处理。"));
                    }
                    Err(error) => {
                        self.deployment_error=error;
                        self.feedback.error(super::ui_feedback::Scope::Services(0),uu_i18n::text("更新尚未完成，请重试或查看诊断详情。"));
                    }
                }
            }
        }
        if let Some(deployment) = self.deployment.take() {
            let automatic=std::mem::take(&mut self.automatic_job);
            if matches!(deployment,Deployment::Install) {self.automatic_update_attempted=true;}
            let (send, receive) = mpsc::channel();
            self.job = Some(receive);
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let result = deployment::execute(deployment, automatic);
                let _ = send.send(result);
                ctx.request_repaint();
            });
        }
        self.feedback.tick(ctx);
        ctx.request_repaint_after(Duration::from_millis(500));
        installed
    }
    pub fn keep_displays(&self) -> Option<bool> {
        self.manager.as_ref().filter(|m| m.owns_profile()).and_then(|m| m.keep_displays)
    }
    fn schedule_automatic_sync(&mut self) {
        if !self.automatic_sync_enabled {return;}
        let Some(manager)=&self.manager else{return;};
        if !manager.owns_profile() { return; }
        if manager.busy || manager.confirm.is_some() || manager.dirty || manager.state==0 {return;}
        let idle=manager.state==SERVICE_STOPPED || manager.snapshot.as_ref().is_some_and(|s|s.connections.is_empty());
        if !idle {return;}
        if manager.update_needed() {
            if !self.automatic_update_attempted {
                self.automatic_update_attempted=true;
                self.automatic_job=true;
                self.deployment=Some(Deployment::Install);
            }
        } else if let (Some(snapshot),Some(preferences))=(&manager.plugins,&self.plugin_preferences) {
            let missing=manager.loaded_plugins.iter().filter(|loaded|!snapshot.plugins.iter().any(|p|p.id==loaded.id&&p.enabled&&p.compatible&&p.version==loaded.version)).collect::<Vec<_>>();
            let changed=snapshot.plugins.iter().find(|p| {
                (!preferences.get(&p.id).enabled&&p.preferences.enabled)
                    || (manager.loaded_plugins.iter().any(|loaded|loaded.id==p.id&&loaded.version==p.version)&&p.compatible&&p.preferences!=preferences.get(&p.id))
            });
            let key=format!("{}:{}:{:?}",missing.iter().map(|p|format!("{}:{}",p.id,p.library_sha256)).collect::<Vec<_>>().join("|"),serde_json::to_string(preferences).unwrap_or_default(),changed.map(|p|&p.id));
            if (!missing.is_empty()||changed.is_some())&&self.automatic_plugin_attempts.insert(key) {
                if let Some(plugin)=changed {
                    let request=PluginRequest::Save{id:plugin.id.clone(),expected:plugin.preferences.clone(),value:preferences.get(&plugin.id)};
                    self.manager.as_mut().unwrap().submit(Job::Rpc(Request::Plugins{request}));
                }else if !missing.is_empty() {
                    self.automatic_job=true;
                    self.deployment=Some(Deployment::ImportPlugins);
                }
            }
        }
    }
    pub fn password_configured(&self) -> Option<bool> {
        self.manager
            .as_ref()
            .filter(|m| m.snapshot.is_some() && m.owns_profile())
            .and_then(|m| m.trust.as_ref())
            .map(|t| t.password_permissions.is_some())
    }
    pub fn open_pairing(&mut self) {
        self.select_section(Section::Pairing);
    }
    fn select_section(&mut self,section:Section) {
        self.requested_section=Some(section);
        if let Some(manager) = &mut self.manager {
            manager.section = section;
            self.requested_section=None;
        }
    }
    pub fn open_virtual_displays(&mut self, _ctx: &egui::Context) {
        self.select_section(Section::VirtualDisplays);
    }
    pub fn online(&self) -> bool {
        self.manager.as_ref().is_some_and(|m| m.ready && m.owns_profile())
    }
    pub fn toggle(&mut self) {
        if let Some(manager) = &mut self.manager {
            if !manager.checked {
                return;
            }
            if !manager.owns_profile() {
                if manager.switch_ready() { manager.request_action(Confirmation::Update); }
                return;
            }
            if manager.state == 0 {
                self.deployment = Some(Deployment::Install);
            } else if manager.state == SERVICE_RUNNING {
                manager.request_action(Confirmation::Stop);
            } else if manager.state == SERVICE_STOPPED {
                manager.submit(Job::Start);
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
            if !self.deployment_error.is_empty() {
                ui.collapsing(uu_i18n::text("诊断详情"),|ui|{ui.label(&self.deployment_error);});
            }
            return installed;
        }
        if let Some(manager) = &mut self.manager {
            if !manager.owns_profile() {
                self.show_status_card(ui, available);
                return installed;
            }
            if self.job.is_some() {ui.horizontal(|ui|{ui.spinner();ui.label(uu_i18n::text("正在完成更新…"));});}
            manager.show_tabs(ui);
        }
        let section = self.manager.as_ref().map_or(Section::Management, |manager| manager.section);
        if section == Section::Management {
            self.show_status_card(ui, available);
        }
        if let Some(manager) = &mut self.manager {
            ui.add_enabled_ui(self.job.is_none(),|ui|manager.show_body(ui));
        }
        installed
    }

    fn show_status_card(&mut self, ui: &mut egui::Ui, available: bool) {
        let busy = self.job.is_some() || self.manager.as_ref().is_some_and(|manager| manager.busy || manager.confirm.is_some());
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
                let outdated = manager.update_needed();
                let own_profile = manager.owns_profile();
                if !own_profile {
                    ui.colored_label(crate::theme::palette(ui.visuals()).warning, uu_i18n::text("本机服务正在使用其它配置；切换后才会使用本目录的服务端设置和配对。"));
                    if !manager.switch_ready() { ui.weak(uu_i18n::text("请先结束当前服务的串流，再切换配置。")); }
                }
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
                                !busy && (own_profile || manager.switch_ready()),
                                crate::theme::secondary_button(if !own_profile {
                                    uu_i18n::text("切换到此配置")
                                } else if outdated {
                                    uu_i18n::text("完成更新")
                                } else {
                                    uu_i18n::text("重新部署服务")
                                }),
                            )
                            .clicked()
                        {
                            manager.request_action(Confirmation::Update);
                        }
                        if ui
                            .add_enabled(
                                !busy
                                    && own_profile
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
                                manager.submit(Job::Start);
                            } else {
                                manager.request_action(Confirmation::Stop);
                            }
                        }
                    }
                });
                if outdated && own_profile {
                    ui.colored_label(
                        crate::theme::palette(ui.visuals()).warning,
                        uu_i18n::text(if self.automatic_sync_enabled {
                            "Eclipse 更新尚未完成；空闲时会自动继续，Windows 可能请求系统授权。"
                        }else {"本机服务与当前便携版本不同；需要使用此版本提供共享时，请手动更新服务。"}),
                    );
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
    section_frame(ui, |ui| {
        ui.strong(title);
        ui.add_space(8.);
        contents(ui);
    });
}
fn section_frame(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    let frame = crate::theme::card(ui.visuals());
    let width = (ui.available_width()
        - frame.inner_margin.sum().x
        - frame.outer_margin.sum().x
        - 2. * frame.stroke.width)
        .max(1.);
    frame.show(ui, |ui| {
        ui.set_width(width);
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
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
#[derive(Clone)]
enum Confirmation {
    Update,
    ImportPlugins,
    Stop,
    Restart,
}
struct Manager {
    expected_profile: Option<String>,
    allow_legacy_profile: bool,
    virtual_display: super::virtual_display_ui::State,
    worker: worker::Worker,
    state: u32,
    build: Option<BuildStatus>,
    ready: bool,
    checked: bool,
    connection_error: String,
    keep_displays: Option<bool>,
    snapshot: Option<host_admin::Snapshot>,
    trust: Option<TrustSnapshot>,
    plugins:Option<PluginSnapshot>,
    service_connections: connections::State,
    endpoints: Vec<(String, String)>,
    pending: Vec<uu_core::service_admin::Pairing>,
    draft: Sharing,
    revision: String,
    dirty: bool,
    busy: bool,
    section: Section,
    feedback: super::ui_feedback::Feedback,
    job_section: Option<Section>,
    codes: HashMap<String, zeroize::Zeroizing<String>>,
    permissions: HashMap<String, Permissions>,
    pairing: ServicePairing,
    confirm: Option<Confirmation>,
    deployment: Option<Deployment>,
    loaded_plugins: Vec<uu_plugin_sdk::Manifest>,
}
impl Manager {
    fn new() -> anyhow::Result<Self> {
        let config = uu_core::Config::from_env()?;
        let expected_profile = uu_core::service_profile::directory_key(&config.data_dir)?;
        let allow_legacy_profile = false;
        let mut manager = Self::with_worker(worker::Worker::start());
        manager.expected_profile = Some(expected_profile);
        manager.allow_legacy_profile = allow_legacy_profile;
        Ok(manager)
    }
    #[cfg(test)]
    fn empty(sender: mpsc::SyncSender<Job>, events: mpsc::Receiver<Event>, stop: Arc<AtomicBool>) -> Self {
        Self::with_worker(worker::Worker::from_channels(sender, events, stop))
    }
    fn with_worker(worker: worker::Worker) -> Self {
        Self {
            expected_profile: None,
            allow_legacy_profile: true,
            virtual_display: Default::default(),
            worker,
            state: 0,
            build: None,
            ready: false,
            checked: false,
            connection_error: String::new(),
            keep_displays: None,
            snapshot: None,
            trust: None,
            plugins:None,
            service_connections: Default::default(),
            endpoints: vec![],
            pending: vec![],
            draft: Sharing::default(),
            revision: String::new(),
            dirty: false,
            busy: false,
            section: Section::Management,
            feedback: Default::default(),
            job_section: None,
            codes: HashMap::new(),
            permissions: HashMap::new(),
            pairing: Default::default(),
            confirm: None,
            deployment: None,
            loaded_plugins: Vec::new(),
        }
    }
    fn submit(&mut self, job: Job) {
        let foreground = job.foreground();
        if foreground && self.busy {
            return;
        }
        match self.worker.sender.try_send(job) {
            Ok(()) => {
                if foreground {
                    self.busy = true;
                    self.job_section = Some(self.section);
                }
            }
            Err(error) => self.feedback.error(
                self.section.scope(),
                error.to_string(),
            ),
        }
    }
    fn completion_scope(&mut self) -> super::ui_feedback::Scope {
        self.job_section.take().unwrap_or(self.section).scope()
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
            let event = match self.worker.events.try_recv() {
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
                    let response = response.map(|response| match response {
                        Response::ProfileStatus { profile, response } => {
                            if let Some(build) = &mut self.build { build.profile=Some(profile.id); }
                            *response
                        },
                        response => response,
                    });
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
    fn configuration_actions(&mut self, ui:&mut egui::Ui) {
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
                self.request_action(Confirmation::Restart);
            }
        });
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
    fn request_action(&mut self, action:Confirmation) {
        if self.busy || self.confirm.is_some() || self.deployment.is_some() {return;}
        if self.state==SERVICE_STOPPED || self.snapshot.as_ref().is_some_and(|s|s.connections.is_empty()) {
            self.apply_action(action);
        } else {
            self.confirm=Some(action);
        }
    }
    fn update_needed(&self)->bool {
        !self.owns_profile() || self.build.as_ref().is_some_and(|build| {
            build.installed.as_ref().is_some_and(|version|version.revision!=env!("UU_BUILD_COMMIT"))
                || build.running.as_ref().is_some_and(|version|version.revision!=env!("UU_BUILD_COMMIT"))
        })
    }
    fn owns_profile(&self) -> bool {
        let Some(expected) = &self.expected_profile else { return true; };
        match self.build.as_ref().and_then(|build|build.profile.as_ref()) {
            Some(active) => active == expected,
            None => self.allow_legacy_profile,
        }
    }
    fn switch_ready(&self) -> bool {
        self.state == SERVICE_STOPPED || self.snapshot.as_ref().is_some_and(|s|s.connections.is_empty())
    }
    fn apply_action(&mut self, action:Confirmation) {
        match action {
            Confirmation::Update=>self.deployment=Some(Deployment::Install),
            Confirmation::ImportPlugins=>self.deployment=Some(Deployment::ImportPlugins),
            Confirmation::Stop=>self.submit(Job::Stop),
            Confirmation::Restart=>self.save(true),

        }
    }
    fn show_confirmation(&mut self,ctx:&egui::Context) {
        if self.confirm.is_none(){return;}
        if self.state==SERVICE_STOPPED || self.snapshot.as_ref().is_some_and(|s|s.connections.is_empty()) {
            if let Some(action)=self.confirm.take(){self.apply_action(action);}
            return;
        }
        let mut accept=false;let mut cancel=false;
        egui::Window::new(uu_i18n::text("确认继续"))
            .id(egui::Id::new("service-action-confirmation"))
            .anchor(egui::Align2::CENTER_CENTER,egui::Vec2::ZERO)
            .collapsible(false).resizable(false).default_width(420.).show(ctx,|ui|{
                if let Some(snapshot)=&self.snapshot {
                    ui.label(uu_i18n::format!("此操作会断开当前 {} 个串流会话。",snapshot.connections.len()));
                }else{ui.label(uu_i18n::text("无法确认当前连接数；此操作可能会断开正在进行的串流。"));}
                ui.horizontal(|ui|{
                    accept=ui.add(crate::theme::primary_button(uu_i18n::text("确认继续"))).clicked();
                    cancel=ui.add(crate::theme::secondary_button(uu_i18n::text("取消"))).clicked();
                });
            });
        if accept {if let Some(action)=self.confirm.take(){self.apply_action(action);}}
        else if cancel {self.confirm=None;}
    }
    fn apply_plugin_snapshot(&mut self,snapshot:PluginSnapshot) {
        self.plugins=Some(snapshot);
    }
    fn plugins_ui(&mut self,ui:&mut egui::Ui,enabled:bool) {
        // The user's loaded extensions define the product surface. Stale
        // protected files, disabled extensions and SDK failures do not add UI.
        let providers=uu_core::plugins::registry().descriptors();
        for loaded in self.loaded_plugins.clone() {
            let installed=self.plugins.as_ref().and_then(|s|s.plugins.iter().find(|p|p.id==loaded.id)).cloned();
            let ready=installed.as_ref().is_some_and(|p|p.enabled&&p.compatible&&p.error.is_empty()&&p.version==loaded.version);
            ui.push_id(("connection-plugin",&loaded.id),|ui| {
                section_card(ui,&loaded.name,|ui| {
                    if ready {
                        ui.weak(uu_i18n::text("已就绪"));
                    }else{
                        ui.weak(uu_i18n::text(if uu_core::Config::from_env().ok().is_some_and(|c|c.profile_dir().is_some()) {
                            "此便携配置尚未同步到本机服务，可手动完成配置。"
                        }else {"正在完成插件配置；串流空闲后会自动应用。"}));
                        if ui.add_enabled(enabled,crate::theme::secondary_button(uu_i18n::text("完成配置"))).clicked() {
                            self.request_action(if self.update_needed(){Confirmation::Update}else{Confirmation::ImportPlugins});
                        }
                    }
                    if let Some(provider)=providers.iter().find(|p|p.id==loaded.id) {
                        let mut selected=self.draft.plugin_services.iter().any(|id|id==provider.id);
                        if ui.add_enabled_ui(enabled,|ui|crate::theme::switch(ui,&mut selected,provider.name)).inner.changed() {
                            self.draft.plugin_services.retain(|id|id!=provider.id);
                            if selected {self.draft.plugin_services.push(provider.id.into());}
                            self.dirty=true;
                        }
                    }
                    ui.collapsing(uu_i18n::text("诊断详情"),|ui| {
                        ui.label(format!("{} · v{}",loaded.id,loaded.version));
                        if let Some(snapshot)=&self.plugins {ui.small(&snapshot.directory);}
                        if let Some(plugin)=&installed {
                            if !plugin.error.is_empty(){ui.label(&plugin.error);}
                            ui.monospace(serde_json::to_string_pretty(&plugin.preferences).unwrap_or_default());
                        }
                    });
                });
            });
        }
    }
    fn show_tabs(&mut self, ui: &mut egui::Ui) {
        self.virtual_display.poll(None);
        ui.add_space(10.);
        ui.add_enabled_ui(self.confirm.is_none(),|ui|ui.horizontal_wrapped(|ui| {
            for (tab, label) in [
                (Section::Management, "服务管理"),
                (Section::Input, "输入"),
                (Section::AudioVideo, "音频 / 视频"),
                (Section::Encoder, "编码器"),
                (Section::Pairing, "配对设置"),
                (Section::Connections, "连接服务"),
                (Section::VirtualDisplays, "虚拟显示器"),
            ] {
                crate::theme::tab_value(ui, &mut self.section, tab, uu_i18n::text(label));
            }
        }));
        ui.add_space(10.);
    }
    fn show_body(&mut self, ui: &mut egui::Ui) {
        ui.add_enabled_ui(self.confirm.is_none(),|ui|self.show_body_contents(ui));
        self.show_confirmation(ui.ctx());
    }
    fn show_body_contents(&mut self, ui: &mut egui::Ui) {
        self.feedback.tick(ui.ctx());
        self.feedback
            .show(ui, self.section.scope());
        let enabled = self.snapshot.is_some() && !self.busy;
        match self.section {
            Section::Management => {
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
                            .on_hover_text(uu_i18n::text("为当前程序补齐 IPv4 和 IPv6 入站规则，仅开放专用/域网络的服务、发现及串流端口。已有阻止规则和公用网络设置保持不变。"))
                            .clicked()
                        {
                            self.submit(Job::Rpc(Request::Firewall { repair: true }));
                        }
                    });
                });
                section_card(ui, uu_i18n::text("会话"), |ui| self.connections(ui));
            }
            Section::Input | Section::AudioVideo | Section::Encoder | Section::Pairing => {
                let title = match self.section {
                    Section::Input => uu_i18n::text("输入"),
                    Section::AudioVideo => uu_i18n::text("音频 / 视频"),
                    Section::Encoder => uu_i18n::text("编码器"),
                    _ => uu_i18n::text("配对设置"),
                };
                section_card(ui, title, |ui| {
                    ui.add_enabled_ui(enabled, |ui| {
                        let before = self.draft.clone();
                        match self.section {
                            Section::Input | Section::AudioVideo | Section::Encoder => {
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
                                        Section::Input => 2,
                                        Section::AudioVideo => 0,
                                        _ => 1,
                                    },
                                    &monitors,
                                    false,
                                    false,
                                    false,
                                );
                            }
                            Section::Pairing => {
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
                        self.configuration_actions(ui);
                    });
                });
            }
            Section::Connections => {
                section_card(ui, uu_i18n::text("IP 直连"), |ui| {
                    ui.add_enabled_ui(enabled, |ui| {
                        let before=self.draft.clone();
                        self.draft.ip_enabled=true;
                        crate::theme::switch(ui,&mut self.draft.ipv6_enabled,uu_i18n::text("启用 IPv6"))
                            .on_hover_text(uu_i18n::text("允许通过 IPv6 地址直连，默认开启。关闭后仅监听 IPv4；保存并重启服务后生效。"));
                        ui.horizontal_wrapped(|ui| {
                            ui.label(uu_i18n::text("服务端口"));
                            crate::theme::number_input(ui,egui::DragValue::new(&mut self.draft.service_port).range(1..=65535));
                            ui.label(uu_i18n::text("发现端口"));
                            crate::theme::number_input(ui,egui::DragValue::new(&mut self.draft.discovery_port).range(1..=65535));
                        });
                        self.dirty |= self.draft!=before;
                    });
                });
                if let Some(request)=self.service_connections.show(ui,enabled) {
                    self.submit(Job::Rpc(Request::Connections{request}));
                }
                self.plugins_ui(ui,enabled);
                ui.add_enabled_ui(enabled,|ui| {
                    self.configuration_actions(ui);
                    if self.dirty || self.snapshot.as_ref().and_then(|s|s.server.as_ref()).is_some_and(|s|s.saved!=s.active) {
                        ui.weak(uu_i18n::text("名称、端口和连接入口变更需要重启服务；重启会断开当前串流。"));
                    }
                });
            }
            Section::VirtualDisplays => {
                section_frame(ui, |ui| {
                    if self.snapshot.is_some() {
                        let service_busy = self.busy;
                        ui.add_enabled_ui(!service_busy, |ui| {
                            self.virtual_display.show_background_inline(ui);
                        });
                    } else {
                        ui.strong(uu_i18n::text("虚拟显示器"));
                        ui.weak(uu_i18n::text("启动服务后可在这里管理虚拟显示器。"));
                    }
                });
            }
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
struct Standalone {
    entry: Entry,
    host: super::sharing_ui::HostUi,
    appearance: super::theme::PreferenceWatch,
    plugins: super::plugin_ui::Manager,
    platform:super::platform::Platform,
}
impl eframe::App for Standalone {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        crate::boss_key::apply();
        self.appearance.poll(ctx);
        self.plugins.poll();
        self.entry.loaded_plugins=self.plugins.loaded_manifests();
        self.entry.plugin_preferences=self.plugins.service_preferences();
        ctx.request_repaint_after(Duration::from_millis(500));
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.entry.show(ui, &mut self.host, true);
            });
        });
        self.platform.show(ctx);
        crate::boss_key::apply();
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
        super::pairing_ui::host_methods(ui,&mut self.method);
        if super::pairing_ui::host_code_settings(ui,self.method) {
        } else if self.method == 0 {
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
                manager.section = Section::VirtualDisplays;
            }
            Ok(Box::new(Standalone {
                platform:super::platform::Platform::new(cc,None),
                entry: Entry {
                    manager: Some(manager),
                    ..Default::default()
                },
                host: Default::default(),
                appearance,
                plugins: super::plugin_ui::Manager::new(uu_core::Config::from_env().ok().as_ref(),&cc.egui_ctx),
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
    let defer_if_busy=args.len()==5&&args[4]=="--defer-if-busy";
    let action = if (args.len() == 4 || defer_if_busy)
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
            if defer_if_busy && !automatic_restart_ready()? {std::process::exit(10);}
            let mut command = std::process::Command::new(exe.with_file_name("eclipse-cli.exe"));
            command
                .args(["service", action, "--source-data"])
                .arg(&args[3]);
            if defer_if_busy && action == "install" { command.arg("--defer-if-busy"); }
            let output = command.creation_flags(0x08000000).output()?;
            if defer_if_busy && output.status.code() == Some(10) { std::process::exit(10); }
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
        assert_eq!(entry.requested_section,Some(Section::VirtualDisplays));
        let (manager,_send,_jobs,_stop)=fixture();
        entry.manager=Some(manager);
        entry.poll(&ctx,&mut Default::default());
        assert_eq!(entry.manager.as_ref().unwrap().section,Section::VirtualDisplays);
        assert!(entry.requested_section.is_none());
        entry.open_pairing();
        assert_eq!(entry.manager.as_ref().unwrap().section,Section::Pairing);
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
            manager.job_section = Some(Section::AudioVideo);
            manager.section = Section::VirtualDisplays;
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
        manager.section = Section::Management;
        manager.submit(Job::Rpc(Request::Firewall { repair: false }));
        assert!(matches!(jobs.try_recv(), Ok(Job::Rpc(Request::Firewall { repair: false }))));
        manager.section = Section::Connections;
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
        manager.section = Section::Management;
        let mut entry = Entry { manager: Some(manager), ..Default::default() };
        let mut host = super::super::sharing_ui::HostUi::default();
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let service = draw_service(&mut entry, &mut host, &ctx);
        assert!(service.contains(uu_i18n::text("Eclipse 服务")));
        assert!(service.contains(uu_i18n::text("后台服务未启动或尚未就绪")));
        entry.manager.as_mut().unwrap().section = Section::Input;
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
        assert!(h.has("自建服务器"));
        assert!(h.has("＋"));
        h.click("配对设置");
        assert!(h.has("设备名称"));
        assert!(h.has("密码配对"));
        assert!(!h.has("添加授权公钥"));
        h.manager.pairing.password = zeroize::Zeroizing::new("generated-password".into());
        h.click("公钥配对");
        assert!(h.has("添加授权公钥"));
        assert!(!h.has("保存安全连接密码"));
        assert_eq!(&*h.manager.pairing.password, "generated-password");
        h.click("PIN 码配对");
        assert!(h.has("PIN 由控制端在连接时生成，无需在这里预先设置。请保持服务运行，收到的配对请求会显示在本页。"));
        assert!(!h.has("保存安全连接密码"));
        h.click("临时 PIN 码");
        assert!(h.has("选择临时配对会撤销该控制端此前的长期授权。"));
        assert_eq!(&*h.manager.pairing.password, "generated-password");
        h.click("音频 / 视频");
        assert!(!h.has("设备名称"));
        h.click("输入");
        assert!(!h.has("密码配对"));
        assert!(!h.has("自建服务器"));
        h.click("虚拟显示器");
        assert!(!h.has("虚拟显示器上限"));
        assert!(h.has("＋"));
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
        h.click("＋");
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
    fn restart_with_no_streams_applies_from_the_current_settings_tab() {
        for section in [Section::Input,Section::AudioVideo,Section::Encoder,Section::Connections] {
            let mut h=Harness::new();h.manager.section=section;
            h.click("保存并重启服务");
            assert_eq!(h.manager.section,section);
            assert!(h.manager.confirm.is_none());
            assert!(!h.has("确认继续"));
            assert!(matches!(h.jobs.try_recv(),Ok(Job::Rpc(Request::Admin{request:host_admin::Request::Save{restart:true,..}}))));
        }
    }
    fn active_stream(manager:&mut Manager) {
        manager.snapshot.as_mut().unwrap().connections.push(host_admin::Connection {
            id:"generated".into(),peer:"generated-peer".into(),name:"Generated stream".into(),
            status:"streaming".into(),permissions:Permissions::CONTROL,
        });
    }
    #[test]
    fn active_restart_confirms_on_current_tab_and_cancel_keeps_the_draft() {
        let mut h=Harness::new();h.manager.section=Section::Encoder;active_stream(&mut h.manager);
        h.manager.draft.audio=false;h.manager.dirty=true;
        h.click("保存并重启服务");
        assert!(h.manager.confirm.is_some());assert_eq!(h.manager.section,Section::Encoder);
        assert!(h.has("此操作会断开当前 1 个串流会话。"));
        assert!(matches!(h.jobs.try_recv(),Err(mpsc::TryRecvError::Empty)));
        h.click("取消");
        assert!(h.manager.confirm.is_none());assert!(!h.manager.draft.audio);assert!(h.manager.dirty);
        assert!(matches!(h.jobs.try_recv(),Err(mpsc::TryRecvError::Empty)));
    }
    #[test]
    fn unknown_stream_count_confirms_and_zero_count_never_shows_a_zero_warning() {
        let mut h=Harness::new();h.manager.section=Section::Connections;h.manager.snapshot=None;
        h.manager.request_action(Confirmation::Update);h.frame(vec![]);h.frame(vec![]);
        assert!(h.has("无法确认当前连接数；此操作可能会断开正在进行的串流。"));
        h.send.send(state(true,Sharing::default())).unwrap();h.frame(vec![]);
        assert!(h.manager.confirm.is_none());assert!(matches!(h.manager.deployment,Some(Deployment::Install)));
    }
    fn generated_plugin()->uu_plugin_sdk::Manifest {
        uu_plugin_sdk::Manifest{api_version:1,sdk_id:uu_plugin_sdk::SDK_ID.into(),id:"fixture".into(),name:"Generated extension".into(),version:"1".into(),library:"fixture.dll".into(),library_sha256:"a".repeat(64),data_files:vec![]}
    }
    #[test]
    fn only_loaded_plugins_appear_and_technical_errors_stay_in_diagnostics() {
        let mut h=Harness::new();h.manager.section=Section::Connections;
        h.manager.apply_plugin_snapshot(PluginSnapshot{directory:"generated/private/plugin/path".into(),plugins:vec![uu_core::service_admin::PluginInfo {
            id:"fixture".into(),name:"Generated extension".into(),version:"1".into(),enabled:false,compatible:false,error:"generated SDK mismatch detail".into(),preferences:Default::default(),
        }]});
        h.frame(vec![]);h.frame(vec![]);
        assert!(!h.has("后台插件"));assert!(!h.has("Generated extension"));
        assert!(!h.has("generated/private/plugin/path"));assert!(!h.has("generated SDK mismatch detail"));
        h.manager.loaded_plugins=vec![generated_plugin()];h.frame(vec![]);h.frame(vec![]);
        assert!(h.has("Generated extension"));assert!(h.has("完成配置"));
        assert!(!h.has("generated/private/plugin/path"));assert!(!h.has("generated SDK mismatch detail"));
        h.click("诊断详情");assert!(h.has("generated SDK mismatch detail"));
        h.manager.loaded_plugins.clear();h.frame(vec![]);assert!(!h.has("Generated extension"));
    }
    #[test]
    fn automatic_update_waits_for_idle_and_never_repeats_an_authorization_attempt() {
        let (mut manager,send,_jobs,_stop)=fixture();send.send(state(true,Sharing::default())).unwrap();manager.poll();
        manager.build=Some(BuildStatus{state:SERVICE_RUNNING,process_id:42,installed:Some(BuildVersion{revision:"a".repeat(40)}),running:None,running_error:None,profile:None});
        active_stream(&mut manager);
        let mut entry=Entry{manager:Some(manager),automatic_sync_enabled:true,..Default::default()};
        entry.schedule_automatic_sync();assert!(entry.deployment.is_none());
        entry.manager.as_mut().unwrap().snapshot.as_mut().unwrap().connections.clear();
        entry.schedule_automatic_sync();assert!(matches!(entry.deployment.take(),Some(Deployment::Install)));
        entry.schedule_automatic_sync();assert!(entry.deployment.is_none());
    }
    #[test]
    fn another_configuration_is_read_only_and_never_automatically_redeployed() {
        let (mut manager,send,jobs,_stop)=fixture();send.send(state(true,Sharing::default())).unwrap();manager.poll();
        manager.expected_profile=Some("a".repeat(64));manager.allow_legacy_profile=false;
        manager.build=Some(BuildStatus{state:SERVICE_RUNNING,process_id:42,installed:Some(BuildVersion{revision:"b".repeat(40)}),running:None,running_error:None,profile:Some("c".repeat(64))});
        let mut entry=Entry{manager:Some(manager),automatic_sync_enabled:true,..Default::default()};
        assert!(!entry.online());assert_eq!(entry.keep_displays(),None);
        entry.schedule_automatic_sync();assert!(entry.deployment.is_none());assert!(jobs.try_recv().is_err());
        entry.manager.as_mut().unwrap().build.as_mut().unwrap().profile=None;
        entry.schedule_automatic_sync();assert!(entry.deployment.is_none(),"a historical service must be activated explicitly");
        entry.manager.as_mut().unwrap().build.as_mut().unwrap().profile=Some("a".repeat(64));
        assert!(entry.online());assert_eq!(entry.keep_displays(),Some(true));
    }
    #[test]
    fn automatic_update_preserves_temporary_displays_and_unfinished_preparation() {
        use uu_core::virtual_display::{Display,LeaseId,Owner,Spec,Status,preparation};
        let mut status=Status{supported:true,driver_present:true,available_slots:1,leased:0,allocated:0,keep_after_stream:false,limit:2,supports_lifetimes:true,supports_configuration:true,supports_catalog:true,on_demand:false,explanation:String::new(),install_url:String::new(),preparation:None};
        let display=Display{id:LeaseId(uuid::Uuid::new_v4()),owner:Owner::Manual,spec:Spec::default(),actual:None,name:"Generated".into(),source:"generated-source".into(),active:true,persistent:false,enabled:true,restore_layout:true,cleanup_pending:false,last_error:None};
        let response=|status:Status,displays|Response::VirtualDisplay{response:uu_core::native_virtual_display::Response::Snapshot{status,displays,slots:vec![]}};
        assert!(!automatic_restart_safe(&Response::Applied));
        assert!(automatic_restart_safe(&response(status.clone(),vec![])));
        assert!(!automatic_restart_safe(&response(status.clone(),vec![display.clone()])));
        assert!(automatic_restart_safe(&response(status.clone(),vec![Display{persistent:true,..display.clone()}])));
        assert!(automatic_restart_safe(&response(status.clone(),vec![Display{enabled:false,active:false,..display.clone()}])),"disabled temporary definitions own no hardware and must not block an idle update");
        assert!(!automatic_restart_safe(&response(status.clone(),vec![Display{persistent:true,cleanup_pending:true,..display}])));
        status.preparation=Some(preparation::Status{phase:preparation::Phase::RecoveryPending,configured_capacity:None,targets:0,available:0,requested_capacity:2,queued:false,error:Some("generated pending recovery".into())});
        assert!(!automatic_restart_safe(&response(status,vec![])));
    }
    #[test]
    fn plugin_policy_changes_use_the_existing_owner_rpc_without_another_installation() {
        for enabled in [true,false] {
            let (mut manager,send,jobs,_stop)=fixture();send.send(state(true,Sharing::default())).unwrap();manager.poll();
            let before=uu_core::plugins::PluginPreferences{enabled:true,..Default::default()};
            let after=uu_core::plugins::PluginPreferences{enabled,options:serde_json::from_value(serde_json::json!({"generated_option":true})).unwrap()};
            manager.loaded_plugins=if enabled{vec![generated_plugin()]}else{vec![]};
            manager.apply_plugin_snapshot(PluginSnapshot{directory:"generated".into(),plugins:vec![uu_core::service_admin::PluginInfo{id:"fixture".into(),name:"Generated extension".into(),version:"1".into(),enabled:true,compatible:true,error:String::new(),preferences:before.clone()}]});
            let mut preferences=uu_core::plugins::Preferences::default();preferences.plugins.insert("fixture".into(),after.clone());
            let mut entry=Entry{manager:Some(manager),plugin_preferences:Some(preferences),automatic_sync_enabled:true,..Default::default()};
            entry.schedule_automatic_sync();assert!(entry.deployment.is_none());
            assert!(matches!(jobs.try_recv(),Ok(Job::Rpc(Request::Plugins{request:PluginRequest::Save{id,expected,value}})) if id=="fixture"&&expected==before&&value==after));
        }
    }

    #[test]
    fn independent_profiles_never_schedule_service_updates_or_plugin_changes() {
        let (mut manager, send, jobs, _stop) = fixture();
        send.send(state(true, Sharing::default())).unwrap();
        manager.poll();
        manager.build = Some(BuildStatus { state:SERVICE_RUNNING, process_id:42,
            installed:Some(BuildVersion { revision:"a".repeat(40) }), running:None, running_error:None, profile:None });
        let mut entry = Entry { manager:Some(manager), ..Default::default() };
        entry.set_loaded_plugins(vec![generated_plugin()], Some(Default::default()), false);
        entry.schedule_automatic_sync();
        assert!(entry.deployment.is_none());
        assert!(!matches!(jobs.try_recv(), Ok(Job::Rpc(Request::Plugins { .. }))));
        entry.set_loaded_plugins(vec![], None, true);
        entry.schedule_automatic_sync();
        assert!(matches!(entry.deployment, Some(Deployment::Install)));
    }

}
