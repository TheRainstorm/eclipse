//! UU Remote desktop client.
//!
//! Layout mirrors the official client: a device sidebar on the left, a large
//! remote-screen area on the right, and a status/footer bar.

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
}

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

fn setup_fonts(ctx: &egui::Context) {
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
    ctx.set_theme(egui::ThemePreference::Dark);
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_fonts(&cc.egui_ctx);
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
            }
            Err(e) => self.status = format!("连接失败: {e}"),
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut keys = Vec::new();
        let mut mods = Vec::new();
        let focused = ctx.input(|i| {
            for ev in &i.events {
                if let egui::Event::Key { key, pressed, .. } = ev {
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
                    ui.add_space(60.0);
                    ui.heading("网易UU远程");
                    ui.label("Rust 原生客户端 · 扫码登录");
                    ui.add_space(20.0);
                    if let Some(tex) = &self.login_texture {
                        let s = tex.size_vec2();
                        ui.image((tex.id(), s));
                    } else {
                        ui.add_space(220.0);
                    }
                    ui.add_space(12.0);
                    ui.label(&self.login_status);
                    ui.add_space(12.0);
                    if ui.button("刷新二维码").clicked() {
                        self.start_login();
                    }
                });
            });
            return;
        }
        ctx.request_repaint();

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new("网易UU远程").strong());
                ui.label(egui::RichText::new("Rust 原生客户端").weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some((w, hgt, _)) = self.latest.get(&self.selected_track) {
                        let f = self.fps.get(&self.selected_track).copied().unwrap_or(0.0);
                        ui.label(format!("{w}x{hgt} · {f:.0}fps"));
                    }
                    ui.label(&self.status);
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
                    if ui.button("全屏").clicked() {
                        let full = ui.ctx().input(|i| i.viewport().fullscreen.unwrap_or(false));
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
                    }
                });
            });
        });

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
                    let text = format!("{}  {}", if online { "●" } else { "○" }, d.alias);
                    let color = if online {
                        egui::Color32::from_rgb(34, 197, 94)
                    } else {
                        egui::Color32::from_rgb(120, 120, 120)
                    };
                    ui.horizontal(|ui| {
                        if let Some(tex) = self.wallpapers.get(&i) {
                            ui.image((tex.id(), egui::vec2(36.0, 36.0)));
                        }
                        let resp = ui.add_sized(
                            [ui.available_width(), 36.0],
                            egui::Button::new(egui::RichText::new(text).color(color))
                                .selected(selected),
                        );
                        if resp.clicked() {
                            clicked = Some(i);
                        }
                    });
                    ui.label(egui::RichText::new(&d.device_id).small().weak());
                    ui.add_space(2.0);
                }
                if let Some(i) = clicked {
                    self.selected = Some(i);
                }
            });

        egui::TopBottomPanel::bottom("footer").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(i) = self.selected {
                    let d = &self.devices[i];
                    ui.label(format!("{}  ({})", d.alias, d.device_id));
                    if self.controller.is_some() {
                        if ui.button("断开").clicked() {
                            self.disconnect();
                        }
                    } else if ui.button("连接").clicked() {
                        let idx = i;
                        self.selected = Some(idx);
                        self.connect();
                    }
                } else {
                    ui.label("选择一台设备开始远控");
                }
            });
        });

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
        });
        for (payload, kind) in pending {
            self.send_input(payload, kind);
        }
    }
}

fn main() -> eframe::Result {
    uu_core::install_crypto_provider();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("网易UU远程"),
        ..Default::default()
    };
    eframe::run_native(
        "网易UU远程",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
