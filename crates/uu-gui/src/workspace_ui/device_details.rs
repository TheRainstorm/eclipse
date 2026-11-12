//! A centered, responsive device page. Identity details stay secondary to
//! connecting, choosing a route, and editing the current streaming profile.
use super::{card, muted, primary, theme, Action, DeviceView, Profile, Workspace};
use eframe::egui::{self,RichText};

impl Workspace {
    pub(super) fn device_details(&mut self,ui:&mut egui::Ui,id:&str,actions:&mut Vec<Action>) {
        let profile=match self.book.device(id).cloned() {
            Ok(profile)=>profile,
            Err(error)=>{self.feedback.error(super::super::ui_feedback::Scope::Devices,error.to_string());self.route.device=DeviceView::List;return;}
        };
        self.statistics.poll(self.service_config.as_ref().map(|config|config.at("stream-statistics.json")),ui.ctx());
        let width=ui.available_width().min(1160.);
        let inset=((ui.available_width()-width)*0.5).max(0.);
        ui.horizontal_top(|ui| {
            ui.add_space(inset);
            ui.vertical(|ui| {
                ui.set_width(width);
                self.feedback.show(ui,super::super::ui_feedback::Scope::DeviceDetails(id.to_owned()));
                if width>=740. {
                    ui.horizontal_top(|ui| {
                        self.device_picture(ui,&profile,300.);
                        ui.add_space(24.);
                        ui.vertical(|ui|{ui.set_width((width-324.).max(240.));self.device_summary(ui,&profile,actions);});
                    });
                }else {
                    self.device_picture(ui,&profile,width.min(320.));
                    ui.add_space(14.);self.device_summary(ui,&profile,actions);
                }
                ui.add_space(22.);
                if width>=1000. {
                    let left=(width-24.)*0.44;
                    let item_gap=ui.spacing().item_spacing.x;
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x=24.;
                        ui.allocate_ui_with_layout(egui::vec2(left,0.),egui::Layout::top_down(egui::Align::Min),|ui| {
                            ui.spacing_mut().item_spacing.x=item_gap;
                            ui.set_width(left);
                            card(ui,|ui|self.device_routes(ui,&profile));
                        });
                        ui.allocate_ui_with_layout(egui::vec2(width-left-24.,0.),egui::Layout::top_down(egui::Align::Min),|ui| {
                            ui.spacing_mut().item_spacing.x=item_gap;
                            ui.set_width(width-left-24.);
                            card(ui,|ui|self.client_config_list(ui,&profile));
                        });
                    });
                }else {
                    card(ui,|ui|self.device_routes(ui,&profile));
                    card(ui,|ui|self.client_config_list(ui,&profile));
                }
                ui.collapsing(uu_i18n::text("设备身份与配对详情"),|ui| {
                    if let Some(peer)=&profile.peer {
                        muted(ui,uu_i18n::text("设备公钥指纹"));
                        ui.add(egui::Label::new(RichText::new(peer).monospace()).wrap());
                        if ui.add(theme::secondary_button(uu_i18n::text("复制指纹")).small()).clicked(){ui.ctx().copy_text(peer.clone());}
                    }else{ui.weak(uu_i18n::text("首次连接时，被控端输入控制端显示的短码即可保存授权。"));}
                });
                ui.add_space(12.);
                ui.horizontal(|ui| {ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui| {
                    if ui.add(theme::secondary_button(uu_i18n::text("移除此设备…"))).clicked(){self.confirm_device_removal(&profile);}
                });});
            });
        });
    }

    fn device_summary(&mut self,ui:&mut egui::Ui,profile:&Profile,actions:&mut Vec<Action>) {
        ui.horizontal_wrapped(|ui| {
            ui.heading(&profile.name);
            if primary(ui,uu_i18n::text("连接此设备")).clicked(){actions.push(Action::Open(profile.id.clone()));}
        });
        ui.add_space(6.);
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(if profile.peer.is_some(){theme::palette(ui.visuals()).success}else{theme::palette(ui.visuals()).muted},uu_i18n::text(if profile.peer.is_some(){"已完成设备配对"}else{"首次连接需要配对"}));
            muted(ui,super::last_connection(profile.last_connected));
        });
        if let Some(route)=profile.route().ok().filter(|route|self.route_visible(route)) {muted(ui,self.route_label(route));}
        muted(ui,uu_i18n::format!("累计串流时长：{}",self.statistics.device_time(&profile.id)));
        ui.add_space(10.);
        let mut pinned=profile.preview.as_ref().is_some_and(|preview|preview.pinned);
        if ui.add_enabled_ui(profile.preview.is_some(),|ui|theme::switch(ui,&mut pinned,uu_i18n::text("固定当前截图"))).inner
            .on_hover_text(uu_i18n::text("固定后保留这张截图；取消固定后，下次串流结束时自动更新")).changed() {
            if let (Some(store),Some(preview))=(&self.store,&profile.preview) {
                if let Err(error)=store.pin_preview(&profile.id,&preview.id,pinned){self.feedback.error(self.device_scope(),uu_i18n::format!("无法保存截图设置：{error:#}"));}
                self.refresh();
            }
        }
    }

    fn device_routes(&mut self,ui:&mut egui::Ui,profile:&Profile) {
        ui.horizontal(|ui| {
            ui.strong(uu_i18n::text("连接方式"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui| {
                if ui.add(theme::secondary_button(uu_i18n::text("添加地址")).small()).clicked(){self.new_device(Some(profile.id.clone()),None);}
            });
        });
        ui.add_space(8.);
        let routes:Vec<_>=profile.routes.iter().filter(|route|self.route_visible(route)).collect();
        if routes.is_empty(){ui.weak(uu_i18n::text("请选择可用连接方式"));}
        for route in routes {
            ui.push_id(&route.id,|ui| {
                ui.horizontal(|ui| {
                    let label=self.route_label(route);
                    let text_width=(ui.available_width()-28.-ui.spacing().item_spacing.x).max(60.);
                    let choose=ui.allocate_ui_with_layout(egui::vec2(text_width,36.),egui::Layout::left_to_right(egui::Align::Center),|ui| {
                        ui.set_min_size(ui.available_size());
                        let radio=ui.radio(profile.selected_route==route.id,"");
                        let text=ui.add(egui::Label::new(&label).truncate().sense(egui::Sense::click())).on_hover_text(&label);
                        radio.clicked()||text.clicked()
                    }).inner;
                    if choose {
                        let mut updated=profile.clone();updated.selected_route=route.id.clone();
                        if let Some(store)=&self.store {if let Err(error)=store.save_device(updated){self.feedback.error(self.device_scope(),uu_i18n::format!("无法保存连接方式：{e:#}",e=error));}}
                        self.refresh();
                    }
                    uu_plugin_sdk::ui::icon_menu(ui,uu_plugin_sdk::ui::Icon::More,uu_i18n::text("更多"),|ui| {
                        if theme::menu_action(ui,true,uu_i18n::text("编辑")).clicked(){self.new_device(Some(profile.id.clone()),Some(route.id.clone()));ui.close_menu();}
                        if theme::menu_action(ui,profile.routes.len()>1,uu_i18n::text("移除")).clicked() {
                            let mut updated=profile.clone();updated.routes.retain(|item|item.id!=route.id);
                            if updated.selected_route==route.id {updated.selected_route=updated.routes[0].id.clone();}
                            if let Some(store)=&self.store {match store.save_device(updated){Ok(())=>self.refresh(),Err(error)=>self.feedback.error(self.device_scope(),uu_i18n::format!("无法移除地址：{e}",e=error))}}
                            ui.close_menu();
                        }
                    });
                });
            });
        }
    }
}
