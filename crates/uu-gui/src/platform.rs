//! Windows startup placement and local monitor controls, in physical pixels.
use eframe::egui;
#[cfg(target_os = "linux")]
#[path = "wayland_capture.rs"]
mod wayland_capture;

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

#[cfg(any(windows, test))]
fn parse_origin(value: &str) -> Option<(i32, i32)> {
    let (x, y) = value.split_once(',')?;
    Some((x.parse().ok()?, y.parse().ok()?))
}

pub struct Platform {
    #[cfg(target_os = "linux")]
    wayland: std::cell::RefCell<Option<wayland_capture::Capture>>,
    #[cfg(windows)]
    window: windows_sys::Win32::Foundation::HWND,
    #[cfg(windows)]
    hook: windows_sys::Win32::UI::WindowsAndMessaging::HHOOK,
    #[cfg(windows)]
    old_proc: isize,
    #[cfg(windows)]
    restore_monitor: Option<(i32, i32)>,
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
            let _=windows::CAPTURE_CONTEXT.set(_cc.egui_ctx.clone());
            let old_proc=unsafe{windows::install_window_keyboard(window)};
            let hook = unsafe { windows::install_monitor_keys(window) };
            Self {
                window,
                hook,
                old_proc,
                restore_monitor: None,
                show_pending: true,
            }
        }
        #[cfg(not(windows))]
        {
            Self {
                #[cfg(target_os = "linux")]
                wayland: std::cell::RefCell::new(wayland_capture::Capture::new(_cc)),
                show_pending: false,
            }
        }
    }
    pub fn cursor_size(&self,ctx:&egui::Context)->egui::Vec2 {
        #[cfg(windows)] {
            use windows_sys::Win32::UI::{HiDpi::{GetDpiForWindow,GetSystemMetricsForDpi},WindowsAndMessaging::{SM_CXCURSOR,SM_CYCURSOR}};
            let dpi=unsafe{GetDpiForWindow(self.window)};
            if dpi!=0 {return egui::vec2(unsafe{GetSystemMetricsForDpi(SM_CXCURSOR,dpi)} as f32,unsafe{GetSystemMetricsForDpi(SM_CYCURSOR,dpi)} as f32)/ctx.pixels_per_point();}
        }
        let _=ctx;egui::Vec2::splat(32.0)
    }
    pub fn window_id(&self)->isize {
        #[cfg(windows)] {self.window as isize}
        #[cfg(not(windows))] {0}
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
    pub fn move_monitors(&mut self, _fullscreen: bool) -> bool {
        #[cfg(windows)]
        {
            unsafe {
                let mut moved = windows::move_pending(self.window, _fullscreen);
                if moved && _fullscreen {
                    self.restore_monitor = windows::monitor_origin(self.window);
                }
                if !_fullscreen {
                    if let Some(origin) = self.restore_monitor.take() {
                        if !moved {
                            moved = windows::move_to_origin(self.window, origin);
                        }
                    }
                }
                moved
            }
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
    pub fn window_keyboard(&self,active:bool,shortcuts:bool)->bool {
        #[cfg(windows)] {
            windows::LOCAL_SHORTCUTS.store(shortcuts,std::sync::atomic::Ordering::Release);
            let active=active&&self.old_proc!=0;
            windows::WINDOW_INPUT.store(active,std::sync::atomic::Ordering::Release);active
        }
        #[cfg(not(windows))] {let _=(active,shortcuts);false}
    }
    pub fn capture_keyboard(&self, active: bool) -> bool {
        #[cfg(windows)] {
            let active=active && !self.hook.is_null();
            windows::CAPTURE.store(active,std::sync::atomic::Ordering::Release);
            active
        }
        #[cfg(target_os = "linux")]
        if let Some(capture) = self.wayland.borrow_mut().as_mut() { capture.update(active); }
        // Wayland still delivers ordinary egui events; the inhibitor only
        // prevents the compositor from intercepting its system shortcuts.
        #[cfg(not(windows))] {let _=active;false}
    }
    pub fn captured_keys(&self) -> Vec<crate::keyboard_capture::Decision> {
        #[cfg(windows)] {
            let mut queue=windows::CAPTURE_QUEUE.lock().unwrap();
            queue.drain(..).collect()
        }
        #[cfg(not(windows))] {Vec::new()}
    }
    pub fn capture_overflowed(&self)->bool {
        #[cfg(windows)] {windows::CAPTURE_OVERFLOW.swap(false,std::sync::atomic::Ordering::AcqRel)}
        #[cfg(not(windows))] {false}
    }
    pub fn show(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.show_pending) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }
}

#[cfg(windows)]
impl Drop for Platform {
    fn drop(&mut self) {
        unsafe {
            windows::remove_monitor_keys(self.hook);
            windows::remove_window_keyboard(self.window,self.old_proc);
        }
    }
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
enum Direction {
    Left = 1,
    Right = 2,
    Up = 3,
    Down = 4,
}

#[cfg(any(windows, test))]
#[derive(Debug, PartialEq)]
enum ArrowEvent {
    Forward,
    Consume,
    Move(Direction),
}
#[cfg(any(windows, test))]
#[derive(Default)]
struct MonitorKeys {
    held: std::sync::atomic::AtomicU8,
}
#[cfg(any(windows, test))]
impl MonitorKeys {
    fn event(&self, vk: u32, down: bool, owned_chord: bool, already_down: bool) -> ArrowEvent {
        use std::sync::atomic::Ordering::Relaxed;
        if !(0x25..=0x28).contains(&vk) {
            return ArrowEvent::Forward;
        }
        let bit = 1 << (vk - 0x25);
        if !down {
            return if self.held.fetch_and(!bit, Relaxed) & bit != 0 {
                ArrowEvent::Consume
            } else {
                ArrowEvent::Forward
            };
        }
        if self.held.load(Relaxed) & bit != 0 {
            return ArrowEvent::Consume;
        }
        if !owned_chord || already_down {
            return ArrowEvent::Forward;
        }
        self.held.fetch_or(bit, Relaxed);
        ArrowEvent::Move(match vk {
            0x25 => Direction::Left,
            0x26 => Direction::Up,
            0x27 => Direction::Right,
            _ => Direction::Down,
        })
    }
}

/// Cycle in desktop coordinate order, including vertically arranged screens.
#[cfg(any(windows, test))]
fn next_monitor(rects: &[egui::Rect], current: usize, direction: Direction) -> Option<usize> {
    if rects.len() < 2 || current >= rects.len() {
        return None;
    }
    let vertical = matches!(direction, Direction::Up | Direction::Down);
    let mut order: Vec<_> = (0..rects.len()).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (rects[a].center(), rects[b].center());
        if vertical {
            a.y.total_cmp(&b.y).then(a.x.total_cmp(&b.x))
        } else {
            a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y))
        }
    });
    let index = order.iter().position(|&i| i == current)?;
    let step = if matches!(direction, Direction::Left | Direction::Up) {
        order.len() - 1
    } else {
        1
    };
    Some(order[(index + step) % order.len()])
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
    use std::sync::atomic::{AtomicIsize, AtomicU64, Ordering::Relaxed};
    use windows_sys::Win32::{
        System::LibraryLoader::GetModuleHandleW, UI::Input::KeyboardAndMouse::*,
    };
    pub(super) static WINDOW_INPUT: std::sync::atomic::AtomicBool=std::sync::atomic::AtomicBool::new(false);
    pub(super) static LOCAL_SHORTCUTS: std::sync::atomic::AtomicBool=std::sync::atomic::AtomicBool::new(false);
    static MODIFIERS: std::sync::Mutex<crate::keyboard_capture::Modifiers>=std::sync::Mutex::new(crate::keyboard_capture::Modifiers::new());
    static OLD_PROC: AtomicIsize=AtomicIsize::new(0);
    static WINDOW_KEYS: std::sync::Mutex<crate::keyboard_capture::Keys>=std::sync::Mutex::new(crate::keyboard_capture::Keys::new());
    pub unsafe fn install_window_keyboard(window:HWND)->isize {
        if window.is_null(){return 0;}
        let old=SetWindowLongPtrW(window,GWLP_WNDPROC,window_keyboard as *const () as isize);
        OLD_PROC.store(old,Relaxed);old
    }
    pub unsafe fn remove_window_keyboard(window:HWND,old:isize) {
        WINDOW_INPUT.store(false,Relaxed);
        LOCAL_SHORTCUTS.store(false,Relaxed);
        if old!=0 && IsWindow(window)!=0 && GetWindowLongPtrW(window,GWLP_WNDPROC)==window_keyboard as *const () as isize {
            SetWindowLongPtrW(window,GWLP_WNDPROC,old);
        }
    }
    fn enqueue(event:crate::keyboard_capture::Decision,window:HWND) {
        if let Ok(mut queue)=CAPTURE_QUEUE.try_lock() {
            if queue.len()<512 {queue.push_back(event);}else{CAPTURE_OVERFLOW.store(true,Relaxed);}
        }else{CAPTURE_OVERFLOW.store(true,Relaxed);}
        if let Some(context)=CAPTURE_CONTEXT.get(){context.request_repaint();}
        unsafe{PostMessageW(window,WM_NULL,0,0);}
    }
    unsafe extern "system" fn window_keyboard(window:HWND,message:u32,wparam:usize,lparam:isize)->isize {
        let down=message==WM_KEYDOWN||message==WM_SYSKEYDOWN;
        let up=message==WM_KEYUP||message==WM_SYSKEYUP;
        if down||up {
            let vk=crate::keyboard_capture::sided_vk(wparam as u32,lparam as usize);
            let ctrl_alt=if let Ok(mut modifiers)=MODIFIERS.try_lock() {
                // Also retain a window-message fallback if Windows removes a
                // timed-out hook. Updating an observed transition is idempotent.
                modifiers.observe(vk,down);
                modifiers.shortcut_chord()
            }else{false};
            if let Ok(mut keys)=WINDOW_KEYS.try_lock() {
                let foreground=GetForegroundWindow()==window;
                let active=WINDOW_INPUT.load(std::sync::atomic::Ordering::Acquire)&&foreground;
                let shortcuts=LOCAL_SHORTCUTS.load(std::sync::atomic::Ordering::Acquire)&&foreground;
                match keys.event_with_shortcuts(vk,down,active,shortcuts,ctrl_alt,lparam as usize&(1<<30)!=0) {
                    crate::keyboard_capture::Decision::Forward=>{},
                    crate::keyboard_capture::Decision::Consume=>return 0,
                    event=>{enqueue(event,window);return 0;},
                }
            }
        }
        let old=OLD_PROC.load(Relaxed);
        if old==0 {DefWindowProcW(window,message,wparam,lparam)}
        else{CallWindowProcW(std::mem::transmute::<isize,WNDPROC>(old),window,message,wparam,lparam)}
    }
    pub(super) static CAPTURE_CONTEXT: std::sync::OnceLock<eframe::egui::Context>=std::sync::OnceLock::new();
    pub(super) static CAPTURE: std::sync::atomic::AtomicBool=std::sync::atomic::AtomicBool::new(false);
    pub(super) static CAPTURE_OVERFLOW: std::sync::atomic::AtomicBool=std::sync::atomic::AtomicBool::new(false);
    static CAPTURE_KEYS: std::sync::Mutex<crate::keyboard_capture::Keys>=std::sync::Mutex::new(crate::keyboard_capture::Keys::new());
    pub(super) static CAPTURE_QUEUE: std::sync::Mutex<std::collections::VecDeque<crate::keyboard_capture::Decision>>=std::sync::Mutex::new(std::collections::VecDeque::new());
    static OWNER: AtomicIsize = AtomicIsize::new(0);
    static KEYS: super::MonitorKeys = super::MonitorKeys {
        held: std::sync::atomic::AtomicU8::new(0),
    };
    static PENDING: AtomicU64 = AtomicU64::new(0);

    pub unsafe fn install_monitor_keys(window: HWND) -> HHOOK {
        if window.is_null() {
            return std::ptr::null_mut();
        }
        OWNER.store(window as isize, Relaxed);
        if let Ok(mut modifiers)=MODIFIERS.lock() {
            for vk in [0xa0,0xa1,0xa2,0xa3,0xa4,0xa5,0x5b,0x5c] {
                modifiers.observe(vk,GetAsyncKeyState(vk as i32)<0);
            }
        }
        let hook = SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(monitor_key),
            GetModuleHandleW(std::ptr::null()),
            0,
        );
        if hook.is_null() {
            eprintln!("monitor shortcut hook: {}", std::io::Error::last_os_error());
        }
        hook
    }
    pub unsafe fn remove_monitor_keys(hook: HHOOK) {
        CAPTURE.store(false,Relaxed);
        OWNER.store(0, Relaxed);
        if !hook.is_null() {
            UnhookWindowsHookEx(hook);
        }
    }
    unsafe fn mask_start_menu() {
        // A consumed Win chord must not turn into a standalone Win press.
        // VK_FF means "no mapping"; no Ctrl/Alt/Shift transitions are injected.
        // Reference: AutoHotkeyDocs/v1/docs/lib/_MenuMaskKey.htm (technique only).
        let mut down: INPUT = std::mem::zeroed();
        down.r#type = INPUT_KEYBOARD;
        down.Anonymous.ki = KEYBDINPUT {
            wVk: 0xFF,
            wScan: 0,
            dwFlags: 0,
            time: 0,
            dwExtraInfo: 0x5555574D,
        };
        let mut up = down;
        up.Anonymous.ki.dwFlags = KEYEVENTF_KEYUP;
        let inputs = [down, up];
        SendInput(2, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32);
    }
    unsafe extern "system" fn monitor_key(code: i32, message: usize, data: isize) -> isize {
        if code >= 0 {
            let key = &*(data as *const KBDLLHOOKSTRUCT);
            let down = message == WM_KEYDOWN as usize || message == WM_SYSKEYDOWN as usize;
            let up = message == WM_KEYUP as usize || message == WM_SYSKEYUP as usize;
            let foreground=OWNER.load(Relaxed) as HWND;
            let vk=crate::keyboard_capture::sided_vk(key.vkCode,
                ((key.scanCode as usize)<<16)|(((key.flags&LLKHF_EXTENDED) as usize)<<24));
            let (ctrl_alt,win_down,shift_down)=if let Ok(mut modifiers)=MODIFIERS.try_lock() {
                if down||up {modifiers.observe(vk,down);}
                (modifiers.shortcut_chord(),modifiers.win(),modifiers.shift())
            }else{(false,false,false)};
            if (0x25..=0x28).contains(&key.vkCode) && (down || up) {
                let owner = OWNER.load(Relaxed) as HWND;
                let chord = !owner.is_null()
                    && GetForegroundWindow() == owner
                    && win_down && shift_down;
                match KEYS.event(
                    key.vkCode,
                    down,
                    chord,
                    GetAsyncKeyState(key.vkCode as i32) < 0,
                ) {
                    super::ArrowEvent::Consume => return 1,
                    super::ArrowEvent::Move(direction) => {
                        // O(1) hook: defer all monitor queries and moves to the UI.
                        PENDING
                            .fetch_update(Relaxed, Relaxed, |queue| {
                                Some((queue << 4) | direction as u64)
                            })
                            .ok();
                        mask_start_menu();
                        PostMessageW(owner, WM_NULL, 0, 0);
                        return 1;
                    }
                    super::ArrowEvent::Forward => {}
                }
            }
            // Injected input may come from remote desktops or accessibility
            // tools. Only our own no-map Start-menu mask is excluded.
            if (down || up) && !crate::keyboard_capture::our_mask(key.vkCode,key.dwExtraInfo) {
                if let Ok(mut keys)=CAPTURE_KEYS.try_lock() {
                    let owned=!foreground.is_null() && GetForegroundWindow()==foreground;
                    let active=CAPTURE.load(std::sync::atomic::Ordering::Acquire)
                        && owned;
                    let shortcuts=LOCAL_SHORTCUTS.load(std::sync::atomic::Ordering::Acquire)&&owned;
                    let decision=keys.event_with_shortcuts(vk,down,active,shortcuts,ctrl_alt,GetAsyncKeyState(key.vkCode as i32)<0);
                    match decision {
                        crate::keyboard_capture::Decision::Forward=>{},
                        crate::keyboard_capture::Decision::Consume=>return 1,
                        event=>{
                            enqueue(event,foreground);
                            return 1;
                        }
                    }
                }
            }
        }
        CallNextHookEx(std::ptr::null_mut(), code, message, data)
    }
    unsafe extern "system" fn collect_monitor(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        data: isize,
    ) -> i32 {
        if let Some(info) = monitor_info(monitor) {
            (*(data as *mut Vec<(HMONITOR, MONITORINFO)>)).push((monitor, info));
        }
        1
    }
    pub unsafe fn move_pending(window: HWND, fullscreen: bool) -> bool {
        let mut queue = PENDING.swap(0, Relaxed);
        let mut directions = Vec::new();
        while queue != 0 {
            directions.push(match queue & 15 {
                1 => super::Direction::Left,
                2 => super::Direction::Right,
                3 => super::Direction::Up,
                _ => super::Direction::Down,
            });
            queue >>= 4;
        }
        let mut moved = false;
        for direction in directions.into_iter().rev() {
            moved |= move_monitor(window, fullscreen, direction);
        }
        moved
    }
    unsafe fn move_monitor(window: HWND, fullscreen: bool, direction: super::Direction) -> bool {
        let mut monitors: Vec<(HMONITOR, MONITORINFO)> = Vec::new();
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(collect_monitor),
            &mut monitors as *mut _ as isize,
        );
        let current = MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST);
        let Some(index) = monitors.iter().position(|(handle, _)| *handle == current) else {
            return false;
        };
        let rects: Vec<_> = monitors
            .iter()
            .map(|(_, info)| {
                let r = info.rcMonitor;
                eframe::egui::Rect::from_min_max(
                    eframe::egui::pos2(r.left as f32, r.top as f32),
                    eframe::egui::pos2(r.right as f32, r.bottom as f32),
                )
            })
            .collect();
        let Some(target) = super::next_monitor(&rects, index, direction) else {
            return false;
        };
        relocate(window, &monitors[target].1, fullscreen)
    }
    pub unsafe fn move_to_origin(window: HWND, origin: (i32, i32)) -> bool {
        let monitor = MonitorFromPoint(
            POINT {
                x: origin.0,
                y: origin.1,
            },
            MONITOR_DEFAULTTONEAREST,
        );
        let Some(info) = monitor_info(monitor) else {
            return false;
        };
        relocate(window, &info, false)
    }
    unsafe fn relocate(window: HWND, info: &MONITORINFO, fullscreen: bool) -> bool {
        if fullscreen {
            let r = info.rcMonitor;
            return SetWindowPos(
                window,
                std::ptr::null_mut(),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            ) != 0;
        }
        let maximized = IsZoomed(window) != 0;
        if maximized {
            ShowWindow(window, SW_RESTORE);
        }
        let mut old: RECT = std::mem::zeroed();
        if GetWindowRect(window, &mut old) == 0 {
            return false;
        }
        let r = info.rcWork;
        let moved = SetWindowPos(
            window,
            std::ptr::null_mut(),
            (r.left + r.right - (old.right - old.left)) / 2,
            (r.top + r.bottom - (old.bottom - old.top)) / 2,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        ) != 0;
        if maximized {
            ShowWindow(window, SW_MAXIMIZE);
        }
        moved
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
    #[test]
    fn monitor_switch_wraps_on_horizontal_and_negative_vertical_layouts() {
        let screens = [
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(3072.0, 1920.0)),
            egui::Rect::from_min_size(egui::pos2(580.0, -1080.0), egui::vec2(1920.0, 1080.0)),
        ];
        for direction in [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ] {
            assert_eq!(next_monitor(&screens, 0, direction), Some(1));
            assert_eq!(next_monitor(&screens, 1, direction), Some(0));
        }
        assert_eq!(next_monitor(&screens[..1], 0, Direction::Right), None);
    }
    #[test]
    fn monitor_key_owns_repeat_and_release_without_stealing_existing_remote_keys() {
        let keys = MonitorKeys::default();
        assert_eq!(keys.event(0x27, true, false, false), ArrowEvent::Forward);
        assert_eq!(keys.event(0x27, true, true, true), ArrowEvent::Forward);
        assert_eq!(keys.event(0x27, false, false, true), ArrowEvent::Forward);
        assert_eq!(
            keys.event(0x27, true, true, false),
            ArrowEvent::Move(Direction::Right)
        );
        assert_eq!(keys.event(0x27, true, false, true), ArrowEvent::Consume);
        assert_eq!(keys.event(0x27, false, false, true), ArrowEvent::Consume);
        assert_eq!(keys.event(0x53, true, true, false), ArrowEvent::Forward); // Win+Shift+S stays with Windows.
    }
}
