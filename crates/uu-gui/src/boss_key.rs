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
    focused: bool,
}
struct Window {
    native: Weak<winit::window::Window>,
    ctx: egui::Context,
    viewport: egui::ViewportId,
    snapshot: Option<Snapshot>,
    normal_visible: bool,
    recording: bool,
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
    let mut windows = windows().lock().unwrap();
    windows.retain(|w| w.native.strong_count() > 0);
    if windows
        .iter()
        .any(|w| w.native.upgrade().is_some_and(|w| w.id() == native.id()))
    {
        return;
    }
    windows.push(Window {
        native: std::sync::Arc::downgrade(&native),
        ctx: ctx.clone(),
        viewport: ctx.viewport_id(),
        snapshot: hidden().then_some(Snapshot {
            visible: true,
            minimized: false,
            maximized: false,
            focused: false,
        }),
        normal_visible: true,
        recording: false,
    });
}
pub fn normal_visibility(viewport: egui::ViewportId, visible: bool) {
    if let Ok(mut windows) = windows().lock() {
        for window in windows.iter_mut().filter(|w| w.viewport == viewport) {
            if window.normal_visible != visible {
                if let Some(snapshot) = &mut window.snapshot { snapshot.visible = visible; }
            }
            window.normal_visible = visible;
        }
    }
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
    {
        let mut windows = windows().lock().unwrap();
        windows.retain(|w| w.native.strong_count() > 0);
        for window in windows.iter_mut() {
            let Some(native) = window.native.upgrade() else {
                continue;
            };
            if hidden {
                if window.snapshot.is_none() {
                    window.snapshot = Some(Snapshot {
                        visible: native.is_visible().unwrap_or(true),
                        minimized: native.is_minimized().unwrap_or(false),
                        maximized: native.is_maximized(),
                        focused: native.has_focus(),
                    });
                }
                actions.push((native, window.ctx.clone(), window.viewport, None));
            } else if let Some(mut snapshot) = window.snapshot.take() {
                snapshot.visible &= window.normal_visible;
                actions.push((native, window.ctx.clone(), window.viewport, Some(snapshot)));
            }
        }
    }
    for (native, ctx, viewport, snapshot) in actions {
        let visible = snapshot.as_ref().is_some_and(|s| s.visible);
        ctx.send_viewport_cmd_to(viewport, egui::ViewportCommand::Visible(visible));
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
            if snapshot.focused && !snapshot.minimized {
                native.focus_window();
            }
        }
    }
}
