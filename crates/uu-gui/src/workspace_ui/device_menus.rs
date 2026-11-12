//! Device actions and library actions have separate menus and share saved state.
use super::{compact_context_menu, muted, theme, Action, DeviceView, Info, Profile, Workspace};
use eframe::egui;
use uu_core::device_book::Destination;

#[derive(Default)]
pub(super) struct State {
    configs: Option<String>,
    remove: Option<String>,
    error: String,
}

impl Workspace {
    pub(super) fn library_menu(&mut self, ui: &mut egui::Ui, info: &Info, actions: &mut Vec<Action>) {
        compact_context_menu(ui);
        if theme::menu_action(ui, true, uu_i18n::text("添加设备…")).clicked() {
            self.new_device(None, None); ui.close_menu();
        }
        ui.add_enabled_ui(!info.nearby.is_empty(), |ui| {
            ui.menu_button(uu_i18n::text("添加附近设备"), |ui| {
                for (name, address) in &info.nearby {
                    if theme::menu_action(ui, true, name).on_hover_text(address).clicked() {
                        if let Some(store) = &self.store {
                            if let Err(error) = store.upsert_device(name, Destination::Ip { address: address.clone(), legacy_device: None }, None) {
                                self.feedback.error(super::super::ui_feedback::Scope::Devices, uu_i18n::format!("无法添加设备：{error:#}"));
                            }
                            self.refresh();
                        }
                        ui.close_menu();
                    }
                }
            });
        });
        ui.separator();
        if theme::menu_action(ui, true, uu_i18n::text("刷新全部在线状态")).clicked() {
            self.presence.refresh(); ui.close_menu();
        }
        self.device_management_menu(ui, actions);
        ui.separator();
        ui.menu_button(uu_i18n::text("壁纸"), |ui| {
            if theme::menu_action(ui, info.wallpaper_ready, uu_i18n::text("收藏当前壁纸")).clicked() {
                actions.push(Action::FavoriteWallpaper); ui.close_menu();
            }
            if theme::menu_action(ui, info.wallpaper_download_ready, uu_i18n::text("下载当前壁纸…")).clicked() {
                actions.push(Action::DownloadWallpaper); ui.close_menu();
            }
            if theme::menu_action(ui, info.wallpaper_enabled, uu_i18n::text("刷新壁纸")).clicked() {
                actions.push(Action::RefreshWallpaper); ui.close_menu();
            }
            if let Some((width, height)) = info.wallpaper_size {
                ui.separator();
                ui.weak(uu_i18n::format!("原图：{} × {} 像素", width, height));
            }
        });
        ui.separator();
        if theme::menu_action(ui,true,uu_i18n::text("串流统计…")).clicked(){self.statistics.open=true;ui.close_menu();}
        if theme::menu_action(ui, true, uu_i18n::text("本机串流测试")).on_hover_text(uu_i18n::text("只读采集本机画面，通过本机回环完成编解码和显示；无需开启共享服务。")).clicked() {
            actions.push(Action::SelfStream); ui.close_menu();
        }
    }

    pub(super) fn device_menu(&mut self, ui: &mut egui::Ui, profile: &Profile, actions: &mut Vec<Action>) {
        compact_context_menu(ui);
        let routes: Vec<_> = profile.routes.iter().filter(|route| self.route_visible(route)).collect();
        let connectable = routes.iter().any(|route| route.id == profile.selected_route);
        if theme::menu_action(ui, connectable, uu_i18n::text("连接此设备")).clicked() {
            actions.push(Action::Open(profile.id.clone())); ui.close_menu();
        }
        ui.add_enabled_ui(!routes.is_empty(), |ui| {
            ui.menu_button(uu_i18n::text("连接方式"), |ui| {
                for route in routes {
                    if theme::selectable_label(ui, profile.selected_route == route.id, self.route_label(route)).clicked() {
                        let mut updated = profile.clone(); updated.selected_route = route.id.clone();
                        if let Some(store) = &self.store {
                            if let Err(error) = store.save_device(updated) {
                                self.feedback.error(self.device_scope(), uu_i18n::format!("无法保存连接方式：{e:#}", e=error));
                            }
                        }
                        self.refresh(); ui.close_menu();
                    }
                }
            });
        });
        ui.separator();
        ui.menu_button(uu_i18n::text("串流配置"), |ui| {
            for (id, name) in std::iter::once((None, uu_i18n::text("默认配置")))
                .chain(profile.client_configs.iter().map(|value| (Some(value.id.clone()), value.name.as_str()))) {
                if theme::selectable_label(ui, profile.selected_client == id, name).clicked() {
                    if let Some(store) = &self.store {
                        if let Err(error) = store.select_client_config(&profile.id, id) {
                            self.feedback.error(self.device_scope(), uu_i18n::format!("无法选择配置：{e:#}", e=error));
                        }
                    }
                    self.refresh(); ui.close_menu();
                }
            }
            ui.separator();
            if theme::menu_action(ui, true, uu_i18n::text("编辑当前配置…")).clicked() {
                self.edit_selected_client_config(profile); ui.close_menu();
            }
            if theme::menu_action(ui, true, uu_i18n::text("新建配置…")).clicked() {
                self.edit_client_config(profile, None, false); ui.close_menu();
            }
            if theme::menu_action(ui, true, uu_i18n::text("管理配置…")).clicked() {
                self.menus.configs = Some(profile.id.clone()); ui.close_menu();
            }
        });
        ui.separator();
        if theme::menu_action(ui, true, uu_i18n::text("刷新在线状态")).clicked() {
            self.presence.refresh(); ui.close_menu();
        }
        if theme::menu_action(ui, true, uu_i18n::text("设备详情…")).clicked() {
            self.route.device = DeviceView::Details(profile.id.clone()); ui.close_menu();
        }
        ui.separator();
        if theme::menu_action(ui, self.store.is_some(), uu_i18n::text("移除此设备…")).clicked() {
            self.confirm_device_removal(profile); ui.close_menu();
        }
    }

    pub(super) fn edit_selected_client_config(&mut self, profile: &Profile) {
        if let Some(value) = profile.client_configs.iter().find(|value| Some(&value.id) == profile.selected_client.as_ref()) {
            self.edit_client_config(profile, Some(value.clone()), false);
        } else {
            self.edit_global_client_config();
        }
    }

    pub(super) fn client_config_list(&mut self, ui: &mut egui::Ui, profile: &Profile) {
        muted(ui, uu_i18n::text("每台设备可保存多个配置；选择全局默认会跟随应用设置。修改配置只影响下次连接。"));
        self.client_config_picker(ui, profile);
        for value in &profile.client_configs {
            ui.push_id(&value.id, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.strong(&value.name);
                    muted(ui, format!("{} FPS · {}", value.settings.fps, value.settings.codec.label()));
                    if ui.add(theme::secondary_button(uu_i18n::text("编辑")).small()).clicked() { self.edit_client_config(profile, Some(value.clone()), false); }
                    if ui.add(theme::secondary_button(uu_i18n::text("复制")).small()).clicked() { self.edit_client_config(profile, Some(value.clone()), true); }
                    if ui.add(theme::secondary_button(uu_i18n::text("删除")).small()).clicked() {
                        if let Some(store) = &self.store {
                            match store.remove_client_config(&profile.id, &value.id) {
                                Ok(()) => self.refresh(),
                                Err(error) => self.feedback.error(self.device_scope(), uu_i18n::format!("无法删除配置：{e:#}", e=error)),
                            }
                        }
                    }
                });
            });
        }
        if ui.add(theme::secondary_button(uu_i18n::text("新建配置…"))).clicked() { self.edit_client_config(profile, None, false); }
    }

    pub(super) fn confirm_device_removal(&mut self, profile: &Profile) {
        self.menus.remove = Some(profile.id.clone()); self.menus.error.clear();
    }

    pub(super) fn device_menu_dialogs(&mut self, ctx: &egui::Context) {
        if let Some(id) = self.menus.configs.clone() {
            let mut open = true;
            if let Ok(profile) = self.book.device(&id).cloned() {
                egui::Window::new(uu_i18n::text("管理串流配置")).id("device-config-manager".into()).open(&mut open)
                    .collapsible(false).default_width(620.).anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                    .show(ctx, |ui| {
                        ui.strong(&profile.name);
                        ui.add_enabled_ui(self.client_editor.is_none(), |ui| {
                            egui::ScrollArea::vertical().max_height((ctx.screen_rect().height()-180.).max(160.)).show(ui, |ui| { self.client_config_list(ui, &profile); });
                        });
                    });
            } else { open = false; }
            if !open { self.menus.configs = None; }
        }
        if let Some(id) = self.menus.remove.clone() {
            let mut open = true; let mut remove = false; let mut cancel = false;
            if let Ok(profile) = self.book.device(&id) {
                egui::Window::new(uu_i18n::text("移除此设备…")).id("device-remove-confirmation".into()).open(&mut open)
                    .collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO).show(ctx, |ui| {
                        ui.label(uu_i18n::format!("移除设备“{}”？", profile.name));
                        ui.label(uu_i18n::text("仅移除本地记录，现有串流和被控端授权不受影响。"));
                        if !self.menus.error.is_empty() { ui.colored_label(theme::palette(ui.visuals()).danger, &self.menus.error); }
                        ui.horizontal(|ui| {
                            remove = ui.add(theme::danger_button(uu_i18n::text("确认移除"))).clicked();
                            cancel = ui.add(theme::secondary_button(uu_i18n::text("取消"))).clicked();
                        });
                    });
            } else { open = false; }
            if remove {
                let result = self.store.as_ref().ok_or_else(|| anyhow::anyhow!(uu_i18n::text("设备配置不可用"))).and_then(|store| store.forget(&id));
                match result {
                    Ok(()) => {
                        if matches!(&self.route.device, DeviceView::Details(current) if current == &id) { self.route.device = DeviceView::List; }
                        if self.menus.configs.as_ref() == Some(&id) { self.menus.configs = None; }
                        if self.client_editor.as_ref().is_some_and(|editor| editor.device == id) { self.client_editor = None; }
                        self.refresh(); open = false;
                    }
                    Err(error) => self.menus.error = error.to_string(),
                }
            }
            if !open || cancel { self.menus.remove = None; self.menus.error.clear(); }
        }
    }
}
