//! Commands and asynchronous wakeups must retain the session's current window.
use eframe::egui;

/// Video publication needs one paint, rather than egui's two-pass repaint for
/// settling ordinary widgets. A positive sub-frame delay selects the single-pass
/// API; egui subtracts predicted_dt before waking, making this wake immediate.
pub fn request_video_frame(ctx: &egui::Context, viewport: egui::ViewportId) {
    ctx.request_repaint_after_for(std::time::Duration::from_nanos(1), viewport);
}

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
    pub fn request_video_frame(&self) {
        request_video_frame(&self.inner, self.viewport);
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn video_wakeup_is_immediate_without_widget_settling_repaint() {
        let ctx=egui::Context::default();let id=egui::ViewportId::ROOT;
        for _ in 0..3 {let _=ctx.run(Default::default(), |_|{});}
        let wakes=std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));let output=wakes.clone();
        ctx.set_request_repaint_callback(move |info|output.lock().unwrap().push((info.viewport_id,info.delay)));
        request_video_frame(&ctx,id);
        assert_eq!(*wakes.lock().unwrap(),[(id,std::time::Duration::ZERO)]);
        let painted=ctx.run(Default::default(), |_|{});
        assert!(painted.viewport_output[&id].repaint_delay>std::time::Duration::ZERO);
        assert_eq!(wakes.lock().unwrap().len(),1);
        // UI changes retain egui's full repaint contract.
        ctx.request_repaint();
        let painted=ctx.run(Default::default(), |_|{});
        assert_eq!(painted.viewport_output[&id].repaint_delay,std::time::Duration::ZERO);
    }
}
