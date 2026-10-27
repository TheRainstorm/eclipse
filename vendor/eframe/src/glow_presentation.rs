//! Optional per-viewport presentation targets for native Glow applications.
//! The owner renders egui and callbacks into one target, then presents it.
//! Installing a target never changes other windows or the default GL backend.
use egui::{Context, Id, ViewportId};
use std::sync::{Arc, Mutex};
#[derive(Clone, Copy, Debug)]
pub struct ColorTarget {
    /// Whether this window's current output is actually displaying HDR.
    pub hdr: bool,
    /// Absolute luminance used for SDR egui white in this target.
    pub sdr_white_nits: f32,
}
pub trait Presenter: Send {
    /// Bind the render target before clear/egui painting. Called on the owning
    /// native GL thread with its context current. No UI lock is held.
    fn begin(&mut self, size: [u32; 2]) -> Result<ColorTarget, String>;
    /// Present the complete UI and restore the default framebuffer.
    fn present(&mut self, vsync: bool) -> Result<(), String>;
    /// Unbind the target, including hidden/minimized frames that do not present.
    fn finish(&mut self);
    /// Detach custom window content when returning to normal presentation.
    fn deactivate(&mut self) -> Result<(), String>;
}
struct State {
    presenter: Box<dyn Presenter>,
    vsync: bool,
    color: ColorTarget,
    active: bool,
}
#[derive(Clone)]
pub(crate) struct Surface(Arc<Mutex<State>>);
fn key(viewport: ViewportId) -> Id {
    Id::new(("eframe-glow-presentation", viewport))
}
/// Install only after the target and its sharing API have been verified.
/// Remove it on the GL thread while the native context is still current.
pub fn install(ctx: &Context, presenter: Box<dyn Presenter>, vsync: bool, color: ColorTarget) {
    let id = key(ctx.viewport_id());
    ctx.data_mut(|data| {
        data.insert_temp(
            id,
            Surface(Arc::new(Mutex::new(State {
                presenter,
                vsync,
                color,
                active: false,
            }))),
        )
    });
    ctx.request_repaint();
}
pub(crate) fn remove_closed(ctx: &Context, viewport: ViewportId) {
    ctx.data_mut(|data| data.remove::<Surface>(key(viewport)));
}
pub fn remove(ctx: &Context, viewport: ViewportId) {
    let old = ctx.data(|data| data.get_temp::<Surface>(key(viewport)));
    remove_closed(ctx, viewport);
    if let Some(old) = old {
        crate::vsync::request_for(ctx, viewport, old.0.lock().unwrap().vsync);
    }
}
pub fn color_target_for(ctx: &Context, viewport: ViewportId) -> Option<ColorTarget> {
    let surface = ctx.data(|data| data.get_temp::<Surface>(key(viewport)))?;
    let state = surface.0.lock().unwrap();
    state.active.then_some(state.color)
}
/// Color capability of a verified, possibly inactive surface.
pub fn available_color_for(ctx: &Context, viewport: ViewportId) -> Option<ColorTarget> {
    let surface = ctx.data(|data| data.get_temp::<Surface>(key(viewport)))?;
    let color = surface.0.lock().unwrap().color;
    Some(color)
}
/// The active stream owns whether this viewport needs the custom surface.
pub fn set_active(ctx: &Context, viewport: ViewportId, active: bool, vsync: bool) {
    let Some(surface) = ctx.data(|data| data.get_temp::<Surface>(key(viewport))) else {
        return;
    };
    let result = {
        let mut state = surface.0.lock().unwrap();
        state.vsync = vsync;
        if state.active == active {
            return;
        }
        state.active = active;
        if active {
            Ok(())
        } else {
            state.presenter.finish();
            state.presenter.deactivate()
        }
    };
    if let Err(error) = result {
        log::error!("custom Glow deactivation failed: {error}");
        remove(ctx, viewport);
    }
    crate::vsync::request_for(ctx, viewport, vsync);
}
pub(crate) fn begin(ctx: &Context, viewport: ViewportId, size: [u32; 2]) -> Option<Surface> {
    let surface = ctx.data(|data| data.get_temp::<Surface>(key(viewport)))?;
    let result = {
        let mut state = surface.0.lock().unwrap();
        if !state.active {
            return None;
        }
        state.presenter.begin(size).map(|color| state.color = color)
    };
    match result {
        Ok(()) => Some(surface),
        Err(error) => {
            log::error!("custom Glow target failed: {error}");
            remove(ctx, viewport);
            None
        }
    }
}
impl Surface {
    pub fn finish(&self) {
        self.0.lock().unwrap().presenter.finish();
    }
    pub fn set_vsync(&self, enabled: bool) {
        self.0.lock().unwrap().vsync = enabled;
    }
    pub fn present(&self) -> Result<(), String> {
        let mut state = self.0.lock().unwrap();
        let vsync = state.vsync;
        state.presenter.present(vsync)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Mock(Arc<Mutex<Vec<&'static str>>>);
    impl Presenter for Mock {
        fn begin(&mut self, _: [u32; 2]) -> Result<ColorTarget, String> {
            self.0.lock().unwrap().push("begin");
            Ok(ColorTarget {
                hdr: true,
                sdr_white_nits: 240.,
            })
        }
        fn present(&mut self, vsync: bool) -> Result<(), String> {
            self.0
                .lock()
                .unwrap()
                .push(if vsync { "vsync" } else { "immediate" });
            Ok(())
        }
        fn finish(&mut self) {
            self.0.lock().unwrap().push("finish");
        }
        fn deactivate(&mut self) -> Result<(), String> {
            self.0.lock().unwrap().push("detach");
            Ok(())
        }
    }
    #[test]
    fn inactive_surface_never_presents_and_owner_changes_restore_swap_control() {
        let ctx = Context::default();
        let viewport = ViewportId::ROOT;
        let calls = Arc::new(Mutex::new(vec![]));
        install(
            &ctx,
            Box::new(Mock(calls.clone())),
            false,
            ColorTarget {
                hdr: true,
                sdr_white_nits: 80.,
            },
        );
        assert!(available_color_for(&ctx, viewport).is_some());
        assert!(color_target_for(&ctx, viewport).is_none());
        assert!(begin(&ctx, viewport, [640, 480]).is_none());
        assert!(calls.lock().unwrap().is_empty());
        set_active(&ctx, viewport, true, true);
        let surface = begin(&ctx, viewport, [640, 480]).unwrap();
        surface.present().unwrap();
        surface.finish();
        assert_eq!(
            color_target_for(&ctx, viewport).unwrap().sdr_white_nits,
            240.
        );
        set_active(&ctx, viewport, false, false);
        set_active(&ctx, viewport, false, false);
        assert!(begin(&ctx, viewport, [640, 480]).is_none());
        assert_eq!(
            *calls.lock().unwrap(),
            ["begin", "vsync", "finish", "finish", "detach"]
        );
        let mut applied = None;
        crate::vsync::apply(&ctx, viewport, false, |v| {
            applied = Some(v);
            Ok(())
        });
        assert_eq!(applied, Some(false));
        remove_closed(&ctx, viewport);
        assert!(available_color_for(&ctx, viewport).is_none());
    }
}
