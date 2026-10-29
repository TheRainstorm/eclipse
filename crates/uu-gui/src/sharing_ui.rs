//! One host owner for IP, installed connection services and VPS. The GUI owns all human decisions and OS
//! clipboard initialization; listeners share capture defaults and authorization.
use eframe::egui;
use std::{
    sync::{atomic::Ordering, mpsc, Arc},
    thread::JoinHandle,
    time::Duration,
};
use tokio::sync::{mpsc as async_mpsc, watch};
use uu_core::{
    device_auth::{PairApproval, PairRequest, PairStatus},
    device_book::Book,
    device_internet::EndpointStatus,
    device_trust::{Permissions, TrustStore},
    native::HostStatus,
    native_lan as lan,
    shared_host::Access,
};

enum Event {
    Listening(lan::Handle),
    Legacy(lan::Event),
    Clipboard(Arc<uu_core::clipboard::Clipboard>),
    Notice(String),
    Ended,
    Restart,
}

pub(super) fn permissions(ui:&mut egui::Ui,value:&mut Permissions)->bool {
    let mut details=value.details();
    let mut changed=false;
    changed|=ui.checkbox(&mut details.keyboard,uu_i18n::text("键盘")).changed();
    changed|=ui.checkbox(&mut details.mouse,uu_i18n::text("鼠标")).changed();
    changed|=ui.checkbox(&mut details.gamepad,uu_i18n::text("手柄")).changed();
    changed|=ui.checkbox(&mut details.clipboard,uu_i18n::text("剪贴板")).changed();
    changed|=ui.checkbox(&mut details.manage_server,uu_i18n::text("管理服务和连接权限")).changed();
    if changed {*value=value.with_details(details);}
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    fn poll_until(host: &mut HostUi, ctx: &egui::Context, condition: impl Fn(&HostUi) -> bool) {
        let start = std::time::Instant::now();
        while !condition(host) {
            host.poll(ctx, 0);
            assert!(
                start.elapsed() < Duration::from_secs(8),
                "host transition timed out: {:?}",
                host.statuses()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn disabled_ip_never_binds_or_creates_legacy_listener_state() {
        let root=std::env::temp_dir().join(format!("eclipse-host-no-ip-{}",uuid::Uuid::new_v4()));
        let config=uu_core::Config{data_dir:root.join("data"),state_dir:root.join("state"),signaling_host:"unreachable.example.invalid".into()};
        let trust=TrustStore::open(config.at("device-trust")).unwrap();
        let ctx=egui::Context::default();
        let mut host=HostUi::default();
        let mut book=Book::default();book.sharing.ip_enabled=false;
        assert!(host.start(config.clone(),book.clone(),trust.clone(),&ctx).unwrap_err().to_string().contains("至少一个"));
        book.sharing.plugin_services=vec!["example-addon".into()];
        assert!(host.start(config.clone(),book,trust,&ctx).unwrap_err().to_string().contains("至少一个"));
        assert!(!host.uses_provider("example-addon"));
        assert!(host.statuses().iter().all(|(n,_)|n!="IP 直连"));
        assert!(!config.at("native-lan").exists(),"disabled IP must never initialize legacy listener identity or keys");
        host.stop();poll_until(&mut host,&ctx,|h|!h.running());
        assert!(!host.uses_provider("example-addon"));
        let owner=uu_core::shared_host::HostOwner::claim(&config.at("device-trust")).unwrap();drop(owner);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn saved_settings_wait_for_restart_and_restart_releases_the_old_listener_first() {
        let root=std::env::temp_dir().join(format!("eclipse-host-restart-{}",uuid::Uuid::new_v4()));
        let config=uu_core::Config{data_dir:root.join("data"),state_dir:root.join("state"),signaling_host:"unreachable.example.invalid".into()};
        let trust=TrustStore::open(config.at("device-trust")).unwrap();
        let store=uu_core::device_book::Store::open(config.at("workspace")).unwrap();
        let ctx=egui::Context::default();
        let tcp=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let udp=std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        let mut book=Book::default();
        book.sharing.service_port=tcp.local_addr().unwrap().port();
        book.sharing.discovery_port=udp.local_addr().unwrap().port();
        book.sharing.audio=false;book.sharing.control=false;
        drop((tcp,udp));
        store.sharing(book.sharing.clone()).unwrap();
        let mut host=HostUi::default();
        host.start(config.clone(),book.clone(),trust.clone(),&ctx).unwrap();
        poll_until(&mut host,&ctx,|h|h.statuses().iter().any(|(_,s)|matches!(s,EndpointStatus::Ready)));
        let old_port=book.sharing.service_port;
        let next=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        book.sharing.service_port=next.local_addr().unwrap().port();
        book.sharing.audio=true;
        store.sharing(book.sharing.clone()).unwrap();
        assert!(host.changes_pending(&store.read().unwrap().sharing));
        assert!(!host.active_sharing.as_ref().unwrap().audio);
        let mut saved = store.read().unwrap().sharing;
        saved.plugin_services=vec!["example-addon".into()];
        store.sharing(saved).unwrap();
        assert!(!host.uses_provider("example-addon"), "a saved extension entrance is inactive until restart");
        store.sharing(book.sharing.clone()).unwrap();
        assert!(std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,old_port)).is_err());
        let mut invalid=book.clone();invalid.sharing.name.clear();
        assert!(host.restart(config.clone(),invalid,trust.clone(),&ctx).is_err());
        assert!(!host.stopping());
        drop(next);
        host.restart(config,store.read().unwrap(),trust,&ctx).unwrap();
        poll_until(&mut host,&ctx,|h|!h.stopping()&&h.active_sharing.as_ref().is_some_and(|s|s.audio)&&h.statuses().iter().any(|(_,s)|matches!(s,EndpointStatus::Ready)));
        assert!(!host.changes_pending(&book.sharing));
        let _old=std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,old_port)).unwrap();
        assert!(std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,book.sharing.service_port)).is_err());
        host.stop();poll_until(&mut host,&ctx,|h|!h.running());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn shared_host_reports_port_conflicts_and_releases_all_listeners_and_ownership_on_stop() {
        let root =
            std::env::temp_dir().join(format!("eclipse-sharing-ui-{}", uuid::Uuid::new_v4()));
        let config = uu_core::Config {
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            signaling_host: "unreachable.example.invalid".into(),
        };
        let trust = TrustStore::open(config.at("device-trust")).unwrap();
        let ctx = egui::Context::default();
        let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let udp = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        let mut book = Book::default();
        book.sharing.audio = false;
        book.sharing.control = false;
        book.sharing.service_port = tcp.local_addr().unwrap().port();
        book.sharing.discovery_port = udp.local_addr().unwrap().port();
        let mut host = HostUi::default();
        host.start(config.clone(), book.clone(), trust.clone(), &ctx)
            .unwrap();
        poll_until(&mut host, &ctx, |host| !host.running());
        assert!(host.statuses().iter().any(
            |(_, status)| matches!(status,EndpointStatus::Error(text) if text.contains("IP"))
        ));
        drop(tcp);
        drop(udp);
        host.start(config.clone(), book.clone(), trust.clone(), &ctx)
            .unwrap();
        poll_until(&mut host, &ctx, |h| {
            h.statuses()
                .iter()
                .any(|(_, s)| matches!(s, EndpointStatus::Ready))
        });
        assert!(std::net::TcpListener::bind((
            std::net::Ipv4Addr::LOCALHOST,
            book.sharing.service_port
        ))
        .is_err());
        let mut other = HostUi::default();
        assert!(other
            .start(config.clone(), book.clone(), trust, &ctx)
            .is_err());
        assert!(
            host.access
                .as_ref()
                .unwrap()
                .sessions
                .active_peer()
                .is_none(),
            "starting listeners alone never acquires media"
        );
        assert!(host.pairing_open(),"starting the server accepts pairing without a second switch");
        host.stop();
        assert!(!host.pairing_open());
        poll_until(&mut host, &ctx, |host| !host.running());
        let _tcp =
            std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, book.sharing.service_port))
                .unwrap();
        let _udp = std::net::UdpSocket::bind((
            std::net::Ipv4Addr::UNSPECIFIED,
            book.sharing.discovery_port,
        ))
        .unwrap();
        let _owner = uu_core::shared_host::HostOwner::claim(&config.at("device-trust")).unwrap();
        drop(_owner);
        std::fs::remove_dir_all(root).unwrap();
    }
}
struct Worker {
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}
struct Endpoint {
    name: String,
    provider:Option<String>,
    state: watch::Receiver<EndpointStatus>,
}
struct Pending {
    id: String,
    name: String,
    respond: Option<tokio::sync::oneshot::Sender<Option<PairApproval>>>,
    status: watch::Receiver<PairStatus>,
    code: String,
    control: bool,
    manage:bool,
    temporary:bool,
}
struct LegacyPending {
    request: lan::PairingRequest,
    entered: String,
}

pub(super) struct SessionView {
    pub id:String,
    pub peer_id:String,
    pub name:String,
    pub status:HostStatus,
    pub permissions:Permissions,
}

#[derive(Default)]
pub(super) struct HostUi {
    worker: Option<Worker>,
    events: Option<mpsc::Receiver<Event>>,
    requests: Option<async_mpsc::UnboundedReceiver<PairRequest>>,
    handle: Option<lan::Handle>,
    pub access: Option<Arc<Access>>,
    endpoints: Vec<Endpoint>,
    pending: Vec<Pending>,
    legacy_pending: Vec<LegacyPending>,
    pub notice: String,
    notice_changed_at: Option<std::time::Instant>,
    last_notice: String,
    stopping: bool,
    allow_control: bool,
    active_sharing: Option<uu_core::device_book::Sharing>,
    restart: Option<(uu_core::Config, Book, TrustStore)>,
    restart_context:Option<(uu_core::Config,TrustStore)>,
}
impl HostUi {
    pub fn running(&self) -> bool {
        self.worker.is_some()
    }
    pub fn stopping(&self) -> bool {
        self.stopping
    }
    pub fn changes_pending(&self,saved:&uu_core::device_book::Sharing)->bool {
        self.active_sharing.as_ref().is_some_and(|active|active!=saved)
    }
    pub fn uses_provider(&self,id:&str) -> bool {
        self.endpoints.iter().any(|entry|entry.provider.as_deref()==Some(id))
            || self.restart.as_ref().is_some_and(|(_,book,_)|book.sharing.plugin_services.iter().any(|provider|provider==id))
    }
    pub fn restart(&mut self,config:uu_core::Config,book:Book,trust:TrustStore,ctx:&egui::Context)->anyhow::Result<()> {
        book.sharing.validate()?;
        anyhow::ensure!(book.sharing.ip_enabled || !book.sharing.plugin_services.is_empty() || book.services.iter().any(|s|s.host_enabled),"{}", uu_i18n::format!("请先启用至少一个连接入口"));
        if !self.running() {return self.start(config,book,trust,ctx);}
        anyhow::ensure!(!self.stopping,"{}", uu_i18n::format!("服务正在停止，请稍后重试"));
        self.stop();
        self.restart=Some((config,book,trust));
        self.notice=uu_i18n::text("正在停止旧服务，清理完成后应用已保存配置…").into();
        Ok(())
    }
    pub fn notice_is_error(&self) -> bool {
        super::ui_feedback::looks_like_error(&self.notice)
    }
    pub fn clear_notice(&mut self) {
        self.notice.clear();
        self.last_notice.clear();
        self.notice_changed_at = None;
    }
    pub fn statuses(&self) -> Vec<(String, EndpointStatus)> {
        self.endpoints
            .iter()
            .map(|e| (e.name.clone(), e.state.borrow().clone()))
            .collect()
    }
    pub fn sessions(&self) -> Vec<SessionView> {
        let Some(access)=&self.access else{return Vec::new();};
        access.sessions.details().into_iter().map(|(id,peer,status)| {
            let record=access.peer_permissions(&peer).ok().flatten();
            let name=record.as_ref().map(|(d,_)|d.name.clone()).unwrap_or_else(||uu_i18n::text("已授权设备").into());
            let permissions=record.map(|(_,p)|p).unwrap_or(Permissions::VIEW);
            SessionView{id,peer_id:peer,name,status,permissions}
        }).collect()
    }
    pub fn set_session_permissions(&self, peer_id:&str, permissions:Permissions) -> anyhow::Result<()> {
        let access=self.access.as_ref().ok_or_else(||anyhow::anyhow!("{}", uu_i18n::format!("本机共享未运行")))?;
        access.set_permissions(peer_id,permissions)?;
        Ok(())
    }
    pub fn disconnect_session(&self, id:&str) {
        if let Some(access)=&self.access {access.sessions.disconnect_session(id);}
    }
    pub fn pairing_open(&self) -> bool {
        self.access.as_ref().is_some_and(|a| a.pairing.is_open())
    }
    pub fn allow_pairing(&mut self) {
        if let Some(access) = &self.access {
            access.pairing.open();
        }
        if let Some(handle) = &self.handle {
            handle.allow_pairing();
        }
        self.notice =
            uu_i18n::text("已允许新设备配对。请在控制端发起连接，再输入控制端显示的 8 位配对码。").into();
    }
    pub fn close_pairing(&mut self) {
        if let Some(access) = &self.access {
            access.pairing.close();
        }
        if let Some(handle) = &self.handle {
            handle.close_pairing();
        }
    }
    pub fn stop(&mut self) {
        self.restart=None;
        self.close_pairing();
        if let Some(access) = &self.access {
            access.sessions.disconnect();
        }
        if let Some(handle) = &self.handle {
            handle.stop_session();
        }
        if let Some(worker) = &self.worker {
            worker.stop.send_replace(true);
            self.stopping = true;
        }
    }
    pub fn disconnect(&self) {
        if let Some(access) = &self.access {
            access.sessions.disconnect();
        }
        if let Some(handle) = &self.handle {
            handle.stop_session();
        }
    }
    pub fn legacy_clients(&self) -> Vec<uu_core::native_lan_store::AuthorizedClient> {
        self.handle
            .as_ref()
            .map(|h| h.clients())
            .unwrap_or_default()
    }
    pub fn server_id(&self) -> Option<&str> {
        self.handle.as_ref().map(|h| h.server_id())
    }
    pub fn revoke_legacy(&mut self, id: &str) {
        if let Some(handle) = &self.handle {
            self.notice = match handle.revoke(id) {
                Ok(()) => uu_i18n::text("已撤销设备授权").into(),
                Err(e) => uu_i18n::format!("无法撤销：{e}"),
            };
        }
    }
    pub fn start(
        &mut self,
        config: uu_core::Config,
        book: Book,
        trust: TrustStore,
        ctx: &egui::Context,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!self.running(), "{}", uu_i18n::format!("共享已经开启"));
        let (send, events) = mpsc::channel();
        let factory = book.sharing.clipboard.then(|| {
            let send = send.clone();
            Arc::new(move |_id: &str| {
                let state = Arc::new(uu_core::clipboard::Clipboard::default());
                state.enabled.store(true, Ordering::Release);
                send.send(Event::Clipboard(state.clone()))
                    .map_err(|_| anyhow::anyhow!("{}", uu_i18n::format!("共享界面已关闭")))?;
                Ok(state)
            }) as lan::ClipboardFactory
        });
        let restart_send = send.clone();
        let restart_context = ctx.clone();
        let forward = send.clone();
        let prepared = uu_core::host_runtime::prepare(config.clone(), book.clone(), trust, factory,
            move || {
                restart_send.send(Event::Restart).map_err(|_|anyhow::anyhow!("{}", uu_i18n::format!("服务管理窗口已经关闭")))?;
                restart_context.request_repaint(); Ok(())
            },
            move |event| {
                let event = match event {
                    uu_core::host_runtime::Event::Listening(handle) => Event::Listening(handle),
                    uu_core::host_runtime::Event::Legacy(event) => Event::Legacy(event),
                    uu_core::host_runtime::Event::Notice(message) => Event::Notice(message),
                };
                let _ = forward.send(event);
            },
        )?;
        let access = prepared.access;
        let pair_requests = prepared.requests;
        let endpoints = prepared.endpoints.into_iter().map(|entry| Endpoint {name:entry.name,state:entry.state,provider:entry.provider}).collect();
        self.restart_context = Some((config, access.store.clone()));
        let (stop, receiver) = watch::channel(false);
        let context = ctx.clone();
        let task = std::thread::Builder::new().name("eclipse-shared-host".into()).spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build()?;
                let result = runtime.block_on(prepared.runner.run(receiver));
                runtime.shutdown_timeout(Duration::from_secs(2));
                result
            })();
            if let Err(error) = result { let _ = send.send(Event::Notice(uu_i18n::format!("无法开启共享：{error:#}"))); }
            let _ = send.send(Event::Ended);
            context.request_repaint();
        })?;
        self.worker = Some(Worker {
            stop,
            task: Some(task),
        });
        self.events = Some(events);
        self.requests = Some(pair_requests);
        self.access = Some(access);
        self.endpoints = endpoints;
        self.allow_control = book.sharing.control;
        self.active_sharing=Some(book.sharing);
        self.stopping = false;
        self.notice.clear();
        Ok(())
    }
    pub fn poll(&mut self, ctx: &egui::Context, window: isize) {
        let mut restart_requested=false;
        if self.notice != self.last_notice {
            self.last_notice = self.notice.clone();
            self.notice_changed_at = (!self.notice.is_empty()).then(std::time::Instant::now);
        }
        if !self.notice.is_empty()
            && !super::ui_feedback::looks_like_error(&self.notice)
            && self.notice_changed_at.is_some_and(|at| at.elapsed() >= Duration::from_secs(2))
        {
            self.notice.clear();
            self.last_notice.clear();
            self.notice_changed_at = None;
        }
        if self.notice_changed_at.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if self.running() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if let Some(events) = &self.events {
            while let Ok(event) = events.try_recv() {
                match event {
                    Event::Listening(handle) => self.handle = Some(handle),
                    Event::Notice(message) => self.notice = message,
                    Event::Clipboard(state) => {
                        if self.stopping {
                            state.stopped.store(true, Ordering::Release);
                            state.enabled.store(false, Ordering::Release);
                        } else if let Err(error) =
                            super::clipboard::start_host(state.clone(), window)
                        {
                            state.status(uu_i18n::format!("剪贴板不可用：{error}"));
                        }
                    }
                    Event::Legacy(event) => match event {
                        lan::Event::Pairing(request) => {
                            self.legacy_pending.push(LegacyPending {
                                request,
                                entered: String::new(),
                            });
                            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                                egui::UserAttentionType::Informational,
                            ));
                        }
                        lan::Event::PairingEnded(id) => {
                            self.legacy_pending.retain(|p| p.request.id != id)
                        }
                        lan::Event::Warning(message) => self.notice = message,
                        _ => {}
                    },
                    Event::Ended => {}
                    Event::Restart=>restart_requested=true,
                }
            }
        }
        if let Some(requests) = &mut self.requests {
            while let Ok(request) = requests.try_recv() {
                self.pending.push(Pending {
                    id: request.id,
                    name: request.device.name,
                    respond: Some(request.respond),
                    status: request.status,
                    code: String::new(),
                    control: self.allow_control,
                    manage:false,
                    temporary:request.method==uu_core::device_auth::PairingMethod::TemporaryCode,
                });
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                    egui::UserAttentionType::Informational,
                ));
            }
        }
        self.pending.retain(|p| match *p.status.borrow() {
            PairStatus::Paired => {
                self.notice = uu_i18n::format!("已保存 {} 的设备授权", p.name);
                false
            }
            PairStatus::Failed => {
                self.notice = uu_i18n::format!("{} 的配对未完成，请核对短码后重新发起。", p.name);
                false
            }
            _ => true,
        });
        if restart_requested&&!self.stopping {
            let result=(||->anyhow::Result<()> {
                let (config,trust)=self.restart_context.clone().ok_or_else(||anyhow::anyhow!("{}", uu_i18n::format!("服务管理已关闭")))?;
                let book=uu_core::device_book::Store::open(config.at("device-trust"))?.read()?;
                self.restart(config,book,trust,ctx)
            })();
            if let Err(error)=result {self.notice=uu_i18n::format!("远程重启服务失败：{error:#}");}
        }
        if self
            .worker
            .as_ref()
            .is_some_and(|w| w.task.as_ref().is_some_and(|t| t.is_finished()))
        {
            self.worker = None;
            self.handle = None;
            self.access = None;
            self.events = None;
            self.requests = None;
            self.pending.clear();
            self.legacy_pending.clear();
            self.stopping = false;
            self.active_sharing=None;
        }
        if !self.running() {
            if let Some((config,book,trust))=self.restart.take() {
                if let Err(error)=self.start(config,book,trust,ctx) {
                    self.notice=uu_i18n::format!("重新启动服务失败：{error:#}");
                }
            }
        }
    }
    pub fn dialogs(&mut self, ctx: &egui::Context) {
        let mut remove = None;
        for (index, pending) in self.pending.iter_mut().enumerate() {
            let mut open = true;
            let waiting = *pending.status.borrow() == PairStatus::Waiting;
            egui::Window::new(uu_i18n::text("允许新设备"))
                .id(egui::Id::new(("device-pair", &pending.id)))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(430.)
                .show(ctx, |ui| {
                    ui.heading(uu_i18n::format!("{} 希望连接本机", pending.name));
                    ui.add_space(8.);
                    if waiting {
                        ui.label(uu_i18n::text("输入控制端显示的 8 位配对码："));
                        ui.add(
                            egui::TextEdit::singleline(&mut pending.code)
                                .hint_text(uu_i18n::text("例如 4827 1936"))
                                .char_limit(12),
                        );
                        ui.add_enabled_ui(self.allow_control, |ui| {
                            ui.checkbox(&mut pending.control, uu_i18n::text("允许键盘、鼠标和手柄控制"));
                        });
                        ui.weak(if pending.temporary{uu_i18n::text("本次为临时配对。批准后撤销此前长期授权，仅允许这次串流，断开后必须重新配对。")}else{uu_i18n::text("批准后记住这台设备，以后连接不再输入配对码。")});
                        let permission = if pending.control {
                            Permissions::CONTROL
                        } else {
                            Permissions::VIEW
                        };
                        ui.checkbox(&mut pending.manage,uu_i18n::text("允许管理服务配置和连接权限"));
                        let mut details=permission.details();details.manage_server=pending.manage;
                        let permission=if pending.manage {permission.with_details(details)}else{permission};
                        let approval = PairApproval::new(&pending.code, permission);
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    approval.is_ok(),
                                    egui::Button::new(
                                        egui::RichText::new(if pending.temporary{uu_i18n::text("允许本次串流")}else{uu_i18n::text("允许并保存设备")})
                                            .color(egui::Color32::WHITE),
                                    )
                                    .fill(super::theme::ACCENT),
                                )
                                .clicked()
                            {
                                if let Some(respond) = pending.respond.take() {
                                    let _ = respond.send(approval.ok());
                                }
                            }
                            if ui.button(uu_i18n::text("拒绝")).clicked() {
                                remove = Some(index);
                            }
                        });
                    } else {
                        ui.spinner();
                        ui.label(uu_i18n::text("正在核验并保存设备身份…"));
                    }
                });
            if !open {
                remove = Some(index);
            }
        }
        if let Some(index) = remove {
            let pending = self.pending.remove(index);
            if let Some(respond) = pending.respond {
                let _ = respond.send(None);
            }
        }
        let mut decision = None;
        if let Some(pending) = self.legacy_pending.first_mut() {
            egui::Window::new(uu_i18n::text("确认旧版 IP 设备配对"))
                .id("legacy-device-pair".into())
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(uu_i18n::format!("{} 请求配对", pending.request.client_name));
                    ui.label(uu_i18n::text("输入控制端显示的八位核对码："));
                    ui.text_edit_singleline(&mut pending.entered);
                    let entered: String = pending
                        .entered
                        .chars()
                        .filter(char::is_ascii_digit)
                        .collect();
                    let expected: String = pending
                        .request
                        .code
                        .chars()
                        .filter(char::is_ascii_digit)
                        .collect();
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(entered == expected, egui::Button::new(uu_i18n::text("允许配对")))
                            .clicked()
                        {
                            decision = Some(true);
                        }
                        if ui.button(uu_i18n::text("拒绝")).clicked() {
                            decision = Some(false);
                        }
                    });
                });
        }
        if let Some(decision) = decision {
            let pending = self.legacy_pending.remove(0);
            let _ = pending.request.decision.send(decision);
        }
    }
}
