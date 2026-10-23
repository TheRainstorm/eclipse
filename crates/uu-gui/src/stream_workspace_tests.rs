//! Real native windows and encrypted generated streams. No desktop capture,
//! keyboard injection, gamepad, account or system audio is used by this fixture.
use super::*;
#[cfg(windows)]
#[path = "../../../tools/native/windows_owned_station.rs"]
mod owned_station;
use std::{
    path::PathBuf,
    sync::{atomic::Ordering, Mutex},
    time::{Duration, Instant},
};

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn launch(path: PathBuf, name: &str) -> Launch {
    Launch {
        target: StreamTarget {
            connection: ConnectionTarget::NativeInvitation(path),
            alias: name.into(),
            view_only: true,
        },
        code: None,
    }
}
#[test]
fn connection_identity_groups_profiles_independently_of_the_launch_snapshot() {
    let target = |path: &str| StreamTarget {
        connection: ConnectionTarget::NativeProfile {
            id: "generated-profile".into(),
            snapshot: path.into(),
        },
        alias: "Generated".into(),
        view_only: false,
    };
    assert_eq!(target("one.json").key(), target("two.json").key());
    let request = uu_core::assistance::Request::new("12345678", "TEST99").unwrap();
    let launch = Launch::new(
        StreamTarget {
            connection: ConnectionTarget::UuAssistance(request.id.clone()),
            alias: "Generated".into(),
            view_only: true,
        },
        Some(&request),
    )
    .unwrap();
    let decoded: Launch = serde_json::from_slice(&serde_json::to_vec(&launch).unwrap()).unwrap();
    assert_eq!(decoded.assistance().unwrap().unwrap().code(), "TEST99");
}

#[test]
fn device_and_monitor_widgets_route_clicks_and_drags_to_their_session() {
    let context = egui::Context::default();
    crate::setup_style(&context);
    let actions = Rc::new(RefCell::new(Vec::new()));
    let tabs = Tabs {
        groups: vec![
            Group {
                id: 11,
                name: "Generated A".into(),
                screens: vec![(1, "Monitor A".into())],
                selected: Some(1),
            },
            Group {
                id: 22,
                name: "Generated B".into(),
                screens: vec![(2, "Monitor B".into())],
                selected: Some(2),
            },
        ],
        active: 11,
        actions: actions.clone(),
        local_ui: Default::default(),
    };
    let render = |events| {
        context.run(
            egui::RawInput {
                events,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1300., 300.),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::TopBottomPanel::top("groups").show(ctx, |ui| tabs.show(ui));
            },
        )
    };
    fn text(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                Some(text.pos + text.galley.rect.center().to_vec2())
            }
            egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(|shape| text(shape, label)),
            _ => None,
        }
    }
    let output = render(Vec::new());
    let position = |label| {
        output
            .shapes
            .iter()
            .find_map(|shape| text(&shape.shape, label))
            .unwrap_or_else(|| panic!("missing {label}"))
    };
    let a = position("Generated A");
    let b = position("Generated B");
    let monitor = position("Monitor B");
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    render(vec![egui::Event::PointerMoved(a), button(a, true)]);
    render(vec![button(a, false)]);
    assert_eq!(*actions.borrow(), vec![Action::Select(11, None)]);
    actions.borrow_mut().clear();
    render(vec![
        egui::Event::PointerMoved(monitor),
        button(monitor, true),
    ]);
    render(vec![button(monitor, false)]);
    assert_eq!(*actions.borrow(), vec![Action::Select(22, Some(2))]);
    actions.borrow_mut().clear();
    render(vec![egui::Event::PointerMoved(b), button(b, true)]);
    render(vec![egui::Event::PointerMoved(b + egui::vec2(10., 0.))]);
    render(vec![egui::Event::PointerMoved(a)]);
    render(vec![button(a, false)]);
    assert!(actions.borrow().iter().any(|a| matches!(
        a,
        Action::Reorder {
            source: 22,
            target: 11,
            ..
        }
    )));
    actions.borrow_mut().clear();
    render(vec![egui::Event::PointerMoved(b), button(b, true)]);
    render(vec![egui::Event::PointerMoved(b + egui::vec2(10., 0.))]);
    let outside = egui::pos2(-25., 80.);
    render(vec![egui::Event::PointerMoved(outside)]);
    render(vec![button(outside, false)]);
    assert_eq!(*actions.borrow(), vec![Action::Detach(22)]);
}

std::thread_local! {
    static SCREENSHOTS:std::cell::RefCell<Option<std::collections::HashMap<egui::ViewportId,Arc<egui::ColorImage>>>>=const {std::cell::RefCell::new(None)};
}
pub(super) fn observe_screenshot(ctx:&egui::Context) {
    // Viewport input belongs to that viewport's pass. The root must not race
    // its child by polling transient input events after a later child pass.
    let image=ctx.input(|input|input.events.iter().find_map(|event|if let egui::Event::Screenshot{image,..}=event{Some(image.clone())}else{None}));
    if let Some(image)=image {
        SCREENSHOTS.with(|slot|if let Some(images)=&mut *slot.borrow_mut(){images.insert(ctx.viewport_id(),image);});
    }
}
struct Probe {
    workspace: Workspace,
    directory: PathBuf,
    second: Option<Launch>,
    join: Option<std::thread::JoinHandle<anyhow::Result<uu_core::local_workspace::Join>>>,
    phase: u8,
    reported_phase:Option<u8>,
    started: Instant,
    first: u64,
    second_id: u64,
    stats: Vec<Arc<uu_core::media::MediaStats>>,
    epochs: Vec<u64>,
    counts: Vec<u64>,
    passed: Arc<Mutex<bool>>,
}
impl eframe::App for Probe {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if self.reported_phase!=Some(self.phase) {
            eprintln!("generated workspace window probe phase={}",self.phase);
            self.reported_phase=Some(self.phase);
        }
        assert!(
            self.started.elapsed() < Duration::from_secs(40),
            "workspace probe timed out in phase {}",
            self.phase
        );
        eframe::App::update(&mut self.workspace, ctx, frame);
        STATE.with(|slot| {
            let mut state = slot.borrow_mut();
            let state = state.as_mut().unwrap();
            let decoded = |s: &Session| {
                s.app
                    .controller
                    .as_ref()
                    .map_or(0, |h| h.stats.decoded_total.load(Ordering::Acquire))
            };
            match self.phase {
                0 if state.windows[0]
                    .sessions
                    .first()
                    .is_some_and(|s| decoded(s) >= 8) =>
                {
                    let bytes = zeroize::Zeroizing::new(
                        serde_json::to_vec(&self.second.take().unwrap()).unwrap(),
                    );
                    let directory = self.directory.clone();
                    self.join = Some(std::thread::spawn(move || {
                        uu_core::local_workspace::join(&directory, &bytes)
                    }));
                    self.phase = 1;
                }
                1 if state.windows[0].sessions.len() == 2
                    && state.windows[0].sessions.iter().all(|s| decoded(s) >= 8) =>
                {
                    assert_eq!(eframe::viewport_window(ctx).unwrap().is_visible(),Some(false),"coordinator must remain hidden");
                    assert!(matches!(
                        self.join.take().unwrap().join().unwrap().unwrap(),
                        uu_core::local_workspace::Join::Attached
                    ));
                    let window = &state.windows[0];
                    self.first = window.sessions[0].id;
                    self.second_id = window.sessions[1].id;
                    assert!(!window.sessions[0].app.active_session);
                    assert!(window.sessions[1].app.active_session);
                    assert!(Rc::ptr_eq(&window.sessions[0].app.gamepad,&window.sessions[1].app.gamepad));
                    self.stats = window
                        .sessions
                        .iter()
                        .map(|s| s.app.controller.as_ref().unwrap().stats.clone())
                        .collect();
                    self.epochs = self
                        .stats
                        .iter()
                        .map(|s| s.native_media_epoch.load(Ordering::Acquire))
                        .collect();
                    state.apply(
                        Action::Reorder {
                            source: self.second_id,
                            target: self.first,
                            after: false,
                        },
                        ctx,
                    );
                    assert_eq!(state.windows[0].sessions[0].id, self.second_id);
                    state.apply(Action::Select(self.first, None), ctx);
                    let first = state.windows[0]
                        .sessions
                        .iter_mut()
                        .find(|s| s.id == self.first)
                        .unwrap();
                    first.app.preferences.fps = 24;
                    first.app.target_fps = 24;
                    first.app.send_capture_settings();
                    self.phase = 2;
                }
                2 if self.stats[0]
                    .native_quality_current
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|q| q.fps == 24) =>
                {
                    assert_eq!(
                        self.stats[1]
                            .native_quality_current
                            .lock()
                            .unwrap()
                            .as_ref()
                            .unwrap()
                            .fps,
                        60
                    );
                    self.counts = self
                        .stats
                        .iter()
                        .map(|s| s.decoded_total.load(Ordering::Acquire))
                        .collect();
                    ctx.send_viewport_cmd_to(state.windows[0].viewport(),egui::ViewportCommand::Screenshot);
                    self.phase = 6;
                }
                6=>{
                    let image=SCREENSHOTS.with(|slot|slot.borrow_mut().as_mut().unwrap().remove(&state.windows[0].viewport()));
                    if let Some(image)=image {
                        assert!(image.size[0]>300&&image.size[1]>200);
                        if let Some(output)=std::env::var_os("ECLIPSE_WORKSPACE_PROBE_OUTPUT") {
                            let pixels=image.pixels.iter().flat_map(|pixel|pixel.to_array()).collect();
                            image::RgbaImage::from_raw(image.size[0] as u32,image.size[1] as u32,pixels).unwrap().save(PathBuf::from(output)).unwrap();
                        }
                        state.apply(Action::Detach(self.first),ctx);assert_eq!(state.windows.len(),2);
                        assert!(!Rc::ptr_eq(&state.windows[0].sizing,&state.windows[1].sizing));
                        self.phase=3;
                    }
                },
                3 if state.windows.len() == 2
                    && state.windows[1].platform.is_some()
                    && self
                        .stats
                        .iter()
                        .zip(&self.counts)
                        .all(|(s, old)| s.decoded_total.load(Ordering::Acquire) > old + 8) =>
                {
                    for (index, id) in [self.first, self.second_id].into_iter().enumerate() {
                        let (window, session) = state
                            .windows
                            .iter()
                            .find_map(|w| w.sessions.iter().find(|s| s.id == id).map(|s| (w, s)))
                            .unwrap();
                        assert!(Arc::ptr_eq(
                            &self.stats[index],
                            &session.app.controller.as_ref().unwrap().stats
                        ));
                        assert_eq!(
                            self.stats[index].native_media_epoch.load(Ordering::Acquire),
                            self.epochs[index]
                        );
                        assert_eq!(session.app.context.viewport(), window.viewport());
                        assert_eq!(self.stats[index].errors.load(Ordering::Acquire), 0);
                        assert!(!session.app.textures.is_empty());
                    }
                    // Closing one native window's session leaves the other
                    // receiving on the exact same media epoch and controller.
                    state.apply(Action::Close(self.first), ctx);
                    self.counts[1] = self.stats[1].decoded_total.load(Ordering::Acquire);
                    self.phase = 4;
                }
                4 if state.windows.len() == 1
                    && self.stats[1].decoded_total.load(Ordering::Acquire) > self.counts[1] + 8 =>
                {
                    assert_eq!(state.windows[0].sessions[0].id, self.second_id);
                    assert_eq!(
                        self.stats[1].native_media_epoch.load(Ordering::Acquire),
                        self.epochs[1]
                    );
                    state.apply(Action::Close(self.second_id), ctx);
                    self.phase = 5;
                    *self.passed.lock().unwrap() = true;
                    println!("two live sessions: attach, tab selection, independent quality, group reorder, native detach and isolated close; epochs unchanged, decode errors=0");
                }
                _ => {}
            }
        });
        ctx.request_repaint_after(Duration::from_millis(10));
    }
    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        eframe::App::on_exit(&mut self.workspace, gl);
    }
}

#[test]
#[ignore = "run alone: private XDG directories and an isolated Xvfb or newly owned Windows station"]
fn native_windows_attach_reorder_and_detach_two_live_generated_sessions() {
    eprintln!("generated workspace probe starting");
    SCREENSHOTS.with(|slot|*slot.borrow_mut()=Some(Default::default()));
    uu_core::install_crypto_provider();
    let directory = Directory(std::env::temp_dir().join(format!(
        "eclipse-workspace-window-test-{}",
        uuid::Uuid::new_v4()
    )));
    std::fs::create_dir(&directory.0).unwrap();
    std::env::set_var("XDG_DATA_HOME", directory.0.join("data"));
    std::env::set_var("XDG_STATE_HOME", directory.0.join("state"));
    #[cfg(windows)]
    let _station = owned_station::Station::create_with_keyboard_hook().unwrap();
    let mut preferences = crate::settings::Settings::default();
    preferences.software_decoder = true;
    preferences
        .save(&uu_core::Config::from_env().unwrap().at("gui-settings.json"))
        .unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(3)
        .enable_all()
        .build()
        .unwrap();
    let mut hosts = Vec::new();
    let mut stops = Vec::new();
    let mut invitations = Vec::new();
    for index in 0..2 {
        let invitation = directory.0.join(format!("generated-{index}.json"));
        let config = uu_core::native::HostConfig {
            encoder_tuning:Default::default(),
            session: None,
            video_policy: Some(uu_core::native_quality::Policy {
                codecs: vec![uu_core::native::VideoCodec::H264],
                preferred_codec: None,
            }),
            codec: Default::default(),
            rate_control: Default::default(),
            bind: "127.0.0.1:0".parse().unwrap(),
            source: uu_core::native::CaptureSource::Synthetic {
                width: 320 + index * 64,
                height: 192,
                fps: 60,
            },
            invitation_file: invitation.clone(),
            invitation_lifetime: Duration::from_secs(60),
            fps: 60,
            bitrate_kbps: 2000,
            encoder: uu_core::native::EncoderPreference::Software,
            session_limit: Some(Duration::from_secs(45)),
            allow_input: false,
            audio: None,
            clipboard: None,
        };
        let (stop, rx) = tokio::sync::watch::channel(false);
        stops.push(stop);
        hosts.push(runtime.spawn(uu_core::native::serve(config, rx)));
        invitations.push(invitation);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !invitations.iter().all(|p| p.exists()) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let first = launch(invitations[0].clone(), "Generated A");
    let second = launch(invitations[1].clone(), "Generated B");
    let ipc = directory.0.join("ipc");
    let owner = match uu_core::local_workspace::join(&ipc, b"initial").unwrap() {
        uu_core::local_workspace::Join::Owner(owner) => owner,
        _ => panic!("first owner"),
    };
    let passed = Arc::new(Mutex::new(false));
    let result = passed.clone();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_visible(false),
        vsync: false,
        event_loop_builder: Some(Box::new(|builder| {
            #[cfg(windows)]
            use winit::platform::windows::EventLoopBuilderExtWindows;
            #[cfg(target_os = "linux")]
            use winit::platform::x11::EventLoopBuilderExtX11;
            builder.with_any_thread(true);
        })),
        ..Default::default()
    };
    eframe::run_native(
        "Generated workspace probe",
        options,
        Box::new(move |cc| {
            eprintln!("generated workspace GL ready; creating coordinator");
            let workspace=Workspace::new(cc,first,owner,false,None);
            eprintln!("generated workspace coordinator ready");
            Ok(Box::new(Probe {
                workspace,
                directory: ipc,
                second: Some(second),
                join: None,
                phase: 0,
                reported_phase:None,
                started: Instant::now(),
                first: 0,
                second_id: 0,
                stats: Vec::new(),
                epochs: Vec::new(),
                counts: Vec::new(),
                passed: result,
            }))
        }),
    )
    .unwrap();
    for stop in stops {
        let _ = stop.send(true);
    }
    runtime.block_on(async {
        for host in hosts {
            host.await.unwrap().unwrap();
        }
    });
    assert!(
        *passed.lock().unwrap(),
        "all native window transitions must complete"
    );
}
