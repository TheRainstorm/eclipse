//! Native state checks on a fresh noninteractive station. This cannot prove
//! foreground permission on the user's interactive desktop.
#[path = "../../../../tools/native/windows_owned_station.rs"]
mod owned_station;
use super::boss_key::windows as boss;
use anyhow::{ensure, Result};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::{Foundation::*, UI::WindowsAndMessaging::*};
use winit::{
    event_loop::EventLoop,
    platform::windows::{EventLoopBuilderExtWindows, WindowExtWindows},
    window::{Window, WindowAttributes},
};
fn hwnd(window: &Window) -> HWND {
    let RawWindowHandle::Win32(handle) = window.window_handle().unwrap().as_raw() else { unreachable!() };
    handle.hwnd.get() as HWND
}
unsafe fn property(window: &Window, name: *const u16) -> bool { !GetPropW(hwnd(window), name).is_null() }
fn visible_style(window: &Window) -> bool {
    unsafe { GetWindowLongPtrW(hwnd(window), GWL_STYLE) as u32 & WS_VISIBLE != 0 }
}
fn rect(window: &Window) -> [i32; 4] {
    let mut value = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    assert_ne!(unsafe { GetWindowRect(hwnd(window), &mut value) }, 0);
    [value.left, value.top, value.right, value.bottom]
}
struct Marked(Vec<Window>);
impl Drop for Marked {
    fn drop(&mut self) { for window in &self.0 { unsafe { boss::unmark(hwnd(window)); } } }
}
#[allow(deprecated)]
pub fn run() -> Result<()> {
    ensure!(std::env::args().any(|arg| arg == "--owned-window-station"),
        "--owned-window-station is required");
    let _station = owned_station::Station::create()?;
    let event_loop = EventLoop::builder().with_any_thread(true).build()?;
    let make = |title: &str, visible| event_loop.create_window(WindowAttributes::default()
        .with_title(title).with_visible(visible).with_inner_size(winit::dpi::PhysicalSize::new(320, 200)));
    let windows = Marked(vec![make("Owned restored Eclipse", true)?,
        make("Owned minimized Eclipse", true)?, make("Owned hidden Eclipse", false)?]);
    let external = make("Owned external foreground surrogate", true)?;
    windows.0[0].set_maximized(true);
    windows.0[1].set_minimized(true);
    windows.0[0].set_visible_no_activate(true);
    windows.0[1].set_visible_no_activate(true);
    external.set_visible_no_activate(true);
    ensure!(windows.0[1].is_minimized() == Some(true), "fixture did not minimize its own window");
    ensure!(windows.0[0].is_maximized(), "fixture did not maximize its own window");
    for window in &windows.0 { unsafe { boss::mark(hwnd(window)); } }
    boss::registered(true);
    let before: Vec<_> = windows.0.iter().map(rect).collect();
    let visible: Vec<_> = windows.0.iter().map(visible_style).collect();
    ensure!(visible == [true, true, false], "unexpected fixture visibility flags: {visible:?}");
    // Some Windows versions report 0 for every window on an invisible station,
    // despite WS_VISIBLE being set. Never pretend this exercises foreground.
    let raw_visibility: Vec<_> = windows.0.iter().map(|w| unsafe { IsWindowVisible(hwnd(w)) }).collect();
    let visibility_query_supported = raw_visibility[0] != 0;
    ensure!(boss::toggle(), "native hide intent rejected");
    let targets = windows.0.iter().filter(|window| unsafe {
        property(window, windows_sys::w!("Eclipse.BossKey.RestoreTarget.1"))
    }).count();
    ensure!(targets == usize::from(visibility_query_supported), "unexpected restore target count");
    if visibility_query_supported {
        ensure!(unsafe { property(&windows.0[0], windows_sys::w!("Eclipse.BossKey.RestoreTarget.1")) },
            "previously hidden and minimized windows must not become the focus target");
    }
    for window in &windows.0 { window.set_visible_no_activate(false); }
    ensure!(boss::toggle(), "native restore intent rejected");
    ensure!(windows.0.iter().filter(|window| unsafe {
        property(window, windows_sys::w!("Eclipse.BossKey.Activate.1"))
    }).count() == usize::from(visibility_query_supported), "unexpected activation intent count");
    for (window, visible) in windows.0.iter().zip(&visible) {
        boss::receive(hwnd(window), false);
        window.set_visible_no_activate(*visible);
        // The GUI also queues the ordinary visibility command. Its state must
        // already agree, so replay cannot activate or unminimize a sibling.
        window.set_visible(*visible);
    }
    let focus = boss::take_focus_result(hwnd(&windows.0[0]));
    ensure!(focus.is_some() == visibility_query_supported, "unexpected activation result");
    ensure!(windows.0.iter().skip(1).all(|w| boss::take_focus_result(hwnd(w)).is_none()),
        "sibling windows must not request foreground");
    ensure!(windows.0[1].is_minimized() == Some(true), "restore changed minimized state");
    ensure!(windows.0[0].is_maximized(), "restore changed maximized state");
    ensure!(windows.0.iter().zip(&visible).all(|(w,v)| visible_style(w) == *v),
        "restore changed prior hidden flags");
    ensure!(windows.0.iter().map(rect).collect::<Vec<_>>() == before, "restore changed geometry");
    ensure!(visible_style(&external), "unmarked external window was hidden");
    ensure!(boss::toggle(), "second hide rejected");
    for window in &windows.0 { window.set_visible_no_activate(false); }
    boss::registered(false);
    boss::restore_if_unavailable();
    ensure!(!super::boss_key::hidden(), "lost hotkey registration must restore visibility");
    for window in &windows.0 {
        ensure!(!unsafe { property(window, windows_sys::w!("Eclipse.BossKey.Activate.1")) },
            "automatic recovery must not request activation");
    }
    println!("{}", serde_json::json!({"window":"passed","owned_station":true,
        "single_restore_target":visibility_query_supported.then_some(true),
        "visibility_query_supported":visibility_query_supported,"raw_visibility":raw_visibility,
        "prior_hidden_flags_preserved":true,"minimized_preserved":true,"maximized_preserved":true,
        "geometry_preserved":true,"automatic_recovery_does_not_activate":true,
        "native_focus_result":focus,"interactive_foreground_verified":false}));
    Ok(())
}
