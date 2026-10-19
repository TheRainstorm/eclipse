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
/// Root controls stay visible while a child opens beside them, like UU.
fn side_submenu<R>(ui: &mut egui::Ui, blocked: &mut Vec<Rect>, kind: Icon, label: impl Into<String>, body: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<Option<R>> {
    let label = label.into();
    let id = ui.make_persistent_id(("side-menu", &label));
    let open = ui.memory(|m| m.is_popup_open(id));
    let response = ui.add(egui::Button::new(format!("      {label}     >")).selected(open));
    row_icon(ui, &response, kind, Color32::from_rgb(42, 52, 66));
    if response.clicked() { ui.memory_mut(|m| m.toggle_popup(id)); }
    else if response.hovered() && ui.memory(|m| m.any_popup_open()) { ui.memory_mut(|m| m.open_popup(id)); }
    let mut inner = None;
    if ui.memory(|m| m.is_popup_open(id)) {
        let screen = ui.ctx().screen_rect().shrink(4.0);
        let x = if response.rect.left() - screen.left() >= 218.0 {
            response.rect.left() - 218.0
        } else { response.rect.right() + 6.0 };
        let area = egui::Area::new(id).order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(x, response.rect.top())).constrain_to(screen)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).fill(Color32::from_rgb(250, 251, 253))
                    .rounding(8.0).show(ui, |ui| {
                        ui.set_width(196.0);
                        inner = Some(ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), body).inner);
                    });
            });
        blocked.push(area.response.rect);
        let parent = ui.memory(|m| m.area_rect(egui::Id::new("stream-control-menu"))).unwrap_or(response.rect);
        let outside = ui.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos()
            .is_some_and(|p| !area.response.rect.contains(p) && !parent.contains(p)));
        if outside { ui.memory_mut(|m| m.close_popup()); }
    }
    egui::InnerResponse::new(inner, response)
}
fn unavailable(ui: &mut egui::Ui, kind: Icon, label: &str, hint: &str) {
    let response = ui
        .add_enabled(false, egui::Button::new(format!("      {label}")))
        .on_hover_text(hint);
    row_icon(ui, &response, kind, Color32::from_gray(160));
}

pub(super) struct StreamMenu {
    pub visible: bool,
    pub open: bool,
    pub anchor: Pos2,
    position: Option<Pos2>,
    trigger_rect: Option<Rect>,
}
impl Default for StreamMenu {
    fn default() -> Self {
        Self {
            visible: true,
            open: false,
            anchor: Pos2::ZERO,
            position: None,
            trigger_rect: None,
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
                        ui.label(RichText::new("输入连接中…").size(12.0).color(Color32::from_gray(110)))
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
                            if !self.stream_menu.open { ctx.memory_mut(|m| m.close_popup()); }
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
            if ui
                .selectable_label(
                    self.selected_track == track,
                    format!("显示屏 {}", track + 1),
                )
                .clicked()
            {
                self.release_all_keys();
                for message in self.pointer.release_all(self.input_screen(self.selected_track)) {
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

    /// Draw first so the stream can exclude every control rectangle from input.
    pub(super) fn stream_controls(&mut self, ctx: &egui::Context) -> Vec<Rect> {
        let mut blocked = Vec::new();
        if !self.fullscreen {
            if let Some(rect) = self.stream_menu.trigger_rect { blocked.push(rect); }
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
                                if !self.stream_menu.open { ctx.memory_mut(|m| m.close_popup()); }
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
            // Custom child menus must never leave an invisible popup trapping input.
            ctx.memory_mut(|m| m.close_popup());
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
        let menu = egui::Area::new("stream-control-menu".into())
            .order(egui::Order::Foreground)
            .fixed_pos(position)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(Color32::from_rgb(250, 251, 253))
                    .rounding(8.0)
                    .inner_margin(egui::Margin::symmetric(9.0, 7.0))
                    .show(ui, |ui| {
                        ui.set_width(242.0);
                        ui.visuals_mut().widgets.inactive.bg_fill = Color32::TRANSPARENT;
                        ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                        ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                            ui.spacing_mut().interact_size.y = 27.0;
                            side_submenu(
                                ui, &mut blocked,
                                Icon::Desktop,
                                format!("显示屏 {}", self.selected_track + 1),
                                |ui| self.display_menu(ui),
                            );
                            side_submenu(ui, &mut blocked, Icon::Desktop, "画质", |ui| {
                                let mut confirm = false;
                                for (index, (label, _)) in super::BITRATE_PRESETS.iter().enumerate()
                                {
                                    if ui.selectable_value(&mut self.bitrate_idx, index, *label).clicked() {
                                        confirm = index != 5;
                                    }
                                }
                                if self.bitrate_idx == 5 {
                                    ui.add(
                                        egui::Slider::new(&mut self.custom_mbps, 1..=100)
                                            .suffix(" Mbps"),
                                    );
                                    confirm |= ui.button("确认自定义码率").clicked();
                                }
                                ui.separator();
                                let previous_resolution = self.resolution_idx;
                                for (index, label) in [
                                    "跟随远端分辨率",
                                    "1920 × 1080",
                                    "2560 × 1440",
                                    "3840 × 2160",
                                ]
                                .iter()
                                .enumerate()
                                {
                                    if ui.selectable_value(&mut self.resolution_idx, index, *label).clicked() {
                                        confirm = true;
                                    }
                                }
                                if confirm || previous_resolution != self.resolution_idx {
                                    self.send_capture_settings();
                                    ui.close_menu();
                ui.memory_mut(|m| m.close_popup());
                                }
                            });
                            side_submenu(ui, &mut blocked, Icon::Play, format!("帧率 · {} FPS", self.target_fps), |ui| {
                                for fps in [30, 60, 90, 120, 144] {
                                    if ui.selectable_label(self.target_fps == fps, format!("{fps} FPS")).clicked() {
                                        self.target_fps = fps;
                                        self.fps_draft = fps;
                                        self.send_capture_settings();
                                        ui.close_menu();
                                        ui.memory_mut(|m| m.close_popup());
                                    }
                                }
                                ui.separator();
                                ui.horizontal(|ui| {
                                    ui.label("自定义");
                                    ui.add(egui::DragValue::new(&mut self.fps_draft).range(5..=144).suffix(" FPS"));
                                });
                                if ui.button("确认自定义帧率").clicked() {
                                    self.target_fps = self.fps_draft.clamp(5, 144);
                                    self.send_capture_settings();
                                    ui.close_menu();
                                    ui.memory_mut(|m| m.close_popup());
                                }
                                ui.label(RichText::new("实际帧率以 HUD Rx / De / Rd 为准；
静态画面、远端采集和本地刷新率均有影响。").small().weak());
                                ui.separator();
                                let current = self.controller.as_ref().and_then(|h|
                                    h.stats.remote_screens.lock().unwrap().selected().cloned());
                                if let Some(screen) = &current {
                                    ui.label(format!("当前远端：{} · {}Hz", screen.kind_label(), screen.refresh_hz));
                                } else {
                                    ui.label("当前远端屏幕：等待信息");
                                }
                                let enter = !current.as_ref().is_some_and(|s| s.kind == 2);
                                if ui.add_enabled(self.controller.is_some() && self.screen_request.is_none(),
                                    egui::Button::new(if enter { "请求切换超级屏" } else { "退出超级屏" })).clicked() {
                                    self.request_super_screen(enter);
                                    ui.close_menu();
                                    ui.memory_mut(|m| m.close_popup());
                                }
                                ui.label(RichText::new("物理屏采集受远端限制；超级屏可请求更高刷新率。").small().weak());
                            });
                            if !self.settings_feedback.is_empty() {
                                ui.label(RichText::new(&self.settings_feedback).size(11.0)
                                    .color(Color32::from_rgb(51, 139, 111)));
                            }
                            if !self.screen_feedback.is_empty() {
                                ui.label(RichText::new(&self.screen_feedback).size(11.0));
                            }
                            side_submenu(ui, &mut blocked, Icon::Max, "窗口", |ui| {
                                if ui
                                    .button(if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
                                        "还原窗口"
                                    } else {
                                        "最大化窗口"
                                    })
                                    .clicked()
                                {
                                    self.toggle_maximized(ctx);
                                    self.stream_menu.open = false;
                                    ui.close_menu();
                ui.memory_mut(|m| m.close_popup());
                                }
                                ui.label(RichText::new("Ctrl+Alt+F 切换全屏串流").small().weak());
                                ui.checkbox(&mut self.stream_menu.visible, "全屏显示悬浮入口");
                                ui.label(RichText::new("Ctrl+Alt+M 可随时恢复入口").small().weak());
                            });
                            unavailable(ui, Icon::Play, "声音", "音频尚未接入");
                            side_submenu(ui, &mut blocked, Icon::Person, "安全", |ui| {
                                if ui
                                    .checkbox(&mut self.view_only, "仅观看，禁止远程输入")
                                    .changed()
                                {
                                    self.release_all_keys();
                                    for message in self.pointer.release_all(self.input_screen(self.selected_track))
                                    {
                                        self.send_input(message, 15);
                                    }
                                    self.remote_focus = false;
                                }
                            });
                            unavailable(ui, Icon::Link, "外设", "外设透传尚未接入");
                            unavailable(ui, Icon::Folder, "工具", "文件、终端和端口映射尚未接入");
                            side_submenu(ui, &mut blocked, Icon::Grid, "更多", |ui| {
                                ui.checkbox(&mut self.hud, "串流统计 · Ctrl+Alt+S");
                                if ui.button("隐藏悬浮入口 · Ctrl+Alt+M").clicked() {
                                    self.stream_menu.visible = false;
                                    self.stream_menu.open = false;
                                    ui.close_menu();
                ui.memory_mut(|m| m.close_popup());
                                }

                            });
                            ui.separator();
                            if ui
                                .button(if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
                                    "还原窗口"
                                } else {
                                    "最大化窗口"
                                })
                                .clicked()
                            {
                                self.toggle_maximized(ctx);
                                self.stream_menu.open = false;
                            }
                            if ui
                                .button(
                                    RichText::new("退出远控")
                                        .color(Color32::from_rgb(198, 65, 57)),
                                )
                                .clicked()
                            {
                                self.stream_menu.open = false;
                                self.disconnect();
                            }
                        });
                    });
            });
        blocked.push(menu.response.rect);
        let outside_click = ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| !blocked.iter().any(|r| r.contains(p)))
        });
        if (outside_click && !ctx.memory(|m| m.any_popup_open()))
            || ctx.input(|i| i.key_pressed(egui::Key::Escape))
        {
            self.stream_menu.open = false;
            ctx.memory_mut(|m| m.close_popup());
        }
        if !self.stream_menu.open { ctx.memory_mut(|m| m.close_popup()); }
        blocked
    }
}
