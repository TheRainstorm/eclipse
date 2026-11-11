//! Generated native windows on an owned Xvfb and private session bus only.
#[cfg(any(target_os = "linux", windows))]
#[path = "../src/boss_key.rs"]
mod boss_key;
#[cfg(target_os = "linux")]
#[path = "../../../tools/native/owned_display.rs"]
mod owned_display;

#[cfg(target_os = "linux")]
mod probe {
    use super::*;
    use eframe::egui;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::{
        io::BufRead,
        path::PathBuf,
        process::{Child, Command, Stdio},
        time::{Duration, Instant},
    };
    use x11rb::{
        connection::Connection,
        protocol::{xproto::ConnectionExt as _, xtest::ConnectionExt as _},
        rust_connection::RustConnection,
    };
    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn normal() -> egui::ViewportId {
        egui::ViewportId::from_hash_of("owned-boss-normal")
    }
    fn hidden() -> egui::ViewportId {
        egui::ViewportId::from_hash_of("owned-boss-already-hidden")
    }
    struct Keys {
        connection: RustConnection,
        root: u32,
        control: u8,
        alt: u8,
        key: u8,
        external: u32,
        active_atom: u32,
    }
    impl Keys {
        fn new(child: bool) -> anyhow::Result<Self> {
            use x11rb::protocol::xproto::*;
            let (connection, screen) = RustConnection::connect(None)?;
            let root = connection.setup().roots[screen].root;
            let min = connection.setup().min_keycode;
            let map = connection
                .get_keyboard_mapping(min, connection.setup().max_keycode - min + 1)?
                .reply()?;
            let code = |symbol| {
                map.keysyms
                    .chunks(map.keysyms_per_keycode as usize)
                    .position(|s| s.contains(&symbol))
                    .map(|i| min + i as u8)
                    .unwrap()
            };
            let (control, alt, key) = (code(0xffe3), code(0xffe9), code(0x42));
            let active_atom = connection.intern_atom(false, b"_NET_ACTIVE_WINDOW")?.reply()?.atom;
            let external = if child { 0 } else {
                // A tiny owned window manager accepts EWMH activation requests.
                // It changes focus only for this fixture's verified window IDs.
                connection.change_window_attributes(root, &ChangeWindowAttributesAux::new()
                    .event_mask(EventMask::SUBSTRUCTURE_NOTIFY))?.check()?;
                let id = connection.generate_id()?;
                connection.create_window(x11rb::COPY_DEPTH_FROM_PARENT, id, root,
                    800, 400, 160, 80, 0, WindowClass::INPUT_OUTPUT, 0,
                    &CreateWindowAux::new())?.check()?;
                connection.map_window(id)?.check()?;
                id
            };
            Ok(Self {
                connection,
                root,
                control,
                alt,
                key,
                external,
                active_atom,
            })
        }
        fn focus(&self, window: u32) {
            self.connection.set_input_focus(x11rb::protocol::xproto::InputFocus::PARENT,
                window, x11rb::CURRENT_TIME).unwrap().check().unwrap();
            self.connection.flush().unwrap();
        }
        fn focused(&self) -> u32 { self.connection.get_input_focus().unwrap().reply().unwrap().focus }
        fn activations(&self, owned: &[u32]) {
            while let Some(event) = self.connection.poll_for_event().unwrap() {
                if let x11rb::protocol::Event::ClientMessage(event) = event {
                    if event.type_ == self.active_atom {
                        assert!(owned.contains(&event.window), "activation must target a fixture window");
                        self.focus(event.window);
                    }
                }
            }
        }
        fn press(&self, down: bool) {
            let codes = if down {
                [self.control, self.alt, self.key]
            } else {
                [self.key, self.alt, self.control]
            };
            for code in codes {
                self.connection
                    .xtest_fake_input(if down { 2 } else { 3 }, code, 0, self.root, 0, 0, 0)
                    .unwrap()
                    .check()
                    .unwrap();
            }
            self.connection.flush().unwrap();
        }
    }
    fn xid(window: &winit::window::Window) -> u32 {
        match window.window_handle().unwrap().as_raw() {
            RawWindowHandle::Xlib(handle) => handle.window as u32,
            RawWindowHandle::Xcb(handle) => handle.window.get(),
            _ => panic!("owned X11 window expected"),
        }
    }
    struct App {
        child: bool,
        report: PathBuf,
        stop: PathBuf,
        started: Instant,
        phase: u8,
        at: Instant,
        keys: Keys,
        identity: Vec<String>,
        geometry: Vec<(u32, u32)>,
        last: String,
        hide_ms: u128,
        restore_ms: u128,
        recording_checked: bool,
    }
    impl App {
        fn native(&self, ctx: &egui::Context) -> Option<(Vec<String>, Vec<bool>, Vec<(u32, u32)>)> {
            let ids = if self.child {
                vec![egui::ViewportId::ROOT]
            } else {
                vec![egui::ViewportId::ROOT, normal(), hidden()]
            };
            let native: Option<Vec<_>> = ids
                .into_iter()
                .map(|id| eframe::viewport_window_for(ctx, id))
                .collect();
            let native = native?;
            Some((
                native.iter().map(|w| format!("{:?}", w.id())).collect(),
                native
                    .iter()
                    .map(|w| w.is_visible().unwrap_or(false))
                    .collect(),
                native
                    .iter()
                    .map(|w| {
                        let s = w.inner_size();
                        (s.width, s.height)
                    })
                    .collect(),
            ))
        }
        fn child_state(&self) -> serde_json::Value {
            std::fs::read(&self.report)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default()
        }
    }
    fn continue_probe(ctx: &egui::Context) { ctx.request_repaint_after(Duration::from_millis(10)); }
    impl eframe::App for App {
        fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
            assert!(
                self.started.elapsed() < Duration::from_secs(14),
                "owned boss-key window probe timed out in phase {}",
                self.phase
            );
            boss_key::register(ctx);
            boss_key::apply();
            if !self.child {
                for (id, visible, title) in [
                    (normal(), !boss_key::hidden(), "Owned normal window"),
                    (hidden(), false, "Owned previously hidden window"),
                ] {
                    ctx.show_viewport_deferred(
                        id,
                        egui::ViewportBuilder::default()
                            .with_title(title)
                            .with_inner_size([280., 180.])
                            .with_visible(visible),
                        |ctx, _| {
                            boss_key::register(ctx);
                            boss_key::apply();
                            egui::CentralPanel::default().show(ctx, |ui| {
                                ui.label("Generated native window only");
                            });
                        },
                    );
                }
            }
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label("Generated native window only");
            });
            let Some((identity, visible, geometry)) = self.native(ctx) else {
                ctx.request_repaint();
                return;
            };
            if self.child {
                let state = serde_json::json!({"ready":boss_key::available(),"hidden":boss_key::hidden(),"visible":visible[0],"xid":xid(&eframe::viewport_window(ctx).unwrap())});
                let encoded = state.to_string();
                if self.last != encoded {
                    std::fs::write(&self.report, &encoded).unwrap();
                    self.last = encoded;
                }
                if self.stop.exists() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            } else {
                let mut owned: Vec<_> = [egui::ViewportId::ROOT, normal(), hidden()].iter()
                    .map(|id| xid(&eframe::viewport_window_for(ctx, *id).unwrap())).collect();
                let child_xid = self.child_state()["xid"].as_u64().unwrap_or(0) as u32;
                if child_xid != 0 { owned.push(child_xid); }
                self.keys.activations(&owned);
                match self.phase {
                    0 if boss_key::available()
                        && self.child_state()["ready"] == true
                        && visible == [true, true, false] =>
                    {
                        if !self.recording_checked {
                            boss_key::recording(egui::ViewportId::ROOT, true);
                            self.phase = 9;
                            continue_probe(ctx);
                            return;
                        }
                        self.identity = identity;
                        self.geometry = geometry;
                        // The last Eclipse foreground window belongs to the
                        // other process. Local work then takes foreground.
                        self.keys.focus(child_xid);
                        self.at = Instant::now();
                        self.phase = 11;
                    }
                    1 if visible == [false, false, false]
                        && self.child_state()["visible"] == false =>
                    {
                        self.hide_ms = self.at.elapsed().as_millis();
                        assert!(boss_key::hidden());
                        assert_eq!(self.keys.focused(), self.keys.external);
                        self.phase = 2;
                    }
                    2 if self.at.elapsed() > Duration::from_millis(850) => {
                        assert!(boss_key::hidden(), "holding the hotkey must not repeat");
                        self.keys.press(false);
                        self.at = Instant::now();
                        self.phase = 3;
                    }
                    3 if self.at.elapsed() > Duration::from_millis(100) => {
                        self.keys.press(true);
                        self.keys.press(false);
                        self.at = Instant::now();
                        self.phase = 4;
                    }
                    4 if !boss_key::hidden()
                        && visible == [true, true, false]
                        && self.child_state()["visible"] == true
                        && self.keys.focused() == child_xid =>
                    {
                        self.restore_ms = self.at.elapsed().as_millis();
                        assert_eq!(identity, self.identity);
                        assert_eq!(geometry, self.geometry);
                        std::fs::write(&self.stop, b"stop").unwrap();
                        self.at = Instant::now();
                        self.phase = 5;
                    }
                    5 if self.at.elapsed() > Duration::from_millis(1800)
                        && boss_key::available() =>
                    {
                        self.keys.press(true);
                        self.keys.press(false);
                        self.at = Instant::now();
                        self.phase = 6;
                    }
                    6 if boss_key::hidden() && visible == [false, false, false] => {
                        self.keys.press(true);
                        self.keys.press(false);
                        self.phase = 7;
                    }
                    7 if !boss_key::hidden() && visible == [true, true, false]
                        && owned[..3].contains(&self.keys.focused()) => {
                        assert_eq!(identity, self.identity);
                        assert_eq!(geometry, self.geometry);
                        println!(
                            "{}",
                            serde_json::json!({"window":"passed","processes":2,"windows":4,"hide_ms":self.hide_ms,"restore_ms":self.restore_ms,"repeat_suppressed":true,"prior_hidden_preserved":true,"geometry_and_identity_preserved":true,"owner_exit_takeover":true,"recording_suspension":true,"external_focus_preserved_while_hidden":true,"last_active_process_restored":true,"owned_ewmh_window_manager":true})
                        );
                        self.phase = 8;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    9 if !boss_key::available() && self.child_state()["ready"] == false => {
                        self.keys.press(true); self.keys.press(false);
                        self.at = Instant::now(); self.phase = 10;
                    }
                    10 if self.at.elapsed() > Duration::from_millis(250) => {
                        assert!(!boss_key::hidden());
                        assert_eq!(visible, [true, true, false]);
                        assert_eq!(self.child_state()["visible"], true);
                        boss_key::recording(egui::ViewportId::ROOT, false);
                        self.recording_checked = true; self.phase = 0;
                    }
                    11 if self.at.elapsed() > Duration::from_millis(200) => {
                        assert_eq!(self.keys.focused(), child_xid);
                        self.keys.focus(self.keys.external);
                        self.at = Instant::now(); self.phase = 12;
                    }
                    12 if self.at.elapsed() > Duration::from_millis(150) => {
                        assert_eq!(self.keys.focused(), self.keys.external);
                        self.keys.press(true);
                        self.at = Instant::now(); self.phase = 1;
                    }
                    _ => {}
                }
            }
            boss_key::apply();
            ctx.request_repaint_after(Duration::from_millis(10));
        }
    }
    pub fn run() -> anyhow::Result<()> {
        let args: Vec<_> = std::env::args().collect();
        let display = args
            .windows(2)
            .find(|a| a[0] == "--x11-display")
            .map(|a| a[1].clone())
            .ok_or_else(|| anyhow::anyhow!("--x11-display is required"))?;
        owned_display::check(&display)?;
        let child = args.iter().any(|a| a == "--child");
        let scratch =
            std::env::temp_dir().join(format!("eclipse-boss-probe-{}", uuid::Uuid::new_v4()));
        let mut bus = None;
        let mut process = None;
        let report = if child {
            PathBuf::from(args.windows(2).find(|a| a[0] == "--report").unwrap()[1].clone())
        } else {
            std::fs::create_dir(&scratch)?;
            let mut daemon = Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1", "--nopidfile"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?;
            let mut address = String::new();
            std::io::BufReader::new(daemon.stdout.take().unwrap()).read_line(&mut address)?;
            std::env::set_var("DBUS_SESSION_BUS_ADDRESS", address.trim());
            bus = Some(OwnedChild(daemon));
            let report = scratch.join("child.json");
            process = Some(OwnedChild(
                Command::new(std::env::current_exe()?)
                    .arg("--x11-display")
                    .arg(&display)
                    .arg("--child")
                    .arg("--report")
                    .arg(&report)
                    .stdout(Stdio::null())
                    .spawn()?,
            ));
            let deadline = Instant::now() + Duration::from_secs(6);
            while std::fs::read(&report)
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .is_none_or(|v| v["ready"] != true)
            {
                anyhow::ensure!(
                    Instant::now() < deadline,
                    "child did not register the global shortcut"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            report
        };
        let stop = report.with_extension("stop");
        eframe::run_native(
            "Owned boss-key probe",
            eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default().with_inner_size([320., 220.]),
                ..Default::default()
            },
            Box::new(move |cc| {
                boss_key::register(&cc.egui_ctx);
                Ok(Box::new(App {
                    child,
                    report,
                    stop,
                    started: Instant::now(),
                    phase: 0,
                    at: Instant::now(),
                    keys: Keys::new(child).unwrap(),
                    identity: vec![],
                    geometry: vec![],
                    last: String::new(),
                    hide_ms: 0,
                    restore_ms: 0,
                    recording_checked: false,
                }))
            }),
        )
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        drop(process);
        drop(bus);
        if !child {
            std::fs::remove_dir_all(scratch)?;
        }
        Ok(())
    }
}
#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    probe::run()
}
#[cfg(windows)]
#[path = "boss_key_probe/windows.rs"]
mod windows_probe;
#[cfg(windows)]
fn main() -> anyhow::Result<()> { windows_probe::run() }
#[cfg(not(any(target_os = "linux", windows)))]
fn main() {
    eprintln!("This probe requires an owned Xvfb and private session bus.");
}
