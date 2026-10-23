//! Native handles belong to a viewport, rather than to the root Frame.
use egui::{Context, Id, ViewportId};
use std::sync::{Arc, Weak};
use winit::window::Window;

fn key(viewport: ViewportId) -> Id {
    Id::new(("eframe-native-window", viewport))
}

/// Borrow the live Glow viewport's native window. A weak entry ensures that
/// removing a viewport still destroys its window and surface normally.
pub fn viewport_window(ctx: &Context) -> Option<Arc<Window>> {
    viewport_window_for(ctx, ctx.viewport_id())
}
/// Access another live viewport on the UI thread, for ordered input release.
pub fn viewport_window_for(ctx: &Context, viewport: ViewportId) -> Option<Arc<Window>> {
    let id = key(viewport);
    ctx.data(|data| data.get_temp::<Weak<Window>>(id))
        .and_then(|window| window.upgrade())
}

pub(crate) fn register(ctx: &Context, viewport: ViewportId, window: &Arc<Window>) {
    ctx.data_mut(|data| data.insert_temp(key(viewport), Arc::downgrade(window)));
}

/// CPU frame duration of this Glow viewport, excluding buffer-swap waits.
pub fn viewport_cpu_usage(ctx: &Context) -> Option<f32> {
    let id = Id::new(("eframe-viewport-cpu", ctx.viewport_id()));
    ctx.data(|data| data.get_temp::<f32>(id))
}
pub(crate) fn report_cpu_usage(ctx: &Context, viewport: ViewportId, seconds: f32) {
    ctx.data_mut(|data| data.insert_temp(Id::new(("eframe-viewport-cpu", viewport)), seconds));
}
