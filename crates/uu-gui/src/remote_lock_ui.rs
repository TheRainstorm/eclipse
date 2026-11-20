//! Client actions for the host-owned workstation lock policy.
use eframe::egui;
impl crate::App {
    pub(super) fn current_stream_exit(&mut self) {
        self.stream_menu.open = false;
        if let Some(tabs) = &self.workspace_tabs {
            tabs.actions
                .borrow_mut()
                .push(crate::stream_workspace::Action::CloseGroup(tabs.active));
        } else {
            self.disconnect();
        }
    }
    pub(super) fn remote_lock_menu(&mut self, ui: &mut egui::Ui) {
        let Some(stats) = self.controller.as_ref().map(|h| h.stats.clone()) else {
            return;
        };
        let Some(status) = stats.desktop_lock.status() else {
            return;
        };
        ui.separator();
        let supported = stats.desktop_lock.exit_lock_supported();
        let can_change = supported && status.can_lock && status.originally_locked.is_some();
        let mut exit_lock = stats.desktop_lock.exit_lock_override()
            .filter(|_| can_change)
            .unwrap_or(status.originally_locked.is_some() && status.locks_on_exit());
        let hint = if !supported {
            "请更新远端 Eclipse 后台以修改本次串流的退出锁屏设置。"
        } else if !status.can_lock {
            "当前会话没有锁定远端的权限"
        } else if status.originally_locked.is_none() {
            "初始锁屏状态未知，本次不会自动锁屏"
        } else {
            "本次串流退出时生效。主机有其它会话时，继续保留远端桌面。"
        };
        if ui.add_enabled_ui(can_change, |ui| {
            crate::theme::switch(ui, &mut exit_lock, uu_i18n::text("退出当前串流后锁屏"))
        }).inner.on_hover_text(uu_i18n::text(hint)).changed() {
            stats.desktop_lock.set_exit_lock(exit_lock);
            if let Some(tabs) = &self.workspace_tabs {
                tabs.actions.borrow_mut().push(crate::stream_workspace::Action::SetExitLock(tabs.active, exit_lock));
            }
        }
        if let Some(error) = &status.error {
            ui.colored_label(crate::theme::palette(ui.visuals()).warning, error);
        }
        for (disconnect, label) in [(false, "锁定远端"), (true, "锁定远端并断开")] {
            if crate::theme::menu_action(ui, status.can_lock && !stats.desktop_lock.busy(), uu_i18n::text(label))
                .on_hover_text(uu_i18n::text("锁定远端的整个 Windows 桌面，会影响其他正在观看的客户端；断开只退出当前设备的串流。"))
                .clicked() {
                self.release_all_keys(); self.release_pointer_capture(ui.ctx());
                match stats.desktop_lock.lock(disconnect) {
                    Ok(()) => self.native_ui.feedback.report(crate::ui_feedback::Scope::Stream, crate::ui_feedback::Kind::Progress,
                        uu_i18n::text("正在等待远端确认锁屏")),
                    Err(error) => self.native_ui.feedback.error(crate::ui_feedback::Scope::Stream, error.to_string()),
                }
                self.stream_menu.open = false; ui.close_menu();
            }
        }
    }
    pub(super) fn poll_remote_lock(&mut self, ctx: &egui::Context) {
        let Some(stats) = self.controller.as_ref().map(|h| h.stats.clone()) else {
            return;
        };
        if let Some(result) = stats.desktop_lock.take_result() {
            if let Some(error) = result.error {
                self.native_ui
                    .feedback
                    .error(crate::ui_feedback::Scope::Stream, error);
            } else if result.disconnect {
                self.current_stream_exit();
            } else {
                self.native_ui.feedback.success(
                    crate::ui_feedback::Scope::Stream,
                    uu_i18n::text("已确认远端锁屏"),
                );
            }
        }
        if stats.desktop_lock.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}
