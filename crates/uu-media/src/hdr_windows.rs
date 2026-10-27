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

unsafe extern "C" {
    fn eclipse_native_present_deactivate(value:*mut c_void,error:*mut c_char,len:usize)->i32;
    fn eclipse_native_present_info(value:*mut c_void,hdr:*mut i32,white:*mut f32);
    fn eclipse_native_present_open(hwnd:*mut c_void,error:*mut c_char,len:usize)->*mut c_void;
    fn eclipse_native_present_close(value:*mut c_void);
    fn eclipse_native_present_device(value:*mut c_void)->*mut c_void;
    fn eclipse_native_present_texture(value:*mut c_void,index:u32)->*mut c_void;
    fn eclipse_native_present_resize(value:*mut c_void,w:u32,h:u32,error:*mut c_char,len:usize)->i32;
    fn eclipse_native_present_begin(value:*mut c_void);
    fn eclipse_native_present_debug(value:*mut c_void,enabled:i32,hdr:i32,white:f32);
    fn eclipse_native_present_debug_pixels(value:*mut c_void,data:*mut u16,capacity:usize,w:*mut u32,h:*mut u32,serial:*mut u64)->usize;
    fn eclipse_native_present_image(value:*mut c_void,id:u64,handles:*const *mut c_void,ready:u64,consumed:u64,rect:*const f32,hdr:i32,white:f32,error:*mut c_char,len:usize)->i32;
    fn eclipse_native_present_cursor(value:*mut c_void,w:u32,h:u32,alpha:*const u8,xored:*const u8,error:*mut c_char,len:usize)->i32;
    fn eclipse_native_present_cursor_position(value:*mut c_void,rect:*const f32);
    fn eclipse_native_present_frame(value:*mut c_void,index:u32,vsync:i32,submitted:*mut u64,error:*mut c_char,len:usize)->i32;
}
/// D3D video and GL UI share the final scRGB composition; no video WGL import.
/// All methods run on the owning native window's rendering thread.
pub struct NativePresentation(NonNull<c_void>);
impl NativePresentation {
    pub unsafe fn new(hwnd:*mut c_void)->Result<Self,String>{let mut e=[0;1024];NonNull::new(eclipse_native_present_open(hwnd,e.as_mut_ptr(),e.len())).map(Self).ok_or_else(||message(&e))}
    pub fn device_ptr(&self)->*mut c_void{unsafe{eclipse_native_present_device(self.0.as_ptr())}}
    pub fn texture_ptr(&self,index:u32)->*mut c_void{unsafe{eclipse_native_present_texture(self.0.as_ptr(),index)}}
    pub fn resize(&mut self,size:[u32;2])->Result<(),String>{let mut e=[0;1024];if unsafe{eclipse_native_present_resize(self.0.as_ptr(),size[0],size[1],e.as_mut_ptr(),e.len())}==0{Ok(())}else{Err(message(&e))}}
    pub fn begin(&mut self){unsafe{eclipse_native_present_begin(self.0.as_ptr());}}
    pub fn info(&self)->(bool,f32){let mut hdr=0;let mut white=80.;unsafe{eclipse_native_present_info(self.0.as_ptr(),&mut hdr,&mut white);} (hdr!=0,white)}
    pub fn image(&mut self,id:u64,handles:[usize;3],ready:u64,consumed:u64,rect:[f32;4],hdr:bool,white:f32)->Result<(),String>{
        let handles=handles.map(|h|h as *mut c_void);let mut e=[0;1024];
        if unsafe{eclipse_native_present_image(self.0.as_ptr(),id,handles.as_ptr(),ready,consumed,rect.as_ptr(),i32::from(hdr),white,e.as_mut_ptr(),e.len())}==0{Ok(())}else{Err(message(&e))}
    }
    pub fn cursor(&mut self,size:[u32;2],alpha:&[u8],xored:&[u8])->Result<(),String>{
        if size.iter().any(|v|*v==0||*v>512)||alpha.len()!=size[0] as usize*size[1] as usize*4||xored.len()!=alpha.len(){return Err("invalid native cursor pixels".into());}
        let mut e=[0;1024];if unsafe{eclipse_native_present_cursor(self.0.as_ptr(),size[0],size[1],alpha.as_ptr(),xored.as_ptr(),e.as_mut_ptr(),e.len())}==0{Ok(())}else{Err(message(&e))}
    }
    pub fn cursor_position(&mut self,rect:Option<[f32;4]>){unsafe{eclipse_native_present_cursor_position(self.0.as_ptr(),rect.as_ref().map_or(std::ptr::null(),|r|r.as_ptr()));}}
    pub fn present(&mut self,index:u32,vsync:bool,submitted:&mut u64)->Result<(),String>{let mut e=[0;1024];if unsafe{eclipse_native_present_frame(self.0.as_ptr(),index,i32::from(vsync),submitted,e.as_mut_ptr(),e.len())}==0{Ok(())}else{Err(message(&e))}}
    /// Generated-pixel diagnostics; this does not change desktop HDR settings.
    pub fn inspect_pixels(&mut self,enabled:bool,color:Option<(bool,f32)>){unsafe{eclipse_native_present_debug(self.0.as_ptr(),i32::from(enabled),color.map_or(-1,|v|i32::from(v.0)),color.map_or(0.,|v|v.1));}}
    pub fn inspected_pixels(&self)->Option<([u32;2],u64,Vec<u16>)>{
        let(mut w,mut h,mut serial)=(0,0,0);let size=unsafe{eclipse_native_present_debug_pixels(self.0.as_ptr(),std::ptr::null_mut(),0,&mut w,&mut h,&mut serial)};
        if size==0||w>2048||h>2048||size!=w as usize*h as usize*4{return None;}
        let mut pixels=vec![0;size];unsafe{eclipse_native_present_debug_pixels(self.0.as_ptr(),pixels.as_mut_ptr(),size,&mut w,&mut h,&mut serial);}
        Some(([w,h],serial,pixels))
    }
    pub fn deactivate(&mut self)->Result<(),String>{let mut e=[0;1024];if unsafe{eclipse_native_present_deactivate(self.0.as_ptr(),e.as_mut_ptr(),e.len())}==0{Ok(())}else{Err(message(&e))}}
}
impl Drop for NativePresentation{fn drop(&mut self){unsafe{eclipse_native_present_close(self.0.as_ptr());}}}
