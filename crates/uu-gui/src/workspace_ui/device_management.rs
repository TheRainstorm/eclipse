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
        let label = if self.devices_hidden { "显示设备" } else { "隐藏设备" };
        if theme::menu_action(ui, true, uu_i18n::text(label)).clicked() {
            self.devices_hidden = !self.devices_hidden;
            self.management = State::default();
            actions.push(Action::DevicesHidden(self.devices_hidden));
            ui.close_menu();
        }
        if theme::menu_action(ui, true, uu_i18n::text("设备管理")).clicked() {
            self.devices_hidden = false;
            self.management.active = true;
            actions.push(Action::DevicesHidden(false));
            ui.close_menu();
        }
    }

    pub(super) fn device_management_toolbar(&mut self, ui: &mut egui::Ui, profiles: &[Profile]) {
        if !self.management.active { return; }
        self.management.selected.retain(|id| profiles.iter().any(|p| &p.id == id));
        theme::card(ui.visuals()).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.strong(uu_i18n::text("设备管理"));
                ui.label(uu_i18n::format!("已选择 {} 台设备", self.management.selected.len()));
                if ui.add(theme::secondary_button(uu_i18n::text("全选"))).clicked() {
                    self.management.selected = profiles.iter().map(|p| p.id.clone()).collect();
                }
                if ui.add(theme::secondary_button(uu_i18n::text("取消选择"))).clicked() {
                    self.management.selected.clear();
                }
                if ui.add_enabled(!self.management.selected.is_empty(), theme::danger_button(uu_i18n::text("删除所选"))).clicked() {
                    self.management.confirm_delete = true;
                }
                if ui.add(theme::primary_button(uu_i18n::text("完成"))).clicked() {
                    self.management = State::default();
                }
            });
            ui.weak(uu_i18n::text("点击设备进行选择，拖动设备调整顺序。"));
            if self.management.confirm_delete && !self.management.selected.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(uu_i18n::format!("删除所选 {} 台设备？仅移除本地记录，现有串流继续运行。", self.management.selected.len()));
                    if ui.add(theme::danger_button(uu_i18n::text("确认删除"))).clicked() {
                        self.management.delete = true;
                    }
                    if ui.add(theme::secondary_button(uu_i18n::text("取消"))).clicked() {
                        self.management.confirm_delete = false;
                    }
                });
            }
        });
        ui.add_space(16.);
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
