//! Isolated-compositor test driver. Standard US physical keymap, persistent
//! keyboard and pointer. JSON requests on stdin; no access to other sessions.
#[cfg(target_os = "linux")]
mod linux {
    use std::{
        ffi::CStr,
        io::{BufRead, Write},
        os::fd::AsFd,
        time::{Duration, Instant},
    };
    use wayland_client::{
        protocol::{wl_registry, wl_seat},
        Connection, Dispatch, QueueHandle,
    };
    use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
        zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1 as KeyboardManager,
        zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1 as Keyboard,
    };
    use wayland_protocols_wlr::virtual_pointer::v1::client::{
        zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1 as PointerManager,
        zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1 as Pointer,
    };
    #[derive(Default)]
    struct State {
        seat: Option<wl_seat::WlSeat>,
        km: Option<KeyboardManager>,
        pm: Option<PointerManager>,
    }
    impl Dispatch<wl_registry::WlRegistry, ()> for State {
        fn event(
            s: &mut Self,
            registry: &wl_registry::WlRegistry,
            e: wl_registry::Event,
            _: &(),
            _: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global {
                name,
                interface,
                version,
            } = e
            {
                match interface.as_str() {
                    "wl_seat" if s.seat.is_none() => {
                        s.seat = Some(registry.bind(name, version.min(5), qh, ()))
                    }
                    "zwp_virtual_keyboard_manager_v1" => {
                        s.km = Some(registry.bind(name, 1, qh, ()))
                    }
                    "zwlr_virtual_pointer_manager_v1" => {
                        s.pm = Some(registry.bind(name, 1, qh, ()))
                    }
                    _ => {}
                }
            }
        }
    }
    wayland_client::delegate_noop!(State: ignore wl_seat::WlSeat);
    wayland_client::delegate_noop!(State: ignore KeyboardManager);
    wayland_client::delegate_noop!(State: ignore Keyboard);
    wayland_client::delegate_noop!(State: ignore PointerManager);
    wayland_client::delegate_noop!(State: ignore Pointer);
    fn keymap() -> anyhow::Result<Vec<u8>> {
        let xkb = xkbcommon_dl::xkbcommon_handle();
        // SAFETY: checked library allocations, balanced references and free.
        unsafe {
            let context =
                (xkb.xkb_context_new)(xkbcommon_dl::xkb_context_flags::XKB_CONTEXT_NO_FLAGS);
            anyhow::ensure!(!context.is_null(), "XKB context");
            let names = xkbcommon_dl::xkb_rule_names {
                rules: std::ptr::null(),
                model: std::ptr::null(),
                layout: c"us".as_ptr(),
                variant: std::ptr::null(),
                options: std::ptr::null(),
            };
            let map = (xkb.xkb_keymap_new_from_names)(
                context,
                &names,
                xkbcommon_dl::xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS,
            );
            (xkb.xkb_context_unref)(context);
            anyhow::ensure!(!map.is_null(), "US XKB keymap");
            let text = (xkb.xkb_keymap_get_as_string)(
                map,
                xkbcommon_dl::xkb_keymap_format::XKB_KEYMAP_FORMAT_TEXT_V1,
            );
            (xkb.xkb_keymap_unref)(map);
            anyhow::ensure!(!text.is_null(), "XKB keymap text");
            let result = CStr::from_ptr(text).to_bytes_with_nul().to_vec();
            libc::free(text as *mut libc::c_void);
            Ok(result)
        }
    }
    fn code(key: &str) -> anyhow::Result<(u32, bool)> {
        let value = match key {
            "a" => (30, false),
            "A" => (30, true),
            "s" => (31, false),
            "f" => (33, false),
            "x" => (45, false),
            "q" => (16, false),
            "z" => (44, false),
            "c" => (46, false),
            "v" => (47, false),
            ";" => (39, false),
            ":" => (39, true),
            "," => (51, false),
            "." => (52, false),
            "?" => (53, true),
            "/" => (53, false),
            "[" => (26, false),
            "]" => (27, false),
            "{" => (26, true),
            "}" => (27, true),
            "+" => (13, true),
            "-" => (12, false),
            "_" => (12, true),
            "F22" => (192, false),
            "F23" => (193, false),
            _ => anyhow::bail!("Unsupported fixture key"),
        };
        Ok(value)
    }
    pub fn main() -> anyhow::Result<()> {
        let conn = Connection::connect_to_env()?;
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        let mut state = State::default();
        let _registry = conn.display().get_registry(&qh, ());
        queue.roundtrip(&mut state)?;
        let seat = state
            .seat
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No seat"))?;
        let keyboard = state
            .km
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No virtual keyboard protocol"))?
            .create_virtual_keyboard(seat, &qh, ());
        let pointer = state
            .pm
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No virtual pointer protocol"))?
            .create_virtual_pointer(Some(seat), &qh, ());
        let bytes = keymap()?;
        let path = std::env::temp_dir().join(format!("uu-test-keymap-{}", std::process::id()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .open(&path)?;
        std::fs::remove_file(&path)?;
        file.write_all(&bytes)?;
        keyboard.keymap(1, file.as_fd(), bytes.len() as u32);
        queue.roundtrip(&mut state)?;
        let start = Instant::now();
        println!("{{\"ready\":true}}");
        std::io::stdout().flush()?;
        for line in std::io::stdin().lock().lines() {
            let v: serde_json::Value = serde_json::from_str(&line?)?;
            let time = || start.elapsed().as_millis() as u32;
            if let Some(dx) = v.get("dx").and_then(|x| x.as_f64()) {
                pointer.motion(time(), dx, v["dy"].as_f64().unwrap());
                pointer.frame();
            } else if let Some(x) = v.get("x").and_then(|x| x.as_u64()) {
                pointer.motion_absolute(
                    time(),
                    x as u32,
                    v["y"].as_u64().unwrap() as u32,
                    1600,
                    1000,
                );
                pointer.frame();
                conn.flush()?;
                std::thread::sleep(Duration::from_millis(60));
                pointer.button(
                    time(),
                    0x110,
                    wayland_client::protocol::wl_pointer::ButtonState::Pressed,
                );
                pointer.frame();
                conn.flush()?;
                std::thread::sleep(Duration::from_millis(40));
                pointer.button(
                    time(),
                    0x110,
                    wayland_client::protocol::wl_pointer::ButtonState::Released,
                );
                pointer.frame();
            } else {
                let text = v.get("text").and_then(|s| s.as_str());
                let keys: Vec<String> = if let Some(text) = text {
                    text.chars().map(|c| c.to_string()).collect()
                } else {
                    vec![v["key"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("Missing key"))?
                        .to_owned()]
                };
                for key in keys {
                    let (key, shift) = code(&key)?;
                    let mut mask = if shift { 1 } else { 0 };
                    for m in v["mods"].as_array().into_iter().flatten() {
                        mask |= match m.as_str() {
                            Some("shift") => 1,
                            Some("ctrl") => 4,
                            Some("alt") => 8,
                            Some("logo") => 64,
                            _ => 0,
                        };
                    }
                    keyboard.modifiers(mask, 0, 0, 0);
                    conn.flush()?;
                    std::thread::sleep(Duration::from_millis(35));
                    keyboard.key(time(), key, 1);
                    conn.flush()?;
                    std::thread::sleep(Duration::from_millis(35));
                    keyboard.key(time(), key, 0);
                    keyboard.modifiers(0, 0, 0, 0);
                    conn.flush()?;
                    std::thread::sleep(Duration::from_millis(35));
                }
            }
            queue.roundtrip(&mut state)?;
            println!("{{\"ok\":true}}");
            std::io::stdout().flush()?;
        }
        keyboard.modifiers(0, 0, 0, 0);
        keyboard.destroy();
        pointer.destroy();
        conn.flush()?;
        Ok(())
    }
}
#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    linux::main()
}
#[cfg(not(target_os = "linux"))]
fn main() {}
