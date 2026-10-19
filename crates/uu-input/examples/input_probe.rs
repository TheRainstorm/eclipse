//! Exercise XTest only inside the explicit display's own fixture window.
#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use anyhow::{ensure, Context};
    use uu_input::{InputEvent as I, InputInjector, KeyCode, MouseButton};
    use x11rb::{
        connection::Connection,
        protocol::{
            xproto::{
                ConnectionExt as _, CreateWindowAux, EventMask, InputFocus, KeyButMask, WindowClass,
            },
            Event,
        },
        COPY_FROM_PARENT,
    };
    let mut args = std::env::args().skip(1);
    ensure!(
        args.next().as_deref() == Some("--x11-display"),
        "explicit --x11-display is required"
    );
    let display = args.next().context("missing owned X11 display")?;
    ensure!(args.next().is_none(), "unexpected argument");
    let (conn, index) = x11rb::connect(Some(&display))?;
    let screen = &conn.setup().roots[index];
    let root = screen.root;
    let window = conn.generate_id()?;
    conn.create_window(
        COPY_FROM_PARENT as u8,
        window,
        root,
        10,
        10,
        600,
        300,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new()
            .background_pixel(screen.white_pixel)
            .event_mask(
                EventMask::KEY_PRESS
                    | EventMask::KEY_RELEASE
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::POINTER_MOTION,
            ),
    )?
    .check()?;
    conn.map_window(window)?.check()?;
    conn.set_input_focus(InputFocus::PARENT, window, 0u32)?
        .check()?;
    ensure!(
        conn.get_input_focus()?.reply()?.focus == window,
        "owned fixture lost focus"
    );
    let mut input = uu_input::linux::XTestInjector::connect(Some(&display))?;
    input.inject(I::MouseMoveAbsolute { x: 70, y: 80 })?;
    let pointer = conn.query_pointer(root)?.reply()?;
    ensure!(
        (pointer.root_x, pointer.root_y) == (70, 80),
        "absolute pointer mapping failed"
    );
    input.inject(I::MouseMoveRelative { dx: 5, dy: -3 })?;
    let pointer = conn.query_pointer(root)?.reply()?;
    ensure!(
        (pointer.root_x, pointer.root_y) == (75, 77),
        "relative pointer mapping failed"
    );
    for button in [MouseButton::Middle, MouseButton::Back, MouseButton::Forward] {
        input.inject(I::MouseButton {
            button,
            pressed: true,
        })?;
        input.inject(I::MouseButton {
            button,
            pressed: false,
        })?;
    }
    input.inject(I::MouseScroll { dx: 60, dy: 60 })?;
    input.inject(I::MouseScroll { dx: 60, dy: 60 })?;
    input.inject(I::Key {
        key: KeyCode(0xe1),
        pressed: true,
    })?;
    for usage in [0x33, 0x1e, 0x2f] {
        input.inject(I::Key {
            key: KeyCode(usage),
            pressed: true,
        })?;
        input.inject(I::Key {
            key: KeyCode(usage),
            pressed: false,
        })?;
    }
    input.release_all()?;
    for usage in [0xe4, 0xe6, 4] {
        input.inject(I::Key {
            key: KeyCode(usage),
            pressed: true,
        })?;
        input.release_all()?;
    }
    // One injector must not release a key owned by another one.
    let mut other = uu_input::linux::XTestInjector::connect(Some(&display))?;
    other.inject(I::Key {
        key: KeyCode::ESC,
        pressed: true,
    })?;
    input.inject(I::Key {
        key: KeyCode::ESC,
        pressed: false,
    })?;
    input.release_all()?;
    let code = KeyCode::ESC.mapping()?.xkb as usize;
    ensure!(
        conn.query_keymap()?.reply()?.keys[code / 8] & (1 << (code % 8)) != 0,
        "unrelated held key was released"
    );
    other.release_all()?;
    let keymap = conn.query_keymap()?.reply()?.keys;
    for key in [4, 0xe1, 0xe4, 0xe6, 0x29] {
        let code = KeyCode(key).mapping()?.xkb as usize;
        ensure!(
            keymap[code / 8] & (1 << (code % 8)) == 0,
            "session left a key held"
        );
    }
    let mut buttons = Vec::new();
    let mut keys = Vec::new();
    while let Some(event) = conn.poll_for_event()? {
        match event {
            Event::ButtonPress(e) => buttons.push((true, e.detail)),
            Event::ButtonRelease(e) => buttons.push((false, e.detail)),
            Event::KeyPress(e) => keys.push((e.detail, e.state)),
            _ => {}
        }
    }
    for detail in [2, 8, 9, 7, 4] {
        ensure!(
            buttons.contains(&(true, detail)) && buttons.contains(&(false, detail)),
            "button or wheel axis mismatch"
        );
    }
    ensure!(
        keys.iter()
            .any(|(key, state)| *key == 47 && state.contains(KeyButMask::SHIFT)),
        "shifted punctuation was not received"
    );
    for detail in [105, 108] {
        ensure!(
            keys.iter().any(|(key, _)| *key == detail),
            "modifier side was lost"
        );
    }
    ensure!(
        conn.get_input_focus()?.reply()?.focus == window,
        "owned fixture lost focus"
    );
    conn.destroy_window(window)?.check()?;
    println!("{{\"input\":\"passed\",\"keys\":{},\"buttons\":{},\"absolute\":true,\"relative\":true,\"session_release\":true}}",keys.len(),buttons.len());
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This fixture requires an owned X11 test server");
    std::process::exit(2);
}
