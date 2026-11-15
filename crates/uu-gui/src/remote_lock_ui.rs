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
        ui.weak(exit_text(&status));
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
fn exit_text(status: &uu_core::host_lock::Status) -> &'static str {
    uu_i18n::text(if status.policy == uu_core::host_lock::Policy::Never {
        "退出后保持当前桌面"
    } else if status.originally_locked.is_none() {
        "初始锁屏状态未知，本次不会自动锁屏"
    } else if status.locks_on_exit() {
        if status.sessions <= 1 {
            "退出当前串流后锁屏"
        } else {
            "最后一个会话结束后锁屏；关闭单个屏幕不会锁屏"
        }
    } else {
        "连接前桌面未锁定，退出后保持当前桌面"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exit_explanation_distinguishes_last_session_original_state_and_unknown_state() {
        let mut status = uu_core::host_lock::Status {
            policy: uu_core::host_lock::Policy::Restore,
            originally_locked: Some(true),
            sessions: 1,
            can_lock: true,
            error: None,
        };
        assert_eq!(exit_text(&status), uu_i18n::text("退出当前串流后锁屏"));
        status.sessions = 2;
        assert_eq!(
            exit_text(&status),
            uu_i18n::text("最后一个会话结束后锁屏；关闭单个屏幕不会锁屏")
        );
        status.originally_locked = Some(false);
        assert_eq!(
            exit_text(&status),
            uu_i18n::text("连接前桌面未锁定，退出后保持当前桌面")
        );
        status.originally_locked = None;
        assert_eq!(
            exit_text(&status),
            uu_i18n::text("初始锁屏状态未知，本次不会自动锁屏")
        );
    }
}
