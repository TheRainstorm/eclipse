//! The internet entry changes rendezvous only; streaming uses the native UI.
use super::{
    sessions::{ConnectionTarget, StreamTarget},
    App,
};
use eframe::egui::{self, RichText};
use std::{
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::mpsc,
    thread::JoinHandle,
};
use tokio::sync::watch;
use uu_core::{
    internet::{Provider, Settings},
    native::{CaptureMonitor, CaptureSource, EncoderPreference, HostStatus, SharingConfig},
};

pub(super) fn read_profile(path: &Path) -> anyhow::Result<Settings> {
    anyhow::ensure!(
        std::fs::metadata(path)?.len() <= 64 * 1024,
        "连接配置文件过大"
    );
    serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|_| anyhow::anyhow!("连接配置文件格式错误，原文件已保留"))
}
struct Sharing {
    stop: watch::Sender<bool>,
    worker: Option<JoinHandle<()>>,
    status: watch::Receiver<HostStatus>,
    result: mpsc::Receiver<Result<(), String>>,
}
impl Drop for Sharing {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
pub(super) struct InternetUi {
    pub selected: bool,
    settings: Settings,
    key: String,
    token: String,
    ca: String,
    notice: String,
    monitors: Vec<CaptureMonitor>,
    monitor: usize,
    fps: u32,
    mbps: u32,
    encoder: EncoderPreference,
    input: bool,
    audio: bool,
    sharing: Option<Sharing>,
}
impl InternetUi {
    pub fn load(config: Option<&uu_core::Config>) -> Self {
        let mut notice = String::new();
        let mut settings = config
            .and_then(|c| {
                let path = c.at("internet/settings.json");
                if !path.exists() {
                    return None;
                }
                match read_profile(&path) {
                    Ok(value) => Some(value),
                    Err(error) => {
                        notice = error.to_string();
                        None
                    }
                }
            })
            .unwrap_or_default();
        if let Some(config) = config {
            if settings.access_key.as_os_str().is_empty() {
                settings.access_key = config.at("internet/access.key");
            }
        }
        let key = settings.access_key.display().to_string();
        let token = settings.server_token.display().to_string();
        let ca = settings
            .server_ca
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        Self {
            selected: false,
            settings,
            key,
            token,
            ca,
            notice,
            monitors: Vec::new(),
            monitor: 0,
            fps: 60,
            mbps: 18,
            encoder: EncoderPreference::Auto,
            input: true,
            audio: true,
            sharing: None,
        }
    }
    fn sync(&mut self) {
        self.settings.access_key = PathBuf::from(self.key.trim().trim_matches('"'));
        self.settings.server_token = PathBuf::from(self.token.trim().trim_matches('"'));
        self.settings.server_ca =
            (!self.ca.trim().is_empty()).then(|| PathBuf::from(self.ca.trim().trim_matches('"')));
        self.settings.device = self.settings.device.trim().to_owned();
    }
    fn save(&mut self, config: &uu_core::Config) -> anyhow::Result<PathBuf> {
        self.sync();
        let root = config.at("internet");
        std::fs::create_dir_all(&root)?;
        let data = serde_json::to_vec_pretty(&self.settings)?;
        std::fs::write(root.join("settings.json"), &data)?;
        // Each child receives an immutable snapshot so editing another target
        // cannot change a running window's reconnect destination.
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        data.hash(&mut hash);
        let path = root.join(format!("connection-{:016x}.json", hash.finish()));
        if !path.exists() {
            std::fs::write(&path, &data)?;
        }
        Ok(path)
    }
    pub fn running(&self) -> bool {
        self.sharing.is_some()
    }
    pub fn stop(&mut self) {
        if let Some(sharing) = &self.sharing {
            sharing.stop.send_replace(true);
        }
    }
    pub fn poll(&mut self, ctx: &egui::Context) {
        if self.running() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if self
            .sharing
            .as_ref()
            .is_some_and(|s| s.worker.as_ref().is_some_and(|w| w.is_finished()))
        {
            let sharing = self.sharing.take().unwrap();
            self.notice = match sharing.result.try_recv() {
                Ok(Ok(())) => "共享已结束".into(),
                Ok(Err(error)) => format!("共享失败：{error}"),
                Err(_) => "共享线程已结束".into(),
            };
        }
    }
    fn start(&mut self, config: uu_core::Config) -> anyhow::Result<()> {
        anyhow::ensure!(!self.running(), "共享正在进行");
        self.save(&config)?;
        let source = self
            .monitors
            .get(self.monitor)
            .map(|m| CaptureSource::Monitor(m.name.clone()))
            .unwrap_or(CaptureSource::Primary);
        let sharing = SharingConfig {
            source,
            fps: self.fps,
            bitrate_kbps: self.mbps * 1000,
            encoder: self.encoder,
            session_limit: None,
            allow_input: self.input,
            audio: self.audio.then_some(uu_core::native::AudioSource::System),
            clipboard: None,
        };
        let settings = self.settings.clone();
        uu_core::internet::AccessKey::load(&settings.access_key)?;
        let (stop, rx) = watch::channel(false);
        let (status, states) = watch::channel(HostStatus::Starting);
        let (tx, result) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("native-internet-host".into())
            .spawn(move || {
                let outcome = (|| -> anyhow::Result<()> {
                    let runtime = tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(2)
                        .enable_all()
                        .build()?;
                    let result = runtime.block_on(uu_core::internet::host(
                        settings,
                        config,
                        sharing,
                        rx,
                        Some(status),
                    ));
                    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
                    result?;
                    Ok(())
                })();
                let _ = tx.send(outcome.map_err(|e| format!("{e:#}")));
            })?;
        self.sharing = Some(Sharing {
            stop,
            worker: Some(worker),
            status: states,
            result,
        });
        self.notice.clear();
        Ok(())
    }
}
fn field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    ui.label(label);
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .desired_width(f32::INFINITY),
    );
    ui.add_space(8.);
}
impl App {
    pub(super) fn internet_home(&mut self, ctx: &egui::Context) {
        let config = self.config.clone();
        let logged_in = self.logged_in;
        let local_uu=config.as_ref().and_then(|c|uu_core::ClientState::read(&c.at("state.json")).ok().flatten()).and_then(|s|s.device_id);
        let mut connect = false;
        let mut share = false;
        let mut login = false;
        let page = &mut self.internet_ui;
        egui::CentralPanel::default().frame(egui::Frame::none().fill(super::theme::CANVAS).inner_margin(24.)).show(ctx,|ui|{
            egui::ScrollArea::vertical().show(ui,|ui|{
                ui.heading("通过连接服务使用原生串流");ui.weak("找到对端后，共用 IP 直连的画面、声音和键鼠操作。");ui.add_space(18.);
                egui::Frame::none().fill(egui::Color32::WHITE).rounding(12.).inner_margin(20.).show(ui,|ui|{
                    ui.horizontal(|ui|{
                        ui.selectable_value(&mut page.settings.provider,Provider::SelfHosted,"自建服务器");
                        ui.selectable_value(&mut page.settings.provider,Provider::Uu,"UU 账号（兼容）");
                    });ui.add_space(14.);
                    if page.settings.provider==Provider::SelfHosted {
                        field(ui,"服务器 IP 和端口",&mut page.settings.server,"server.example:8443");
                        field(ui,"服务器访问凭据文件",&mut page.token,"server.token");
                        field(ui,"服务器证书（自签名时填写）",&mut page.ca,"server.pem；正式证书可留空");
                    } else {
                        ui.horizontal(|ui|{ui.label(if logged_in {"已使用当前 UU 登录账号"} else {"需要先登录 UU 账号"});if !logged_in&&ui.button("登录账号").clicked(){login=true;}});
                        ui.weak("连接双方均运行本项目原生模式；UU 仅负责设备寻址与建连。");ui.add_space(8.);
                        if let Some(id)=&local_uu {ui.horizontal(|ui|{ui.label(format!("本机 UU 编号：{id}"));if ui.small_button("复制").clicked(){ui.output_mut(|o|o.copied_text=id.clone());}});}
                    }
                    field(ui,if page.settings.provider==Provider::Uu {"目标 UU 设备编号（观看时填写）"}else{"设备名称（两端填写相同名称）"},&mut page.settings.device,"my-desktop");
                    ui.separator();ui.label(RichText::new("设备配对").strong());ui.weak("首次使用创建配对文件，通过可信方式复制到另一台设备，两端选择同一文件。");
                    field(ui,"配对文件",&mut page.key,"access.key");
                    if ui.button("创建新的配对文件").clicked(){
                        page.sync();let result=(||->anyhow::Result<()>{
                            if let Some(parent)=page.settings.access_key.parent().filter(|p|!p.as_os_str().is_empty()){std::fs::create_dir_all(parent)?;}
                            uu_core::internet::AccessKey::create(&page.settings.access_key)
                        })();page.notice=match result{Ok(())=>"配对文件已创建，请复制到另一台设备。".into(),Err(e)=>format!("创建失败：{e}")};
                    }
                    ui.add_space(12.);ui.horizontal(|ui|{
                        ui.add_enabled_ui(!page.running(),|ui|{
                            if ui.button(RichText::new("连接并打开画面").strong()).clicked(){connect=true;}
                        });
                    });
                });ui.add_space(14.);
                egui::Frame::none().fill(egui::Color32::WHITE).rounding(12.).inner_margin(20.).show(ui,|ui|{
                    ui.heading("共享本机");
                    ui.add_enabled_ui(!page.running(),|ui|{
                        if ui.button("刷新显示器").clicked(){match uu_core::native::monitors(){Ok(monitors)=>{page.monitors=monitors;page.monitor=0;},Err(e)=>page.notice=e.to_string()}}
                        egui::ComboBox::from_id_salt("internet-monitor").selected_text(page.monitors.get(page.monitor).map(|m|format!("{} · {}×{}",m.name,m.width,m.height)).unwrap_or_else(||"主显示器".into())).show_ui(ui,|ui|{
                            for (i,m) in page.monitors.iter().enumerate(){ui.selectable_value(&mut page.monitor,i,format!("{} · {}×{}",m.name,m.width,m.height));}
                        });
                        ui.horizontal(|ui|{ui.label("帧率");ui.add(egui::DragValue::new(&mut page.fps).range(1..=60));ui.label("码率 Mbps");ui.add(egui::DragValue::new(&mut page.mbps).range(1..=100));});
                        egui::ComboBox::from_id_salt("internet-encoder").selected_text(super::native_host_ui::preference_label(page.encoder)).show_ui(ui,|ui|{
                            for value in [EncoderPreference::Auto,EncoderPreference::Hardware,EncoderPreference::Software]{ui.selectable_value(&mut page.encoder,value,super::native_host_ui::preference_label(value));}
                        });ui.checkbox(&mut page.input,"允许键鼠控制");ui.checkbox(&mut page.audio,"共享系统声音");
                    });
                    if let Some(sharing)=&page.sharing {
                        let label=match &*sharing.status.borrow(){HostStatus::Streaming{width,height,fps,..}=>format!("正在串流 · {width}×{height} · {fps} FPS"),_=>"正在连接服务或等待对端…".into()};ui.label(label);
                        if ui.button("停止共享").clicked(){page.stop();}
                    }else if ui.button("开始共享").clicked(){share=true;}
                });ui.add_space(12.);
                egui::CollapsingHeader::new("开发选项").show(ui,|ui|{
                    ui.checkbox(&mut page.settings.development_relay,"允许开发中继测试（UU TURN/UDP）");
                    ui.weak("默认关闭；开启后可能使用中继，日志记录实际选中路径。");
                });
                if !page.notice.is_empty(){ui.add_space(12.);ui.label(&page.notice);}
            });
        });
        if login {
            self.start_login();
        }
        if connect || share {
            let result = (|| -> anyhow::Result<()> {
                let config = config.ok_or_else(|| anyhow::anyhow!("配置目录不可用"))?;
                if self.internet_ui.settings.provider == Provider::Uu {
                    anyhow::ensure!(logged_in, "请先登录 UU 账号");
                }
                if share {
                    self.internet_ui.start(config)?;
                } else {
                    let profile = self.internet_ui.save(&config)?;
                    let target = StreamTarget {
                        connection: ConnectionTarget::NativeInternet(profile),
                        alias: self.internet_ui.settings.device.clone(),
                        view_only: false,
                    };
                    self.sessions.open(
                        &std::env::current_exe()?,
                        &target,
                        self.platform.origin(),
                    )?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                self.internet_ui.notice = format!("无法开始：{error:#}");
            }
        }
    }
}
