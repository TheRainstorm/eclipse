//! The console host owns this short-lived tool window even when the launcher
//! is closed. No activation, GUI runtime, tray icon or polling is required.
use anyhow::{Context, Result};
use std::{mem::size_of, ptr, sync::OnceLock};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{HiDpi::*, WindowsAndMessaging::*},
};

const CLASS: &str = "Eclipse.ConnectionNotice.v1";
struct Text { title: Vec<u16>, body: Vec<u16>, hint: Vec<u16>, scale: f32 }
fn wide(text: &str) -> Vec<u16> { text.encode_utf16().chain([0]).collect() }

pub fn show(record: &crate::connection_history::Record) {
    let name = record.name.clone();
    if let Err(error) = std::thread::Builder::new().name("host-notice".into()).spawn(move || {
        if let Err(error) = run(&name) { tracing::warn!(%error, "connection notice unavailable"); }
    }) { tracing::warn!(%error, "connection notice worker unavailable"); }
}

fn run(name: &str) -> Result<()> {
    unsafe {
        // This is a dedicated thread. Its DPI context cannot affect the host's
        // capture/input owners or a foreground GUI thread.
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let module = GetModuleHandleW(ptr::null());
        let class_name = wide(CLASS);
        static REGISTERED: OnceLock<bool> = OnceLock::new();
        anyhow::ensure!(*REGISTERED.get_or_init(|| {
            let class = WNDCLASSW { lpfnWndProc: Some(procedure), hInstance: module,
                lpszClassName: class_name.as_ptr(), ..std::mem::zeroed() };
            RegisterClassW(&class) != 0
        }), "could not register connection notice");
        let monitor = MonitorFromPoint(windows_sys::Win32::Foundation::POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..std::mem::zeroed() };
        anyhow::ensure!(GetMonitorInfoW(monitor, &mut info) != 0, "could not locate notification area");
        let scale = GetDpiForSystem().max(96) as f32 / 96.0;
        let px = |value: i32| (value as f32 * scale).round() as i32;
        let mut text = Text { title: wide(uu_i18n::text("Eclipse · 连接提醒")),
            body: wide(&uu_i18n::format!("{name} 已连接")),
            hint: wide(uu_i18n::text("服务管理 → 会话，可查看连接历史")), scale };
        let window = CreateWindowExW(WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            wide(CLASS).as_ptr(), wide("Eclipse connection notice").as_ptr(), WS_POPUP | WS_BORDER,
            info.rcWork.right - px(376), info.rcWork.bottom - px(124), px(360), px(108),
            ptr::null_mut(), ptr::null_mut(), module, &mut text as *mut Text as _);
        anyhow::ensure!(!window.is_null(), "could not create connection notice");
        if SetTimer(window, 1, super::DURATION.as_millis() as u32, None) == 0 {
            DestroyWindow(window);
            return Err(std::io::Error::last_os_error()).context("could not schedule notice dismissal");
        }
        ShowWindow(window, SW_SHOWNOACTIVATE);
        let mut message = std::mem::zeroed();
        loop {
            let result = GetMessageW(&mut message, ptr::null_mut(), 0, 0);
            if result == 0 { break; }
            if result == -1 { DestroyWindow(window); anyhow::bail!("connection notice message loop failed"); }
            TranslateMessage(&message); DispatchMessageW(&message);
        }
    }
    Ok(())
}
unsafe extern "system" fn procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_NCCREATE => {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        WM_MOUSEACTIVATE => return MA_NOACTIVATE as _,
        WM_TIMER | WM_LBUTTONUP => { DestroyWindow(window); return 0; }
        WM_DESTROY => { KillTimer(window, 1); PostQuitMessage(0); return 0; }
        WM_PAINT => {
            let text = GetWindowLongPtrW(window, GWLP_USERDATA) as *const Text;
            if text.is_null() { return DefWindowProcW(window, message, wparam, lparam); }
            let text = &*text;
            let mut paint = std::mem::zeroed();
            let dc = BeginPaint(window, &mut paint);
            let mut bounds = std::mem::zeroed(); GetClientRect(window, &mut bounds);
            let brush = CreateSolidBrush(0x00fafafa); FillRect(dc, &bounds, brush); DeleteObject(brush);
            let accent = CreateSolidBrush(0x00d17b30);
            FillRect(dc, &RECT { right: (4.0 * text.scale) as i32, ..bounds }, accent); DeleteObject(accent);
            SetBkMode(dc, TRANSPARENT as _);
            for (value, y, height, font_size, color) in [
                (&text.title, 12, 20, 13, 0x00605040),
                (&text.body, 36, 25, 16, 0x00202020),
                (&text.hint, 74, 20, 12, 0x00606060),
            ] {
                let px = |v: i32| (v as f32 * text.scale).round() as i32;
                let font = CreateFontW(-px(font_size), 0, 0, 0, FW_NORMAL as _, 0, 0, 0,
                    DEFAULT_CHARSET as _, OUT_DEFAULT_PRECIS as _, CLIP_DEFAULT_PRECIS as _, CLEARTYPE_QUALITY as _,
                    DEFAULT_PITCH as _, wide("Segoe UI").as_ptr());
                let old = SelectObject(dc, font);
                SetTextColor(dc, color);
                let mut rect = RECT { left: px(18), top: px(y), right: bounds.right - px(16), bottom: px(y + height) };
                DrawTextW(dc, value.as_ptr(), value.len() as i32 - 1, &mut rect, DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX);
                SelectObject(dc, old); DeleteObject(font);
            }
            EndPaint(window, &paint); return 0;
        }
        _ => {},
    }
    DefWindowProcW(window, message, wparam, lparam)
}

#[cfg(test)]
#[path = "../../../../tools/native/windows_owned_station.rs"]
mod station;
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    unsafe extern "system" fn find(window: HWND, data: LPARAM) -> i32 {
        let mut class = [0u16; 128];
        let count = GetClassNameW(window, class.as_mut_ptr(), class.len() as _);
        if String::from_utf16_lossy(&class[..count.max(0) as usize]) == CLASS && IsWindowVisible(window) != 0 {
            *(data as *mut usize) = window as usize; return 0;
        }
        1
    }
    #[test]
    #[ignore = "requires an owned noninteractive Windows station"]
    fn owned_notice_never_activates_and_destroys_itself_after_three_seconds() {
        let _station = station::Station::create().unwrap();
        let thread = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
        let observer = std::thread::spawn(move || {
            let start = Instant::now();
            loop {
                let mut window = 0usize;
                unsafe { EnumThreadWindows(thread, Some(find), &mut window as *mut usize as _); }
                if window != 0 {
                    let window = window as HWND;
                    unsafe {
                        let style = GetWindowLongPtrW(window, GWL_EXSTYLE) as u32;
                        assert_eq!(style & (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST),
                            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST);
                        assert_ne!(GetForegroundWindow(), window);
                        assert_eq!(SendMessageW(window, WM_MOUSEACTIVATE, 0, 0), MA_NOACTIVATE as isize);
                    }
                    return window as usize;
                }
                assert!(start.elapsed() < Duration::from_secs(2));
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let start = Instant::now(); run("Owned notification fixture").unwrap();
        let elapsed = start.elapsed();
        let window = observer.join().unwrap();
        assert!(elapsed.as_millis() >= 2990 && elapsed.as_secs_f32() < 4.5, "{elapsed:?}");
        assert_eq!(unsafe { IsWindow(window as HWND) }, 0);
        println!("owned notice dismissed after {} ms; no activation, HWND destroyed", elapsed.as_millis());
    }
}
