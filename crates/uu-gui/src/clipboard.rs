//! System clipboard runs off the UI thread. Only the foreground stream may
//! read/write text. A separately permitted host uses a peer-leased active state.
//! Clipboard contents never appear in logs or command arguments.
use std::sync::Arc;
use uu_core::clipboard::Clipboard;

pub fn start(control: Arc<Clipboard>, window: isize) -> std::io::Result<()> {
    start_following(control,Arc::new(std::sync::atomic::AtomicIsize::new(window)))
}
pub fn start_following(control:Arc<Clipboard>,window:Arc<std::sync::atomic::AtomicIsize>)->std::io::Result<()> {
    start_role(control,window,true)
}

/// Only an explicit native-host checkbox may call this; readiness is leased to its peer.
pub fn start_host(control: Arc<Clipboard>, window: isize) -> std::io::Result<()> {
    start_role(control,Arc::new(std::sync::atomic::AtomicIsize::new(window)),false)
}
fn start_role(
    control: Arc<Clipboard>,
    owner: Arc<std::sync::atomic::AtomicIsize>,
    foreground_only: bool,
) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let window=owner.load(std::sync::atomic::Ordering::Acquire);
        if window == 0
            || unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsWindow(window as _) } == 0
        {
            control
                .available
                .store(false, std::sync::atomic::Ordering::Release);
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "剪贴板所有者窗口尚未就绪",
            ));
        }
        // Use the caller's authorized desktop explicitly. A worker must not
        // inherit a stale startup desktop after an RDP/session transition.
        let desktop = unsafe {
            windows_sys::Win32::System::StationsAndDesktops::GetThreadDesktop(
                windows_sys::Win32::System::Threading::GetCurrentThreadId(),
            )
        } as usize;
        if desktop == 0 {
            return Err(std::io::Error::last_os_error());
        }
        std::thread::Builder::new()
            .name("uu-clipboard".into())
            .spawn(move || {
                if unsafe {
                    windows_sys::Win32::System::StationsAndDesktops::SetThreadDesktop(desktop as _)
                } == 0
                {
                    control
                        .available
                        .store(false, std::sync::atomic::Ordering::Release);
                    control.status("无法打开当前桌面的剪贴板");
                    return;
                }
                run(control, owner, foreground_only)
            })?;
    }
    #[cfg(target_os = "linux")]
    {
        let _ = (&owner, foreground_only);
        std::thread::Builder::new()
            .name("uu-clipboard".into())
            .spawn(move || run_linux(control, !foreground_only))?;
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (&owner, foreground_only);
        control.status("当前构建未启用系统剪贴板");
    }
    Ok(())
}
#[cfg(any(windows, target_os = "linux", test))]
fn canonical(text: &str) -> String {
    text.replace("\r\n", "\n")
}

#[cfg(target_os = "linux")]
fn run_linux(control: Arc<Clipboard>, suppress_initial: bool) {
    use std::sync::atomic::Ordering::{Acquire, Release};
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(value) => value,
        Err(_) => {
            control.available.store(false, Release);
            control.status("无法打开系统剪贴板，请检查 Wayland / X11 会话");
            return;
        }
    };
    control.available.store(true, Release);
    let mut last_text = None;
    while !control.stopped.load(Acquire) {
        if control.ready() {
            let mut local_changed = false;
            if let Ok(value) = clipboard.get_text() {
                if value.len() <= uu_core::clipboard::MAX_TEXT && !value.contains('\0') {
                    let normalized = canonical(&value);
                    if last_text.as_ref() != Some(&normalized) {
                        if !control.read_only.load(Acquire)
                            && (!suppress_initial || last_text.is_some())
                        {
                            *control.local.lock().unwrap() = Some(value);
                            control.protocol.lock().unwrap().cancel_pending();
                            local_changed = true;
                        }
                        last_text = Some(normalized);
                    }
                }
            }
            // A newly copied local selection takes precedence over a response
            // to an older remote clipboard request, matching the Windows path.
            let remote = control.remote.lock().unwrap().take();
            if let Some(value) = remote.filter(|_| !local_changed) {
                let normalized = canonical(&value);
                if last_text.as_ref() != Some(&normalized) && control.ready() {
                    if clipboard.set_text(value.clone()).is_ok() {
                        last_text = Some(normalized);
                        control.status("文本剪贴板已同步");
                    } else {
                        let mut pending = control.remote.lock().unwrap();
                        if pending.is_none() && control.ready() {
                            *pending = Some(value);
                        }
                        control.status("本地剪贴板写入繁忙，正在重试");
                    }
                }
            }
        } else {
            last_text = None;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    control.available.store(false, Release);
    control.status("剪贴板已停止");
}

#[cfg(windows)]
fn run(control: Arc<Clipboard>, owner:Arc<std::sync::atomic::AtomicIsize>, foreground_only: bool) {
    use std::sync::atomic::Ordering::{Acquire, Release};
    control.available.store(true, Release);
    let mut last_sequence = 0;
    let mut last_text = None;
    let mut was_active = false;
    while !control.stopped.load(Acquire) {
        let window=owner.load(Acquire);
        let active = control.ready() && (!foreground_only || native::foreground(window));
        if active {
            let sequence = native::sequence();
            let mut local_changed = false;
            if !was_active || sequence != last_sequence {
                match native::read(window, foreground_only) {
                    Ok(Some(value)) => {
                        let normalized = canonical(&value);
                        if last_text.as_ref() != Some(&normalized) {
                            if control.ready() && owner.load(Acquire)==window && !control.read_only.load(Acquire) && (foreground_only || was_active) {
                                *control.local.lock().unwrap() = Some(value);
                                local_changed = true;
                                control.protocol.lock().unwrap().cancel_pending();
                            }
                            last_text = Some(normalized);
                        }
                        last_sequence = sequence;
                    }
                    Ok(None) => {
                        last_sequence = sequence;
                    }
                    Err(()) => control.status("剪贴板被其他程序占用，正在重试"),
                }
            }
            let remote = control.remote.lock().unwrap().take();
            if !local_changed {
                if let Some(value) = remote {
                    let normalized = canonical(&value);
                    if last_text.as_ref() != Some(&normalized)
                        && control.ready()
                        && owner.load(Acquire)==window
                        && (!foreground_only || native::foreground(window))
                    {
                        if native::write(window, &value, foreground_only).is_ok() {
                            last_text = Some(normalized);
                            last_sequence = native::sequence();
                            control.status("文本剪贴板已同步");
                        } else {
                            let mut pending = control.remote.lock().unwrap();
                            if pending.is_none() && control.ready() {
                                *pending = Some(value);
                            }
                            control.status("本地剪贴板写入繁忙，正在重试");
                        }
                    }
                }
            }
        } else {
            last_text = None;
            last_sequence = 0;
        }
        was_active = active;
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    control.available.store(false, Release);
    control.status("剪贴板已停止");
}

#[cfg(windows)]
mod native {
    use windows_sys::Win32::{
        Foundation::{GlobalFree, HWND},
        System::{DataExchange::*, Memory::*},
        UI::WindowsAndMessaging::{GetForegroundWindow, IsWindow},
    };
    const FORMAT: u32 = 13;
    struct Open;
    impl Drop for Open {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }
    pub fn foreground(window: isize) -> bool {
        window != 0 && unsafe { GetForegroundWindow() == window as HWND }
    }
    pub fn sequence() -> u32 {
        unsafe { GetClipboardSequenceNumber() }
    }
    fn open(window: isize, foreground_only: bool) -> Result<Open, ()> {
        if window == 0
            || unsafe { IsWindow(window as HWND) } == 0
            || (foreground_only && !foreground(window))
        {
            return Err(());
        }
        if unsafe { OpenClipboard(window as HWND) } == 0 {
            return Err(());
        }
        Ok(Open)
    }
    pub fn read(window: isize, foreground_only: bool) -> Result<Option<String>, ()> {
        let _open = open(window, foreground_only)?;
        unsafe {
            if IsClipboardFormatAvailable(FORMAT) == 0 {
                return Ok(None);
            }
            let handle = GetClipboardData(FORMAT);
            if handle.is_null() {
                return Err(());
            }
            let size = GlobalSize(handle);
            if size == 0 || size > uu_core::clipboard::MAX_TEXT * 2 + 2 || size % 2 != 0 {
                return Ok(None);
            }
            let pointer = GlobalLock(handle) as *const u16;
            if pointer.is_null() {
                return Err(());
            }
            let units = std::slice::from_raw_parts(pointer, size / 2);
            let end = units.iter().position(|n| *n == 0).unwrap_or(units.len());
            let value = String::from_utf16(&units[..end]).map_err(|_| ());
            GlobalUnlock(handle);
            let value = value?;
            if value.len() > uu_core::clipboard::MAX_TEXT {
                return Ok(None);
            }
            Ok(Some(value))
        }
    }
    pub fn write(window: isize, value: &str, foreground_only: bool) -> Result<(), ()> {
        if value.len() > uu_core::clipboard::MAX_TEXT || value.contains('\0') {
            return Err(());
        }
        let units: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let handle = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2);
            if handle.is_null() {
                return Err(());
            }
            let pointer = GlobalLock(handle) as *mut u16;
            if pointer.is_null() {
                GlobalFree(handle);
                return Err(());
            }
            std::ptr::copy_nonoverlapping(units.as_ptr(), pointer, units.len());
            GlobalUnlock(handle);
            let opened = open(window, foreground_only);
            if opened.is_err() || (foreground_only && !foreground(window)) {
                GlobalFree(handle);
                return Err(());
            }
            let _open = opened.unwrap();
            if EmptyClipboard() == 0 || SetClipboardData(FORMAT, handle).is_null() {
                GlobalFree(handle);
                return Err(());
            }
            Ok(()) // Windows owns the movable allocation after successful SetClipboardData.
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn newline_normalization_prevents_cross_platform_clipboard_echoes() {
        assert_eq!(canonical("中文\r\n🙂"), canonical("中文\n🙂"));
        assert_ne!(canonical("a\rb"), canonical("a\nb"));
    }
}
