//! Commands and asynchronous wakeups must retain the session's current window.
use eframe::egui;

#[derive(Clone)]
pub struct Context {
    inner: egui::Context,
    viewport: egui::ViewportId,
}
impl Context {
    pub fn new(ctx: &egui::Context) -> Self {
        Self {
            inner: ctx.clone(),
            viewport: ctx.viewport_id(),
        }
    }
    pub fn viewport(&self) -> egui::ViewportId {
        self.viewport
    }
    pub fn request_repaint(&self) {
        self.inner.request_repaint_of(self.viewport);
    }
    pub fn request_repaint_after(&self, duration: std::time::Duration) {
        self.inner
            .request_repaint_after_for(duration, self.viewport);
    }
    pub fn send_viewport_cmd(&self, command: egui::ViewportCommand) {
        self.inner.send_viewport_cmd_to(self.viewport, command);
    }
}
impl std::ops::Deref for Context {
    type Target = egui::Context;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
