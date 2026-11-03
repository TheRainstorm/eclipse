//! Commands and asynchronous wakeups must retain the session's current window.
use eframe::egui;
use std::sync::{Arc, Mutex};

/// Video publication needs one paint, rather than egui's two-pass repaint for
/// settling ordinary widgets. A positive sub-frame delay selects the single-pass
/// API; egui subtracts predicted_dt before waking, making this wake immediate.
pub fn request_video_frame(ctx: &egui::Context, viewport: egui::ViewportId) {
    ctx.request_repaint_after_for(std::time::Duration::from_nanos(1), viewport);
}

#[derive(Clone)]
pub struct Context {
    inner: egui::Context,
    route: Arc<Mutex<Route>>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Bound,
    Transferring,
    Retired,
}
struct Route {
    viewport: egui::ViewportId,
    state: State,
}
impl Context {
    pub fn new(ctx: &egui::Context) -> Self {
        Self {
            inner: ctx.clone(),
            route: Arc::new(Mutex::new(Route {
                viewport: ctx.viewport_id(),
                state: State::Bound,
            })),
        }
    }
    pub fn viewport(&self) -> egui::ViewportId {
        self.route.lock().unwrap().viewport
    }
    /// Session moves keep this route alive so callbacks created before the move
    /// follow the destination. A session never crosses egui integrations.
    pub fn bind(&self, ctx: &egui::Context) {
        assert!(self.inner == *ctx, "session moved across egui integrations");
        let viewport = ctx.viewport_id();
        let mut route = self.route.lock().unwrap();
        if route.state != State::Retired {
            route.viewport = viewport;
            route.state = State::Bound;
        }
    }
    /// Input has already been released on the old surface. Until the destination
    /// is bound, late media/cursor publications must not wake the old surface.
    pub fn suspend(&self) {
        let mut route = self.route.lock().unwrap();
        if route.state == State::Bound {
            route.state = State::Transferring;
        }
    }
    pub fn retire(&self) {
        self.route.lock().unwrap().state = State::Retired;
    }
    fn bound(&self) -> Option<egui::ViewportId> {
        let route = self.route.lock().unwrap();
        (route.state == State::Bound).then_some(route.viewport)
    }
    pub fn request_repaint(&self) {
        // Never hold the route lock across egui: egui invokes its repaint
        // callback synchronously. The native backend also rejects undeclared
        // viewport placeholders, covering a wake already in flight at bind.
        if let Some(viewport) = self.bound() {
            self.inner.request_repaint_of(viewport);
        }
    }
    pub fn request_video_frame(&self) {
        if let Some(viewport) = self.bound() {
            request_video_frame(&self.inner, viewport);
        }
    }
    pub fn request_repaint_after(&self, duration: std::time::Duration) {
        if let Some(viewport) = self.bound() {
            self.inner.request_repaint_after_for(duration, viewport);
        }
    }
    pub fn send_viewport_cmd(&self, command: egui::ViewportCommand) {
        if let Some(viewport) = self.bound() {
            self.inner.send_viewport_cmd_to(viewport, command);
        }
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
    fn input(id: egui::ViewportId) -> egui::RawInput {
        egui::RawInput {
            viewport_id: id,
            viewports: [
                (egui::ViewportId::ROOT, Default::default()),
                (id, Default::default()),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        }
    }
    fn declare(ctx: &egui::Context, id: egui::ViewportId) {
        ctx.show_viewport_deferred(id, Default::default(), |_, _| {});
    }
    #[test]
    fn callbacks_follow_root_child_and_sibling_moves_without_reviving_old_windows() {
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(false);
        let a = egui::ViewportId::from_hash_of("child-a");
        let b = egui::ViewportId::from_hash_of("child-b");
        let route = Context::new(&ctx);
        let cursor = route.clone();
        let frames = route.clone();
        for destination in [a, egui::ViewportId::ROOT, a, b, egui::ViewportId::ROOT] {
            route.suspend();
            let _ = ctx.run(input(egui::ViewportId::ROOT), |ctx| {
                if destination != egui::ViewportId::ROOT {
                    declare(ctx, destination)
                }
            });
            cursor.request_repaint();
            frames.request_video_frame();
            let _ = ctx.run(input(destination), |ctx| route.bind(ctx));
            cursor.request_repaint();
            frames.request_video_frame();
            cursor.send_viewport_cmd(egui::ViewportCommand::Title("current session".into()));
            let output = ctx.run(input(destination), |_| {});
            assert_eq!(route.viewport(), destination);
            assert!(output.viewport_output[&destination].commands.iter().any(|command|matches!(command,egui::ViewportCommand::Title(title) if title=="current session")));
            for child in [a, b] {
                if child != destination {
                    assert!(
                        !output.viewport_output.contains_key(&child),
                        "a cursor/frame clone revived a retired surface"
                    );
                }
            }
        }
    }
    #[test]
    fn cancelled_transfer_resumes_original_owner_and_retirement_is_final() {
        let ctx = egui::Context::default();
        let owner = Context::new(&ctx);
        let callback = owner.clone();
        owner.suspend();
        assert_eq!(callback.bound(), None);
        owner.bind(&ctx);
        assert_eq!(callback.bound(), Some(egui::ViewportId::ROOT));
        owner.retire();
        owner.bind(&ctx);
        assert_eq!(callback.bound(), None);
        callback.request_repaint();
        callback.request_video_frame();
        callback.request_repaint_after(std::time::Duration::from_millis(10));
        callback.send_viewport_cmd(egui::ViewportCommand::Title("late".into()));
        let output = ctx.run(Default::default(), |_| {});
        assert!(!output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .iter()
            .any(|command| matches!(command, egui::ViewportCommand::Title(_))));
    }
    #[test]
    fn another_session_retiring_does_not_retire_the_moved_owner() {
        let ctx = egui::Context::default();
        let first = Context::new(&ctx);
        let second = Context::new(&ctx);
        let callback = second.clone();
        second.suspend();
        first.retire();
        second.bind(&ctx);
        assert_eq!(callback.bound(), Some(egui::ViewportId::ROOT));
        assert_eq!(first.bound(), None);
    }
    #[test]
    fn repaint_callback_can_retire_route_without_a_cross_callback_lock() {
        let ctx = egui::Context::default();
        for _ in 0..3 {
            let _ = ctx.run(Default::default(), |_| {});
        }
        let owner = Context::new(&ctx);
        let route = owner.clone();
        ctx.set_request_repaint_callback(move |_| route.retire());
        owner.request_repaint();
        assert_eq!(owner.bound(), None);
    }
    #[test]
    fn video_wakeup_is_immediate_without_widget_settling_repaint() {
        let ctx = egui::Context::default();
        let id = egui::ViewportId::ROOT;
        for _ in 0..3 {
            let _ = ctx.run(Default::default(), |_| {});
        }
        let wakes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let output = wakes.clone();
        ctx.set_request_repaint_callback(move |info| {
            output.lock().unwrap().push((info.viewport_id, info.delay))
        });
        request_video_frame(&ctx, id);
        assert_eq!(*wakes.lock().unwrap(), [(id, std::time::Duration::ZERO)]);
        let painted = ctx.run(Default::default(), |_| {});
        assert!(painted.viewport_output[&id].repaint_delay > std::time::Duration::ZERO);
        assert_eq!(wakes.lock().unwrap().len(), 1);
        // UI changes retain egui's full repaint contract.
        ctx.request_repaint();
        let painted = ctx.run(Default::default(), |_| {});
        assert_eq!(
            painted.viewport_output[&id].repaint_delay,
            std::time::Duration::ZERO
        );
    }
}
