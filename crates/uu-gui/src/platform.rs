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
    hook: Option<std::rc::Rc<windows::KeyboardHook>>,
    #[cfg(windows)]
    route: std::sync::Arc<windows::Route>,
    #[cfg(windows)]
    focus_events: std::cell::RefCell<std::collections::VecDeque<serde_json::Value>>,
    #[cfg(windows)]
    bindings:std::cell::RefCell<uu_core::hotkeys::Bindings>,
    show_pending: std::cell::Cell<bool>,
}
impl Platform {
    pub fn new(_cc: &eframe::CreationContext<'_>, _origin: Option<(i32, i32)>) -> Self {
        Self::for_window(_cc,&_cc.egui_ctx,_origin)
    }
    pub fn for_window(_handles:&(impl raw_window_handle::HasWindowHandle+raw_window_handle::HasDisplayHandle),_ctx:&egui::Context,_origin:Option<(i32,i32)>)->Self {
        #[cfg(windows)]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let window = match _handles.window_handle().ok().map(|h| h.as_raw()) {
                Some(RawWindowHandle::Win32(handle)) => handle.hwnd.get() as _,
                _ => std::ptr::null_mut(),
            };
            if let Some(origin) = _origin {
                unsafe {
                    windows::place_startup(window, origin);
                }
            }
            let route=unsafe{windows::install_window_keyboard(window,_ctx.clone())};
            let hook = unsafe { windows::shared_keyboard_hook(window) };
            Self {
                window,
                hook,
                route,
                focus_events: Default::default(),
                bindings:Default::default(),
                show_pending: std::cell::Cell::new(true),
            }
        }
        #[cfg(not(windows))]
        {
            Self {
                #[cfg(target_os = "linux")]
                wayland: std::cell::RefCell::new(wayland_capture::Capture::new(_handles)),
                show_pending: std::cell::Cell::new(false),
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
    /// Select the session that owns this window's input queue. The caller first
    /// releases the previous session's remote keys, pointer and gamepads.
    pub fn select_session(&self,session:u64) {
        #[cfg(windows)] self.route.select(session);
        #[cfg(not(windows))] let _=session;
    }
    pub fn window_keyboard(&self,active:bool,shortcuts:bool,nested_passthrough:bool,bindings:&uu_core::hotkeys::Bindings)->bool {
        #[cfg(windows)] {
            let active=active&&self.route.old_proc!=0;
            if *self.bindings.borrow()!=*bindings {
                self.route.configure_bindings(bindings);
                *self.bindings.borrow_mut()=bindings.clone();
            }
            self.route.set_policy(active,shortcuts,nested_passthrough);
            active
        }
        #[cfg(not(windows))] {let _=(active,shortcuts,nested_passthrough,bindings);false}
    }
    pub fn capture_keyboard(&self, active: bool) -> bool {
        #[cfg(windows)] {
            if let Some(hook)=&self.hook {hook.probe();}
            let active=active && self.hook.as_ref().is_some_and(|hook| hook.alive());
            self.route.set_capture(active);
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
            let mut queue=self.route.queue.lock().unwrap();
            queue.drain(..).collect()
        }
        #[cfg(not(windows))] {Vec::new()}
    }
    pub fn capture_overflowed(&self)->bool {
        #[cfg(windows)] {self.route.overflow.swap(false,std::sync::atomic::Ordering::AcqRel)}
        #[cfg(not(windows))] {false}
    }
    /// Aggregate routing metadata only: never record typed keys or window titles.
    pub fn keyboard_diagnostics(&self)->Option<serde_json::Value> {
        #[cfg(windows)] {Some(unsafe{windows::keyboard_diagnostics(self.window,self.hook.as_deref())})}
        #[cfg(not(windows))] {None}
    }
    pub fn show(&self, ctx: &egui::Context) {
        if self.show_pending.replace(false) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.focus_window();
        }
    }
    pub fn focus_events(&self) -> Vec<serde_json::Value> {
        #[cfg(windows)] { self.focus_events.borrow_mut().drain(..).collect() }
        #[cfg(not(windows))] { Vec::new() }
    }
    pub fn focus_window(&self) {
        #[cfg(windows)]
        if !self.window.is_null() {
            let event = unsafe {windows::show_and_focus(self.window)};
            let mut events = self.focus_events.borrow_mut();
            if events.len() == 8 { events.pop_front(); }
            events.push_back(event);
        }
    }
}

#[cfg(windows)]
impl Drop for Platform {
    fn drop(&mut self) {
        unsafe {
            self.hook.take();
            windows::remove_window_keyboard(self.window,&self.route);
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
    unsafe fn foreground_snapshot() -> serde_json::Value {
        let window = GetForegroundWindow();
        let mut process = 0;
        let thread = GetWindowThreadProcessId(window, &mut process);
        serde_json::json!({"window":window as usize,"process":process,"thread":thread})
    }
    pub unsafe fn show_and_focus(window: HWND) -> serde_json::Value {
        let started = std::time::Instant::now();
        let before = foreground_snapshot();
        ShowWindow(window,SW_SHOW);
        let accepted = SetForegroundWindow(window) != 0;
        // SetFocus alone can activate a background window's queue without
        // making it foreground. Keep both results visible in diagnostics.
        SetFocus(window);
        serde_json::json!({
            "event":"window_activation", "target":window as usize,
            "requested_at_ms":uu_core::time::now_ms(),
            "elapsed_us":started.elapsed().as_micros() as u64,
            "accepted":accepted, "before":before, "after":foreground_snapshot(),
            "keyboard_focus_matches":windows_sys::Win32::UI::Input::KeyboardAndMouse::GetFocus()==window
        })
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
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    use windows_sys::Win32::{
        System::LibraryLoader::GetModuleHandleW, UI::Input::KeyboardAndMouse::*,
    };
    const INPUT:u8=1;
    const SHORTCUTS:u8=2;
    const NESTED:u8=4;
    const CAPTURE:u8=8;
    pub struct Route {
        pub old_proc:isize,
        context:eframe::egui::Context,
        viewport:eframe::egui::ViewportId,
        policy:std::sync::atomic::AtomicU8,
        session:AtomicU64,
        keys:std::sync::Mutex<crate::keyboard_capture::RecipientKeys>,
        pub queue:std::sync::Mutex<std::collections::VecDeque<crate::keyboard_capture::Decision>>,
        pub overflow:std::sync::atomic::AtomicBool,
    }
    impl Route {
        pub fn configure_bindings(&self,bindings:&uu_core::hotkeys::Bindings) {
            let modifiers=crate::keyboard_capture::Modifiers::from_key_state(|vk|unsafe{GetAsyncKeyState(vk as i32)<0});
            self.keys.lock().unwrap().configure(bindings,&modifiers);
        }
        pub fn set_policy(&self,input:bool,shortcuts:bool,nested:bool) {
            self.policy.fetch_update(Relaxed,Relaxed,|old|Some((old&CAPTURE)
                |if input{INPUT}else{0}|if shortcuts{SHORTCUTS}else{0}|if nested{NESTED}else{0})).ok();
        }
        pub fn set_capture(&self,enabled:bool) {
            if enabled {self.policy.fetch_or(CAPTURE,Relaxed);}else{self.policy.fetch_and(!CAPTURE,Relaxed);}
        }
        pub fn select(&self,session:u64) {
            if self.session.swap(session,std::sync::atomic::Ordering::AcqRel)==session {return;}
            self.policy.store(0,Relaxed);
            self.queue.lock().unwrap().clear();
            let modifiers=crate::keyboard_capture::Modifiers::from_key_state(|vk|unsafe{GetAsyncKeyState(vk as i32)<0});
            self.keys.lock().unwrap().select((self as *const Self as isize,session),&modifiers);
        }
    }
    fn routes()->&'static std::sync::RwLock<std::collections::HashMap<isize,std::sync::Arc<Route>>> {
        static ROUTES:std::sync::OnceLock<std::sync::RwLock<std::collections::HashMap<isize,std::sync::Arc<Route>>>>=std::sync::OnceLock::new();
        ROUTES.get_or_init(Default::default)
    }
    fn route(window:HWND)->Option<std::sync::Arc<Route>> {
        routes().read().ok()?.get(&(window as isize)).cloned()
    }
    static HOOK_CALLBACKS: AtomicU64=AtomicU64::new(0);
    static HOOK_CAPTURED: AtomicU64=AtomicU64::new(0);
    static HOOK_BACKGROUND: AtomicU64=AtomicU64::new(0);
    static HOOK_INACTIVE: AtomicU64=AtomicU64::new(0);
    static HOOK_PREHELD: AtomicU64=AtomicU64::new(0);
    static HOOK_WIN_SEEN: AtomicU64=AtomicU64::new(0);
    static HOOK_WIN_CAPTURED: AtomicU64=AtomicU64::new(0);
    static WINDOW_WIN_SEEN: AtomicU64=AtomicU64::new(0);
    static WINDOW_ALT_SEEN: AtomicU64=AtomicU64::new(0);
    static WINDOW_CAPTURED: AtomicU64=AtomicU64::new(0);
    static WINDOW_TAB_SEEN: AtomicU64=AtomicU64::new(0);
    static WINDOW_HARDWARE: AtomicU64=AtomicU64::new(0);
    static WINDOW_INJECTED: AtomicU64=AtomicU64::new(0);
    static WINDOW_SYSTEM: AtomicU64=AtomicU64::new(0);
    static WINDOW_UNKNOWN: AtomicU64=AtomicU64::new(0);
    static HOOK_TAB_SEEN: AtomicU64=AtomicU64::new(0);
    static HOOK_TAB_CAPTURED: AtomicU64=AtomicU64::new(0);
    static HOOK_INJECTED: AtomicU64=AtomicU64::new(0);
    static HOOK_FIRST_CALLBACK: AtomicU64=AtomicU64::new(0);
    static HOOK_LAST_CALLBACK: AtomicU64=AtomicU64::new(0);
    static HOOK_LAST_COMPLETED: AtomicU64=AtomicU64::new(0);
    static HOOK_MAX_CALLBACK_US: AtomicU64=AtomicU64::new(0);
    static HOOK_PUMP_ACK: AtomicU64=AtomicU64::new(0);
    static HOOK_PUMP_LATENCY: AtomicU64=AtomicU64::new(0);
    const PUMP_PROBE:u32=WM_APP+0x71;
    fn clock_ms()->u64 {
        static START:std::sync::OnceLock<std::time::Instant>=std::sync::OnceLock::new();
        START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64+1
    }
    fn age_ms(now:u64,then:u64)->Option<u64> {(then!=0).then(||now.saturating_sub(then))}
    // Measure callback wall time, including downstream hooks when forwarding.
    // Never write a log, allocate, or wait for the UI from this guard.
    struct CallbackTimer(std::time::Instant);
    impl Drop for CallbackTimer {
        fn drop(&mut self) {
            HOOK_MAX_CALLBACK_US.fetch_max(self.0.elapsed().as_micros() as u64,Relaxed);
            HOOK_LAST_COMPLETED.store(clock_ms(),Relaxed);
        }
    }
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
    pub unsafe fn install_window_keyboard(window:HWND,context:eframe::egui::Context)->std::sync::Arc<Route> {
        let old=if window.is_null(){0}else{SetWindowLongPtrW(window,GWLP_WNDPROC,window_keyboard as *const () as isize)};
        let viewport=context.viewport_id();
        let result=std::sync::Arc::new(Route {old_proc:old,context,viewport,policy:0.into(),session:0.into(),
            keys:std::sync::Mutex::new(crate::keyboard_capture::RecipientKeys::new()),queue:Default::default(),overflow:false.into()});
        routes().write().unwrap().insert(window as isize,result.clone());
        result
    }
    pub unsafe fn remove_window_keyboard(window:HWND,route:&Route) {
        route.policy.store(0,Relaxed);
        if route.old_proc!=0 && IsWindow(window)!=0 && GetWindowLongPtrW(window,GWLP_WNDPROC)==window_keyboard as *const () as isize {
            SetWindowLongPtrW(window,GWLP_WNDPROC,route.old_proc);
        }
        routes().write().unwrap().remove(&(window as isize));
    }
    fn enqueue(event:crate::keyboard_capture::Decision,window:HWND,route:&Route,session:u64) {
        if let Ok(mut queue)=route.queue.try_lock() {
            if route.session.load(std::sync::atomic::Ordering::Acquire)!=session {return;}
            if queue.len()<512 {queue.push_back(event);}else{route.overflow.store(true,Relaxed);}
        }else{route.overflow.store(true,Relaxed);}
        unsafe{PostMessageW(window,WM_NULL,0,0);}
    }
    unsafe extern "system" fn window_keyboard(window:HWND,message:u32,wparam:usize,lparam:isize)->isize {
        let Some(state)=route(window) else{return DefWindowProcW(window,message,wparam,lparam)};
        if message==WM_NULL { state.context.request_repaint_of(state.viewport); }
        let down=message==WM_KEYDOWN||message==WM_SYSKEYDOWN;
        let up=message==WM_KEYUP||message==WM_SYSKEYUP;
        if down||up {
            let vk=crate::keyboard_capture::sided_vk(wparam as u32,lparam as usize);
            if matches!(vk,0x5b|0x5c){WINDOW_WIN_SEEN.fetch_add(1,Relaxed);}
            if matches!(vk,0x12|0xa4|0xa5){WINDOW_ALT_SEEN.fetch_add(1,Relaxed);}
            if vk==0x09 {WINDOW_TAB_SEEN.fetch_add(1,Relaxed);}
            // Source metadata applies to every key; never retain typed keys.
            use windows_sys::Win32::UI::Input::{GetCurrentInputMessageSource,INPUT_MESSAGE_SOURCE,IMO_HARDWARE,IMO_INJECTED,IMO_SYSTEM};
            let mut source:INPUT_MESSAGE_SOURCE=std::mem::zeroed();
            let origin=if GetCurrentInputMessageSource(&mut source)!=0 {source.originId}else{0};
            match origin {
                IMO_HARDWARE=>&WINDOW_HARDWARE, IMO_INJECTED=>&WINDOW_INJECTED,
                IMO_SYSTEM=>&WINDOW_SYSTEM, _=>&WINDOW_UNKNOWN,
            }.fetch_add(1,Relaxed);
            // The hook thread can be ahead of this queued window message.
            // GetKeyState describes this message, not the latest physical input.
            let modifiers=crate::keyboard_capture::Modifiers::from_key_state(|vk|GetKeyState(vk as i32)<0);
            if let Ok(mut keys)=state.keys.try_lock() {
                let policy=state.policy.load(std::sync::atomic::Ordering::Acquire);
                let route=crate::keyboard_capture::WindowRoute {
                    input:policy&INPUT!=0,
                    foreground:GetForegroundWindow()==window,
                    system_capture:policy&CAPTURE!=0,
                    shortcuts:policy&SHORTCUTS!=0,
                    nested_passthrough:policy&NESTED!=0,
                };
                let remote=route.input && route.foreground && (route.system_capture || !crate::keyboard_capture::local_system_key(vk,modifiers.win()));
                match keys.event(vk,down,crate::keyboard_capture::WindowRoute{input:remote,..route},&modifiers,lparam as usize&(1<<30)!=0) {
                    crate::keyboard_capture::Decision::Forward=>{},
                    crate::keyboard_capture::Decision::Consume=>return 0,
                    event=>{WINDOW_CAPTURED.fetch_add(1,Relaxed);enqueue(event,window,&state,state.session.load(Relaxed));return 0;},
                }
            }
        }
        let old=state.old_proc;
        if old==0 {DefWindowProcW(window,message,wparam,lparam)}
        else{CallWindowProcW(std::mem::transmute::<isize,WNDPROC>(old),window,message,wparam,lparam)}
    }
    fn preheld_for_capture(flags:u32,async_down:bool)->bool {
        // Injected input may already be reflected by GetAsyncKeyState before
        // the low-level callback runs. It is outer-client input, not a local
        // physical key that belonged to the desktop before capture.
        flags&(LLKHF_INJECTED|LLKHF_LOWER_IL_INJECTED)==0 && async_down
    }
    std::thread_local! {
        static HOOK_STATE: std::cell::RefCell<(crate::keyboard_capture::Modifiers,crate::keyboard_capture::RecipientKeys)> =
            const { std::cell::RefCell::new((crate::keyboard_capture::Modifiers::new(),crate::keyboard_capture::RecipientKeys::new())) };
        static SHARED_HOOK:std::cell::RefCell<std::rc::Weak<KeyboardHook>>=const {std::cell::RefCell::new(std::rc::Weak::new())};
    }
    pub unsafe fn shared_keyboard_hook(window:HWND)->Option<std::rc::Rc<KeyboardHook>> {
        SHARED_HOOK.with(|slot| {
            if let Some(hook)=slot.borrow().upgrade() {if hook.alive(){return Some(hook);}}
            let hook=std::rc::Rc::new(install_keyboard_hook(window)?);
            *slot.borrow_mut()=std::rc::Rc::downgrade(&hook);
            Some(hook)
        })
    }
    pub struct KeyboardHook {
        thread: Option<std::thread::JoinHandle<()>>,
        id: u32,
        installed_ms:u64,
        probe_sent:std::cell::Cell<u64>,
        probe_error:std::cell::Cell<Option<i32>>,
    }
    impl KeyboardHook {
        pub fn alive(&self)->bool {self.thread.as_ref().is_some_and(|thread| !thread.is_finished())}
        pub fn probe(&self) {
            let now=clock_ms();
            if self.probe_sent.get()!=0 && now.saturating_sub(self.probe_sent.get())<250 {return;}
            self.probe_sent.set(now);
            let ok=unsafe{PostThreadMessageW(self.id,PUMP_PROBE,now as usize,0)};
            self.probe_error.set(if ok==0 {std::io::Error::last_os_error().raw_os_error()}else{None});
        }
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
        let now=clock_ms();
        let window_thread=GetWindowThreadProcessId(window,std::ptr::null_mut());
        let same_desktop=hook.and_then(|hook| {
            Some(desktop_name(window_thread)?==desktop_name(hook.id)?)
        });
        let health=serde_json::json!({
            "hook_age_ms":hook.map(|hook|now.saturating_sub(hook.installed_ms)),
            "pump_probe_sent_ms":hook.map(|hook|hook.probe_sent.get()),
            "pump_probe_ack_ms":HOOK_PUMP_ACK.load(Relaxed),
            "pump_ack_age_ms":age_ms(now,HOOK_PUMP_ACK.load(Relaxed)),
            "pump_latency_ms":HOOK_PUMP_LATENCY.load(Relaxed),
            "pump_post_error":hook.and_then(|hook|hook.probe_error.get()),
            "first_callback_ms":HOOK_FIRST_CALLBACK.load(Relaxed),
            "last_callback_age_ms":age_ms(now,HOOK_LAST_CALLBACK.load(Relaxed)),
            "last_completed_age_ms":age_ms(now,HOOK_LAST_COMPLETED.load(Relaxed)),
            "max_callback_us":HOOK_MAX_CALLBACK_US.load(Relaxed),
            "hook_injected":HOOK_INJECTED.load(Relaxed),
            "tab_seen":HOOK_TAB_SEEN.load(Relaxed),"tab_captured":HOOK_TAB_CAPTURED.load(Relaxed),
            "window_tab_seen":WINDOW_TAB_SEEN.load(Relaxed),
            "window_origins":{"hardware":WINDOW_HARDWARE.load(Relaxed),"injected":WINDOW_INJECTED.load(Relaxed),
                "system":WINDOW_SYSTEM.load(Relaxed),"unknown":WINDOW_UNKNOWN.load(Relaxed)}
        });
        serde_json::json!({
            "window_valid":IsWindow(window)!=0,
            "foreground_matches":!window.is_null()&&GetForegroundWindow()==window,
            "foreground":foreground_snapshot(),
            "hook_thread_alive":hook.is_some_and(KeyboardHook::alive),
            "hook_health":health,
            "same_desktop":same_desktop,
            "capture_requested":route(window).is_some_and(|r|r.policy.load(Relaxed)&CAPTURE!=0),
            "callbacks":HOOK_CALLBACKS.load(Relaxed),"captured":HOOK_CAPTURED.load(Relaxed),
            "forwarded_background":HOOK_BACKGROUND.load(Relaxed),
            "forwarded_inactive":HOOK_INACTIVE.load(Relaxed),"forwarded_preheld":HOOK_PREHELD.load(Relaxed),
            "win_seen":HOOK_WIN_SEEN.load(Relaxed),"win_captured":HOOK_WIN_CAPTURED.load(Relaxed),
            "window_win_seen":WINDOW_WIN_SEEN.load(Relaxed),
            "window_alt_seen":WINDOW_ALT_SEEN.load(Relaxed),"window_captured":WINDOW_CAPTURED.load(Relaxed),
            "win_up":HOOK_WIN_UP.load(Relaxed),"win_injected_up":HOOK_WIN_INJECTED_UP.load(Relaxed),
            "alt_up":HOOK_ALT_UP.load(Relaxed),"alt_injected_up":HOOK_ALT_INJECTED_UP.load(Relaxed),
            "win_down":HOOK_WIN_DOWN.load(Relaxed),"alt_down":HOOK_ALT_DOWN.load(Relaxed),
            "win_forwarded_preheld":HOOK_WIN_FORWARDED_PREHELD.load(Relaxed),
            "alt_forwarded_preheld":HOOK_ALT_FORWARDED_PREHELD.load(Relaxed),
            "win_forwarded_inactive":HOOK_WIN_FORWARDED_INACTIVE.load(Relaxed),
            "alt_forwarded_inactive":HOOK_ALT_FORWARDED_INACTIVE.load(Relaxed),
            "win_forwarded_background":HOOK_WIN_FORWARDED_BACKGROUND.load(Relaxed),
            "alt_forwarded_background":HOOK_ALT_FORWARDED_BACKGROUND.load(Relaxed)
        })
    }
    impl Drop for KeyboardHook {
        fn drop(&mut self) {
            unsafe { PostThreadMessageW(self.id,WM_QUIT,0,0); }
            if let Some(thread)=self.thread.take(){let _=thread.join();}
        }
    }
    // winit registers both mouse and keyboard Raw Input when creating its event
    // loop. We consume only raw mouse motion; keyboard ownership belongs to the
    // window-message/low-level-hook paths. Keeping a foreground raw keyboard
    // registration can bypass low-level hooks for physical input on Windows.
    // Remove only the keyboard usage, before installing the hook and showing
    // the window. This is process-wide, not a fullscreen/focus toggle.
    unsafe fn remove_raw_keyboard_registration() -> std::io::Result<()> {
        use windows_sys::Win32::UI::Input::{RegisterRawInputDevices, RAWINPUTDEVICE, RIDEV_REMOVE};
        let device=RAWINPUTDEVICE {
            usUsagePage:0x01, // Generic Desktop Controls
            usUsage:0x06, // Keyboard
            dwFlags:RIDEV_REMOVE,
            hwndTarget:std::ptr::null_mut(),
        };
        if RegisterRawInputDevices(&device,1,std::mem::size_of::<RAWINPUTDEVICE>() as u32)==0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    pub unsafe fn install_keyboard_hook(window: HWND) -> Option<KeyboardHook> {
        if window.is_null(){return None;}
        if let Err(error)=remove_raw_keyboard_registration() {
            eprintln!("keyboard capture Raw Input configuration: {error}");
            return None;
        }
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
            let installed_ms=clock_ms();
            if ready_tx.send(Some((windows_sys::Win32::System::Threading::GetCurrentThreadId(),installed_ms))).is_ok(){
                while GetMessageW(&mut message,std::ptr::null_mut(),0,0)>0 {
                    if message.message==PUMP_PROBE {
                        HOOK_PUMP_LATENCY.store(clock_ms().saturating_sub(message.wParam as u64),Relaxed);
                        HOOK_PUMP_ACK.store(message.wParam as u64,std::sync::atomic::Ordering::Release);
                        continue;
                    }
                    TranslateMessage(&message);DispatchMessageW(&message);
                }
            }
            UnhookWindowsHookEx(hook);
        }).ok()?;
        match ready_rx.recv().ok().flatten() {
            Some((id,installed_ms))=>Some(KeyboardHook{thread:Some(thread),id,installed_ms,
                probe_sent:std::cell::Cell::new(0),probe_error:std::cell::Cell::new(None)}),
            None=>{let _=thread.join();None}
        }
    }
    unsafe extern "system" fn monitor_key(code: i32, message: usize, data: isize) -> isize {
        let _timer=(code>=0).then(||CallbackTimer(std::time::Instant::now()));
        if code >= 0 {
            let key = &*(data as *const KBDLLHOOKSTRUCT);
            let down = message == WM_KEYDOWN as usize || message == WM_SYSKEYDOWN as usize;
            let up = message == WM_KEYUP as usize || message == WM_SYSKEYUP as usize;
            let foreground=GetForegroundWindow();
            // Never block a low-level hook on the UI thread's registry lock.
            let selected=routes().try_read().ok().and_then(|routes|routes.get(&(foreground as isize)).cloned());
            let policy=selected.as_ref().map_or(0,|r|r.policy.load(std::sync::atomic::Ordering::Acquire));
            let session=selected.as_ref().map_or(0,|r|r.session.load(std::sync::atomic::Ordering::Acquire));
            let vk=crate::keyboard_capture::sided_vk(key.vkCode,
                ((key.scanCode as usize)<<16)|(((key.flags&LLKHF_EXTENDED) as usize)<<24));
            // Accept injected input from outer remote desktop clients as well.
            // Ownership applies uniformly to every key; no Win-chord exceptions.
            if down||up {
                let now=clock_ms();
                let _=HOOK_FIRST_CALLBACK.compare_exchange(0,now,Relaxed,Relaxed);
                HOOK_LAST_CALLBACK.store(now,Relaxed);
                HOOK_CALLBACKS.fetch_add(1,Relaxed);
                if key.flags&LLKHF_INJECTED!=0 {HOOK_INJECTED.fetch_add(1,Relaxed);}
                if vk==0x09 {HOOK_TAB_SEEN.fetch_add(1,Relaxed);}
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
                let owned=selected.is_some();
                let active=policy&CAPTURE!=0;
                let shortcuts=policy&SHORTCUTS!=0;
                let nested=policy&NESTED!=0;
                let already_down=preheld_for_capture(key.flags,GetAsyncKeyState(key.vkCode as i32)<0);
                let decision=HOOK_STATE.with(|state| {
                    let mut state=state.borrow_mut();
                    let (modifiers,keys)=&mut *state;
                    keys.select((if owned{foreground as isize}else{0},session),modifiers);
                    modifiers.observe(vk,down);
                    keys.event(vk,down,crate::keyboard_capture::WindowRoute {
                        input:active,shortcuts,foreground:owned,system_capture:active,nested_passthrough:nested,
                    },modifiers,already_down)
                });
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
                        if vk==0x09 {HOOK_TAB_CAPTURED.fetch_add(1,Relaxed);}
                        if let Some(route)=&selected {enqueue(event,foreground,route,session);}return 1;
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
    #[ignore = "run alone: owns a newly created noninteractive window station"]
    fn isolated_windows_keep_their_procedures_policies_and_session_queues() {
        let _station=super::owned_station::Station::create_with_keyboard_hook().unwrap();
        struct Window(HWND,std::sync::Arc<Route>);
        impl Drop for Window {fn drop(&mut self){unsafe{remove_window_keyboard(self.0,&self.1);DestroyWindow(self.0);}}}
        let create=||unsafe {
            let class:Vec<u16>="STATIC\0".encode_utf16().collect();
            let window=CreateWindowExW(0,class.as_ptr(),std::ptr::null(),0,0,0,0,0,
                HWND_MESSAGE,std::ptr::null_mut(),std::ptr::null_mut(),std::ptr::null());
            assert!(!window.is_null());
            Window(window,install_window_keyboard(window,Default::default()))
        };
        let a=create();let b=create();
        a.1.select(11);b.1.select(22);
        a.1.set_policy(true,true,false);a.1.set_capture(true);
        b.1.set_policy(false,true,true);
        assert_eq!(a.1.policy.load(Relaxed),INPUT|SHORTCUTS|CAPTURE);
        assert_eq!(b.1.policy.load(Relaxed),SHORTCUTS|NESTED);
        for window in [&a,&b] {
            // Seed a forwarded down, then deliver its native release. Release
            // ownership must survive loss of foreground and policy changes.
            assert_eq!(window.1.keys.lock().unwrap().keys.event_with_shortcuts(0x41,true,true,false,false,false,false),
                crate::keyboard_capture::Decision::Remote{vk:0x41,pressed:true});
            unsafe{SendMessageW(window.0,WM_KEYUP,0x41,0);}
            let mut queue=window.1.queue.lock().unwrap();
            assert_eq!(queue.pop_front(),Some(crate::keyboard_capture::Decision::Remote{vk:0x41,pressed:false}));
            assert!(queue.is_empty());
        }
        enqueue(crate::keyboard_capture::Decision::Shortcut(crate::shortcuts::Action::Stats),a.0,&a.1,11);
        assert_eq!(a.1.queue.lock().unwrap().len(),1);
        assert!(b.1.queue.lock().unwrap().is_empty());
        a.1.select(12);
        enqueue(crate::keyboard_capture::Decision::Shortcut(crate::shortcuts::Action::Exit),a.0,&a.1,11);
        assert!(a.1.queue.lock().unwrap().is_empty());
        let first=unsafe{shared_keyboard_hook(a.0)}.unwrap();
        let second=unsafe{shared_keyboard_hook(b.0)}.unwrap();
        assert!(std::rc::Rc::ptr_eq(&first,&second));
        drop(first);drop(a);
        assert!(second.alive());
        assert_eq!(b.1.policy.load(Relaxed),SHORTCUTS|NESTED);
        // Exercise the surviving native subclass and its own previous proc.
        unsafe{SendMessageW(b.0,WM_SETTEXT,0,"generated\0".encode_utf16().collect::<Vec<_>>().as_ptr() as isize);}
        assert_eq!(unsafe{GetWindowTextLengthW(b.0)},9);
        drop(second);drop(b);
        assert!(routes().read().unwrap().is_empty());
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
        // Reproduce winit's process-wide registration, including raw mouse
        // input needed by relative mode. Exercise the production setup below.
        use windows_sys::Win32::UI::Input::{RegisterRawInputDevices,GetRegisteredRawInputDevices,
            RAWINPUTDEVICE,RIDEV_DEVNOTIFY,RIDEV_REMOVE};
        let devices=[
            RAWINPUTDEVICE{usUsagePage:1,usUsage:2,dwFlags:RIDEV_DEVNOTIFY,hwndTarget:window.0},
            RAWINPUTDEVICE{usUsagePage:1,usUsage:6,dwFlags:RIDEV_DEVNOTIFY,hwndTarget:window.0},
        ];
        let size=std::mem::size_of::<RAWINPUTDEVICE>() as u32;
        assert_ne!(unsafe{RegisterRawInputDevices(devices.as_ptr(),2,size)},0);
        let registrations=||unsafe {
            let mut count=0;
            assert_eq!(GetRegisteredRawInputDevices(std::ptr::null_mut(),&mut count,size),0);
            let mut devices:Vec<RAWINPUTDEVICE>=vec![std::mem::zeroed();count as usize];
            let read=GetRegisteredRawInputDevices(devices.as_mut_ptr(),&mut count,size);
            assert_ne!(read,u32::MAX);
            devices.truncate(read as usize);devices
        };
        let before=registrations();
        assert!(before.iter().any(|d|d.usUsagePage==1&&d.usUsage==6));
        let mouse_before=before.iter().find(|d|d.usUsagePage==1&&d.usUsage==2).unwrap();
        assert_eq!(mouse_before.hwndTarget,window.0);
        // Compare OS readback before/after: Windows does not necessarily echo
        // registration-only flags (DEVNOTIFY) in this query.
        HOOK_PUMP_ACK.store(0,Ordering::Release);
        // Drop/join the hook before the window and station.
        let hook=unsafe{install_keyboard_hook(window.0)}.expect("isolated hook installation");
        assert!(hook.alive());
        for _ in 0..2 {
            let registered=registrations();
            assert!(!registered.iter().any(|d|d.usUsagePage==1&&d.usUsage==6),
                "raw keyboard registration competes with low-level capture");
            let mouse=registered.iter().find(|d|d.usUsagePage==1&&d.usUsage==2).unwrap();
            assert_eq!(mouse.hwndTarget,window.0);
            assert_eq!(mouse.dwFlags,mouse_before.dwFlags);
            // Reinitializing is idempotent and must keep the raw mouse target.
            unsafe{remove_raw_keyboard_registration().unwrap();}
        }
        let snapshot=unsafe{keyboard_diagnostics(window.0,Some(&hook))};
        assert_eq!(snapshot["window_valid"],true);
        assert_eq!(snapshot["hook_thread_alive"],true);
        assert_eq!(snapshot["same_desktop"],true);
        hook.probe();
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(HOOK_PUMP_ACK.load(Ordering::Acquire),hook.probe_sent.get(),"hook depended on UI message pumping");
        let snapshot=unsafe{keyboard_diagnostics(window.0,Some(&hook))};
        assert!(snapshot["hook_health"]["pump_ack_age_ms"].as_u64().is_some());
        assert_eq!(snapshot["hook_health"]["pump_post_error"],serde_json::Value::Null);
        drop(hook);
        let mouse=RAWINPUTDEVICE{usUsagePage:1,usUsage:2,dwFlags:RIDEV_REMOVE,hwndTarget:std::ptr::null_mut()};
        assert_ne!(unsafe{RegisterRawInputDevices(&mouse,1,size)},0);
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
