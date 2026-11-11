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
const RESTORABLE: *const u16 = windows_sys::w!("Eclipse.BossKey.Restorable.1");
const RESTORE_TARGET: *const u16 = windows_sys::w!("Eclipse.BossKey.RestoreTarget.1");
const ACTIVATE: *const u16 = windows_sys::w!("Eclipse.BossKey.Activate.1");
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
    RemovePropW(window, RESTORABLE);
    RemovePropW(window, RESTORE_TARGET);
    RemovePropW(window, ACTIVATE);
    results().lock().unwrap().remove(&(window as isize));
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
    if active && super::hidden() { broadcast(false, false); }
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
fn results() -> &'static std::sync::Mutex<std::collections::HashMap<isize, serde_json::Value>> {
    static RESULTS: OnceLock<std::sync::Mutex<std::collections::HashMap<isize, serde_json::Value>>> = OnceLock::new();
    RESULTS.get_or_init(Default::default)
}
pub fn take_focus_result(window: HWND) -> Option<serde_json::Value> {
    let mut event = results().lock().unwrap().remove(&(window as isize))?;
    // Cross-input-queue activation can finish after SetForegroundWindow returns.
    event["foreground_when_observed"] = unsafe { (GetForegroundWindow() == window).into() };
    Some(event)
}
pub fn receive(window: HWND, hidden: bool) {
    super::observe(hidden);
    unsafe {
        let activation = if hidden { 0 } else { RemovePropW(window, ACTIVATE) as isize };
        if activation != 0 && !GetPropW(window, RESTORABLE).is_null() {
            // Handle the foreground grant in the HWND dispatch, before a full
            // GUI frame. A later key-up in another process revokes ASFW rights.
            SetWindowPos(window, HWND_TOP, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
            let accepted = SetForegroundWindow(window) != 0;
            let matches = GetForegroundWindow() == window;
            results().lock().unwrap().insert(window as isize, serde_json::json!({
                "action":"boss_restore", "foreground_granted":activation == 2,
                "accepted":accepted, "foreground_matches":matches,
            }));
        }
    }
}
pub fn toggle() -> bool {
    if !available() || recording_active() {
        return false;
    }
    let hidden = unsafe { !any(2, false) };
    broadcast(hidden, !hidden);
    true
}
pub fn restore_if_unavailable() {
    if super::hidden() && !available() {
        broadcast(false, false);
    }
}
pub fn normal_visibility(native: &winit::window::Window, visible: bool) {
    if visible { return; }
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = native.window_handle() else { return; };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else { return; };
    let window = handle.hwnd.get() as HWND;
    unsafe { RemovePropW(window, RESTORABLE); RemovePropW(window, RESTORE_TARGET); RemovePropW(window, ACTIVATE); }
}
fn broadcast(hidden: bool, activate: bool) {
    unsafe extern "system" fn collect(window: HWND, data: LPARAM) -> BOOL {
        if !GetPropW(window, MARKER).is_null() {
            (*(data as *mut Vec<HWND>)).push(window);
        }
        1
    }
    unsafe {
        let mut windows = vec![];
        EnumWindows(Some(collect), &mut windows as *mut Vec<HWND> as isize);
        // EnumWindows supplies Z order. An explicit restore has one target,
        // including when the foreground at hide time belonged to another app.
        let target = if hidden {
            windows.iter().copied().find(|&w| IsWindowVisible(w) != 0 && IsIconic(w) == 0)
        } else {
            windows.iter().copied().find(|&w| !GetPropW(w, RESTORE_TARGET).is_null())
                .or_else(|| windows.iter().copied().find(|&w| !GetPropW(w, RESTORABLE).is_null()))
        };
        let mut granted = false;
        if activate {
            if let Some(target) = target {
                let mut pid = 0;
                GetWindowThreadProcessId(target, &mut pid);
                // The hotkey owner received the input gesture. Transfer that
                // foreground permission only to the chosen window's process.
                granted = AllowSetForegroundWindow(pid) != 0;
            }
        }
        for &window in &windows {
            if GetPropW(window, MARKER).is_null() { continue; }
            RemovePropW(window, ACTIVATE);
            if hidden {
                RemovePropW(window, RESTORABLE);
                RemovePropW(window, RESTORE_TARGET);
                if IsWindowVisible(window) != 0 && IsIconic(window) == 0 { SetPropW(window, RESTORABLE, 1usize as _); }
                if Some(window) == target { SetPropW(window, RESTORE_TARGET, 1usize as _); }
            } else if activate && Some(window) == target {
                SetPropW(window, ACTIVATE, if granted { 2usize } else { 1usize } as _);
            }
            SetPropW(window, MARKER, if hidden { 2 } else { 1 } as _);
        }
        for window in windows {
            // Use the foreground grant while handling the hotkey, before an
            // unrelated key-up can revoke it. Never wait on every GUI owner.
            if !(activate && Some(window) == target
                && SendMessageTimeoutW(window, message(), hidden as usize, 0,
                    SMTO_ABORTIFHUNG | SMTO_BLOCK, 100, std::ptr::null_mut()) != 0) {
                // A busy UI still receives the visibility change. ACTIVATE is
                // consumed once, including if a timed-out dispatch completes.
                PostMessageW(window, message(), hidden as usize, 0);
            }
        }
    }
    super::observe(hidden);
}
