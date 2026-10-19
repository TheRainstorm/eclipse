//! Native layout based on the supplied UU desktop reference, with a separate
//! uncluttered stream view. All artwork here is egui vector drawing.
use super::App;
use eframe::egui::{self, Color32 as C, Pos2, Rect, Response, RichText, Sense, Stroke, Vec2};
const BLUE: C = C::from_rgb(53, 131, 255);
const INK: C = C::from_rgb(24, 29, 35);
const MUTED: C = C::from_rgb(115, 124, 135);
const SIDEBAR: C = C::from_rgb(240, 245, 248);

#[derive(Clone, Copy)]
pub(super) enum Icon {
    Desktop,
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
}
pub(super) fn icon(p: &egui::Painter, rect: Rect, kind: Icon, color: C) {
    let r = Rect::from_center_size(rect.center(), Vec2::splat(18.0));
    let pt = |x: f32, y: f32| Pos2::new(r.left() + x, r.top() + y);
    let stroke = Stroke::new(1.5, color);
    let line = |a: (f32, f32), b: (f32, f32)| {
        p.line_segment([pt(a.0, a.1), pt(b.0, b.1)], stroke);
    };
    match kind {
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
            p.circle_stroke(r.center(), 6.0, stroke);
            p.circle_stroke(r.center(), 2.0, stroke);
            for (a, b) in [
                ((9.0, 0.0), (9.0, 3.0)),
                ((9.0, 15.0), (9.0, 18.0)),
                ((0.0, 9.0), (3.0, 9.0)),
                ((15.0, 9.0), (18.0, 9.0)),
            ] {
                line(a, b);
            }
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
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, 6.0, ui.visuals().widgets.hovered.bg_fill);
    }
    icon(ui.painter(), rect, kind, ui.visuals().text_color());
    response.on_hover_text(tip)
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
fn logo(p: &egui::Painter, rect: Rect) {
    let c = rect.center();
    for (points, color) in [
        (
            vec![c + egui::vec2(-1.0, -14.0), c + egui::vec2(11.0, -1.0), c],
            C::from_rgb(0, 139, 255),
        ),
        (
            vec![c + egui::vec2(11.0, -1.0), c + egui::vec2(0.0, 14.0), c],
            C::from_rgb(20, 190, 242),
        ),
        (
            vec![c + egui::vec2(0.0, 14.0), c + egui::vec2(-13.0, 2.0), c],
            C::from_rgb(30, 114, 255),
        ),
        (
            vec![c + egui::vec2(-13.0, 2.0), c + egui::vec2(-1.0, -14.0), c],
            C::from_rgb(0, 211, 222),
        ),
    ] {
        p.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
    }
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
                    ui.label(RichText::new("网易UU远程").size(19.0).color(INK));
                    let space = (ui.available_width() - 182.0).max(0.0);
                    let (_, drag) =
                        ui.allocate_exact_size(egui::vec2(space, 34.0), Sense::click_and_drag());
                    if drag.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if drag.double_clicked() {
                        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, Icon::Close, "关闭").clicked() {
                            self.disconnect();
                            self.close_at = Some(
                                std::time::Instant::now() + std::time::Duration::from_millis(200),
                            );
                        }
                        if icon_button(ui, Icon::Max, "最大化 / 还原").clicked() {
                            let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                        }
                        if icon_button(ui, Icon::Min, "最小化").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                        ui.separator();
                        if icon_button(ui, Icon::Settings, "设置").clicked() {
                            self.show_settings = !self.show_settings;
                        }
                        icon_button(
                            ui,
                            Icon::Person,
                            if self.logged_in {
                                "已登录"
                            } else {
                                "未登录"
                            },
                        );
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
                    if ui.button("刷新二维码").clicked() {
                        self.start_login();
                    }
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
                icon(
                    ui.painter(),
                    Rect::from_min_size(
                        card.right_top() + egui::vec2(-49.0, 16.0),
                        Vec2::splat(30.0),
                    ),
                    Icon::Desktop,
                    C::from_white_alpha(65),
                );
                text_at(
                    ui.painter(),
                    card.left_top() + egui::vec2(18.0, 38.0),
                    "随时连接，自在掌控",
                    17.0,
                    C::WHITE,
                );
                text_at(
                    ui.painter(),
                    card.left_top() + egui::vec2(18.0, 72.0),
                    "你的设备，触手可及",
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
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.separator();
                    if ui
                        .add(
                            egui::Button::new("⚙  设置")
                                .frame(false)
                                .min_size(egui::vec2(120.0, 36.0)),
                        )
                        .clicked()
                    {
                        self.show_settings = true;
                    }
                });
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

    pub(super) fn settings_ui(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = true;
        egui::Window::new("连接设置")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(320.0)
            .show(ctx, |ui| {
                ui.label("画质与带宽");
                let previous = (self.bitrate_idx, self.custom_mbps);
                egui::ComboBox::from_id_salt("settings-quality")
                    .selected_text(super::BITRATE_PRESETS[self.bitrate_idx].0)
                    .show_ui(ui, |ui| {
                        for (index, (label, _)) in super::BITRATE_PRESETS.iter().enumerate() {
                            ui.selectable_value(&mut self.bitrate_idx, index, *label);
                        }
                    });
                if self.bitrate_idx == 5 {
                    ui.add(egui::Slider::new(&mut self.custom_mbps, 1..=100).suffix(" Mbps"));
                }
                let resolution_before=self.resolution_idx;
                egui::ComboBox::from_id_salt("resolution-quality").selected_text(["跟随远端分辨率","1920 × 1080","2560 × 1440","3840 × 2160"][self.resolution_idx]).show_ui(ui,|ui|{
                    for (index,label) in ["跟随远端分辨率","1920 × 1080","2560 × 1440","3840 × 2160"].iter().enumerate(){ui.selectable_value(&mut self.resolution_idx,index,*label);}
                });
                if resolution_before!=self.resolution_idx{self.send_bitrate();}
                if previous != (self.bitrate_idx, self.custom_mbps) {
                    self.send_bitrate();
                }
                ui.add_space(10.0);
                ui.label(
                    RichText::new("Ctrl + Alt + F　切换全屏\nF1　显示 / 隐藏串流统计").color(MUTED),
                );
            });
        self.show_settings = open;
    }

    pub(super) fn stream_ui(&mut self, ctx: &egui::Context, events: &[egui::Event]) {
        if !self.fullscreen { self.connection_bar(ctx); }
        let was_open = self.stream_menu.open;
        let controls = self.stream_controls(ctx);
        let control_pointer = ctx.input(|i|i.pointer.hover_pos().is_some_and(|p|controls.iter().any(|r|r.contains(p))));
        let controls_active = self.disconnecting || self.window_edge_active || was_open || self.stream_menu.open || control_pointer || ctx.memory(|m|m.any_popup_open());
        let mut messages = Vec::new();
        egui::CentralPanel::default().frame(egui::Frame::none().fill(C::BLACK)).show(ctx,|ui| {
            ui.set_clip_rect(ui.max_rect());
            let rect=ui.available_rect_before_wrap();ui.allocate_rect(rect,Sense::hover());
            let chosen=self.textures.contains_key(&self.selected_track).then_some(self.selected_track).or_else(||self.textures.keys().next().copied());
            if let Some(track)=chosen {
                let texture=&self.textures[&track];let size=texture.size_vec2();let scale=(rect.width()/size.x).min(rect.height()/size.y);
                let draw=Rect::from_center_size(rect.center(),size*scale);
                ui.painter().image(texture.id(),draw,Rect::from_min_max(Pos2::ZERO,egui::pos2(1.0,1.0)),C::WHITE);
                if self.unpresented.remove(&track) { *self.frame_count.entry(track).or_insert(0) += 1; }
                let pointer=ctx.input(|i|i.pointer.hover_pos());
                if events.iter().any(|ev|matches!(ev,egui::Event::PointerButton {pos,pressed:true,..} if draw.contains(*pos))) {self.remote_focus=true;}
                if events.iter().any(|ev|matches!(ev,egui::Event::PointerButton {pos,pressed:true,..} if !rect.contains(*pos))) {
                    self.remote_focus=false;self.release_all_keys();messages.extend(self.pointer.release_all(track+1));
                }
                if self.show_settings || controls_active {if self.remote_focus || !self.pressed_vks.is_empty(){self.remote_focus=false;self.release_all_keys();}messages.extend(self.pointer.release_all(track+1));}
                if !self.view_only && !self.show_settings && !controls_active {messages.extend(self.pointer.events(events,draw,pointer,track+1));}
            }else{
                ui.painter().text(rect.center(),egui::Align2::CENTER_CENTER,&self.status,egui::FontId::proportional(18.0),C::from_rgb(160,170,185));
            }
            if self.hud { self.paint_hud(ui, rect); }

        });
        for message in messages {
            self.send_input(message, 15);
        }
    }
}
