//! Explicit local sharing controls and first-pair confirmation.

#[cfg(test)]
mod port_tests {
    use super::*;
    #[test]
    fn completed_start_failure_is_shown_after_worker_cleanup() {
        let (send, receive)=mpsc::channel();
        let (stop, _)=watch::channel(false);
        let task=std::thread::spawn(move || {send.send(Ready::Ended(Err("TCP fixture-port 已被占用".into()))).unwrap();});
        while !task.is_finished() {std::thread::yield_now();}
        let mut host=HostUi::default();
        host.status="正在开启共享…".into();
        host.ready=Some(receive);
        host.worker=Some(Worker{stop,task:Some(task)});
        host.poll(&egui::Context::default());
        assert!(!host.running());
        assert!(host.open);
        assert!(host.notice.contains("已被占用"));
        host.configure_ports(51000,51001);
        assert_eq!(host.ports(),(51000,51001));
        host.poll(&egui::Context::default());
        assert!(host.notice.contains("已被占用"),"failure must remain visible on later frames");
    }
}

use eframe::egui;
use std::{
    net::SocketAddr,
    path::Path,
    sync::{atomic::Ordering, mpsc, Arc},
    thread::JoinHandle,
    time::Duration,
};
use tokio::sync::{mpsc as async_mpsc, watch};
use uu_core::{native, native_lan as lan};
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
struct Pending {
    request: lan::PairingRequest,
    entered: String,
}
enum Ready {
    Listening(lan::Handle, SocketAddr),
    Ended(Result<(), String>),
    Clipboard {
        id: String,
        state: Arc<uu_core::clipboard::Clipboard>,
    },
}
pub(super) struct HostUi {
    pub open: bool,
    name: String,
    port: u16,
    discovery_port: u16,
    monitors: Vec<native::CaptureMonitor>,
    monitor: usize,
    fps: u32,
    mbps: u32,
    encoder: native::EncoderPreference,
    input: bool,
    audio: bool,
    clipboard: bool,
    clipboard_state: Option<(String, Arc<uu_core::clipboard::Clipboard>)>,
    owner_window: isize,
    worker: Option<Worker>,
    ready: Option<mpsc::Receiver<Ready>>,
    events: Option<async_mpsc::UnboundedReceiver<lan::Event>>,
    handle: Option<lan::Handle>,
    notice: String,
    status: String,
    active_session: Option<String>,
    pending: Vec<Pending>,
    addresses: Vec<String>,
    stopping: bool,
}
impl Default for HostUi {
    fn default() -> Self {
        Self {
            open: false,
            name: std::env::var("COMPUTERNAME")
                .ok()
                .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
                .map(|s| {
                    s.trim()
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(64)
                        .collect()
                })
                .filter(|s: &String| !s.is_empty())
                .unwrap_or_else(|| "Eclipse server".into()),
            port: uu_core::native_discovery::SERVICE_PORT,
            discovery_port: uu_core::native_discovery::DISCOVERY_PORT,
            monitors: Vec::new(),
            monitor: 0,
            fps: 60,
            mbps: 18,
            encoder: native::EncoderPreference::Auto,
            input: true,
            audio: true,
            clipboard: false,
            clipboard_state: None,
            owner_window: 0,
            worker: None,
            ready: None,
            events: None,
            handle: None,
            notice: String::new(),
            status: "尚未启用共享".into(),
            active_session: None,
            pending: Vec::new(),
            addresses: Vec::new(),
            stopping: false,
        }
    }
}
impl HostUi {
    pub fn ports(&self) -> (u16, u16) { (self.port, self.discovery_port) }
    pub fn configure_ports(&mut self, service: u16, discovery: u16) {
        if !self.running() { self.port=service; self.discovery_port=discovery; }
    }
    pub fn running(&self) -> bool {
        self.worker.is_some()
    }
    pub fn stop(&mut self) {
        if let Some(worker) = &self.worker {
            worker.stop.send_replace(true);
            self.stopping = true;
            self.status = "正在停止共享…".into();
        }
    }
    pub fn show(&mut self) {
        self.open = true;
        if !self.running() {
            self.refresh_monitors();
        }
    }
    fn refresh_monitors(&mut self) {
        #[cfg(target_os = "linux")]
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            self.monitors.clear();
            self.notice =
                "当前 Wayland 会话尚未支持共享屏幕；可连接其它主机，或在 X11 会话共享本机。".into();
            return;
        }
        match native::monitors() {
            Ok(monitors) => {
                self.monitors = monitors;
                self.monitor = self.monitor.min(self.monitors.len().saturating_sub(1));
                self.notice.clear();
            }
            Err(error) => {
                self.monitors.clear();
                self.notice = format!("无法获取共享屏幕：{error}");
            }
        }
    }
    fn poll_ready(&mut self) {
        if let Some(rx) = &self.ready {
            while let Ok(event) = rx.try_recv() {
                match event {
                    Ready::Clipboard { id, state } => {
                        if !state.stopped.load(Ordering::Acquire) {
                            if let Err(error) =
                                super::clipboard::start_host(state.clone(), self.owner_window)
                            {
                                state.available.store(false, Ordering::Release);
                                state.status("无法启动本机文本剪贴板");
                                self.notice = format!("无法启用文本剪贴板：{error}");
                            }
                            self.clipboard_state = Some((id, state));
                        }
                    }
                    Ready::Listening(handle, address) => {
                        self.handle = Some(handle);
                        self.status = "共享已开启，等待连接".into();
                        self.addresses = super::native_interfaces::enumerate()
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|a|a.ip.is_ipv4()==address.is_ipv4())
                            .map(|a| SocketAddr::new(a.ip, address.port()).to_string())
                            .collect();
                    }
                    Ready::Ended(result) => {
                        self.addresses.clear();
                        if let Err(error) = result {
                            self.notice = format!("无法保持共享：{error}");
                            self.open = true;
                        }
                        self.status = "共享已停止".into();
                    }
                }
            }
        }
    }
    pub fn poll(&mut self, ctx: &egui::Context) {
        self.poll_ready();
        if let Some(rx) = &mut self.events {
            while let Ok(event) = rx.try_recv() {
                match event {
                    lan::Event::Pairing(request) => {
                        self.pending.push(Pending {
                            request,
                            entered: String::new(),
                        });
                        self.open = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                            egui::UserAttentionType::Informational,
                        ));
                    }
                    lan::Event::PairingEnded(id) => self.pending.retain(|p| p.request.id != id),
                    lan::Event::Warning(message) => self.notice = message,
                    lan::Event::SessionStarted { id, client } => {
                        let name = self
                            .handle
                            .as_ref()
                            .and_then(|h| h.clients().into_iter().find(|p| p.id == client))
                            .map(|p| p.name)
                            .unwrap_or_else(|| "已配对设备".into());
                        self.status = format!("正在为 {name} 建立串流");
                        self.active_session = Some(id);
                    }
                    lan::Event::SessionProgress { id, status } => {
                        if self.active_session.as_deref() == Some(&id) {
                            self.status = match status {
                                native::HostStatus::Starting => "正在准备串流…".into(),
                                native::HostStatus::Waiting { .. } => {
                                    "等待已授权设备建立媒体连接…".into()
                                }
                                native::HostStatus::Preparing => "正在准备屏幕采集与编码…".into(),
                                native::HostStatus::Streaming {
                                    width,
                                    height,
                                    fps,
                                    audio,
                                    encoder,
                                } => {
                                    format!("串流中 · {width}×{height} @ {fps} FPS · {encoder} · {audio}")
                                }
                            };
                        }
                    }
                    lan::Event::SessionEnded(id) => {
                        if self
                            .clipboard_state
                            .as_ref()
                            .is_some_and(|(session, _)| session == &id)
                        {
                            self.clipboard_state = None;
                        }
                        if self.active_session.as_deref() == Some(&id) {
                            self.active_session = None;
                            self.status = "共享已开启，等待连接".into();
                        }
                    }
                }
            }
        }
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.task.as_ref().is_some_and(|task| task.is_finished()))
        {
            // The worker can finish between the first drain and this check.
            // Consume its final notification before dropping the receiver.
            self.poll_ready();
            if self.status == "正在开启共享…" {
                self.status = "共享启动失败".into();
                self.notice = "共享线程意外退出，请检查端口设置后重试".into();
                self.open = true;
            }
            self.worker = None;
            self.handle = None;
            self.ready = None;
            self.events = None;
            self.pending.clear();
            self.active_session = None;
            self.clipboard_state = None;
            self.stopping = false;
        }
        if self.running() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
    fn start(&mut self, data_dir: &Path, ctx: &egui::Context) -> anyhow::Result<()> {
        anyhow::ensure!(!self.running(), "共享已经开启");
        let monitor = self
            .monitors
            .get(self.monitor)
            .ok_or_else(|| anyhow::anyhow!("请选择可用屏幕"))?;
        anyhow::ensure!(
            monitor.encodable(self.encoder, self.fps),
            "此屏幕尺寸暂不支持，请选择其它屏幕"
        );
        let (ready, ready_rx) = mpsc::channel();
        let clipboard = self.clipboard.then(|| {
            let requests = ready.clone();
            let context = ctx.clone();
            Arc::new(move |id: &str| {
                let state = Arc::new(uu_core::clipboard::Clipboard::default());
                state.enabled.store(true, Ordering::Release);
                requests
                    .send(Ready::Clipboard {
                        id: id.to_owned(),
                        state: state.clone(),
                    })
                    .map_err(|_| anyhow::anyhow!("共享窗口已关闭"))?;
                context.request_repaint();
                Ok(state)
            }) as lan::ClipboardFactory
        });
        let config = lan::Config {codec:Default::default(),rate_control:Default::default(),
            name: self.name.trim().into(),
            bind: SocketAddr::from(([0, 0, 0, 0], self.port)),
            discovery_bind: Some(SocketAddr::from((
                [0, 0, 0, 0],
                self.discovery_port,
            ))),
            data_dir: data_dir.into(),
            source: native::CaptureSource::Monitor(monitor.name.clone()),
            fps: self.fps,
            bitrate_kbps: self.mbps * 1000,
            encoder: self.encoder,
            allow_input: self.input,
            audio: self.audio.then_some(native::AudioSource::System),
            clipboard,
        };
        let (stop, rx) = watch::channel(false);
        let (events, event_rx) = async_mpsc::unbounded_channel();
        let context = ctx.clone();
        let task = std::thread::Builder::new()
            .name("eclipse-lan-server".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready.send(Ready::Ended(Err(error.to_string())));
                        context.request_repaint();
                        return;
                    }
                };
                let result = runtime.block_on(async {
                    let (server, handle) = lan::Server::bind(config, rx, events).await?;
                    let address = server.address()?;
                    ready
                        .send(Ready::Listening(handle, address))
                        .map_err(|_| anyhow::anyhow!("共享窗口已关闭"))?;
                    context.request_repaint();
                    server.run().await
                });
                runtime.shutdown_timeout(Duration::from_secs(2));
                let _ = ready.send(Ready::Ended(result.map_err(|e| format!("{e:#}"))));
                context.request_repaint();
            })?;
        self.worker = Some(Worker {
            stop,
            task: Some(task),
        });
        self.ready = Some(ready_rx);
        self.events = Some(event_rx);
        self.notice.clear();
        self.status = "正在开启共享…".into();
        self.stopping = false;
        Ok(())
    }
    pub fn ui(&mut self, ctx: &egui::Context, data_dir: Option<&Path>, owner_window: isize) {
        self.owner_window = owner_window;
        if self.open {
            let mut open = true;
            let mut start = false;
            let mut stop = false;
            egui::Window::new("共享本机")
                .id("lan-host-settings".into())
                .open(&mut open)
                .collapsible(false)
                .default_width(480.)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().max_height((ctx.screen_rect().height()-120.0).max(160.0)).show(ui, |ui| {
                    if !self.notice.is_empty() {
                        ui.add(egui::Label::new(egui::RichText::new(&self.notice)
                            .color(egui::Color32::from_rgb(160,75,35))).wrap());
                        ui.add_space(10.0);
                    }
                    ui.label(egui::RichText::new(&self.status).strong());
                    ui.add_space(10.);
                    if self.running() {
                        ui.weak(format!("监听所有 IPv4 网卡：0.0.0.0:{}（TCP）",self.port));
                        ui.weak("控制端请填写下面的网卡地址；不同网段需路由与防火墙允许连接。");
                        for address in &self.addresses {
                            ui.horizontal(|ui| {
                                ui.label(address);
                                if ui.small_button("复制地址").clicked() {
                                    ctx.copy_text(address.clone());
                                }
                            });
                        }
                        if let Some(handle) = &self.handle {
                            if self.active_session.is_some() && ui.button("断开当前串流").clicked()
                            {
                                handle.stop_session();
                                self.status = "正在断开串流…".into();
                            }
                            ui.horizontal(|ui| {
                                if ui.button("允许新设备配对（2 分钟）").clicked() {
                                    handle.allow_pairing();
                                    self.notice =
                                        "请在另一台设备发起配对，并核对客户端显示的配对码。".into();
                                }
                                if ui.small_button("停止接收配对").clicked() {
                                    handle.close_pairing();
                                }
                            });
                            ui.add_space(8.);
                            ui.label("已授权设备");
                            let peers = handle.clients();
                            if peers.is_empty() {
                                ui.weak("尚无已配对设备");
                            }
                            for peer in peers {
                                ui.horizontal(|ui| {
                                    ui.label(&peer.name);
                                    if ui.small_button("撤销授权").clicked() {
                                        if let Err(error) = handle.revoke(&peer.id) {
                                            self.notice = error.to_string();
                                        }
                                    }
                                });
                            }
                        }
                        if let Some((_, state)) = &self.clipboard_state {
                            let status = if state.ready() {
                                "文本剪贴板已由双方启用".to_owned()
                            } else if state.available.load(Ordering::Acquire) {
                                "文本剪贴板等待控制端启用".to_owned()
                            } else {
                                state.status.lock().unwrap().clone()
                            };
                            if !status.is_empty() {
                                ui.weak(status);
                            }
                        }
                        ui.add_space(10.);
                        if ui
                            .add_enabled(!self.stopping, egui::Button::new("停止共享"))
                            .clicked()
                        {
                            stop = true;
                        }
                    } else {
                        egui::Grid::new("lan-host-fields")
                            .num_columns(2)
                            .spacing([18., 12.])
                            .show(ui, |ui| {
                                ui.label("设备名称");
                                ui.text_edit_singleline(&mut self.name);
                                ui.end_row();
                                ui.label("共享屏幕");
                                egui::ComboBox::from_id_salt("lan-monitor")
                                    .selected_text(
                                        self.monitors
                                            .get(self.monitor)
                                            .map(|m| {
                                                format!("{} · {}×{}", m.name, m.width, m.height)
                                            })
                                            .unwrap_or_else(|| "未找到可用屏幕".into()),
                                    )
                                    .show_ui(ui, |ui| {
                                        for (i, m) in self.monitors.iter().enumerate() {
                                            ui.selectable_value(
                                                &mut self.monitor,
                                                i,
                                                format!("{} · {}×{}", m.name, m.width, m.height),
                                            );
                                        }
                                    });
                                ui.end_row();
                                ui.label("编码加速");
                                egui::ComboBox::from_id_salt("lan-host-encoder")
                                    .selected_text(super::native_host_ui::preference_label(self.encoder))
                                    .show_ui(ui,|ui| {
                                        for preference in [native::EncoderPreference::Auto,native::EncoderPreference::Hardware,native::EncoderPreference::Software] {
                                            ui.selectable_value(&mut self.encoder,preference,super::native_host_ui::preference_label(preference));
                                        }
                                    });
                                ui.end_row();
                                ui.label("帧率");
                                ui.horizontal(|ui| {
                                    ui.selectable_value(&mut self.fps, 30, "30 FPS");
                                    ui.selectable_value(&mut self.fps, 60, "60 FPS");
                                });
                                ui.end_row();
                                ui.label("码率");
                                ui.add(
                                    egui::DragValue::new(&mut self.mbps)
                                        .range(1..=100)
                                        .suffix(" Mbps"),
                                );
                                ui.end_row();
                                ui.label("服务端口");
                                ui.add(egui::DragValue::new(&mut self.port).range(1..=65535));
                                ui.end_row();
                                ui.label("发现端口");
                                ui.add(egui::DragValue::new(&mut self.discovery_port).range(1..=65535));
                                ui.end_row();
                            });
                        ui.add_space(10.);
                        ui.weak("服务端口使用 TCP，发现端口使用 UDP；端口自动保存。更改发现端口时，请将控制端设为相同端口，也可用 IP:服务端口 手动连接。");
                        ui.checkbox(&mut self.input, "允许已配对设备控制键盘和鼠标");
                        ui.checkbox(&mut self.audio, "共享系统声音");
                        ui.checkbox(&mut self.clipboard, "允许双方启用文本剪贴板");
                        ui.weak("只有在你确认配对后，设备才能访问本机。关闭软件会停止共享。");
                        ui.add_space(10.);
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    data_dir.is_some() && !self.monitors.is_empty(),
                                    egui::Button::new("启用共享"),
                                )
                                .clicked()
                            {
                                start = true;
                            }
                            if ui.button("刷新屏幕").clicked() {
                                self.refresh_monitors();
                            }
                        });
                    }
                    });
                });
            self.open = open;
            if stop {
                self.stop();
            }
            if start {
                if let Some(dir) = data_dir {
                    if let Err(error) = self.start(dir, ctx) {
                        self.notice = format!("无法开启共享：{error}");
                    }
                }
            }
        }
        let mut decision = None;
        if let Some(pending) = self.pending.first_mut() {
            egui::Window::new("确认新设备配对")
                .id("lan-pair-host".into())
                .collapsible(false)
                .resizable(false)
                .default_width(420.)
                .show(ctx, |ui| {
                    ui.label(format!("{} 希望连接本机", pending.request.client_name));
                    ui.add_space(8.);
                    ui.label("请在发起连接的设备查看配对码，并在这里输入：");
                    ui.add(
                        egui::TextEdit::singleline(&mut pending.entered)
                            .hint_text("8 位配对码")
                            .char_limit(12),
                    );
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
                    let matches = entered == expected;
                    if entered.len() == 8 && !matches {
                        ui.colored_label(
                            egui::Color32::from_rgb(170, 55, 55),
                            "号码不匹配，请核对发起连接的设备。",
                        );
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(matches, egui::Button::new("允许配对"))
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
        if let Some(allowed) = decision {
            let pending = self.pending.remove(0);
            if pending.request.decision.send(allowed).is_err() {
                self.notice = "配对请求已结束，请从客户端重试。".into();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_session_does_not_open_a_queued_clipboard_backend() {
        let state = Arc::new(uu_core::clipboard::Clipboard::default());
        state.stopped.store(true, Ordering::Release);
        let (send, receiver) = mpsc::channel();
        send.send(Ready::Clipboard {
            id: "cancelled".into(),
            state: state.clone(),
        })
        .ok()
        .unwrap();
        let mut host = HostUi::default();
        host.ready = Some(receiver);
        host.poll(&egui::Context::default());
        assert!(host.clipboard_state.is_none());
        assert!(!state.available.load(Ordering::Acquire));
        assert!(state.status.lock().unwrap().is_empty());
    }
    #[test]
    fn an_old_session_ending_cannot_clear_a_new_sessions_progress() {
        let (events, rx) = async_mpsc::unbounded_channel();
        let mut host = HostUi::default();
        host.events = Some(rx);
        let ctx = egui::Context::default();
        events
            .send(lan::Event::SessionStarted {
                id: "old".into(),
                client: "generated".into(),
            })
            .ok()
            .unwrap();
        events
            .send(lan::Event::SessionStarted {
                id: "new".into(),
                client: "generated".into(),
            })
            .ok()
            .unwrap();
        events
            .send(lan::Event::SessionProgress {
                id: "new".into(),
                status: native::HostStatus::Streaming {
                    width: 1920,
                    height: 1080,
                    fps: 60,
                    audio: "disabled".into(),
                    encoder: "generated-backend".into(),
                },
            })
            .ok()
            .unwrap();
        events
            .send(lan::Event::SessionEnded("old".into()))
            .ok()
            .unwrap();
        events
            .send(lan::Event::SessionProgress {
                id: "old".into(),
                status: native::HostStatus::Preparing,
            })
            .ok()
            .unwrap();
        host.poll(&ctx);
        assert_eq!(host.active_session.as_deref(), Some("new"));
        assert!(host.status.contains("1920×1080 @ 60 FPS"));
        assert!(host.status.contains("generated-backend"));
        events
            .send(lan::Event::SessionEnded("new".into()))
            .ok()
            .unwrap();
        host.poll(&ctx);
        assert!(host.active_session.is_none());
        assert_eq!(host.status, "共享已开启，等待连接");
    }
}
