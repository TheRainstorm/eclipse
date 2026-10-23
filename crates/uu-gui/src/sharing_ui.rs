//! One host owner for IP, UU and VPS. The GUI owns all human decisions and OS
//! clipboard initialization; listeners share capture defaults and authorization.
use eframe::egui;
use std::{
    sync::{atomic::Ordering, mpsc, Arc},
    thread::JoinHandle,
    time::Duration,
};
use tokio::sync::{mpsc as async_mpsc, watch};
use uu_core::{
    device_auth::{PairApproval, PairRequest, PairStatus, PairingControl},
    device_book::Book,
    device_internet::EndpointStatus,
    device_trust::{Permissions, TrustStore},
    internet::{Provider, Settings},
    native::HostStatus,
    native_lan as lan,
    shared_host::{Access, SessionGate},
};

enum Event {
    Listening(lan::Handle),
    Legacy(lan::Event),
    Clipboard(Arc<uu_core::clipboard::Clipboard>),
    Notice(String),
    Ended,
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
        book.sharing.uu_enabled=true;
        host.start(config.clone(),book,trust,&ctx).unwrap();
        assert!(host.statuses().iter().all(|(n,_)|n!="IP 直连"));
        poll_until(&mut host,&ctx,|h|h.statuses().iter().any(|(_,s)|matches!(s,EndpointStatus::Error(_))));
        assert!(!config.at("native-lan").exists(),"disabled IP must never initialize legacy listener identity or keys");
        host.stop();poll_until(&mut host,&ctx,|h|!h.running());
        let owner=uu_core::shared_host::HostOwner::claim(&config.at("device-trust")).unwrap();drop(owner);
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
        host.allow_pairing();
        assert!(host.pairing_open());
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
    _owner: uu_core::shared_host::HostOwner,
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
    state: watch::Receiver<EndpointStatus>,
}
struct Pending {
    id: String,
    name: String,
    respond: Option<tokio::sync::oneshot::Sender<Option<PairApproval>>>,
    status: watch::Receiver<PairStatus>,
    code: String,
    control: bool,
}
struct LegacyPending {
    request: lan::PairingRequest,
    entered: String,
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
    stopping: bool,
    allow_control: bool,
}
impl HostUi {
    pub fn running(&self) -> bool {
        self.worker.is_some()
    }
    pub fn stopping(&self) -> bool {
        self.stopping
    }
    pub fn statuses(&self) -> Vec<(String, EndpointStatus)> {
        self.endpoints
            .iter()
            .map(|e| (e.name.clone(), e.state.borrow().clone()))
            .collect()
    }
    pub fn sessions(&self) -> Vec<(String, String, HostStatus)> {
        let Some(access)=&self.access else{return Vec::new();};
        let peers=access.store.peers().unwrap_or_default();
        access.sessions.details().into_iter().map(|(id,peer,status)| {
            let name=peers.iter().find(|p|p.device.id()==peer).map(|p|p.device.name.clone()).unwrap_or_else(|| "已授权设备".into());
            (id,name,status)
        }).collect()
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
            "已允许新设备配对。请在控制端发起连接，再输入控制端显示的 8 位配对码。".into();
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
                Ok(()) => "已撤销设备授权".into(),
                Err(e) => format!("无法撤销：{e}"),
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
        anyhow::ensure!(!self.running(), "共享已经开启");
        book.sharing.validate()?;
        anyhow::ensure!(book.sharing.ip_enabled || book.sharing.uu_enabled || book.services.iter().any(|s| s.host_enabled), "请先在连接服务中启用至少一个入口");
        let owner = uu_core::shared_host::HostOwner::claim(&config.at("device-trust"))?;
        let (send, events) = mpsc::channel();
        let (requests, pair_requests) = async_mpsc::unbounded_channel();
        let access = Arc::new(Access {
            store: trust,
            name: book.sharing.name.clone(),
            pairing: Arc::new(PairingControl::new(requests)),
            sessions: Arc::new(SessionGate::default()),
        });
        let (stop, receiver) = watch::channel(false);
        let (stream, _) = watch::channel(HostStatus::Starting);
        let (ip_status, ip_updates) = watch::channel(EndpointStatus::Starting);
        let mut endpoints = Vec::new();
        if book.sharing.ip_enabled {
            endpoints.push(Endpoint { name: "IP 直连".into(), state: ip_updates });
        }
        let mut services = Vec::new();
        if book.sharing.uu_enabled {
            let (state, updates) = watch::channel(EndpointStatus::Starting);
            endpoints.push(Endpoint {
                name: "UU 账号".into(),
                state: updates,
            });
            services.push((
                Settings {
                    provider: Provider::Uu,
                    device_auth: true,
                    ..Default::default()
                },
                state,
            ));
        }
        for service in book.services.iter().filter(|s| s.host_enabled) {
            let (state, updates) = watch::channel(EndpointStatus::Starting);
            endpoints.push(Endpoint {
                name: service.name.clone(),
                state: updates,
            });
            services.push((
                Settings {
                    provider: Provider::SelfHosted,
                    device_auth: true,
                    server: service.address.clone(),
                    device: service.published_name.clone(),
                    server_token: service.token_file.clone(),
                    server_ca: service.certificate.clone(),
                    ..Default::default()
                },
                state,
            ));
        }
        let sharing = book.sharing.clone();
        let server_access = access.clone();
        let context = ctx.clone();
        // The actual UI wakes on a short timer while sharing; the factory only
        // creates transport state here and initializes OS handles on the UI thread.
        let factory = sharing.clipboard.then(|| {
            let send = send.clone();
            Arc::new(move |_id: &str| {
                let state = Arc::new(uu_core::clipboard::Clipboard::default());
                state.enabled.store(true, Ordering::Release);
                send.send(Event::Clipboard(state.clone()))
                    .map_err(|_| anyhow::anyhow!("共享界面已关闭"))?;
                Ok(state)
            }) as lan::ClipboardFactory
        });
        let task=std::thread::Builder::new().name("eclipse-shared-host".into()).spawn(move || {
            let result=(||->anyhow::Result<()> {
                let runtime=tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build()?;
                let result=runtime.block_on(async {
                    let (lan_events,mut receive)=async_mpsc::unbounded_channel();
                    let forward=send.clone();let progress=stream.clone();
                    let forwarding=tokio::spawn(async move {while let Some(event)=receive.recv().await {
                        if let lan::Event::SessionProgress{status,..}=&event {progress.send_replace(status.clone());}
                        if forward.send(Event::Legacy(event)).is_err(){break;}
                    }});
                    let mut tasks=tokio::task::JoinSet::new();
                    if sharing.ip_enabled {
                    let lan_config=lan::Config{video_policy:sharing.native().video_policy,codec:sharing.native().codec,rate_control:sharing.native().rate_control,name:sharing.name.clone(),bind:([0,0,0,0],sharing.service_port).into(),
                        discovery_bind:Some(([0,0,0,0],sharing.discovery_port).into()),data_dir:config.at("native-lan"),
                        source:sharing.native().source,
                        fps:sharing.fps,bitrate_kbps:sharing.bitrate_mbps*1000,encoder:sharing.encoding.native(),
                        allow_input:sharing.control,audio:sharing.audio.then_some(uu_core::native::AudioSource::System),clipboard:factory.clone()};
                    match lan::Server::bind_with_access(lan_config,receiver.clone(),lan_events,server_access.clone()).await {
                        Ok((server,handle))=>{
                            ip_status.send_replace(EndpointStatus::Ready);send.send(Event::Listening(handle)).ok();
                            tasks.spawn(async move {let result=server.run().await;
                                ip_status.send_replace(match &result {Ok(())=>EndpointStatus::Stopped,Err(e)=>EndpointStatus::Error(format!("IP 服务异常：{e:#}"))});result
                            });
                        },
                        Err(error)=>{let text=format!("IP 共享不可用：{error:#}");ip_status.send_replace(EndpointStatus::Error(text.clone()));send.send(Event::Notice(text)).ok();},
                    }
                    }
                    for (settings,status) in services {
                        let config=config.clone();let defaults=sharing.native();let access=server_access.clone();
                        let stop=receiver.clone();let phase=stream.clone();let factory=factory.clone();
                        tasks.spawn(async move {uu_core::device_internet::listen(settings,config,defaults,access,stop,status,phase,factory).await;Ok(())});
                    }
                    let mut stop=receiver.clone();
                    while !*stop.borrow() && !tasks.is_empty() {
                        tokio::select! {
                            _=stop.changed()=>break,
                            result=tasks.join_next()=>{
                                match result {Some(Err(error))=>{send.send(Event::Notice(format!("共享线程异常：{error}"))).ok();},Some(Ok(Err(error)))=>{send.send(Event::Notice(format!("共享服务异常：{error:#}"))).ok();},_=>{}}
                            }
                        }
                    }
                    let drain=async {while tasks.join_next().await.is_some(){}};
                    if tokio::time::timeout(Duration::from_secs(5),drain).await.is_err(){tasks.abort_all();while tasks.join_next().await.is_some(){}}
                    forwarding.abort();let _=forwarding.await;Ok::<_,anyhow::Error>(())
                });
                runtime.shutdown_timeout(Duration::from_secs(2));result
            })();
            if let Err(error)=result {send.send(Event::Notice(format!("无法开启共享：{error:#}"))).ok();}
            send.send(Event::Ended).ok();context.request_repaint();
        })?;
        self.worker = Some(Worker {
            stop,
            task: Some(task),
            _owner: owner,
        });
        self.events = Some(events);
        self.requests = Some(pair_requests);
        self.access = Some(access);
        self.endpoints = endpoints;
        self.allow_control = book.sharing.control;
        self.stopping = false;
        self.notice.clear();
        Ok(())
    }
    pub fn poll(&mut self, ctx: &egui::Context, window: isize) {
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
                            state.status(format!("剪贴板不可用：{error}"));
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
                });
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                    egui::UserAttentionType::Informational,
                ));
            }
        }
        self.pending.retain(|p| match *p.status.borrow() {
            PairStatus::Paired => {
                self.notice = format!("已保存 {} 的设备授权", p.name);
                false
            }
            PairStatus::Failed => {
                self.notice = format!("{} 的配对未完成，请核对短码后重新发起。", p.name);
                false
            }
            _ => true,
        });
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
        }
    }
    pub fn dialogs(&mut self, ctx: &egui::Context) {
        let mut remove = None;
        for (index, pending) in self.pending.iter_mut().enumerate() {
            let mut open = true;
            let waiting = *pending.status.borrow() == PairStatus::Waiting;
            egui::Window::new("允许新设备")
                .id(egui::Id::new(("device-pair", &pending.id)))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(430.)
                .show(ctx, |ui| {
                    ui.heading(format!("{} 希望连接本机", pending.name));
                    ui.add_space(8.);
                    if waiting {
                        ui.label("输入控制端显示的 8 位配对码：");
                        ui.add(
                            egui::TextEdit::singleline(&mut pending.code)
                                .hint_text("例如 4827 1936")
                                .char_limit(12),
                        );
                        ui.add_enabled_ui(self.allow_control, |ui| {
                            ui.checkbox(&mut pending.control, "允许键盘、鼠标和手柄控制");
                        });
                        ui.weak("批准后记住这台设备，以后连接不再输入配对码。");
                        let permission = if pending.control {
                            Permissions::CONTROL
                        } else {
                            Permissions::VIEW
                        };
                        let approval = PairApproval::new(&pending.code, permission);
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    approval.is_ok(),
                                    egui::Button::new(
                                        egui::RichText::new("允许并保存设备")
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
                            if ui.button("拒绝").clicked() {
                                remove = Some(index);
                            }
                        });
                    } else {
                        ui.spinner();
                        ui.label("正在核验并保存设备身份…");
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
            egui::Window::new("确认旧版 IP 设备配对")
                .id("legacy-device-pair".into())
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!("{} 请求配对", pending.request.client_name));
                    ui.label("输入控制端显示的八位核对码：");
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
                            .add_enabled(entered == expected, egui::Button::new("允许配对"))
                            .clicked()
                        {
                            decision = Some(true);
                        }
                        if ui.button("拒绝").clicked() {
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
