//! WGL sharing API used by decoder import and HDR presentation.
use std::ffi::{c_char, c_void, CStr};
pub(crate) type Handle = *mut c_void;
#[link(name = "opengl32")]
extern "system" {
    fn wglGetProcAddress(name: *const c_char) -> *const c_void;
    fn wglGetCurrentDC() -> Handle;
}
#[derive(Clone, Copy)]
pub(crate) struct Api {
    pub(crate) open: unsafe extern "system" fn(Handle) -> Handle,
    pub(crate) close: unsafe extern "system" fn(Handle) -> i32,
    pub(crate) register: unsafe extern "system" fn(Handle, Handle, u32, u32, u32) -> Handle,
    pub(crate) unregister: unsafe extern "system" fn(Handle, Handle) -> i32,
    pub(crate) lock: unsafe extern "system" fn(Handle, i32, *mut Handle) -> i32,
    pub(crate) unlock: unsafe extern "system" fn(Handle, i32, *mut Handle) -> i32,
}
impl Api {
    pub(crate) unsafe fn load() -> Option<Self> {
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
