//! UU Remote desktop client.
//!
//! Layout mirrors the official client: a device sidebar on the left, a large
//! remote-screen area on the right, and a status/footer bar.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::sync::mpsc::{channel, Receiver};

use eframe::egui;
use serde::Deserialize;
use uu_core::controller::{ControllerEvent, ControllerHandle};

mod input;
mod clipboard;
mod cursor;
mod keyboard_capture;
mod settings;
mod sessions;
mod platform;
mod shortcuts;
mod hud;
mod window;
mod stream_menu;
mod login;
mod ui;
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
    #[serde(skip)]
    wallpaper_rgba: Option<(u32, u32, Vec<u8>)>,
}

#[derive(Debug, Deserialize)]
struct DeviceListData {
    #[serde(default)]
    my_binded_devices: Vec<Device>,
}

#[derive(Debug, Deserialize)]
struct DeviceListResp {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    data: Option<DeviceListData>,
}

fn load_devices() -> anyhow::Result<Vec<Device>> {
    let config = uu_core::Config::from_env()?;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let client = uu_core::SignalClient::new(&config)?;
        let api = uu_core::UuApi::new(client, config.at("state.json"));
        let body = api.device_list().await?;
        let resp: DeviceListResp = serde_json::from_value(body)?;
        anyhow::ensure!(resp.code == 0, "device list code {}", resp.code);
        let mut devices = resp.data.map(|d| d.my_binded_devices).unwrap_or_default();
        for d in &mut devices {
            if d.wallpaper_url.is_empty() {
                continue;
            }
            if let Ok(r) = reqwest::get(&d.wallpaper_url).await {
                if let Ok(bytes) = r.bytes().await {
                    if let Ok(img) = image::load_from_memory(&bytes) {
                        let rgba = img.to_rgba8();
                        d.wallpaper_rgba = Some((rgba.width(), rgba.height(), rgba.into_raw()));
                    }
                }
            }
        }
        Ok(devices)
    })
}

struct App {
    stream_target: Option<sessions::StreamTarget>,
    sessions: sessions::Sessions,
    platform: platform::Platform,
    build_label: String,
    executable_path: String,
    context: egui::Context,
    devices: Vec<Device>,
    selected: Option<usize>,
    search: String,
    status: String,
    rx: Option<Receiver<anyhow::Result<Vec<Device>>>>,
    config: Option<uu_core::Config>,
    controller: Option<ControllerHandle>,
    textures: std::collections::HashMap<u32, egui::TextureHandle>,
    wallpapers: std::collections::HashMap<usize, egui::TextureHandle>,
    frame_count: std::collections::HashMap<u32, u32>,
    fps: std::collections::HashMap<u32, f32>,
    last_fps: std::time::Instant,
    unpresented: std::collections::HashSet<u32>,
    render_ms: f32,
    selected_track: u32,
    seq: u32,
    kseq: u32,
    pressed_vks: std::collections::HashSet<u32>,
    mod_state: [bool; 4],
    focused: bool,
    logged_in: bool,
    login_rx: Option<Receiver<LoginEvent>>,
    login_texture: Option<egui::TextureHandle>,
    login_status: String,
    hud: bool,
    preferences: settings::Settings,
    settings_open: bool,
    settings_draft: settings::Settings,
    settings_notice: String,
    shortcuts: shortcuts::Shortcuts,
    native_keyboard_capture: bool,
    autoconnect_done: bool,
    fullscreen: bool,
    restore_maximized: bool,
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
    remote_focus: bool,
    view_only: bool,
    stream_menu: stream_menu::StreamMenu,
    window_sizing: window::WindowSizing,
    close_at: Option<std::time::Instant>,
    disconnecting: bool,
    exit_requested: bool,
    window_edge_active: bool,
}

/// Match the supplied official menu. Explicit rates use quality 6 and are
/// encoded as bits/second; automatic quality 5 remains controlled by the host.
const BITRATE_PRESETS: &[(&str, u64, Option<u32>)] = &[
    ("自动（原画）", 5, None),
    ("原画 30M", 6, Some(30)),
    ("超清 14M", 6, Some(14)),
    ("高清 8M", 6, Some(8)),
    ("自定义", 6, None),
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
        _ => return None,
    })
}

fn setup_style(ctx: &egui::Context) {
    // Remote Ctrl +/- belongs to the remote app, never to this client's UI zoom.
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
    let mut fonts = egui::FontDefinitions::default();
    let candidates: Vec<&str> = if cfg!(target_os = "windows") {
        vec![
            r"C:\Windows\Fonts\msyh.ttc",
            r"C:\Windows\Fonts\simhei.ttf",
            r"C:\Windows\Fonts\simsun.ttc",
        ]
    } else {
        vec![
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
            "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        ]
    };
    for path in candidates {
        if let Ok(data) = std::fs::read(path) {
            fonts
                .font_data
                .insert("cjk".into(), egui::FontData::from_owned(data));
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

    let mut visuals = egui::Visuals::light();
    visuals.panel_fill = egui::Color32::from_rgb(247, 250, 252);
    visuals.window_fill = egui::Color32::WHITE;
    visuals.extreme_bg_color = egui::Color32::WHITE;
    visuals.faint_bg_color = egui::Color32::from_rgb(238, 243, 247);
    visuals.selection.bg_fill = egui::Color32::from_rgb(53, 131, 255);
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(238, 243, 247);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(225, 235, 247);
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(53, 131, 255);
    visuals.widgets.noninteractive.fg_stroke.color = egui::Color32::from_rgb(25, 31, 38);
    visuals.window_rounding = egui::Rounding::same(12.0);
    ctx.set_visuals(visuals);
    let mut style = (*ctx.style()).clone();
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.spacing.interact_size.y = 30.0;
    ctx.set_style(style);
    ctx.set_theme(egui::ThemePreference::Light);
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, stream_target: Option<sessions::StreamTarget>, origin: Option<(i32, i32)>) -> Self {
        setup_style(&cc.egui_ctx);
        let platform = platform::Platform::new(cc, origin);
        let config = uu_core::Config::from_env().ok();
        let logged_in = config
            .as_ref()
            .map(|c| {
                uu_core::api::ClientState::load(&c.at("state.json"))
                    .token
                    .is_some()
            })
            .unwrap_or(false);
        let (preferences, settings_notice) = match settings::Settings::path().map(|p|settings::Settings::load(&p)) {
            Some(Ok(s))=>(s,String::new()),Some(Err(e))=>(settings::Settings::default(),format!("读取设置失败：{e}")),None=>(settings::Settings::default(),String::new()),
        };
        let mut app = Self {
            stream_target,
            sessions: sessions::Sessions::default(),
            platform,
            build_label: installed_revision(),
            executable_path: std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default(),
            context: cc.egui_ctx.clone(),
            devices: Vec::new(),
            selected: None,
            search: String::new(),
            status: String::new(),
            rx: None,
            config,
            controller: None,
            textures: std::collections::HashMap::new(),
            wallpapers: std::collections::HashMap::new(),
            frame_count: std::collections::HashMap::new(),
            fps: std::collections::HashMap::new(),
            last_fps: std::time::Instant::now(),
            unpresented: Default::default(),
            render_ms: 0.0,
            selected_track: 0,
            seq: 1,
            kseq: 1,
            pressed_vks: std::collections::HashSet::new(),
            mod_state: [false; 4],
            focused: true,
            logged_in,
            login_rx: None,
            login_texture: None,
            login_status: String::new(),
            hud: std::env::var("UU_HUD").map(|v| v != "0").unwrap_or(preferences.hud),
            preferences: preferences.clone(),
            settings_open: false,
            settings_draft: preferences.clone(),
            settings_notice,
            shortcuts: shortcuts::Shortcuts::default(),
            native_keyboard_capture: false,
            autoconnect_done: false,
            fullscreen: false,
            restore_maximized: false,
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
            remote_focus: false,
            view_only: false,
            stream_menu: stream_menu::StreamMenu {visible:preferences.floating_menu,..Default::default()},
            window_sizing: window::WindowSizing::default(),
            close_at: None,
            disconnecting: false,
            exit_requested: false,
            window_edge_active: false,
        };
        if let Some(target) = app.stream_target.clone() {
            app.devices.push(Device {
                alias: target.alias, device_id: target.device_id,
                status: "CONNECTED".into(), controllable: true,
                wallpaper_url: String::new(), platform: 0, wallpaper_rgba: None,
            });
            app.selected = Some(0);
            app.view_only = target.view_only;
            app.autoconnect_done = true;
            app.connect();
        } else if logged_in {
            app.start_load();
        } else {
            app.status = "请扫码登录".into();
        }
        app
    }

    fn start_load(&mut self) {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(load_devices());
        });
        self.rx = Some(rx);
        self.status = "正在加载设备…".to_string();
    }

    fn start_login(&mut self) {
        let Some(config) = self.config.clone() else {
            return;
        };
        self.login_status = "获取二维码…".into();
        self.login_rx = Some(login::spawn(config));
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
                LoginEvent::Done => {
                    self.logged_in = true;
                    self.login_rx = None;
                    self.start_load();
                }
                LoginEvent::Error(e) => self.login_status = format!("登录失败: {e}"),
            }
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.rx {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok(devices) => {
                        self.status = format!("已加载 {} 台设备", devices.len());
                        self.wallpapers.clear();
                        for (i, d) in devices.iter().enumerate() {
                            if let Some((w, h, rgba)) = &d.wallpaper_rgba {
                                let img = egui::ColorImage::from_rgba_unmultiplied(
                                    [*w as usize, *h as usize],
                                    rgba,
                                );
                                self.wallpapers.insert(
                                    i,
                                    ctx.load_texture(
                                        format!("wallpaper-{i}"),
                                        img,
                                        egui::TextureOptions::LINEAR,
                                    ),
                                );
                            }
                        }
                        self.devices = devices;
                        self.selected = self
                            .devices
                            .iter()
                            .position(|d| d.alias == "KVM-WIN11")
                            .or_else(|| (!self.devices.is_empty()).then_some(0));
                        // Optional headless auto-connect for testing:
                        //   UU_AUTOCONNECT=<explicit device_id|alias>
                        if !self.autoconnect_done {
                            self.autoconnect_done = true;
                            // Only an explicit device id or alias is accepted —
                            // never "first online" (that once picked the wrong
                            // machine). Testing uses KVM-WIN11 explicitly.
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
                    Err(e) => self.status = format!("加载失败: {e}"),
                }
                self.rx = None;
            }
        }
    }

    fn poll_controller(&mut self, ctx: &egui::Context) {
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
        let mut frames = Vec::new();
        while let Ok(f) = h.frames.try_recv() {
            frames.push(f);
        }
        for (idx, w, hgt, rgba) in frames {
            let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, hgt as usize], &rgba);
            match self.textures.get_mut(&idx) {
                Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                None => {
                    let t = ctx.load_texture(
                        format!("remote-{idx}"),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.textures.insert(idx, t);
                }
            }
            self.unpresented.insert(idx);
        }
        if let Some(h) = &self.controller {
            let response = h.stats.settings_response.load(std::sync::atomic::Ordering::Acquire);
            if response >> 1 == 2000 + self.settings_seq && self.settings_feedback.starts_with("已发送") {
                if response & 1 != 0 {self.remote_cursor_active=self.preferences.remote_cursor;}
                self.settings_feedback = if response & 1 != 0 {
                    format!("远端已确认请求：{} FPS · {}", self.target_fps, BITRATE_PRESETS[self.bitrate_idx].0)
                } else { "远端拒绝了画质设置，请重新选择".into() };
            }
        }
        if apply_settings {
            self.send_capture_settings();
        }
        if self.controller.as_ref().is_some_and(|h| h.stopped.load(std::sync::atomic::Ordering::Acquire)) {
            if let Some(h)=&self.controller {h.stats.audio.stop();h.stats.clipboard.stopped.store(true,std::sync::atomic::Ordering::Release);}
            self.controller = None;
            self.disconnecting = false;
            self.remote_focus = false;
            self.textures.clear();
            self.frame_count.clear();
            self.fps.clear();
            self.settings_feedback.clear();
            self.cursor=cursor::Cursor::default();self.remote_cursor_active=false;
        }
    }

    fn send_input(&mut self, payload: serde_json::Value, kind: u32) {
        self.send_input_ch(payload, kind, 35);
    }

    fn input_screen(&self, track: u32) -> u32 {
        self.controller.as_ref().and_then(|h|
            h.stats.remote_screens.lock().unwrap().for_track(track).map(|s| s.id))
            .filter(|id| *id >= 0).map(|id| id as u32).unwrap_or(track + 1)
    }
    fn input_ready(&self) -> bool {
        !self.disconnecting && self.controller.as_ref().is_some_and(|h|
            h.stats.control_open.load(std::sync::atomic::Ordering::Acquire) != 0)
    }

    /// Apply frame rate, quality and resolution together so another setting never resets FPS.
    fn send_capture_settings(&mut self) {
        let Some(h) = &self.controller else { return };
        let (_, quality, preset_mbps) = BITRATE_PRESETS[self.bitrate_idx];
        let custom_bps = if quality == 6 {
            Some(preset_mbps.unwrap_or(self.custom_mbps) as u64 * 1_000_000)
        } else {
            None
        };
        self.settings_seq = self.settings_seq.wrapping_add(1);
        let resolution = match self.resolution_idx {
            1=>uu_core::control_wire::CaptureResolution::Exact(1920,1080),
            2=>uu_core::control_wire::CaptureResolution::Exact(2560,1440),
            3=>uu_core::control_wire::CaptureResolution::Exact(3840,2160),
            _=>uu_core::control_wire::CaptureResolution::Native,
        };
        let msg = uu_core::control_wire::capture_setting_request_with_cursor(
            2000 + self.settings_seq,
            uu_core::control_wire::now_ms(),
            uu_core::control_wire::CaptureFrameRate::new(self.target_fps).unwrap_or_default(),
            quality,
            custom_bps,
            resolution,
            self.preferences.remote_cursor,
        );
        self.settings_feedback = if h.settings.send(msg).is_ok() {
            format!("已发送：{} FPS · {} · {}", self.target_fps, BITRATE_PRESETS[self.bitrate_idx].0,
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
            shortcuts::Action::Menu=>{self.remote_focus=false;self.release_pointer_capture(ctx);self.stream_menu.visible=!self.stream_menu.visible;self.stream_menu.open=false;ctx.memory_mut(|m|m.close_popup());self.persist_preferences();},
            shortcuts::Action::Fullscreen=>self.toggle_fullscreen(ctx),
            shortcuts::Action::Exit=>self.disconnect(),
            shortcuts::Action::Capture=>{self.remote_focus=!self.remote_focus;},
        }
    }
    fn update_keyboard_capture(&mut self,ctx:&egui::Context) {
        let desired=self.fullscreen && self.preferences.capture_keyboard && self.input_ready()
            && self.remote_focus && !self.view_only && !self.stream_menu.open && !self.settings_open && ctx.input(|i|i.focused);
        let active=self.platform.capture_keyboard(desired);
        if self.native_keyboard_capture!=active {
            self.release_all_keys();self.native_keyboard_capture=active;
        }
    }

    fn release_pointer_capture(&mut self,ctx:&egui::Context) {
        for message in self.pointer.release_all(self.input_screen(self.selected_track)){self.send_input(message,15);}
        if self.mouse_captured {
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::viewport::CursorGrab::None));
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(true));self.mouse_captured=false;
        }
    }
    fn update_pointer_capture(&mut self,ctx:&egui::Context) {
        let capture=self.preferences.relative_mouse && self.remote_focus && self.input_ready() && self.focused
            && !self.view_only && !self.stream_menu.open && !self.settings_open && !self.textures.is_empty();
        if capture!=self.mouse_captured {
            self.release_pointer_capture(ctx);
            if capture {
                // Windows supports confinement; raw WM_INPUT deltas remain
                // unbounded even when its invisible cursor reaches the edge.
                ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::viewport::CursorGrab::Confined));
                ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(false));self.mouse_captured=true;
            }
        }
        if self.mouse_captured {ctx.set_cursor_icon(egui::CursorIcon::None);}
    }

    fn update_clipboard(&mut self,ctx:&egui::Context) {
        let ready=self.input_ready();
        if let Some(h)=&self.controller {
            use std::sync::atomic::Ordering::{Acquire,Release};
            let clipboard=&h.stats.clipboard;
            clipboard.enabled.store(self.preferences.clipboard&&clipboard.available.load(Acquire),Release);
            clipboard.read_only.store(self.view_only,Release);
            let active=ready&&ctx.input(|i|i.focused)&&!self.disconnecting;
            let previous=clipboard.active.swap(active,Release);
            if !active || !clipboard.enabled.load(Acquire) {
                clipboard.local.lock().unwrap().take();clipboard.remote.lock().unwrap().take();
                if previous {*clipboard.protocol.lock().unwrap()=Default::default();}
            }
        }
    }

    fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        self.release_all_keys();
        self.fullscreen = !self.fullscreen;
        self.stream_menu.open = false;
        ctx.memory_mut(|m| m.close_popup());
        if self.fullscreen {
            // Windows keeps WS_MAXIMIZE when entering borderless fullscreen.
            // Restore first so its client area can cover the entire monitor.
            self.restore_maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            if self.restore_maximized {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            if self.restore_maximized {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
            self.restore_maximized = false;
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

    /// The controlled end expects raw JSON on `CONTROL_DATA_CHANNEL`; the
    /// `kind`/`channel` arguments are kept only to avoid touching every call
    /// site and are ignored.
    fn send_input_ch(&mut self, payload: serde_json::Value, _kind: u32, _channel: u16) {
        if self.view_only && !matches!(payload["action"].as_str(), Some("kbd_release" | "mouse_release")) {
            return;
        }
        if !self.input_ready() && !matches!(payload["action"].as_str(), Some("kbd_release" | "mouse_release")) {
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

    fn send_key(&mut self, vk: u32, pressed: bool) {
        if pressed {
            self.pressed_vks.insert(vk);
        } else {
            self.pressed_vks.remove(&vk);
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
            (0x5B, modifiers.mac_cmd, 3),
        ] {
            if self.mod_state[index] != down {
                self.send_key(vk, down);
                self.mod_state[index] = down;
            }
        }
    }

    fn release_all_keys(&mut self) {
        let keys: Vec<u32> = self.pressed_vks.drain().collect();
        for vk in keys {
            self.send_key(vk, false);
        }
        // Clear modifier state on the remote side as well; a lost release
        // leaves the controlled desktop thinking Alt/Ctrl is held down.
        for vk in [0x11u32, 0x10, 0x12, 0xA2, 0xA0, 0xA4, 0x5B] {
            self.send_key(vk, false);
        }
        self.mod_state = [false; 4];
    }

    fn disconnect(&mut self) {
        if self.stream_target.is_some() { self.exit_requested = true; }
        if self.disconnecting { return; }
        self.release_pointer_capture(&self.context.clone());
        self.stream_menu.open = false;
        self.context.memory_mut(|m| m.close_popup());
        self.release_all_keys();
        for message in self.pointer.release_all(self.input_screen(self.selected_track)) {
            self.send_input(message, 15);
        }
        self.remote_focus = false;
        if let Some(h) = &self.controller {
            let _ = h.close.send(());
            self.disconnecting = true;
        }
        self.textures.clear();
        self.frame_count.clear();
        self.fps.clear();
        self.status = if self.disconnecting { "正在释放连接…" } else { "已断开" }.into();
    }

    fn open_stream_window(&mut self) {
        let Some(device) = self.selected.and_then(|i| self.devices.get(i)) else { return };
        let target = sessions::StreamTarget {
            device_id: device.device_id.clone(), alias: device.alias.clone(), view_only: self.view_only,
        };
        let result = std::env::current_exe().and_then(|exe| self.sessions.open(&exe, &target, self.platform.origin()));
        self.status = match result {
            Ok(true) => format!("已打开 {} 的串流窗口，可继续选择其他设备", target.alias),
            Ok(false) => format!("{} 的串流窗口已打开", target.alias),
            Err(error) => format!("无法打开串流窗口：{error}"),
        };
    }

    fn connect(&mut self) {
        if self.stream_target.is_none() {
            self.open_stream_window();
            return;
        }
        if self.controller.is_some() { return; }
        let Some(i) = self.selected else { return };
        let Some(config) = self.config.clone() else {
            self.status = "配置不可用".into();
            return;
        };
        let device = self.devices[i].device_id.clone();
        let geo = std::env::var("UU_GEO_IP").unwrap_or_else(|_| "36.159.243.209".into());
        let stats=std::sync::Arc::new(uu_core::media::MediaStats::default());
        stats.audio.volume.store(self.preferences.volume,std::sync::atomic::Ordering::Relaxed);
        stats.audio.muted.store(self.preferences.muted,std::sync::atomic::Ordering::Relaxed);
        stats.clipboard.available.store(cfg!(windows),std::sync::atomic::Ordering::Release);
        stats.clipboard.enabled.store(self.preferences.clipboard && cfg!(windows),std::sync::atomic::Ordering::Release);
        match uu_core::controller::spawn_with_stats(config, device, geo,stats) {
            Ok(h) => {
                let context = self.context.clone();
                h.frames.set_waker(move || context.request_repaint());
                h.stats.audio.volume.store(self.preferences.volume,std::sync::atomic::Ordering::Relaxed);
                h.stats.audio.muted.store(self.preferences.muted,std::sync::atomic::Ordering::Relaxed);
                if let Err(error)=clipboard::start(h.stats.clipboard.clone(),self.platform.window_id()) {
                    h.stats.clipboard.status(format!("剪贴板线程启动失败：{error}"));h.stats.clipboard.enabled.store(false,std::sync::atomic::Ordering::Release);h.stats.clipboard.available.store(false,std::sync::atomic::Ordering::Release);
                }
                self.controller = Some(h);
                self.window_sizing.begin_stream();
                if !self.fullscreen {
                    self.context.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
                }
                self.remote_focus = true;
                self.status = "连接中…".into();
                for vk in [0x11u32, 0x10, 0x12, 0xA2, 0xA0, 0xA4, 0x5B] {
                    self.send_key(vk, false);
                }
                // Apply UI settings once CONTROL is ready, in poll_controller.
            }
            Err(e) => self.status = format!("连接失败: {e}"),
        }
    }
}

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Also cover backend shutdown paths that do not deliver CloseRequested.
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
        if let Some(seconds) = frame.info().cpu_usage {
            self.render_ms = self.render_ms * 0.9 + seconds * 1000.0 * 0.1;
        }
        // Read back native state: maximized and fullscreen are distinct modes.
        if let Some(fullscreen) = ctx.input(|i| i.viewport().fullscreen) {
            self.fullscreen = fullscreen;
        }
        if self.platform.move_monitors(self.fullscreen) {
            self.release_all_keys();
            for message in self.pointer.release_all(self.input_screen(self.selected_track)) {
                self.send_input(message, 15);
            }
            self.remote_focus = false;
            self.shortcuts.clear();
        }
        self.window_edge_active = !self.mouse_captured && window::resize_border(ctx, self.fullscreen);
        if ctx.input(|i| i.viewport().close_requested()) && self.controller.is_some() {
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
        self.poll(ctx);
        self.poll_controller(ctx);
        let video_px = self.textures.get(&self.selected_track).or_else(|| self.textures.values().next())
            .map(|texture| texture.size_vec2());
        self.window_sizing.update(ctx, self.stream_target.is_some(), video_px,
            self.window_edge_active && ctx.input(|i| i.pointer.primary_pressed()), window::native_geometry(frame, ctx));
        if self.exit_requested && self.controller.is_none() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.poll_login(ctx);
        let (events, focused) = ctx.input(|i| (i.events.clone(), i.focused));
        if self.focused && !focused {
            self.shortcuts.clear();
            self.release_all_keys();
            for message in self.pointer.release_all(self.input_screen(self.selected_track)) {
                self.send_input(message, 15);
            }
            self.remote_focus = false;
        }
        self.focused = focused;
        self.update_keyboard_capture(ctx);
        if self.platform.capture_overflowed() {self.release_all_keys();self.remote_focus=false;self.update_keyboard_capture(ctx);}
        for event in self.platform.captured_keys() {
            match event {
                keyboard_capture::Decision::Remote{vk,pressed}=> {
                    if !pressed || (self.native_keyboard_capture && self.input_ready()) {self.send_key(vk,pressed);}
                },
                keyboard_capture::Decision::Shortcut(action)=>self.local_shortcut(action,ctx),
                _=>{},
            }
        }
        for event in &events {
            if let egui::Event::Key {
                key,
                pressed,
                modifiers,
                repeat,
                ..
            } = event
            {
                if std::env::var_os("UU_UI_DEBUG").is_some() && matches!(key, egui::Key::S | egui::Key::M | egui::Key::F) {
                    eprintln!("ui shortcut {key:?} pressed={pressed} repeat={repeat} ctrl={} alt={}", modifiers.ctrl, modifiers.alt);
                }
                if self.stream_target.is_none() { continue; }
                match self.shortcuts.key(*key, *pressed, *repeat, *modifiers, true) {
                    shortcuts::KeyAction::Consumed => continue,
                    shortcuts::KeyAction::Trigger(action) => {
                        self.local_shortcut(action,ctx);
                        continue;
                    }
                    shortcuts::KeyAction::Forward => {}
                }
                if !self.native_keyboard_capture && self.remote_focus && !self.view_only && !self.stream_menu.open && !self.settings_open {
                    self.sync_modifiers(*modifiers);
                    if let Some(vk) = vk_for(*key) {
                        self.send_key(vk, *pressed);
                    }
                }
            }
        }
        if !self.native_keyboard_capture && self.remote_focus && !self.view_only && !self.stream_menu.open && !self.settings_open {
            self.sync_modifiers(ctx.input(|i| i.modifiers));
        }
        let elapsed = self.last_fps.elapsed().as_secs_f32();
        if elapsed >= 1.0 {
            for fps in self.fps.values_mut() { *fps = 0.0; }
            for (track, count) in self.frame_count.drain() {
                self.fps.insert(track, count as f32 / elapsed);
            }
            self.last_fps = std::time::Instant::now();
        }
        if self.stream_target.is_some() {
            self.stream_ui(ctx, &events);
        } else {
            self.home_ui(ctx);
        }
        self.settings_ui(ctx);
        self.update_keyboard_capture(ctx);
        self.update_pointer_capture(ctx);
        self.update_clipboard(ctx);
        self.platform.show(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(
            if self.controller.is_some() { 100 } else { 150 },
        ));
    }
}

fn main() -> eframe::Result {
    uu_core::install_crypto_provider();
    let stream_target = match sessions::StreamTarget::parse(std::env::args().skip(1)) {
        Ok(target) => target,
        Err(error) => { eprintln!("{error}"); return Ok(()); }
    };
    let origin = platform::startup_origin(stream_target.is_some());
    let title = match &stream_target {
        Some(target) => format!("{} · UU串流 · {}", target.alias, installed_revision()),
        None => format!("网易UU远程 · Rust · {}", installed_revision()),
    };
    // Vertical sync is on by default (tear-free). `UU_VSYNC=0` disables it
    // (e.g. for very low latency on a tearing-tolerant setup).
    let preferences=settings::Settings::path().and_then(|p|settings::Settings::load(&p).ok()).unwrap_or_default();
    if std::env::var_os("UU_DECODER").is_none() && preferences.software_decoder {std::env::set_var("UU_DECODER","sw");}
    let vsync = std::env::var("UU_VSYNC").map(|v| v != "0").unwrap_or(preferences.vsync);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 760.0])
            .with_decorations(false)
            .with_min_inner_size([560.0, 360.0])
            .with_title(title)
            .with_visible(!cfg!(windows)),
        vsync,
        centered: !cfg!(windows),
        ..Default::default()
    };
    eframe::run_native(
        "网易UU远程",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, stream_target, origin)))),
    )
}

fn installed_revision() -> String {
    std::env::current_exe().ok()
        .and_then(|path| std::fs::read(path.parent()?.join("version.json")).ok())
        .and_then(|data| serde_json::from_slice::<serde_json::Value>(&data).ok())
        .and_then(|manifest| manifest["source_commit"].as_str().map(|s| s.chars().take(12).collect()))
        .unwrap_or_else(|| "开发构建".into())
}

