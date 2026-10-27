//! Shared NT image and D3D11 GPU timeline ownership between producer and reader.
use std::{
    ffi::{c_char, c_void, CStr},
    ptr::NonNull,
};
unsafe extern "C" {
    fn eclipse_external_open(texture: *mut c_void, error: *mut c_char, size: usize) -> *mut c_void;
    fn eclipse_external_close(value: *mut c_void);
    fn eclipse_external_handle(value: *mut c_void, index: u32) -> *mut c_void;
    fn eclipse_external_before_write(
        value: *mut c_void,
        consumed: u64,
        error: *mut c_char,
        size: usize,
    ) -> i32;
    fn eclipse_external_ready(value: *mut c_void, error: *mut c_char, size: usize) -> u64;
    fn eclipse_external_completed(value: *mut c_void) -> u64;
    fn eclipse_external_idle(value: *mut c_void, error: *mut c_char, size: usize) -> i32;
}
fn message(e: &[c_char]) -> String {
    unsafe { CStr::from_ptr(e.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}
pub struct ExternalSync(NonNull<c_void>);
impl ExternalSync {
    /// The converter's immutable output must outlive all shared GPU reads.
    pub(super) fn new(converter: &super::Converter) -> Result<Self, String> {
        let mut e = [0; 1024];
        NonNull::new(unsafe {
            eclipse_external_open(converter.texture_ptr(), e.as_mut_ptr(), e.len())
        })
        .map(Self)
        .ok_or_else(|| message(&e))
    }
    pub fn handles(&self) -> [*mut c_void; 3] {
        std::array::from_fn(|i| unsafe { eclipse_external_handle(self.0.as_ptr(), i as u32) })
    }
    pub fn before_write(&self, consumed: u64) -> Result<(), String> {
        let mut e = [0; 1024];
        if unsafe {
            eclipse_external_before_write(self.0.as_ptr(), consumed, e.as_mut_ptr(), e.len())
        } == 0
        {
            Ok(())
        } else {
            Err(message(&e))
        }
    }
    pub fn ready(&self) -> Result<u64, String> {
        let mut e = [0; 1024];
        let v = unsafe { eclipse_external_ready(self.0.as_ptr(), e.as_mut_ptr(), e.len()) };
        if v > 0 {
            Ok(v)
        } else {
            Err(message(&e))
        }
    }
    pub fn completed(&self) -> u64 {
        unsafe { eclipse_external_completed(self.0.as_ptr()) }
    }
    /// Slow retirement only; the reader must first submit its completion signals.
    pub fn synchronize(&self) -> Result<(), String> {
        let mut e = [0; 1024];
        if unsafe { eclipse_external_idle(self.0.as_ptr(), e.as_mut_ptr(), e.len()) } == 0 {
            Ok(())
        } else {
            Err(message(&e))
        }
    }
}
impl Drop for ExternalSync {
    fn drop(&mut self) {
        unsafe {
            eclipse_external_close(self.0.as_ptr());
        }
    }
}
