//! Application navigation and provider-independent streaming surfaces.
use super::App;
use eframe::egui::{self,Color32 as C,Pos2,Rect,Response,RichText,Sense,Stroke,Vec2};
pub(super) use uu_plugin_sdk::ui::*;
pub(super) fn logo(p: &egui::Painter, rect: Rect) {
    super::brand::paint_logo(p, rect);
}

impl App {
    fn title_bar(&mut self, ctx: &egui::Context) {
        let page = self.workspace.route.page;
        let wallpaper_root = page == super::workspace_ui::Page::Devices
            && matches!(self.workspace.route.device, super::workspace_ui::DeviceView::List)
            && self.preferences.wallpaper_enabled;
        let detail_name = match (&self.workspace.route.device, page) {
            (super::workspace_ui::DeviceView::Details(id), super::workspace_ui::Page::Devices) => {
                self.workspace.book.device(id).ok().map(|device| device.name.clone())
            }
            _ => None,
        };
        let current = match page {
            super::workspace_ui::Page::Devices => uu_i18n::text("设备列表").to_owned(),
            super::workspace_ui::Page::Services => uu_i18n::text("服务管理").into(),
            super::workspace_ui::Page::Plugin => self.workspace.route.plugin.as_ref().and_then(|(id,page)|self.plugins.pages(uu_plugin_sdk::Placement::Navigation).into_iter().find(|(p,x)|p==id&&&x.id==page).map(|(_,p)|p.title)).unwrap_or_else(||uu_i18n::text("插件").into()),
            super::workspace_ui::Page::Settings => uu_i18n::text("应用设置").into(),
        };
        let running = self.workspace.host.running();
        let failed = self
            .workspace
            .host
            .statuses()
            .iter()
            .any(|(_, status)| matches!(status, uu_core::device_internet::EndpointStatus::Error(_)));
        egui::TopBottomPanel::top("home-title")
            .exact_height(58.0)
            .frame(
                egui::Frame::none()
                    .fill(if wallpaper_root {
                        C::TRANSPARENT
                    } else {
                        super::theme::SURFACE
                    })
                    .inner_margin(egui::Margin::symmetric(16.0, 8.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    let (rect, response) =
                        ui.allocate_exact_size(egui::vec2(30.0, 34.0), Sense::click_and_drag());
                    logo(ui.painter(), rect);
                    if response.clicked() {
                        self.select_workspace_page(super::workspace_ui::Page::Devices);
                    }
                    if response.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    let title_color = if wallpaper_root { C::WHITE } else { INK };
                    let separator_color = if wallpaper_root {
                        C::from_white_alpha(190)
                    } else {
                        MUTED
                    };
                    if ctx.screen_rect().width() >= 520.0 {
                        ui.label(RichText::new(super::brand::NAME).size(18.0).strong().color(title_color));
                        ui.label(RichText::new("/").size(17.0).color(separator_color));
                    }
                    if let Some(name) = detail_name {
                        if ui.link(RichText::new(uu_i18n::text("设备列表")).size(17.0)).clicked() {
                            self.workspace.route.device = super::workspace_ui::DeviceView::List;
                        }
                        ui.label(RichText::new("/").size(17.0).color(separator_color));
                        ui.add(egui::Label::new(RichText::new(name).size(17.0).color(title_color)).truncate());
                    } else {
                        ui.add(
                            egui::Label::new(RichText::new(current).size(17.0).color(title_color))
                                .truncate(),
                        );
                    }
                    let controls_width = 255.0;
                    let space = (ui.available_width() - controls_width).max(0.0);
                    let (_, drag) =
                        ui.allocate_exact_size(egui::vec2(space, 34.0), Sense::click_and_drag());
                    if drag.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if drag.double_clicked() {
                        self.toggle_maximized(ctx);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, Icon::Close, uu_i18n::text("关闭")).clicked() {
                            self.disconnect();
                            self.close_at = Some(
                                std::time::Instant::now() + std::time::Duration::from_millis(200),
                            );
                        }
                        if icon_button(ui, Icon::Max, uu_i18n::text("最大化 / 还原")).clicked() {
                            self.toggle_maximized(ctx);
                        }
                        if icon_button(ui, Icon::Min, uu_i18n::text("最小化")).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                        ui.add_space(4.0);
                        if icon_button_tinted(
                            ui,
                            Icon::Settings,
                            uu_i18n::text("应用设置"),
                            page == super::workspace_ui::Page::Settings,
                            if wallpaper_root { C::WHITE } else { INK },
                        )
                        .clicked()
                        {
                            self.select_workspace_page(super::workspace_ui::Page::Settings);
                        }
                        for (id,page) in self.plugins.pages(uu_plugin_sdk::Placement::Navigation) {
                            let selected=self.workspace.route.page==super::workspace_ui::Page::Plugin&&self.workspace.route.plugin.as_ref()==Some(&(id.clone(),page.id.clone()));
                            if icon_button_selected(ui,page.icon.unwrap_or(Icon::Extension),&page.title,selected).clicked(){self.select_plugin_page(id,page.id);}
                        }
                        let response = ui.add_sized([30.0, 30.0], egui::Button::new("").frame(false));
                        let selected = page == super::workspace_ui::Page::Services;
                        if selected || response.hovered() {
                            ui.painter().rect_filled(
                                response.rect,
                                6.0,
                                C::from_rgb(222, 235, 249),
                            );
                        }
                        let color = if failed {
                            super::theme::DANGER
                        } else if running {
                            C::from_rgb(34, 197, 94)
                        } else if wallpaper_root {
                            C::WHITE
                        } else {
                            MUTED
                        };
                        icon(ui.painter(), response.rect, Icon::Globe, color);
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                ui.is_enabled(),
                                uu_i18n::text("服务管理"),
                            )
                        });
                        if response
                            .on_hover_text(if failed {
                                uu_i18n::text("服务管理 · 部分入口异常")
                            } else if running {
                                uu_i18n::text("服务管理 · 运行中")
                            } else {
                                uu_i18n::text("服务管理 · 已停止")
                            })
                            .clicked()
                        {
                            self.select_workspace_page(super::workspace_ui::Page::Services);
                        }
                        if icon_button_tinted(
                            ui,
                            Icon::Desktop,
                            uu_i18n::text("设备列表"),
                            page == super::workspace_ui::Page::Devices,
                            if wallpaper_root { C::WHITE } else { INK },
                        )
                        .clicked()
                        {
                            self.select_workspace_page(super::workspace_ui::Page::Devices);
                        }
                    });
                });
            });
    }

    pub(super) fn home_ui(&mut self, ctx: &egui::Context) {
        if self.workspace.route.page==super::workspace_ui::Page::Plugin&&!self.workspace.route.plugin.as_ref().is_some_and(|(id,_)|self.plugins.enabled(id)){self.workspace.route.page=super::workspace_ui::Page::Devices;}
        if self.workspace.route.page == super::workspace_ui::Page::Devices
            && matches!(self.workspace.route.device, super::workspace_ui::DeviceView::List)
        {
            self.background.sync(&self.preferences, ctx);
            self.background.paint_background(ctx, ctx.screen_rect());
        }
        self.title_bar(ctx);
        match self.workspace.route.page {
            super::workspace_ui::Page::Devices | super::workspace_ui::Page::Services => {
                self.workspace_home(ctx)
            }
            super::workspace_ui::Page::Plugin => self.plugin_page_ui(ctx),
            super::workspace_ui::Page::Settings => self.settings_home(ctx),
        }
    }

    pub(super) fn stream_ui(&mut self, ctx: &egui::Context, events: &[egui::Event]) {
        if !self.fullscreen { self.connection_bar(ctx); }
        let workspace_bar=if self.fullscreen {
            self.workspace_tabs.as_ref().filter(|tabs|tabs.multiple()).map(|tabs|
                egui::TopBottomPanel::top("fullscreen-device-groups").exact_height(34.)
                    .frame(egui::Frame::none().fill(super::theme::SIDEBAR))
                    .show(ctx,|ui|tabs.show(ui)).response.rect)
        }else{None};
        let was_open = self.stream_menu.owns_input(self.fullscreen) || self.displays.menu_open;
        let mut controls = self.stream_controls(ctx);
        controls.extend(workspace_bar);
        let control_pointer = !self.mouse_captured && ctx.input(|i|i.pointer.hover_pos().is_some_and(|p|controls.iter().any(|r|r.contains(p))));
        let picture_blocked = self.displays.menu_open || self.local_dialog_open() || self.window_edge_active || was_open || self.stream_menu.owns_input(self.fullscreen) || control_pointer
            || self.workspace_tabs.as_ref().is_some_and(|tabs|tabs.owns_input(ctx));
        let controls_active = picture_blocked || !self.input_ready();
        let mut messages = Vec::new();
        egui::CentralPanel::default().frame(egui::Frame::none().fill(C::BLACK)).show(ctx,|ui| {
            ui.set_clip_rect(ui.max_rect());
            let rect=ui.available_rect_before_wrap();ui.allocate_rect(rect,Sense::hover());
            let chosen=(!self.displays.waiting_binding()&&self.textures.contains_key(&self.selected_track)).then_some(self.selected_track);
            if let Some(track)=chosen {
                let input_screen = self.input_screen(track);
                let texture=&self.textures[&track];let size=texture.size_vec2();let scale=(rect.width()/size.x).min(rect.height()/size.y);
                let draw=Rect::from_center_size(rect.center(),size*scale);
                let native_cursor=self.uses_engine_controls()&&self.controller.as_ref().is_some_and(|h|h.stats.native_cursor.supported());
                let cursor=if self.uses_engine_controls()&&self.preferences.cursor_mode.remote(){self.controller.as_ref().and_then(|h|h.stats.native_cursor.snapshot())}else{None};
                texture.set_native_cursor(cursor);
                texture.paint(ui,draw);
                if self.unpresented.remove(&track) { texture.record_new_image();*self.frame_count.entry(track).or_insert(0) += 1; }
                let pointer=ctx.input(|i|i.pointer.hover_pos());
                let hud_pointer=self.hud&&self.paint_hud(ui,rect);
                let controls_active=controls_active||hud_pointer;
                if events.iter().any(|ev|matches!(ev,egui::Event::PointerButton {pos,pressed:true,..} if draw.contains(*pos))) && self.active_session && self.input_capture.activate_picture(self.input_conditions(ctx),picture_blocked||hud_pointer) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    self.platform.focus_window();
                }
                if controls_active {messages.extend(self.pointer.release_all(input_screen));}
                if self.input_capture.pointer(self.input_conditions(ctx),controls_active) {
                    if self.preferences.relative_mouse {
                        if self.mouse_captured {messages.extend(self.pointer.events_relative(events,input_screen));}
                    } else {messages.extend(self.pointer.events(events,draw,pointer,input_screen));}
                }
                let remote_available=if self.uses_engine_controls(){native_cursor}else{self.remote_cursor_active};
                let local_cursor=self.preferences.cursor_mode.local_visible(remote_available);
                if !local_cursor && !controls_active && pointer.is_some_and(|p|draw.contains(p)) {ctx.set_cursor_icon(egui::CursorIcon::None);}
                if !self.uses_engine_controls() && local_cursor && !self.preferences.relative_mouse && !controls_active {
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
                        if ui.put(Rect::from_center_size(center+egui::vec2(0.0,44.0),egui::vec2(156.0,34.0)),egui::Button::new(uu_i18n::text("重试当前显示器"))).clicked(){self.select_remote_screen(id);}
                    }
                    if !self.disconnecting && ui.put(Rect::from_center_size(center+egui::vec2(0.0,94.0),egui::vec2(116.0,34.0)),egui::Button::new(uu_i18n::text("取消连接"))).clicked(){self.disconnect();}
                } else {
                    if ui.put(Rect::from_center_size(center+egui::vec2(-70.0,54.0),egui::vec2(124.0,36.0)),egui::Button::new(RichText::new(uu_i18n::text("重新连接")).color(C::WHITE)).fill(BLUE)).clicked(){self.connect();}
                    if ui.put(Rect::from_center_size(center+egui::vec2(70.0,54.0),egui::vec2(124.0,36.0)),egui::Button::new(uu_i18n::text("关闭串流窗口"))).clicked(){self.disconnect();}
                }
            }
            if chosen.is_some() && !self.displays.notice.is_empty() {
                let notice=Rect::from_center_size(rect.center_top()+egui::vec2(0.0,38.0),egui::vec2((rect.width()-40.0).clamp(1.0,480.0),44.0));
                ui.painter().rect_filled(notice,6.0,C::from_black_alpha(160));
                ui.put(notice,egui::Label::new(RichText::new(&self.displays.notice).color(C::WHITE)).wrap());
            }
            if chosen.is_some() && self.fullscreen && !ctx.input(|i|i.focused) {
                let hint=Rect::from_center_size(rect.center_bottom()-egui::vec2(0.0,54.0),egui::vec2((rect.width()-40.0).clamp(1.0,360.0),40.0));
                ui.painter().rect_filled(hint,6.0,C::from_black_alpha(170));
                ui.put(hint,egui::Label::new(RichText::new(uu_i18n::text("点击画面以获取键盘控制")).color(C::WHITE)).wrap());
            }
            if self.hud && chosen.is_none() { self.paint_hud(ui, rect); }

        });
        for message in messages {
            self.send_input(message, 15);
        }
    }
}
