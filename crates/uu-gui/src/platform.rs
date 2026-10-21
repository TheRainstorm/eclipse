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
    /// Aggregate routing metadata only: never record typed keys or window titles.
    pub fn keyboard_diagnostics(&self)->Option<serde_json::Value> {
        #[cfg(windows)] {Some(unsafe{windows::keyboard_diagnostics(self.window,self.hook.as_ref())})}
        #[cfg(not(windows))] {None}
    }
    pub fn show(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.show_pending) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.focus_window();
        }
    }
    pub fn focus_window(&self) {
        #[cfg(windows)]
        if !self.window.is_null() {
            unsafe {windows::show_and_focus(self.window);}
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
    pub unsafe fn show_and_focus(window: HWND) {
        if window.is_null() {return;}
        ShowWindow(window,SW_SHOW);
        SetForegroundWindow(window);
        SetFocus(window);
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
    static OLD_PROC: AtomicIsize=AtomicIsize::new(0);
    static WINDOW_KEYS: std::sync::Mutex<crate::keyboard_capture::Keys>=std::sync::Mutex::new(crate::keyboard_capture::Keys::new());
    static HOOK_CALLBACKS: AtomicU64=AtomicU64::new(0);
    static HOOK_CAPTURED: AtomicU64=AtomicU64::new(0);
    static HOOK_BACKGROUND: AtomicU64=AtomicU64::new(0);
    static HOOK_INACTIVE: AtomicU64=AtomicU64::new(0);
    static HOOK_PREHELD: AtomicU64=AtomicU64::new(0);
    static HOOK_WIN_SEEN: AtomicU64=AtomicU64::new(0);
    static HOOK_WIN_CAPTURED: AtomicU64=AtomicU64::new(0);
    static WINDOW_WIN_SEEN: AtomicU64=AtomicU64::new(0);
    static WINDOW_ALT_SEEN: AtomicU64=AtomicU64::new(0);
    static HOOK_WIN_UP: AtomicU64=AtomicU64::new(0);
    static HOOK_WIN_INJECTED_UP: AtomicU64=AtomicU64::new(0);
    static HOOK_ALT_UP: AtomicU64=AtomicU64::new(0);
    static HOOK_ALT_INJECTED_UP: AtomicU64=AtomicU64::new(0);
    static HOOK_WIN_DOWN: AtomicU64=AtomicU64::new(0);
    static HOOK_ALT_DOWN: AtomicU64=AtomicU64::new(0);
    static HOOK_WIN_FORWARDED_PREHELD: AtomicU64=AtomicU64::new(0);
    static HOOK_ALT_FORWARDED_PREHELD: AtomicU64=AtomicU64::new(0);
    static HOOK_WIN_FORWARDED_INACTIVE: AtomicU64=AtomicU64::new(0);
    static HOOK_ALT_FORWARDED_INACTIVE: AtomicU64=AtomicU64::new(0);
    static HOOK_WIN_FORWARDED_BACKGROUND: AtomicU64=AtomicU64::new(0);
    static HOOK_ALT_FORWARDED_BACKGROUND: AtomicU64=AtomicU64::new(0);
    static LAST_FORWARDED_MODIFIER_VK: AtomicU64=AtomicU64::new(0);
    static LAST_FORWARDED_MODIFIER_FLAGS: AtomicU64=AtomicU64::new(0);
    static LAST_FORWARDED_MODIFIER_CAPTURE: AtomicU64=AtomicU64::new(0);
    static LAST_FORWARDED_MODIFIER_OWNED: AtomicU64=AtomicU64::new(0);
    static LAST_FORWARDED_MODIFIER_ACTIVE: AtomicU64=AtomicU64::new(0);
    static LAST_FORWARDED_MODIFIER_PREHELD: AtomicU64=AtomicU64::new(0);
    static LAST_FORWARDED_MODIFIER_AT_MS: AtomicU64=AtomicU64::new(0);
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
            if matches!(vk,0x5b|0x5c){WINDOW_WIN_SEEN.fetch_add(1,Relaxed);}
            if matches!(vk,0x12|0xa4|0xa5){WINDOW_ALT_SEEN.fetch_add(1,Relaxed);}
            // The hook thread can be ahead of this queued window message.
            // GetKeyState describes this message, not the latest physical input.
            let modifiers=crate::keyboard_capture::Modifiers::from_key_state(|vk|GetKeyState(vk as i32)<0);
            let ctrl_alt=modifiers.shortcut_chord();
            if let Ok(mut keys)=WINDOW_KEYS.try_lock() {
                let foreground=GetForegroundWindow()==window;
                // Window messages cannot suppress shell processing. When system
                // capture is active they join the same remote ownership path;
                // RemoteKeys deduplicates any event already seen by the hook.
                let local_system=crate::keyboard_capture::local_system_key(vk,modifiers.win());
                let system_capture=CAPTURE.load(std::sync::atomic::Ordering::Acquire);
                let active=window_routes_remote(
                    WINDOW_INPUT.load(std::sync::atomic::Ordering::Acquire),foreground,local_system,system_capture);
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
    fn window_routes_remote(window_input:bool,foreground:bool,local_system:bool,system_capture:bool)->bool {
        window_input && foreground && (!local_system || system_capture)
    }
    fn capture_owns(foreground_matches:bool,system_key:bool,injected:bool,capture:bool)->bool {
        foreground_matches || (capture && (system_key || injected))
    }
    fn preheld_for_capture(flags:u32,async_down:bool)->bool {
        // Injected input may already be reflected by GetAsyncKeyState before
        // the low-level callback runs. It is outer-client input, not a local
        // physical key that belonged to the desktop before capture.
        flags&(LLKHF_INJECTED|LLKHF_LOWER_IL_INJECTED)==0 && async_down
    }
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
    unsafe fn desktop_name(thread:u32)->Option<Vec<u16>> {
        use windows_sys::Win32::System::StationsAndDesktops::*;
        let desktop=GetThreadDesktop(thread);
        if desktop.is_null(){return None;}
        let mut name=[0u16;512];let mut needed=0;
        if GetUserObjectInformationW(desktop,UOI_NAME,name.as_mut_ptr().cast(),std::mem::size_of_val(&name) as u32,&mut needed)==0 {return None;}
        Some(name[..name.iter().position(|&v|v==0)?].to_vec())
    }
    pub unsafe fn keyboard_diagnostics(window:HWND,hook:Option<&KeyboardHook>)->serde_json::Value {
        let window_thread=GetWindowThreadProcessId(window,std::ptr::null_mut());
        let same_desktop=hook.and_then(|hook| {
            Some(desktop_name(window_thread)?==desktop_name(hook.id)?)
        });
        serde_json::json!({
            "window_valid":IsWindow(window)!=0,
            "foreground_matches":!window.is_null()&&GetForegroundWindow()==window,
            "hook_thread_alive":hook.is_some_and(KeyboardHook::alive),
            "same_desktop":same_desktop,
            "capture_requested":CAPTURE.load(Relaxed),
            "callbacks":HOOK_CALLBACKS.load(Relaxed),"captured":HOOK_CAPTURED.load(Relaxed),
            "forwarded_background":HOOK_BACKGROUND.load(Relaxed),
            "forwarded_inactive":HOOK_INACTIVE.load(Relaxed),"forwarded_preheld":HOOK_PREHELD.load(Relaxed),
            "win_seen":HOOK_WIN_SEEN.load(Relaxed),"win_captured":HOOK_WIN_CAPTURED.load(Relaxed),
            "window_win_seen":WINDOW_WIN_SEEN.load(Relaxed),
            "window_alt_seen":WINDOW_ALT_SEEN.load(Relaxed),
            "win_up":HOOK_WIN_UP.load(Relaxed),"win_injected_up":HOOK_WIN_INJECTED_UP.load(Relaxed),
            "alt_up":HOOK_ALT_UP.load(Relaxed),"alt_injected_up":HOOK_ALT_INJECTED_UP.load(Relaxed),
            "win_down":HOOK_WIN_DOWN.load(Relaxed),"alt_down":HOOK_ALT_DOWN.load(Relaxed),
            "win_forwarded_preheld":HOOK_WIN_FORWARDED_PREHELD.load(Relaxed),
            "alt_forwarded_preheld":HOOK_ALT_FORWARDED_PREHELD.load(Relaxed),
            "win_forwarded_inactive":HOOK_WIN_FORWARDED_INACTIVE.load(Relaxed),
            "alt_forwarded_inactive":HOOK_ALT_FORWARDED_INACTIVE.load(Relaxed),
            "win_forwarded_background":HOOK_WIN_FORWARDED_BACKGROUND.load(Relaxed),
            "alt_forwarded_background":HOOK_ALT_FORWARDED_BACKGROUND.load(Relaxed),
            "last_forwarded_modifier":{
                "vk":LAST_FORWARDED_MODIFIER_VK.load(Relaxed),
                "flags":LAST_FORWARDED_MODIFIER_FLAGS.load(Relaxed),
                "capture":LAST_FORWARDED_MODIFIER_CAPTURE.load(Relaxed)!=0,
                "owned":LAST_FORWARDED_MODIFIER_OWNED.load(Relaxed)!=0,
                "active":LAST_FORWARDED_MODIFIER_ACTIVE.load(Relaxed)!=0,
                "already_down":LAST_FORWARDED_MODIFIER_PREHELD.load(Relaxed)!=0,
                "at_ms":LAST_FORWARDED_MODIFIER_AT_MS.load(Relaxed)
            }
        })
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
                HOOK_CALLBACKS.fetch_add(1,Relaxed);
                let win=matches!(vk,0x5b|0x5c);
                let alt=matches!(vk,0x12|0xa4|0xa5);
                if win{HOOK_WIN_SEEN.fetch_add(1,Relaxed);}
                if down&&win{HOOK_WIN_DOWN.fetch_add(1,Relaxed);}
                if down&&alt{HOOK_ALT_DOWN.fetch_add(1,Relaxed);}
                if up&&win {
                    HOOK_WIN_UP.fetch_add(1,Relaxed);
                    if key.flags&LLKHF_INJECTED!=0 {HOOK_WIN_INJECTED_UP.fetch_add(1,Relaxed);}
                }
                if up&&matches!(vk,0x12|0xa4|0xa5) {
                    HOOK_ALT_UP.fetch_add(1,Relaxed);
                    if key.flags&LLKHF_INJECTED!=0 {HOOK_ALT_INJECTED_UP.fetch_add(1,Relaxed);}
                }
                let capture=CAPTURE.load(std::sync::atomic::Ordering::Acquire);
                let injected=key.flags&(LLKHF_INJECTED|LLKHF_LOWER_IL_INJECTED)!=0;
                let owned=!foreground.is_null()&&capture_owns(GetForegroundWindow()==foreground,win||alt,injected,capture);
                let active=capture&&owned;
                let shortcuts=LOCAL_SHORTCUTS.load(std::sync::atomic::Ordering::Acquire)&&owned;
                let already_down=preheld_for_capture(key.flags,GetAsyncKeyState(key.vkCode as i32)<0);
                let decision=HOOK_STATE.with(|state| {
                    let mut state=state.borrow_mut();
                    let (modifiers,keys)=&mut *state;
                    modifiers.observe(vk,down);
                    keys.event_with_shortcuts(vk,down,active,shortcuts,modifiers.shortcut_chord(),already_down)
                });
                if (win||alt) && matches!(decision,crate::keyboard_capture::Decision::Forward) {
                    LAST_FORWARDED_MODIFIER_VK.store(vk as u64,Relaxed);
                    LAST_FORWARDED_MODIFIER_FLAGS.store(key.flags as u64,Relaxed);
                    LAST_FORWARDED_MODIFIER_CAPTURE.store(capture as u64,Relaxed);
                    LAST_FORWARDED_MODIFIER_OWNED.store(owned as u64,Relaxed);
                    LAST_FORWARDED_MODIFIER_ACTIVE.store(active as u64,Relaxed);
                    LAST_FORWARDED_MODIFIER_PREHELD.store(already_down as u64,Relaxed);
                    LAST_FORWARDED_MODIFIER_AT_MS.store(uu_core::control_wire::now_ms(),Relaxed);
                }
                match decision {
                    crate::keyboard_capture::Decision::Forward=>{
                        if !owned{
                            HOOK_BACKGROUND.fetch_add(1,Relaxed);
                            if win{HOOK_WIN_FORWARDED_BACKGROUND.fetch_add(1,Relaxed);}
                            if alt{HOOK_ALT_FORWARDED_BACKGROUND.fetch_add(1,Relaxed);}
                        }else if !active{
                            HOOK_INACTIVE.fetch_add(1,Relaxed);
                            if win{HOOK_WIN_FORWARDED_INACTIVE.fetch_add(1,Relaxed);}
                            if alt{HOOK_ALT_FORWARDED_INACTIVE.fetch_add(1,Relaxed);}
                        }else if already_down{
                            HOOK_PREHELD.fetch_add(1,Relaxed);
                            if win{HOOK_WIN_FORWARDED_PREHELD.fetch_add(1,Relaxed);}
                            if alt{HOOK_ALT_FORWARDED_PREHELD.fetch_add(1,Relaxed);}
                        }
                    },
                    crate::keyboard_capture::Decision::Consume=>return 1,
                    event=>{
                        HOOK_CAPTURED.fetch_add(1,Relaxed);
                        if win{HOOK_WIN_CAPTURED.fetch_add(1,Relaxed);}
                        enqueue(event,foreground);return 1;
                    }
                }
            }
        }
        CallNextHookEx(std::ptr::null_mut(), code, message, data)
    }

    #[cfg(test)]
    #[test]
    fn injected_input_is_not_treated_as_preheld() {
        assert!(!preheld_for_capture(LLKHF_INJECTED,true));
        assert!(!preheld_for_capture(LLKHF_LOWER_IL_INJECTED,true));
        assert!(preheld_for_capture(0,true));
        assert!(!preheld_for_capture(0,false));
    }

    #[cfg(test)]
    #[test]
    fn window_layer_obeys_system_capture_for_local_system_keys() {
        assert!(window_routes_remote(true,true,false,false));
        assert!(!window_routes_remote(true,true,true,false));
        assert!(window_routes_remote(true,true,true,true));
        assert!(!window_routes_remote(false,true,true,true));
    }

    #[cfg(test)]
    #[test]
    fn active_capture_owns_system_and_injected_input_without_foreground_match() {
        assert!(capture_owns(true,false,false,false));
        assert!(capture_owns(false,true,false,true));
        assert!(capture_owns(false,false,true,true));
        assert!(!capture_owns(false,true,false,false));
        assert!(!capture_owns(false,false,true,false));
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
        let snapshot=unsafe{keyboard_diagnostics(window.0,Some(&hook))};
        assert_eq!(snapshot["window_valid"],true);
        assert_eq!(snapshot["hook_thread_alive"],true);
        assert_eq!(snapshot["same_desktop"],true);
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
