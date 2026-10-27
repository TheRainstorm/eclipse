//! uu-client remote desktop client.
//!
//! Layout mirrors the official client: a device sidebar on the left, a large
//! remote-screen area on the right, and a status/footer bar.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::sync::mpsc::{channel, Receiver};

use eframe::egui;
use serde::Deserialize;
use uu_core::controller::{ControllerEvent, ControllerHandle};

mod input;
mod displays;
mod display_layout;
mod clipboard;
mod cursor;
mod keyboard_capture;
mod input_capture;
mod settings;
mod app_startup;
mod settings_page;
mod theme;
mod sessions;
mod single_instance;
mod platform;
mod shortcuts;
mod hud;
mod hud_panel;
mod video_texture;
#[cfg(windows)]
mod wgl_interop;
#[cfg(windows)]
mod video_hdr;
mod window;
mod window_context;
mod stream_workspace;
mod stream_menu;
mod login;
mod ui;
mod brand;
mod build_info;
mod account;
mod uu_devices;
mod audio_controls;
mod native_ui;
mod gamepad;
mod internet_ui;
mod workspace_ui;
mod ui_feedback;
mod sharing_ui;
#[cfg(windows)]
mod service_ui;
mod server_config_ui;
mod remote_admin_ui;
mod pairing_ui;
mod connection_details;
mod lan_ui;
mod lan_host_ui;
mod native_keys;
mod native_host_ui;
mod native_interfaces;
mod wallpapers;
mod background;
mod device_preview;
mod device_library;
mod device_presence;
use login::LoginEvent;

#[derive(Clone, Debug, Deserialize)]
struct Device {
    alias: String,
    device_id: String,
    status: String,
    #[serde(default)]
    controllable: bool,
    #[serde(default)]
    wallpaper_url: String,
    #[serde(default)]
    #[allow(dead_code)]
    platform: i64,
}

#[derive(Debug, Deserialize)]
struct DeviceListData {
    #[serde(default)]
    my_binded_devices: Vec<Device>,
}

#[derive(Debug, Deserialize)]
struct DeviceListResp {
    code: i64,
    #[serde(default)]
    data: Option<DeviceListData>,
}

fn parse_devices(body: serde_json::Value) -> anyhow::Result<Vec<Device>> {
    let resp: DeviceListResp = serde_json::from_value(body)?;
    anyhow::ensure!(resp.code == 0, "获取设备列表失败，服务返回代码 {}", resp.code);
    Ok(resp.data.map(|d| d.my_binded_devices).unwrap_or_default())
}

#[cfg(test)]
mod launcher_tests {
    #[test]
    fn device_fetch_rejects_a_changed_account_before_requesting_the_list() {
        let root = std::env::temp_dir().join(format!("uu-list-account-{}", uuid::Uuid::new_v4()));
        let config = uu_core::Config {
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            signaling_host: "unreachable.example.invalid".into(),
        };
        let mut state = uu_core::ClientState::new();
        state.token = Some("new-account".into());
        state.save(&config.at("state.json")).unwrap();
        let before = std::fs::read(config.at("state.json")).unwrap();
        let error = super::load_devices(config.clone(), "previous-account".into()).unwrap_err();
        assert!(error.to_string().contains("账号已变化"), "{error:#}");
        assert_eq!(std::fs::read(config.at("state.json")).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_or_rejected_responses_are_not_treated_as_an_empty_account() {
        assert!(super::parse_devices(serde_json::json!({})).is_err());
        assert!(super::parse_devices(serde_json::json!({"code":1120,"data":{"my_binded_devices":[]}})).is_err());
        assert!(super::parse_devices(serde_json::json!({"code":0,"data":{"my_binded_devices":[]}})).unwrap().is_empty());
    }

    #[test]
    fn light_theme_keeps_touch_targets_when_started_from_either_theme() {
        for theme in [eframe::egui::Theme::Dark, eframe::egui::Theme::Light] {
            let context = eframe::egui::Context::default();
            context.set_theme(theme);
            super::setup_style(&context);
            let mut height = 0.0;
            let _ = context.run(Default::default(), |context| {
                eframe::egui::CentralPanel::default().show(context, |ui| {
                    height = ui.button("Retry").rect.height();
                });
            });
            assert_eq!(context.theme(), eframe::egui::Theme::Light);
            assert!(height >= 30.0, "custom button target was lost after theme selection");
        }
    }
}

fn load_devices(config: uu_core::Config, token: String) -> anyhow::Result<Vec<Device>> {
    let rt = tokio::runtime::Runtime::new()?;
    let result = rt.block_on(async {
        let client = uu_core::SignalClient::new(&config)?;
        let api = uu_core::UuApi::new(client, config.at("state.json"))?;
        anyhow::ensure!(api.state.token.as_deref() == Some(token.as_str()), "账号已变化，请刷新设备列表");
        let body = tokio::time::timeout(std::time::Duration::from_secs(15), api.device_list())
            .await.map_err(|_| anyhow::anyhow!("获取设备列表超时，请检查网络后重试"))??;
        let mut devices = parse_devices(body)?;
        // Opt-in privacy for screen sharing/documentation; no account data is changed.
        if std::env::var("UU_SCREENSHOT_PRIVACY").as_deref() == Ok("1") {
            for (index, device) in devices.iter_mut().enumerate() {
                device.alias = format!("远程设备 {:02}", index + 1);
                device.wallpaper_url.clear();
            }
        }
        Ok(devices)
    });
    rt.shutdown_background();
    result
}

struct App {
    instance_id:u64,
    stream_target: Option<sessions::StreamTarget>,
    workspace_tabs:Option<stream_workspace::Tabs>,
    active_session:bool,
    assistance:Option<uu_core::assistance::Request>,
    native_ui: native_ui::NativeUi,
    gamepad: std::rc::Rc<std::cell::RefCell<gamepad::Reader>>,
    workspace:workspace_ui::Workspace,
    connection_details_open:bool,
    remote_admin_ui:remote_admin_ui::RemoteAdminUi,
    pairing_ui:pairing_ui::ClientUi,
    lan: lan_ui::LanUi,
    clipboard_window:std::sync::Arc<std::sync::atomic::AtomicIsize>,
    native_host: native_host_ui::NativeHostUi,
    sessions: sessions::Sessions,
    platform: std::rc::Rc<platform::Platform>,
    build_label: String,
    executable_path: String,
    context: window_context::Context,
    devices: Vec<Device>,
    device_load_error: Option<String>,
    device_library: device_library::Library,
    device_filter: device_library::Filter,
    selected: Option<usize>,
    search: String,
    status: String,
    rx: Option<Receiver<anyhow::Result<Vec<Device>>>>,
    config: Option<uu_core::Config>,
    controller: Option<ControllerHandle>,
    textures: std::collections::HashMap<u32, video_texture::VideoTexture>,
    video_gl: Option<std::sync::Arc<eframe::glow::Context>>,
    presentation_vsync: bool,
    wallpapers: std::collections::HashMap<usize, egui::TextureHandle>,
    wallpaper_worker: Option<wallpapers::Worker>,
    background:background::Background,
    preview_writer:device_preview::Writer,
    recorded_connection:bool,
    frame_count: std::collections::HashMap<u32, u32>,
    fps: std::collections::HashMap<u32, f32>,
    last_fps: std::time::Instant,
    last_diagnostic: std::time::Instant,
    unpresented: std::collections::HashSet<u32>,
    render_ms: f32,
    hud_cache: hud::Cache,
    render_window: std::time::Instant,
    render_total_seconds: f32,
    render_samples: u32,
    selected_track: u32,
    displays: displays::Displays,
    seq: u32,
    kseq: u32,
    pressed_vks: input_capture::RemoteKeys,
    mod_state: [bool; 4],
    focused: bool,
    logged_in: bool,
    login_rx: Option<login::Worker>,
    account_token: Option<String>,
    account_error: Option<String>,
    account_poll_at: std::time::Instant,
    login_texture: Option<egui::TextureHandle>,
    login_status: String,
    uu_devices: uu_devices::Manager,
    hud: bool,
    preferences: settings::Settings,
    stream_settings_open: bool,
    settings_section: settings_page::Section,
    about_open: bool,
    settings_draft: settings::Settings,
    settings_notice: String,
    settings_notice_at: Option<std::time::Instant>,
    settings_storage_error: bool,
    audio_save_after: Option<std::time::Instant>,
    shortcuts: shortcuts::Shortcuts,
    native_keyboard_capture: bool,
    native_window_keyboard: bool,
    nested_passthrough: bool,
    autoconnect_done: bool,
    fullscreen: bool,
    window_snapshot: Option<window::Snapshot>,
    restore_maximized: std::rc::Rc<std::cell::Cell<bool>>,
    bitrate_idx: usize,
    resolution_idx: usize,
    custom_mbps: u32,
    target_fps: u32,
    fps_draft: u32,
    settings_seq: u64,
    settings_feedback: String,
    pointer: input::RemotePointer,
    mouse_captured: bool,
    remote_cursor_active: bool,
    cursor: cursor::Cursor,
    input_capture: input_capture::Capture,
    view_only: bool,
    stream_menu: stream_menu::StreamMenu,
    window_sizing: std::rc::Rc<std::cell::RefCell<window::WindowSizing>>,
    close_at: Option<std::time::Instant>,
    disconnecting: bool,
    output_reconnect_pending: bool,
    exit_requested: bool,
    window_edge_active: bool,
}

/// Preserve saved legacy indices. Native recommendations are calculated by the
/// host at the actual output geometry; compatibility quality 5 stays automatic.
const BITRATE_PRESETS: &[(&str, u64, Option<u32>)] = &[
    ("自动（原画）", 5, None),
    ("原画 30M", 6, Some(30)),
    ("超清 14M", 6, Some(14)),
    ("高清 8M", 6, Some(8)),
    ("自定义", 6, None),
    ("推荐码率（LAN Quality）", 5, None),
];

fn vk_for(key: egui::Key) -> Option<u32> {
    use egui::Key::*;
    Some(match key {
        A => 0x41,
        B => 0x42,
        C => 0x43,
        D => 0x44,
        E => 0x45,
        F => 0x46,
        G => 0x47,
        H => 0x48,
        I => 0x49,
        J => 0x4A,
        K => 0x4B,
        L => 0x4C,
        M => 0x4D,
        N => 0x4E,
        O => 0x4F,
        P => 0x50,
        Q => 0x51,
        R => 0x52,
        S => 0x53,
        T => 0x54,
        U => 0x55,
        V => 0x56,
        W => 0x57,
        X => 0x58,
        Y => 0x59,
        Z => 0x5A,
        Num0 => 0x30,
        Num1 => 0x31,
        Num2 => 0x32,
        Num3 => 0x33,
        Num4 => 0x34,
        Num5 => 0x35,
        Num6 => 0x36,
        Num7 => 0x37,
        Num8 => 0x38,
        Num9 => 0x39,
        Enter => 0x0D,
        Backspace => 0x08,
        Space => 0x20,
        Tab => 0x09,
        Escape => 0x1B,
        ArrowUp => 0x26,
        ArrowDown => 0x28,
        ArrowLeft => 0x25,
        ArrowRight => 0x27,
        Insert=>0x2d, Delete=>0x2e, Home=>0x24, End=>0x23, PageUp=>0x21, PageDown=>0x22,
        Semicolon|Colon=>0xba, Equals|Plus=>0xbb, Comma=>0xbc, Minus=>0xbd, Period=>0xbe,
        Slash|Questionmark=>0xbf, Backtick=>0xc0, OpenBracket=>0xdb, Backslash|Pipe=>0xdc,
        CloseBracket=>0xdd, Quote=>0xde,
        F1=>0x70,F2=>0x71,F3=>0x72,F4=>0x73,F5=>0x74,F6=>0x75,F7=>0x76,F8=>0x77,
        F9=>0x78,F10=>0x79,F11=>0x7a,F12=>0x7b,F13=>0x7c,F14=>0x7d,F15=>0x7e,F16=>0x7f,
        F17=>0x80,F18=>0x81,F19=>0x82,F20=>0x83,F21=>0x84,F22=>0x85,F23=>0x86,F24=>0x87,
        _ => return None,
    })
}

fn setup_style(ctx: &egui::Context) {
    // Styles are stored per theme. Select Light before updating its style;
    // otherwise a dark-system startup customizes Dark and then discards it.
    ctx.set_theme(egui::ThemePreference::Light);
    // Remote Ctrl +/- belongs to the remote app, never to this client's UI zoom.
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
    let mut fonts = egui::FontDefinitions::default();
    let mut candidates: Vec<(String, u32)> = if cfg!(target_os = "windows") {
        vec![
            r"C:\Windows\Fonts\msyh.ttc",
            r"C:\Windows\Fonts\simhei.ttf",
            r"C:\Windows\Fonts\simsun.ttc",
        ]
    } else {
        vec![
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/wenquanyi/wqy-zenhei/wqy-zenhei.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
            "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        ]
    }.into_iter().map(|path| (path.to_owned(), 0)).collect();
    #[cfg(target_os = "linux")]
    if let Ok(result) = std::process::Command::new("fc-match")
        .args(["-f", "%{file}\n%{index}\n", "sans-serif:lang=zh-cn"]).output() {
        if result.status.success() {
            let matched = String::from_utf8_lossy(&result.stdout);
            let mut lines = matched.lines();
            if let Some(path) = lines.next().filter(|path| !path.is_empty()) {
                let index = lines.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                candidates.insert(0, (path.to_owned(), index));
            }
        }
    }
    for (path, index) in candidates {
        if let Ok(data) = std::fs::read(path) {
            let mut font = egui::FontData::from_owned(data);
            font.index = index;
            fonts.font_data.insert("cjk".into(), font);
            fonts
                .families
                .get_mut(&egui::FontFamily::Proportional)
                .unwrap()
                .insert(0, "cjk".into());
            fonts
                .families
                .get_mut(&egui::FontFamily::Monospace)
                .unwrap()
                .push("cjk".into());
            break;
        }
    }
    ctx.set_fonts(fonts);

    theme::apply(ctx);
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, stream_target: Option<sessions::StreamTarget>, origin: Option<(i32, i32)>, presentation_vsync: bool, assistance:Option<uu_core::assistance::Request>) -> Self {
        setup_style(&cc.egui_ctx);
        if let Some(gl)=&cc.gl {video_texture::enable_gpu_video(gl);}
        Self::new_session(&cc.egui_ctx,cc.gl.clone(),std::rc::Rc::new(platform::Platform::new(cc,origin)),stream_target,presentation_vsync,assistance)
    }
    fn new_session(ctx:&egui::Context,gl:Option<std::sync::Arc<eframe::glow::Context>>,platform:std::rc::Rc<platform::Platform>,stream_target:Option<sessions::StreamTarget>,presentation_vsync:bool,assistance:Option<uu_core::assistance::Request>)->Self {
        static NEXT_INSTANCE:std::sync::atomic::AtomicU64=std::sync::atomic::AtomicU64::new(1);
        let (config, config_error) = match uu_core::Config::from_env() {
            Ok(config) => (Some(config), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let native=stream_target.as_ref().is_some_and(|target|target.is_native());
        let (account_token, account_error) = if native {(None, None)} else {
            match config.as_ref().map(|config| account::read_token(&config.at("state.json"))) {
                Some(Ok(token)) => (token, None),
                Some(Err(error)) => (None, Some(error.to_string())),
                None => (None, config_error),
            }
        };
        let logged_in = account_token.is_some();
        let device_library = if native { device_library::Library::default() } else { device_library::Library::for_account(config.as_ref(), account_token.as_deref()) };
        let (mut preferences, settings_notice, settings_storage_error) = match settings::Settings::path().map(|p|settings::Settings::load(&p)) {
            Some(Ok(s))=>(s,String::new(),false),Some(Err(e))=>(settings::Settings::default(),format!("读取设置失败：{e}"),true),None=>(settings::Settings::default(),"无法确定设置目录，当前使用默认设置".into(),true),
        };
        if let Some(sessions::StreamTarget {connection:sessions::ConnectionTarget::NativeProfile{snapshot,..},..}) = &stream_target {
            if let Ok(launch) = uu_core::device_book::Launch::read(snapshot) {
                if let Some(client) = launch.client { preferences.client = client; }
            }
        }
        if stream_target.is_some() {
            if let Ok(value)=std::env::var("UU_VSYNC"){preferences.vsync=value!="0";}
        }
        let mut app = Self {
            instance_id:NEXT_INSTANCE.fetch_add(1,std::sync::atomic::Ordering::Relaxed),
            stream_target,
            workspace_tabs:None,
            active_session:true,
            assistance,
            native_ui: native_ui::NativeUi::default(),
            gamepad:Default::default(),
            workspace:workspace_ui::Workspace::load(config.as_ref(),(preferences.lan_service_port,preferences.lan_discovery_port)),
            connection_details_open:false,
            remote_admin_ui:Default::default(),
            pairing_ui:Default::default(),
            lan:lan_ui::LanUi::new(config.as_ref().map(|c|c.at("native-lan"))),
            clipboard_window:std::sync::Arc::new(std::sync::atomic::AtomicIsize::new(platform.window_id())),
            native_host: native_host_ui::NativeHostUi::default(),
            sessions: sessions::Sessions::default(),
            platform,
            build_label: build_info::label(),
            executable_path: std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default(),
            context: window_context::Context::new(ctx),
            devices: Vec::new(),
            device_load_error: None,
            device_library,
            device_filter: device_library::Filter::All,
            selected: None,
            search: String::new(),
            status: String::new(),
            rx: None,
            preview_writer:Default::default(),recorded_connection:false,
            background:background::Background::new(config.as_ref().map(|c|c.at("appearance/wallpaper.jpg"))),
            config,
            controller: None,
            textures: std::collections::HashMap::new(),
            video_gl: gl,
            presentation_vsync,
            wallpapers: std::collections::HashMap::new(),
            wallpaper_worker: None,
            frame_count: std::collections::HashMap::new(),
            fps: std::collections::HashMap::new(),
            last_fps: std::time::Instant::now(),
            last_diagnostic: std::time::Instant::now(),
            unpresented: Default::default(),
            render_ms: 0.0,
            hud_cache: Default::default(),
            render_window: std::time::Instant::now(),
            render_total_seconds: 0.0,
            render_samples: 0,
            selected_track: 0,
            displays: displays::Displays::default(),
            seq: 1,
            kseq: 1,
            pressed_vks: input_capture::RemoteKeys::default(),
            mod_state: [false; 4],
            focused: false,
            logged_in,
            login_rx: None,
            account_token,
            account_error,
            account_poll_at: std::time::Instant::now(),
            login_texture: None,
            login_status: String::new(),
            uu_devices: Default::default(),
            hud: std::env::var("UU_HUD").map(|v| v != "0").unwrap_or(preferences.hud),
            preferences: preferences.clone(),
            stream_settings_open: false,
            settings_section: settings_page::Section::default(),
            about_open: false,
            settings_draft: preferences.clone(),
            settings_notice,
            settings_notice_at: None,
            settings_storage_error,
            audio_save_after: None,
            shortcuts: shortcuts::Shortcuts::default(),
            native_keyboard_capture: false,
            native_window_keyboard: false,
            nested_passthrough: preferences.nested_shortcuts,
            autoconnect_done: false,
            fullscreen: false,
            window_snapshot: None,
            restore_maximized: Default::default(),
            bitrate_idx: preferences.bitrate,
            resolution_idx: preferences.resolution,
            custom_mbps: preferences.custom_mbps,
            target_fps: if std::env::var_os("UU_FPS").is_some(){uu_core::control_wire::CaptureFrameRate::from_env().fps()}else{preferences.fps},
            fps_draft: preferences.fps,
            settings_seq: 0,
            settings_feedback: String::new(),
            pointer: input::RemotePointer::default(),
            mouse_captured: false,
            remote_cursor_active: false,
            cursor: cursor::Cursor::default(),
            input_capture: input_capture::Capture::default(),
            view_only: false,
            stream_menu: stream_menu::StreamMenu::default(),
            window_sizing: Default::default(),
            close_at: None,
            disconnecting: false,
            output_reconnect_pending: false,
            exit_requested: false,
            window_edge_active: false,
        };
        if let Some(target) = app.stream_target.clone() {
            app.devices.push(Device {
                alias: target.alias, device_id: match target.connection {sessions::ConnectionTarget::UuDevice(id) | sessions::ConnectionTarget::UuAssistance(id)=>id,sessions::ConnectionTarget::NativeInvitation(_) | sessions::ConnectionTarget::NativeDevice(_) | sessions::ConnectionTarget::NativeInternet(_) | sessions::ConnectionTarget::NativeProfile{..}=>String::new()},
                status: "CONNECTED".into(), controllable: true,
                wallpaper_url: String::new(), platform: 0,
            });
            app.selected = Some(0);
            app.view_only = target.view_only;
            app.autoconnect_done = true;
            app.connect();
        } else if logged_in {
            app.start_load();
        } else {
            app.status = "选择连接方式".into();
        }
        app
    }

    fn start_load(&mut self) {
        if self.rx.is_some() { return; }
        let (Some(config), Some(token)) = (self.config.clone(), self.account_token.clone()) else { return; };
        self.wallpaper_worker = None;
        self.device_load_error = None;
        let (tx, rx) = channel();
        let context = self.context.clone();
        std::thread::spawn(move || {
            let _ = tx.send(load_devices(config, token));
            context.request_repaint();
        });
        self.rx = Some(rx);
        self.status = "正在加载设备…".to_string();
    }

    fn ensure_visible_device_selection(&mut self) {
        let visible = self.device_library.visible_indices(&self.devices, self.device_filter, &self.search);
        self.selected = device_library::Library::retain_selection(self.selected, &visible);
    }

    fn start_login(&mut self) {
        if let Some(error) = &self.account_error {
            self.login_status = error.clone();
            return;
        }
        let Some(config) = self.config.clone() else {
            self.login_status = "无法确定登录目录，请检查本机用户目录配置".into();
            return;
        };
        let name = match self.workspace.prepare_device_name() {
            Ok(name) => name,
            Err(error) => { self.login_status = error.to_string(); return; }
        };
        self.login_status = "获取二维码…".into();
        self.login_rx = None;
        self.login_texture = None;
        self.login_rx = Some(login::spawn(config, name));
    }

    fn rename_uu_device(&mut self) {
        let Some(config) = self.config.clone() else { return; };
        let name = match self.workspace.prepare_device_name() {
            Ok(name) => name,
            Err(error) => { self.login_status = error.to_string(); return; }
        };
        self.login_rx = None;
        self.login_texture = None;
        self.login_status = "正在更新 UU 注册信息…".into();
        self.login_rx = Some(login::rename(config, name));
    }

    fn poll_login(&mut self, ctx: &egui::Context) {
        if self.login_rx.is_none() {
            return;
        }
        let mut events = Vec::new();
        if let Some(rx) = &self.login_rx {
            loop {
                match rx.try_recv() {
                    Ok(ev) => events.push(ev),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                }
            }
        }
        for ev in events {
            match ev {
                LoginEvent::Qr(url) => match login::qr_rgba(&url, 6) {
                    Ok((w, h, rgba)) => {
                        let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
                        self.login_texture =
                            Some(ctx.load_texture("qr", img, egui::TextureOptions::NEAREST));
                        self.login_status = "请用手机扫码".into();
                    }
                    Err(e) => self.login_status = format!("二维码生成失败: {e}"),
                },
                LoginEvent::Status(s) => self.login_status = s,
                LoginEvent::Done {token, user_id, device_name, name_error} => {
                    let saved = self.config.as_ref().ok_or_else(||anyhow::anyhow!("无法确定登录目录"))
                        .and_then(|config|account::store_login(&config.at("state.json"),token.clone(),user_id,device_name));
                    self.login_rx = None;
                    match saved {
                        Ok(()) => {
                            self.apply_account(Some(token));
                            if let Some(error) = name_error { self.login_status = format!("账号已登录，设备改名失败：{error}；可点击“保存名称”重试。"); }
                        },
                        Err(error) => self.login_status = format!("保存登录失败：{error}"),
                    }
                }
                LoginEvent::NameSaved { name, token } => {
                    self.login_rx = None;
                    self.login_status = match self.config.as_ref()
                        .ok_or_else(|| anyhow::anyhow!("无法确定登录目录"))
                        .and_then(|c| account::store_device_name(&c.at("state.json"), &token, name)) {
                        Ok(()) => { self.rx = None; self.start_load(); "UU 设备名称已更新".into() },
                        Err(error) => format!("云端名称已更新，保存本地记录失败：{error}"),
                    };
                }
                LoginEvent::Error(e) => {
                    self.login_rx = None;
                    self.login_status = if self.logged_in { format!("保存设备名称失败：{e}") } else { format!("登录失败：{e}") };
                },
            }
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.rx {
            let result = match rx.try_recv() {
                Ok(result) => Some(result),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(anyhow::anyhow!("设备列表加载已中断，请重试"))),
            };
            if let Some(result) = result {
                match result {
                    Ok(devices) => {
                        self.device_load_error = None;
                        self.status = format!("已加载 {} 台设备", devices.len());
                        self.wallpapers.clear();
                        let requests = devices.iter().filter(|d| !d.wallpaper_url.is_empty())
                            .map(|d| wallpapers::Request { device_id: d.device_id.clone(), url: d.wallpaper_url.clone() }).collect::<Vec<_>>();
                        self.wallpaper_worker = if requests.is_empty() { None } else { wallpapers::Worker::start(requests, ctx.clone()).ok() };
                        let selected_id = self.selected.and_then(|i| self.devices.get(i)).map(|d| d.device_id.clone());
                        self.devices = devices;
                        self.selected = selected_id.and_then(|id| self.devices.iter().position(|d|d.device_id == id))
                            .or_else(|| (!self.devices.is_empty()).then_some(0));
                        self.ensure_visible_device_selection();
                        // Optional headless auto-connect for testing:
                        //   UU_AUTOCONNECT=<explicit device_id|alias>
                        if !self.autoconnect_done {
                            self.autoconnect_done = true;
                            // Only an explicit device id or alias is accepted —
                            // never "first online" (that once picked the wrong
                            // machine).
                            if let Ok(want) = std::env::var("UU_AUTOCONNECT") {
                                if let Some(i) = self
                                    .devices
                                    .iter()
                                    .position(|d| d.device_id == want || d.alias == want)
                                {
                                    self.selected = Some(i);
                                    self.connect();
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let error = e.to_string();
                        self.status = format!("加载失败: {error}");
                        self.device_load_error = Some(error);
                    }
                }
                self.rx = None;
            }
        }
        if let Some(worker) = &mut self.wallpaper_worker {
            for _ in 0..2 {
                let Some(preview) = worker.try_recv() else { break };
                if let Some(index) = self.devices.iter().position(|d| d.device_id == preview.device_id && d.wallpaper_url == preview.url) {
                    let image = egui::ColorImage::from_rgba_unmultiplied([preview.width as usize, preview.height as usize], &preview.rgba);
                    self.wallpapers.insert(index, ctx.load_texture(format!("wallpaper-{index}"), image, egui::TextureOptions::LINEAR));
                }
            }
            if worker.finished() { self.wallpaper_worker = None; }
        }
    }

    fn poll_controller(&mut self, ctx: &egui::Context) {
        let native=self.is_native();
        let Some(h) = &mut self.controller else {
            return;
        };
        let mut apply_settings = false;
        while let Ok(ev) = h.events.try_recv() {
            match ev {
                ControllerEvent::Status(s) => {
                    apply_settings |= s == "已请求目标画质/码率";
                    self.status = s;
                }
                ControllerEvent::Error(e) => self.status = format!("错误: {e}"),
            }
        }
        if native && self.displays.native_frame_epoch!=h.frames.generation(){
            self.displays.native_frame_epoch=h.frames.generation();
            self.textures.clear();self.unpresented.clear();self.frame_count.clear();self.fps.clear();
            self.hud_cache=Default::default();
        }
        let mut frames = Vec::new();
        while let Ok(f) = h.frames.try_recv_image() {
            frames.push(f);
        }
        let mut arrived=Vec::new();
        for image in frames {
            let idx=match &image {uu_core::display_frames::DisplayImage::Rgba(f)=>f.0,uu_core::display_frames::DisplayImage::Decoded(track,..)=>*track};
            if !self.recorded_connection {
                if let (Some(sessions::StreamTarget{connection:sessions::ConnectionTarget::NativeProfile{id,..},..}),Some(store))=(&self.stream_target,&self.workspace.store) {
                    self.preview_writer.submit(store.clone(),id.clone(),None);
                }
                self.recorded_connection=true;
            }
            let source_white=h.stats.native_hdr_response.lock().unwrap().as_ref().map_or(203.,|r|r.metadata.sdr_white_nits);
            arrived.push(idx);
            match self.textures.get_mut(&idx) {
                Some(t) => t.set(ctx, image),
                None => {
                    if native && self.status.starts_with("正在") {self.status="原生串流已连接".into();}
                    h.stats.diagnostics.record(serde_json::json!({"event":"first_frame","track":idx,"backend":h.stats.backend(),"video_size":h.stats.res()}));
                    self.textures.insert(idx, video_texture::VideoTexture::new(ctx, self.video_gl.as_ref(), image));
                }
            }
            if let Some(texture)=self.textures.get_mut(&idx){texture.set_source_white(source_white);}
            self.unpresented.insert(idx);
        }
        if let Some(h) = &self.controller {
            if let Some(applied)=h.stats.native_quality_applied.lock().unwrap().as_ref().filter(|a|a.request==self.settings_seq) {
                self.settings_feedback=match &applied.error {Some(error)=>format!("画质修改失败：{error}"),None=>format!("远端已应用：{} FPS · {:.1} Mbps",applied.settings.fps,applied.bitrate_kbps as f64/1000.)};
            }
            let response = h.stats.settings_response.load(std::sync::atomic::Ordering::Acquire);
            if response >> 1 == 2000 + self.settings_seq && self.settings_feedback.starts_with("已发送") {
                if response & 1 != 0 {self.remote_cursor_active=self.preferences.cursor_mode.remote();}
                self.settings_feedback = if response & 1 != 0 {
                    format!("远端已确认请求：{} FPS · {}", self.target_fps, settings::bitrate_label(self.bitrate_idx,self.is_native()))
                } else { "远端拒绝了画质设置，请重新选择".into() };
            }
        }
        if self.supports_remote(uu_core::capabilities::Capability::Displays) {self.poll_displays(&arrived);}
        if apply_settings {
            self.send_capture_settings();
        }
        if self.controller.as_ref().is_some_and(|h| h.stopped.load(std::sync::atomic::Ordering::Acquire)) {
            if let Some(h)=&self.controller {h.stats.audio.stop();h.stats.clipboard.stopped.store(true,std::sync::atomic::Ordering::Release);}
            self.release_all_keys();self.release_pointer_capture(ctx);self.shortcuts.clear();
            self.save_last_remote_frame();
            self.controller = None;
            self.disconnecting = false;
            self.input_capture.release();
            #[cfg(windows)]
        eframe::glow_presentation::remove(&self.context,self.context.viewport());
        self.textures.clear();
            self.hud_cache=Default::default();
            self.frame_count.clear();
            self.fps.clear();
            self.settings_feedback.clear();
            self.cursor=cursor::Cursor::default();self.remote_cursor_active=false;
            self.displays=displays::Displays::default();
            if std::mem::take(&mut self.output_reconnect_pending) && !self.exit_requested {
                self.connect();
            }
        }
    }

    fn send_input(&mut self, payload: serde_json::Value, kind: u32) {
        self.send_input_ch(payload, kind, 35);
    }

    fn input_screen(&self, track: u32) -> u32 {
        if track==self.selected_track {
            if let Some(id)=self.displays.selected.filter(|id|*id>=0){return id as u32;}
        }
        self.controller.as_ref().and_then(|h| {
            let screens=h.stats.remote_screens.lock().unwrap();
            self.displays.selected.and_then(|id|screens.screens.iter().find(|s|s.id==id && s.assigned_track()==Some(track)))
                .or_else(||screens.for_track(track)).map(|s|s.id)
        })
            .filter(|id| *id >= 0).map(|id| id as u32).unwrap_or(track + 1)
    }
    fn input_ready(&self) -> bool {
        self.supports_remote(uu_core::capabilities::Capability::Input) && !self.disconnecting && self.displays.ready() && self.controller.as_ref().is_some_and(|h|
            h.stats.control_open.load(std::sync::atomic::Ordering::Acquire) != 0)
    }

    /// Apply frame rate, quality and resolution together so another setting never resets FPS.
    fn send_capture_settings(&mut self) {self.send_capture_settings_through(false);}
    fn capture_profile(&self) -> displays::CaptureProfile {
        (self.target_fps, self.bitrate_idx, if self.bitrate_idx == 4 {self.custom_mbps} else {0}, self.resolution_idx, self.preferences.cursor_mode.remote())
    }
    fn capture_resolution(&self) -> uu_core::control_wire::CaptureResolution {
        match self.resolution_idx {
            1=>uu_core::control_wire::CaptureResolution::Exact(1920,1080),
            2=>uu_core::control_wire::CaptureResolution::Exact(2560,1440),
            3=>uu_core::control_wire::CaptureResolution::Exact(3840,2160),
            _=>uu_core::control_wire::CaptureResolution::Unchanged,
        }
    }
    fn send_capture_settings_through(&mut self,immediate:bool) {
        if self.stream_target.as_ref().is_some_and(|target|target.is_native()) {
            if self.disconnecting {return;}
            let request=self.preferences.native_request();
            let dynamic=self.controller.as_ref().is_some_and(|h|h.stats.native_quality_response.lock().unwrap().as_ref().is_some_and(|r|r.dynamic));
            let reconnect=self.controller.as_ref().is_some_and(|h|h.stats.native_quality_request.get().is_some_and(|old|if dynamic {old.bounds!=request.bounds||old.codec!=request.codec}else{old!=&request}));
            let reconnect=reconnect || self.controller.as_ref().is_some_and(|h|h.stats.native_hdr_requested.get().is_some_and(|old|*old!=self.preferences.hdr));
            if reconnect {
                self.output_reconnect_pending=true;self.stop_connection();self.status="正在应用原生串流画质…".into();
            }else if dynamic {
                if let Some(h)=&self.controller {
                    let rate=*h.stats.native_rate_response.lock().unwrap();
                    if rate.is_none() && self.preferences.rate_control.is_some() {
                        self.settings_feedback="此主机不支持客户端选择码率模式，请升级主机".into();return;
                    }
                    let settings=uu_core::native_quality::Update{fps:request.fps,bitrate_kbps:request.bitrate_kbps,rate_control:rate.map(|r|self.preferences.rate_control.unwrap_or(r.default))};
                    if h.stats.native_quality_current.lock().unwrap().as_ref()==Some(&settings){return;}
                    self.settings_seq=self.settings_seq.wrapping_add(1);
                    self.settings_feedback=match h.native_input.as_ref().ok_or("画质控制通路未连接").and_then(|input|input.set_quality(self.settings_seq,settings).map_err(|_|"画质请求队列已满或连接已关闭")) {
                        Ok(())=>"已发送原生画质更新，等待主机实际应用…".into(),Err(error)=>error.into(),
                    };
                }
            }else{
                self.settings_feedback=if self.controller.as_ref().is_some_and(|h|h.stats.native_quality_response.lock().unwrap().is_some()) {"画质已与主机协商，实际参数见串流信息"}else{"旧版主机使用其本机画质设置；升级主机后可协商"}.into();
            }
            return;
        }
        if !self.supports_remote(uu_core::capabilities::Capability::Quality) {self.settings_feedback="当前画质由被控端设置".into();return;}
        let Some(frame_rate)=uu_core::control_wire::CaptureFrameRate::new(self.target_fps) else {self.settings_feedback="UU 兼容协议支持 5–144 FPS；请选择有效帧率".into();return;};
        if self.disconnecting { return; }
        let resolution = self.capture_resolution();
        if self.controller.as_ref().is_some_and(|h| h.stats.output_resolution.get()
            .is_some_and(|previous| previous.output_bounds() != resolution.output_bounds())
            || h.stats.prefer_hevc.load(std::sync::atomic::Ordering::Acquire) != self.preferences.hevc_preferred())
        {
            // The host replaces runtime scale limits with negotiated decoder
            // bounds. Release the old session before negotiating a new bound.
            self.output_reconnect_pending = true;
            self.stop_connection();
            self.status = "正在切换视频设置…".into();
            return;
        }
        let profile = self.capture_profile();
        let Some(h) = &self.controller else { return };
        let (_, quality, preset_mbps) = BITRATE_PRESETS[self.bitrate_idx];
        let custom_bps = if quality == 6 {
            Some(preset_mbps.unwrap_or(self.custom_mbps) as u64 * 1_000_000)
        } else {
            None
        };
        self.settings_seq = self.settings_seq.wrapping_add(1);
        let source=self.displays.selected.unwrap_or_else(||h.stats.remote_screens.lock().unwrap().for_track(self.selected_track).map_or(-1,|s|s.id));
        let msg = uu_core::control_wire::capture_setting_for_screen(
            2000 + self.settings_seq,
            uu_core::control_wire::now_ms(),
            frame_rate,
            quality,
            custom_bps,
            resolution,
            self.preferences.cursor_mode.remote(),
            source,
        );
        let request=2000+self.settings_seq;
        self.displays.settings_sent(request, source, self.selected_track, profile);
        let sent=if immediate {h.input.send(msg)}else{h.settings.send(msg)};
        self.settings_feedback = if sent.is_ok() {
            format!("已发送：{} FPS · {} · {}", self.target_fps, settings::bitrate_label(self.bitrate_idx,self.is_native()),
                if quality == 6 { format!("{} Mbps", preset_mbps.unwrap_or(self.custom_mbps)) }
                else { "以实际接收码率为准".into() })
        } else {
            "设置发送失败：连接已关闭".into()
        };
    }

    fn local_shortcut(&mut self, action: shortcuts::Action, ctx: &egui::Context) {
        self.release_all_keys();
        match action {
            shortcuts::Action::Stats=>{self.hud=!self.hud;self.persist_preferences();},
            shortcuts::Action::Menu=>{self.release_pointer_capture(ctx);self.stream_menu.visible=!self.stream_menu.visible;self.stream_menu.open=false;ctx.memory_mut(|m|m.close_popup());self.persist_preferences();},
            shortcuts::Action::Mute=>{if self.supports_remote(uu_core::capabilities::Capability::Audio){self.toggle_audio_mute();}},
            shortcuts::Action::Fullscreen=>self.toggle_fullscreen(ctx),
            shortcuts::Action::Exit=>self.disconnect(),
            shortcuts::Action::MouseMode=>{
                self.release_pointer_capture(ctx);
                self.preferences.relative_mouse=!self.preferences.relative_mouse;
                self.status=if self.preferences.relative_mouse {"鼠标：游戏模式 · 相对移动"} else {"鼠标：桌面模式 · 绝对定位"}.into();
                self.persist_preferences();
            },
            shortcuts::Action::CursorMode=>{
                self.preferences.cursor_mode=self.preferences.cursor_mode.next();
                self.status=format!("光标：{}",self.preferences.cursor_mode.label());
                self.persist_preferences();
            },
            shortcuts::Action::ToggleNested=>{
                self.nested_passthrough=!self.nested_passthrough;
                self.status=if self.nested_passthrough {
                    "嵌套快捷键：Ctrl+Alt 客户端命令发送到远端；F12 切回本机".into()
                } else {
                    "嵌套快捷键：Ctrl+Alt 客户端命令由本机处理".into()
                };
                self.persist_preferences();
            },
            shortcuts::Action::Capture=>{
                self.input_capture.toggle();
                if let Some(h)=&self.controller { h.stats.diagnostics.record(serde_json::json!({"event":"input_focus","active":self.input_capture.requested()})); }
            },
        }
        self.update_keyboard_capture(ctx);
    }
    fn input_conditions(&self,ctx:&egui::Context)->input_capture::Conditions {
        input_capture::Conditions {
            ready:self.input_ready(), focused:self.active_session && ctx.input(|i|i.focused), view_only:self.view_only,
            local_ui:self.stream_menu.owns_input(self.fullscreen) || self.displays.menu_open || self.local_dialog_open()
                || self.workspace_tabs.as_ref().is_some_and(|tabs|tabs.owns_input(ctx)),
        }
    }
    fn remote_input_active(&self,ctx:&egui::Context)->bool {
        self.input_capture.active(self.input_conditions(ctx))
    }
    fn update_keyboard_capture(&mut self,ctx:&egui::Context) {
        let conditions=self.input_conditions(ctx);
        let remote=self.input_capture.active(conditions);
        if self.is_native() {
            let requested=self.input_capture.wants_control(conditions)&&!self.disconnecting&&self.displays.ready();
            if let Some(sender)=self.controller.as_ref().and_then(|h|h.native_input.as_ref()) {
                if sender.requested()!=requested {sender.set_enabled(requested);}
            }
        }
        if self.input_capture.stopped(conditions){self.release_all_keys();}
        // Client commands remain local while viewing, in menus, or after
        // releasing remote input. They do not depend on a host control grant.
        let shortcuts=self.stream_target.is_some() && !self.disconnecting && ctx.input(|i|i.focused);
        let window=self.platform.window_keyboard(remote,shortcuts,self.nested_passthrough);
        let active=self.platform.capture_keyboard(self.input_capture.system(conditions,self.fullscreen,self.preferences.capture_keyboard));
        if self.native_keyboard_capture!=active || self.native_window_keyboard!=window {
            if let Some(h)=&self.controller {h.stats.diagnostics.record(serde_json::json!({
                "event":"input_capture", "requested":self.input_capture.requested(),
                "ready":conditions.ready, "focused":conditions.focused,
                "view_only":conditions.view_only, "local_ui":conditions.local_ui,
                "fullscreen":self.fullscreen, "system_keys_enabled":self.preferences.capture_keyboard,
                "remote_input":remote, "native_window_input":window, "native_system_capture":active,
                "keyboard":self.platform.keyboard_diagnostics()
            }));}
        }
        let gamepad=if self.preferences.gamepad && self.is_native() && remote {
            self.controller.as_ref().and_then(|h|h.native_input.as_ref().map(|input|gamepad::Target{input:input.clone(),stats:h.stats.clone()}))
        }else{None};
        self.gamepad.borrow_mut().configure(self.instance_id,gamepad);
        self.native_keyboard_capture=active;self.native_window_keyboard=window;
    }

    fn release_pointer_capture(&mut self,ctx:&egui::Context) {
        for message in self.pointer.release_all(self.input_screen(self.selected_track)){self.send_input(message,15);}
        if self.mouse_captured {
            // Release before another window acquires the process-wide native
            // pointer grab, rather than enqueueing a later release behind it.
            if let Some(window)=eframe::viewport_window_for(ctx,self.context.viewport()) {
                let _=window.set_cursor_grab(winit::window::CursorGrabMode::None);
                window.set_cursor_visible(true);
            }else{
                ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::viewport::CursorGrab::None));
                ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(true));
            }
            self.mouse_captured=false;
        }
    }
    fn update_pointer_capture(&mut self,ctx:&egui::Context) {
        let capture=self.preferences.relative_mouse && self.remote_input_active(ctx) && !self.textures.is_empty();
        if capture!=self.mouse_captured {
            self.release_pointer_capture(ctx);
            if capture {
                // Windows supports confinement; raw WM_INPUT deltas remain
                // unbounded even when its invisible cursor reaches the edge.
                let mode = if cfg!(target_os = "linux") {
                    egui::viewport::CursorGrab::Locked
                } else {
                    egui::viewport::CursorGrab::Confined
                };
                ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(mode));
                ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(false));self.mouse_captured=true;
            }
        }
        if self.mouse_captured {ctx.set_cursor_icon(egui::CursorIcon::None);}
    }

    fn update_clipboard(&mut self, ctx: &egui::Context) {
        use std::sync::atomic::Ordering::{Acquire, AcqRel, Release};
        let native = self.is_native();
        let ready = self.input_ready()
            || (native && self.supports_remote(uu_core::capabilities::Capability::Clipboard)
                && self.controller.as_ref().is_some_and(|h| h.stats.decoded_total.load(Acquire) > 0));
        if let Some(h) = &self.controller {
            let clipboard = &h.stats.clipboard;
            let enabled = (if native { self.preferences.native_clipboard } else { self.preferences.clipboard })
                && clipboard.available.load(Acquire);
            let active = self.active_session && ready && ctx.input(|i| i.focused) && !self.disconnecting;
            // Invalidate incoming data before reactivation, even if a complete
            // focus round trip happened between the transport's polling ticks.
            if native && (clipboard.enabled.load(Acquire) != enabled
                || clipboard.active.load(Acquire) != active
                || clipboard.read_only.load(Acquire) != self.view_only)
            {
                clipboard.generation.fetch_add(1, AcqRel);
            }
            clipboard.enabled.store(enabled, Release);
            clipboard.read_only.store(self.view_only, Release);
            let previous = clipboard.active.swap(active, Release);
            if !active || !enabled {
                clipboard.local.lock().unwrap().take();
                clipboard.remote.lock().unwrap().take();
                if previous { *clipboard.protocol.lock().unwrap() = Default::default(); }
            }
        }
    }

    fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        self.displays.menu_open=false;
        self.release_all_keys();
        self.fullscreen = !self.fullscreen;
        self.stream_menu.open = false;
        ctx.memory_mut(|m| m.close_popup());
        if self.fullscreen {
            self.input_capture.request();
            // Windows keeps WS_MAXIMIZE when entering borderless fullscreen.
            // Restore first so its client area can cover the entire monitor.
            self.restore_maximized.set(ctx.input(|i| i.viewport().maximized.unwrap_or(false)));
            if self.restore_maximized.get() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            if self.active_session {self.platform.focus_window();}
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            if self.restore_maximized.get() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
            self.restore_maximized.set(false);
        }
    }

    fn toggle_maximized(&mut self, ctx: &egui::Context) {
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        if self.fullscreen {
            self.fullscreen = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
        self.stream_menu.open = false;
        ctx.memory_mut(|m| m.close_popup());
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }

    /// Encode UI semantics for the selected transport. Native sessions use
    /// typed HID commands; the compatibility path retains its CONTROL JSON.
    fn send_input_ch(&mut self, payload: serde_json::Value, _kind: u32, _channel: u16) {
        if self.view_only && !matches!(payload["action"].as_str(), Some("kbd_release" | "mouse_release")) {
            return;
        }
        if !self.input_ready() && !matches!(payload["action"].as_str(), Some("kbd_release" | "mouse_release")) {
            return;
        }
        if self.is_native() {
            if let Some(command)=native_keys::command(&payload){self.send_native(command);}
            return;
        }
        let Some(h) = &self.controller else { return };
        self.seq = self.seq.wrapping_add(1);
        if std::env::var_os("UU_INPUT_AUDIT").is_some()
            && matches!(
                payload["action"].as_str(),
                Some("mouse_press" | "mouse_release")
            )
        {
            println!(
                "pointer audit: {} button={}",
                payload["action"], payload["button"]
            );
        }
        let _ = h.input.send(payload.to_string().into_bytes());
    }

    #[track_caller]
    fn send_key_repeat(&mut self, vk: u32) {
        let payload = serde_json::json!({
            "action": "kbd_press", "interrept": "i", "key": vk, "kseq": self.kseq
        });
        self.kseq = self.kseq.wrapping_add(1);
        self.send_input_ch(payload, 8, 0);
    }

    fn send_modifier_repeats(&mut self) {
        let uu_compat = !self.is_native();
        for vk in self.pressed_vks.modifier_repeats(std::time::Instant::now(), uu_compat) {
            self.send_key_repeat(vk);
        }
    }

    #[track_caller]
    fn send_key(&mut self, vk: u32, pressed: bool) {
        let change=self.pressed_vks.event(vk,pressed);
        if change==input_capture::KeyChange::None{return;}
        let caller=std::panic::Location::caller();
        if change!=input_capture::KeyChange::Repeat && matches!(vk,0x10..=0x12|0x5b|0x5c|0xa0..=0xa5) {
            if let Some(h)=&self.controller {h.stats.diagnostics.record(serde_json::json!({
                "event":"keyboard_transition","vk":vk,"pressed":pressed,"kseq":self.kseq,
                "keyboard":self.platform.keyboard_diagnostics(),
                "source":std::path::Path::new(caller.file()).file_name().and_then(|name|name.to_str()),
                "line":caller.line(),"fullscreen":self.fullscreen,"focused":self.focused
            }));}
        }
        let payload = serde_json::json!({
            "action": if pressed { "kbd_press" } else { "kbd_release" },
            "interrept": "i", "key": vk, "kseq": self.kseq
        });
        self.kseq = self.kseq.wrapping_add(1);
        self.send_input_ch(payload, 8, 0);
    }

    fn sync_modifiers(&mut self, modifiers: egui::Modifiers) {
        for (vk, down, index) in [
            (0xA2, modifiers.ctrl, 0),
            (0xA0, modifiers.shift, 1),
            (0xA4, modifiers.alt, 2),
            (0x5B, modifiers.mac_cmd && (!cfg!(target_os = "linux") || (self.fullscreen && self.preferences.capture_keyboard)), 3),
        ] {
            if self.mod_state[index] != down {
                self.send_key(vk, down);
                self.mod_state[index] = down;
            }
        }
    }

    #[track_caller]
    fn release_all_keys(&mut self) {
        // A view-only window that never forwarded a key must not send blanket
        // modifier releases to the host when it gains/loses local focus.
        if self.view_only && self.pressed_vks.is_empty() && self.mod_state.iter().all(|down| !down) {
            return;
        }
        if self.is_native() {
            if let Some(sender)=self.controller.as_ref().and_then(|h|h.native_input.as_ref()){sender.release_all();}
            self.pressed_vks.clear();self.mod_state=[false;4];return;
        }
        let keys=self.pressed_vks.held();
        if !keys.is_empty() {
            let caller=std::panic::Location::caller();
            if let Some(h)=&self.controller {h.stats.diagnostics.record(serde_json::json!({
                "event":"input_release_all", "held_count":keys.len(),
                "win_held":keys.iter().any(|vk|matches!(*vk,0x5b|0x5c)),
                "alt_held":keys.iter().any(|vk|matches!(*vk,0x12|0xa4|0xa5)),
                "source":std::path::Path::new(caller.file()).file_name().and_then(|name|name.to_str()),
                "line":caller.line(),"fullscreen":self.fullscreen,"focused":self.focused
            }));}
        }
        for vk in keys {self.send_key(vk,false);}
        self.mod_state = [false; 4];
    }

    /// A transport/routing reset releases held input without cancelling the
    /// user's request. Focus, UI ownership and host authorization still gate it.
    fn recover_input(&mut self, reason:&str) {
        if let Some(h)=&self.controller {h.stats.diagnostics.record(serde_json::json!({
            "event":"input_reset", "reason":reason, "requested":self.input_capture.requested()
        }));}
        self.release_all_keys();
        let screen=self.input_screen(self.selected_track);
        let releases=self.pointer.release_all(screen);
        // Native release_all is out of band and invalidates queued commands.
        // Do not enqueue more releases into the queue that just failed.
        if !self.is_native() {for release in releases {self.send_input(release,15);}}
    }

    fn deactivate_session(&mut self) {
        self.active_session=false;
        if let Some(handle)=&self.controller {handle.frames.set_waker(||{});}
        self.release_session_input();
    }
    fn release_session_input(&mut self) {
        self.release_all_keys();
        for message in self.pointer.release_all(self.input_screen(self.selected_track)){self.send_input(message,15);}
        self.release_pointer_capture(&self.context.clone());
        self.gamepad.borrow_mut().configure(self.instance_id,None);
        self.shortcuts.clear();self.focused=false;
        self.platform.window_keyboard(false,false,false);self.platform.capture_keyboard(false);
        if let Some(handle)=&self.controller {
            if let Some(input)=&handle.native_input{input.set_enabled(false);}
            use std::sync::atomic::Ordering::{AcqRel,Release};
            let clipboard=&handle.stats.clipboard;
            clipboard.active.store(false,Release);clipboard.generation.fetch_add(1,AcqRel);
            clipboard.local.lock().unwrap().take();clipboard.remote.lock().unwrap().take();
        }
    }
    fn decoder_kind(&self)->uu_core::media::DecoderKind {
        use uu_core::media::DecoderKind;
        if std::env::var_os("UU_DECODER").is_some(){DecoderKind::from_env()}
        else if self.preferences.software_decoder{DecoderKind::Software}else{DecoderKind::Auto}
    }

    fn save_last_remote_frame(&mut self) {
        if let (Some(sessions::StreamTarget{connection:sessions::ConnectionTarget::NativeProfile{id,..},..}),Some(store))=(&self.stream_target,&self.workspace.store) {
            if let Some(texture)=self.textures.get(&self.selected_track).or_else(||self.textures.values().next()) {
                match texture.snapshot() {
                    Ok(image)=>self.preview_writer.submit(store.clone(),id.clone(),Some(image)),
                    Err(error)=>self.set_settings_notice(format!("本次预览未更新：{error}")),
                }
            }
        }
    }
    fn disconnect(&mut self) {
        self.output_reconnect_pending = false;
        if self.stream_target.is_some() { self.exit_requested = true; }
        self.stop_connection();
    }

    fn stop_connection(&mut self) {
        if self.disconnecting { return; }
        self.save_last_remote_frame();
        self.release_pointer_capture(&self.context.clone());
        self.stream_menu.open = false;
        self.displays.menu_open=false;
        self.context.memory_mut(|m| m.close_popup());
        self.release_all_keys();
        for message in self.pointer.release_all(self.input_screen(self.selected_track)) {
            self.send_input(message, 15);
        }
        self.input_capture.release();
        if let Some(h) = &self.controller {
            let _ = h.close.send(());
            self.disconnecting = true;
        }
        self.textures.clear();
            self.hud_cache=Default::default();
        self.frame_count.clear();
        self.fps.clear();
        self.status = if self.disconnecting { "正在释放连接…" } else { "已断开" }.into();
    }

    fn open_stream_window(&mut self) {
        let Some(device) = self.selected.and_then(|i| self.devices.get(i)) else { return };
        let target = sessions::StreamTarget {
            connection: sessions::ConnectionTarget::UuDevice(device.device_id.clone()), alias: device.alias.clone(), view_only: self.view_only,
        };
        let result = std::env::current_exe().and_then(|exe| self.sessions.open(&exe, &target, self.platform.origin()));
        if result.is_ok() {
            if let sessions::ConnectionTarget::UuDevice(id) = &target.connection { self.device_library.record_opened(id); }
        }
        self.status = match result {
            Ok(true) => format!("已打开 {} 的串流窗口，可继续选择其他设备", target.alias),
            Ok(false) => format!("{} 的串流窗口已打开", target.alias),
            Err(error) => format!("无法打开串流窗口：{error}"),
        };
    }

    fn connect(&mut self) {
        self.recorded_connection=false;
        if self.stream_target.is_none() {
            self.open_stream_window();
            return;
        }
        if self.controller.is_some() { return; }
        if let Some(sessions::ConnectionTarget::NativeProfile{snapshot,..})=self.stream_target.as_ref().map(|target|target.connection.clone()) {
            self.connect_native_profile(snapshot);return;
        }
        if let Some(sessions::ConnectionTarget::NativeInvitation(path))=self.stream_target.as_ref().map(|target|target.connection.clone()) {
            self.connect_native(path);return;
        }
        if let Some(sessions::ConnectionTarget::NativeInternet(path))=self.stream_target.as_ref().map(|target|target.connection.clone()) {
            self.connect_native_internet(path);return;
        }
        if let Some(sessions::ConnectionTarget::NativeDevice(id))=self.stream_target.as_ref().map(|target|target.connection.clone()) {
            self.connect_native_device(id);return;
        }
        if matches!(self.preferences.codec,settings::CodecPreference::Av1|settings::CodecPreference::Pyrowave) {
            self.status="AV1 / PyroWave 仅适用于原生连接；UU 兼容连接请选择自动、H.264 或 HEVC".into();
            return;
        }
        if uu_core::control_wire::CaptureFrameRate::new(self.target_fps).is_none() {self.status="UU 兼容协议支持 5–144 FPS，请调整客户端帧率".into();return;}
        self.displays=displays::Displays::default();self.selected_track=0;
        let Some(i) = self.selected else { return };
        let Some(config) = self.config.clone() else {
            self.status = "配置不可用".into();
            return;
        };
        let device = self.devices[i].device_id.clone();
        self.displays.layout_path = display_layout::DisplayLayout::path(&config, &device);
        if let Some(path) = &self.displays.layout_path {
            match display_layout::DisplayLayout::load(path) {
                Ok(layout) => self.displays.layout = layout,
                Err(error) => self.set_settings_notice(format!("显示器排序读取失败：{error}")),
            }
        }
        let geo = uu_core::config::geo_exit_ip();
        let stats=std::sync::Arc::new(uu_core::media::MediaStats::default());
        let _=stats.decoder_preference.set(self.decoder_kind());
        stats.diagnostics.start([config.state_dir.join("diagnostics")]);
        let _ = stats.output_resolution.set(self.capture_resolution());
        stats.prefer_hevc.store(self.preferences.hevc_preferred(), std::sync::atomic::Ordering::Release);
        stats.diagnostics.record(serde_json::json!({"event":"client","build":self.build_label,"display_mode":self.preferences.display_mode,"fps":self.target_fps}));
        stats.display_mode.store((self.preferences.display_mode==uu_core::screen_control::DisplayMode::ExclusiveVirtual) as u32,std::sync::atomic::Ordering::Release);
        stats.audio.volume.store(self.preferences.volume,std::sync::atomic::Ordering::Relaxed);
        stats.audio.muted.store(self.preferences.muted,std::sync::atomic::Ordering::Relaxed);
        stats.clipboard.available.store(cfg!(any(windows, target_os = "linux")),std::sync::atomic::Ordering::Release);
        stats.clipboard.enabled.store(self.preferences.clipboard && cfg!(any(windows, target_os = "linux")),std::sync::atomic::Ordering::Release);
        let target=match &self.stream_target {
            Some(target) if matches!(target.connection,sessions::ConnectionTarget::UuAssistance(_))=>{
                let Some(request)=self.assistance.clone() else {self.status="远程协助验证码不可用，请从主页重新发起".into();return;};
                uu_core::controller::Target::Assistance(request)
            },
            _=>uu_core::controller::Target::Device(device),
        };
        match uu_core::controller::spawn_target(config, target, geo,stats) {
            Ok(h) => {
                let context = self.context.clone();
                h.frames.enable_planar();
                h.frames.set_waker(move || context.request_repaint());
                h.stats.audio.volume.store(self.preferences.volume,std::sync::atomic::Ordering::Relaxed);
                h.stats.audio.muted.store(self.preferences.muted,std::sync::atomic::Ordering::Relaxed);
                if let Err(error)=clipboard::start_following(h.stats.clipboard.clone(),self.clipboard_window.clone()) {
                    h.stats.clipboard.status(format!("剪贴板线程启动失败：{error}"));h.stats.clipboard.enabled.store(false,std::sync::atomic::Ordering::Release);h.stats.clipboard.available.store(false,std::sync::atomic::Ordering::Release);
                }
                self.controller = Some(h);
                self.window_sizing.borrow_mut().begin_stream();
                if self.active_session && !self.fullscreen {
                    self.context.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
                }
                self.input_capture.request();
                self.context.send_viewport_cmd(egui::ViewportCommand::Focus);
                if self.active_session {self.platform.focus_window();}
                self.status = "连接中…".into();
                // Apply UI settings once CONTROL is ready, in poll_controller.
            }
            Err(e) => self.status = format!("连接失败: {e}"),
        }
    }
}

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.workspace.host.stop();
        self.lan.host.stop();
        self.native_host.stop();
        // Delete custom GL textures while eframe still owns a current context.
        self.textures.clear();
            self.hud_cache=Default::default();
        // Also cover backend shutdown paths that do not deliver CloseRequested.
        if self.audio_save_after.take().is_some(){self.persist_preferences();}
        self.disconnect();
        if let Some(h) = &self.controller {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
            while !h.stopped.load(std::sync::atomic::Ordering::Acquire)
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.update_frame(ctx,frame.info().cpu_usage);
    }
}
impl App {
    fn update_frame(&mut self,ctx:&egui::Context,cpu_usage:Option<f32>) {
        if let Some(result)=eframe::vsync_result(ctx) {
            match result {
                Ok(enabled)=>self.presentation_vsync=enabled,
                Err(error) if self.preferences.vsync!=self.presentation_vsync=> {
                    self.preferences.vsync=self.presentation_vsync;
                    self.settings_draft.vsync=self.presentation_vsync;
                    self.persist_preferences();
                    self.set_settings_notice(format!("垂直同步修改失败：{error}"));
                }
                Err(_)=>{}
            }
        }
        self.workspace_poll(ctx);
        let preview_error = self.preview_writer.error.lock().unwrap().take();
        if let Some(error) = preview_error {
            self.set_settings_notice(error);
        }
        self.native_host.poll(ctx);
        self.lan.set_discovery(self.preferences.lan_discovery);
        self.lan.configure_ports(self.workspace.book.sharing.service_port, self.workspace.book.sharing.discovery_port);
        let lan_active=self.stream_target.is_none()&&!self.compatibility_active();
        self.lan.poll(ctx,lan_active);
        if let Some(seconds) = cpu_usage {
            self.render_total_seconds += seconds;
            self.render_samples += 1;
        }
        if self.render_window.elapsed() >= std::time::Duration::from_millis(500) {
            if self.render_samples > 0 {self.render_ms=self.render_total_seconds*1000.0/self.render_samples as f32;}
            self.render_total_seconds=0.0;self.render_samples=0;
            self.render_window=std::time::Instant::now();
        }
        // Read back native state: maximized and fullscreen are distinct modes.
        if let Some(fullscreen) = ctx.input(|i| i.viewport().fullscreen) {
            self.fullscreen = fullscreen;
        }
        self.window_edge_active = !self.mouse_captured && window::resize_border(ctx, self.fullscreen);
        if self.workspace_tabs.is_none() && ctx.input(|i| i.viewport().close_requested()) && (self.controller.is_some() || self.native_host.running() || self.lan.host.running() || self.workspace.host.running()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.exit_requested = true;
            self.disconnect();
        }
        if self
            .close_at
            .is_some_and(|at| std::time::Instant::now() >= at)
        {
            self.exit_requested = true;
            self.disconnect();
            self.close_at = None;
        }
        self.sessions.reap();
        self.poll_account();
        self.poll(ctx);
        self.poll_controller(ctx);
        #[cfg(windows)]
        {
            let hdr=self.is_native() && self.controller.as_ref().is_some_and(|h|h.stats.native_hdr_response.lock().unwrap().is_some());
            if hdr && eframe::glow_presentation::available_color_for(ctx,ctx.viewport_id()).is_none() {
                if let Some(gl)=self.video_gl.clone(){let _=video_hdr::enable(ctx,gl,self.preferences.vsync);}
            }
            eframe::glow_presentation::set_active(ctx,ctx.viewport_id(),hdr,self.preferences.vsync);
        }
        let video_px = self.textures.get(&self.selected_track)
            .map(|texture| texture.size_vec2());
        let native_geometry=window::native_geometry(ctx);
        if let Some(h)=&self.controller {
            let snapshot=window::Snapshot::new(ctx,native_geometry);
            if self.window_snapshot!=Some(snapshot) {
                h.stats.diagnostics.record(snapshot.report(ctx));
                self.window_snapshot=Some(snapshot);
            }
        }else{self.window_snapshot=None;}
        self.window_sizing.borrow_mut().set_settings_page(self.stream_settings_open || self.workspace.route.page == workspace_ui::Page::Settings);
        self.window_sizing.borrow_mut().update(ctx, self.stream_target.is_some(), video_px,
            self.window_edge_active && ctx.input(|i| i.pointer.primary_pressed()), native_geometry);
        if self.exit_requested { self.native_host.stop(); self.lan.host.stop(); self.workspace.host.stop();}
        if self.workspace_tabs.is_none() && self.exit_requested && self.controller.is_none() && !self.native_host.running() && !self.lan.host.running() && !self.workspace.host.running() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.poll_login(ctx);
        self.autostart_shared_host(ctx);
        if let Some(current) = self.uu_devices.show(ctx, self.config.as_ref(), self.account_token.as_deref()) {
            if current {
                self.login_rx = None;
                let result = self.config.as_ref().ok_or_else(|| anyhow::anyhow!("无法确定登录目录"))
                    .and_then(|config| account::sign_out_matching(&config.at("state.json"), self.account_token.as_deref()));
                match result {
                    Ok(true) => self.apply_account(None),
                    Ok(false) => self.retry_account_read(),
                    Err(error) => self.status = format!("设备已解绑，退出本地账号失败：{error}"),
                }
            }
            else {
                self.rx = None;
                self.devices.clear();
                self.start_load();
            }
        }
        let (events, focused) = ctx.input(|i| (i.events.clone(), i.focused));
        if self.focused != focused {
            if let Some(h) = &self.controller {
                h.stats.diagnostics.record(serde_json::json!({
                    "event":"window_focus", "focused":focused, "fullscreen":self.fullscreen,
                    "keyboard":self.platform.keyboard_diagnostics()
                }));
            }
        }
        if self.focused && !focused {
            self.shortcuts.clear();
            self.release_all_keys();
            for message in self.pointer.release_all(self.input_screen(self.selected_track)) {
                self.send_input(message, 15);
            }
        }
        self.focused = focused;
        self.update_keyboard_capture(ctx);
        if self.platform.capture_overflowed() {
            // The retained queue can contain half of a transition; discard it
            // before the out-of-band release. Preserve the user's control intent.
            self.platform.captured_keys();
            self.recover_input("keyboard_queue_overflow");
            self.update_keyboard_capture(ctx);
        }
        for event in self.platform.captured_keys() {
            match event {
                keyboard_capture::Decision::Remote{vk,pressed}=> {
                    if !pressed || ((self.native_keyboard_capture||self.native_window_keyboard) && self.input_ready()) {self.send_key(vk,pressed);}
                },
                keyboard_capture::Decision::Shortcut(action)=>self.local_shortcut(action,ctx),
                _=>{},
            }
        }
        self.send_modifier_repeats();
        for event in &events {
            if let egui::Event::Key {
                key,
                pressed,
                modifiers,
                repeat,
                physical_key,
                ..
            } = event
            {
                if std::env::var_os("UU_UI_DEBUG").is_some() && matches!(key, egui::Key::S | egui::Key::M | egui::Key::F) {
                    eprintln!("ui shortcut {key:?} pressed={pressed} repeat={repeat} ctrl={} alt={}", modifiers.ctrl, modifiers.alt);
                }
                if self.stream_target.is_none() { continue; }
                match self.shortcuts.key(*key, *pressed, *repeat, *modifiers, true, self.nested_passthrough) {
                    shortcuts::KeyAction::Consumed => continue,
                    shortcuts::KeyAction::Trigger(action) => {
                        self.local_shortcut(action,ctx);
                        continue;
                    }
                    shortcuts::KeyAction::Forward => {}
                }
                // A swallowed native key never generates this window event.
                // Any key that reaches egui still needs forwarding, including
                // keys from another input source or a hook that timed out.
                if !self.native_window_keyboard && self.remote_input_active(ctx) {
                    self.sync_modifiers(*modifiers);
                    if self.is_native() {
                        if let Some(usage)=native_keys::hid_for_egui(physical_key.unwrap_or(*key)){
                            self.send_native(uu_core::native_input::InputCommand::Key{usage,pressed:*pressed});
                        }
                    } else if let Some(vk) = vk_for(physical_key.unwrap_or(*key)) {
                        self.send_key(vk, *pressed);
                    }
                }
            }
        }
        if !self.native_keyboard_capture && !self.native_window_keyboard && self.remote_input_active(ctx) {
            self.sync_modifiers(ctx.input(|i| i.modifiers));
        }
        let elapsed = self.last_fps.elapsed().as_secs_f32();
        if elapsed >= 0.5 {
            for fps in self.fps.values_mut() { *fps = 0.0; }
            for (track, count) in self.frame_count.drain() {
                self.fps.insert(track, count as f32 / elapsed);
            }
            self.last_fps = std::time::Instant::now();
        }
        if self.last_diagnostic.elapsed() >= std::time::Duration::from_secs(5) {
            self.last_diagnostic = std::time::Instant::now();
            if let Some(h) = &self.controller {
                use std::sync::atomic::Ordering::Relaxed;
                let s = &h.stats;
                let mut report = serde_json::json!({"event":"video_stats",
                    "backend":s.backend(),"decoded":s.decoded_total.load(Relaxed),
                    "errors":s.errors.load(Relaxed),"damaged":s.damaged_frames.load(Relaxed),
                    "rx_fps":s.src_fps(),"decode_fps":s.dec_fps(),
                    "render_fps":self.fps.get(&self.selected_track).copied().unwrap_or(0.0),
                    "decode_ms":s.decode_ms(),"render_ms":self.render_ms,
                    "width":s.res().0,"height":s.res().1,
                    "video_render_path":self.textures.get(&self.selected_track).map(|t|t.render_path()),
                    "video_gpu_submit_acquire_release_ms":self.textures.get(&self.selected_track).and_then(|t|t.gpu_timings_ms()),
                    "presentation_vsync":self.presentation_vsync,
                    "continuous_video_repaint":false,
                    "video_mailbox_wait_ms":h.frames.delivery_wait_ms(self.selected_track),
                    "video_mailbox_published_replaced_delivered":h.frames.delivery_counts(self.selected_track),
                    "video_dxgi_shared_export":self.textures.get(&self.selected_track).and_then(|t|t.gpu_shared_export()),
                    "remote_cursor_requested":self.preferences.cursor_mode.remote(),
                    "remote_cursor_active":self.remote_cursor_active,
                    "relative_mouse":self.preferences.relative_mouse,
                    "audio_underruns":s.audio.underruns.load(Relaxed),
                    "audio_queue_ms":s.audio.queue_ms.load(Relaxed),
                    "audio_dropped":s.audio.dropped.load(Relaxed),
                    "audio_receive_decode_us":s.audio.receive_decode_us.load(Relaxed),
                    "waiting_idr":s.waiting_for_keyframe.load(Relaxed),
                    "keyframe_requests":s.keyframe_requests.load(Relaxed),
                    "injected_loss":s.injected_loss.load(Relaxed),"loss_ppm":s.loss_ppm.load(Relaxed),
                    "queue_drops":s.queue_drops.load(Relaxed),
                    "twcc_metadata_drops":s.twcc_metadata_drops.load(Relaxed),
                    "twcc_received_packets":s.twcc_received_packets.load(Relaxed),
                    "decoded_display_skips_all_tracks":s.decoded_display_skips.load(Relaxed),
                    "frame_cadence":s.frame_cadence.lock().unwrap().clone(),
                    "control_rtt_last_min_p50_p95_max_us":*s.rtt_distribution_us.lock().unwrap(),
                    "input_sent":s.input_messages_sent.load(Relaxed),"input_errors":s.input_send_errors.load(Relaxed),
                    "keyboard_repeats_forwarded":self.pressed_vks.forwarded_repeats(),
                    "control_open":s.control_open.load(Relaxed),"keyboard":self.platform.keyboard_diagnostics()});
                report["network_loss"]=serde_json::to_value(s.network_loss.lock().unwrap().as_ref()).unwrap_or_default();
                report["build"]=self.build_label.clone().into();
                report["presentation_swap_interval"]=eframe::vsync_interval(ctx).into();
                report["video_color"]=self.textures.get(&self.selected_track).and_then(|t|t.color_diagnostics()).into();
                let cursor=s.native_cursor.snapshot();
                report["native_cursor"]=serde_json::json!({"supported":s.native_cursor.supported(),"revision":cursor.as_ref().map(|c|c.revision),"shape_id":cursor.as_ref().map(|c|c.shape_id),"position":cursor.as_ref().and_then(|c|c.position),"desktop":cursor.as_ref().map(|c|c.desktop),"shape_size":cursor.as_ref().and_then(|c|c.shape.as_ref()).and_then(|s|s.description.size())});
                report["video_paint_window"]=self.textures.get(&self.selected_track).and_then(|t|t.paint_diagnostics()).into();
                report["native_input_available"]=s.native_input_available.load(Relaxed).into();
                report["input_granted"]=s.capabilities.allows(uu_core::capabilities::Capability::Input).into();
                report["input_requested"]=h.native_input.as_ref().is_some_and(|sender|sender.requested()).into();
                report["input_focus_requested"]=self.input_capture.requested().into();
                report["active_session"]=self.active_session.into();
                report["window_focused"]=ctx.input(|i|i.focused).into();
                report["local_ui_owns_input"]=self.input_conditions(ctx).local_ui.into();
                report["recovery_causes"]=serde_json::json!({"receive_damage":s.recovery_receive_damage.load(Relaxed),"decode_queue":s.recovery_decode_queue.load(Relaxed),"decoder_error":s.recovery_decoder_error.load(Relaxed),"format_change":s.recovery_format_change.load(Relaxed)});
                report["bitrate_kbps"] = s.bitrate_kbps.load(Relaxed).into();
                report["codec"] = s.codec.lock().unwrap().clone().into();
                if let Some(timing) = eframe::viewport_frame_timings(ctx) {
                    let [prepare, clear, update, paint, finish, present] = timing.mean_ms;
                    report["render_frame_window"] = serde_json::json!({
                        "samples":timing.samples,"window_ms":timing.window_ms,
                        "prepare_ms":prepare,"clear_ms":clear,"ui_ms":update,
                        "paint_ms":paint,"viewport_commands_ms":finish,"present_ms":present,
                        "paint_setup_clear_draw_post_ms":timing.mean_paint_ms,
                        "cpu_ms":prepare+clear+update+paint+finish,"cpu_max_ms":timing.cpu_max_ms,
                    });
                    let [textures,state,clip,vertices,indices,texture_bind,draw,callbacks,cleanup]=timing.mean_glow_ms;
                    report["glow_draw_window"] = serde_json::json!({
                        "samples":timing.samples,"window_ms":timing.window_ms,
                        "textures_ms":textures,"state_ms":state,"clip_ms":clip,
                        "vertex_upload_ms":vertices,"index_upload_ms":indices,
                        "texture_bind_ms":texture_bind,"mesh_draw_ms":draw,
                        "callbacks_ms":callbacks,"cleanup_ms":cleanup,
                        "scope":"CPU wall time in GL call groups, including driver waits; not GPU execution or scanout",
                    });
                }
                s.diagnostics.record(report);
            }
        }
        if self.stream_settings_open {
            self.stream_settings_ui(ctx);
        } else if self.stream_target.is_some() {
            self.stream_ui(ctx, &events);
        } else {
            self.home_ui(ctx);
        }
        self.native_ui(ctx);
        self.native_host_ui(ctx);
        if self.stream_target.is_none() {
            self.lan.dialogs(ctx,self.platform.window_id());
            let (service, discovery)=self.lan.host.ports();
            if (service, discovery)!=(self.preferences.lan_service_port,self.preferences.lan_discovery_port) {
                self.preferences.lan_service_port=service;
                self.preferences.lan_discovery_port=discovery;
                self.persist_preferences();
            }
        }
        self.about_ui(ctx);
        self.connection_details_ui(ctx);
        if self.stream_target.is_none(){self.workspace_dialogs(ctx);}
        self.device_pairing_ui(ctx);
        if self.audio_save_after.is_some_and(|at|std::time::Instant::now()>=at) {
            self.audio_save_after=None;self.persist_preferences();
        }
        self.update_keyboard_capture(ctx);
        self.update_pointer_capture(ctx);
        self.update_clipboard(ctx);
        self.platform.show(ctx);
        for event in self.platform.focus_events() {
            if let Some(h) = &self.controller { h.stats.diagnostics.record(event); }
        }
        let idle_poll_ms = if self.controller.is_some() || self.close_at.is_some() { 100 }
            else if self.rx.is_some() || self.login_rx.is_some() { 150 }
            else if self.compatibility_active() { 500 } else { 1000 };
        // Frame and input events wake the UI directly. Extra vblank-paced
        // redraws regressed WGL ownership waits on the production client.
        ctx.request_repaint_after(std::time::Duration::from_millis(idle_poll_ms));
    }
}

fn main() -> eframe::Result {
    uu_core::install_crypto_provider();
    #[cfg(windows)]
    if let Some(result)=service_ui::dispatch() {return result;}
    let stream_target = match sessions::StreamTarget::parse(std::env::args().skip(1)) {
        Ok(target) => target,
        Err(error) => { eprintln!("{error}"); return Ok(()); }
    };
    let assistance=if let Some(sessions::StreamTarget{connection:sessions::ConnectionTarget::UuAssistance(id),..})=&stream_target {
        let request=uu_core::assistance::Request::read(id,std::io::stdin().lock());
        match request {Ok(request)=>Some(request),Err(error)=>{eprintln!("{error}");return Ok(());}}
    }else{None};
    let stream_workspace=if let Some(target)=stream_target.clone() {
        let result=(||->anyhow::Result<_> {
            let launch=stream_workspace::Launch::new(target,assistance.as_ref())?;
            let config=uu_core::Config::from_env()?;
            let bytes=zeroize::Zeroizing::new(serde_json::to_vec(&launch)?);
            Ok((launch,uu_core::local_workspace::join(&config.at("stream-workspace-v1"),&bytes)?))
        })();
        match result {
            Ok((_,uu_core::local_workspace::Join::Attached))=>return Ok(()),
            Ok((launch,uu_core::local_workspace::Join::Owner(owner)))=>Some((launch,owner)),
            Err(error)=>{eprintln!("无法打开串流窗口：{error:#}");return Ok(());}
        }
    }else{None};
    let _launcher=if stream_target.is_none() {
        match single_instance::launcher() {
            Ok(Some(guard))=>Some(guard),
            Ok(None)=>return Ok(()),
            Err(error)=>return Err(eframe::Error::AppCreation(Box::new(error))),
        }
    }else{None};
    let origin = platform::startup_origin(stream_target.is_some());
    let title = match &stream_target {
        Some(target) => format!("{} · {}", target.alias, brand::NAME),
        None => brand::NAME.into(),
    };
    // Vertical sync is on by default (tear-free). `UU_VSYNC=0` disables it
    // (e.g. for very low latency on a tearing-tolerant setup).
    let mut preferences=settings::Settings::path().and_then(|p|settings::Settings::load(&p).ok()).unwrap_or_default();
    if let Some(sessions::StreamTarget { connection:sessions::ConnectionTarget::NativeProfile{snapshot,..},..})=&stream_target {
        if let Ok(launch)=uu_core::device_book::Launch::read(snapshot) {
            if let Some(client)=launch.client {preferences.client=client;}
        }
    }
    let vsync = std::env::var("UU_VSYNC").map(|v| v != "0").unwrap_or(preferences.vsync);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 760.0])
            .with_decorations(false)
            .with_min_inner_size([560.0, 360.0])
            .with_title(title)
            .with_app_id(brand::NAME)
            .with_icon(brand::window_icon())
            .with_visible(stream_workspace.is_none()),
        vsync:vsync && stream_workspace.is_none(),
        centered: !cfg!(windows),
        ..Default::default()
    };
    eframe::run_native(
        brand::NAME,
        options,
        Box::new(move |cc| {
            if let Some((launch,owner))=stream_workspace {Ok(Box::new(stream_workspace::Workspace::new(cc,launch,owner,vsync,origin)))}
            else {Ok(Box::new(App::new(cc,stream_target,origin,vsync,assistance)))}
        }),
    )
}

