//! Same-adapter D3D video plus a three-slot GL UI overlay. No video WGL lock.
use crate::video_texture::NativeImage;
use crate::video_texture::present_trace;
use crate::video_texture::dxgi_receipt;
use crate::wgl_interop::{Api, Handle};
use eframe::{
    egui,
    glow::{self, HasContext},
    glow_presentation::{ColorTarget, Presenter},
};
use std::{
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};
use uu_core::display_frames::hdr::NativePresentation;
#[derive(Clone)]
pub struct Inspection {
    pub tag: u64,
    pub color: Option<(bool, f32)>,
    pub result: Arc<std::sync::Mutex<Option<InspectedFrame>>>,
}
pub struct InspectedFrame {
    pub tag: u64,
    pub size: [u32; 2],
    pub serial: u64,
    pub pixels: Vec<u16>,
    pub actual_hdr: bool,
    pub color: Option<(bool, f32)>,
    pub ui_reused: bool,
    pub native_image_submitted: bool,
}
struct Overlay {
    texture: glow::Texture,
    fbo: glow::Framebuffer,
    object: Handle,
}
/// The image identity is the export owner AND its producer fence value. UI
/// equality is established separately by the renderer's complete overlay key.
#[derive(Clone, Copy, PartialEq)]
struct Scene {
    image: (u64, u64),
    rect: [f32; 4],
    source_hdr: bool,
    source_white: f32,
    // A shape can finish arriving without a new position revision.
    cursor_revision: Option<(u64, Option<usize>)>,
    output_hdr: bool,
    output_white: f32,
    vsync: bool,
}
struct NativePresenter {
    gl: Arc<glow::Context>,
    native: NativePresentation,
    api: Api,
    device: Handle,
    ui_key: Option<u64>,
    next_ui_key: Option<u64>,
    reuse_ui: bool,
    last_native: bool,
    last_scene: Option<Scene>,
    finish_error: Option<String>,
    overlays: Vec<Overlay>,
    size: [u32; 2],
    index: usize,
    locked: bool,
    color: ColorTarget,
    inspection: Option<Inspection>,
    image: Option<NativeImage>,
    shape: Option<Arc<uu_core::native_cursor::Shape>>,
    trace:Option<present_trace::Producer>,
    observing:bool,
    receipt:dxgi_receipt::Bridge,
}
unsafe impl Send for NativePresenter {}
pub fn enable(ctx: &egui::Context, gl: Arc<glow::Context>, vsync: bool) -> Result<bool, String> {
    if eframe::glow_presentation::native_video_available(ctx, ctx.viewport_id()) {
        return Ok(
            eframe::glow_presentation::available_color_for(ctx, ctx.viewport_id())
                .is_some_and(|v| v.hdr),
        );
    }
    let key = egui::Id::new(("native-video-attempt", ctx.viewport_id()));
    let now = std::time::Instant::now();
    if ctx
        .data(|d| d.get_temp::<std::time::Instant>(key))
        .is_some_and(|t| now.duration_since(t) < std::time::Duration::from_secs(1))
    {
        return Ok(false);
    }
    ctx.data_mut(|d| d.insert_temp(key, now));
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let window = eframe::viewport_window_for(ctx, ctx.viewport_id())
        .ok_or("native video window unavailable")?;
    let RawWindowHandle::Win32(handle) =
        window.window_handle().map_err(|e| e.to_string())?.as_raw()
    else {
        return Ok(false);
    };
    // A window has a single composition target. Release a legacy target
    // before creating its replacement; a failed upgrade can recreate fallback.
    if eframe::glow_presentation::available_color_for(ctx, ctx.viewport_id()).is_some() {
        eframe::glow_presentation::remove(ctx, ctx.viewport_id());
    }
    let mut presenter = unsafe { NativePresenter::new(gl, handle.hwnd.get() as Handle)? };
    let color = presenter.begin([2, 2])?;
    presenter.finish();
    eframe::glow_presentation::remove(ctx, ctx.viewport_id());
    eframe::glow_presentation::install(ctx, Box::new(presenter), vsync, color);
    Ok(color.hdr)
}
impl NativePresenter {
    unsafe fn new(gl: Arc<glow::Context>, hwnd: Handle) -> Result<Self, String> {
        let api = Api::load().ok_or("native UI requires WGL interop")?;
        let mut native = NativePresentation::new(hwnd)?;
        let trace=present_trace::from_env();
        let observing=trace.as_ref().is_some_and(|trace|trace.active());
        native.observe_presents(observing);
        let device = (api.open)(native.device_ptr());
        if device.is_null() {
            return Err("native UI device sharing failed".into());
        }
        Ok(Self {
            gl,
            native,
            api,
            device,
            ui_key: None,
            next_ui_key: None,
            reuse_ui: false,
            last_native: false,
            last_scene: None,
            finish_error: None,
            overlays: Vec::new(),
            size: [0; 2],
            index: 0,
            locked: false,
            color: ColorTarget {
                hdr: false,
                sdr_white_nits: 80.,
            },
            image: None,
            trace,observing,
            receipt:dxgi_receipt::Bridge::default(),
            inspection: None,
            shape: None,
        })
    }
    unsafe fn clear(&mut self) {
        self.receipt.reset();
        self.finish();
        self.gl.finish();
        self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        for mut overlay in self.overlays.drain(..) {
            if !overlay.object.is_null() {
                (self.api.unregister)(self.device, overlay.object);
                overlay.object = ptr::null_mut();
            }
            self.gl.delete_framebuffer(overlay.fbo);
            self.gl.delete_texture(overlay.texture);
        }
        self.size = [0; 2];
        self.image = None;
        self.ui_key = None;
        self.reuse_ui = false;
        self.last_native = false;
        self.last_scene = None;
    }
    unsafe fn resize(&mut self, size: [u32; 2]) -> Result<(), String> {
        if self.size == size {
            return Ok(());
        }
        self.clear();
        self.native.resize(size)?;
        for index in 0..3 {
            let texture = self.gl.create_texture()?;
            let fbo = match self.gl.create_framebuffer() {
                Ok(v) => v,
                Err(e) => {
                    self.gl.delete_texture(texture);
                    return Err(e);
                }
            };
            let object = (self.api.register)(
                self.device,
                self.native.texture_ptr(index),
                texture.0.get(),
                glow::TEXTURE_2D,
                2,
            );
            self.overlays.push(Overlay {
                texture,
                fbo,
                object,
            });
            if object.is_null() {
                return Err("native UI ring registration failed".into());
            }
            if (self.api.lock)(
                self.device,
                1,
                &mut self.overlays.last_mut().unwrap().object,
            ) == 0
            {
                return Err("native UI ring lock failed".into());
            }
            self.index = self.overlays.len() - 1;
            self.locked = true;
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            self.gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture),
                0,
            );
            let complete =
                self.gl.check_framebuffer_status(glow::FRAMEBUFFER) == glow::FRAMEBUFFER_COMPLETE;
            if (self.api.unlock)(
                self.device,
                1,
                &mut self.overlays.last_mut().unwrap().object,
            ) == 0
            {
                return Err("native UI ring unlock failed".into());
            }
            self.locked = false;
            if !complete {
                return Err("native UI ring framebuffer incomplete".into());
            }
        }
        self.size = size;
        self.index = 0;
        Ok(())
    }
}
impl Presenter for NativePresenter {
    fn supports_native_video(&self) -> bool {
        true
    }
    fn cache_ui(&self) -> bool {
        std::env::var("UU_NATIVE_UI_CACHE").as_deref() != Ok("0")
    }
    fn prepare_ui(&mut self, key: Option<u64>) {
        self.reuse_ui = self.last_native && key.is_some() && key == self.ui_key;
        self.next_ui_key = key;
    }
    fn ui_reused(&self) -> bool {
        self.reuse_ui
    }
    fn invalidate_ui(&mut self) {
        self.ui_key = None;
        self.last_native = false;
        self.last_scene = None;
    }

    fn begin(&mut self, size: [u32; 2]) -> Result<ColorTarget, String> {
        unsafe {
            self.resize(size.map(|v| v.max(1)))?;
            self.native.begin();
            self.image = None;
            if !self.reuse_ui {
                // This slot may be painted without a Present (hidden/minimized).
                // Never associate its new contents with the previous frame's key.
                self.ui_key = None;
                self.index = (self.index + 1) % self.overlays.len();
                let overlay = &mut self.overlays[self.index];
                if (self.api.lock)(self.device, 1, &mut overlay.object) == 0 {
                    return Err("native UI ring acquire failed".into());
                }
                self.locked = true;
                self.gl
                    .bind_framebuffer(glow::FRAMEBUFFER, Some(overlay.fbo));
            } else {
                self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            }
            let (hdr, white) = self.native.info();
            let (hdr, white) = self
                .inspection
                .as_ref()
                .and_then(|i| i.color)
                .unwrap_or((hdr, white));
            self.color = ColorTarget {
                hdr,
                sdr_white_nits: white,
            };
            Ok(self.color)
        }
    }
    fn native_video(
        &mut self,
        image: Arc<dyn std::any::Any + Send + Sync>,
    ) -> Result<bool, String> {
        if let Some(inspection) = image.downcast_ref::<Inspection>() {
            self.native.inspect_pixels(true, inspection.color);
            self.inspection = Some(inspection.clone());
            return Ok(true);
        }
        let Some(image) = image.downcast_ref::<NativeImage>() else {
            return Ok(false);
        };
        if !image.valid.load(Ordering::Acquire) {
            return Ok(false);
        }
        let next = image.consumed.load(Ordering::Acquire).saturating_add(1);
        self.native.image(
            image.id,
            image.handles,
            image.ready,
            next,
            image.rect,
            image.hdr,
            image.source_white,
        )?;
        let mut bounds = None;
        if let Some(s) = &image.cursor {
            if let (Some(position), Some(shape)) = (s.position, s.shape.as_ref()) {
                if !s.desktop.contains(&0) {
                    if let Some(size) = shape.description.size() {
                        if self
                            .shape
                            .as_ref()
                            .is_none_or(|old| !Arc::ptr_eq(old, shape))
                        {
                            if let Some((alpha, xor)) = shape.layers() {
                                self.native.cursor(size, &alpha, &xor)?;
                                self.shape = Some(shape.clone());
                            }
                        }
                        if self
                            .shape
                            .as_ref()
                            .is_some_and(|old| Arc::ptr_eq(old, shape))
                        {
                            bounds = Some([
                                position[0] as f32 / s.desktop[0] as f32,
                                position[1] as f32 / s.desktop[1] as f32,
                                size[0] as f32 / s.desktop[0] as f32,
                                size[1] as f32 / s.desktop[1] as f32,
                            ]);
                        }
                    }
                }
            }
        }
        self.native.cursor_position(bounds);
        self.image = Some(image.clone());
        Ok(true)
    }
    fn present(&mut self, vsync: bool) -> Result<(), String> {
        self.finish();
        if let Some(error) = self.finish_error.take() {
            self.receipt.reset();
            self.last_scene = None;
            if let Some(image) = self.image.take() {
                image.valid.store(false, Ordering::Release);
            }
            return Err(error);
        }
        let mut submitted = 0;
        let frame=self.image.as_ref().and_then(|image|image.observation.as_ref());
        let wanted=self.trace.as_ref().is_some_and(|trace|trace.active())
            || dxgi_receipt::Bridge::wanted(frame);
        if wanted!=self.observing {
            self.native.observe_presents(wanted);
            self.observing=wanted;
        }
        if !wanted {self.receipt.reset();}
        let scene = self.image.as_ref().map(|image| Scene {
            image: (image.id, image.ready),
            rect: image.rect,
            source_hdr: image.hdr,
            source_white: image.source_white,
            cursor_revision: image.cursor.as_ref().map(|cursor| (
                cursor.revision,
                cursor.shape.as_ref().map(|shape| Arc::as_ptr(shape) as usize),
            )),
            output_hdr: self.color.hdr,
            output_white: self.color.sdr_white_nits,
            vsync,
        });
        let unchanged = self.reuse_ui && self.inspection.is_none()
            && scene.is_some() && scene == self.last_scene;
        let result = self
            .native
            .present_if_changed(self.index as u32, vsync, &mut submitted, unchanged);
        if matches!(result, Ok(false)) {
            // No GPU read, consumer-fence signal, Present or statistics query
            // happened. Keep the previous receipt ledger; never synthesize one.
            self.image = None;
            return Ok(());
        }
        if self.observing {
            if let Some(raw)=self.native.take_present_observation() {
                let offered=self.image.as_ref().map(|image|(image.id,image.ready));
                let frame=self.image.as_ref().and_then(|image|image.observation.as_ref());
                if let Some(trace)=&self.trace {
                    trace.record(present_trace::Row::new(raw,offered,frame,self.reuse_ui,self.inspection.is_some()));
                }
                self.receipt.record(raw,offered,frame,self.inspection.is_some());
            }else {
                self.receipt.reset();
            }
        }
        if result.is_err() {self.receipt.reset();}
        self.last_native = self.image.is_some() && result.is_ok();
        self.last_scene = if self.last_native { scene } else { None };
        self.ui_key = if self.last_native {
            self.next_ui_key
        } else {
            None
        };
        if let Some(image) = self.image.take() {
            if submitted > 0 {
                image.consumed.store(submitted, Ordering::Release);
            }
            if result.is_err() {
                image.valid.store(false, Ordering::Release);
            }
        }
        if result.is_ok() {
            if let Some(inspection) = &self.inspection {
                if let Some((size, serial, pixels)) = self.native.inspected_pixels() {
                    let (actual_hdr, _) = self.native.info();
                    *inspection.result.lock().unwrap() = Some(InspectedFrame {
                        tag: inspection.tag,
                        size,
                        serial,
                        pixels,
                        actual_hdr,
                        color: inspection.color,
                        ui_reused: self.reuse_ui,
                        native_image_submitted: self.last_native,
                    });
                }
            }
        }
        result.map(|_| ())
    }
    fn finish(&mut self) {
        unsafe {
            if self.locked {
                if (self.api.unlock)(self.device, 1, &mut self.overlays[self.index].object) == 0 {
                    self.finish_error = Some("native UI ring release failed".into());
                } else {
                    self.locked = false;
                }
            }
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
    }
    fn deactivate(&mut self) -> Result<(), String> {
        self.receipt.reset();
        self.finish();
        self.image = None;
        self.shape = None;
        self.ui_key = None;
        self.last_native = false;
        self.last_scene = None;
        self.native.deactivate()
    }
}
impl Drop for NativePresenter {
    fn drop(&mut self) {
        unsafe {
            self.clear();
            (self.api.close)(self.device);
        }
    }
}
