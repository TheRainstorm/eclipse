//! Native layout based on the supplied UU desktop reference, with a separate
//! uncluttered stream view. Application branding uses the embedded Eclipse icon.
use super::App;
use eframe::egui::{self, Color32 as C, Pos2, Rect, Response, RichText, Sense, Stroke, Vec2};
const BLUE: C = C::from_rgb(53, 131, 255);
const INK: C = C::from_rgb(24, 29, 35);
const MUTED: C = C::from_rgb(115, 124, 135);
const SIDEBAR: C = C::from_rgb(240, 245, 248);

#[derive(Clone, Copy)]
pub(super) enum Icon {
    Desktop,
    Picture,
    Exit,
    Grid,
    Settings,
    Refresh,
    Close,
    Min,
    Max,
    Play,
    Folder,
    Terminal,
    Link,
    Person,
    Star,
    Info,
    Volume,
    Muted,
}
pub(super) fn icon(p: &egui::Painter, rect: Rect, kind: Icon, color: C) {
    let r = Rect::from_center_size(rect.center(), Vec2::splat(18.0));
    let pt = |x: f32, y: f32| Pos2::new(r.left() + x, r.top() + y);
    let stroke = Stroke::new(1.5, color);
    let line = |a: (f32, f32), b: (f32, f32)| {
        p.line_segment([pt(a.0, a.1), pt(b.0, b.1)], stroke);
    };
    match kind {
        Icon::Volume | Icon::Muted => {
            p.add(egui::Shape::closed_line(vec![pt(2.0, 7.0), pt(5.0, 7.0), pt(9.0, 3.0), pt(9.0, 15.0), pt(5.0, 11.0), pt(2.0, 11.0)], stroke));
            if matches!(kind, Icon::Muted) {
                line((12.0, 6.0), (17.0, 12.0));
                line((12.0, 12.0), (17.0, 6.0));
            } else {
                for radius in [4.0, 7.0] {
                    let points = (0..=8).map(|step| {
                        let angle = -0.8 + step as f32 * 0.2;
                        pt(9.0, 9.0) + egui::vec2(angle.cos(), angle.sin()) * radius
                    }).collect();
                    p.add(egui::Shape::line(points, stroke));
                }
            }
        }
        Icon::Info => {
            p.circle_stroke(r.center(), 7.0, stroke);
            p.circle_filled(pt(9.0, 5.0), 0.9, color);
            line((9.0, 8.0), (9.0, 13.0));
        }
        Icon::Exit => {
            line((8.0, 2.0), (2.0, 2.0));
            line((2.0, 2.0), (2.0, 16.0));
            line((2.0, 16.0), (8.0, 16.0));
            line((7.0, 9.0), (17.0, 9.0));
            line((13.0, 5.0), (17.0, 9.0));
            line((17.0, 9.0), (13.0, 13.0));
        }
        Icon::Picture => {
            p.rect_stroke(Rect::from_min_max(pt(1.0, 2.0), pt(17.0, 16.0)), 2.0, stroke);
            p.circle_stroke(pt(6.0, 6.0), 1.5, stroke);
            line((2.0, 14.0), (8.0, 9.0));
            line((8.0, 9.0), (11.0, 12.0));
            line((11.0, 12.0), (14.0, 9.0));
            line((14.0, 9.0), (17.0, 12.0));
        }
        Icon::Desktop => {
            p.rect_stroke(
                Rect::from_min_max(pt(1.0, 2.0), pt(17.0, 13.0)),
                2.0,
                stroke,
            );
            line((9.0, 13.0), (9.0, 16.0));
            line((5.0, 17.0), (13.0, 17.0));
        }
        Icon::Grid => {
            for x in [1.0, 10.0] {
                for y in [1.0, 10.0] {
                    p.rect_stroke(Rect::from_min_size(pt(x, y), Vec2::splat(6.0)), 1.0, stroke);
                }
            }
        }
        Icon::Settings => {
            let mut points = Vec::with_capacity(32);
            for tooth in 0..8 {
                for (offset, radius) in [(-0.35, 6.0), (-0.20, 8.0), (0.20, 8.0), (0.35, 6.0)] {
                    let angle = (tooth as f32 + offset) * std::f32::consts::TAU / 8.0;
                    points.push(r.center() + egui::vec2(angle.cos(), angle.sin()) * radius);
                }
            }
            p.add(egui::Shape::closed_line(points, stroke));
            p.circle_stroke(r.center(), 2.6, stroke);
        }
        Icon::Refresh => {
            p.circle_stroke(r.center(), 6.0, stroke);
            line((15.0, 2.0), (15.0, 7.0));
            line((15.0, 7.0), (10.0, 7.0));
        }
        Icon::Close => {
            line((3.0, 3.0), (15.0, 15.0));
            line((15.0, 3.0), (3.0, 15.0));
        }
        Icon::Min => line((3.0, 9.0), (15.0, 9.0)),
        Icon::Max => {
            p.rect_stroke(
                Rect::from_min_max(pt(3.0, 3.0), pt(15.0, 15.0)),
                0.0,
                stroke,
            );
        }
        Icon::Play => {
            p.rect_stroke(r.shrink(1.0), 3.0, stroke);
            p.add(egui::Shape::closed_line(
                vec![pt(7.0, 5.0), pt(13.0, 9.0), pt(7.0, 13.0)],
                stroke,
            ));
        }
        Icon::Folder => {
            p.rect_stroke(
                Rect::from_min_max(pt(1.0, 5.0), pt(17.0, 16.0)),
                2.0,
                stroke,
            );
            line((1.0, 5.0), (1.0, 2.0));
            line((1.0, 2.0), (7.0, 2.0));
            line((7.0, 2.0), (10.0, 5.0));
        }
        Icon::Terminal => {
            p.rect_stroke(r.shrink(1.0), 3.0, stroke);
            line((4.0, 6.0), (7.0, 9.0));
            line((7.0, 9.0), (4.0, 12.0));
            line((10.0, 12.0), (14.0, 12.0));
        }
        Icon::Link => {
            p.rect_stroke(Rect::from_min_max(pt(1.0, 3.0), pt(6.0, 15.0)), 2.0, stroke);
            p.rect_stroke(
                Rect::from_min_max(pt(12.0, 3.0), pt(17.0, 15.0)),
                2.0,
                stroke,
            );
            line((6.0, 9.0), (12.0, 9.0));
        }
        Icon::Person => {
            p.circle_stroke(pt(9.0, 5.0), 3.0, stroke);
            p.rect_stroke(
                Rect::from_min_max(pt(3.0, 11.0), pt(15.0, 17.0)),
                3.0,
                stroke,
            );
        }
        Icon::Star => {
            let points = (0..10)
                .map(|i| {
                    let a = i as f32 * std::f32::consts::PI / 5.0 - std::f32::consts::FRAC_PI_2;
                    let radius = if i % 2 == 0 { 8.0 } else { 3.5 };
                    r.center() + egui::vec2(a.cos(), a.sin()) * radius
                })
                .collect();
            p.add(egui::Shape::closed_line(points, stroke));
        }
    }
}
pub(super) fn icon_button(ui: &mut egui::Ui, kind: Icon, tip: &str) -> Response {
    icon_button_selected(ui, kind, tip, false)
}
pub(super) fn icon_button_selected(ui: &mut egui::Ui, kind: Icon, tip: &str, selected: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
    if response.hovered() || selected {
        ui.painter()
            .rect_filled(rect, 6.0, if selected {C::from_rgb(222, 235, 249)} else {ui.visuals().widgets.hovered.bg_fill});
    }
    icon(ui.painter(), rect, kind, if selected {BLUE} else {ui.visuals().text_color()});
    response.on_hover_text(tip)
}

pub(super) fn icon_label_button(ui: &mut egui::Ui, kind: Icon, label: &str) -> Response {
    let text = ui.painter().layout_no_wrap(label.into(), egui::FontId::proportional(14.0), ui.visuals().text_color());
    let (rect, response) = ui.allocate_exact_size(egui::vec2(40.0 + text.size().x, 30.0), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 6.0, ui.visuals().widgets.hovered.bg_fill);
    }
    icon(ui.painter(), Rect::from_center_size(rect.left_center() + egui::vec2(14.0, 0.0), Vec2::splat(18.0)), kind, ui.visuals().text_color());
    ui.painter().galley(egui::pos2(rect.left() + 30.0, rect.center().y - text.size().y / 2.0), text, ui.visuals().text_color());
    response
}
fn text_at(p: &egui::Painter, pos: Pos2, text: impl Into<String>, size: f32, color: C) {
    p.text(
        pos,
        egui::Align2::LEFT_CENTER,
        text.into(),
        egui::FontId::proportional(size),
        color,
    );
}
fn online(status: &str) -> bool {
    matches!(status, "CONNECTED" | "ready" | "ONLINE")
}
pub(super) fn logo(p: &egui::Painter, rect: Rect) {
    super::brand::paint_logo(p, rect);
}

impl App {
    fn title_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("home-title")
            .exact_height(58.0)
            .frame(
                egui::Frame::none()
                    .fill(SIDEBAR)
                    .inner_margin(egui::Margin::symmetric(18.0, 8.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    let (rect, response) =
                        ui.allocate_exact_size(egui::vec2(30.0, 34.0), Sense::drag());
                    logo(ui.painter(), rect);
                    if response.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    ui.label(RichText::new(super::brand::NAME).size(19.0).color(INK));
                    let space = (ui.available_width() - 220.0).max(0.0);
                    let (_, drag) =
                        ui.allocate_exact_size(egui::vec2(space, 34.0), Sense::click_and_drag());
                    if drag.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if drag.double_clicked() {
                        self.toggle_maximized(ctx);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, Icon::Close, "关闭").clicked() {
                            self.disconnect();
                            self.close_at = Some(
                                std::time::Instant::now() + std::time::Duration::from_millis(200),
                            );
                        }
                        if icon_button(ui, Icon::Max, "最大化 / 还原").clicked() {
                            self.toggle_maximized(ctx);
                        }
                        if icon_button(ui, Icon::Min, "最小化").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                        if icon_button(ui,Icon::Settings,"软件设置").clicked(){self.open_settings();}
                        if icon_button(ui,Icon::Info,"关于软件").clicked(){self.open_about();}
                        self.account_button(ui);
                    });
                });
            });
    }

    pub(super) fn home_ui(&mut self, ctx: &egui::Context) {
        self.title_bar(ctx);
        if !self.logged_in {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(45.0);
                    ui.heading("连接你的每一台设备");
                    ui.add_space(12.0);
                    ui.label(RichText::new("手机扫码登录，随时进入远程桌面").color(MUTED));
                    ui.add_space(24.0);
                    if let Some(texture) = &self.login_texture {
                        ui.image((texture.id(), egui::vec2(224.0, 224.0)));
                    } else {
                        ui.add_space(224.0);
                    }
                    ui.add_space(16.0);
                    ui.label(&self.login_status);
                    if ui.button(if self.login_texture.is_none() && self.login_rx.is_none() {"扫码登录"} else {"刷新二维码"}).clicked() {
                        self.start_login();
                    }
                    if self.login_rx.is_some() && ui.button("取消登录").clicked() {
                        self.login_rx = None;
                        self.login_texture = None;
                        self.login_status = "已取消登录".into();
                    }
                    if !self.status.is_empty() {ui.label(RichText::new(&self.status).color(MUTED));}
                });
            });
            return;
        }
        let sidebar_width = if ctx.screen_rect().width() < 800.0 {
            205.0
        } else {
            250.0
        };
        egui::SidePanel::left("home-devices")
            .exact_width(sidebar_width)
            .resizable(false)
            .frame(
                egui::Frame::none()
                    .fill(SIDEBAR)
                    .inner_margin(egui::Margin::same(10.0)),
            )
            .show(ctx, |ui| {
                let (card, _) =
                    ui.allocate_exact_size(egui::vec2(ui.available_width(), 108.0), Sense::hover());
                ui.painter().rect_filled(card, 5.0, BLUE);
                text_at(
                    ui.painter(),
                    card.left_top() + egui::vec2(18.0, 38.0),
                    super::brand::TAGLINE,
                    17.0,
                    C::WHITE,
                );
                text_at(
                    ui.painter(),
                    card.left_top() + egui::vec2(18.0, 72.0),
                    "开源连接，自由协作",
                    13.0,
                    C::from_white_alpha(220),
                );
                ui.add_space(15.0);
                ui.horizontal(|ui| {
                    icon(
                        ui.painter(),
                        Rect::from_min_size(ui.cursor().min, Vec2::splat(22.0)),
                        Icon::Desktop,
                        INK,
                    );
                    ui.add_space(28.0);
                    ui.label(RichText::new("我的设备").size(16.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, Icon::Refresh, "刷新设备").clicked() {
                            self.start_load();
                        }
                    });
                });
                ui.add_space(8.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("搜索设备")
                        .desired_width(f32::INFINITY)
                        .margin(egui::vec2(10.0, 6.0)),
                );
                ui.add_space(8.0);
                let mut selected = None;
                egui::ScrollArea::vertical()
                    .max_height((ui.available_height() - 175.0).max(90.0))
                    .show(ui, |ui| {
                        for (index, device) in self.devices.iter().enumerate().filter(|(_, d)| {
                            self.search.is_empty()
                                || d.alias.to_lowercase().contains(&self.search.to_lowercase())
                        }) {
                            let (row, response) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), 45.0),
                                Sense::click(),
                            );
                            let active = self.selected == Some(index);
                            if active || response.hovered() {
                                ui.painter().rect_filled(
                                    row,
                                    5.0,
                                    if active {
                                        C::from_rgb(225, 232, 238)
                                    } else {
                                        C::from_rgb(232, 239, 244)
                                    },
                                );
                            }
                            if active {
                                ui.painter().rect_filled(
                                    Rect::from_min_size(
                                        row.left_center() + egui::vec2(0.0, -10.0),
                                        egui::vec2(3.0, 20.0),
                                    ),
                                    2.0,
                                    BLUE,
                                );
                            }
                            ui.painter().circle_filled(
                                row.left_center() + egui::vec2(17.0, 0.0),
                                3.5,
                                if online(&device.status) {
                                    C::from_rgb(46, 202, 83)
                                } else {
                                    C::from_rgb(182, 187, 194)
                                },
                            );
                            let tile = Rect::from_center_size(
                                row.left_center() + egui::vec2(44.0, 0.0),
                                Vec2::splat(25.0),
                            );
                            ui.painter().rect_filled(
                                tile,
                                5.0,
                                if online(&device.status) {
                                    BLUE
                                } else {
                                    C::from_rgb(178, 183, 190)
                                },
                            );
                            icon(ui.painter(), tile.shrink(4.0), Icon::Grid, C::WHITE);
                            let label = if device.alias.chars().count() > 17 {
                                format!("{}…", device.alias.chars().take(15).collect::<String>())
                            } else {
                                device.alias.clone()
                            };
                            text_at(
                                &ui.painter().with_clip_rect(row),
                                row.left_center() + egui::vec2(66.0, 0.0),
                                label,
                                15.0,
                                if online(&device.status) { INK } else { MUTED },
                            );
                            if response.clicked() {
                                selected = Some(index);
                            }
                        }
                    });
                if let Some(index) = selected {
                    self.selected = Some(index);
                }
                ui.add_space(13.0);
                for (kind, label) in [
                    (Icon::Grid, "全部设备"),
                    (Icon::Link, "远程协助"),
                    (Icon::Star, "收藏设备"),
                ] {
                    let (row, response) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 39.0),
                        Sense::hover(),
                    );
                    icon(
                        ui.painter(),
                        Rect::from_center_size(
                            row.left_center() + egui::vec2(28.0, 0.0),
                            Vec2::splat(22.0),
                        ),
                        kind,
                        if matches!(kind, Icon::Grid) {
                            BLUE
                        } else {
                            MUTED
                        },
                    );
                    text_at(
                        ui.painter(),
                        row.left_center() + egui::vec2(55.0, 0.0),
                        label,
                        15.0,
                        if matches!(kind, Icon::Grid) {
                            INK
                        } else {
                            MUTED
                        },
                    );
                    if !matches!(kind, Icon::Grid) {
                        response.on_hover_text("暂未开放");
                    }
                }

            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(C::from_rgb(248, 251, 253))
                    .inner_margin(egui::Margin::symmetric(36.0, 26.0)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let Some(index) = self.selected.filter(|i| *i < self.devices.len()) else {
                        ui.add_space(80.0);
                        ui.heading("欢迎回来");
                        ui.label("从左侧选择一台设备，开始远程连接。");
                        if self.rx.is_some() {
                            ui.spinner();
                        }
                        return;
                    };
                    let device = self.devices[index].clone();
                    let is_online = online(&device.status);
                    ui.horizontal(|ui| {
                        let (pill, _) =
                            ui.allocate_exact_size(egui::vec2(61.0, 27.0), Sense::hover());
                        ui.painter().rect_filled(pill, 14.0, INK);
                        ui.painter().circle_filled(
                            pill.left_center() + egui::vec2(13.0, 0.0),
                            4.0,
                            if is_online {
                                C::from_rgb(0, 225, 168)
                            } else {
                                MUTED
                            },
                        );
                        text_at(
                            ui.painter(),
                            pill.left_center() + egui::vec2(24.0, 0.0),
                            if is_online { "在线" } else { "离线" },
                            13.0,
                            C::WHITE,
                        );
                        ui.add_space(5.0);
                        ui.label(
                            RichText::new(&device.alias)
                                .strong()
                                .size(if ui.available_width() < 300.0 {
                                    22.0
                                } else {
                                    29.0
                                })
                                .color(INK),
                        );
                        ui.menu_button("···", |ui| {
                            ui.label(&device.device_id);
                            if ui.button("复制设备 ID").clicked() {
                                ui.output_mut(|o| o.copied_text = device.device_id.clone());
                                ui.close_menu();
                            }
                        });
                    });
                    ui.add_space(25.0);
                    let width = ui.available_width();
                    let height = (width * 0.40).clamp(140.0, 360.0);
                    let (hero, response) =
                        ui.allocate_exact_size(egui::vec2(width, height), Sense::click());
                    if let Some(texture) = self.wallpapers.get(&index) {
                        let image_size = texture.size_vec2();
                        let scale = (hero.width() / image_size.x).max(hero.height() / image_size.y);
                        let uv_size = hero.size() / (image_size * scale);
                        let uv = Rect::from_center_size(egui::pos2(0.5, 0.5), uv_size);
                        ui.painter().image(texture.id(), hero, uv, C::WHITE);
                    } else {
                        ui.painter()
                            .rect_filled(hero, 7.0, C::from_rgb(169, 211, 250));
                        for n in (0..7).rev() {
                            let center =
                                hero.center() + egui::vec2(n as f32 * 24.0 - 50.0, n as f32 * 8.0);
                            ui.painter().with_clip_rect(hero).circle_stroke(
                                center,
                                hero.height() * 0.44 + n as f32 * 12.0,
                                Stroke::new(22.0, C::from_rgb(65 + n * 9, 133 + n * 8, 243)),
                            );
                        }
                    }
                    let label = if !is_online {
                        "设备当前离线"
                    } else if !device.controllable {
                        "设备暂不可连接"
                    } else {
                        "进入桌面  →"
                    };
                    let enter = Rect::from_center_size(
                        hero.center_bottom() + egui::vec2(0.0, -46.0),
                        egui::vec2(178.0, 46.0),
                    );
                    ui.painter().rect_filled(
                        enter,
                        24.0,
                        C::from_black_alpha(if response.hovered() { 130 } else { 90 }),
                    );
                    ui.painter().text(
                        enter.center(),
                        egui::Align2::CENTER_CENTER,
                        label,
                        egui::FontId::proportional(20.0),
                        C::WHITE,
                    );
                    if response.clicked() && is_online && device.controllable {
                        self.view_only = false;
                        self.connect();
                    }
                    if response.hovered() && is_online && device.controllable {
                        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    ui.add_space(0.0);
                    egui::Frame::none()
                        .stroke(Stroke::new(1.0, C::from_rgb(216, 222, 228)))
                        .rounding(egui::Rounding {
                            nw: 0.0,
                            ne: 0.0,
                            sw: 7.0,
                            se: 7.0,
                        })
                        .inner_margin(egui::Margin::symmetric(4.0, 12.0))
                        .show(ui, |ui| {
                            let cell = (width - 16.0) / 4.0;
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                for (kind, label, enabled) in [
                                    (Icon::Folder, "文件传输", false),
                                    (Icon::Play, "观看模式", is_online && device.controllable),
                                    (Icon::Terminal, "终端", false),
                                    (Icon::Link, "端口映射", false),
                                ] {
                                    let (r, response) = ui.allocate_exact_size(
                                        egui::vec2(cell, 30.0),
                                        if enabled {
                                            Sense::click()
                                        } else {
                                            Sense::hover()
                                        },
                                    );
                                    let color = if enabled {
                                        INK
                                    } else {
                                        C::from_rgb(167, 175, 184)
                                    };
                                    icon(
                                        ui.painter(),
                                        Rect::from_center_size(
                                            r.center()
                                                - egui::vec2(
                                                    if cell < 110.0 { 0.0 } else { 34.0 },
                                                    0.0,
                                                ),
                                            Vec2::splat(20.0),
                                        ),
                                        kind,
                                        color,
                                    );
                                    if cell >= 110.0 {
                                        text_at(
                                            ui.painter(),
                                            r.center() - egui::vec2(17.0, 0.0),
                                            label,
                                            14.0,
                                            color,
                                        );
                                    }
                                    if !enabled {
                                        response.on_hover_text("暂未开放");
                                    } else if response.on_hover_text(label).clicked() {
                                        self.view_only = true;
                                        self.connect();
                                    }
                                }
                            });
                        });
                    ui.add_space(24.0);
                    ui.label(RichText::new("快速启动").size(17.0).color(MUTED));
                    ui.add_space(11.0);
                    let (quick, response) =
                        ui.allocate_exact_size(egui::vec2(width, 115.0), Sense::click());
                    // Fine dashes match the reference's quiet empty-state card.
                    let stroke = Stroke::new(1.0, C::from_rgb(214, 222, 230));
                    for x in (0..width as usize).step_by(14) {
                        let x = quick.left() + x as f32;
                        ui.painter().line_segment(
                            [
                                egui::pos2(x, quick.top()),
                                egui::pos2((x + 7.0).min(quick.right()), quick.top()),
                            ],
                            stroke,
                        );
                        ui.painter().line_segment(
                            [
                                egui::pos2(x, quick.bottom()),
                                egui::pos2((x + 7.0).min(quick.right()), quick.bottom()),
                            ],
                            stroke,
                        );
                    }
                    for y in (0..115).step_by(14) {
                        let y = quick.top() + y as f32;
                        for x in [quick.left(), quick.right()] {
                            ui.painter().line_segment(
                                [
                                    egui::pos2(x, y),
                                    egui::pos2(x, (y + 7.0).min(quick.bottom())),
                                ],
                                stroke,
                            );
                        }
                    }
                    let center = quick.center() - egui::vec2(0.0, 10.0);
                    ui.painter()
                        .circle_filled(center, 22.0, C::from_rgb(232, 238, 243));
                    ui.painter().text(
                        center,
                        egui::Align2::CENTER_CENTER,
                        "+",
                        egui::FontId::proportional(30.0),
                        MUTED,
                    );
                    ui.painter().text(
                        center + egui::vec2(0.0, 38.0),
                        egui::Align2::CENTER_CENTER,
                        "连接此设备",
                        egui::FontId::proportional(13.0),
                        INK,
                    );
                    if response.clicked() && is_online && device.controllable {
                        self.view_only = false;
                        self.connect();
                    }
                    ui.add_space(16.0);
                    ui.label(RichText::new(&self.status).size(12.0).color(MUTED));
                });
            });
    }

    pub(super) fn stream_ui(&mut self, ctx: &egui::Context, events: &[egui::Event]) {
        if !self.fullscreen { self.connection_bar(ctx); }
        let was_open = self.stream_menu.open || self.displays.menu_open;
        let controls = self.stream_controls(ctx);
        let control_pointer = !self.mouse_captured && ctx.input(|i|i.pointer.hover_pos().is_some_and(|p|controls.iter().any(|r|r.contains(p))));
        let controls_active = self.displays.menu_open || self.local_dialog_open() || !self.input_ready() || self.window_edge_active || was_open || self.stream_menu.open || control_pointer;
        let mut messages = Vec::new();
        egui::CentralPanel::default().frame(egui::Frame::none().fill(C::BLACK)).show(ctx,|ui| {
            ui.set_clip_rect(ui.max_rect());
            let rect=ui.available_rect_before_wrap();ui.allocate_rect(rect,Sense::hover());
            let chosen=(!self.displays.waiting_binding()&&self.textures.contains_key(&self.selected_track)).then_some(self.selected_track);
            if let Some(track)=chosen {
                let input_screen = self.input_screen(track);
                let texture=&self.textures[&track];let size=texture.size_vec2();let scale=(rect.width()/size.x).min(rect.height()/size.y);
                let draw=Rect::from_center_size(rect.center(),size*scale);
                ui.painter().image(texture.id(),draw,Rect::from_min_max(Pos2::ZERO,egui::pos2(1.0,1.0)),C::WHITE);
                if self.unpresented.remove(&track) { *self.frame_count.entry(track).or_insert(0) += 1; }
                let pointer=ctx.input(|i|i.pointer.hover_pos());
                if events.iter().any(|ev|matches!(ev,egui::Event::PointerButton {pos,pressed:true,..} if draw.contains(*pos))) {self.remote_focus=true;}
                if !self.mouse_captured && events.iter().any(|ev|matches!(ev,egui::Event::PointerButton {pos,pressed:true,..} if !rect.contains(*pos))) {
                    self.remote_focus=false;self.release_all_keys();messages.extend(self.pointer.release_all(input_screen));
                }
                if controls_active {if self.remote_focus || !self.pressed_vks.is_empty(){self.remote_focus=false;self.release_all_keys();}messages.extend(self.pointer.release_all(input_screen));}
                if !self.view_only && !controls_active && self.focused && self.remote_focus {
                    if self.preferences.relative_mouse {
                        if self.mouse_captured {messages.extend(self.pointer.events_relative(events,input_screen));}
                    } else {messages.extend(self.pointer.events(events,draw,pointer,input_screen));}
                }
                if self.remote_cursor_active && !controls_active && pointer.is_some_and(|p|draw.contains(p)) {ctx.set_cursor_icon(egui::CursorIcon::None);}
                if !self.remote_cursor_active && !self.preferences.relative_mouse && !controls_active {
                    let revision=self.cursor.revision();
                    let shape=self.controller.as_ref().and_then(|h|h.stats.cursor.lock().unwrap().as_ref().filter(|(r,_)|*r!=revision).cloned());
                    self.cursor.paint(ctx,shape,pointer,draw,input_screen,self.platform.cursor_size(ctx));
                }
            }else{
                let center=rect.center();
                let message=Rect::from_center_size(center-egui::vec2(0.0,24.0),egui::vec2((rect.width()-48.0).clamp(1.0,640.0),80.0));
                ui.put(message,egui::Label::new(RichText::new(if !self.displays.notice.is_empty(){&self.displays.notice}else{&self.status}).size(16.0).color(C::from_rgb(180,190,205))).wrap());
                if self.controller.is_some() {
                    if self.displays.ready() || self.displays.switching() {
                        ui.put(Rect::from_center_size(center+egui::vec2(0.0,44.0),egui::vec2(24.0,24.0)),egui::Spinner::new());
                    } else if let Some(id)=self.displays.selected {
                        if ui.put(Rect::from_center_size(center+egui::vec2(0.0,44.0),egui::vec2(156.0,34.0)),egui::Button::new("重试当前显示器")).clicked(){self.select_remote_screen(id);}
                    }
                    if !self.disconnecting && ui.put(Rect::from_center_size(center+egui::vec2(0.0,94.0),egui::vec2(116.0,34.0)),egui::Button::new("取消连接")).clicked(){self.disconnect();}
                } else {
                    if ui.put(Rect::from_center_size(center+egui::vec2(-70.0,54.0),egui::vec2(124.0,36.0)),egui::Button::new(RichText::new("重新连接").color(C::WHITE)).fill(BLUE)).clicked(){self.connect();}
                    if ui.put(Rect::from_center_size(center+egui::vec2(70.0,54.0),egui::vec2(124.0,36.0)),egui::Button::new("关闭串流窗口")).clicked(){self.disconnect();}
                }
            }
            if chosen.is_some() && !self.displays.notice.is_empty() {
                let notice=Rect::from_center_size(rect.center_top()+egui::vec2(0.0,38.0),egui::vec2((rect.width()-40.0).clamp(1.0,480.0),44.0));
                ui.painter().rect_filled(notice,6.0,C::from_black_alpha(160));
                ui.put(notice,egui::Label::new(RichText::new(&self.displays.notice).color(C::WHITE)).wrap());
            }
            if self.hud { self.paint_hud(ui, rect); }

        });
        for message in messages {
            self.send_input(message, 15);
        }
    }
}
