//! Explicit, foreground host sharing. No service, account or display mutation.
use super::App;
use eframe::egui;
use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    thread::JoinHandle,
    time::Duration,
};
use tokio::sync::watch;
use uu_core::native::{CaptureMonitor, CaptureSource, HostConfig, HostReport, HostStatus};

struct Sharing {
    stop: watch::Sender<bool>,
    status: watch::Receiver<HostStatus>,
    worker: Option<JoinHandle<Result<HostReport, String>>>,
    stopping: bool,
}
impl Sharing {
    fn stop(&mut self) {
        self.stopping = true;
        self.stop.send_replace(true);
    }
}
impl Drop for Sharing {
    fn drop(&mut self) {
        self.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(super) struct NativeHostUi {
    pub open: bool,
    bind: String,
    monitors: Vec<CaptureMonitor>,
    monitor: Option<String>,
    fps: u32,
    bitrate_kbps: u32,
    allow_input: bool,
    invitation: String,
    notice: String,
    sharing: Option<Sharing>,
}
impl Default for NativeHostUi {
    fn default() -> Self {
        Self {
            open: false,
            bind: "127.0.0.1".into(),
            monitors: Vec::new(),
            monitor: None,
            fps: 60,
            bitrate_kbps: 8000,
            allow_input: false,
            invitation: String::new(),
            notice: String::new(),
            sharing: None,
        }
    }
}
impl NativeHostUi {
    pub fn running(&self) -> bool {
        self.sharing.is_some()
    }
    pub fn stop(&mut self) {
        if let Some(sharing) = &mut self.sharing {
            sharing.stop();
        }
    }
    pub fn poll(&mut self, ctx: &egui::Context) {
        if self.running() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if !self
            .sharing
            .as_ref()
            .is_some_and(|s| s.worker.as_ref().is_some_and(|w| w.is_finished()))
        {
            return;
        }
        let mut sharing = self.sharing.take().unwrap();
        self.notice = match sharing.worker.take().unwrap().join() {
            Ok(Ok(report)) => format!(
                "共享已结束 · 已发送 {} 帧 · 输入错误 {}",
                report.sent, report.input_errors
            ),
            Ok(Err(error)) => format!("共享已停止：{error}"),
            Err(_) => "共享线程意外结束".into(),
        };
        ctx.request_repaint();
    }
    fn refresh(&mut self) {
        match uu_core::native::monitors() {
            Ok(monitors) => {
                if self
                    .monitor
                    .as_ref()
                    .is_some_and(|name| !monitors.iter().any(|m| &m.name == name))
                {
                    self.monitor = None;
                }
                self.monitors = monitors;
                self.notice.clear();
            }
            Err(error) => {
                self.monitors.clear();
                self.notice = format!("无法访问显示器：{error}");
            }
        }
    }
    fn start(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.running(), "共享正在进行");
        anyhow::ensure!(!self.monitors.is_empty(), "请先获取可用显示器");
        let ip: IpAddr = self
            .bind
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("请填写本机网卡 IP 地址"))?;
        anyhow::ensure!(
            !ip.is_unspecified() && !ip.is_multicast(),
            "请选择本机的具体网卡地址"
        );
        let invitation_file = PathBuf::from(self.invitation.trim().trim_matches('"'));
        anyhow::ensure!(
            invitation_file.file_name().is_some(),
            "请选择新的邀请文件路径"
        );
        anyhow::ensure!(
            !invitation_file.exists(),
            "邀请文件已存在，请更换路径；不会覆盖已有文件"
        );
        if let Some(parent) = invitation_file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
        {
            let mut dirs = std::fs::DirBuilder::new();
            dirs.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                dirs.mode(0o700);
            }
            dirs.create(parent)?;
        }
        let config = HostConfig {
            bind: SocketAddr::new(ip, 0),
            source: self
                .monitor
                .clone()
                .map(CaptureSource::Monitor)
                .unwrap_or(CaptureSource::Primary),
            invitation_file,
            invitation_lifetime: Duration::from_secs(600),
            fps: self.fps,
            bitrate_kbps: self.bitrate_kbps,
            session_limit: None,
            allow_input: self.allow_input,
        };
        let (stop, rx) = watch::channel(false);
        let (status, states) = watch::channel(HostStatus::Starting);
        let worker = std::thread::Builder::new()
            .name("native-host".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())?;
                runtime
                    .block_on(uu_core::native::serve_with_status(config, rx, Some(status)))
                    .map_err(|e| format!("{e:#}"))
            })?;
        self.sharing = Some(Sharing {
            stop,
            status: states,
            worker: Some(worker),
            stopping: false,
        });
        self.notice.clear();
        Ok(())
    }
}
impl App {
    pub(super) fn open_native_host(&mut self) {
        self.native_ui.open = false;
        self.native_host.open = true;
        if !self.native_host.running() {
            self.native_host.refresh();
        }
        if self.native_host.invitation.is_empty() {
            let directory = self
                .config
                .as_ref()
                .map(|c| c.at("native-invitations"))
                .unwrap_or_else(|| std::env::temp_dir().join("uu-client-invitations"));
            self.native_host.invitation = directory
                .join(format!("{}.eclipse-invite.json", uuid::Uuid::new_v4()))
                .display()
                .to_string();
        }
    }
    pub(super) fn native_host_ui(&mut self, ctx: &egui::Context) {
        if !self.native_host.open {
            return;
        }
        let mut open = true;
        let host = &mut self.native_host;
        egui::Window::new("共享此设备")
            .id("native-host".into())
            .open(&mut open)
            .collapsible(false)
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(egui::Color32::WHITE)
                    .rounding(10.0)
                    .inner_margin(14.0),
            )
            .resizable(false)
            .default_width(480.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("通过原生邀请共享当前桌面，无需账号。");
                ui.weak("H.264 软件编码 · 当前共享不包含系统音频");
                ui.add_space(10.0);
                if let Some(sharing) = &mut host.sharing {
                    let status = sharing.status.borrow().clone();
                    let text = if sharing.stopping {
                        "正在停止共享并释放键鼠…".into()
                    } else {
                        match status {
                            HostStatus::Starting => "正在启动共享…".into(),
                            HostStatus::Waiting {
                                address,
                                expires_at,
                            } => {
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs();
                                format!(
                                    "等待连接 · {address} · 邀请剩余 {} 秒",
                                    expires_at.saturating_sub(now)
                                )
                            }
                            HostStatus::Preparing => "正在建立加密串流…".into(),
                            HostStatus::Streaming { width, height, fps } => {
                                format!("正在共享 · {width}×{height} @ {fps} FPS")
                            }
                        }
                    };
                    ui.label(egui::RichText::new(text).strong());
                    ui.add_space(8.0);
                    ui.label(if host.allow_input {
                        "本次共享允许受邀端请求键鼠控制"
                    } else {
                        "本次共享仅允许观看"
                    });
                    ui.label("将邀请文件私下发给要连接的设备：");
                    ui.add(
                        egui::TextEdit::singleline(&mut host.invitation)
                            .desired_width(460.0)
                            .interactive(false),
                    );
                    if ui.button("复制邀请路径").clicked() {
                        ui.output_mut(|o| o.copied_text = host.invitation.clone());
                    }
                    ui.add_space(12.0);
                    if ui
                        .add_enabled(
                            !sharing.stopping,
                            egui::Button::new(
                                egui::RichText::new("停止共享")
                                    .color(egui::Color32::from_rgb(175, 65, 60)),
                            )
                            .fill(egui::Color32::from_rgb(255, 241, 239))
                            .min_size(egui::vec2(130.0, 32.0)),
                        )
                        .clicked()
                    {
                        sharing.stop();
                    }
                    ctx.request_repaint_after(Duration::from_millis(100));
                } else {
                    ui.horizontal(|ui| {
                        ui.label("显示器");
                        egui::ComboBox::from_id_salt("native-host-monitor")
                            .selected_text(host.monitor.as_deref().unwrap_or("主显示器"))
                            .width(300.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut host.monitor, None, "主显示器");
                                for m in &host.monitors {
                                    ui.selectable_value(
                                        &mut host.monitor,
                                        Some(m.name.clone()),
                                        format!(
                                            "{} · {}×{}{}",
                                            m.name,
                                            m.width,
                                            m.height,
                                            if m.primary { " · 主屏" } else { "" }
                                        ),
                                    );
                                }
                            });
                        if ui.button("刷新").clicked() {
                            host.refresh();
                        }
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label("本机网卡 IP");
                        ui.add(egui::TextEdit::singleline(&mut host.bind).desired_width(250.0));
                    });
                    ui.weak("跨设备连接时填写本机局域网地址；127.0.0.1 仅限本机。");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label("帧率");
                        ui.add(
                            egui::DragValue::new(&mut host.fps)
                                .range(1..=60)
                                .suffix(" FPS"),
                        );
                        ui.add_space(16.0);
                        ui.label("码率");
                        ui.add(
                            egui::DragValue::new(&mut host.bitrate_kbps)
                                .range(64..=100_000)
                                .speed(500)
                                .suffix(" kbps"),
                        );
                    });
                    ui.add_space(8.0);
                    ui.checkbox(&mut host.allow_input, "允许受邀端请求键盘和鼠标控制");
                    ui.add_space(8.0);
                    ui.label("新邀请文件（10 分钟内可连接）");
                    ui.add(egui::TextEdit::singleline(&mut host.invitation).desired_width(460.0));
                    ui.weak("停止或关闭共享后邀请失效。每次只接受一个连接。");
                    ui.add_space(12.0);
                    if ui
                        .add_enabled(
                            !host.monitors.is_empty(),
                            egui::Button::new(
                                egui::RichText::new("开始共享").color(egui::Color32::WHITE),
                            )
                            .fill(egui::Color32::from_rgb(53, 131, 255))
                            .min_size(egui::vec2(130.0, 32.0)),
                        )
                        .clicked()
                    {
                        if let Err(error) = host.start() {
                            host.notice = format!("无法启动共享：{error}");
                        }
                    }
                }
                if !host.notice.is_empty() {
                    ui.add_space(10.0);
                    ui.label(&host.notice);
                }
            });
        host.open = open;
    }
}
