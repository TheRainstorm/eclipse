//! UU Remote desktop client.
//!
//! Layout mirrors the official client: a device sidebar on the left, a large
//! remote-screen area on the right, and a status/footer bar.

use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver};

use eframe::egui;
use serde::Deserialize;
use uu_core::controller::{ControllerEvent, ControllerHandle};

mod login;
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
                        d.wallpaper_rgba =
                            Some((rgba.width(), rgba.height(), rgba.into_raw()));
                    }
                }
            }
        }
        Ok(devices)
    })
}

struct App {
    devices: Vec<Device>,
    selected: Option<usize>,
    search: String,
    status: String,
    rx: Option<Receiver<anyhow::Result<Vec<Device>>>>,
    config: Option<uu_core::Config>,
    controller: Option<ControllerHandle>,
    textures: std::collections::HashMap<u32, egui::TextureHandle>,
    latest: std::collections::HashMap<u32, (u32, u32, Vec<u8>)>,
    wallpapers: std::collections::HashMap<usize, egui::TextureHandle>,
    frame_count: std::collections::HashMap<u32, u32>,
    fps: std::collections::HashMap<u32, f32>,
    last_fps: std::time::Instant,
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
    autoconnect_done: bool,
    fullscreen: bool,
    bitrate_idx: usize,
    custom_mbps: u32,
    settings_seq: u64,
}

/// Bitrate/quality presets sent as a CaptureSettingRequest. Values are the
/// official GvPb_VideoQuality enum: 1 fast, 2 general, 3 hd, 4 bluray, 5 auto,
/// 6 custom (uses the custom Mbit/s field).
const BITRATE_PRESETS: &[(&str, u64)] = &[
    ("自动", 5),
    ("快速 1M", 1),
    ("一般 2M", 2),
    ("高清 8M", 3),
    ("超清 20M", 4),
    ("自定义", 6),
];

fn vk_for(key: egui::Key) -> Option<u32> {
    use egui::Key::*;
    Some(match key {
        A => 0x41, B => 0x42, C => 0x43, D => 0x44, E => 0x45, F => 0x46,
        G => 0x47, H => 0x48, I => 0x49, J => 0x4A, K => 0x4B, L => 0x4C,
        M => 0x4D, N => 0x4E, O => 0x4F, P => 0x50, Q => 0x51, R => 0x52,
        S => 0x53, T => 0x54, U => 0x55, V => 0x56, W => 0x57, X => 0x58,
        Y => 0x59, Z => 0x5A,
        Num0 => 0x30, Num1 => 0x31, Num2 => 0x32, Num3 => 0x33, Num4 => 0x34,
        Num5 => 0x35, Num6 => 0x36, Num7 => 0x37, Num8 => 0x38, Num9 => 0x39,
        Enter => 0x0D, Backspace => 0x08, Space => 0x20, Tab => 0x09,
        Escape => 0x1B, ArrowUp => 0x26, ArrowDown => 0x28,
        ArrowLeft => 0x25, ArrowRight => 0x27,
        _ => return None,
    })
}

/// A coloured dot + label for the status bar.
fn status_marker(status: &str) -> (&'static str, egui::Color32) {
    if status.contains("已连接") {
        ("●", egui::Color32::from_rgb(52, 199, 110))
    } else if status.contains("错误") || status.contains("失败") || status.contains("断开") {
        ("●", egui::Color32::from_rgb(230, 90, 90))
    } else if status.contains("连接")
        || status.contains("等待")
        || status.contains("加载")
        || status.contains("中")
    {
        ("●", egui::Color32::from_rgb(230, 180, 70))
    } else {
        ("●", egui::Color32::from_rgb(120, 126, 138))
    }
}

fn setup_style(ctx: &egui::Context) {
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
            fonts.font_data.insert("cjk".into(), egui::FontData::from_owned(data));
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

    let accent = egui::Color32::from_rgb(70, 122, 220);
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = egui::Color32::from_rgb(18, 20, 26);
    visuals.window_fill = egui::Color32::from_rgb(24, 27, 34);
    visuals.extreme_bg_color = egui::Color32::from_rgb(12, 14, 18);
    visuals.faint_bg_color = egui::Color32::from_rgb(30, 34, 42);
    visuals.hyperlink_color = egui::Color32::from_rgb(120, 170, 255);
    visuals.selection.bg_fill = accent;
    visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(24, 27, 34);
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(34, 38, 48);
    visuals.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(34, 38, 48);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(46, 52, 66);
    visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(46, 52, 66);
    visuals.widgets.active.bg_fill = accent;
    visuals.widgets.active.weak_bg_fill = accent;
    for w in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        w.rounding = egui::Rounding::same(8.0);
    }
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size.y = 28.0;
    ctx.set_style(style);
    ctx.set_theme(egui::ThemePreference::Dark);
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_style(&cc.egui_ctx);
        // eframe 0.29 + glow does not always schedule repaints when only a
        // background thread has new data, so drive the UI at ~60 Hz.
        let repaint_ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(16));
            repaint_ctx.request_repaint();
        });
        let config = uu_core::Config::from_env().ok();
        let logged_in = config
            .as_ref()
            .map(|c| {
                uu_core::api::ClientState::load(&c.at("state.json"))
                    .token
                    .is_some()
            })
            .unwrap_or(false);
        let mut app = Self {
            devices: Vec::new(),
            selected: None,
            search: String::new(),
            status: String::new(),
            rx: None,
            config,
            controller: None,
            textures: std::collections::HashMap::new(),
            latest: std::collections::HashMap::new(),
            wallpapers: std::collections::HashMap::new(),
            frame_count: std::collections::HashMap::new(),
            fps: std::collections::HashMap::new(),
            last_fps: std::time::Instant::now(),
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
            hud: std::env::var("UU_HUD").map(|v| v != "0").unwrap_or(false),
            autoconnect_done: false,
            fullscreen: false,
            bitrate_idx: 4,
            custom_mbps: 30,
            settings_seq: 0,
        };
        if logged_in {
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
        let Some(config) = self.config.clone() else { return };
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
                                    [*w as usize, *h as usize], rgba);
                                self.wallpapers.insert(i, ctx.load_texture(
                                    format!("wallpaper-{i}"), img,
                                    egui::TextureOptions::LINEAR));
                            }
                        }
                        self.devices = devices;
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
        let Some(h) = &mut self.controller else { return };
        while let Ok(ev) = h.events.try_recv() {
            match ev {
                ControllerEvent::Status(s) => self.status = s,
                ControllerEvent::Error(e) => self.status = format!("错误: {e}"),
            }
        }
        let mut frames = Vec::new();
        while let Ok(f) = h.frames.try_recv() {
            frames.push(f);
        }
        for (idx, w, hgt, rgba) in frames {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [w as usize, hgt as usize],
                &rgba,
            );
            match self.textures.get_mut(&idx) {
                Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                None => {
                    let t = ctx.load_texture(
                        format!("remote-{idx}"), image, egui::TextureOptions::LINEAR);
                    self.textures.insert(idx, t);
                }
            }
            self.latest.insert(idx, (w, hgt, rgba));
            *self.frame_count.entry(idx).or_insert(0) += 1;
        }
    }

    fn send_input(&mut self, payload: serde_json::Value, kind: u32) {
        self.send_input_ch(payload, kind, 35);
    }

    /// Send the currently selected bitrate/quality preset to the controlled end.
    fn send_bitrate(&mut self) {
        let Some(h) = &self.controller else { return };
        let (_, quality) = BITRATE_PRESETS[self.bitrate_idx];
        let bitrate = if quality == 6 {
            Some(self.custom_mbps as u64)
        } else {
            None
        };
        self.settings_seq = self.settings_seq.wrapping_add(1);
        let msg = uu_core::control_wire::capture_setting_request(
            2000 + self.settings_seq,
            2,
            Some(quality),
            bitrate,
        );
        let _ = h.settings.send(msg);
    }

    fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        self.fullscreen = !self.fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
    }

    /// The controlled end expects raw JSON on `CONTROL_DATA_CHANNEL`; the
    /// `kind`/`channel` arguments are kept only to avoid touching every call
    /// site and are ignored.
    fn send_input_ch(&mut self, payload: serde_json::Value, _kind: u32, _channel: u16) {
        let Some(h) = &self.controller else { return };
        self.seq = self.seq.wrapping_add(1);
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
        self.release_all_keys();
        if let Some(h) = &self.controller {
            let _ = h.close.send(());
        }
        self.controller = None;
        self.textures.clear();
        self.latest.clear();
        self.frame_count.clear();
        self.fps.clear();
        self.status = "已断开".into();
    }

    fn connect(&mut self) {
        let Some(i) = self.selected else { return };
        let Some(config) = self.config.clone() else {
            self.status = "配置不可用".into();
            return;
        };
        let device = self.devices[i].device_id.clone();
        let geo = std::env::var("UU_GEO_IP").unwrap_or_else(|_| "36.159.243.209".into());
        match uu_core::controller::spawn(config, device, geo) {
            Ok(h) => {
                self.controller = Some(h);
                self.status = "连接中…".into();
                for vk in [0x11u32, 0x10, 0x12, 0xA2, 0xA0, 0xA4, 0x5B] {
                    self.send_key(vk, false);
                }
                self.send_bitrate();
            }
            Err(e) => self.status = format!("连接失败: {e}"),
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut keys = Vec::new();
        let mut mods = Vec::new();
        let mut toggle_hud = false;
        let mut toggle_fs = false;
        let focused = ctx.input(|i| {
            for ev in &i.events {
                if let egui::Event::Key { key, pressed, .. } = ev {
                    if matches!(key, egui::Key::F1) && *pressed {
                        toggle_hud = true;
                    }
                    // Ctrl+Alt+F toggles fullscreen; do not forward the F.
                    if matches!(key, egui::Key::F)
                        && *pressed
                        && i.modifiers.ctrl
                        && i.modifiers.alt
                    {
                        toggle_fs = true;
                        continue;
                    }
                    if let Some(vk) = vk_for(*key) {
                        keys.push((vk, *pressed));
                    }
                }
            }
            let want = [
                (0xA2u32, i.modifiers.ctrl, 0usize),
                (0xA0, i.modifiers.shift, 1),
                (0xA4, i.modifiers.alt, 2),
                (0x5B, i.modifiers.command, 3),
            ];
            for (vk, down, idx) in want {
                if self.mod_state[idx] != down {
                    mods.push((vk, down, idx));
                }
            }
            i.focused
        });
        if toggle_hud {
            self.hud = !self.hud;
        }
        if toggle_fs {
            self.toggle_fullscreen(ctx);
        }
        if self.focused && !focused {
            self.release_all_keys();
        }
        self.focused = focused;
        for (vk, pressed) in keys {
            self.send_key(vk, pressed);
        }
        for (vk, down, idx) in mods {
            self.send_key(vk, down);
            self.mod_state[idx] = down;
        }
        let elapsed = self.last_fps.elapsed().as_secs_f32();
        if elapsed >= 1.0 {
            let counts: Vec<(u32, u32)> = self.frame_count.drain().collect();
            for (k, c) in counts {
                self.fps.insert(k, c as f32 / elapsed);
            }
            self.last_fps = std::time::Instant::now();
        }
        self.poll(ctx);
        self.poll_controller(ctx);
        self.poll_login(ctx);

        if !self.logged_in {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(56.0);
                    ui.label(egui::RichText::new("网易 UU 远程").size(30.0).strong());
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("Rust 原生客户端 · 跨平台").weak());
                    ui.add_space(24.0);
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgb(24, 27, 34))
                        .rounding(egui::Rounding::same(14.0))
                        .inner_margin(egui::Margin::same(22.0))
                        .show(ui, |ui| {
                            if let Some(tex) = &self.login_texture {
                                let s = tex.size_vec2();
                                ui.image((tex.id(), s));
                            } else {
                                ui.add_space(240.0);
                            }
                            ui.add_space(14.0);
                            ui.label(egui::RichText::new(&self.login_status).size(15.0));
                            ui.add_space(10.0);
                            if ui.button(egui::RichText::new("刷新二维码").size(15.0)).clicked() {
                                self.start_login();
                            }
                        });
                });
            });
            return;
        }
        ctx.request_repaint();

        let streaming = self.controller.is_some();
        if !self.fullscreen {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if streaming {
                    ui.label(egui::RichText::new("串流").size(13.0).weak());
                } else {
                    ui.label(egui::RichText::new("网易 UU 远程").size(19.0).strong());
                }
                ui.add_space(8.0);
                let (dot, col) = status_marker(&self.status);
                ui.label(egui::RichText::new(dot).color(col).size(12.0));
                ui.label(egui::RichText::new(&self.status).size(13.0).weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("全屏 (Ctrl+Alt+F)").clicked() {
                        self.toggle_fullscreen(ui.ctx());
                    }
                    let mut idx = self.bitrate_idx;
                    egui::ComboBox::from_id_salt("bitrate")
                        .selected_text(BITRATE_PRESETS[idx].0)
                        .show_ui(ui, |ui| {
                            for (i, (label, _)) in BITRATE_PRESETS.iter().enumerate() {
                                ui.selectable_value(&mut idx, i, *label);
                            }
                        });
                    if idx != self.bitrate_idx {
                        self.bitrate_idx = idx;
                        self.send_bitrate();
                    }
                    if BITRATE_PRESETS[self.bitrate_idx].1 == 6 {
                        let r = ui.add(
                            egui::DragValue::new(&mut self.custom_mbps)
                                .range(1..=1000)
                                .suffix(" Mbps"),
                        );
                        if r.changed() {
                            self.send_bitrate();
                        }
                    }
                    if ui.button(if self.hud { "HUD ✓" } else { "HUD" }).clicked() {
                        self.hud = !self.hud;
                    }
                    if streaming && ui.button("断开").clicked() {
                        self.disconnect();
                    }
                    if !self.textures.is_empty() {
                        let mut keys: Vec<u32> = self.textures.keys().copied().collect();
                        keys.sort();
                        egui::ComboBox::from_id_salt("display")
                            .selected_text(format!("显示屏 {}", self.selected_track + 1))
                            .show_ui(ui, |ui| {
                                for k in keys {
                                    ui.selectable_value(
                                        &mut self.selected_track, k,
                                        format!("显示屏 {}", k + 1));
                                }
                            });
                    }
                    if let Some((w, hgt, _)) = self.latest.get(&self.selected_track) {
                        let f = self.fps.get(&self.selected_track).copied().unwrap_or(0.0);
                        ui.label(
                            egui::RichText::new(format!("{w}×{hgt} · {f:.0} fps"))
                                .monospace()
                                .size(13.0)
                                .weak(),
                        );
                    }
                });
            });
            ui.add_space(4.0);
        });

        if !streaming {
        egui::SidePanel::left("devices")
            .resizable(true)
            .default_width(280.0)
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("我的设备").strong().size(16.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("刷新").clicked() {
                            self.start_load();
                        }
                    });
                });
                ui.add_space(4.0);
                ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("搜索设备"));
                ui.add_space(6.0);
                let q = self.search.to_lowercase();
                let items: Vec<usize> = self
                    .devices
                    .iter()
                    .enumerate()
                    .filter(|(_, d)| {
                        q.is_empty()
                            || d.alias.to_lowercase().contains(&q)
                            || d.device_id.contains(&q)
                    })
                    .map(|(i, _)| i)
                    .collect();
                let mut clicked = None;
                for i in items {
                    let d = &self.devices[i];
                    let online = d.status == "CONNECTED" || d.status == "ready";
                    let selected = self.selected == Some(i);
                    let fill = if selected {
                        egui::Color32::from_rgb(38, 58, 102)
                    } else if online {
                        egui::Color32::from_rgb(26, 30, 38)
                    } else {
                        egui::Color32::from_rgb(22, 24, 30)
                    };
                    let frame = egui::Frame::none()
                        .fill(fill)
                        .rounding(egui::Rounding::same(10.0))
                        .inner_margin(egui::Margin::same(8.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let rect = ui.allocate_exact_size(
                                    egui::vec2(42.0, 42.0),
                                    egui::Sense::hover(),
                                );
                                match self.wallpapers.get(&i) {
                                    Some(tex) => {
                                        ui.painter().image(
                                            tex.id(),
                                            rect.0,
                                            egui::Rect::from_min_max(
                                                egui::pos2(0.0, 0.0),
                                                egui::pos2(1.0, 1.0),
                                            ),
                                            egui::Color32::WHITE,
                                        );
                                    }
                                    None => {
                                        ui.painter().rect_filled(
                                            rect.0,
                                            8.0,
                                            egui::Color32::from_rgb(34, 38, 48),
                                        );
                                    }
                                }
                                ui.vertical(|ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(if online { "●" } else { "○" })
                                                .color(if online {
                                                    egui::Color32::from_rgb(52, 199, 110)
                                                } else {
                                                    egui::Color32::from_rgb(120, 126, 138)
                                                })
                                                .size(11.0),
                                        );
                                        ui.label(egui::RichText::new(&d.alias).strong().size(15.0));
                                    });
                                    ui.label(
                                        egui::RichText::new(&d.device_id).small().weak().size(11.0),
                                    );
                                });
                            });
                        });
                    let resp = frame.response.interact(egui::Sense::click());
                    if resp.clicked() {
                        clicked = Some(i);
                    }
                    if resp.hovered() && !selected {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    ui.add_space(4.0);
                }
                if let Some(i) = clicked {
                    self.selected = Some(i);
                }
            });

        egui::TopBottomPanel::bottom("footer").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if let Some(i) = self.selected {
                    let (alias, device_id, controllable) = {
                        let d = &self.devices[i];
                        (d.alias.clone(), d.device_id.clone(), d.controllable)
                    };
                    ui.label(egui::RichText::new(alias).strong());
                    ui.label(egui::RichText::new(device_id).weak().size(12.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.controller.is_some() {
                            if ui
                                .button(egui::RichText::new("断开").size(15.0))
                                .clicked()
                            {
                                self.disconnect();
                            }
                        } else if ui
                            .button(egui::RichText::new("连接").size(15.0).strong())
                            .clicked()
                        {
                            self.connect();
                        }
                        if self.controller.is_none() && !controllable {
                            ui.label(egui::RichText::new("该设备不可被控").weak().size(12.0));
                        }
                    });
                } else {
                    ui.label(egui::RichText::new("选择一台设备开始远控").weak());
                }
            });
            ui.add_space(4.0);
        });
        }
        }

        let mut pending: Vec<(serde_json::Value, u32)> = Vec::new();
        egui::CentralPanel::default().show(ctx, |ui| {
            let rect = ui.available_rect_before_wrap();
            let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
            let chosen = if self.textures.contains_key(&self.selected_track) {
                Some(self.selected_track)
            } else {
                self.textures.keys().next().copied()
            };
            if let Some(track) = chosen {
                let tex = &self.textures[&track];
                let size = tex.size_vec2();
                let scale = (rect.width() / size.x).min(rect.height() / size.y);
                let draw = egui::Rect::from_center_size(rect.center(), size * scale);
                ui.painter().image(
                    tex.id(),
                    draw,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                if let Some(p) = resp.hover_pos() {
                    let x = ((p.x - draw.left()) / draw.width()).clamp(0.0, 1.0);
                    let y = ((p.y - draw.top()) / draw.height()).clamp(0.0, 1.0);
                    pending.push((
                        serde_json::json!({
                            "abs_x": x, "abs_y": y,
                            "action": "mouse_move_absolute", "screen_id": 1
                        }),
                        15,
                    ));
                }
                if resp.drag_started() {
                    pending.push((
                        serde_json::json!({"action":"mouse_press","button":1,"screen_id":1}),
                        15,
                    ));
                }
                if resp.dragged() {
                    if let Some(p) = resp.interact_pointer_pos() {
                        let x = ((p.x - draw.left()) / draw.width()).clamp(0.0, 1.0);
                        let y = ((p.y - draw.top()) / draw.height()).clamp(0.0, 1.0);
                        pending.push((
                            serde_json::json!({
                                "abs_x": x, "abs_y": y,
                                "action": "mouse_move_absolute", "screen_id": 1
                            }),
                            15,
                        ));
                    }
                }
                if resp.drag_stopped() {
                    pending.push((
                        serde_json::json!({"action":"mouse_release","button":1,"screen_id":1}),
                        15,
                    ));
                }
                if resp.clicked() {
                    for action in ["mouse_press", "mouse_release"] {
                        pending.push((
                            serde_json::json!({"action":action,"button":1,"screen_id":1}),
                            15,
                        ));
                    }
                }
                if resp.secondary_clicked() {
                    for action in ["mouse_press", "mouse_release"] {
                        pending.push((
                            serde_json::json!({"action":action,"button":2,"screen_id":1}),
                            15,
                        ));
                    }
                }
                if resp.middle_clicked() {
                    for action in ["mouse_press", "mouse_release"] {
                        pending.push((
                            serde_json::json!({"action":action,"button":3,"screen_id":1}),
                            15,
                        ));
                    }
                }
                if resp.contains_pointer() {
                    let scroll = ui.ctx().input(|i| i.raw_scroll_delta.y);
                    if scroll.abs() > 0.1 {
                        pending.push((
                            serde_json::json!({
                                "action": "mouse_scroll", "delta_x": 0,
                                "delta_y": if scroll > 0.0 { 120 } else { -120 }
                            }),
                            16,
                        ));
                    }
                }
            } else {
                ui.painter()
                    .rect_filled(rect, 0.0, egui::Color32::from_rgb(12, 14, 18));
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "选择设备并连接",
                    egui::FontId::proportional(22.0),
                    egui::Color32::from_rgb(90, 96, 110),
                );
            }
            if self.hud {
                if let Some(h) = &self.controller {
                    let s = &h.stats;
                    let (w, hgt) = s.res();
                    let rfps = self.fps.get(&self.selected_track).copied().unwrap_or(0.0);
                    let lines = [
                        "UU Remote · HUD   (F1 切换)".to_string(),
                        format!("{w}×{hgt}   {}", s.backend()),
                        format!(
                            "src {:.1} fps   dec {:.1} fps   render {:.0} fps",
                            s.src_fps(),
                            s.dec_fps(),
                            rfps
                        ),
                        format!(
                            "bitrate {:.2} Mbps   decode {:.2} ms",
                            s.bitrate_kbps.load(Ordering::Relaxed) as f32 / 1000.0,
                            s.decode_ms()
                        ),
                        format!(
                            "packets {}   lost {}   errors {}",
                            s.packets.load(Ordering::Relaxed),
                            s.lost.load(Ordering::Relaxed),
                            s.errors.load(Ordering::Relaxed)
                        ),
                    ];
                    let font = egui::FontId::monospace(13.0);
                    let line_h = 17.0;
                    let pad = 10.0;
                    let x = rect.left() + 14.0;
                    let y = rect.top() + 14.0;
                    let bg = egui::Rect::from_min_size(
                        egui::pos2(x - pad, y - pad),
                        egui::vec2(420.0, line_h * lines.len() as f32 + pad * 2.0),
                    );
                    ui.painter()
                        .rect_filled(bg, 6.0, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180));
                    for (i, line) in lines.iter().enumerate() {
                        ui.painter().text(
                            egui::pos2(x, y + i as f32 * line_h),
                            egui::Align2::LEFT_TOP,
                            line,
                            font.clone(),
                            egui::Color32::from_rgb(215, 225, 240),
                        );
                    }
                }
            }
        });
        for (payload, kind) in pending {
            self.send_input(payload, kind);
        }
    }
}

fn main() -> eframe::Result {
    uu_core::install_crypto_provider();
    // Vertical sync is on by default (tear-free). `UU_VSYNC=0` disables it
    // (e.g. for very low latency on a tearing-tolerant setup).
    let vsync = std::env::var("UU_VSYNC").map(|v| v != "0").unwrap_or(true);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 680.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title("网易UU远程 · Rust"),
        vsync,
        ..Default::default()
    };
    eframe::run_native(
        "网易UU远程",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
