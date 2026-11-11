//! One desktop shortcut broadcasts visibility to marked Eclipse windows only.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    OnceLock,
};
use windows_sys::Win32::{
    Foundation::{BOOL, HWND, LPARAM},
    UI::WindowsAndMessaging::*,
};
const MARKER: *const u16 = windows_sys::w!("Eclipse.BossKey.Window.1");
const REGISTERED: *const u16 = windows_sys::w!("Eclipse.BossKey.Ready.1");
const RECORDING: *const u16 = windows_sys::w!("Eclipse.Shortcut.Recording.1");
static OWNED: AtomicBool = AtomicBool::new(false);
pub fn message() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    *MESSAGE.get_or_init(|| {
        let name: Vec<u16> = "Eclipse.BossKey.Visibility.1\0".encode_utf16().collect();
        unsafe { RegisterWindowMessageW(name.as_ptr()) }
    })
}
pub unsafe fn mark(window: HWND) {
    if window.is_null() {
        return;
    }
    let hidden = any(2, false);
    super::observe(hidden);
    SetPropW(window, MARKER, if hidden { 2 } else { 1 } as _);
    if OWNED.load(Ordering::Acquire) {
        SetPropW(window, REGISTERED, 1usize as _);
    }
}
pub unsafe fn unmark(window: HWND) {
    RemovePropW(window, MARKER);
    RemovePropW(window, REGISTERED);
    RemovePropW(window, RECORDING);
}
struct Query {
    value: isize,
    ready: bool,
    found: bool,
    recording: bool,
}
unsafe extern "system" fn query(window: HWND, data: LPARAM) -> BOOL {
    let query = &mut *(data as *mut Query);
    let marker = GetPropW(window, MARKER) as isize;
    if marker != 0
        && if query.recording {
            !GetPropW(window, RECORDING).is_null()
        } else if query.ready {
            !GetPropW(window, REGISTERED).is_null()
        } else {
            marker == query.value
        }
    {
        query.found = true;
        return 0;
    }
    1
}
unsafe fn any(value: isize, ready: bool) -> bool {
    let mut data = Query {
        value,
        ready,
        found: false,
        recording: false,
    };
    EnumWindows(Some(query), &mut data as *mut _ as isize);
    data.found
}
pub fn recording_active() -> bool {
    let mut data = Query { value: 0, ready: false, found: false, recording: true };
    unsafe { EnumWindows(Some(query), &mut data as *mut _ as isize); }
    data.found
}
pub unsafe fn recording_window(window: HWND) -> bool {
    !GetPropW(window, RECORDING).is_null()
}
pub fn recording(window: HWND, active: bool) {
    unsafe {
        if active { SetPropW(window, RECORDING, 1usize as _); }
        else { RemovePropW(window, RECORDING); }
    }
    if active && super::hidden() { broadcast(false); }
}
pub fn available() -> bool {
    OWNED.load(Ordering::Acquire) || unsafe { any(0, true) }
}
pub fn registered(owned: bool) {
    OWNED.store(owned, Ordering::Release);
    unsafe extern "system" fn update(window: HWND, data: LPARAM) -> BOOL {
        let mut pid = 0;
        GetWindowThreadProcessId(window, &mut pid);
        if pid == windows_sys::Win32::System::Threading::GetCurrentProcessId()
            && !GetPropW(window, MARKER).is_null()
        {
            if data != 0 {
                SetPropW(window, REGISTERED, 1usize as _);
            } else {
                RemovePropW(window, REGISTERED);
            }
        }
        1
    }
    unsafe {
        EnumWindows(Some(update), owned as isize);
    }
}
pub fn receive(hidden: bool) {
    super::observe(hidden);
}
pub fn toggle() -> bool {
    if !available() || recording_active() {
        return false;
    }
    broadcast(unsafe { !any(2, false) });
    true
}
pub fn restore_if_unavailable() {
    if super::hidden() && !available() {
        broadcast(false);
    }
}
fn broadcast(hidden: bool) {
    unsafe extern "system" fn send(window: HWND, data: LPARAM) -> BOOL {
        if !GetPropW(window, MARKER).is_null() {
            SetPropW(window, MARKER, if data != 0 { 2 } else { 1 } as _);
            PostMessageW(window, message(), data as usize, 0);
        }
        1
    }
    super::observe(hidden);
    unsafe {
        EnumWindows(Some(send), hidden as isize);
    }
}
