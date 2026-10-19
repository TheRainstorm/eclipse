//! Windows startup placement and local monitor controls, in physical pixels.
use eframe::egui;

pub fn startup_origin(streaming: bool) -> Option<(i32, i32)> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::POINT,
            UI::{
                HiDpi::{
                    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                },
                WindowsAndMessaging::GetCursorPos,
            },
        };
        // Capture the launch position before creating any window, using the same
        // physical coordinate space as the future per-monitor-aware HWND.
        unsafe {
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
        if streaming {
            if let Some(origin) = std::env::var("UU_WINDOW_ORIGIN")
                .ok()
                .and_then(|v| parse_origin(&v))
            {
                return Some(origin);
            }
        }
        let mut point = POINT { x: 0, y: 0 };
        if unsafe { GetCursorPos(&mut point) } != 0 {
            return Some((point.x, point.y));
        }
    }
    let _ = streaming;
    None
}

fn parse_origin(value: &str) -> Option<(i32, i32)> {
    let (x, y) = value.split_once(',')?;
    Some((x.parse().ok()?, y.parse().ok()?))
}

pub struct Platform {
    #[cfg(windows)]
    window: windows_sys::Win32::Foundation::HWND,
    show_pending: bool,
}
impl Platform {
    pub fn new(_cc: &eframe::CreationContext<'_>, _origin: Option<(i32, i32)>) -> Self {
        #[cfg(windows)]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let window = match _cc.window_handle().ok().map(|h| h.as_raw()) {
                Some(RawWindowHandle::Win32(handle)) => handle.hwnd.get() as _,
                _ => std::ptr::null_mut(),
            };
            if let Some(origin) = _origin {
                unsafe {
                    windows::place_startup(window, origin);
                }
            }
            Self {
                window,
                show_pending: true,
            }
        }
        #[cfg(not(windows))]
        {
            Self {
                show_pending: false,
            }
        }
    }
    pub fn origin(&self) -> Option<(i32, i32)> {
        #[cfg(windows)]
        {
            unsafe { windows::monitor_origin(self.window) }
        }
        #[cfg(not(windows))]
        {
            None
        }
    }
    pub fn show(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.show_pending) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }
}

#[cfg(windows)]
mod windows {
    use windows_sys::Win32::{
        Foundation::{HWND, POINT, RECT},
        Graphics::Gdi::*,
        UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
    };
    unsafe fn monitor_info(monitor: HMONITOR) -> Option<MONITORINFO> {
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        (GetMonitorInfoW(monitor, &mut info) != 0).then_some(info)
    }
    pub unsafe fn monitor_origin(window: HWND) -> Option<(i32, i32)> {
        let r = monitor_info(MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST))?.rcWork;
        Some(((r.left + r.right) / 2, (r.top + r.bottom) / 2))
    }
    pub unsafe fn place_startup(window: HWND, origin: (i32, i32)) {
        if window.is_null() {
            return;
        }
        let Some(info) = monitor_info(MonitorFromPoint(
            POINT {
                x: origin.0,
                y: origin.1,
            },
            MONITOR_DEFAULTTONEAREST,
        )) else {
            return;
        };
        let r = info.rcWork;
        let mut old: RECT = std::mem::zeroed();
        if GetWindowRect(window, &mut old) == 0 {
            return;
        }
        // First move the hidden native window, so GetDpiForWindow reports the
        // destination DPI. Never multiply the destination origin by primary DPI.
        SetWindowPos(
            window,
            std::ptr::null_mut(),
            (r.left + r.right - (old.right - old.left)) / 2,
            (r.top + r.bottom - (old.bottom - old.top)) / 2,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        let ppp = GetDpiForWindow(window).max(96) as f32 / 96.0;
        let margin = (8.0 * ppp) as i32;
        let width = (1100.0 * ppp) as i32;
        let height = (760.0 * ppp) as i32;
        let width = width.min(r.right - r.left - margin * 2);
        let height = height.min(r.bottom - r.top - margin * 2);
        SetWindowPos(
            window,
            std::ptr::null_mut(),
            (r.left + r.right - width) / 2,
            (r.top + r.bottom - height) / 2,
            width,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_origin_preserves_negative_physical_coordinates() {
        assert_eq!(parse_origin("1540,-564"), Some((1540, -564)));
        assert_eq!(parse_origin("-2000,360"), Some((-2000, 360)));
        assert_eq!(parse_origin("1,2,3"), None);
    }
}
