//! Native window placement and keyboard ownership.
use eframe::egui;
#[cfg(target_os = "linux")]
#[path = "wayland_capture.rs"]
mod wayland_capture;
#[cfg(all(windows,test))]
#[allow(dead_code)]
#[path = "../../../tools/native/windows_owned_station.rs"]
mod owned_station;

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
    hook: Option<windows::KeyboardHook>,
    #[cfg(windows)]
    old_proc: isize,
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
            let hook = unsafe { windows::install_keyboard_hook(window) };
            Self {
                window,
                hook,
                old_proc,
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
            let active=active && self.hook.as_ref().is_some_and(|hook| hook.alive());
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
            self.hook.take();
            windows::remove_window_keyboard(self.window,self.old_proc);
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
    use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};
    use windows_sys::Win32::{
        System::LibraryLoader::GetModuleHandleW, UI::Input::KeyboardAndMouse::*,
    };
    pub(super) static WINDOW_INPUT: std::sync::atomic::AtomicBool=std::sync::atomic::AtomicBool::new(false);
    pub(super) static LOCAL_SHORTCUTS: std::sync::atomic::AtomicBool=std::sync::atomic::AtomicBool::new(false);
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
        unsafe{PostMessageW(window,WM_NULL,0,0);}
    }
    unsafe extern "system" fn window_keyboard(window:HWND,message:u32,wparam:usize,lparam:isize)->isize {
        if message==WM_NULL {
            // Wake egui on the UI thread; the hook must never wait for its locks.
            if let Some(context)=CAPTURE_CONTEXT.get(){context.request_repaint();}
        }
        let down=message==WM_KEYDOWN||message==WM_SYSKEYDOWN;
        let up=message==WM_KEYUP||message==WM_SYSKEYUP;
        if down||up {
            let vk=crate::keyboard_capture::sided_vk(wparam as u32,lparam as usize);
            // The hook thread can be ahead of this queued window message.
            // GetKeyState describes this message, not the latest physical input.
            let modifiers=crate::keyboard_capture::Modifiers::from_key_state(|vk|GetKeyState(vk as i32)<0);
            let ctrl_alt=modifiers.shortcut_chord();
            if let Ok(mut keys)=WINDOW_KEYS.try_lock() {
                let foreground=GetForegroundWindow()==window;
                // A window message cannot suppress Windows shell processing. If the
                // hook didn't own a Win chord, keep the entire chord local.
                let local_system=crate::keyboard_capture::local_system_key(vk,modifiers.win());
                let active=WINDOW_INPUT.load(std::sync::atomic::Ordering::Acquire)&&foreground&&!local_system;
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
    std::thread_local! {
        static HOOK_STATE: std::cell::RefCell<(crate::keyboard_capture::Modifiers,crate::keyboard_capture::Keys)> =
            const { std::cell::RefCell::new((crate::keyboard_capture::Modifiers::new(),crate::keyboard_capture::Keys::new())) };
    }
    pub(super) static CAPTURE_QUEUE: std::sync::Mutex<std::collections::VecDeque<crate::keyboard_capture::Decision>>=std::sync::Mutex::new(std::collections::VecDeque::new());
    static OWNER: AtomicIsize = AtomicIsize::new(0);
    #[cfg(test)]
    static HOOK_HEARTBEAT: std::sync::atomic::AtomicBool=std::sync::atomic::AtomicBool::new(false);
    pub struct KeyboardHook {
        thread: Option<std::thread::JoinHandle<()>>,
        id: u32,
    }
    impl KeyboardHook {
        pub fn alive(&self)->bool {self.thread.as_ref().is_some_and(|thread| !thread.is_finished())}
    }
    impl Drop for KeyboardHook {
        fn drop(&mut self) {
            CAPTURE.store(false,Relaxed);
            OWNER.store(0,Relaxed);
            unsafe { PostThreadMessageW(self.id,WM_QUIT,0,0); }
            if let Some(thread)=self.thread.take(){let _=thread.join();}
        }
    }
    pub unsafe fn install_keyboard_hook(window: HWND) -> Option<KeyboardHook> {
        if window.is_null(){return None;}
        OWNER.store(window as isize,Relaxed);
        let (ready_tx,ready_rx)=std::sync::mpsc::sync_channel(1);
        // Low-level hooks run on their installing thread. A renderer stall on
        // that thread lets Windows silently remove the hook (LowLevelHooksTimeout).
        let thread=std::thread::Builder::new().name("keyboard-capture".into()).spawn(move || unsafe {
            let mut message: MSG=std::mem::zeroed();
            PeekMessageW(&mut message,std::ptr::null_mut(),0,0,PM_NOREMOVE);
            HOOK_STATE.with(|state| {
                let mut state=state.borrow_mut();
                state.0=crate::keyboard_capture::Modifiers::from_key_state(|vk|GetAsyncKeyState(vk as i32)<0);
            });
            let hook=SetWindowsHookExW(WH_KEYBOARD_LL,Some(monitor_key),GetModuleHandleW(std::ptr::null()),0);
            if hook.is_null(){
                eprintln!("keyboard capture hook: {}",std::io::Error::last_os_error());
                let _=ready_tx.send(None);return;
            }
            if ready_tx.send(Some(windows_sys::Win32::System::Threading::GetCurrentThreadId())).is_ok(){
                while GetMessageW(&mut message,std::ptr::null_mut(),0,0)>0 {
                    #[cfg(test)]
                    if message.message==WM_APP {
                        HOOK_HEARTBEAT.store(true,std::sync::atomic::Ordering::Release);
                        continue;
                    }
                    TranslateMessage(&message);DispatchMessageW(&message);
                }
            }
            UnhookWindowsHookEx(hook);
        }).ok()?;
        match ready_rx.recv().ok().flatten() {
            Some(id)=>Some(KeyboardHook{thread:Some(thread),id}),
            None=>{let _=thread.join();None}
        }
    }
    unsafe extern "system" fn monitor_key(code: i32, message: usize, data: isize) -> isize {
        if code >= 0 {
            let key = &*(data as *const KBDLLHOOKSTRUCT);
            let down = message == WM_KEYDOWN as usize || message == WM_SYSKEYDOWN as usize;
            let up = message == WM_KEYUP as usize || message == WM_SYSKEYUP as usize;
            let foreground=OWNER.load(Relaxed) as HWND;
            let vk=crate::keyboard_capture::sided_vk(key.vkCode,
                ((key.scanCode as usize)<<16)|(((key.flags&LLKHF_EXTENDED) as usize)<<24));
            // Accept injected input from outer remote desktop clients as well.
            // Ownership applies uniformly to every key; no Win-chord exceptions.
            if down||up {
                let owned=!foreground.is_null()&&GetForegroundWindow()==foreground;
                let active=CAPTURE.load(std::sync::atomic::Ordering::Acquire)&&owned;
                let shortcuts=LOCAL_SHORTCUTS.load(std::sync::atomic::Ordering::Acquire)&&owned;
                let decision=HOOK_STATE.with(|state| {
                    let mut state=state.borrow_mut();
                    let (modifiers,keys)=&mut *state;
                    modifiers.observe(vk,down);
                    keys.event_with_shortcuts(vk,down,active,shortcuts,modifiers.shortcut_chord(),GetAsyncKeyState(key.vkCode as i32)<0)
                });
                match decision {
                    crate::keyboard_capture::Decision::Forward=>{},
                    crate::keyboard_capture::Decision::Consume=>return 1,
                    event=>{enqueue(event,foreground);return 1;}
                }
            }
        }
        CallNextHookEx(std::ptr::null_mut(), code, message, data)
    }

    #[cfg(test)]
    #[test]
    #[ignore = "run alone: owns a newly created noninteractive window station"]
    fn isolated_keyboard_hook_pumps_messages_while_ui_is_blocked() {
        use std::sync::atomic::Ordering;
        use std::time::Duration;
        let _station=super::owned_station::Station::create_with_keyboard_hook().unwrap();
        struct Window(HWND);
        impl Drop for Window {fn drop(&mut self){unsafe{DestroyWindow(self.0);}}}
        let class:Vec<u16>="STATIC\0".encode_utf16().collect();
        let window=Window(unsafe{CreateWindowExW(0,class.as_ptr(),std::ptr::null(),0,0,0,0,0,
            HWND_MESSAGE,std::ptr::null_mut(),std::ptr::null_mut(),std::ptr::null())});
        assert!(!window.0.is_null());
        HOOK_HEARTBEAT.store(false,Ordering::Release);
        // Drop/join the hook before the window and station.
        let hook=unsafe{install_keyboard_hook(window.0)}.expect("isolated hook installation");
        assert!(hook.alive());
        assert_ne!(unsafe{PostThreadMessageW(hook.id,WM_APP,0,0)},0);
        std::thread::sleep(Duration::from_millis(150));
        assert!(HOOK_HEARTBEAT.load(Ordering::Acquire),"hook depended on UI message pumping");
        drop(hook);
        assert_eq!(OWNER.load(Relaxed),0);
        assert!(!CAPTURE.load(Relaxed));
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
