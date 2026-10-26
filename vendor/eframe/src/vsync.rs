//! Per-surface swap control for the native Glow integration.
use egui::{Context, Id, ViewportId};

#[derive(Clone, Default)]
struct Request {
    desired: bool,
    pending: bool,
    result: Option<Result<bool, String>>,
}

fn key(viewport: ViewportId) -> Id { Id::new(("eframe-swap-interval", viewport)) }
fn observed_key(viewport: ViewportId) -> Id { Id::new(("eframe-observed-swap-interval", viewport)) }

/// Last interval read from the current Windows drawable, rather than a cached
/// request result. `None` means the backend has no readback API.
pub fn vsync_interval(ctx: &Context) -> Option<i32> {
    ctx.data(|data| data.get_temp(observed_key(ctx.viewport_id())))
}

/// Validate the current drawable before issuing any paint commands. Restore a
/// successful policy if its actual interval drifted after a surface transition.
pub(crate) fn observe(ctx: &Context, viewport: ViewportId, actual: Option<i32>) -> bool {
    ctx.data_mut(|data| {
        match actual {
            Some(actual) => data.insert_temp(observed_key(viewport), actual),
            None => { data.remove::<i32>(observed_key(viewport)); },
        }
        data.get_temp::<Request>(key(viewport)).is_some_and(|request| {
            !request.pending && matches!(request.result, Some(Ok(_)))
                && actual.is_some_and(|value| value != i32::from(request.desired))
        })
    })
}

/// Change vertical synchronization for the current native Glow viewport.
/// Windows applies it before painting; other backends apply it before the next
/// buffer swap. Inspect [`vsync_result`] on a later frame.
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

pub(crate) fn remove(ctx: &Context, viewport: ViewportId) {
    ctx.data_mut(|data| {
        data.remove::<Request>(key(viewport));
        data.remove::<i32>(observed_key(viewport));
    });
}

pub(crate) fn apply(
    ctx: &Context,
    viewport: ViewportId,
    recreated: bool,
    set: impl FnOnce(bool) -> Result<(), String>,
) {
    let Some(mut request) = ctx.data(|data| data.get_temp::<Request>(key(viewport))) else { return };
    if !request.pending && !recreated { return; }
    let notify = request.pending;
    request.result = Some(set(request.desired).map(|()| request.desired));
    request.pending = false;
    ctx.data_mut(|data| data.insert_temp(key(viewport), request));
    // Restoring an existing policy already happens inside a paint. Scheduling
    // another paint here can keep windows with different intervals repainting
    // each other indefinitely on drivers sharing swap state across contexts.
    if notify { ctx.request_repaint_of(viewport); }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restoration_does_not_keep_an_idle_window_repainting() {
        let ctx=Context::default();let id=ViewportId::ROOT;
        request_for(&ctx,id,false);apply(&ctx,id,false,|_|Ok(()));
        for _ in 0..5 {let _=ctx.run(egui::RawInput::default(),|_|{});}
        assert!(!ctx.has_requested_repaint());
        assert!(observe(&ctx,id,Some(1)));
        apply(&ctx,id,true,|_|Ok(()));
        assert!(!ctx.has_requested_repaint(),"restoring a paint must not schedule another paint");
        request_for(&ctx,id,true);
        assert!(ctx.has_requested_repaint());
    }
    #[test]
    fn actual_drawable_interval_is_restored_without_changing_other_viewports() {
        let ctx=Context::default();let a=ViewportId::ROOT;let b=ViewportId::from_hash_of("other");
        request_for(&ctx,a,false);request_for(&ctx,b,true);
        apply(&ctx,a,false,|v|{assert!(!v);Ok(())});
        apply(&ctx,b,false,|v|{assert!(v);Ok(())});
        assert!(!observe(&ctx,a,Some(0)));
        assert!(observe(&ctx,a,Some(1)),"a cached success must not hide drawable drift");
        assert!(!observe(&ctx,b,Some(1)));
        apply(&ctx,a,true,|v|{assert!(!v);Ok(())});
        assert!(!observe(&ctx,a,Some(0)));
        assert_eq!(vsync_interval(&ctx),Some(0));
        assert!(!observe(&ctx,a,None));assert_eq!(vsync_interval(&ctx),None);
        observe(&ctx,a,Some(0));remove(&ctx,a);
        assert_eq!(vsync_interval(&ctx),None);
        assert_eq!(vsync_result(&ctx),None);
        assert!(!observe(&ctx,a,Some(1)));
    }
    #[test]
    fn failed_or_absent_policy_does_not_retry_on_every_paint() {
        let ctx=Context::default();let id=ViewportId::ROOT;
        assert!(!observe(&ctx,id,Some(1)));
        request_for(&ctx,id,false);apply(&ctx,id,false,|_|Err("unsupported".into()));
        assert!(!observe(&ctx,id,Some(1)));
        request_for(&ctx,id,false);apply(&ctx,id,false,|_|Ok(()));
        assert!(observe(&ctx,id,Some(1)));
    }
}
