//! Scoped, short-lived UI feedback.
use eframe::egui;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Scope {
    Devices,
    DeviceDetails(String),
    Services(u8),
    Compatibility,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Info,
    Success,
    Error,
}

#[derive(Clone, Debug)]
pub(super) struct Message {
    pub scope: Scope,
    pub kind: Kind,
    pub text: String,
    shown_at: Instant,
}

impl Message {
    fn visible(&self, now: Instant) -> bool {
        match self.kind {
            Kind::Error => true,
            Kind::Success => now.duration_since(self.shown_at) < Duration::from_secs(2),
            Kind::Info => now.duration_since(self.shown_at) < Duration::from_secs(4),
        }
    }
}

#[derive(Default)]
pub(super) struct Feedback {
    message: Option<Message>,
}

impl Feedback {
    pub fn report(&mut self, scope: Scope, kind: Kind, text: impl Into<String>) {
        let text = text.into();
        if text.is_empty() {
            self.message = None;
        } else {
            self.message = Some(Message {
                scope,
                kind,
                text,
                shown_at: Instant::now(),
            });
        }
    }

    pub fn success(&mut self, scope: Scope, text: impl Into<String>) {
        self.report(scope, Kind::Success, text);
    }

    pub fn error(&mut self, scope: Scope, text: impl Into<String>) {
        self.report(scope, Kind::Error, text);
    }

    pub fn clear_scope(&mut self, scope: &Scope) {
        if self.message.as_ref().is_some_and(|message| &message.scope == scope) {
            self.message = None;
        }
    }

    pub fn current(&self, scope: &Scope) -> Option<&Message> {
        self.message
            .as_ref()
            .filter(|message| &message.scope == scope && message.visible(Instant::now()))
    }

    #[cfg(test)]
    pub fn text(&self) -> &str {
        self.message
            .as_ref()
            .map(|message| message.text.as_str())
            .unwrap_or("")
    }

    pub fn show(&mut self, ui: &mut egui::Ui, scope: Scope) {
        let Some(message) = self.current(&scope).cloned() else {
            return;
        };
        let mut dismiss = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(&message.text).color(match message.kind {
                    Kind::Info => super::theme::MUTED,
                    Kind::Success => super::theme::SUCCESS,
                    Kind::Error => super::theme::DANGER,
                }),
            );
            if message.kind == Kind::Error && ui.small_button(uu_i18n::text("关闭")).clicked() {
                dismiss = true;
            }
        });
        if dismiss {
            self.clear_scope(&scope);
        }
        ui.add_space(8.0);
    }

    pub fn tick(&self, ctx: &egui::Context) {
        if self.message.as_ref().is_some_and(|message| {
            message.kind == Kind::Info
                || message.kind == Kind::Success
        }) {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

pub(super) fn looks_like_error(text: &str) -> bool {
    [
        "失败", "无法", "错误", "异常", "占用", "未完成", "意外", "拒绝", "超时",
    ]
    .iter()
    .any(|needle| text.contains(needle)) || ["failed","cannot","error","invalid","unavailable","timed out","denied","unexpected","incomplete"]
        .iter().any(|needle|text.to_ascii_lowercase().contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_is_scoped_and_success_expires() {
        let mut feedback = Feedback::default();
        feedback.success(Scope::Devices, "saved");
        assert!(feedback.current(&Scope::Devices).is_some());
        assert!(feedback.current(&Scope::Services(0)).is_none());
        feedback.report(Scope::Services(0), Kind::Error, "failed");
        assert!(feedback.current(&Scope::Devices).is_none());
        assert!(feedback.current(&Scope::Services(0)).is_some());
    }
}
