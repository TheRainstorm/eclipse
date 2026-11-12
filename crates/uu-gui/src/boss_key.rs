//! Process-wide visibility owns no connection, audio or return-to-local state.
use eframe::egui;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex, OnceLock, Weak,
};

#[cfg(target_os = "linux")]
#[path = "boss_key/linux.rs"]
mod linux;
#[cfg(windows)]
#[path = "boss_key/windows.rs"]
pub(super) mod windows;

static HIDDEN: AtomicBool = AtomicBool::new(false);
struct Snapshot {
    visible: bool,
    minimized: bool,
    maximized: bool,
}
struct Window {
    native: Weak<winit::window::Window>,
    ctx: egui::Context,
    viewport: egui::ViewportId,
    snapshot: Option<Snapshot>,
    normal_visible: bool,
    recording: bool,
    #[cfg(target_os = "linux")]
    focused: bool,
    #[cfg(target_os = "linux")]
    focus_order: u64,
    #[cfg(target_os = "linux")]
    activation: Option<(String, std::time::Instant)>,
}
fn windows() -> &'static Mutex<Vec<Window>> {
    static WINDOWS: OnceLock<Mutex<Vec<Window>>> = OnceLock::new();
    WINDOWS.get_or_init(Default::default)
}
pub fn hidden() -> bool {
    HIDDEN.load(Ordering::Acquire)
}
fn observe(hidden: bool) {
    HIDDEN.store(hidden, Ordering::Release);
    if let Ok(windows) = windows().lock() {
        for window in windows.iter() {
            window.ctx.request_repaint_of(window.viewport);
            window.ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
    }
}
pub fn register(ctx: &egui::Context) {
    let Some(native) = eframe::viewport_window(ctx) else {
        return;
    };
    #[cfg(target_os = "linux")]
    linux::start();
    #[cfg(target_os = "linux")]
    let focused = native.has_focus();
    let mut windows = windows().lock().unwrap();
    windows.retain(|w| w.native.strong_count() > 0);
    if let Some(window) = windows.iter_mut()
        .find(|w| w.native.upgrade().is_some_and(|w| w.id() == native.id())) {
        #[cfg(target_os = "linux")]
        {
            if focused && !window.focused { window.focus_order = next_focus(); linux::focused(); }
            window.focused = focused;
        }
        #[cfg(not(target_os = "linux"))] let _ = window;
        return;
    }
    drop(windows);
    #[cfg(target_os = "linux")]
    if focused { linux::focused(); }
    let mut windows = self::windows().lock().unwrap();
    windows.push(Window {
        native: std::sync::Arc::downgrade(&native),
        ctx: ctx.clone(),
        viewport: ctx.viewport_id(),
        snapshot: hidden().then_some(Snapshot {
            visible: true,
            minimized: false,
            maximized: false,
        }),
        normal_visible: true,
        recording: false,
        #[cfg(target_os = "linux")] focused,
        #[cfg(target_os = "linux")] focus_order: if focused { next_focus() } else { 0 },
        #[cfg(target_os = "linux")] activation: None,
    });
}
#[cfg(target_os = "linux")]
fn next_focus() -> u64 {
    static ORDER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    ORDER.fetch_add(1, Ordering::Relaxed) + 1
}
#[cfg(target_os = "linux")]
pub(super) fn take_activation_token(ctx: &egui::Context) -> Option<String> {
    let viewport = ctx.viewport_id();
    ctx.data_mut(|data| data.remove_temp(egui::Id::new(("boss-activation", viewport))))
}

#[cfg(all(test, target_os = "linux"))]
mod activation_tests {
    #[test]
    fn viewport_token_is_consumed_once_without_reentering_context_lock() {
        let (sent, received) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let ctx = eframe::egui::Context::default();
            let key = eframe::egui::Id::new(("boss-activation", ctx.viewport_id()));
            ctx.data_mut(|data| data.insert_temp(key, String::from("owned-activation")));
            let first = super::take_activation_token(&ctx);
            let second = super::take_activation_token(&ctx);
            let _ = sent.send((first, second));
        });
        let (first, second) = received.recv_timeout(std::time::Duration::from_secs(2))
            .expect("activation token lookup must not deadlock the UI context");
        assert_eq!(first.as_deref(), Some("owned-activation"));
        assert_eq!(second, None);
    }
}
pub fn normal_visibility(viewport: egui::ViewportId, visible: bool) {
    #[cfg(windows)] let mut native_windows = vec![];
    if let Ok(mut windows) = windows().lock() {
        for window in windows.iter_mut().filter(|w| w.viewport == viewport) {
            if window.normal_visible != visible {
                if let Some(snapshot) = &mut window.snapshot { snapshot.visible = visible; }
            }
            window.normal_visible = visible;
            #[cfg(windows)] if let Some(native) = window.native.upgrade() { native_windows.push(native); }
        }
    }
    #[cfg(windows)] for native in native_windows { windows::normal_visibility(&native, visible); }
}
/// Recording owns the complete keyboard stream, including global commands.
pub fn recording(viewport: egui::ViewportId, active: bool) {
    #[cfg(windows)] let mut changed = vec![];
    let any;
    {
        let mut windows = windows().lock().unwrap();
        windows.retain(|w| w.native.strong_count() > 0);
        for window in windows.iter_mut().filter(|w| w.viewport == viewport) {
            if window.recording == active { continue; }
            window.recording = active;
            #[cfg(windows)]
            if let Some(native) = window.native.upgrade() { changed.push(native); }
        }
        any = windows.iter().any(|w| w.recording);
    }
    #[cfg(windows)]
    for native in changed {
        use raw_window_handle::HasWindowHandle;
        if let Ok(handle) = native.window_handle() {
            if let raw_window_handle::RawWindowHandle::Win32(handle) = handle.as_raw() {
                windows::recording(handle.hwnd.get() as _, active);
            }
        }
    }
    #[cfg(target_os = "linux")] linux::recording(any);
    #[cfg(not(target_os = "linux"))] let _ = any;
}
pub fn configure(bindings: &uu_core::hotkeys::Bindings) {
    #[cfg(target_os = "linux")]
    linux::configure(bindings.chords(uu_core::hotkeys::Action::BossKey));
    #[cfg(not(target_os = "linux"))]
    let _ = bindings;
}
pub fn toggle() -> bool {
    if !available() {
        return false;
    }
    #[cfg(windows)]
    {
        windows::toggle()
    }
    #[cfg(target_os = "linux")]
    {
        linux::toggle()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        false
    }
}
pub fn available() -> bool {
    #[cfg(windows)]
    {
        windows::available()
    }
    #[cfg(target_os = "linux")]
    {
        linux::available()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        false
    }
}
/// The GUI owner releases every remote input lease before calling this.
/// Native windows retain their fullscreen and placement while hidden.
pub fn apply() {
    let hidden = hidden();
    let mut actions = vec![];
    #[cfg(target_os = "linux")]
    let ready = linux::available();
    #[cfg(target_os = "linux")]
    let activation = (!hidden).then(linux::take_activation).flatten().filter(|_| ready);
    #[cfg(target_os = "linux")]
    let mut focus_target = None;
    #[cfg(target_os = "linux")]
    let mut focus_actions = vec![];
    {
        let mut windows = windows().lock().unwrap();
        windows.retain(|w| w.native.strong_count() > 0);
        for (index, window) in windows.iter_mut().enumerate() {
            let Some(native) = window.native.upgrade() else {
                continue;
            };
            #[cfg(not(target_os = "linux"))] let _ = index;
            #[cfg(target_os = "linux")]
            if hidden || !window.normal_visible || !ready {
                window.activation = None;
            } else if let Some((_, deadline)) = &window.activation {
                if std::time::Instant::now() >= *deadline {
                    window.activation = None;
                } else if native.is_visible() != Some(false) && native.is_minimized() != Some(true) {
                    focus_actions.push((native.clone(), window.ctx.clone(), window.viewport,
                        window.activation.take().unwrap().0));
                } else {
                    // X11 maps asynchronously. Keep this one gesture until
                    // MapNotify; focus_window would discard it during YesWait.
                    window.ctx.request_repaint_after_for(std::time::Duration::from_millis(10), egui::ViewportId::ROOT);
                }
            }
            #[cfg(target_os = "linux")]
            if activation.is_some() && window.normal_visible {
                let eligible = window.snapshot.as_ref().map(|s| s.visible && !s.minimized)
                    .unwrap_or_else(|| native.is_visible() != Some(false) && native.is_minimized() != Some(true));
                if eligible && focus_target.as_ref().is_none_or(|(_, order)| window.focus_order > *order) {
                    focus_target = Some((index, window.focus_order));
                }
            }
            if hidden {
                if window.snapshot.is_none() {
                    window.snapshot = Some(Snapshot {
                        visible: native.is_visible().unwrap_or(true),
                        minimized: native.is_minimized().unwrap_or(false),
                        maximized: native.is_maximized(),
                    });
                }
                actions.push((native, window.ctx.clone(), window.viewport, None));
            } else if let Some(mut snapshot) = window.snapshot.take() {
                snapshot.visible &= window.normal_visible;
                actions.push((native, window.ctx.clone(), window.viewport, Some(snapshot)));
            }
        }
        #[cfg(target_os = "linux")]
        if let Some((index, _)) = focus_target {
            let window = &mut windows[index];
            window.activation = Some((activation.unwrap(), std::time::Instant::now() + std::time::Duration::from_secs(1)));
            window.ctx.request_repaint_after_for(std::time::Duration::from_millis(10), egui::ViewportId::ROOT);
        }
    }
    for (native, ctx, viewport, snapshot) in actions {
        let visible = snapshot.as_ref().is_some_and(|s| s.visible);
        ctx.send_viewport_cmd_to(viewport, egui::ViewportCommand::Visible(visible));
        #[cfg(windows)]
        {
            use winit::platform::windows::WindowExtWindows;
            native.set_visible_no_activate(visible);
        }
        #[cfg(not(windows))]
        if native.is_visible() != Some(visible) {
            native.set_visible(visible);
        }
        if let Some(snapshot) = snapshot.filter(|s| s.visible) {
            if native.is_maximized() != snapshot.maximized {
                native.set_maximized(snapshot.maximized);
            }
            if native.is_minimized() != Some(snapshot.minimized) {
                native.set_minimized(snapshot.minimized);
            }
        }
    }
    #[cfg(target_os = "linux")]
    for (native, ctx, viewport, token) in focus_actions {
        if !token.is_empty() {
            ctx.data_mut(|data| data.insert_temp(egui::Id::new(("boss-activation", viewport)), token));
            ctx.request_repaint_of(viewport);
        } else { native.focus_window(); }
    }
}
