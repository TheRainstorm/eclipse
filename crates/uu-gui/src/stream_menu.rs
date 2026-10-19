//! Connection chrome and the optional floating control menu.
use super::ui::{icon, icon_button, Icon};
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Stroke};

fn row_icon(ui: &egui::Ui, response: &egui::Response, kind: Icon, color: Color32) {
    let rect = Rect::from_center_size(
        egui::pos2(response.rect.left() + 16.0, response.rect.center().y),
        egui::vec2(16.0, 16.0),
    );
    icon(ui.painter(), rect, kind, color);
}
fn submenu<R>(
    ui: &mut egui::Ui,
    kind: Icon,
    label: impl Into<String>,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    let response = ui.menu_button(format!("      {}   ", label.into()), body);
    row_icon(ui, &response.response, kind, Color32::from_rgb(42, 52, 66));
    let right = response.response.rect.right_center() - egui::vec2(7.0, 0.0);
    ui.painter().line_segment(
        [right - egui::vec2(2.0, 3.0), right + egui::vec2(1.0, 0.0)],
        Stroke::new(1.0, Color32::GRAY),
    );
    ui.painter().line_segment(
        [right + egui::vec2(1.0, 0.0), right + egui::vec2(-2.0, 3.0)],
        Stroke::new(1.0, Color32::GRAY),
    );
    response
}
const MENU_INK: Color32 = Color32::from_rgb(29, 32, 37);
const MENU_WIDTH: f32 = 236.0;

fn popup_frame(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::popup(ui.style())
        .fill(Color32::from_rgb(249, 249, 249))
        .stroke(Stroke::new(1.0, Color32::from_rgb(215, 215, 215)))
        .rounding(7.0)
        .inner_margin(egui::Margin::same(4.0))
        .shadow(egui::epaint::Shadow {
            offset: egui::vec2(0.0, 3.0),
            blur: 10.0,
            spread: 0.0,
            color: Color32::from_black_alpha(22),
        })
}

/// Fixed gutters keep icons, selection dots, labels and arrows aligned.
fn menu_row(
    ui: &mut egui::Ui,
    label: &str,
    kind: Option<Icon>,
    selected: bool,
    arrow: bool,
    hint: &str,
    active: bool,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), egui::Sense::click());
    let ink = if !ui.is_enabled() {
        Color32::from_gray(160)
    } else if label == "退出远控" {
        Color32::from_rgb(187, 65, 55)
    } else {
        MENU_INK
    };
    if ui.is_enabled() && (response.hovered() || active) {
        ui.painter()
            .rect_filled(rect, 3.0, Color32::from_rgb(233, 233, 233));
    }
    if let Some(kind) = kind {
        icon(
            ui.painter(),
            Rect::from_center_size(
                egui::pos2(rect.left() + 16.0, rect.center().y),
                egui::vec2(16.0, 16.0),
            ),
            kind,
            ink,
        );
    } else if selected {
        ui.painter()
            .circle_filled(egui::pos2(rect.left() + 16.0, rect.center().y), 1.8, ink);
    }
    ui.painter().text(
        egui::pos2(rect.left() + 36.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(14.0),
        ink,
    );
    if arrow {
        let center = egui::pos2(rect.right() - 13.0, rect.center().y);
        ui.painter().line_segment(
            [center - egui::vec2(2.0, 3.0), center + egui::vec2(1.0, 0.0)],
            Stroke::new(1.0, Color32::from_gray(125)),
        );
        ui.painter().line_segment(
            [
                center + egui::vec2(1.0, 0.0),
                center + egui::vec2(-2.0, 3.0),
            ],
            Stroke::new(1.0, Color32::from_gray(125)),
        );
    } else if !hint.is_empty() {
        ui.painter().text(
            egui::pos2(rect.right() - 12.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            hint,
            egui::FontId::proportional(12.0),
            Color32::from_gray(145),
        );
    }
    response
}
fn separator(ui: &mut egui::Ui) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 9.0), egui::Sense::hover());
    ui.painter().line_segment(
        [rect.left_center(), rect.right_center()],
        Stroke::new(1.0, Color32::from_gray(233)),
    );
}
fn child_position(row: Rect, screen: Rect, width: f32) -> Pos2 {
    let x = if row.right() + width + 9.0 <= screen.right() {
        row.right() + 5.0
    } else {
        row.left() - width - 13.0
    };
    egui::pos2(x.max(screen.left()), row.top() - 4.0)
}
/// The root remains visible while the chosen panel opens beside it.
fn side_submenu<R>(
    ui: &mut egui::Ui,
    blocked: &mut Vec<Rect>,
    kind: Icon,
    label: impl Into<String>,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    let label = label.into();
    let id = ui.make_persistent_id(("side-menu", &label));
    let open = ui.memory(|m| m.is_popup_open(id));
    let response = menu_row(ui, &label, Some(kind), false, true, "", open);
    if response.clicked() {
        ui.memory_mut(|m| m.toggle_popup(id));
    } else if response.hovered() && ui.memory(|m| m.any_popup_open()) {
        ui.memory_mut(|m| m.open_popup(id));
    }
    let mut inner = None;
    if ui.memory(|m| m.is_popup_open(id)) {
        let screen = ui.ctx().screen_rect().shrink(4.0);
        let area = egui::Area::new(id)
            .order(egui::Order::Foreground)
            .fixed_pos(child_position(response.rect, screen, MENU_WIDTH))
            .constrain_to(screen)
            .show(ui.ctx(), |ui| {
                popup_frame(ui).show(ui, |ui| {
                    ui.set_width(MENU_WIDTH);
                    ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                    egui::ScrollArea::vertical()
                        .max_height((screen.height() - 16.0).max(64.0))
                        .show(ui, |ui| {
                            inner = Some(
                                ui.with_layout(
                                    egui::Layout::top_down_justified(egui::Align::Min),
                                    body,
                                )
                                .inner,
                            );
                        });
                });
            });
        blocked.push(area.response.rect);
    }
    egui::InnerResponse::new(inner, response)
}
fn unavailable(ui: &mut egui::Ui, kind: Icon, label: &str, hint: &str) {
    ui.add_enabled_ui(false, |ui| {
        menu_row(ui, label, Some(kind), false, true, "", false)
    })
    .inner
    .on_disabled_hover_text(hint);
}
#[derive(Clone, Copy, PartialEq)]
pub(super) enum QualityDetail {
    Bitrate,
    Resolution,
    FrameRate,
}

pub(super) struct StreamMenu {
    pub visible: bool,
    pub open: bool,
    pub anchor: Pos2,
    pub(super) position: Option<Pos2>,
    pub(super) trigger_rect: Option<Rect>,
    pub(super) quality_detail: Option<(QualityDetail, Rect)>,
}
impl Default for StreamMenu {
    fn default() -> Self {
        Self {
            visible: true,
            open: false,
            anchor: Pos2::ZERO,
            position: None,
            trigger_rect: None,
            quality_detail: None,
        }
    }
}

impl super::App {
    pub(super) fn connection_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("connection-top")
            .exact_height(super::window::CONNECTION_BAR_HEIGHT)
            .frame(
                egui::Frame::none()
                    .fill(Color32::from_rgb(244, 247, 251))
                    .inner_margin(egui::Margin::symmetric(10.0, 2.0)),
            )
            .show(ctx, |ui| {
                if self.mouse_captured {ui.disable();}
                ui.horizontal_centered(|ui| {
                    ui.label(
                        RichText::new("◆")
                            .color(Color32::from_rgb(53, 131, 255))
                            .size(20.0),
                    );
                    let title = self
                        .selected
                        .and_then(|i| self.devices.get(i))
                        .map(|d| d.alias.as_str())
                        .unwrap_or("远程桌面");
                    ui.label(RichText::new(title).size(12.0));
                    ui.separator();
                    submenu(
                        ui,
                        Icon::Desktop,
                        format!("显示屏 {}", self.selected_track + 1),
                        |ui| self.display_menu(ui),
                    );
                    if self.view_only {
                        ui.label(RichText::new("仅观看").color(Color32::from_rgb(98, 125, 160)));
                    }
                    if !self.view_only && self.controller.is_some() && !self.input_ready() {
                        ui.label(
                            RichText::new("输入连接中…")
                                .size(12.0)
                                .color(Color32::from_gray(110)),
                        )
                        .on_hover_text("视频和输入分别建立连接，输入就绪后可操作");
                    }
                    if let Some(controller) = &self.controller {
                        ui.label(
                            RichText::new(format!(
                                "{:.1} Mbps",
                                controller
                                    .stats
                                    .bitrate_kbps
                                    .load(std::sync::atomic::Ordering::Relaxed)
                                    as f32
                                    / 1000.0
                            ))
                            .size(11.0)
                            .color(Color32::from_rgb(51, 159, 126)),
                        );
                    }
                    let width = (ui.available_width() - 236.0).max(0.0);
                    let (_, drag) =
                        ui.allocate_exact_size(egui::vec2(width, 28.0), egui::Sense::drag());
                    if drag.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, Icon::Close, "退出远控").clicked() {
                            self.disconnect();
                        }
                        if icon_button(ui, Icon::Max, "最大化 / 还原窗口").clicked() {
                            self.toggle_maximized(ctx);
                        }
                        if icon_button(ui, Icon::Min, "最小化").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                        let control = ui.add(egui::Button::new("      控制中心").frame(false));
                        self.stream_menu.trigger_rect = Some(control.rect);
                        row_icon(ui, &control, Icon::Settings, Color32::from_rgb(42, 52, 66));
                        if control.clicked() {
                            self.stream_menu.open = !self.stream_menu.open;
                            if !self.stream_menu.open {
                                ctx.memory_mut(|m| m.close_popup());
                            }
                            self.stream_menu.anchor = control.rect.left_bottom();
                        }
                    });
                });
            });
    }

    fn display_menu(&mut self, ui: &mut egui::Ui) {
        let mut tracks: Vec<_> = self.textures.keys().copied().collect();
        tracks.sort_unstable();
        for track in tracks {
            if menu_row(
                ui,
                &format!("显示屏 {}", track + 1),
                None,
                self.selected_track == track,
                false,
                "",
                false,
            )
            .clicked()
            {
                self.release_all_keys();
                for message in self
                    .pointer
                    .release_all(self.input_screen(self.selected_track))
                {
                    self.send_input(message, 15);
                }
                self.selected_track = track;
                self.window_sizing.begin_stream();
                ui.close_menu();
                ui.memory_mut(|m| m.close_popup());
            }
        }
        if self.textures.is_empty() {
            ui.add_enabled(false, egui::Label::new("正在等待显示屏…"));
        }
    }

    fn confirm_quality(&mut self, ui: &mut egui::Ui) {
        self.persist_preferences();
        self.send_capture_settings();
        self.stream_menu.quality_detail = None;
        ui.memory_mut(|m| m.close_popup());
    }

    fn quality_menu(&mut self, ui: &mut egui::Ui) {
        for (index, (label, _, _)) in super::BITRATE_PRESETS.iter().enumerate() {
            let custom = index == 4;
            let response = menu_row(
                ui,
                label,
                None,
                self.bitrate_idx == index,
                custom,
                "",
                false,
            );
            if custom && (response.hovered() || response.clicked()) {
                self.stream_menu.quality_detail = Some((QualityDetail::Bitrate, response.rect));
            } else if !custom && response.hovered() {
                self.stream_menu.quality_detail = None;
            }
            if !custom && response.clicked() {
                self.bitrate_idx = index;
                self.confirm_quality(ui);
            }
        }
        separator(ui);
        for fps in [144, 120, 90, 60, 30] {
            let response = menu_row(
                ui,
                &format!("{fps} 帧"),
                None,
                self.target_fps == fps,
                false,
                "",
                false,
            );
            if response.hovered() {
                self.stream_menu.quality_detail = None;
            }
            if response.clicked() {
                self.target_fps = fps;
                self.fps_draft = fps;
                self.confirm_quality(ui);
            }
        }
        let custom_fps = ![30, 60, 90, 120, 144].contains(&self.target_fps);
        let label = if custom_fps {
            format!("自定义 · {} 帧", self.target_fps)
        } else {
            "自定义帧率".into()
        };
        let response = menu_row(ui, &label, None, custom_fps, true, "", false);
        if response.hovered() || response.clicked() {
            self.stream_menu.quality_detail = Some((QualityDetail::FrameRate, response.rect));
        }
        separator(ui);
        let response = menu_row(ui, "分辨率", None, false, true, "", false);
        if response.hovered() || response.clicked() {
            self.stream_menu.quality_detail = Some((QualityDetail::Resolution, response.rect));
        }
    }

    fn quality_detail(&mut self, ctx: &egui::Context, blocked: &mut Vec<Rect>) {
        let Some((kind, row)) = self.stream_menu.quality_detail else {
            return;
        };
        let screen = ctx.screen_rect().shrink(4.0);
        let mut position = child_position(row, screen, 204.0);
        if ctx.memory(|m| m.area_rect(egui::Id::new("stream-control-menu")))
            .is_some_and(|root| root.center().x > row.center().x)
            && row.left() - screen.left() >= 217.0
        {
            position.x = row.left() - 217.0;
        }
        let response = egui::Area::new("quality-detail".into())
            .order(egui::Order::Foreground)
            .fixed_pos(position)
            .constrain_to(screen)
            .show(ctx, |ui| {
                popup_frame(ui).show(ui, |ui| {
                    ui.set_width(204.0);
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                    ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
                        match kind {
                            QualityDetail::Bitrate => {
                                ui.add_space(5.0);
                                ui.horizontal(|ui| {
                                    ui.label("码率");
                                    ui.add(
                                        egui::DragValue::new(&mut self.custom_mbps)
                                            .range(1..=100)
                                            .suffix(" Mbps"),
                                    );
                                });
                                ui.add_space(5.0);
                                separator(ui);
                                if menu_row(ui, "确认", None, false, false, "", false).clicked() {
                                    self.bitrate_idx = 4;
                                    self.confirm_quality(ui);
                                }
                            }
                            QualityDetail::FrameRate => {
                                ui.add_space(5.0);
                                ui.horizontal(|ui| {
                                    ui.label("帧率");
                                    ui.add(
                                        egui::DragValue::new(&mut self.fps_draft)
                                            .range(5..=144)
                                            .suffix(" FPS"),
                                    );
                                });
                                ui.add_space(5.0);
                                separator(ui);
                                if menu_row(ui, "确认", None, false, false, "", false).clicked() {
                                    self.target_fps = self.fps_draft.clamp(5, 144);
                                    self.confirm_quality(ui);
                                }
                            }
                            QualityDetail::Resolution => {
                                for (index, label) in [
                                    "跟随远端分辨率",
                                    "1920 × 1080",
                                    "2560 × 1440",
                                    "3840 × 2160",
                                ]
                                .iter()
                                .enumerate()
                                {
                                    if menu_row(
                                        ui,
                                        label,
                                        None,
                                        self.resolution_idx == index,
                                        false,
                                        "",
                                        false,
                                    )
                                    .clicked()
                                    {
                                        self.resolution_idx = index;
                                        self.confirm_quality(ui);
                                    }
                                }
                            }
                        }
                    });
                });
            });
        blocked.push(response.response.rect);
    }

    /// Draw first so the stream can exclude every control rectangle from input.
    pub(super) fn stream_controls(&mut self, ctx: &egui::Context) -> Vec<Rect> {
        let mut blocked = Vec::new();
        if self.mouse_captured {return blocked;}
        if !self.fullscreen {
            if let Some(rect) = self.stream_menu.trigger_rect {
                blocked.push(rect);
            }
        }
        if self.fullscreen && self.stream_menu.visible {
            let screen = ctx.screen_rect();
            let default = egui::pos2(screen.right() - 54.0, 8.0);
            let position = self.stream_menu.position.unwrap_or(default);
            let position = egui::pos2(
                position.x.clamp(screen.left(), screen.right() - 40.0),
                position.y.clamp(screen.top(), screen.bottom() - 40.0),
            );
            let response = egui::Area::new("floating-control-handle".into())
                .order(egui::Order::Foreground)
                .movable(false)
                .fixed_pos(position)
                .constrain_to(screen)
                .show(ctx, |ui| {
                    egui::Frame::none()
                        .fill(Color32::from_rgb(249, 250, 252))
                        .rounding(8.0)
                        .stroke(Stroke::new(1.0, Color32::from_rgb(217, 224, 234)))
                        .inner_margin(egui::Margin::same(3.0))
                        .show(ui, |ui| {
                            let (rect, button) = ui.allocate_exact_size(
                                egui::vec2(26.0, 26.0),
                                egui::Sense::click_and_drag(),
                            );
                            icon(
                                ui.painter(),
                                rect,
                                Icon::Settings,
                                Color32::from_rgb(53, 73, 96),
                            );
                            if button.dragged() {
                                self.stream_menu.position = Some(position + button.drag_delta());
                            }
                            if button.clicked() {
                                self.stream_menu.open = !self.stream_menu.open;
                                if !self.stream_menu.open {
                                    ctx.memory_mut(|m| m.close_popup());
                                }
                            }
                            button.on_hover_text(
                                "控制中心 · 拖动可移动\nCtrl+Alt+M 隐藏 / 显示悬浮菜单",
                            );
                        });
                });
            self.stream_menu.anchor = response.response.rect.left_bottom();
            blocked.push(response.response.rect);
        }
        if !self.stream_menu.open {
            self.stream_menu.quality_detail = None;
            // Settings own their ComboBox popup while their window is open.
            // Otherwise clear old stream children so no invisible popup traps input.
            if !self.settings_open {ctx.memory_mut(|m| m.close_popup());}
            return blocked;
        }
        let screen = ctx.screen_rect();
        let anchor = self.stream_menu.anchor;
        let position = egui::pos2(
            anchor
                .x
                .min(screen.right() - 264.0)
                .max(screen.left() + 6.0),
            anchor
                .y
                .min(screen.bottom() - 386.0)
                .max(screen.top() + 6.0),
        );
        let mut quality_open = false;
        let menu = egui::Area::new("stream-control-menu".into())
            .order(egui::Order::Foreground)
            .fixed_pos(position)
            .constrain_to(screen)
            .show(ctx, |ui| {
                popup_frame(ui).show(ui, |ui| {
                    ui.set_width(MENU_WIDTH);
                    ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                    egui::ScrollArea::vertical()
                        .max_height((screen.height() - 24.0).max(64.0))
                        .show(ui, |ui| {
                            ui.with_layout(
                                egui::Layout::top_down_justified(egui::Align::Min),
                                |ui| {
                                    side_submenu(
                                        ui,
                                        &mut blocked,
                                        Icon::Desktop,
                                        format!("显示屏 {}", self.selected_track + 1),
                                        |ui| self.display_menu(ui),
                                    );
                                    quality_open = side_submenu(
                                        ui,
                                        &mut blocked,
                                        Icon::Picture,
                                        "画质",
                                        |ui| self.quality_menu(ui),
                                    )
                                    .inner
                                    .is_some();
                                    side_submenu(ui, &mut blocked, Icon::Max, "窗口", |ui| {
                                        if menu_row(
                                            ui,
                                            if ctx
                                                .input(|i| i.viewport().maximized.unwrap_or(false))
                                            {
                                                "还原窗口"
                                            } else {
                                                "最大化窗口"
                                            },
                                            None,
                                            false,
                                            false,
                                            "",
                                            false,
                                        )
                                        .clicked()
                                        {
                                            self.toggle_maximized(ctx);
                                        }
                                        separator(ui);
                                        ui.checkbox(
                                            &mut self.stream_menu.visible,
                                            "全屏显示悬浮入口",
                                        );
                                    });
                                    side_submenu(ui, &mut blocked, Icon::Play, "声音", |ui| {
                                        let mut changed=false;
                                        if let Some(h) = &self.controller {
                                            use std::sync::atomic::Ordering::Relaxed;
                                            let audio = &h.stats.audio;
                                            let mut muted = audio.muted.load(Relaxed);
                                            if ui.checkbox(&mut muted, "静音").changed() { audio.muted.store(muted,Relaxed);changed=true; }
                                            let mut volume=audio.volume.load(Relaxed);
                                            if ui.add(egui::Slider::new(&mut volume,0..=100).text("音量")).changed() {audio.volume.store(volume,Relaxed);changed=true;}
                                            ui.label(audio.status.lock().unwrap().as_str());
                                            ui.label(format!("缓冲 {} ms · 接收 {} 包",audio.queue_ms.load(Relaxed),audio.packets.load(Relaxed)));
                                        }
                                        if changed{self.persist_preferences();}
                                    });
                                    side_submenu(ui, &mut blocked, Icon::Desktop, "鼠标", |ui| {
                                        let old=(self.preferences.relative_mouse,self.preferences.remote_cursor);
                                        ui.selectable_value(&mut self.preferences.relative_mouse,false,"桌面模式 · 绝对定位");
                                        ui.selectable_value(&mut self.preferences.relative_mouse,true,"游戏模式 · 相对移动");
                                        separator(ui);
                                        ui.selectable_value(&mut self.preferences.remote_cursor,false,"本地光标");
                                        ui.selectable_value(&mut self.preferences.remote_cursor,true,"远端光标");
                                        ui.small("Ctrl+Alt+Z 释放／捕获键鼠");
                                        if old!=(self.preferences.relative_mouse,self.preferences.remote_cursor) {
                                            self.release_pointer_capture(ctx);self.remote_focus=false;self.persist_preferences();
                                            self.send_capture_settings();ctx.memory_mut(|m|m.close_popup());
                                        }
                                    });
                                    side_submenu(ui, &mut blocked, Icon::Person, "安全", |ui| {
                                        if ui
                                            .checkbox(&mut self.view_only, "仅观看，禁止远程输入")
                                            .changed()
                                        {
                                            self.release_all_keys();
                                            for message in self
                                                .pointer
                                                .release_all(self.input_screen(self.selected_track))
                                            {
                                                self.send_input(message, 15);
                                            }
                                            self.remote_focus = false;
                                        }
                                    });
                                    side_submenu(ui,&mut blocked,Icon::Folder,"剪贴板",|ui| {
                                        if ui.checkbox(&mut self.preferences.clipboard,"同步文本剪贴板").changed(){self.persist_preferences();}
                                        ui.small("仅同步当前前台串流，支持最多30 KiB文本；图片和文件不传输。");
                                        if let Some(h)=&self.controller {ui.label(h.stats.clipboard.status.lock().unwrap().as_str());}
                                    });
                                    unavailable(ui, Icon::Link, "外设", "外设透传尚未接入");
                                    unavailable(
                                        ui,
                                        Icon::Folder,
                                        "工具",
                                        "文件、终端和端口映射尚未接入",
                                    );
                                    if menu_row(ui,"软件设置",Some(Icon::Settings),false,false,"",false).clicked(){self.open_settings();}
                                    side_submenu(ui, &mut blocked, Icon::Grid, "更多", |ui| {
                                        if menu_row(
                                            ui,
                                            "串流统计",
                                            None,
                                            self.hud,
                                            false,
                                            "Ctrl+Alt+S",
                                            false,
                                        )
                                        .clicked()
                                        {
                                            self.hud = !self.hud;self.persist_preferences();
                                        }
                                        if menu_row(
                                            ui,
                                            "隐藏悬浮入口",
                                            None,
                                            false,
                                            false,
                                            "Ctrl+Alt+M",
                                            false,
                                        )
                                        .clicked()
                                        {
                                            self.stream_menu.visible = false;
                                            self.stream_menu.open = false;
                                            ctx.memory_mut(|m| m.close_popup());
                                        }
                                    });
                                    separator(ui);
                                    if menu_row(
                                        ui,
                                        if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
                                            "还原窗口"
                                        } else {
                                            "最大化窗口"
                                        },
                                        Some(Icon::Max),
                                        false,
                                        false,
                                        "",
                                        false,
                                    )
                                    .clicked()
                                    {
                                        self.toggle_maximized(ctx);
                                    }
                                    ui.scope(|ui| {
                                        if menu_row(
                                            ui,
                                            "退出远控",
                                            Some(Icon::Exit),
                                            false,
                                            false,
                                            "",
                                            false,
                                        )
                                        .clicked()
                                        {
                                            self.stream_menu.open = false;
                                            self.disconnect();
                                        }
                                    });
                                },
                            );
                        });
                });
            });
        if quality_open {
            self.quality_detail(ctx, &mut blocked);
        } else {
            self.stream_menu.quality_detail = None;
        }
        blocked.push(menu.response.rect);
        let outside_click = ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| !blocked.iter().any(|r| r.contains(p)))
        });
        if outside_click || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.stream_menu.open = false;
            ctx.memory_mut(|m| m.close_popup());
        }
        if !self.stream_menu.open {
            ctx.memory_mut(|m| m.close_popup());
        }
        blocked
    }
}
