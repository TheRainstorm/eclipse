//! Dedicated local administrator manager. Its lifetime is independent of the
//! installed service, and its editors share the normal host validation rules.
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
    device_book::{Sharing, Store},
    device_trust::{Permissions, TrustStore},
    host_admin,
    service_admin::{windows::*, Request, Response},
};
use windows_sys::Win32::System::Services::{SERVICE_RUNNING, SERVICE_STOPPED};

#[derive(Default)]
pub(super) struct Entry {
    status: Option<u32>,
    build:Option<BuildStatus>,
    next: Option<Instant>,
    pending_install: bool,
    install_started: bool,
    job: Option<mpsc::Receiver<Result<(), String>>>,
    notice: String,
}
impl Entry {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        host: &mut super::sharing_ui::HostUi,
        available: bool,
    ) -> bool {
        let mut installed = false;
        if self.next.is_none_or(|next| Instant::now() >= next) {
            self.build=build_status().ok();
            self.status = self.build.as_ref().map(|build|build.state).or_else(||status().ok());
            self.next = Some(Instant::now() + Duration::from_secs(1));
        }
        if let Some(job) = &self.job {
            if let Ok(result) = job.try_recv() {
                self.job = None;
                self.next = None;
                match result {
                    Ok(()) => {
                        self.notice = uu_i18n::text("后台服务管理操作已完成").into();
                        installed = self.install_started;
                    }
                    Err(error) => self.notice = error,
                }
                self.install_started = false;
            }
        }
        if self.pending_install && !host.running() {
            self.pending_install = false;
            self.install_started = true;
            let result = (|| -> anyhow::Result<_> {
                let config = uu_core::Config::from_env()?;
                let exe = std::env::current_exe()?;
                Ok((
                    exe,
                    format!(
                        "--background-service-action install --source-data \"{}\"",
                        config.data_dir.display()
                    ),
                ))
            })();
            match result {
                Ok((exe, args)) => self.launch(exe, args),
                Err(error) => self.notice = error.to_string(),
            }
        }
        egui::Frame::group(ui.style()).show(ui,|ui| {
            ui.strong(uu_i18n::text("Windows 后台服务 · 锁屏串流"));
            ui.label(match self.status {Some(SERVICE_RUNNING)=>uu_i18n::text("后台服务运行中"),Some(SERVICE_STOPPED)=>uu_i18n::text("后台服务已停止"),Some(_)=>uu_i18n::text("后台服务正在转换状态"),None=>uu_i18n::text("后台服务尚未安装")});
            if let Some(build)=&self.build {version_labels(ui,build);}
            ui.weak(uu_i18n::text("后台服务随 Windows 启动，支持已登录控制台用户的锁屏串流。首次安装复制共享设置与配对授权；插件及其账号需单独同步。"));
            let busy=self.job.is_some()||self.pending_install;
            if self.status.is_some() {
                if ui.add_enabled(!busy,egui::Button::new(uu_i18n::text("管理后台服务"))).clicked() {
                    match std::env::current_exe() {
                        Ok(exe)=>self.launch(exe,"--open-background-service-manager".into()),Err(error)=>self.notice=error.to_string(),
                    }
                }
                ui.weak(uu_i18n::text("打开管理员管理窗口，查看运行版本、设置及虚拟屏；打开或关闭窗口不更新程序，也不重启服务。"));
                ui.add_space(6.);
                if ui.add_enabled(!busy,egui::Button::new(uu_i18n::text("更新后台程序"))).clicked() {
                    if host.running(){host.stop();}self.pending_install=true;
                }
                ui.weak(uu_i18n::text("安装当前完整程序包并启动后台，保留已有设置与配对，不导入插件或账号；会断开后台串流并停止本窗口的前台共享。"));
                ui.add_space(6.);
                if ui.add_enabled(!busy,egui::Button::new(uu_i18n::text("同步插件并重启后台服务"))).clicked() {
                    let result=(||->anyhow::Result<_>{
                        let config=uu_core::Config::from_env()?;
                        let exe=std::env::current_exe()?;
                        Ok((exe,format!("--background-service-action import-plugins --source-data \"{}\"",config.data_dir.display())))
                    })();
                    match result {Ok((exe,args))=>self.launch(exe,args),Err(error)=>self.notice=error.to_string()}
                }
                ui.weak(uu_i18n::text("将当前已启用插件及其声明的配置、账号复制到后台，覆盖对应状态；后台原来运行时重启，原来停止时保持停止。"));
                ui.weak(uu_i18n::text("原生虚拟显示器不依赖插件，无需为管理虚拟屏同步插件。"));
            }else if ui.add_enabled(available&&!busy,egui::Button::new(uu_i18n::text("安装并启动后台服务"))).clicked() {
                if host.running(){host.stop();}self.pending_install=true;self.notice=uu_i18n::text("正在准备后台服务安装…").into();
            }
            if busy {ui.spinner();ui.ctx().request_repaint_after(Duration::from_millis(100));}
            if !self.notice.is_empty(){ui.label(&self.notice);}
        });
        ui.add_space(10.);
        installed
    }
    fn launch(&mut self, exe: std::path::PathBuf, args: String) {
        let (send, receive) = mpsc::channel();
        self.job = Some(receive);
        std::thread::spawn(move || {
            let result=(||->anyhow::Result<()> {
                if args.starts_with("--background-service-action ") {
                    uu_core::service_admin::package::verify(exe.parent().ok_or_else(||anyhow::anyhow!("{}", uu_i18n::format!("无法确定安装包目录")))?,env!("UU_BUILD_COMMIT"))?;
                }
                elevated(&exe,&args)
            })();
            let _ = send.send(result.map_err(|e| format!("{e:#}")));
        });
    }
}
fn version_labels(ui:&mut egui::Ui,build:&BuildStatus) {
    if let Some(running)=&build.running {
        ui.label(uu_i18n::format!("后台运行版本：{} · PID {}",running.label(),build.process_id))
            .on_hover_text(&running.revision);
    } else if build.process_id!=0 {
        ui.label(uu_i18n::format!("后台运行版本：无法读取 · PID {}",build.process_id))
            .on_hover_text(build.running_error.as_deref().unwrap_or("Unknown service image"));
        ui.weak(uu_i18n::text("当前权限可能无法读取系统后台进程版本。请打开管理员管理窗口核对；安装版本不能代替运行版本。"));
    }
    if let Some(installed)=&build.installed {
        if build.running.as_ref().is_none_or(|running|running.revision!=installed.revision) {
            ui.label(uu_i18n::format!("后台安装版本：{}",installed.label())).on_hover_text(&installed.revision);
        }
    }
    ui.weak(uu_i18n::format!("当前程序版本：{} · {}",env!("CARGO_PKG_VERSION"),crate::build_info::label()));
}

enum Job {
    Refresh,
    Firewall(bool),
    Rpc(Request),
    Control(&'static str),
    SaveStopped {
        expected: Sharing,
        value: Sharing,
        start: bool,
    },
}
enum Event {
    Firewall(Result<String,String>),
    State(u32, Result<Response, String>),
    Done(bool, Result<(), String>),
}
struct Manager {
    firewall: String,
    virtual_display:super::virtual_display_ui::State,
    build:Option<BuildStatus>,
    trust: TrustStore,
    sender: mpsc::Sender<Job>,
    events: mpsc::Receiver<Event>,
    stop: Arc<AtomicBool>,
    state: u32,
    snapshot: Option<host_admin::Snapshot>,
    endpoints: Vec<(String, String)>,
    pending: Vec<uu_core::service_admin::Pairing>,
    draft: Sharing,
    baseline: Sharing,
    revision: String,
    dirty: bool,
    busy: bool,
    section: u8,
    notice: String,
    codes: HashMap<String, zeroize::Zeroizing<String>>,
    permissions: HashMap<String, Permissions>,
    pairing: super::pairing_ui::HostUi,
}
impl Drop for Manager {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}
fn control(exe: &std::path::Path, command: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let output = std::process::Command::new(exe)
        .args(["service", command])
        .creation_flags(0x08000000)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(uu_i18n::format!(
            "后台服务操作失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}
impl Manager {
    fn new() -> anyhow::Result<Self> {
        administrator_file_owner()?;
        let paths = Paths::discover()?;
        let installed = paths.manifest()?;
        let executable = paths.binary_directory(&installed).join(installed.cli_name());
        let config = paths.config();
        let store = Store::open(config.at("device-trust"))?;
        let draft = store.read()?.sharing;
        let trust = TrustStore::open(config.at("device-trust"))?;
        let (sender, jobs) = mpsc::channel();
        let (send, events) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker_config = config.clone();
        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = send.send(Event::Done(false, Err(error.to_string())));
                    return;
                }
            };
            while !stopped.load(Ordering::Acquire) {
                match jobs.recv_timeout(Duration::from_millis(650)) {
                    Ok(job) => {
                        let saved = matches!(
                            &job,
                            Job::SaveStopped { .. }
                                | Job::Rpc(Request::Admin {
                                    request: host_admin::Request::Save { .. }
                                })
                        );
                        let result = (|| -> anyhow::Result<()> {
                            match job {
                                Job::Refresh => {}
                                Job::Firewall(repair) => {
                                    let result=(||->anyhow::Result<String>{
                                        let paths=Paths::discover()?;let installed=paths.manifest()?;
                                        let exe=firewall_program(&paths,&installed)?;
                                        let sharing=Store::open(worker_config.at("device-trust"))?.read()?.sharing;
                                        let report=if repair {uu_core::firewall::ensure(&exe,sharing.service_port,sharing.discovery_port)?} else {uu_core::firewall::inspect(&exe,sharing.service_port,sharing.discovery_port)?};
                                        Ok(report.message())
                                    })().map_err(|e|format!("{e:#}"));
                                    let _=send.send(Event::Firewall(result));
                                }
                                Job::Rpc(request) => {
                                    match runtime.block_on(request_remote(request))? {
                                        Response::Error { message } => anyhow::bail!(message),
                                        _ => {}
                                    }
                                }
                                Job::Control(command) => {
                                    control(&executable, command).map_err(anyhow::Error::msg)?
                                }
                                Job::SaveStopped {
                                    expected,
                                    value,
                                    start,
                                } => {
                                    anyhow::ensure!(
                                        status()? == SERVICE_STOPPED,
                                        "{}", uu_i18n::format!("后台服务已启动，请重新读取后保存"));
                                    value.validate()?;
                                    let store = Store::open(worker_config.at("device-trust"))?;
                                    store.edit(|book| {
                                        anyhow::ensure!(
                                            book.sharing == expected,
                                            "{}", uu_i18n::format!("配置已在其它窗口修改，请重新读取"));
                                        anyhow::ensure!(
                                            value.ip_enabled
                                                || !value.plugin_services.is_empty()
                                                || book.services.iter().any(|s| s.host_enabled),
                                            "{}", uu_i18n::format!("至少保留一个连接入口"));
                                        book.sharing = value;
                                        Ok(())
                                    })?;
                                    if start {
                                        control(&executable, "start")
                                            .map_err(anyhow::Error::msg)?;
                                    }
                                }
                            }
                            Ok(())
                        })()
                        .map_err(|e| e.to_string());
                        let _ = send.send(Event::Done(saved, result));
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                let state = status().unwrap_or(0);
                let response = if state == SERVICE_RUNNING {
                    runtime
                        .block_on(request_remote(Request::Status))
                        .map_err(|e| e.to_string())
                } else {
                    (|| -> anyhow::Result<Response> {
                        let saved = Store::open(worker_config.at("device-trust"))?
                            .read()?
                            .sharing;
                        let mut details = Permissions::CONTROL.details();
                        details.manage_server = true;
                        Ok(Response::Status {
                            endpoints: vec![],
                            pending: vec![],
                            snapshot: host_admin::Snapshot {
                                own_permissions: Permissions::CONTROL.with_details(details),
                                connections: vec![],
                                server: Some(host_admin::ServerSettings {
                                    active: saved.clone(),
                                    saved,
                                    revision: String::new(),
                                    monitors: vec![],
                                    portal: false,
                                }),
                            },
                        })
                    })()
                    .map_err(|e| e.to_string())
                };
                if send.send(Event::State(state, response)).is_err() {
                    break;
                }
            }
        });
        let _=sender.send(Job::Firewall(false));
        Ok(Self {
            firewall:String::new(),
            virtual_display:Default::default(),
            build:build_status().ok(),
            trust,
            sender,
            events,
            stop,
            state: 0,
            snapshot: None,
            endpoints: vec![],
            pending: vec![],
            baseline: draft.clone(),
            draft,
            revision: String::new(),
            dirty: false,
            busy: false,
            section: 0,
            notice: String::new(),
            codes: HashMap::new(),
            permissions: HashMap::new(),
            pairing: Default::default(),
        })
    }
    fn submit(&mut self, job: Job) {
        self.busy = true;
        if self.sender.send(job).is_err() {
            self.busy = false;
            self.notice = uu_i18n::text("后台管理工作线程已结束").into();
        }
    }
    fn poll(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Done(saved, result) => {
                    self.busy = false;
                    match result {
                        Ok(()) => {
                            if saved {
                                self.dirty = false;
                            }
                            self.notice = uu_i18n::text("操作已完成").into();
                        }
                        Err(error) => self.notice = error,
                    }
                }
                Event::Firewall(result)=> {self.firewall=match result {Ok(message)=>message,Err(error)=>error};}
                Event::State(state, result) => {
                    self.build=build_status().ok();
                    self.state = state;
                    match result {
                        Ok(Response::Status {
                            endpoints,
                            pending,
                            snapshot,
                        }) => {
                            self.codes
                                .retain(|id, _| pending.iter().any(|p| p.id == *id));
                            self.permissions
                                .retain(|id, _| pending.iter().any(|p| p.id == *id));
                            self.endpoints = endpoints;
                            self.pending = pending;
                            if !self.dirty {
                                if let Some(server) = &snapshot.server {
                                    self.draft = server.saved.clone();
                                    self.baseline = server.saved.clone();
                                    self.revision = server.revision.clone();
                                }
                            }
                            self.snapshot = Some(snapshot);
                        }
                        Ok(Response::Error { message }) | Err(message) => self.notice = message,
                        _ => {}
                    }
                }
            }
        }
    }
    fn save(&mut self, restart: bool) {
        if self.state == SERVICE_STOPPED {
            self.submit(Job::SaveStopped {
                expected: self.baseline.clone(),
                value: self.draft.clone(),
                start: restart,
            });
        } else {
            self.submit(Job::Rpc(Request::Admin {
                request: host_admin::Request::Save {
                    revision: self.revision.clone(),
                    settings: self.draft.clone(),
                    restart,
                },
            }));
        }
    }
}
async fn request_remote(request: Request) -> anyhow::Result<Response> {
    uu_core::service_admin::windows::request(request).await
}

impl eframe::App for Manager {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        self.virtual_display.poll(None);
        self.virtual_display.show(ctx,None);
        ctx.request_repaint_after(Duration::from_millis(200));
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading(uu_i18n::text("Windows 后台服务"));
            ui.label(match self.state {
                SERVICE_RUNNING => uu_i18n::text("运行中 · 后台服务独立运行"),
                SERVICE_STOPPED => uu_i18n::text("已停止"),
                _ => uu_i18n::text("正在读取服务状态…"),
            });
            if let Some(build)=&self.build {version_labels(ui,build);}
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.busy,egui::Button::new(uu_i18n::text("检查防火墙"))).clicked(){self.submit(Job::Firewall(false));}
                if ui.add_enabled(!self.busy,egui::Button::new(uu_i18n::text("修复防火墙"))).clicked(){self.submit(Job::Firewall(true));}
            });
            if !self.firewall.is_empty(){ui.label(&self.firewall);}
            ui.horizontal(|ui| {
                if ui.add_enabled(self.state==SERVICE_RUNNING,egui::Button::new(uu_i18n::text("虚拟显示器"))).clicked(){self.virtual_display.open_local(ctx,super::virtual_display_ui::LocalOwner::Background);}
                if ui
                    .add_enabled(
                        !self.busy && self.state == SERVICE_STOPPED,
                        egui::Button::new(uu_i18n::text("启动服务")),
                    )
                    .clicked()
                {
                    self.submit(Job::Control("start"));
                }
                if ui
                    .add_enabled(
                        !self.busy && self.state == SERVICE_RUNNING,
                        egui::Button::new(uu_i18n::text("停止服务")),
                    )
                    .clicked()
                {
                    self.submit(Job::Control("stop"));
                }
                if ui.button(uu_i18n::text("重新读取配置")).clicked() {
                    self.dirty = false;
                    self.submit(Job::Refresh);
                }
                if self.busy {
                    ui.spinner();
                }
            });
            for (name, state) in &self.endpoints {
                ui.label(format!("{name} · {state}"));
            }
            if !self.notice.is_empty() {
                ui.label(&self.notice);
            }
            ui.separator();
            for request in self.pending.clone() {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.strong(uu_i18n::format!("配对请求 · {}", request.device.name));
                    ui.monospace(request.device.id());
                    let code = self.codes.entry(request.id.clone()).or_default();
                    ui.add(
                        egui::TextEdit::singleline(&mut **code)
                            .hint_text(uu_i18n::text("控制端显示的 8 位配对码"))
                            .char_limit(8),
                    );
                    let permissions = self
                        .permissions
                        .entry(request.id.clone())
                        .or_insert(Permissions::CONTROL);
                    super::sharing_ui::permissions(ui, permissions);
                    let permissions = *permissions;
                    let code = code.to_string();
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !self.busy && request.waiting && code.len() == 8,
                                egui::Button::new(uu_i18n::text("批准配对")),
                            )
                            .clicked()
                        {
                            self.submit(Job::Rpc(Request::Approve {
                                id: request.id.clone(),
                                code,
                                permissions,
                            }));
                        }
                        if ui
                            .add_enabled(!self.busy, egui::Button::new(uu_i18n::text("拒绝")))
                            .clicked()
                        {
                            self.submit(Job::Rpc(Request::Reject {
                                id: request.id.clone(),
                            }));
                        }
                    });
                });
            }
            ui.horizontal_wrapped(|ui| {
                for (section, name) in [
                    (0, uu_i18n::text("画面")),
                    (1, uu_i18n::text("编码")),
                    (2, uu_i18n::text("权限")),
                    (3, uu_i18n::text("配对设置")),
                    (4, uu_i18n::text("连接")),
                ] {
                    ui.selectable_value(&mut self.section, section, name);
                }
            });
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_enabled_ui(!self.busy, |ui| {
                    let before = self.draft.clone();
                    match self.section {
                        0..=2 => {
                            let monitors = self
                                .snapshot
                                .as_ref()
                                .and_then(|s| s.server.as_ref())
                                .map(|s| s.monitors.clone())
                                .unwrap_or_default();
                            super::server_config_ui::edit(
                                ui,
                                &mut self.draft,
                                self.section,
                                &monitors,
                                false,
                                false,
                            );
                        }
                        3 => {
                            super::server_config_ui::device_name(ui, &mut self.draft, true);
                            self.pairing.show(ui, &self.trust, &self.draft.name, None);
                            ui.separator();
                            ui.strong(uu_i18n::text("已授权设备"));
                            match self.trust.peers() {
                                Ok(peers) => {
                                    for peer in peers
                                        .into_iter()
                                        .filter(|p| p.inbound.is_some() && !p.revoked)
                                    {
                                        egui::Frame::group(ui.style()).show(ui, |ui| {
                                            ui.label(&peer.device.name);
                                            ui.monospace(peer.device.id());
                                            let mut permissions = peer.inbound.unwrap();
                                            if super::sharing_ui::permissions(ui, &mut permissions)
                                            {
                                                self.notice = match self.trust.authorize(
                                                    peer.device.clone(),
                                                    permissions,
                                                    None,
                                                ) {
                                                    Ok(()) => uu_i18n::text("设备授权已更新").into(),
                                                    Err(error) => error.to_string(),
                                                };
                                            }
                                            if ui.small_button(uu_i18n::text("撤销授权")).clicked() {
                                                self.notice =
                                                    match self.trust.revoke(&peer.device.id()) {
                                                        Ok(()) => uu_i18n::text("设备授权已撤销").into(),
                                                        Err(error) => error.to_string(),
                                                    };
                                            }
                                        });
                                    }
                                }
                                Err(error) => {
                                    ui.label(error.to_string());
                                }
                            }
                        }
                        _ => {
                            ui.checkbox(&mut self.draft.ip_enabled, uu_i18n::text("IP 直连"));
                            for provider in uu_core::plugins::registry().descriptors() {
                                let mut enabled=self.draft.plugin_services.iter().any(|id|id==provider.id);
                                if ui.checkbox(&mut enabled,provider.name).changed(){self.draft.plugin_services.retain(|id|id!=provider.id);if enabled {self.draft.plugin_services.push(provider.id.into());}}
                            }
                            ui.horizontal(|ui| {
                                ui.label(uu_i18n::text("服务端口"));
                                ui.add(
                                    egui::DragValue::new(&mut self.draft.service_port)
                                        .range(1..=65535),
                                );
                                ui.label(uu_i18n::text("发现端口"));
                                ui.add(
                                    egui::DragValue::new(&mut self.draft.discovery_port)
                                        .range(1..=65535),
                                );
                            });
                        }
                    }
                    self.dirty |= self.draft != before;
                    ui.add_space(14.);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!self.busy, egui::Button::new(uu_i18n::text("保存，下次启动生效")))
                            .clicked()
                        {
                            self.save(false);
                        }
                        if ui
                            .add_enabled(!self.busy, egui::Button::new(uu_i18n::text("保存并重启服务")))
                            .clicked()
                        {
                            self.save(true);
                        }
                    });
                    if let Some(snapshot) = self.snapshot.clone() {
                        ui.separator();
                        ui.strong(uu_i18n::text("当前连接"));
                        for connection in snapshot.connections.clone() {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(format!("{} · {}", connection.name, connection.status));
                                if ui.small_button(uu_i18n::text("断开")).clicked() {
                                    self.submit(Job::Rpc(Request::Admin {
                                        request: host_admin::Request::Disconnect {
                                            session: connection.id,
                                        },
                                    }));
                                }
                            });
                        }
                    }
                    ui.weak(uu_i18n::text("服务运行时接受新设备配对；已有授权、密码和公钥规则继续生效。"));
                });
            });
        });
    }
}
pub(super) fn run() -> eframe::Result {
    run_manager(false)
}
fn run_manager(virtual_displays:bool)->eframe::Result {
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
            if virtual_displays {manager.virtual_display.open_local(&cc.egui_ctx,super::virtual_display_ui::LocalOwner::Background);}
            Ok(Box::new(manager))
        }),
    )
}

/// Called on the sharing startup worker, never on the rendering thread.
pub(super) fn prepare_foreground_firewall(service:u16,discovery:u16)->anyhow::Result<Option<String>> {
    let exe=std::env::current_exe()?;
    let before=uu_core::firewall::inspect(&exe,service,discovery)?;
    if before.rules.as_array().is_some_and(|r|!r.is_empty()) {return Ok(Some(before.message()));}
    if require_administrator().is_ok() {return Ok(Some(uu_core::firewall::ensure(&exe,service,discovery)?.message()));}
    elevated(&exe,&format!("--configure-streaming-firewall {service} {discovery}"))?;
    let after=uu_core::firewall::inspect(&exe,service,discovery)?;
    anyhow::ensure!(after.rules.as_array().is_some_and(|r|!r.is_empty()),"Firewall configuration was not visible after administrator repair");
    Ok(Some(format!("{}\n{}",uu_i18n::text("已由管理员配置防火墙"),after.message())))
}

/// Each UAC action is launched through this GUI version. Management does not
/// depend on an adjacent CLI; install/import still use a verified complete set.
pub(super) fn dispatch() -> Option<eframe::Result> {
    let args:Vec<_>=std::env::args_os().skip(1).collect();
    if args.len()==3 && args[0]=="--configure-streaming-firewall" {
        let result=(||->anyhow::Result<()> {
            require_administrator()?;
            let exe=std::env::current_exe()?;
            uu_core::service_admin::package::verify(exe.parent().ok_or_else(||anyhow::anyhow!("cannot determine package directory"))?,env!("UU_BUILD_COMMIT"))?;
            let service=args[1].to_str().ok_or_else(||anyhow::anyhow!("invalid service port"))?.parse::<u16>()?;
            let discovery=args[2].to_str().ok_or_else(||anyhow::anyhow!("invalid discovery port"))?.parse::<u16>()?;
            uu_core::firewall::ensure(&exe,service,discovery)?;Ok(())
        })();
        if let Err(error)=result {show_error(&format!("Firewall: {error:#}"));std::process::exit(1);}
        return Some(Ok(()));
    }
    if args==["--prepare-virtual-display-driver"] {
        let result=(||->anyhow::Result<()> {
            require_administrator()?;
            let exe=std::env::current_exe()?;
            uu_core::service_admin::package::verify(exe.parent().ok_or_else(||anyhow::anyhow!("{}",uu_i18n::format!("无法确定安装包目录")))?,env!("UU_BUILD_COMMIT"))?;
            uu_core::virtual_display::prepare()?;
            Ok(())
        })();
        if let Err(error)=result {
            let error=format!("{error:#}");
            show_error(&uu_i18n::format!("准备虚拟显示器失败：{error}"));
            std::process::exit(1);
        }
        return Some(Ok(()));
    }
    if args.len()==2&&args[0]=="--install-virtual-display-driver" {
        let result=super::virtual_display_install::install_downloaded(std::path::Path::new(&args[1]));
        if let Err(error)=result {show_error(&uu_i18n::format!("安装虚拟显示器驱动失败：{error}"));std::process::exit(1);}
        return Some(Ok(()));
    }
    if args==["--manage-background-service"] || args==["--manage-virtual-displays"] {
        let result=run_manager(args==["--manage-virtual-displays"]);
        if let Err(error)=&result {show_error(&uu_i18n::format!("无法打开后台服务管理：{error}"));}
        return Some(result);
    }
    let virtual_displays=args==["--open-virtual-display-manager"];
    let opening=args==["--open-background-service-manager"]||virtual_displays;
    let action=if args.len()==4 && args[0]=="--background-service-action" && args[2]=="--source-data" {
        args[1].to_str().filter(|s|matches!(*s,"install"|"import-plugins"))
    }else{None};
    if !opening && action.is_none(){return None;}
    let result=(||->anyhow::Result<()> {
        require_administrator()?;
        let exe=std::env::current_exe()?;
        if opening {
            std::process::Command::new(exe).arg(if virtual_displays{"--manage-virtual-displays"}else{"--manage-background-service"}).spawn()?;
        }else if let Some(action)=action {
            use std::os::windows::process::CommandExt;
            uu_core::service_admin::package::verify(exe.parent().ok_or_else(||anyhow::anyhow!("{}", uu_i18n::format!("无法确定安装包目录")))?,env!("UU_BUILD_COMMIT"))?;
            let mut command=std::process::Command::new(exe.with_file_name("eclipse-cli.exe"));
            command.args(["service",action,"--source-data"]).arg(&args[3]);
            if action=="install"{command.arg("--open-manager");}
            let output=command.creation_flags(0x08000000).output()?;
            anyhow::ensure!(output.status.success(),"{}", uu_i18n::format!("后台服务操作失败（{}）：\n{}",output.status,String::from_utf8_lossy(&output.stderr).trim()));
        }
        Ok(())
    })();
    if let Err(error)=result {show_error(&format!("{error:#}"));std::process::exit(1);}
    Some(Ok(()))
}
fn show_error(message:&str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW,MB_ICONERROR,MB_OK};
    unsafe{MessageBoxW(std::ptr::null_mut(),wide(message).as_ptr(),wide(uu_i18n::text("Eclipse · 后台服务")).as_ptr(),MB_OK|MB_ICONERROR);}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pairing_and_status_updates_preserve_unsaved_settings_and_manager_close_keeps_service_independent(
    ) {
        let root =
            std::env::temp_dir().join(format!("eclipse-service-manager-{}", uuid::Uuid::new_v4()));
        let trust = TrustStore::open(root.join("trust")).unwrap();
        let (sender, jobs) = mpsc::channel();
        let (send, events) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let mut saved = Sharing::default();
        saved.name = "Generated service".into();
        let mut manager = Manager {
            firewall:String::new(),
            virtual_display:Default::default(),
            build:None,
            trust,
            sender,
            events,
            stop: stop.clone(),
            state: SERVICE_RUNNING,
            snapshot: None,
            endpoints: vec![],
            pending: vec![],
            draft: saved.clone(),
            baseline: saved.clone(),
            revision: "0".repeat(64),
            dirty: false,
            busy: true,
            section: 0,
            notice: String::new(),
            codes: HashMap::new(),
            permissions: HashMap::new(),
            pairing: Default::default(),
        };
        manager.draft.audio = false;
        manager.dirty = true;
        send.send(Event::Done(false, Ok(()))).unwrap();
        let status = |saved: Sharing| Response::Status {
            endpoints: vec![],
            pending: vec![],
            snapshot: host_admin::Snapshot {
                own_permissions: Permissions::VIEW,
                connections: vec![],
                server: Some(host_admin::ServerSettings {
                    active: saved.clone(),
                    saved,
                    revision: "1".repeat(64),
                    monitors: vec![],
                    portal: false,
                }),
            },
        };
        send.send(Event::State(SERVICE_RUNNING, Ok(status(saved.clone()))))
            .unwrap();
        manager.poll();
        assert!(manager.dirty);
        assert!(
            !manager.draft.audio,
            "approving a pair must not discard the settings draft"
        );
        saved.audio = false;
        send.send(Event::Done(true, Ok(()))).unwrap();
        send.send(Event::State(SERVICE_RUNNING, Ok(status(saved))))
            .unwrap();
        manager.poll();
        assert!(!manager.dirty);
        assert!(!manager.draft.audio);
        assert_eq!(manager.revision, "1".repeat(64));
        drop(manager);
        assert!(stop.load(Ordering::Acquire));
        assert!(
            jobs.try_recv().is_err(),
            "closing the manager queues no service stop command"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
