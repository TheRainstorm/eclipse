//! Connection chrome and the optional floating control menu.
use super::ui::{icon, icon_button, Icon};
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Stroke};

pub(super) struct StreamMenu {
    pub visible: bool,
    pub open: bool,
    pub anchor: Pos2,
    position: Option<Pos2>,
}
impl Default for StreamMenu {
    fn default() -> Self {
        Self {
            visible: true,
            open: false,
            anchor: Pos2::ZERO,
            position: None,
        }
    }
}

impl super::App {
    pub(super) fn connection_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("connection-top")
            .exact_height(36.0)
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
                    ui.menu_button(format!("▣  显示屏 {}", self.selected_track + 1), |ui| {
                        self.display_menu(ui)
                    });
                    if self.view_only {
                        ui.label(RichText::new("仅观看").color(Color32::from_rgb(98, 125, 160)));
                    }
                    if let Some(controller) = &self.controller {
                        ui.label(
                            RichText::new(format!(
                                "▥  {:.1} Mbps",
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
                        if icon_button(ui, Icon::Max, "切换全屏 · Ctrl+Alt+F").clicked() {
                            self.toggle_fullscreen(ctx);
                        }
                        if icon_button(ui, Icon::Min, "最小化").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                        let control = ui.button("控制中心");
                        if control.clicked() {
                            self.stream_menu.open = !self.stream_menu.open;
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
                for message in self.pointer.release_all(self.selected_track + 1) {
                    self.send_input(message, 15);
                }
                self.selected_track = track;
                ui.close_menu();
            }
        }
        if self.textures.is_empty() {
            ui.add_enabled(false, egui::Label::new("正在等待显示屏…"));
        }
    }

    /// Draw first so the stream can exclude every control rectangle from input.
    pub(super) fn stream_controls(&mut self, ctx: &egui::Context) -> Vec<Rect> {
        let mut blocked = Vec::new();
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
                            ui.menu_button(
                                format!("▣  显示屏 {}", self.selected_track + 1),
                                |ui| self.display_menu(ui),
                            );
                            ui.menu_button("▧  画质", |ui| {
                                let before = (self.bitrate_idx, self.custom_mbps);
                                for (index, (label, _)) in super::BITRATE_PRESETS.iter().enumerate()
                                {
                                    ui.selectable_value(&mut self.bitrate_idx, index, *label);
                                }
                                if self.bitrate_idx == 5 {
                                    ui.add(
                                        egui::Slider::new(&mut self.custom_mbps, 1..=100)
                                            .suffix(" Mbps"),
                                    );
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
                                    ui.selectable_value(&mut self.resolution_idx, index, *label);
                                }
                                if previous_resolution != self.resolution_idx {
                                    self.send_bitrate();
                                }
                                if before != (self.bitrate_idx, self.custom_mbps) {
                                    self.send_bitrate();
                                }
                            });
                            ui.menu_button("□  窗口", |ui| {
                                if ui
                                    .button(if self.fullscreen {
                                        "退出全屏"
                                    } else {
                                        "进入全屏"
                                    })
                                    .clicked()
                                {
                                    self.toggle_fullscreen(ctx);
                                    self.stream_menu.open = false;
                                    ui.close_menu();
                                }
                                ui.checkbox(&mut self.stream_menu.visible, "全屏显示悬浮入口");
                                ui.label(RichText::new("Ctrl+Alt+M 可随时恢复入口").small().weak());
                            });
                            ui.add_enabled(false, egui::Button::new("♬  声音"))
                                .on_hover_text("音频尚未接入");
                            ui.menu_button("♧  安全", |ui| {
                                if ui
                                    .checkbox(&mut self.view_only, "仅观看，禁止远程输入")
                                    .changed()
                                {
                                    self.release_all_keys();
                                    for message in self.pointer.release_all(self.selected_track + 1)
                                    {
                                        self.send_input(message, 15);
                                    }
                                    self.remote_focus = false;
                                }
                            });
                            ui.add_enabled(false, egui::Button::new("♧  外设"))
                                .on_hover_text("外设透传尚未接入");
                            ui.add_enabled(false, egui::Button::new("▣  工具"))
                                .on_hover_text("文件、终端和端口映射尚未接入");
                            ui.menu_button("▦  更多", |ui| {
                                ui.checkbox(&mut self.hud, "串流统计 · F1");
                                if ui.button("隐藏悬浮入口 · Ctrl+Alt+M").clicked() {
                                    self.stream_menu.visible = false;
                                    self.stream_menu.open = false;
                                    ui.close_menu();
                                }
                                if ui.button("连接设置").clicked() {
                                    self.show_settings = true;
                                    self.stream_menu.open = false;
                                    ui.close_menu();
                                }
                            });
                            ui.separator();
                            if ui
                                .button(if self.fullscreen {
                                    "⛶  退出全屏                       Ctrl+Alt+F"
                                } else {
                                    "⛶  进入全屏                       Ctrl+Alt+F"
                                })
                                .clicked()
                            {
                                self.toggle_fullscreen(ctx);
                                self.stream_menu.open = false;
                            }
                            if ui
                                .button(
                                    RichText::new("↪  退出远控")
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
        }
        blocked
    }
}
