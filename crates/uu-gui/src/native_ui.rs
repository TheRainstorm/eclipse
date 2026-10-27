//! Native invitation entry and session attachment, independent of an account login.
use super::{
    sessions::{ConnectionTarget, StreamTarget},
    App,
};
use eframe::egui;
use std::{path::PathBuf, sync::Arc};
use uu_core::capabilities::Capability;

pub(super) struct NativeUi {
    pub open: bool,
    path: String,
    notice: String,
    request_control:bool,
}
impl Default for NativeUi {
    fn default()->Self{Self{open:false,path:String::new(),notice:String::new(),request_control:true}}
}
impl App {
    pub(super) fn is_native(&self) -> bool {
        self.stream_target
            .as_ref()
            .is_some_and(StreamTarget::is_native)
    }
    pub(super) fn supports_remote(&self, capability: Capability) -> bool {
        self.controller
            .as_ref()
            .map(|h| h.stats.capabilities.allows(capability))
            .unwrap_or(!self.is_native())
    }
    pub(super) fn native_input_available(&self)->bool {
        self.is_native()&&self.controller.as_ref().is_some_and(|h|h.stats.native_input_available.load(std::sync::atomic::Ordering::Acquire))
    }
    /// Local input controls remain configurable while a menu, lost focus or
    /// view-only policy temporarily relinquishes the remote control grant.
    pub(super) fn input_controls_available(&self) -> bool {
        self.supports_remote(Capability::Input) || self.native_input_available()
    }
    pub(super) fn send_native(&mut self,command:uu_core::native_input::InputCommand) {
        use uu_core::native_input::InputCommand as I;
        let release=matches!(command,I::Key{pressed:false,..}|I::Button{pressed:false,..}|I::ReleaseAll{});
        if (!self.input_ready()||self.view_only)&&!release{return;}
        let failed=self.controller.as_ref().and_then(|h|h.native_input.as_ref()).is_some_and(|sender|sender.send(command).is_err());
        if failed {
            if let Some(h)=&self.controller{h.stats.input_send_errors.fetch_add(1,std::sync::atomic::Ordering::Relaxed);}
            self.input_capture.release();self.pressed_vks.clear();self.mod_state=[false;4];
            let screen=self.input_screen(self.selected_track);let _=self.pointer.release_all(screen);
        }
    }
    pub(super) fn open_native(&mut self) {
        self.native_host.open = false;
        self.login_rx = None;
        self.login_texture = None;
        self.native_ui.open = true;
        self.native_ui.notice.clear();
    }
    pub(super) fn native_ui(&mut self, ctx: &egui::Context) {
        if self.stream_target.is_none() {
            let dropped = ctx.input(|input| {
                input
                    .raw
                    .dropped_files
                    .iter()
                    .find_map(|file| file.path.clone())
            });
            if let Some(path) = dropped {
                if path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with(".eclipse-invite.json"))
                {
                    self.native_ui.path = path.display().to_string();
                    self.open_native();
                }
            }
        }
        if !self.native_ui.open {
            return;
        }
        let mut open = true;
        let mut connect = false;
        let mut share = false;
        egui::Window::new("原生直连")
            .id("native-connect".into())
            .frame(egui::Frame::window(&ctx.style()).fill(egui::Color32::WHITE).rounding(10.0).inner_margin(14.0))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(440.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                if ui.button("共享此设备 →").clicked(){ share = true; }
                ui.add_space(8.0);
                ui.label("使用主机生成的邀请，不需要登录账号。");
                ui.add_space(10.0);
                ui.label("将邀请文件拖入窗口，或填写文件路径：");
                let edited = ui.add_sized(
                    [420.0, 30.0],
                    egui::TextEdit::singleline(&mut self.native_ui.path)
                        .hint_text("session.eclipse-invite.json"),
                );
                if edited.changed() {
                    self.native_ui.notice.clear();
                }
                ui.add_space(8.0);
                ui.checkbox(&mut self.native_ui.request_control,"请求输入控制（需主机允许）");
                ui.weak("未获主机许可时仅观看；画质由主机设置。");
                if !self.native_ui.notice.is_empty() {
                    ui.colored_label(egui::Color32::from_rgb(180, 75, 65), &self.native_ui.notice);
                }
                ui.add_space(12.0);
                if ui
                    .add_enabled(
                        !self.native_ui.path.trim().is_empty(),
                        egui::Button::new(egui::RichText::new("打开共享画面").color(egui::Color32::WHITE))
                            .fill(egui::Color32::from_rgb(53,131,255)).min_size(egui::vec2(140.0, 32.0)),
                    )
                    .clicked()
                {
                    connect = true;
                }
            });
        self.native_ui.open = open;
        if share { self.open_native_host(); return; }
        if connect {
            self.open_native_window();
        }
    }
    fn open_native_window(&mut self) {
        let path = PathBuf::from(self.native_ui.path.trim().trim_matches('"'));
        let result = (|| -> anyhow::Result<bool> {
            let invitation = uu_core::native_pairing::Invitation::load(&path)?;
            drop(invitation);
            let target = StreamTarget {
                connection: ConnectionTarget::NativeInvitation(path.canonicalize()?),
                alias: "原生直连".into(),
                view_only: !self.native_ui.request_control,
            };
            Ok(self
                .sessions
                .open(&std::env::current_exe()?, &target, self.platform.origin())?)
        })();
        match result {
            Ok(opened) => {
                self.native_ui.open = false;
                self.status = if opened {
                    "已打开原生串流窗口"
                } else {
                    "原生串流窗口已打开"
                }
                .into();
            }
            Err(error) => self.native_ui.notice = format!("无法使用邀请：{error}"),
        }
    }
    pub(super) fn connect_native(&mut self, path: PathBuf) {
        self.connect_native_source(Some(path),None,None,None);
    }
    pub(super) fn connect_native_device(&mut self,id:String){self.connect_native_source(None,Some(id),None,None);}
    pub(super) fn connect_native_internet(&mut self,path:PathBuf){self.connect_native_source(None,None,Some(path),None);}
    pub(super) fn connect_native_profile(&mut self,path:PathBuf){self.connect_native_source(None,None,None,Some(path));}
    fn connect_native_source(&mut self,path:Option<PathBuf>,device:Option<String>,internet:Option<PathBuf>,profile:Option<PathBuf>){
        self.remote_admin_ui=Default::default();
        let stats = Arc::new(uu_core::media::MediaStats::default());
        stats.pairing.enable_choices();
        let kind=self.decoder_kind();
        let _=stats.decoder_preference.set(kind);
        let _=stats.native_quality_request.set(self.preferences.native_request());
        let _=stats.native_rate_request.set(uu_core::native_quality::RateRequest{selection:self.preferences.rate_control});
        let _=stats.native_hdr_requested.set(self.preferences.hdr);
        #[cfg(windows)]
        if self.preferences.hdr && kind!=uu_core::media::DecoderKind::Software && self.preferences.codec!=super::settings::CodecPreference::H264 {
            if let Some(gl)=self.video_gl.clone() {
                match super::video_hdr::enable(&self.context,gl,self.preferences.vsync) {
                    Ok(capable)=>stats.native_hdr_capable.store(capable,std::sync::atomic::Ordering::Release),
                    Err(error)=>stats.diagnostics.record(serde_json::json!({"event":"hdr_presentation_unavailable","reason":error})),
                }
            }
        }
        if kind==uu_core::media::DecoderKind::Software {let _=stats.native_codec_capabilities.set(vec![uu_core::native::VideoCodec::H264]);}
        stats.audio.volume.store(self.preferences.volume,std::sync::atomic::Ordering::Release);
        stats.audio.muted.store(self.preferences.muted,std::sync::atomic::Ordering::Release);
        if let Some(config) = &self.config {
            stats.diagnostics.start([config.state_dir.join("diagnostics")]);
        }
        stats.diagnostics.record(
            serde_json::json!({"event":"client","transport":"native","build":self.build_label}),
        );
        stats
            .clipboard
            .enabled
            .store(false, std::sync::atomic::Ordering::Release);
        stats
            .clipboard
            .available
            .store(cfg!(any(windows,target_os="linux")), std::sync::atomic::Ordering::Release);
        self.displays = super::displays::Displays::default();
        self.selected_track = 0;
        let result=if let Some(path)=profile {
            (||->anyhow::Result<_>{
                let config=self.config.clone().ok_or_else(||anyhow::anyhow!("无法读取设备配置目录"))?;
                let launch=uu_core::device_book::Launch::read(&path)?;
                uu_core::native_controller::spawn_profile(launch,config,stats,!self.view_only)
            })()
        }else if let Some(profile)=internet {
            (|| -> anyhow::Result<_> {
                let config=self.config.clone().ok_or_else(||anyhow::anyhow!("无法读取连接配置目录"))?;
                let settings=super::internet_ui::read_profile(&profile)?;
                uu_core::native_controller::spawn_internet(settings,config,stats,!self.view_only)
            })()
        }else if let Some(id)=device {
            self.config.as_ref().ok_or_else(||anyhow::anyhow!("无法确定原生设备配置目录")).and_then(|config|uu_core::native_controller::spawn_device(config.at("native-lan/paired-devices.json"),id,stats,!self.view_only))
        }else{uu_core::native_controller::spawn_with_input(path.unwrap(),stats,!self.view_only)};
        match result {
            Ok(handle) => {
                let context = self.context.clone();
                handle.frames.enable_planar();
                handle.frames.set_waker(move || context.request_repaint());
                if let Err(error)=super::clipboard::start_following(handle.stats.clipboard.clone(),self.clipboard_window.clone()){handle.stats.clipboard.available.store(false,std::sync::atomic::Ordering::Release);handle.stats.clipboard.status(format!("剪贴板不可用：{error}"));}
                self.controller = Some(handle);
                self.window_sizing.borrow_mut().begin_stream();
                self.input_capture.request();
                if self.active_session && !self.fullscreen {
                    self.context
                        .send_viewport_cmd(egui::ViewportCommand::Maximized(false));
                }
                self.status = "正在建立原生串流…".into();
            }
            Err(error) => self.status = format!("原生连接失败：{error}"),
        }
    }
}
