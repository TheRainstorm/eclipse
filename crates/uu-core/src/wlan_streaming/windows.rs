//! Dynamically loaded system WLAN API, no notifications and no global settings.
use super::{Api, Interface};
use std::{ffi::c_void, ptr, sync::Arc};
use windows_sys::{
    Win32::{
        Foundation::{FreeLibrary, GetLastError},
        System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
    },
    core::GUID,
};
type Open = unsafe extern "system" fn(u32, *const c_void, *mut u32, *mut *mut c_void) -> u32;
type Close = unsafe extern "system" fn(*mut c_void, *const c_void) -> u32;
type Enum = unsafe extern "system" fn(*mut c_void, *const c_void, *mut *mut c_void) -> u32;
type Query = unsafe extern "system" fn(
    *mut c_void,
    *const GUID,
    i32,
    *const c_void,
    *mut u32,
    *mut *mut c_void,
    *mut i32,
) -> u32;
type Set = unsafe extern "system" fn(
    *mut c_void,
    *const GUID,
    i32,
    u32,
    *const c_void,
    *const c_void,
) -> u32;
type Free = unsafe extern "system" fn(*mut c_void);
struct Library(usize);
impl Drop for Library {
    fn drop(&mut self) {
        unsafe {
            FreeLibrary(self.0 as _);
        }
    }
}
struct Wlan {
    _library: Library,
    open: Open,
    close: Close,
    enumerate: Enum,
    query: Query,
    set: Set,
    free: Free,
}
struct Memory {
    ptr: *mut c_void,
    free: Free,
}
impl Drop for Memory {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                (self.free)(self.ptr);
            }
        }
    }
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Info {
    guid: GUID,
    description: [u16; 256],
    state: u32,
}
fn guid(id: [u8; 16]) -> GUID {
    unsafe { ptr::read_unaligned(id.as_ptr().cast()) }
}
impl Api for Wlan {
    fn open(&self) -> Result<(usize, u32), u32> {
        let mut handle = ptr::null_mut();
        let mut version = 0;
        let result = unsafe { (self.open)(2, ptr::null(), &mut version, &mut handle) };
        if result != 0 {
            Err(result)
        } else if handle.is_null() {
            Err(13)
        } else {
            Ok((handle as usize, version))
        }
    }
    fn interfaces(&self, handle: usize) -> Result<Vec<Interface>, u32> {
        let mut data = ptr::null_mut();
        let result = unsafe { (self.enumerate)(handle as _, ptr::null(), &mut data) };
        let _memory = Memory {
            ptr: data,
            free: self.free,
        };
        if result != 0 {
            return Err(result);
        }
        if data.is_null() {
            return Err(13);
        }
        // WlanEnumInterfaces supplies a counted flexible array, no byte-length
        // out parameter. Bound the trusted system count before pointer math.
        let count = unsafe { ptr::read_unaligned(data.cast::<u32>()) } as usize;
        if count > 1024 {
            return Err(13);
        }
        let items = unsafe { data.cast::<u8>().add(8).cast::<Info>() };
        let mut interfaces = Vec::with_capacity(count);
        for index in 0..count {
            let info = unsafe { ptr::read_unaligned(items.add(index)) };
            let mut id = [0; 16];
            unsafe {
                ptr::copy_nonoverlapping(
                    (&info.guid as *const GUID).cast::<u8>(),
                    id.as_mut_ptr(),
                    16,
                );
            }
            interfaces.push(Interface {
                id,
                state: info.state,
            });
        }
        Ok(interfaces)
    }
    fn query(&self, handle: usize, interface: &Interface) -> Result<bool, u32> {
        let mut size = 0;
        let mut data = ptr::null_mut();
        let mut value_type = 0;
        let result = unsafe {
            (self.query)(
                handle as _,
                &guid(interface.id),
                3,
                ptr::null(),
                &mut size,
                &mut data,
                &mut value_type,
            )
        };
        let _memory = Memory {
            ptr: data,
            free: self.free,
        };
        if result != 0 {
            return Err(result);
        }
        if size != std::mem::size_of::<i32>() as u32 || data.is_null() {
            return Err(13);
        }
        Ok(unsafe { ptr::read_unaligned(data.cast::<i32>()) } != 0)
    }
    fn request(&self, handle: usize, interface: &Interface) -> u32 {
        let enabled = 1i32;
        unsafe {
            (self.set)(
                handle as _,
                &guid(interface.id),
                3,
                std::mem::size_of_val(&enabled) as u32,
                (&enabled as *const i32).cast(),
                ptr::null(),
            )
        }
    }
    fn close(&self, handle: usize) -> u32 {
        unsafe { (self.close)(handle as _, ptr::null()) }
    }
}
pub(super) fn load() -> Result<Arc<dyn Api>, u32> {
    let name = "wlanapi.dll\0".encode_utf16().collect::<Vec<_>>();
    let module =
        unsafe { LoadLibraryExW(name.as_ptr(), ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
    if module.is_null() {
        return Err(unsafe { GetLastError() });
    }
    let library = Library(module as usize);
    macro_rules! function {
        ($name:literal,$kind:ty) => {{
            let address = unsafe { GetProcAddress(module, $name.as_ptr()) }.ok_or(127u32)?;
            unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, $kind>(address) }
        }};
    }
    let open = function!(b"WlanOpenHandle\0", Open);
    let close = function!(b"WlanCloseHandle\0", Close);
    let enumerate = function!(b"WlanEnumInterfaces\0", Enum);
    let query = function!(b"WlanQueryInterface\0", Query);
    let set = function!(b"WlanSetInterface\0", Set);
    let free = function!(b"WlanFreeMemory\0", Free);
    Ok(Arc::new(Wlan {
        _library: library,
        open,
        close,
        enumerate,
        query,
        set,
        free,
    }))
}
