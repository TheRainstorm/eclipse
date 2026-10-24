//! Native FP16 presentation surface. The caller owns the window and GL thread.
use std::{
    ffi::{c_char, c_void, CStr},
    ptr::NonNull,
};
unsafe extern "C" {
    fn eclipse_hdr_present_open(hwnd: *mut c_void, error: *mut c_char, len: usize) -> *mut c_void;
    fn eclipse_hdr_present_close(value: *mut c_void);
    fn eclipse_hdr_present_device(value: *mut c_void) -> *mut c_void;
    fn eclipse_hdr_present_texture(value: *mut c_void) -> *mut c_void;
    fn eclipse_hdr_present_resize(
        value: *mut c_void,
        w: u32,
        h: u32,
        error: *mut c_char,
        len: usize,
    ) -> i32;
    fn eclipse_hdr_present_info(value: *mut c_void, hdr: *mut i32, white: *mut f32);
    fn eclipse_hdr_present_deactivate(value: *mut c_void, error: *mut c_char, len: usize) -> i32;
    fn eclipse_hdr_present_frame(
        value: *mut c_void,
        vsync: i32,
        error: *mut c_char,
        len: usize,
    ) -> i32;
    fn eclipse_hdr_desktop_white(name: *const u16) -> f32;
}
fn message(error: &[c_char]) -> String {
    unsafe { CStr::from_ptr(error.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}
pub struct Presentation(NonNull<c_void>);
impl Presentation {
    /// The HWND and its owning UI thread must outlive this surface.
    pub unsafe fn new(hwnd: *mut c_void) -> Result<Self, String> {
        let mut error = [0; 1024];
        let raw = unsafe { eclipse_hdr_present_open(hwnd, error.as_mut_ptr(), error.len()) };
        NonNull::new(raw).map(Self).ok_or_else(|| message(&error))
    }
    pub fn device_ptr(&self) -> *mut c_void {
        unsafe { eclipse_hdr_present_device(self.0.as_ptr()) }
    }
    pub fn texture_ptr(&self) -> *mut c_void {
        unsafe { eclipse_hdr_present_texture(self.0.as_ptr()) }
    }
    /// The previous texture must be unregistered from WGL before resizing.
    pub fn resize(&mut self, size: [u32; 2]) -> Result<(), String> {
        let mut error = [0; 1024];
        if unsafe {
            eclipse_hdr_present_resize(
                self.0.as_ptr(),
                size[0],
                size[1],
                error.as_mut_ptr(),
                error.len(),
            )
        } == 0
        {
            Ok(())
        } else {
            Err(message(&error))
        }
    }
    pub fn info(&self) -> (bool, f32) {
        let mut hdr = 0;
        let mut white = 80.;
        unsafe {
            eclipse_hdr_present_info(self.0.as_ptr(), &mut hdr, &mut white);
        }
        (hdr != 0, white)
    }
    pub fn deactivate(&mut self) -> Result<(), String> {
        let mut error = [0; 1024];
        if unsafe {
            eclipse_hdr_present_deactivate(self.0.as_ptr(), error.as_mut_ptr(), error.len())
        } == 0
        {
            Ok(())
        } else {
            Err(message(&error))
        }
    }
    pub fn present(&mut self, vsync: bool) -> Result<(), String> {
        let mut error = [0; 1024];
        if unsafe {
            eclipse_hdr_present_frame(
                self.0.as_ptr(),
                vsync.into(),
                error.as_mut_ptr(),
                error.len(),
            )
        } == 0
        {
            Ok(())
        } else {
            Err(message(&error))
        }
    }
}
impl Drop for Presentation {
    fn drop(&mut self) {
        unsafe {
            eclipse_hdr_present_close(self.0.as_ptr());
        }
    }
}
pub fn desktop_sdr_white(name: &[u16]) -> f32 {
    let mut value = name.to_vec();
    if value.last() != Some(&0) {
        value.push(0);
    }
    unsafe { eclipse_hdr_desktop_white(value.as_ptr()) }
}
