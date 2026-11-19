//! EGL imports of leased, already-synchronized NV12 and P010 decoder surfaces.
//! No VA handles cross threads; no CPU map or texture upload on this path.
use eframe::glow::{self, HasContext};
use std::os::unix::{
    ffi::OsStrExt,
    fs::{FileTypeExt, MetadataExt},
};
use std::{
    ffi::{c_char, c_void, CStr},
    num::NonZeroUsize,
    os::fd::AsRawFd,
    sync::Arc,
};
use uu_core::display_frames::{dma_buf, PixelFormat, RawFrame};
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
type QueryModifiers =
    unsafe extern "C" fn(*mut c_void, i32, i32, *mut u64, *mut u32, *mut i32) -> u32;

unsafe fn texture_modifiers(
    query: QueryModifiers,
    display: *mut c_void,
    format: [u8; 4],
) -> Option<[bool; 2]> {
    let format = i32::from_le_bytes(format);
    let mut count = 0;
    if query(
        display,
        format,
        0,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut count,
    ) == 0
        || !(1..=256).contains(&count)
    {
        return None;
    }
    let capacity = count;
    let mut modifiers = vec![0; capacity as usize];
    let mut external_only = vec![1; capacity as usize];
    if query(
        display,
        format,
        capacity,
        modifiers.as_mut_ptr(),
        external_only.as_mut_ptr(),
        &mut count,
    ) == 0
        || !(1..=capacity).contains(&count)
    {
        return None;
    }
    let supported = |wanted| {
        modifiers[..count as usize]
            .iter()
            .zip(&external_only)
            .any(|(&modifier, &external)| modifier == wanted && external == 0)
    };
    Some([supported(0), supported(0x0100_0000_0000_0002)])
}

impl Api {
    unsafe fn render_target(&self, gl: &glow::Context) -> Option<dma_buf::RenderTarget> {
        type QueryDisplay = unsafe extern "C" fn(*mut c_void, i32, *mut isize) -> u32;
        type QueryDevice = unsafe extern "C" fn(*mut c_void, i32) -> *const c_char;
        type QueryString = unsafe extern "C" fn(*mut c_void, i32) -> *const c_char;
        type Proc = unsafe extern "C" fn(*const c_char) -> *const c_void;
        let proc: Proc = *self._library.get(b"eglGetProcAddress\0").ok()?;
        let query_string: QueryString = *self._library.get(b"eglQueryString\0").ok()?;
        let ext = |string: *const c_char, name: &str| {
            !string.is_null()
                && CStr::from_ptr(string)
                    .to_bytes()
                    .split(|&c| c == b' ')
                    .any(|s| s == name.as_bytes())
        };
        let client_extensions = query_string(std::ptr::null_mut(), 0x3055); // EGL_EXTENSIONS
        if !ext(client_extensions, "EGL_EXT_device_query")
            && !ext(client_extensions, "EGL_EXT_device_base")
        {
            return None;
        }
        let display_attr = proc(c"eglQueryDisplayAttribEXT".as_ptr());
        let device_string = proc(c"eglQueryDeviceStringEXT".as_ptr());
        let query_modifiers = proc(c"eglQueryDmaBufModifiersEXT".as_ptr());
        if display_attr.is_null() || device_string.is_null() || query_modifiers.is_null() {
            return None;
        }
        let display_attr = std::mem::transmute::<*const c_void, QueryDisplay>(display_attr);
        let device_string = std::mem::transmute::<*const c_void, QueryDevice>(device_string);
        let query_modifiers = std::mem::transmute::<*const c_void, QueryModifiers>(query_modifiers);
        let display = self.display as *mut c_void;
        let mut device: isize = 0;
        if display_attr(display, 0x322c, &mut device) == 0 || device == 0 {
            return None;
        } // EGL_DEVICE_EXT
        let device = device as *mut c_void;
        if !ext(
            device_string(device, 0x3055),
            "EGL_EXT_device_drm_render_node",
        ) {
            return None;
        }
        let path = device_string(device, 0x3377); // EGL_DRM_RENDER_NODE_FILE_EXT
        if path.is_null() {
            return None;
        }
        let stat =
            std::fs::metadata(std::ffi::OsStr::from_bytes(CStr::from_ptr(path).to_bytes())).ok()?;
        if !stat.file_type().is_char_device() {
            return None;
        }
        let planes = |luma, chroma| {
            let y = texture_modifiers(query_modifiers, display, luma).unwrap_or_default();
            let uv = texture_modifiers(query_modifiers, display, chroma).unwrap_or_default();
            dma_buf::Modifiers {
                linear: y[0] && uv[0],
                intel_y: y[1] && uv[1],
            }
        };
        let nv12 = planes(*b"R8  ", *b"GR88");
        let p010 = if !gl.version().is_embedded
            || gl.supported_extensions().contains("GL_EXT_texture_norm16")
        {
            planes(*b"R16 ", *b"GR32")
        } else {
            dma_buf::Modifiers::default()
        };
        (nv12.linear || nv12.intel_y || p010.linear || p010.intel_y).then_some(
            dma_buf::RenderTarget {
                device: stat.rdev(),
                nv12,
                p010,
            },
        )
    }
}

/// Called with the owner's GL context current, before decoder pool allocation.
pub fn enable(gl: &glow::Context) {
    let mode = std::env::var_os("UU_GPU_VIDEO");
    let auto = mode.as_deref().is_none_or(|value| value == "auto");
    let force = mode.as_deref().is_some_and(|value| value == "1");
    let api = (auto || force)
        .then(|| unsafe { Api::load(gl).ok() })
        .flatten();
    let target = api
        .as_ref()
        .and_then(|api| unsafe { api.render_target(gl) });
    dma_buf::configure_target(target, force && api.is_some());
}
/// A failed import still owns the synchronized decoded surface. Preserve its
/// timestamp and crop, even if this is the last picture of a static desktop.
pub fn recover_frame(frame: &mut RawFrame) -> Result<(), String> {
    let dma = frame
        .dma_buf
        .as_ref()
        .ok_or("GPU frame lacks an exported surface")?;
    dma.reject_import();
    if frame.format != dma.format {
        return Err("decoded frame and exported surface formats differ".into());
    }
    #[cfg(feature = "vaapi")]
    {
        let pixels = dma.read_visible([frame.width, frame.height])?;
        frame.data = pixels;
        frame.stride = frame.format.row_bytes(frame.width);
        frame.gpu = false;
        frame.dma_buf = None;
        Ok(())
    }
    #[cfg(not(feature = "vaapi"))]
    Err("DMA-BUF readback was not compiled in".into())
}

struct Imported {
    gl: Arc<glow::Context>,
    api: Arc<Api>,
    textures: Vec<glow::Texture>,
    images: Vec<usize>,
    fence: Option<NonZeroUsize>,
    // Must outlive all GL sampling, including after a newer frame arrives.
    _frame: RawFrame,
    sync_ok: Arc<std::sync::atomic::AtomicBool>,
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
        self.sync_ok
            .store(false, std::sync::atomic::Ordering::Release);
        if let Some(dma) = &self._frame.dma_buf {
            dma.reject_import();
        }
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
    target: Option<dma_buf::RenderTarget>,
    force_nv12: bool,
    sync_ok: Arc<std::sync::atomic::AtomicBool>,
}
impl Importer {
    pub unsafe fn new(gl: Arc<glow::Context>) -> Option<Self> {
        if !dma_buf::rendering() {
            return None;
        }
        let api = Api::load(&gl).ok()?;
        // A detached viewport may be bound to a different current EGL display.
        let force_nv12 = std::env::var_os("UU_GPU_VIDEO").as_deref() == Some("1".as_ref());
        let target = api.render_target(&gl);
        if !force_nv12 && target.is_none() {
            return None;
        }
        Some(Self {
            api: Arc::new(api),
            target,
            force_nv12,
            sync_ok: Arc::new(std::sync::atomic::AtomicBool::new(true)),
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
        let dma = frame.dma_buf.as_ref().ok_or("GPU frame has no DMA-BUF")?;
        if !self.sync_ok.load(std::sync::atomic::Ordering::Acquire)
            || !dma.import_enabled()
            || frame.format != dma.format
            || !dma_buf::accepts_import(
                self.target,
                self.force_nv12,
                frame.format,
                dma.device,
                dma.modifier,
            )
        {
            return Err("EGL context cannot import this VA surface pool".into());
        }
        if self.retired.len() >= 3 {
            return Ok(false);
        }
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
            sync_ok: self.sync_ok.clone(),
        };
        for p in 0..2 {
            let shift = if p == 0 { 1 } else { 2 };
            let attributes = [
                0x3057,
                (dma.width / shift) as i32,
                0x3056,
                (dma.height / shift) as i32,
                0x3271,
                i32::from_le_bytes(match (frame.format, p) {
                    (PixelFormat::P010, 0) => *b"R16 ",
                    (PixelFormat::P010, _) => *b"GR32",
                    (_, 0) => *b"R8  ",
                    (_, _) => *b"GR88",
                }),
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
    pub fn origin(&self) -> [f32; 2] {
        self.current
            .as_ref()
            .and_then(|f| f._frame.dma_buf.as_ref())
            .map_or([0., 0.], |d| {
                [
                    d.origin[0] as f32 / d.width as f32,
                    d.origin[1] as f32 / d.height as f32,
                ]
            })
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
    pub unsafe fn sampled(&mut self) -> Result<(), RawFrame> {
        if let Some(frame) = &mut self.current {
            if frame.sampled().is_err() {
                let retained = frame._frame.clone();
                self.clear();
                return Err(retained);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_modifiers_reject_external_only_and_incomplete_lists() {
        unsafe extern "C" fn modifiers(
            _: *mut c_void,
            format: i32,
            capacity: i32,
            modifiers: *mut u64,
            external: *mut u32,
            count: *mut i32,
        ) -> u32 {
            *count = 2;
            if capacity > 0 {
                if format == i32::from_le_bytes(*b"FAIL") {
                    *count = capacity + 1;
                    return 1;
                }
                modifiers.write(0);
                modifiers.add(1).write(0x0100_0000_0000_0002);
                external.write(u32::from(format == i32::from_le_bytes(*b"GR88")));
                external.add(1).write(0);
            }
            1
        }
        unsafe {
            assert_eq!(
                texture_modifiers(modifiers, std::ptr::null_mut(), *b"R8  "),
                Some([true, true])
            );
            assert_eq!(
                texture_modifiers(modifiers, std::ptr::null_mut(), *b"GR88"),
                Some([false, true])
            );
            assert_eq!(
                texture_modifiers(modifiers, std::ptr::null_mut(), *b"FAIL"),
                None
            );
        }
    }

    #[test]
    fn mismatched_surface_is_rejected_before_same_frame_readback() {
        let dma = Arc::new(dma_buf::Frame {
            file: std::fs::File::open(std::env::current_exe().unwrap()).unwrap(),
            format: PixelFormat::Nv12,
            origin: [0, 0],
            device: None,
            allocation_size: 6,
            width: 2,
            height: 2,
            pitch: [2, 2],
            offset: [0, 4],
            modifier: 0,
            keep_alive: Arc::new(()),
            importable: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        });
        let mut frame = RawFrame {
            width: 2,
            height: 2,
            format: PixelFormat::P010,
            stride: 4,
            data: Default::default(),
            pts: uu_media::Timestamp(19),
            gpu: true,
            dma_buf: Some(dma.clone()),
            d3d11: None,
        };
        assert!(!frame.is_consistent());
        assert_eq!(
            recover_frame(&mut frame).unwrap_err(),
            "decoded frame and exported surface formats differ"
        );
        assert!(!dma.import_enabled());
        assert!(frame.gpu && frame.data.is_empty());
        assert!(Arc::ptr_eq(frame.dma_buf.as_ref().unwrap(), &dma));
        assert_eq!(frame.pts.0, 19);
    }

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
