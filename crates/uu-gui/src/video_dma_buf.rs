//! EGL imports of leased, already-synchronized NV12 decoder surfaces.
//! No VA handles cross threads; no CPU map or texture upload on this path.
use eframe::glow::{self, HasContext};
use std::{
    ffi::{c_char, c_void, CStr},
    num::NonZeroUsize,
    os::fd::AsRawFd,
    sync::Arc,
};
use uu_core::display_frames::{dma_buf, RawFrame};
type Display = usize;
type Create =
    unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut c_void, *const i32) -> *mut c_void;
type Destroy = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32;
type Target = unsafe extern "C" fn(u32, *mut c_void);
struct Api {
    _library: libloading::Library,
    display: Display,
    create: Create,
    destroy: Destroy,
    target: Target,
}
impl Api {
    unsafe fn load(gl: &glow::Context) -> Result<Self, String> {
        if !gl.supported_extensions().contains("GL_OES_EGL_image") {
            return Err("GL_OES_EGL_image unavailable".into());
        }
        let version = gl.version();
        let core_sync =
            (version.major, version.minor) >= if version.is_embedded { (3, 0) } else { (3, 2) };
        if !core_sync && !gl.supported_extensions().contains("GL_ARB_sync") {
            return Err("GL sync objects unavailable".into());
        }
        let library = libloading::Library::new("libEGL.so.1").map_err(|e| e.to_string())?;
        let current: unsafe extern "C" fn() -> *mut c_void = *library
            .get(b"eglGetCurrentDisplay\0")
            .map_err(|e| e.to_string())?;
        let query: unsafe extern "C" fn(*mut c_void, i32) -> *const c_char = *library
            .get(b"eglQueryString\0")
            .map_err(|e| e.to_string())?;
        let proc: unsafe extern "C" fn(*const c_char) -> *const c_void = *library
            .get(b"eglGetProcAddress\0")
            .map_err(|e| e.to_string())?;
        let display = current();
        if display.is_null() {
            return Err("no current EGL display (GLX retains CPU upload)".into());
        }
        let extensions = query(display, 0x3055);
        if extensions.is_null() {
            return Err("no EGL extensions".into());
        }
        let extensions = CStr::from_ptr(extensions).to_string_lossy();
        for required in [
            "EGL_EXT_image_dma_buf_import",
            "EGL_EXT_image_dma_buf_import_modifiers",
            "EGL_KHR_image_base",
        ] {
            if !extensions.split_ascii_whitespace().any(|s| s == required) {
                return Err(format!("{required} unavailable"));
            }
        }
        let create = proc(c"eglCreateImageKHR".as_ptr());
        let destroy = proc(c"eglDestroyImageKHR".as_ptr());
        let target = proc(c"glEGLImageTargetTexture2DOES".as_ptr());
        if create.is_null() || destroy.is_null() || target.is_null() {
            return Err("EGL image functions unavailable".into());
        }
        Ok(Self {
            _library: library,
            display: display as usize,
            create: std::mem::transmute::<*const c_void, Create>(create),
            destroy: std::mem::transmute::<*const c_void, Destroy>(destroy),
            target: std::mem::transmute::<*const c_void, Target>(target),
        })
    }
}
/// Call while the GUI's GL context is current, before any decoder is started.
pub fn enable(gl: &glow::Context) {
    // Keep the verified CPU-planar path as the default until hardware import
    // validation covers all supported resolutions, layouts and resizing.
    let enabled =
        std::env::var("UU_GPU_VIDEO").as_deref() == Ok("1") && unsafe { Api::load(gl) }.is_ok();
    dma_buf::set_rendering(enabled);
}
struct Imported {
    gl: Arc<glow::Context>,
    api: Arc<Api>,
    textures: Vec<glow::Texture>,
    images: Vec<usize>,
    fence: Option<NonZeroUsize>,
    // Must outlive all GL sampling, including after a newer frame arrives.
    _frame: RawFrame,
}
impl Imported {
    unsafe fn release_fence(&mut self) {
        if let Some(fence) = self.fence.take() {
            self.gl
                .delete_sync(glow::NativeFence(fence.get() as *mut _));
        }
    }
    unsafe fn finish_for_fallback(&self, error: &str) {
        eprintln!("DMA-BUF rendering unavailable, retaining CPU upload: {error}");
        dma_buf::set_rendering(false);
        // Sampling was submitted in this context. Complete it before the VA
        // surface can be returned to its pool without a usable fence.
        self.gl.finish();
    }
    unsafe fn ready(&mut self) -> bool {
        let Some(fence) = self.fence else {
            return true;
        };
        let status = self
            .gl
            .client_wait_sync(glow::NativeFence(fence.get() as *mut _), 0, 0);
        match fence_complete(status) {
            Ok(false) => return false,
            Ok(true) => {}
            Err(error) => self.finish_for_fallback(error),
        }
        self.release_fence();
        true
    }
    unsafe fn sampled(&mut self) -> Result<(), &'static str> {
        // The new fence covers all sampling in this context, including any
        // previous draw. On failure the importer must finish before release.
        let replacement = self.gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0);
        self.release_fence();
        match checked_fence(replacement) {
            Ok(fence) => {
                self.fence = Some(fence);
                Ok(())
            }
            Err(error) => {
                self.finish_for_fallback(error);
                Err(error)
            }
        }
    }
}
fn checked_fence(fence: Result<glow::NativeFence, String>) -> Result<NonZeroUsize, &'static str> {
    // Native glow can wrap a null GLsync in Ok after glFenceSync fails.
    fence
        .ok()
        .and_then(|f| NonZeroUsize::new(f.0 as usize))
        .ok_or("GL fence creation failed")
}
fn fence_complete(status: u32) -> Result<bool, &'static str> {
    match status {
        glow::ALREADY_SIGNALED | glow::CONDITION_SATISFIED => Ok(true),
        glow::TIMEOUT_EXPIRED => Ok(false),
        _ => Err("GL fence completion unavailable"),
    }
}
impl Drop for Imported {
    fn drop(&mut self) {
        unsafe {
            // Normally retired only after a nonblocking fence poll. Shutdown/error
            // may still own a displayed surface; finish before returning it to VA.
            if !self.ready() {
                self.gl.finish();
                self.release_fence();
            }
            for texture in self.textures.drain(..) {
                self.gl.delete_texture(texture);
            }
            for image in self.images.drain(..) {
                (self.api.destroy)(self.api.display as *mut _, image as *mut _);
            }
        }
    }
}
pub struct Importer {
    gl: Arc<glow::Context>,
    api: Arc<Api>,
    current: Option<Imported>,
    retired: Vec<Imported>,
}
impl Importer {
    pub unsafe fn new(gl: Arc<glow::Context>) -> Option<Self> {
        if !dma_buf::rendering() {
            return None;
        }
        Some(Self {
            api: Arc::new(Api::load(&gl).ok()?),
            gl,
            current: None,
            retired: Vec::new(),
        })
    }
    fn reap(&mut self) {
        self.retired.retain_mut(|frame| !unsafe { frame.ready() });
    }
    pub fn clear(&mut self) {
        if let Some(frame) = self.current.take() {
            self.retired.push(frame);
        }
        self.reap();
    }
    /// False means the GPU still uses the bounded retirement slots. Keep only
    /// the newest pending frame; never wait on the UI thread or queue old frames.
    pub unsafe fn set(&mut self, frame: RawFrame) -> Result<bool, String> {
        self.reap();
        if !dma_buf::rendering() {
            return Err("DMA-BUF import disabled".into());
        }
        if self.retired.len() >= 3 {
            return Ok(false);
        }
        let dma = frame.dma_buf.as_ref().ok_or("GPU frame has no DMA-BUF")?;
        if !dma.valid([frame.width, frame.height]) {
            return Err("invalid DMA-BUF layout".into());
        }
        let mut image = Imported {
            gl: self.gl.clone(),
            api: self.api.clone(),
            textures: Vec::new(),
            images: Vec::new(),
            fence: None,
            _frame: frame.clone(),
        };
        for p in 0..2 {
            let shift = if p == 0 { 1 } else { 2 };
            let attributes = [
                0x3057,
                (dma.width / shift) as i32,
                0x3056,
                (dma.height / shift) as i32,
                0x3271,
                if p == 0 {
                    u32::from_le_bytes(*b"R8  ") as i32
                } else {
                    u32::from_le_bytes(*b"GR88") as i32
                },
                0x3272,
                dma.file.as_raw_fd(),
                0x3273,
                dma.offset[p] as i32,
                0x3274,
                dma.pitch[p] as i32,
                0x3443,
                dma.modifier as u32 as i32,
                0x3444,
                (dma.modifier >> 32) as u32 as i32,
                0x3038,
            ];
            let egl_image = (self.api.create)(
                self.api.display as *mut _,
                std::ptr::null_mut(),
                0x3270,
                std::ptr::null_mut(),
                attributes.as_ptr(),
            );
            if egl_image.is_null() {
                return Err(format!("EGL plane {p} import failed"));
            }
            image.images.push(egl_image as usize);
            let texture = self.gl.create_texture()?;
            image.textures.push(texture);
            self.gl.active_texture(glow::TEXTURE0 + p as u32);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            (self.api.target)(glow::TEXTURE_2D, egl_image);
            if self.gl.get_error() != glow::NO_ERROR {
                return Err(format!("GL plane {p} import failed"));
            }
        }
        if let Some(old) = self.current.replace(image) {
            self.retired.push(old);
        }
        Ok(true)
    }
    pub unsafe fn bind(&self) -> [f32; 4] {
        let Some(frame) = &self.current else {
            return [1., 1., 1., 1.];
        };
        for (i, t) in frame.textures.iter().enumerate() {
            self.gl.active_texture(glow::TEXTURE0 + i as u32);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(*t));
        }
        let dma = frame._frame.dma_buf.as_ref().unwrap();
        [
            frame._frame.width as f32 / dma.width as f32,
            frame._frame.height as f32 / dma.height as f32,
            1.0 / dma.width as f32,
            1.0 / dma.height as f32,
        ]
    }
    pub unsafe fn sampled(&mut self) -> bool {
        if let Some(frame) = &mut self.current {
            if frame.sampled().is_ok() {
                return true;
            }
            self.clear();
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_real_fence_can_guard_sampling() {
        assert!(checked_fence(Err("failed".into())).is_err());
        assert!(checked_fence(Ok(glow::NativeFence(std::ptr::null_mut()))).is_err());
        let nonnull = glow::NativeFence(std::ptr::dangling_mut());
        assert!(checked_fence(Ok(nonnull)).is_ok());
    }

    #[test]
    fn only_timeout_keeps_an_import_in_flight() {
        assert_eq!(fence_complete(glow::ALREADY_SIGNALED), Ok(true));
        assert_eq!(fence_complete(glow::CONDITION_SATISFIED), Ok(true));
        assert_eq!(fence_complete(glow::TIMEOUT_EXPIRED), Ok(false));
        assert!(fence_complete(glow::WAIT_FAILED).is_err());
        assert!(fence_complete(0).is_err());
    }
}
