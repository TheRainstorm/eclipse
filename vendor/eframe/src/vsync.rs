//! Per-surface swap control for the native Glow integration.
use egui::{Context, Id, ViewportId};

#[derive(Clone, Default)]
struct Request {
    desired: bool,
    pending: bool,
    result: Option<Result<bool, String>>,
}

fn key(viewport: ViewportId) -> Id { Id::new(("eframe-swap-interval", viewport)) }

/// Change vertical synchronization for the current native Glow viewport.
/// Applied before its next buffer swap. Inspect [`vsync_result`] on a later frame.
pub fn request_vsync(ctx: &Context, enabled: bool) {request_for(ctx,ctx.viewport_id(),enabled);}
pub(crate) fn request_for(ctx:&Context,viewport:ViewportId,enabled:bool) {
    let id=key(viewport);
    ctx.data_mut(|data| data.insert_temp(id, Request {
        desired: enabled, pending: true, result: None,
    }));
    ctx.request_repaint_of(viewport);
}

/// Result of the most recent request for this viewport, or `None` while pending.
pub fn vsync_result(ctx: &Context) -> Option<Result<bool, String>> {
    let id=key(ctx.viewport_id());
    ctx.data(|data| data.get_temp::<Request>(id).and_then(|r| r.result))
}

pub(crate) fn apply(
    ctx: &Context,
    viewport: ViewportId,
    recreated: bool,
    set: impl FnOnce(bool) -> Result<(), String>,
) {
    let Some(mut request) = ctx.data(|data| data.get_temp::<Request>(key(viewport))) else { return };
    if !request.pending && !recreated { return; }
    request.result = Some(set(request.desired).map(|()| request.desired));
    request.pending = false;
    ctx.data_mut(|data| data.insert_temp(key(viewport), request));
    ctx.request_repaint_of(viewport);
}
