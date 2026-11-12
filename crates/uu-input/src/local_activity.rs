//! Observe physical activity time and transient held state without an input log.
//! Injected events never update this clock; observers do not swallow events.
use std::{sync::{atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering}, OnceLock}, time::{Duration, Instant}};

#[derive(Clone, Copy, Debug, Default)]
pub struct Snapshot {
    pub available: bool,
    pub last: Option<Instant>,
    pub held: bool,
    pub revision: u64,
}
struct Activity {
    available: AtomicU32,
    micros: AtomicU64,
    revision: AtomicU64,
    keys: [AtomicU64; 4],
    buttons: AtomicU32,
}
impl Activity {
    const fn new() -> Self {
        Self { available: AtomicU32::new(0), micros: AtomicU64::new(0), revision: AtomicU64::new(0),
            keys: [const { AtomicU64::new(0) }; 4], buttons: AtomicU32::new(0) }
    }
    fn observe(&self, injected: bool, key: Option<(u32, bool)>, button: Option<(u32, bool)>) {
        if injected { return; }
        if let Some((key, down)) = key.filter(|(key, _)| *key < 256) {
            let bits = &self.keys[key as usize / 64];
            let mask = 1u64 << (key % 64);
            if down { bits.fetch_or(mask, Ordering::Relaxed); } else { bits.fetch_and(!mask, Ordering::Relaxed); }
        }
        if let Some((mask, down)) = button {
            if down { self.buttons.fetch_or(mask, Ordering::Relaxed); } else { self.buttons.fetch_and(!mask, Ordering::Relaxed); }
        }
        let micros = origin().elapsed().as_micros().min(u64::MAX as u128) as u64;
        self.micros.store(micros.max(1), Ordering::Release);
        self.revision.fetch_add(1, Ordering::Release);
    }
    fn clear_held(&self) {
        for keys in &self.keys { keys.store(0, Ordering::Relaxed); }
        self.buttons.store(0, Ordering::Relaxed);
    }
    fn snapshot(&self) -> Snapshot {
        let micros = self.micros.load(Ordering::Acquire);
        Snapshot { available: self.available.load(Ordering::Acquire) != 0,
            last: (micros != 0).then(|| origin() + Duration::from_micros(micros)),
            held: self.buttons.load(Ordering::Relaxed) != 0 || self.keys.iter().any(|v| v.load(Ordering::Relaxed) != 0),
            revision: self.revision.load(Ordering::Acquire) }
    }
}
fn origin() -> Instant { *ORIGIN.get_or_init(Instant::now) }
static ORIGIN: OnceLock<Instant> = OnceLock::new();
static ACTIVITY: Activity = Activity::new();
pub fn snapshot() -> Snapshot { ACTIVITY.snapshot() }

#[cfg(windows)]
pub use windows::{start, Monitor};
#[cfg(not(windows))]
pub struct Monitor;
#[cfg(not(windows))]
pub fn start() -> std::io::Result<Monitor> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "physical input observation is unavailable on this platform"))
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{io, ptr, sync::{Arc, Mutex, Weak, mpsc}, thread::JoinHandle};
    use windows_sys::Win32::{Foundation::*, System::{LibraryLoader::*, Threading::GetCurrentThreadId},
        UI::WindowsAndMessaging::*};

    struct Owner { stop: Arc<AtomicBool>, thread_id: u32, thread: Mutex<Option<JoinHandle<()>>> }
    pub struct Monitor { _owner: Arc<Owner> }
    impl Drop for Owner {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0); }
            if let Some(thread) = self.thread.get_mut().unwrap().take() { let _ = thread.join(); }
        }
    }
    unsafe extern "system" fn keyboard(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
        if code >= 0 {
            let event = &*(data as *const KBDLLHOOKSTRUCT);
            let down = matches!(message as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            ACTIVITY.observe(event.flags & LLKHF_INJECTED != 0, Some((event.vkCode, down)), None);
        }
        CallNextHookEx(ptr::null_mut(), code, message, data)
    }
    unsafe extern "system" fn mouse(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
        if code >= 0 {
            let event = &*(data as *const MSLLHOOKSTRUCT);
            let button = match message as u32 {
                WM_LBUTTONDOWN | WM_LBUTTONUP => Some((1, message as u32 == WM_LBUTTONDOWN)),
                WM_RBUTTONDOWN | WM_RBUTTONUP => Some((2, message as u32 == WM_RBUTTONDOWN)),
                WM_MBUTTONDOWN | WM_MBUTTONUP => Some((4, message as u32 == WM_MBUTTONDOWN)),
                WM_XBUTTONDOWN | WM_XBUTTONUP => Some((if event.mouseData >> 16 == 1 { 8 } else { 16 }, message as u32 == WM_XBUTTONDOWN)),
                _ => None,
            };
            ACTIVITY.observe(event.flags & LLMHF_INJECTED != 0, None, button);
        }
        CallNextHookEx(ptr::null_mut(), code, message, data)
    }
    struct Hooks(HHOOK, HHOOK);
    impl Hooks {
        fn install() -> io::Result<Self> {
            unsafe {
                // This callback may live in the shared SDK DLL, rather than
                // the executable. Resolve the module from the callback itself.
                let mut module=ptr::null_mut();
                if GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS|GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                    keyboard as *const () as *const u16,&mut module)==0 {return Err(io::Error::last_os_error());}
                let keys = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), module, 0);
                if keys.is_null() { return Err(io::Error::last_os_error()); }
                let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), module, 0);
                if mouse.is_null() { UnhookWindowsHookEx(keys); return Err(io::Error::last_os_error()); }
                ACTIVITY.available.fetch_add(1, Ordering::AcqRel);
                Ok(Self(keys, mouse))
            }
        }
    }
    impl Drop for Hooks {
        fn drop(&mut self) { unsafe {
            ACTIVITY.available.fetch_sub(1, Ordering::AcqRel);
            UnhookWindowsHookEx(self.0); UnhookWindowsHookEx(self.1);
            ACTIVITY.clear_held();
        } }
    }
    pub fn start() -> io::Result<Monitor> {
        static OWNER: OnceLock<Mutex<Weak<Owner>>> = OnceLock::new();
        let mut slot = OWNER.get_or_init(Mutex::default).lock().unwrap();
        if let Some(owner) = slot.upgrade() { return Ok(Monitor { _owner: owner }); }
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let (ready, receive) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new().name("host-local-input".into()).spawn(move || {
            let setup = || -> io::Result<Hooks> { crate::windows_desktop::sync_input_desktop()?; Hooks::install() };
            let mut hooks = match setup() { Ok(hooks) => Some(hooks), Err(error) => { let _ = ready.send(Err(error)); return; } };
            let id = unsafe { GetCurrentThreadId() };
            unsafe { SetTimer(ptr::null_mut(), 1, 250, None); }
            if ready.send(Ok(id)).is_err() { return; }
            let mut message: MSG = unsafe { std::mem::zeroed() };
            while !stopping.load(Ordering::Acquire) && unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) } > 0 {
                if message.message == WM_TIMER && (hooks.is_none() || crate::windows_desktop::input_desktop_changed().unwrap_or(true)) {
                    // Remove hooks before changing desktop; SetThreadDesktop
                    // rejects a thread that still owns hooks on its old desktop.
                    hooks.take();
                    hooks = setup().ok();
                }
                unsafe { TranslateMessage(&message); DispatchMessageW(&message); }
            }
            unsafe { KillTimer(ptr::null_mut(), 1); }
            drop(hooks);
        })?;
        let thread_id = match receive.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(id)) => id,
            Ok(Err(error)) => { let _ = thread.join(); return Err(error); }
            Err(_) => { stop.store(true, Ordering::Release); return Err(io::Error::new(io::ErrorKind::TimedOut, "physical input observer did not start")); }
        };
        let owner = Arc::new(Owner { stop, thread_id, thread: Mutex::new(Some(thread)) });
        *slot = Arc::downgrade(&owner);
        Ok(Monitor { _owner: owner })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn injected_events_do_not_extend_local_priority_and_held_state_balances() {
        let state = Activity::new();
        state.observe(true, Some((65, true)), None);
        assert_eq!(state.snapshot().revision, 0);
        assert!(!state.snapshot().held);
        state.observe(false, Some((65, true)), None);
        state.observe(false, Some((65, true)), None);
        assert!(state.snapshot().held);
        state.observe(false, Some((65, false)), None);
        assert!(!state.snapshot().held);
        state.observe(false, None, Some((1, true)));
        assert!(state.snapshot().held);
        let revision = state.snapshot().revision;
        state.observe(true, None, Some((1, false)));
        assert_eq!(state.snapshot().revision, revision);
        assert!(state.snapshot().held);
        state.observe(false, None, Some((1, false)));
        assert!(!state.snapshot().held);
        assert!(state.snapshot().last.is_some());
    }
}
