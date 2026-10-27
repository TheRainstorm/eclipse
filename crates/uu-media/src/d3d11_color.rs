//! Same-device GPU SDR/HDR conversion; native texture references retain GPU work.
use std::{
    ffi::{c_char, c_void, CStr},
    ptr::NonNull,
};
use windows::{
    core::Interface,
    Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D},
};
unsafe extern "C" {
    fn eclipse_color_open(
        device: *mut c_void,
        w: u32,
        h: u32,
        decode: i32,
        hdr: i32,
        shared: i32,
        white: f32,
        error: *mut c_char,
        len: usize,
    ) -> *mut c_void;
    fn eclipse_color_close(value: *mut c_void);
    fn eclipse_color_target_new(value:*mut c_void,error:*mut c_char,len:usize)->*mut c_void;
    fn eclipse_color_target_close(value:*mut c_void);
    fn eclipse_color_convert_into(value:*mut c_void,source:*mut c_void,target:*mut c_void,error:*mut c_char,len:usize)->*mut c_void;
    fn eclipse_color_output(value: *mut c_void) -> *mut c_void;
    fn eclipse_color_convert_sdr(value:*mut c_void,source:*mut c_void,index:u32,full:i32,matrix709:i32,error:*mut c_char,len:usize)->*mut c_void;
    fn eclipse_color_convert(
        value: *mut c_void,
        source: *mut c_void,
        index: u32,
        full: i32,
        error: *mut c_char,
        len: usize,
    ) -> *mut c_void;
}
fn message(error: &[c_char]) -> String {
    unsafe { CStr::from_ptr(error.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}
pub(crate) struct Converter(NonNull<c_void>);
/// An encoder-only destination. Reuse requires the AMF input lease to return;
/// ordinary convert() still allocates an independent texture for every sample.
pub(crate) struct EncodeTarget(NonNull<c_void>);
impl Drop for EncodeTarget {fn drop(&mut self){unsafe{eclipse_color_target_close(self.0.as_ptr());}}}
impl Converter {
    pub fn target(&self)->Result<EncodeTarget,String> {
        let mut error=[0;1024];
        NonNull::new(unsafe{eclipse_color_target_new(self.0.as_ptr(),error.as_mut_ptr(),error.len())}).map(EncodeTarget).ok_or_else(||message(&error))
    }
    pub fn convert_into(&self,source:&ID3D11Texture2D,target:&EncodeTarget)->Result<ID3D11Texture2D,String> {
        let mut error=[0;1024];
        let raw=unsafe{eclipse_color_convert_into(self.0.as_ptr(),source.as_raw(),target.0.as_ptr(),error.as_mut_ptr(),error.len())};
        if raw.is_null(){Err(message(&error))}else{Ok(unsafe{ID3D11Texture2D::from_raw(raw)})}
    }
    pub fn new(
        device: &ID3D11Device,
        size: [u32; 2],
        decode: bool,
        hdr: bool,
        shared: bool,
        white: f32,
    ) -> Result<Self, String> {
        Self::with_sharing(device,size,decode,hdr,i32::from(shared),white)
    }
    pub(crate) fn with_sharing(device:&ID3D11Device,size:[u32;2],decode:bool,hdr:bool,sharing:i32,white:f32)->Result<Self,String> {
        let mut error = [0; 1024];
        let raw = unsafe {
            eclipse_color_open(
                device.as_raw(),
                size[0],
                size[1],
                decode.into(),
                hdr.into(),
                sharing,
                white,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        NonNull::new(raw).map(Self).ok_or_else(|| message(&error))
    }
    pub fn output(&self) -> *mut c_void {
        unsafe { eclipse_color_output(self.0.as_ptr()) }
    }
    pub fn convert_sdr(&self,source:&ID3D11Texture2D,index:u32,color:crate::color::YuvColor)->Result<ID3D11Texture2D,String> {
        use crate::color::YuvColor;
        if color.hdr(){return Err("SDR decoder cannot use HDR colour metadata".into());}
        let mut error=[0;1024];
        let raw=unsafe{eclipse_color_convert_sdr(self.0.as_ptr(),source.as_raw(),index,
            i32::from(matches!(color,YuvColor::Full601|YuvColor::Full709)),i32::from(matches!(color,YuvColor::Full709|YuvColor::Limited709)),error.as_mut_ptr(),error.len())};
        if raw.is_null(){Err(message(&error))}else{Ok(unsafe{ID3D11Texture2D::from_raw(raw)})}
    }
    pub fn convert(
        &self,
        source: &ID3D11Texture2D,
        index: u32,
        full: bool,
    ) -> Result<ID3D11Texture2D, String> {
        let mut error = [0; 1024];
        let raw = unsafe {
            eclipse_color_convert(
                self.0.as_ptr(),
                source.as_raw(),
                index,
                full.into(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if raw.is_null() {
            Err(message(&error))
        } else {
            Ok(unsafe { ID3D11Texture2D::from_raw(raw) })
        }
    }
}
impl Drop for Converter {
    fn drop(&mut self) {
        unsafe {
            eclipse_color_close(self.0.as_ptr());
        }
    }
}
