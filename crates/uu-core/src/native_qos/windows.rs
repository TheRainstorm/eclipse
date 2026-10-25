//! ABI declarations follow the Windows SDK qos2.h; no static qwave linkage.
use super::{Api, Error, Priority};
use std::{ffi::c_void, net::SocketAddrV4, ptr, sync::Arc};
use windows_sys::Win32::{
    Foundation::{FreeLibrary, GetLastError},
    System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32},
};
type Open = unsafe extern "system" fn(*const Version, *mut *mut c_void) -> i32;
type Add = unsafe extern "system" fn(*mut c_void, usize, *const Address, i32, u32, *mut u32) -> i32;
type Query = unsafe extern "system" fn(
    *mut c_void,
    u32,
    i32,
    *mut u32,
    *mut c_void,
    u32,
    *mut c_void,
) -> i32;
type Remove = unsafe extern "system" fn(*mut c_void, usize, u32, u32) -> i32;
type Close = unsafe extern "system" fn(*mut c_void) -> i32;
#[repr(C)]
struct Version {
    major: u16,
    minor: u16,
}
#[repr(C)]
struct Address {
    family: u16,
    port: u16,
    ip: [u8; 4],
    zero: [u8; 8],
}
struct Library(usize);
impl Drop for Library {
    fn drop(&mut self) {
        unsafe {
            FreeLibrary(self.0 as _);
        }
    }
}
struct Qwave {
    _library: Library,
    open: Open,
    add: Add,
    query: Query,
    remove: Remove,
    close: Close,
}
fn checked(result: i32, api: &'static str) -> Result<(), Error> {
    if result == 0 {
        Err(Error {
            api,
            code: unsafe { GetLastError() },
        })
    } else {
        Ok(())
    }
}
impl Api for Qwave {
    fn open(&self) -> Result<usize, Error> {
        let mut h = ptr::null_mut();
        checked(
            unsafe { (self.open)(&Version { major: 1, minor: 0 }, &mut h) },
            "QOSCreateHandle",
        )?;
        if h.is_null() {
            Err(Error {
                api: "QOSCreateHandle.null",
                code: 6,
            })
        } else {
            Ok(h as usize)
        }
    }
    fn add(&self, h: usize, socket: usize, dest: SocketAddrV4) -> Result<u32, Error> {
        let addr = Address {
            family: 2,
            port: dest.port().to_be(),
            ip: dest.ip().octets(),
            zero: [0; 8],
        };
        let mut flow = 0;
        checked(
            unsafe { (self.add)(h as _, socket, &addr, 3, 2, &mut flow) },
            "QOSAddSocketToFlow",
        )?;
        Ok(flow)
    }
    fn query(&self, h: usize, flow: u32) -> Result<Priority, Error> {
        let mut priority = Priority::default();
        let mut bytes = std::mem::size_of::<Priority>() as u32;
        checked(
            unsafe {
                (self.query)(
                    h as _,
                    flow,
                    1,
                    &mut bytes,
                    (&mut priority as *mut Priority).cast(),
                    0,
                    ptr::null_mut(),
                )
            },
            "QOSQueryFlow.PacketPriority",
        ).map_err(|error| {
            super::event(serde_json::json!({"action":"query_error","flow":flow,"returned_bytes":bytes,"error":error}));
            error
        })?;
        if bytes != std::mem::size_of::<Priority>() as u32 {
            return Err(Error {
                api: "QOSQueryFlow.size",
                code: bytes,
            });
        }
        Ok(priority)
    }
    fn remove(&self, h: usize, socket: usize, flow: u32) -> Result<(), Error> {
        checked(
            unsafe { (self.remove)(h as _, socket, flow, 0) },
            "QOSRemoveSocketFromFlow",
        )
    }
    fn close(&self, h: usize) -> Result<(), Error> {
        checked(unsafe { (self.close)(h as _) }, "QOSCloseHandle")
    }
}
pub(super) fn load() -> Result<Arc<dyn Api>, Error> {
    let name: Vec<u16> = "qwave.dll\0".encode_utf16().collect();
    let h = unsafe { LoadLibraryExW(name.as_ptr(), ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
    if h.is_null() {
        return Err(Error {
            api: "LoadLibraryExW.qwave.System32",
            code: unsafe { GetLastError() },
        });
    }
    let library = Library(h as usize);
    macro_rules! export {
        ($name:literal,$ty:ty) => {{
            let p =
                unsafe { GetProcAddress(h, concat!($name, "\0").as_ptr()) }.ok_or_else(|| {
                    Error {
                        api: concat!("GetProcAddress.", $name),
                        code: unsafe { GetLastError() },
                    }
                })?;
            unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(p) }
        }};
    }
    let result = Qwave {
        open: export!("QOSCreateHandle", Open),
        add: export!("QOSAddSocketToFlow", Add),
        query: export!("QOSQueryFlow", Query),
        remove: export!("QOSRemoveSocketFromFlow", Remove),
        close: export!("QOSCloseHandle", Close),
        _library: library,
    };
    Ok(Arc::new(result))
}
