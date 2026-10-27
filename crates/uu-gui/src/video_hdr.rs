//! Render one complete stream viewport through a floating-point scRGB surface.
//! The regular GL backend and other viewports retain their own presentation.
use crate::wgl_interop::{Api, Handle};
use eframe::{
    egui,
    glow::{self, HasContext},
    glow_presentation::{ColorTarget, Presenter},
};
use std::{ptr, sync::Arc};
use uu_core::display_frames::hdr::Presentation;
struct Target {
    texture: glow::Texture,
    framebuffer: glow::Framebuffer,
}
struct HdrPresenter {
    gl: Arc<glow::Context>,
    native: Presentation,
    api: Api,
    device: Handle,
    object: Handle,
    shared: Option<Target>,
    size: [u32; 2],
    color: ColorTarget,
    locked: bool,
}
// eframe's registry is Send; all methods and destruction run on its native GL
// thread with the owning context current, as required by Presenter.
unsafe impl Send for HdrPresenter {}
pub fn enable(ctx: &egui::Context, gl: Arc<glow::Context>, vsync: bool) -> Result<bool, String> {
    if let Some(color) = eframe::glow_presentation::available_color_for(ctx, ctx.viewport_id()) {
        return Ok(color.hdr);
    }
    let key = egui::Id::new(("hdr-target-attempt", ctx.viewport_id()));
    let now = std::time::Instant::now();
    if ctx
        .data(|data| data.get_temp::<std::time::Instant>(key))
        .is_some_and(|last| now.duration_since(last) < std::time::Duration::from_secs(1))
    {
        return Ok(false);
    }
    ctx.data_mut(|data| data.insert_temp(key, now));
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let window = eframe::viewport_window_for(ctx, ctx.viewport_id()).ok_or("串流窗口尚未建立")?;
    let RawWindowHandle::Win32(handle) =
        window.window_handle().map_err(|e| e.to_string())?.as_raw()
    else {
        return Ok(false);
    };
    let mut presenter = unsafe { HdrPresenter::new(gl, handle.hwnd.get() as Handle)? };
    // Verify texture allocation, FP16 framebuffer completeness and actual WGL
    // registration before advertising HDR to the peer.
    let color = presenter.begin([2, 2])?;
    presenter.finish();
    eframe::glow_presentation::install(ctx, Box::new(presenter), vsync, color);
    Ok(color.hdr)
}
impl HdrPresenter {
    unsafe fn new(gl: Arc<glow::Context>, hwnd: Handle) -> Result<Self, String> {
        let api = Api::load().ok_or("HDR 需要 WGL/D3D11 共享纹理支持")?;
        let native = Presentation::new(hwnd)?;
        let device = (api.open)(native.device_ptr());
        if device.is_null() {
            return Err("HDR 呈现设备不能与 OpenGL 共享纹理".into());
        }
        Ok(Self {
            gl,
            native,
            api,
            device,
            object: ptr::null_mut(),
            shared: None,
            size: [0, 0],
            color: ColorTarget {
                hdr: true,
                sdr_white_nits: 80.,
            },
            locked: false,
        })
    }
    unsafe fn clear_targets(&mut self) {
        if self.locked {
            (self.api.unlock)(self.device, 1, &mut self.object);
            self.locked = false;
        }
        self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        if !self.object.is_null() {
            (self.api.unregister)(self.device, self.object);
            self.object = ptr::null_mut();
        }
        if let Some(target) = self.shared.take() {
            self.gl.delete_framebuffer(target.framebuffer);
            self.gl.delete_texture(target.texture);
        }
        self.size = [0, 0];
    }
    unsafe fn resize(&mut self, size: [u32; 2]) -> Result<(), String> {
        if self.size == size {
            return Ok(());
        }
        self.clear_targets();
        self.native.resize(size)?;
        let shared_texture = self.gl.create_texture()?;
        let shared_fbo = match self.gl.create_framebuffer() {
            Ok(v) => v,
            Err(e) => {
                self.gl.delete_texture(shared_texture);
                return Err(e);
            }
        };
        self.shared = Some(Target {
            texture: shared_texture,
            framebuffer: shared_fbo,
        });
        self.object = (self.api.register)(
            self.device,
            self.native.texture_ptr(),
            shared_texture.0.get(),
            glow::TEXTURE_2D,
            2,
        ); // WRITE_DISCARD_NV
        if self.object.is_null() {
            return Err("HDR FP16 纹理注册失败".into());
        }
        if (self.api.lock)(self.device, 1, &mut self.object) == 0 {
            return Err("HDR FP16 纹理锁定失败".into());
        }
        self.locked = true;
        self.gl
            .bind_framebuffer(glow::FRAMEBUFFER, Some(shared_fbo));
        self.gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(shared_texture),
            0,
        );
        if self.gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
            return Err("HDR Windows 浮点缓冲不完整".into());
        }
        if (self.api.unlock)(self.device, 1, &mut self.object) == 0 {
            return Err("HDR FP16 纹理解锁失败".into());
        }
        self.locked = false;
        self.size = size;
        Ok(())
    }
}
impl Presenter for HdrPresenter {
    fn begin(&mut self, size: [u32; 2]) -> Result<ColorTarget, String> {
        let size = size.map(|v| v.max(1));
        unsafe {
            self.resize(size)?;
            if (self.api.lock)(self.device, 1, &mut self.object) == 0 {
                return Err("HDR 呈现纹理锁定失败".into());
            }
            self.locked = true;
            self.gl.bind_framebuffer(
                glow::FRAMEBUFFER,
                Some(self.shared.as_ref().unwrap().framebuffer),
            );
        }
        let (hdr, white) = self.native.info();
        self.color = ColorTarget {
            hdr,
            sdr_white_nits: white,
        };
        Ok(self.color)
    }
    fn present(&mut self, vsync: bool) -> Result<(), String> {
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            if (self.api.unlock)(self.device, 1, &mut self.object) == 0 {
                return Err("HDR 呈现纹理解锁失败".into());
            }
            self.locked = false;
        }
        self.native.present(vsync)
    }
    fn finish(&mut self) {
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            if self.locked {
                (self.api.unlock)(self.device, 1, &mut self.object);
                self.locked = false;
            }
        }
    }
    fn deactivate(&mut self) -> Result<(), String> {
        self.native.deactivate()
    }
}
impl Drop for HdrPresenter {
    fn drop(&mut self) {
        unsafe {
            self.clear_targets();
            (self.api.close)(self.device);
        }
    }
}
