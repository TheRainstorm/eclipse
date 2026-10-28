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
    /// Optional native image metadata, scoped to this viewport and paint frame.
    fn native_video(&mut self,_:Arc<dyn std::any::Any+Send+Sync>)->Result<bool,String>{Ok(false)}
    fn supports_native_video(&self)->bool{false}
    /// Opt-in UI-layer reuse. Video callbacks still run for every frame.
    fn cache_ui(&self)->bool{false}
    fn prepare_ui(&mut self,_signature:Option<u64>){}
    fn ui_reused(&self)->bool{false}
    fn invalidate_ui(&mut self){}

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
pub fn native_video_available(ctx:&Context,viewport:ViewportId)->bool{
    ctx.data(|d|d.get_temp::<Surface>(key(viewport))).is_some_and(|s|s.0.lock().unwrap().presenter.supports_native_video())
}
pub fn native_video(ctx:&Context,viewport:ViewportId,image:Arc<dyn std::any::Any+Send+Sync>)->Result<bool,String>{
    let Some(surface)=ctx.data(|d|d.get_temp::<Surface>(key(viewport))) else{return Ok(false);};
    let mut state=surface.0.lock().unwrap();if !state.active{return Ok(false);}
    state.presenter.native_video(image)
}
/// Hash UI meshes only; callback payloads are live per-frame video metadata.
/// Effective texture changes force repaint, as do dimensions, DPI, clips and
/// clear color. Query the painter lazily so ordinary GL windows pay no atlas
/// comparison cost for a presentation cache they do not use.
pub(crate) fn prepare_ui(ctx:&Context,viewport:ViewportId,size:[u32;2],ppp:f32,primitives:&[egui::ClippedPrimitive],textures_unchanged:impl FnOnce()->bool,clear:[f32;4]) {
    let Some(surface)=ctx.data(|d|d.get_temp::<Surface>(key(viewport))) else{return;};
    let mut state=surface.0.lock().unwrap();if !state.active||!state.presenter.cache_ui(){return;}
    use std::hash::{Hash,Hasher};
    let mut hash=std::collections::hash_map::DefaultHasher::new();
    size.hash(&mut hash);ppp.to_bits().hash(&mut hash);clear.map(f32::to_bits).hash(&mut hash);
    let rect=|r:egui::Rect|[r.min.x.to_bits(),r.min.y.to_bits(),r.max.x.to_bits(),r.max.y.to_bits()];
    for p in primitives {
        rect(p.clip_rect).hash(&mut hash);
        match &p.primitive {
            egui::epaint::Primitive::Mesh(mesh)=>{
                0u8.hash(&mut hash);mesh.texture_id.hash(&mut hash);mesh.indices.hash(&mut hash);
                for v in &mesh.vertices {v.pos.x.to_bits().hash(&mut hash);v.pos.y.to_bits().hash(&mut hash);v.uv.x.to_bits().hash(&mut hash);v.uv.y.to_bits().hash(&mut hash);v.color.to_array().hash(&mut hash);}
            },
            egui::epaint::Primitive::Callback(c)=>{1u8.hash(&mut hash);rect(c.rect).hash(&mut hash);},
        }
    }
    let signature=textures_unchanged().then(||hash.finish());
    state.presenter.prepare_ui(signature);
}
pub fn ui_reused(ctx:&Context,viewport:ViewportId)->bool{
    ctx.data(|d|d.get_temp::<Surface>(key(viewport))).is_some_and(|s|{let state=s.0.lock().unwrap();state.active&&state.presenter.ui_reused()})
}
pub fn invalidate_ui(ctx:&Context,viewport:ViewportId){
    if let Some(surface)=ctx.data(|d|d.get_temp::<Surface>(key(viewport))){surface.0.lock().unwrap().presenter.invalidate_ui();ctx.request_repaint();}
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
    struct CacheProbe {
        enabled: bool,
        keys: Arc<Mutex<Vec<Option<u64>>>>,
    }
    impl Presenter for CacheProbe {
        fn cache_ui(&self) -> bool { self.enabled }
        fn prepare_ui(&mut self, key: Option<u64>) { self.keys.lock().unwrap().push(key); }
        fn begin(&mut self, _: [u32; 2]) -> Result<ColorTarget, String> {
            Ok(ColorTarget { hdr: false, sdr_white_nits: 80. })
        }
        fn present(&mut self, _: bool) -> Result<(), String> { Ok(()) }
        fn finish(&mut self) {}
        fn deactivate(&mut self) -> Result<(), String> { Ok(()) }
    }

    #[test]
    fn texture_content_proof_is_lazy_and_dpi_remains_part_of_ui_identity() {
        let ctx = Context::default();
        let viewport = ViewportId::ROOT;
        let keys = Arc::new(Mutex::new(vec![]));
        let color = ColorTarget { hdr: false, sdr_white_nits: 80. };
        prepare_ui(&ctx, viewport, [640, 480], 1., &[], || panic!("no surface"), [0.; 4]);
        install(&ctx, Box::new(CacheProbe { enabled: false, keys: keys.clone() }), false, color);
        set_active(&ctx, viewport, true, false);
        prepare_ui(&ctx, viewport, [640, 480], 1., &[], || panic!("cache disabled"), [0.; 4]);
        install(&ctx, Box::new(CacheProbe { enabled: true, keys: keys.clone() }), false, color);
        prepare_ui(&ctx, viewport, [640, 480], 1., &[], || panic!("inactive surface"), [0.; 4]);
        set_active(&ctx, viewport, true, false);
        prepare_ui(&ctx, viewport, [640, 480], 1., &[], || true, [0.; 4]);
        prepare_ui(&ctx, viewport, [640, 480], 1., &[], || false, [0.; 4]);
        prepare_ui(&ctx, viewport, [640, 480], 1., &[], || true, [0.; 4]);
        prepare_ui(&ctx, viewport, [640, 480], 1.5, &[], || true, [0.; 4]);
        let keys = keys.lock().unwrap();
        assert!(keys[0].is_some());
        assert!(keys[1].is_none());
        assert_eq!(keys[0], keys[2]);
        assert_ne!(keys[0], keys[3]);
    }

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
