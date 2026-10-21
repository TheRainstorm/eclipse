//! D3D11 decoder surfaces -> GPU video conversion -> WGL shared RGB texture.
//! WGL lock/unlock provides GPU ordering between D3D11 and GL. All methods and
//! destruction run with the owning GL context current on the renderer thread.
use eframe::glow::{self, HasContext};
use std::{
    ffi::{c_char, c_void, CStr},
    sync::Arc,
};
use uu_core::display_frames::{
    d3d11_frame::{self, Converter, Frame},
    YuvColor,
};
type Handle = *mut c_void;
#[link(name = "opengl32")]
extern "system" {
    fn wglGetProcAddress(name: *const c_char) -> *const c_void;
    fn wglGetCurrentDC() -> Handle;
}
#[derive(Clone, Copy)]
struct Api {
    open: unsafe extern "system" fn(Handle) -> Handle,
    close: unsafe extern "system" fn(Handle) -> i32,
    register: unsafe extern "system" fn(Handle, Handle, u32, u32, u32) -> Handle,
    unregister: unsafe extern "system" fn(Handle, Handle) -> i32,
    lock: unsafe extern "system" fn(Handle, i32, *mut Handle) -> i32,
    unlock: unsafe extern "system" fn(Handle, i32, *mut Handle) -> i32,
}
impl Api {
    unsafe fn load() -> Option<Self> {
        unsafe fn proc(name: &[u8]) -> Option<*const c_void> {
            let p = wglGetProcAddress(name.as_ptr().cast());
            (!p.is_null() && p as usize > 3 && p as usize != usize::MAX).then_some(p)
        }
        let extensions: unsafe extern "system" fn(Handle) -> *const c_char =
            std::mem::transmute(proc(b"wglGetExtensionsStringARB\0")?);
        let list = extensions(wglGetCurrentDC());
        if list.is_null()
            || !CStr::from_ptr(list)
                .to_bytes()
                .split(|c| *c == b' ')
                .any(|s| s == b"WGL_NV_DX_interop2")
        {
            return None;
        }
        Some(Self {
            open: std::mem::transmute(proc(b"wglDXOpenDeviceNV\0")?),
            close: std::mem::transmute(proc(b"wglDXCloseDeviceNV\0")?),
            register: std::mem::transmute(proc(b"wglDXRegisterObjectNV\0")?),
            unregister: std::mem::transmute(proc(b"wglDXUnregisterObjectNV\0")?),
            lock: std::mem::transmute(proc(b"wglDXLockObjectsNV\0")?),
            unlock: std::mem::transmute(proc(b"wglDXUnlockObjectsNV\0")?),
        })
    }
}
pub fn enable(_gl: &glow::Context) {
    let enabled = std::env::var("UU_D3D11_READBACK").as_deref() != Ok("1")
        && unsafe { Api::load().is_some() };
    d3d11_frame::set_rendering(enabled);
    eprintln!("Windows GPU video interop available={enabled}");
}
pub struct Importer {
    gl: Arc<glow::Context>,
    api: Api,
    device: Handle,
    object: Handle,
    texture: Option<glow::Texture>,
    converter: Option<Converter>,
    current: Option<Arc<Frame>>,
    locked: bool,
    conversion_pending: bool,
}
// The eframe callback is Send+Sync but executes on its owning GL thread.
// No WGL/D3D immediate context method is called from decoder threads here.
unsafe impl Send for Importer {}
impl Importer {
    pub unsafe fn new(gl: Arc<glow::Context>) -> Option<Self> {
        Some(Self {
            gl,
            api: Api::load()?,
            device: std::ptr::null_mut(),
            object: std::ptr::null_mut(),
            texture: None,
            converter: None,
            current: None,
            locked: false,
            conversion_pending: false,
        })
    }
    pub unsafe fn set(&mut self, frame: Arc<Frame>, color: YuvColor) -> Result<(), String> {
        if !self.converter.as_ref().is_some_and(|c| c.matches(&frame)) {
            self.clear();
            let converter = Converter::new(&frame)?;
            self.device = (self.api.open)(converter.device_ptr());
            if self.device.is_null() {
                return Err("WGL cannot share decoder adapter".into());
            }
            let texture = self.gl.create_texture()?;
            self.texture = Some(texture);
            // WGL_ACCESS_READ_ONLY_NV: GL samples; only D3D writes.
            self.object = (self.api.register)(
                self.device,
                converter.texture_ptr(),
                texture.0.get(),
                glow::TEXTURE_2D,
                0,
            );
            self.converter = Some(converter);
            if self.object.is_null() {
                self.clear();
                return Err("WGL D3D11 texture registration failed".into());
            }
        }
        self.current = Some(frame);
        // Mark pending before submission, including partial failure paths.
        self.conversion_pending = true;
        self.converter
            .as_ref()
            .unwrap()
            .convert(self.current.as_ref().unwrap(), color)
    }
    pub unsafe fn bind(&mut self) -> Result<(), String> {
        if self.object.is_null() {
            return Err("WGL video object missing".into());
        }
        if !self.locked {
            if (self.api.lock)(self.device, 1, &mut self.object) == 0 {
                return Err("WGL video synchronization failed".into());
            }
            self.locked = true;
            self.conversion_pending = false;
        }
        self.gl.active_texture(glow::TEXTURE0);
        self.gl.bind_texture(glow::TEXTURE_2D, self.texture);
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
        Ok(())
    }
    pub unsafe fn sampled(&mut self) -> Result<(), String> {
        if self.locked {
            if (self.api.unlock)(self.device, 1, &mut self.object) == 0 {
                return Err("WGL video release failed".into());
            }
            self.locked = false;
        }
        Ok(())
    }
    pub unsafe fn clear(&mut self) {
        if self.locked {
            // Complete GL use before retiring the interop registration.
            self.gl.finish();
            let _ = self.sampled();
        }
        if self.conversion_pending {
            // An import/lock error must not release an MFT sample while a
            // submitted conversion still reads it. Slow path only on failure.
            if let Some(c) = &self.converter {
                let _ = c.readback();
            }
            self.conversion_pending = false;
        }
        if !self.object.is_null() {
            (self.api.unregister)(self.device, self.object);
            self.object = std::ptr::null_mut();
        }
        if let Some(texture) = self.texture.take() {
            self.gl.delete_texture(texture);
        }
        if !self.device.is_null() {
            (self.api.close)(self.device);
            self.device = std::ptr::null_mut();
        }
        self.current = None;
        self.converter = None;
    }
}
impl Drop for Importer {
    fn drop(&mut self) {
        unsafe {
            self.clear();
        }
    }
}
