//! Explicit, foreground host sharing. No service, account or display mutation.
use super::App;
use eframe::egui;
use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{atomic::Ordering, Arc},
    thread::JoinHandle,
    time::Duration,
};
use tokio::sync::watch;
use uu_core::native::{CaptureMonitor, CaptureSource, EncoderPreference, HostConfig, HostReport, HostStatus};

pub(super) fn preference_label(preference:EncoderPreference)->&'static str {
    match preference {
        EncoderPreference::Software=>"软件编码（兼容）",
        EncoderPreference::Auto=>"自动（优先硬件）",
        EncoderPreference::Hardware=>"硬件编码（必需）",
        EncoderPreference::Vaapi=>"VA-API（Linux）",
    }
}
fn actual_encoder_label(backend:&str)->&'static str {
    if backend.starts_with("vaapi ") {"硬件编码 · VA-API"}
    else if backend.starts_with("mf ") {"硬件编码 · Media Foundation"}
    else if backend.contains("fallback")||backend.contains("unavailable") {"软件编码 · 硬件不可用，已回退"}
    else {"软件编码 · H.264"}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sharing_keeps_software_default_and_reports_actual_fallback() {
        let mut host=NativeHostUi::default();assert_eq!(host.encoder,EncoderPreference::Software);
        assert_eq!(host.bind,"127.0.0.1");
        host.monitors.push(CaptureMonitor {name:"generated".into(),x:0,y:0,width:4096,height:2160,primary:true});
        assert!(!host.source_size_supported());host.encoder=EncoderPreference::Auto;
        assert!(host.source_size_supported());host.fps=61;assert!(!host.source_size_supported());
        assert_eq!(actual_encoder_label("vaapi (generated)"),"硬件编码 · VA-API");
        assert_eq!(actual_encoder_label("mf (H.264 hardware, generated Intel encoder)"),"硬件编码 · Media Foundation");
        assert!(actual_encoder_label("openh264 (software) · fallback from VA-API").contains("已回退"));
        assert_eq!(actual_encoder_label("openh264 (software)"),"软件编码 · H.264");
    }
    #[test]
    fn refreshing_interfaces_preserves_explicit_address_and_does_not_start_sharing() {
        let mut host=NativeHostUi::default();host.bind="192.0.2.55".into();
        host.refresh_addresses();
        assert_eq!(host.bind,"192.0.2.55");
        assert!(!host.running());
    }
}

struct Sharing {
    stop: watch::Sender<bool>,
    status: watch::Receiver<HostStatus>,
    worker: Option<JoinHandle<Result<HostReport, String>>>,
    stopping: bool,
    clipboard: Option<Arc<uu_core::clipboard::Clipboard>>,
}
impl Sharing {
    fn stop(&mut self) {
        self.stopping = true;
        if let Some(clipboard) = &self.clipboard {
            clipboard.stopped.store(true, Ordering::Release);
            clipboard.active.store(false, Ordering::Release);
        }
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
    addresses: Vec<super::native_interfaces::Address>,
    address_error: Option<String>,
    monitors: Vec<CaptureMonitor>,
    monitor: Option<String>,
    fps: u32,
    bitrate_kbps: u32,
    encoder: EncoderPreference,
    allow_input: bool,
    share_audio: bool,
    share_clipboard: bool,
    invitation: String,
    notice: String,
    sharing: Option<Sharing>,
}
impl Default for NativeHostUi {
    fn default() -> Self {
        Self {
            open: false,
            bind: "127.0.0.1".into(),
            addresses: Vec::new(),
            address_error: None,
            monitors: Vec::new(),
            monitor: None,
            fps: 60,
            bitrate_kbps: 8000,
            encoder: EncoderPreference::Software,
            allow_input: false,
            share_audio: false,
            share_clipboard: false,
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
        self.refresh_addresses();
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
    fn refresh_addresses(&mut self) {
        match super::native_interfaces::enumerate() {
            Ok(addresses) => {self.addresses=addresses;self.address_error=None;},
            Err(error) => {self.addresses.clear();self.address_error=Some(format!("无法读取本机地址：{error}；仍可手动填写。"));},
        }
        // Refresh never changes the selected bind address or starts a listener.
    }
    fn source_size_supported(&self) -> bool {
        self.monitors
            .iter()
            .find(|m| {
                self.monitor
                    .as_ref()
                    .map_or(m.primary, |name| name == &m.name)
            })
            .map_or(true, |monitor|monitor.encodable(self.encoder,self.fps))
    }
    fn start(&mut self, owner_window: isize) -> anyhow::Result<()> {
        anyhow::ensure!(!self.running(), "共享正在进行");
        anyhow::ensure!(!self.monitors.is_empty(), "请先获取可用显示器");
        anyhow::ensure!(
            self.source_size_supported(),
            "当前显示器超出所选编码器的尺寸范围，请调整编码方式或选择其它显示器"
        );
        let ip: IpAddr = self
            .bind
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("请填写本机网卡 IP 地址"))?;
        anyhow::ensure!(
            !ip.is_unspecified() && !ip.is_multicast(),
            "请选择本机的具体网卡地址"
        );
        anyhow::ensure!(
            !matches!(ip,IpAddr::V6(ip) if ip.is_unicast_link_local()),
            "暂不支持需要接口作用域的 IPv6 链路本地地址，请选择本机 IPv4 或其它 IPv6 地址"
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
        let clipboard = self.share_clipboard.then(|| {
            let state = Arc::new(uu_core::clipboard::Clipboard::default());
            state.enabled.store(true, Ordering::Release);
            state
        });
        let config = HostConfig {codec:Default::default(),rate_control:Default::default(),
            clipboard: clipboard.clone(),
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
            encoder: self.encoder,
            session_limit: None,
            allow_input: self.allow_input,
            audio: self
                .share_audio
                .then_some(uu_core::native::AudioSource::System),
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
            clipboard: clipboard.clone(),
        });
        if let Some(clipboard) = clipboard {
            if let Err(error) = super::clipboard::start_host(clipboard, owner_window) {
                self.stop();
                return Err(error.into());
            }
        }
        self.notice.clear();
        Ok(())
    }
}
impl App {
    pub(super) fn open_native_host(&mut self) {
        self.native_ui.open = false;
        self.settings_open=false;
        if self.stream_target.is_none() {
            self.lan.compatibility=false;
            self.open_settings();
            self.settings_section=super::settings_page::Section::Server;
        }else {
            let result=std::env::current_exe().and_then(|exe|std::process::Command::new(exe).env_remove("UU_AUTOCONNECT").env("ECLIPSE_SHOW_SHARING","1").spawn());
            self.status=match result {Ok(_)=>"已打开主页，可在应用设置中管理服务端".into(),Err(e)=>format!("无法打开主页：{e}")};
        }
    }

    pub(super) fn native_host_ui(&mut self, ctx: &egui::Context) {
        if !self.native_host.open {
            return;
        }
        let mut open = true;
        let owner_window = self.platform.window_id();
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
            .max_width((ctx.screen_rect().width()-48.0).max(260.0))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height((ctx.screen_rect().height()-120.0).max(120.0)).show(ui,|ui| {
                ui.label("通过原生邀请共享当前桌面，无需账号。");
                ui.weak("H.264 视频 · 系统声音由本次共享选择");
                #[cfg(target_os="linux")]
                if std::env::var("XDG_SESSION_TYPE").is_ok_and(|session|session.eq_ignore_ascii_case("wayland")) {
                    ui.weak("Wayland 完整桌面采集尚未接入；当前共享只能使用可访问的 X11 显示源。");
                }
                ui.add_space(10.0);
                if let Some(sharing) = &mut host.sharing {
                    let status = sharing.status.borrow().clone();
                    let backend=match &status {HostStatus::Streaming {encoder,..}=>Some(encoder.clone()),_=>None};
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
                            HostStatus::Streaming {
                                width,
                                height,
                                fps,
                                audio,
                                encoder: _,
                            } => {
                                format!("正在共享 · {width}×{height} · 目标 {fps} FPS  {audio}")
                            }
                        }
                    };
                    ui.label(egui::RichText::new(text).strong());
                    if let Some(backend)=backend {
                        ui.label(actual_encoder_label(&backend));
                        ui.collapsing("编码详情",|ui|{ui.add(egui::Label::new(backend).wrap());});
                    }
                    ui.add_space(8.0);
                    ui.label(if host.allow_input {
                        "本次共享允许受邀端请求输入控制"
                    } else {
                        "键鼠：仅观看"
                    });
                    if let Some(clipboard) = &sharing.clipboard {
                        ui.label(if clipboard.active.load(Ordering::Acquire) {
                            "文本剪贴板：同步中"
                        } else {
                            "文本剪贴板：已允许，等待控制端开启"
                        });
                    }
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
                        ui.label("编码");
                        egui::ComboBox::from_id_salt("native-host-encoder").selected_text(preference_label(host.encoder))
                            .show_ui(ui,|ui| {
                                ui.selectable_value(&mut host.encoder,EncoderPreference::Software,preference_label(EncoderPreference::Software));
                                if uu_core::native::hardware_encoding_compiled() {
                                    for preference in [EncoderPreference::Auto,EncoderPreference::Hardware] {
                                        ui.selectable_value(&mut host.encoder,preference,preference_label(preference));
                                    }
                                }
                            });
                    });
                    if host.encoder==EncoderPreference::Auto {ui.weak("硬件不可用或运行失败时回退软件，开始后显示实际编码方式。");}
                    else if matches!(host.encoder,EncoderPreference::Hardware|EncoderPreference::Vaapi) {ui.weak("驱动不支持时停止并显示原因。当前仍有CPU颜色转换和上传。");}
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
                    if !host.source_size_supported() {
                        ui.colored_label(
                            egui::Color32::from_rgb(180, 75, 65),
                            "当前源尺寸超出所选编码器支持范围，请调整编码方式或选择其它显示器。",
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label("连接地址");
                        let selected=host.addresses.iter().find(|address|address.ip.to_string()==host.bind.trim())
                            .map(|address|format!("{} · {}",address.name,address.ip))
                            .unwrap_or_else(||if host.bind.trim()=="127.0.0.1" {"仅本机（默认）".into()}else{"手动地址".into()});
                        egui::ComboBox::from_id_salt("native-host-address").selected_text(selected.as_str()).width(250.0).truncate()
                            .show_ui(ui,|ui| {
                                ui.selectable_value(&mut host.bind,"127.0.0.1".into(),"仅本机（默认）");
                                for address in &host.addresses {
                                    ui.selectable_value(&mut host.bind,address.ip.to_string(),format!("{} · {}",address.name,address.ip));
                                }
                            }).response.on_hover_text(selected);
                        if ui.button("刷新地址").clicked() {host.refresh_addresses();}
                    });
                    ui.horizontal(|ui| {
                        ui.label("本机网卡 IP");
                        ui.add(egui::TextEdit::singleline(&mut host.bind).desired_width(250.0));
                    });
                    ui.weak("跨设备连接时填写本机局域网地址；127.0.0.1 仅限本机。");
                    if let Some(error)=&host.address_error {ui.colored_label(egui::Color32::from_rgb(180,75,65),error);}
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
                    ui.checkbox(&mut host.allow_input, "允许受邀端请求键盘、鼠标和手柄控制");
                    ui.checkbox(&mut host.share_audio, "共享系统声音（输出设备）");
                    ui.checkbox(
                        &mut host.share_clipboard,
                        "允许文本剪贴板同步（控制端需主动开启）",
                    );
                    ui.add_space(8.0);
                    ui.label("新邀请文件（10 分钟内可连接）");
                    ui.add(egui::TextEdit::singleline(&mut host.invitation).desired_width(460.0));
                    ui.weak("停止或关闭共享后邀请失效。每次只接受一个连接。");
                    ui.add_space(12.0);
                    if ui
                        .add_enabled(
                            !host.monitors.is_empty() && host.source_size_supported(),
                            egui::Button::new(
                                egui::RichText::new("开始共享").color(egui::Color32::WHITE),
                            )
                            .fill(egui::Color32::from_rgb(53, 131, 255))
                            .min_size(egui::vec2(130.0, 32.0)),
                        )
                        .clicked()
                    {
                        if let Err(error) = host.start(owner_window) {
                            host.notice = format!("无法启动共享：{error}");
                        }
                    }
                }
                if !host.notice.is_empty() {
                    ui.add_space(10.0);
                    ui.label(&host.notice);
                }
                });
            });
        host.open = open;
    }
}
