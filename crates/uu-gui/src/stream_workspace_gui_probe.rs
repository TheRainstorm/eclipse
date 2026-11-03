//! Explicit, bounded GUI exercise against caller supplied private invitations.
//! Mouse messages pass through the owned HWND, winit, egui and normal controls.
use super::*;

#[cfg(windows)]
thread_local! {
    static REGIONS: RefCell<Option<std::collections::HashMap<egui::ViewportId, (crate::stream_tabs_ui::StripRects, f32)>>> = const { RefCell::new(None) };
}
pub(super) fn observe_tab_rects(_ctx: &egui::Context, _rects: &crate::stream_tabs_ui::StripRects) {
    #[cfg(windows)]
    REGIONS.with(|slot| {
        if let Some(regions) = slot.borrow_mut().as_mut() {
            regions.insert(
                _ctx.viewport_id(),
                (_rects.clone(), _ctx.pixels_per_point()),
            );
        }
    });
}

#[cfg(windows)]
#[path = "../../../tools/native/windows_owned_station.rs"]
mod owned_station;

#[cfg(windows)]
mod native {
    use super::*;
    use std::{
        path::PathBuf,
        sync::{
            atomic::{AtomicBool, Ordering},
            Mutex,
        },
        time::{Duration, Instant},
    };
    use windows_sys::Win32::{
        Foundation::RECT, System::Threading::GetCurrentProcessId, UI::WindowsAndMessaging::*,
    };

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Config {
        root: PathBuf,
        invitations: [PathBuf; 2],
    }
    struct Held {
        hwnd: isize,
        point: isize,
        right: bool,
        due: Instant,
    }
    struct Probe {
        workspace: Workspace,
        ipc: PathBuf,
        second: Option<Launch>,
        join: Option<std::thread::JoinHandle<anyhow::Result<uu_core::local_workspace::Join>>>,
        held: Option<Held>,
        phase: u8,
        started: Instant,
        ids: [u64; 2],
        stats: Vec<Arc<uu_core::media::MediaStats>>,
        stops: Vec<Arc<AtomicBool>>,
        epochs: Vec<u64>,
        counts: Vec<u64>,
        records: Arc<Mutex<Vec<serde_json::Value>>>,
    }
    impl Probe {
        fn record(&self, event: &str) {
            self.records.lock().unwrap().push(serde_json::json!({
                "event":event,"phase":self.phase,"elapsed_ms":self.started.elapsed().as_millis(),
                "media":self.stats.iter().map(|s|serde_json::json!({
                    "decoded":s.decoded_total.load(Ordering::Acquire),
                    "errors":s.errors.load(Ordering::Acquire),
                    "epoch":s.native_media_epoch.load(Ordering::Acquire),
                    "input_sent":s.input_messages_sent.load(Ordering::Acquire),
                    "audio_packets":s.audio.packets.load(Ordering::Acquire),
                    "audio_output_enabled":s.audio.output_enabled.load(Ordering::Acquire)
                })).collect::<Vec<_>>()
            }));
        }
        fn click(&mut self, state: &State, session: u64, kind: &str, right: bool) -> bool {
            if self.held.is_some() {
                return false;
            }
            let Some(window) = state
                .windows
                .iter()
                .find(|w| w.sessions.iter().any(|s| s.id == session))
            else {
                return false;
            };
            let Some(platform) = window.platform.as_ref() else {
                return false;
            };
            let Some((rects, scale)) =
                REGIONS.with(|slot| slot.borrow().as_ref()?.get(&window.viewport()).cloned())
            else {
                return false;
            };
            let collection = match kind {
                "device" => &rects.devices,
                "detach" => &rects.detach,
                "close" => &rects.close,
                _ => panic!("unknown control"),
            };
            let Some((_, rect)) = collection.iter().find(|(id, _)| *id == session) else {
                return false;
            };
            let point = rect.center() * scale;
            let hwnd = platform.window_id();
            let mut owner = 0;
            let mut client = RECT::default();
            unsafe {
                assert_ne!(GetWindowThreadProcessId(hwnd as _, &mut owner), 0);
                assert_eq!(
                    owner,
                    GetCurrentProcessId(),
                    "mouse target must belong to this process"
                );
                assert_ne!(GetClientRect(hwnd as _, &mut client), 0);
                let x = point.x.round() as i32;
                let y = point.y.round() as i32;
                assert!(
                    x >= 0 && y >= 0 && x < client.right && y < client.bottom,
                    "control outside own client area"
                );
                let lparam = ((y as u32) << 16 | (x as u32 & 0xffff)) as isize;
                assert_ne!(PostMessageW(hwnd as _, WM_MOUSEMOVE, 0, lparam), 0);
                let (message, button) = if right {
                    (WM_RBUTTONDOWN, 2)
                } else {
                    (WM_LBUTTONDOWN, 1)
                };
                assert_ne!(PostMessageW(hwnd as _, message, button, lparam), 0);
                self.held = Some(Held {
                    hwnd,
                    point: lparam,
                    right,
                    due: Instant::now() + Duration::from_millis(90),
                });
            }
            self.records.lock().unwrap().push(serde_json::json!({"event":"native_mouse_down","control":kind,"session":session,"hwnd":hwnd,"right":right}));
            true
        }
    }
    impl eframe::App for Probe {
        fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
            assert!(
                self.started.elapsed() < Duration::from_secs(100),
                "remote GUI probe timeout phase {}",
                self.phase
            );
            if self
                .held
                .as_ref()
                .is_some_and(|held| Instant::now() >= held.due)
            {
                let held = self.held.take().unwrap();
                let mut owner = 0;
                unsafe {
                    assert_ne!(GetWindowThreadProcessId(held.hwnd as _, &mut owner), 0);
                    assert_eq!(owner, GetCurrentProcessId());
                    assert_ne!(
                        PostMessageW(
                            held.hwnd as _,
                            if held.right {
                                WM_RBUTTONUP
                            } else {
                                WM_LBUTTONUP
                            },
                            0,
                            held.point
                        ),
                        0
                    );
                }
            }
            eframe::App::update(&mut self.workspace, ctx, frame);
            STATE.with(|slot| {
                let state = slot.borrow();
                let state = state.as_ref().unwrap();
                // This is receive-only fixture policy, never a workspace action.
                for s in state.windows.iter().flat_map(|w| &w.sessions) {
                    if let Some(handle) = &s.app.controller {
                        handle
                            .stats
                            .audio
                            .output_enabled
                            .store(false, Ordering::Release);
                        assert_eq!(handle.stats.input_messages_sent.load(Ordering::Acquire), 0);
                    }
                }
                let sessions = state
                    .windows
                    .iter()
                    .flat_map(|w| &w.sessions)
                    .collect::<Vec<_>>();
                let ready = |s: &&Session| {
                    s.app
                        .controller
                        .as_ref()
                        .is_some_and(|h| h.stats.decoded_total.load(Ordering::Acquire) >= 12)
                };
                match self.phase {
                    0 if sessions.len() == 1 && sessions.iter().all(ready) => {
                        self.ids[0] = sessions[0].id;
                        let bytes = serde_json::to_vec(&self.second.take().unwrap()).unwrap();
                        let directory = self.ipc.clone();
                        self.join = Some(std::thread::spawn(move || {
                            uu_core::local_workspace::join(&directory, &bytes)
                        }));
                        self.phase = 1;
                    }
                    1 if sessions.len() == 2 && sessions.iter().all(ready) => {
                        assert!(matches!(
                            self.join.take().unwrap().join().unwrap().unwrap(),
                            uu_core::local_workspace::Join::Attached
                        ));
                        self.ids[1] = sessions.iter().find(|s| s.id != self.ids[0]).unwrap().id;
                        for id in self.ids {
                            let s = sessions.iter().find(|s| s.id == id).unwrap();
                            let h = s.app.controller.as_ref().unwrap();
                            self.stats.push(h.stats.clone());
                            self.stops.push(h.stopped.clone());
                            self.epochs
                                .push(h.stats.native_media_epoch.load(Ordering::Acquire));
                            self.counts
                                .push(h.stats.decoded_total.load(Ordering::Acquire));
                        }
                        self.record("two_actual_sessions_ready");
                        self.phase = 10;
                    }
                    10 => {
                        if self.click(state, self.ids[1], "device", true) {
                            self.phase = 2;
                        }
                    }
                    2 if self.held.is_none() => {
                        if self.click(state, self.ids[1], "detach", false) {
                            self.phase = 3;
                        }
                    }
                    3 if self.held.is_none()
                        && state.windows.len() == 2
                        && state.windows.iter().all(|w| w.platform.is_some())
                        && self
                            .stats
                            .iter()
                            .zip(&self.counts)
                            .all(|(s, n)| s.decoded_total.load(Ordering::Acquire) > n + 12) =>
                    {
                        for (index, id) in self.ids.iter().enumerate() {
                            let (w, s) = state
                                .windows
                                .iter()
                                .find_map(|w| {
                                    w.sessions.iter().find(|s| s.id == *id).map(|s| (w, s))
                                })
                                .unwrap();
                            assert!(Arc::ptr_eq(
                                &self.stats[index],
                                &s.app.controller.as_ref().unwrap().stats
                            ));
                            assert_eq!(
                                self.stats[index].native_media_epoch.load(Ordering::Acquire),
                                self.epochs[index]
                            );
                            assert_eq!(self.stats[index].errors.load(Ordering::Acquire), 0);
                            assert!(!s.app.textures.is_empty());
                            assert_eq!(s.app.context.viewport(), w.viewport());
                        }
                        self.record("native_detach_both_video_continued");
                        self.counts[1] = self.stats[1].decoded_total.load(Ordering::Acquire);
                        if self.click(state, self.ids[0], "close", false) {
                            self.phase = 4;
                        }
                    }
                    4 if self.held.is_none()
                        && !sessions.iter().any(|s| s.id == self.ids[0])
                        && self.stops[0].load(Ordering::Acquire)
                        && self.stats[1].decoded_total.load(Ordering::Acquire)
                            > self.counts[1] + 12 =>
                    {
                        assert_eq!(self.stats[1].errors.load(Ordering::Acquire), 0);
                        assert_eq!(
                            self.stats[1].native_media_epoch.load(Ordering::Acquire),
                            self.epochs[1]
                        );
                        self.record("native_connection_close_survivor_continued");
                        if self.click(state, self.ids[1], "close", false) {
                            self.phase = 5;
                        }
                    }
                    5 if sessions.is_empty() => {
                        self.phase = 6;
                        self.record("all_owned_connections_closed");
                    }
                    _ => {}
                }
            });
            ctx.request_repaint_after(Duration::from_millis(10));
        }
        fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
            self.workspace.on_exit(gl);
        }
    }

    #[test]
    #[ignore = "explicit private invitation config; owns a fresh noninteractive Windows station"]
    fn owned_remote_native_mouse_detach_and_close() {
        let cfg: Config = serde_json::from_slice(
            &std::fs::read(
                std::env::var_os("ECLIPSE_GUI_REMOTE_PROBE_CONFIG")
                    .expect("explicit config path required"),
            )
            .unwrap(),
        )
        .unwrap();
        let root = cfg.root.canonicalize().unwrap();
        let invitations = cfg.invitations.map(|p| p.canonicalize().unwrap());
        assert_ne!(invitations[0], invitations[1]);
        assert!(invitations.iter().all(|p| p.starts_with(&root)));
        let private = root.join("gui-owned-data");
        std::fs::create_dir(&private).unwrap();
        std::env::set_var("XDG_DATA_HOME", &private);
        std::env::set_var("XDG_STATE_HOME", private.join("state"));
        uu_core::install_crypto_provider();
        let _station = owned_station::Station::create_with_keyboard_hook().unwrap();
        let mut preferences = crate::settings::Settings::default();
        preferences.software_decoder = false;
        preferences.muted = true;
        preferences.volume = 0;
        preferences
            .save(&uu_core::Config::from_env().unwrap().at("gui-settings.json"))
            .unwrap();
        REGIONS.with(|slot| *slot.borrow_mut() = Some(Default::default()));
        let launch = |path, alias: &str| {
            Launch::new(
                StreamTarget {
                    connection: ConnectionTarget::NativeInvitation(path),
                    alias: alias.into(),
                    view_only: true,
                },
                None,
            )
            .unwrap()
        };
        let first = launch(invitations[0].clone(), "Owned connection one");
        let second = launch(invitations[1].clone(), "Owned connection two");
        let ipc = root.join("gui-owned-ipc");
        let owner = match uu_core::local_workspace::join(&ipc, b"initial").unwrap() {
            uu_core::local_workspace::Join::Owner(owner) => owner,
            _ => panic!("private namespace already in use"),
        };
        let records = Arc::new(Mutex::new(Vec::new()));
        let events = records.clone();
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_visible(false),
            vsync: false,
            event_loop_builder: Some(Box::new(|builder| {
                use winit::platform::windows::EventLoopBuilderExtWindows;
                builder.with_any_thread(true);
            })),
            ..Default::default()
        };
        let outcome = eframe::run_native(
            "Owned remote GUI probe",
            options,
            Box::new(move |cc| {
                Ok(Box::new(Probe {
                    workspace: Workspace::new(cc, first, owner, false, None),
                    ipc,
                    second: Some(second),
                    join: None,
                    held: None,
                    phase: 0,
                    started: Instant::now(),
                    ids: [0, 0],
                    stats: vec![],
                    stops: vec![],
                    epochs: vec![],
                    counts: vec![],
                    records: events,
                }))
            }),
        );
        let rows = records.lock().unwrap();
        let passed = outcome.is_ok()
            && rows
                .iter()
                .any(|r| r["event"] == "all_owned_connections_closed");
        std::fs::write(root.join("gui-result.json"),serde_json::to_vec_pretty(&serde_json::json!({"ok":passed,"events":*rows,"eframe_error":outcome.err().map(|e|e.to_string()),"mouse_path":"PostMessageW -> winit -> egui -> production controls","created_virtual_display":false,"audio_output":"disabled in fixture; negotiated packets reported","stored_pixels":false})).unwrap()).unwrap();
        assert!(passed, "GUI lifecycle did not complete");
        REGIONS.with(|slot| *slot.borrow_mut() = None);
    }
}
