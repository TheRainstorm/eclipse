//! Reproduce a late geometry query only for our own window on an owned Xvfb.
#[cfg(target_os = "linux")]
#[path = "../../../tools/native/owned_display.rs"]
mod owned_display;

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use anyhow::{ensure, Context};
    use egui_winit::winit::{
        event_loop::EventLoop, platform::x11::EventLoopBuilderExtX11, window::Window,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use x11rb::{connection::Connection, protocol::xproto::ConnectionExt};

    let mut args = std::env::args().skip(1);
    ensure!(
        args.next().as_deref() == Some("--x11-display"),
        "explicit owned display required"
    );
    let display = args.next().context("missing owned display")?;
    ensure!(args.next().is_none(), "unexpected argument");
    owned_display::check(&display)?;
    let mut builder = EventLoop::builder();
    builder.with_x11();
    let event_loop = builder.build()?;
    #[allow(deprecated)]
    let window = event_loop.create_window(
        Window::default_attributes()
            .with_title("Owned X11 destruction fixture")
            .with_inner_size(egui_winit::winit::dpi::PhysicalSize::new(320, 180))
            .with_decorations(false)
            .with_visible(false),
    )?;
    let xid = match window.window_handle()?.as_raw() {
        RawWindowHandle::Xlib(handle) => u32::try_from(handle.window)?,
        RawWindowHandle::Xcb(handle) => handle.window.get(),
        _ => anyhow::bail!("expected our owned X11 window"),
    };
    let _ = window.inner_position()?;
    let _ = window.outer_position()?;
    let (connection, _) = x11rb::connect(Some(&display))?;
    connection.destroy_window(xid)?.check()?;
    connection.flush()?;
    for _ in 0..8 {
        let _ = window.inner_position()?;
        let _ = window.outer_position()?;
    }
    drop(window);
    drop(event_loop);
    println!(
        "{}",
        serde_json::json!({"window":"passed","late_position_queries":16,"owned_server_checked":true,"input_sent":false,"clipboard_used":false})
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This fixture requires an explicitly owned Linux Xvfb");
    std::process::exit(2);
}
