//! Account-independent local devices, manual addresses, pairing and launch.
use super::{
    sessions::{ConnectionTarget, StreamTarget},
    ui::{self, Icon},
    App,
};
use eframe::egui::{self, Color32 as C, RichText};
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    path::PathBuf,
    sync::mpsc,
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc as async_mpsc, watch};
use uu_core::{
    native_discovery as discovery, native_lan as lan,
    native_lan_store::{Device, Devices},
};
const BLUE: C = super::theme::ACCENT;
const MUTED: C = super::theme::MUTED;
enum Event {
    Scan(u64, Result<Vec<discovery::Device>, String>),
    Code(String),
    Paired(Result<Device, String>),
    Verified(Result<Device, String>),
}
enum Work {
    Scan(u64, Vec<SocketAddr>),
    Pair { endpoint: String, client_id: String },
    Verify { device: Device, endpoint: String },
}
struct Job {
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}
fn spawn(ctx: &egui::Context, events: mpsc::Sender<Event>, work: Work) -> Result<Job, String> {
    let (stop, mut cancel) = watch::channel(false);
    let ctx = ctx.clone();
    let task = std::thread::Builder::new()
        .name("eclipse-lan-client".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(error) => {
                    let error=uu_i18n::format!("无法启动连接线程：{error}");
                    let event=match &work {
                        Work::Scan(epoch,_)=>Event::Scan(*epoch,Err(error)),
                        Work::Pair {..}=>Event::Paired(Err(error)),
                        Work::Verify {..}=>Event::Verified(Err(error)),
                    };
                    let _ = events.send(event);
                    ctx.request_repaint();
                    return;
                }
            };
            runtime.block_on(async {
                match work {
                    Work::Scan(epoch, targets) => {
                        let result = tokio::select! {
                            _ = cancel.changed() => return,
                            result = discovery::discover(&targets, Duration::from_millis(1200)) => result,
                        };
                        let _ = events.send(Event::Scan(epoch, result.map_err(|e| format!("{e:#}"))));
                    }
                    Work::Pair { endpoint, client_id } => {
                        let (progress, mut codes) = async_mpsc::unbounded_channel();
                        let pairing = lan::pair(&endpoint, &client_id, "Eclipse client", progress);
                        tokio::pin!(pairing);
                        let result = loop {
                            tokio::select! {
                                _ = cancel.changed() => break Err(anyhow::anyhow!("已取消配对")),
                                Some(code) = codes.recv() => {
                                    let _ = events.send(Event::Code(code));
                                    ctx.request_repaint();
                                }
                                result = &mut pairing => break result,
                            }
                        };
                        let _ = events.send(Event::Paired(result.map_err(|e| format!("{e:#}"))));
                    }
                    Work::Verify { device, endpoint } => {
                        let result = tokio::select! {
                            _ = cancel.changed() => return,
                            result = lan::verify_address(&device, &endpoint) => result,
                        };
                        let _ = events.send(Event::Verified(result.map_err(|e| format!("{e:#}"))));
                    }
                }
            });
            runtime.shutdown_timeout(Duration::from_secs(1));
            ctx.request_repaint();
        }).map_err(|e| e.to_string())?;
    Ok(Job {
        stop,
        task: Some(task),
    })
}
fn targets(port: u16) -> Result<Vec<SocketAddr>, String> {
    if let Ok(value) = std::env::var("ECLIPSE_DISCOVERY_TARGETS") {
        let addresses: Result<Vec<_>, _> = value.split(',').map(str::parse::<SocketAddr>).collect();
        let addresses = addresses.map_err(|_| uu_i18n::text("局域网发现地址配置无效").to_owned())?;
        if addresses.is_empty() || addresses.len() > 32 {
            return Err(uu_i18n::text("局域网发现地址数量无效").into());
        }
        return Ok(addresses);
    }
    let mut values = HashSet::new();
    values.insert(SocketAddr::from((
        [255, 255, 255, 255],
        port,
    )));
    if let Ok(interfaces) = if_addrs::get_if_addrs() {
        for interface in interfaces {
            if interface.is_loopback() {
                continue;
            }
            if let if_addrs::IfAddr::V4(ip) = interface.addr {
                if let Some(ip) = ip.broadcast {
                    values.insert(SocketAddr::new(ip.into(), port));
                }
            }
        }
    }
    Ok(values.into_iter().take(32).collect())
}
struct Seen {
    device: discovery::Device,
    at: Instant,
}
#[derive(Clone)]
enum RowKind {
    Paired(Device),
    New,
    Manual,
}
#[derive(Clone)]
struct Row {
    key: String,
    name: String,
    endpoint: String,
    detail: String,
    kind: RowKind,
    changed: bool,
}
pub(super) struct LanUi {
    pub host: super::lan_host_ui::HostUi,
    dir: Option<PathBuf>,
    book: Option<Devices>,
    events: mpsc::Receiver<Event>,
    send: mpsc::Sender<Event>,
    found: HashMap<(String, String), Seen>,
    scan: Option<Job>,
    retired_scans: Vec<Job>,
    scan_epoch: u64,
    discovery_enabled: bool,
    client: Option<Job>,
    last_scan: Option<Instant>,
    discovery_port: u16,
    notice: String,
    add_open: bool,
    address: String,
    pair_code: Option<String>,
    pair_target: String,
    client_dialog: bool,
    client_error: Option<String>,
    client_started: Option<Instant>,
    open_device: Option<Device>,
}
impl LanUi {
    pub fn nearby(&self,local:Option<&str>)->Vec<(String,String)> {
        let mut devices:Vec<_>=self.found.values().filter(|s|s.at.elapsed()<Duration::from_secs(20) && local!=Some(s.device.info.id.as_str()))
            .map(|s|(s.device.info.name.clone(),s.device.address.to_string())).collect();
        devices.sort();devices.dedup();devices
    }
    pub fn configure_ports(&mut self, service: u16, discovery: u16) {
        self.host.configure_ports(service, discovery);
        if self.discovery_port != discovery {
            self.discovery_port = discovery;
            self.reset_scan();
        }
    }
    pub fn set_discovery(&mut self, enabled: bool) {
        if self.discovery_enabled != enabled {
            self.discovery_enabled = enabled;
            self.reset_scan();
        }
    }
    fn reset_scan(&mut self) {
        self.scan_epoch = self.scan_epoch.wrapping_add(1);
        if let Some(job) = self.scan.take() {
            job.stop.send_replace(true);
            // Cancellation closes the UDP socket on its worker. Join only after
            // completion, so changing a preference never blocks the UI thread.
            self.retired_scans.push(job);
        }
        self.last_scan = None;
        self.found.clear();
    }
    pub fn new(dir: Option<PathBuf>) -> Self {
        let (send, events) = mpsc::channel();
        let (book, notice) = if let Some(dir) = &dir {
            match Devices::open(&dir.join("paired-devices.json")) {
                Ok(book) => (Some(book), String::new()),
                Err(error) => (None, uu_i18n::format!("无法读取原生设备配置，原文件已保留：{error}")),
            }
        } else {
            (None, uu_i18n::text("无法确定设备配置目录，请检查本机用户目录。").into())
        };
        Self {
            host: super::lan_host_ui::HostUi::default(),
            dir,
            book,
            events,
            send,
            found: HashMap::new(),
            scan: None,
            retired_scans: Vec::new(),
            scan_epoch: 0,
            discovery_enabled: true,
            client: None,
            last_scan: None,
            discovery_port: discovery::DISCOVERY_PORT,
            notice,
            add_open: false,
            address: String::new(),
            pair_code: None,
            pair_target: String::new(),
            client_dialog: false,
            client_error: None,
            client_started: None,
            open_device: None,
        }
    }
    fn path(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join("paired-devices.json"))
    }
    fn scan(&mut self, ctx: &egui::Context) {
        if !self.discovery_enabled || self.scan.is_some() {
            return;
        }
        self.last_scan = Some(Instant::now());
        self.scan = match targets(self.discovery_port)
            .and_then(|targets| spawn(ctx, self.send.clone(), Work::Scan(self.scan_epoch, targets)))
        {
            Ok(job) => Some(job),
            Err(error) => {
                self.notice = error;
                None
            }
        };
    }
    pub fn poll(&mut self, ctx: &egui::Context, active: bool) {
        self.host.poll(ctx);
        self.retired_scans.retain(|job| !job.task.as_ref().is_none_or(|task| task.is_finished()));
        // Join a finished worker before draining: all final events are now
        // queued, even if it completed between repaint and this poll.
        if self
            .scan
            .as_ref()
            .is_some_and(|job| job.task.as_ref().is_some_and(|task| task.is_finished()))
        {
            self.scan = None;
        }
        let client_finished=self.client.as_ref()
            .is_some_and(|job|job.task.as_ref().is_some_and(|task|task.is_finished()));
        if client_finished {self.client=None;}
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Scan(epoch, _) if !self.discovery_enabled || epoch != self.scan_epoch => continue,
                Event::Scan(_, result) => match result {
                    Ok(devices) => {
                        for device in devices {
                            self.found.insert(
                                (device.info.id.clone(), device.info.fingerprint.clone()),
                                Seen {
                                    device,
                                    at: Instant::now(),
                                },
                            );
                        }
                        self.found
                            .retain(|_, seen| seen.at.elapsed() < Duration::from_secs(20));
                    }
                    Err(error) => self.notice = uu_i18n::format!("局域网发现失败：{error}"),
                },
                Event::Code(code) => {
                    if self.client_dialog {self.pair_code = Some(code);}
                },
                Event::Paired(result) | Event::Verified(result) => {
                    self.pair_code = None;
                    if !self.client_dialog {continue;}
                    match result {
                        Ok(device) => {
                            self.client_dialog=false;
                            if let (Some(path), Some(book)) = (self.path(), &mut self.book) {
                                match book.remember(&path, device.clone()) {
                                    Ok(()) => {
                                        self.notice = uu_i18n::text("设备已就绪，正在打开串流窗口…").into();
                                        self.open_device = Some(device);
                                    }
                                    Err(error) => {
                                        let message=uu_i18n::format!("无法保存设备授权：{error}");
                                        self.notice=message.clone();
                                        self.client_error=Some(message);
                                        self.client_dialog=true;
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            self.notice = uu_i18n::format!("连接 {} 未完成：{error}",self.pair_target);
                            self.client_error=Some(error);
                        },
                    }
                }
            }
        }
        if client_finished && self.client_dialog && self.client_error.is_none() {
            let error=uu_i18n::text("连接线程意外退出，请重试连接。").to_owned();
            self.notice=error.clone();
            self.client_error=Some(error);
            self.pair_code=None;
        }
        if active && self.discovery_enabled
            && self
                .last_scan
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(8))
        {
            self.scan(ctx);
        }
        if self.client.is_some() || self.scan.is_some() || !self.retired_scans.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else if active && self.discovery_enabled {
            let next = self
                .last_scan
                .map(|at| Duration::from_secs(8).saturating_sub(at.elapsed()))
                .unwrap_or(Duration::ZERO);
            ctx.request_repaint_after(next);
        }
    }
    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut paired = HashSet::new();
        let mut endpoints = HashSet::new();
        if let Some(book) = &self.book {
            for device in &book.devices {
                paired.insert(device.id.clone());
                endpoints.insert(device.endpoint.clone());
                let found = self.found.get(&(device.id.clone(), device.fingerprint()));
                let changed =
                    found.is_none() && self.found.values().any(|s| s.device.info.id == device.id);
                let endpoint = found
                    .map(|s| s.device.address.to_string())
                    .unwrap_or_else(|| device.endpoint.clone());
                endpoints.insert(endpoint.clone());
                rows.push(Row {
                    key: device.id.clone(),
                    name: device.name.clone(),
                    endpoint,
                    detail: if changed {
                        uu_i18n::text("设备身份已变化，请核实后重新配对")
                    } else if found.is_some() {
                        uu_i18n::text("已配对 · 在线")
                    } else {
                        uu_i18n::text("已配对 · 未在局域网发现，仍可尝试连接")
                    }
                    .into(),
                    kind: RowKind::Paired(device.clone()),
                    changed,
                });
            }
        }
        for seen in self.found.values() {
            let device = &seen.device;
            if paired.contains(&device.info.id) {
                continue;
            }
            let endpoint = device.address.to_string();
            endpoints.insert(endpoint.clone());
            rows.push(Row {
                key: device.info.id.clone(),
                name: device.info.name.clone(),
                endpoint,
                detail: uu_i18n::text("局域网设备 · 尚未配对").into(),
                kind: RowKind::New,
                changed: false,
            });
        }
        if let Some(book) = &self.book {
            for endpoint in &book.manual {
                if endpoints.contains(endpoint) {
                    continue;
                }
                rows.push(Row {
                    key: endpoint.clone(),
                    name: uu_i18n::text("手动添加的设备").into(),
                    endpoint: endpoint.clone(),
                    detail: uu_i18n::text("尚未配对 · 连接时检查设备").into(),
                    kind: RowKind::Manual,
                    changed: false,
                });
            }
        }
        rows.sort_by(|a, b| {
            matches!(b.kind, RowKind::Paired(_))
                .cmp(&matches!(a.kind, RowKind::Paired(_)))
                .then(a.name.cmp(&b.name))
        });
        rows
    }
    fn connect(&mut self, row: Row, ctx: &egui::Context) {
        if self.client.is_some() {
            return;
        }
        self.notice.clear();
        self.client_error=None;
        self.pair_code=None;
        self.client_started=Some(Instant::now());
        self.client_dialog=true;
        ctx.request_repaint();
        self.pair_target = format!("{} · {}", row.name, row.endpoint);
        match row.kind {
            RowKind::Paired(device) => {
                if row.changed {
                    self.client_dialog=false;
                    self.notice =
                        uu_i18n::text("设备身份已变化，请先核实目标设备；确认后可忘记旧配对并重新连接。").into();
                    return;
                }
                if device.endpoint == row.endpoint {
                    self.client_dialog=false;
                    self.open_device = Some(device);
                    return;
                }
                self.client = match spawn(
                    ctx,
                    self.send.clone(),
                    Work::Verify {
                        device,
                        endpoint: row.endpoint,
                    },
                ) {
                    Ok(job) => Some(job),
                    Err(error) => {
                        self.notice = error.clone();
                        self.client_error=Some(error);
                        None
                    }
                };
            }
            _ => {
                let Some(book) = &self.book else {
                    return;
                };
                self.pair_code = None;
                self.client = match spawn(
                    ctx,
                    self.send.clone(),
                    Work::Pair {
                        endpoint: row.endpoint,
                        client_id: book.client_id.clone(),
                    },
                ) {
                    Ok(job) => Some(job),
                    Err(error) => {
                        self.notice = error.clone();
                        self.client_error=Some(error);
                        None
                    }
                };
            }
        }
    }
    fn forget(&mut self, row: &Row) {
        if let (Some(path), Some(book)) = (self.path(), &mut self.book) {
            let result = match &row.kind {
                RowKind::Paired(device) => book.forget(&path, &device.id),
                RowKind::Manual => book.remove_endpoint(&path, &row.endpoint),
                RowKind::New => {
                    self.found.retain(|(id, _), _| id != &row.key);
                    Ok(())
                }
            };
            if let Err(error) = result {
                self.notice = error.to_string();
            }
        }
    }
    fn add(&mut self) {
        if let (Some(path), Some(book)) = (self.path(), &mut self.book) {
            match book.add_endpoint(&path, &self.address) {
                Ok(()) => {
                    self.add_open = false;
                    self.address.clear();
                    self.notice = uu_i18n::text("设备已添加，点击配对即可连接。").into();
                }
                Err(error) => self.notice = uu_i18n::format!("无法添加设备：{error}"),
            }
        }
    }
    fn ui(&mut self, ctx: &egui::Context) {
        let mut connect = None;
        let mut forget = None;
        let rows = self.rows();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(C::from_rgb(245, 248, 252))
                    .inner_margin(24.),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(uu_i18n::text("局域网设备")).size(25.).strong());
                        ui.label(
                            RichText::new(uu_i18n::text("Eclipse · 无需账号，直接连接你的设备")).color(MUTED),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui::icon_label_button(ui, Icon::Desktop, uu_i18n::text("共享本机")).clicked() {
                            self.host.show();
                        }
                        if ui::icon_label_button(ui, Icon::Link, uu_i18n::text("添加设备")).clicked() {
                            self.add_open = true;
                        }
                        if ui::icon_button(ui, Icon::Refresh, uu_i18n::text("刷新局域网设备")).clicked() {
                            self.scan(ctx);
                        }
                    });
                });
                ui.add_space(18.);
                if !self.notice.is_empty() {
                    egui::Frame::none()
                        .fill(C::from_rgb(237, 243, 253))
                        .rounding(8.)
                        .inner_margin(12.)
                        .show(ui, |ui| {
                            ui.label(&self.notice);
                        });
                    ui.add_space(12.);
                }
                if !self.discovery_enabled || self.scan.is_some() {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(14.));
                        ui.weak(uu_i18n::text("正在查找局域网设备…"));
                    });
                    ui.add_space(8.);
                }
                if rows.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(52.);
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(54., 54.), egui::Sense::hover());
                        ui::icon(ui.painter(), rect, Icon::Desktop, BLUE);
                        ui.add_space(16.);
                        ui.label(RichText::new(uu_i18n::text("还没有发现设备")).size(19.));
                        ui.add_space(8.);
                        ui.label(
                            RichText::new(uu_i18n::text("在目标电脑启用共享，或输入 IP 添加设备。")).color(MUTED),
                        );
                        ui.add_space(8.);
                        ui.label(
                            RichText::new(uu_i18n::text("原账号设备请切换顶部“兼容设备”，登录状态与设置仍保留。"))
                                .size(12.)
                                .color(MUTED),
                        );
                        ui.add_space(16.);
                        if ui.button(uu_i18n::text("添加设备")).clicked() {
                            self.add_open = true;
                        }
                    });
                }
                let row_width = ui.available_width();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for row in &rows {
                            egui::Frame::none()
                                .fill(C::WHITE)
                                .rounding(10.)
                                .stroke(egui::Stroke::new(1., C::from_rgb(226, 232, 240)))
                                .inner_margin(16.)
                                .show(ui, |ui| {
                                    ui.set_min_width((row_width - 32.).max(0.));
                                    ui.horizontal(|ui| {
                                        let width =
                                            (row_width - 32. - 125. - ui.spacing().item_spacing.x)
                                                .max(160.);
                                        ui.allocate_ui_with_layout(
                                            egui::vec2(width, 72.),
                                            egui::Layout::left_to_right(egui::Align::Center),
                                            |ui| {
                                                ui.set_min_width(width);
                                                let (rect, _) = ui.allocate_exact_size(
                                                    egui::vec2(42., 42.),
                                                    egui::Sense::hover(),
                                                );
                                                ui.painter().rect_filled(
                                                    rect,
                                                    9.,
                                                    C::from_rgb(236, 244, 255),
                                                );
                                                ui::icon(
                                                    ui.painter(),
                                                    rect.shrink(10.),
                                                    Icon::Desktop,
                                                    BLUE,
                                                );
                                                ui.add_space(10.);
                                                ui.vertical(|ui| {
                                                    ui.add(
                                                        egui::Label::new(
                                                            RichText::new(&row.name)
                                                                .size(17.)
                                                                .strong(),
                                                        )
                                                        .truncate(),
                                                    );
                                                    ui.label(
                                                        RichText::new(&row.endpoint)
                                                            .size(12.)
                                                            .color(MUTED),
                                                    );
                                                    ui.add(
                                                        egui::Label::new(
                                                            RichText::new(&row.detail)
                                                                .size(12.)
                                                                .color(if row.changed {
                                                                    C::from_rgb(170, 60, 50)
                                                                } else {
                                                                    MUTED
                                                                }),
                                                        )
                                                        .truncate(),
                                                    );
                                                });
                                            },
                                        );
                                        ui.vertical(|ui| {
                                            let label = if matches!(row.kind, RowKind::Paired(_)) {
                                                uu_i18n::text("连接")
                                            } else {
                                                uu_i18n::text("配对")
                                            };
                                            if ui
                                                .add_enabled(
                                                    self.client.is_none()
                                                        && self.book.is_some()
                                                        && !row.changed,
                                                    egui::Button::new(
                                                        RichText::new(label).color(C::WHITE),
                                                    )
                                                    .fill(BLUE)
                                                    .min_size(egui::vec2(90., 32.)),
                                                )
                                                .clicked()
                                            {
                                                connect = Some(row.clone());
                                            }
                                            ui.menu_button(uu_i18n::text("更多"), |ui| {
                                                if ui
                                                    .button(
                                                        if matches!(row.kind, RowKind::Paired(_)) {
                                                            uu_i18n::text("忘记配对")
                                                        } else {
                                                            uu_i18n::text("移除设备")
                                                        },
                                                    )
                                                    .clicked()
                                                {
                                                    forget = Some(row.clone());
                                                    ui.close_menu();
                                                }
                                            });
                                        });
                                    });
                                });
                            ui.add_space(10.);
                        }
                    });
            });
        if let Some(row) = connect {
            self.connect(row, ctx);
        }
        if let Some(row) = forget {
            self.forget(&row);
        }
    }
    pub fn dialogs(&mut self, ctx: &egui::Context, owner_window: isize) {
        if self.add_open {
            let mut open = true;
            let mut add = false;
            egui::Window::new(uu_i18n::text("添加局域网设备"))
                .id("lan-add-device".into())
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(380.)
                .show(ctx, |ui| {
                    ui.label(uu_i18n::text("输入目标电脑的 IP 地址或主机名"));
                    ui.weak(uu_i18n::text("先在目标电脑启用共享并允许新设备配对。"));
                    ui.weak(uu_i18n::text("未填写端口时使用 TCP 48998；其它端口请填写 IP:端口。"));
                    ui.weak(uu_i18n::text("不同网段可手动连接，需路由及防火墙允许互通。"));
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.address)
                            .hint_text(uu_i18n::text("IP 或 主机名[:端口]"))
                            .desired_width(350.),
                    );
                    if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        add = true;
                    }
                    ui.add_space(10.);
                    if ui
                        .add_enabled(self.book.is_some(), egui::Button::new(uu_i18n::text("添加设备")))
                        .clicked()
                    {
                        add = true;
                    }
                });
            self.add_open = open;
            if add {
                self.add();
            }
        }
        if self.client_dialog && (self.client.is_some() || self.client_error.is_some()) {
            let mut cancel = false;
            egui::Window::new(uu_i18n::text("连接设备"))
                .id("lan-client-progress".into())
                .collapsible(false)
                .resizable(false)
                .default_width(390.)
                .show(ctx, |ui| {
                    ui.label(&self.pair_target);
                    if let Some(error) = &self.client_error {
                        ui.colored_label(C::from_rgb(170,60,50),uu_i18n::text("连接未完成"));
                        ui.add(egui::Label::new(error).wrap());
                        ui.add_space(8.);
                        ui.label(uu_i18n::text("请确认目标已启用共享、服务端口正确，并允许防火墙中的 TCP 入站连接。"));
                        ui.weak(uu_i18n::text("跨网段请使用手动地址，广播发现通常不跨路由器。"));
                    } else if let Some(code) = &self.pair_code {
                        ui.add_space(12.);
                        ui.label(RichText::new(code).size(34.).strong().color(BLUE));
                        ui.add_space(12.);
                        ui.label(uu_i18n::text("请在目标电脑输入此配对码并允许连接。"));
                        ui.weak(uu_i18n::text("等待主机确认，最长 90 秒；无需在本机再次输入。"));
                    } else {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            let elapsed=self.client_started.map(|at|at.elapsed().as_secs()).unwrap_or(0);
                            ui.label(uu_i18n::format!("正在检查设备并建立安全连接… {elapsed} 秒"));
                        });
                    }
                    ui.add_space(12.);
                    if ui.button(if self.client_error.is_some() {uu_i18n::text("关闭")} else {uu_i18n::text("取消")}).clicked() {
                        cancel = true;
                    }
                });
            if cancel {
                if let Some(job) = &self.client {
                    job.stop.send_replace(true);
                }
                self.pair_code = None;
                self.client_error=None;
                self.client_dialog=false;
                self.notice=uu_i18n::text("已关闭连接窗口，可点击设备重试。").into();
            }
        }
        self.host.ui(ctx, self.dir.as_deref(), owner_window);
    }
}
impl App {
    pub(super) fn lan_home(&mut self, ctx: &egui::Context) {
        self.lan.ui(ctx);
        if let Some(device) = self.lan.open_device.take() {
            let target = StreamTarget {
                connection: ConnectionTarget::NativeDevice(device.id.clone()),
                alias: device.name.clone(),
                view_only: false,
            };
            match std::env::current_exe()
                .and_then(|exe| self.sessions.open(&exe, &target, self.platform.origin()))
            {
                Ok(true) => self.lan.notice = uu_i18n::text("已打开原生串流窗口").into(),
                Ok(false) => self.lan.notice = uu_i18n::text("此设备的串流窗口已经打开").into(),
                Err(error) => self.lan.notice = uu_i18n::format!("无法打开串流窗口：{error}"),
            }
        }
    }
}

#[cfg(test)]
#[path = "lan_ui_tests.rs"]
mod tests;
