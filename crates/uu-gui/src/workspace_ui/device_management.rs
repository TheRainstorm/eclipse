//! Device management edits the catalog; it never owns live connections.
use super::{theme, Action, Profile, Workspace};
use eframe::egui;
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct State {
    pub active: bool,
    pub selected: BTreeSet<String>,
    confirm_delete: bool,
    delete: bool,
    reorder: Option<(String, String, bool)>,
}
struct Drag(String);

impl Workspace {
    pub(super) fn device_management_menu(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        if theme::menu_action(ui, true, uu_i18n::text("批量管理设备…")).clicked() {
            self.devices_hidden = false;
            self.management.active = true;
            actions.push(Action::DevicesHidden(false));
            ui.close_menu();
        }
        let label = if self.devices_hidden { "显示设备列表" } else { "隐藏设备列表" };
        if theme::menu_action(ui, true, uu_i18n::text(label)).clicked() {
            self.devices_hidden = !self.devices_hidden;
            self.management = State::default();
            actions.push(Action::DevicesHidden(self.devices_hidden));
            ui.close_menu();
        }
    }

    pub(super) fn device_management_toolbar(&mut self, ui: &mut egui::Ui, profiles: &[Profile]) {
        if !self.management.active { return; }
        self.management.selected.retain(|id| profiles.iter().any(|p| &p.id == id));
        let bounds=ui.clip_rect().intersect(ui.ctx().screen_rect());
        egui::Area::new(ui.id().with("device-management-controls"))
            .order(egui::Order::Foreground).movable(false).pivot(egui::Align2::RIGHT_BOTTOM)
            .fixed_pos(bounds.right_bottom()-egui::vec2(16.,16.)).constrain_to(bounds)
            .show(ui.ctx(),|ui| {
                ui.set_width(if self.management.confirm_delete && !self.management.selected.is_empty(){280.}else{140.});
                if self.management.confirm_delete && !self.management.selected.is_empty() {
                    egui::Frame::popup(ui.style()).show(ui,|ui| {
                        ui.set_max_width(280.);
                        ui.label(uu_i18n::format!("删除选中的 {} 台设备？",self.management.selected.len()));
                        ui.horizontal(|ui| {
                            if ui.add(theme::secondary_button(uu_i18n::text("取消"))).clicked(){self.management.confirm_delete=false;}
                            if ui.add(theme::danger_button(uu_i18n::text("确认删除"))).clicked(){self.management.delete=true;}
                        });
                    });
                    ui.add_space(10.);
                }
                ui.allocate_ui_with_layout(egui::vec2(ui.available_width(),40.),egui::Layout::right_to_left(egui::Align::Center),|ui| {
                    ui.spacing_mut().item_spacing.x=10.;
                    if floating_button(ui,crate::ui::Icon::Close,uu_i18n::text("结束管理"),true,false).clicked(){self.management=State::default();}
                    let count=self.management.selected.len();
                    let remove=floating_button(ui,crate::ui::Icon::Trash,uu_i18n::text("删除所选"),count>0,true);
                    if remove.clicked(){self.management.confirm_delete=true;}
                    if count>0 {
                        let center=remove.rect.right_top()+egui::vec2(-2.,2.);
                        ui.painter().circle_filled(center,9.,theme::palette(ui.visuals()).danger);
                        ui.painter().text(center,egui::Align2::CENTER_CENTER,count.to_string(),egui::FontId::proportional(11.),egui::Color32::WHITE);
                    }
                    let all=count==profiles.len() && !profiles.is_empty();
                    if floating_button(ui,crate::ui::Icon::SelectAll,uu_i18n::text(if all {"取消选择"}else{"全选"}),!profiles.is_empty(),false).clicked(){
                        self.management.selected=if all {BTreeSet::new()}else{profiles.iter().map(|p|p.id.clone()).collect()};
                        self.management.confirm_delete=false;
                    }
                });
            });
    }

    pub(super) fn device_management_drag(&mut self, ui: &egui::Ui, response: &egui::Response, id: &str) {
        if !self.management.active { return; }
        response.dnd_set_drag_payload(Drag(id.into()));
        if let Some(drag) = response.dnd_hover_payload::<Drag>().filter(|drag| drag.0 != id) {
            let after = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| p.x >= response.rect.center().x);
            let x = if after { response.rect.right() + 7. } else { response.rect.left() - 7. };
            ui.painter().line_segment([egui::pos2(x, response.rect.top()), egui::pos2(x, response.rect.bottom())], egui::Stroke::new(3., theme::palette(ui.visuals()).primary));
            if response.dnd_release_payload::<Drag>().is_some() {
                self.management.reorder = Some((drag.0.clone(), id.into(), after));
            }
        }
    }

    pub(super) fn finish_device_management(&mut self) {
        let deleting = std::mem::take(&mut self.management.delete);
        let reorder = self.management.reorder.take();
        if !deleting && reorder.is_none() { return; }
        let result = (|| -> anyhow::Result<()> {
            let store = self.store.as_ref().ok_or_else(|| anyhow::anyhow!(uu_i18n::text("设备配置不可用")))?;
            if deleting {
                let ids: Vec<_> = self.management.selected.iter().cloned().collect();
                store.forget_many(&ids)?;
            } else if let Some((source, target, after)) = reorder {
                store.reorder_device(&source, &target, after)?;
            }
            Ok(())
        })();
        self.refresh();
        if deleting && result.is_ok() { self.management.selected.clear(); self.management.confirm_delete = false; }
        match result {
            Ok(()) => self.feedback.success(crate::ui_feedback::Scope::Devices, uu_i18n::text(if deleting { "已删除所选设备" } else { "设备顺序已保存" })),
            Err(error) => self.feedback.error(crate::ui_feedback::Scope::Devices, uu_i18n::format!("设备管理操作失败：{error:#}")),
        }
    }
}

fn floating_button(ui:&mut egui::Ui,icon:crate::ui::Icon,label:&str,enabled:bool,danger:bool)->egui::Response {
    ui.add_enabled_ui(enabled,|ui| {
        let (rect,response)=ui.allocate_exact_size(egui::Vec2::splat(40.),egui::Sense::click());
        response.widget_info(||egui::WidgetInfo::labeled(egui::WidgetType::Button,ui.is_enabled(),label));
        let palette=theme::palette(ui.visuals());
        let hovered=response.hovered()||response.has_focus();
        let fill=if response.is_pointer_button_down_on(){palette.pressed}else if hovered{palette.hover}else{palette.overlay.linear_multiply(0.94)};
        ui.painter().circle_filled(rect.center()+egui::vec2(0.,2.),20.,egui::Color32::from_black_alpha(25));
        ui.painter().circle_filled(rect.center(),20.,fill);
        ui.painter().circle_stroke(rect.center(),20.,egui::Stroke::new(1.,if hovered{palette.primary}else{palette.border}));
        crate::ui::icon(ui.painter(),rect,icon,if !ui.is_enabled(){palette.muted}else if danger{palette.danger}else{palette.text});
        #[cfg(test)]
        ui.ctx().data_mut(|data|data.insert_temp(egui::Id::new(("batch-button",label)),rect));
        response.on_hover_text(label)
    }).inner
}
