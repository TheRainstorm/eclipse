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
        payload: None,
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
    let launch = Launch::new(
        StreamTarget {
            connection: ConnectionTarget::Plugin{plugin:"example-addon".into(),identity:"generated-target".into()},
            alias: "Generated".into(),
            view_only: true,
        },
        Some("{\"credential\":\"generated-secret\"}"),
    )
    .unwrap();
    let decoded: Launch = serde_json::from_slice(&serde_json::to_vec(&launch).unwrap()).unwrap();
    assert_eq!(&*decoded.plugin_payload().unwrap().unwrap(), "{\"credential\":\"generated-secret\"}");
}

fn generated_screen(id:i32)->uu_core::screen_control::RemoteScreen {
    uu_core::screen_control::RemoteScreen{id,display_name:format!("Generated source {id}"),refresh_hz:60,kind:0,width:320,height:192,dpi_scale:None,primary:id==0,video_track_index:if id==0{0}else{-1}}
}
#[test]
fn display_launch_preserves_base_identity_source_alias_and_read_only_mode() {
    let base=ConnectionTarget::NativeDevice("01234567-89ab-4cde-8fab-0123456789ab".into());
    for view_only in [false,true] {
        let target=StreamTarget{connection:base.clone(),alias:"Generated computer".into(),view_only};
        let launch=display_launch(&target,&generated_screen(1)).unwrap();
        assert_eq!(launch.target.connection.base(),&base);
        assert_eq!(launch.target.connection.display(),Some(&DisplayTarget{id:1,source:"Generated source 1".into()}));
        assert_eq!(launch.target.alias,target.alias);assert_eq!(launch.target.view_only,view_only);
        assert!(launch.plugin_payload().unwrap().is_none());
        let another=display_launch(&launch.target,&generated_screen(2)).unwrap();
        assert_eq!(another.target.connection.base(),&base);
        assert_eq!(another.target.connection.display().unwrap().id,2);
        assert!(another.target.connection.validate().is_ok(),"opening another screen must not nest wrappers");
        assert_ne!(another.target.key(),launch.target.key());
        assert_eq!(base_key(&another.target),target.key());
    }
    let loopback=StreamTarget{connection:ConnectionTarget::NativeLoopback,alias:"Generated local test".into(),view_only:true};
    assert!(display_launch(&loopback,&generated_screen(0)).unwrap().target.view_only);
}
#[test]
fn independent_display_launch_and_ipc_reject_plugins_invalid_sources_and_nested_wrappers() {
    let plugin=StreamTarget{connection:ConnectionTarget::Plugin{plugin:"example-addon".into(),identity:"generated".into()},alias:"Generated".into(),view_only:true};
    assert!(display_launch(&plugin,&generated_screen(0)).is_err());
    let wrapper=|connection,source|ConnectionTarget::NativeDisplay{connection:Box::new(connection),display:DisplayTarget{id:0,source}};
    for connection in [
        wrapper(plugin.connection.clone(),String::new()),
        wrapper(wrapper(ConnectionTarget::NativeLoopback,"Inner".into()),"Outer".into()),
        wrapper(ConnectionTarget::NativeLoopback,"A".repeat(257)),
        wrapper(ConnectionTarget::NativeLoopback,"Bad\nsource".into()),
    ] {
        let target=StreamTarget{connection,alias:"Generated".into(),view_only:true};
        assert!(Launch::new(target.clone(),None).is_err());
        // IPC must validate deserialized targets, not just command-line parses.
        let bytes=serde_json::to_vec(&Launch{target,payload:None}).unwrap();
        let decoded:Launch=serde_json::from_slice(&bytes).unwrap();
        assert!(decoded.plugin_payload().is_err());
    }
}
#[test]
fn independent_display_ipc_creates_separate_pending_windows_and_reuses_a_stable_source() {
    let mut state=State{input_owner:None,gamepad:Default::default(),windows:vec![Window::new(1,Vec::new(),None)],gl:None,vsync:false,next:2,actions:Default::default()};
    let target=StreamTarget{connection:ConnectionTarget::NativeDevice("01234567-89ab-4cde-8fab-0123456789ab".into()),alias:"Generated computer".into(),view_only:true};
    assert!(state.attach(display_launch(&target,&generated_screen(1)).unwrap()));
    assert!(state.attach(display_launch(&target,&generated_screen(2)).unwrap()));
    assert_eq!(state.windows.len(),3);
    assert!(state.windows[0].pending.is_empty());
    assert_ne!(state.windows[1].viewport(),state.windows[2].viewport());
    let first_key=state.windows[1].pending[0].target.key();
    state.windows[1].focus=false;state.windows[2].focus=false;
    let mut renumbered=generated_screen(1);renumbered.id=42;
    assert!(state.attach(display_launch(&target,&renumbered).unwrap()));
    assert_eq!(state.windows.len(),3);assert!(state.windows[1].focus);assert!(!state.windows[2].focus);
    assert_eq!(state.windows[1].pending.len(),1);assert_eq!(state.windows[1].pending[0].target.key(),first_key);
}

fn check_display_window_routing(state:&mut State,session:u64,ctx:&egui::Context) {
    // Synthetic capture has no monitor catalog. This temporary inventory tests
    // workspace routing only; pending display launches are removed before any
    // UI pass can start them. Actual media below remains two generated hosts.
    let source=state.windows.iter_mut().flat_map(|w|&mut w.sessions).find(|s|s.id==session).unwrap();
    let original_selected=source.app.displays.selected;
    source.app.displays.selected=Some(0);
    let source_target=source.app.stream_target.clone();
    let stats=source.app.controller.as_ref().unwrap().stats.clone();
    let original_catalog=std::mem::replace(&mut *stats.remote_screens.lock().unwrap(),uu_core::screen_control::RemoteScreens{current_id:0,screens:(0..3).map(generated_screen).collect()});
    let original_capture=stats.desktop_capture_source.lock().unwrap().take();
    // A same-device tab whose catalog is not ready must not match a missing
    // resolution and selected=None as if it already displayed this source.
    let sibling=state.windows.iter_mut().flat_map(|w|&mut w.sessions).find(|s|s.id!=session).unwrap();
    let sibling_id=sibling.id;
    let sibling_target=std::mem::replace(&mut sibling.app.stream_target,source_target.clone());
    let sibling_selected=sibling.app.displays.selected.take();
    let sibling_stats=sibling.app.controller.as_ref().unwrap().stats.clone();
    let sibling_catalog=std::mem::take(&mut *sibling_stats.remote_screens.lock().unwrap());
    let sibling_capture=sibling_stats.desktop_capture_source.lock().unwrap().take();
    let original_windows=state.windows.len();
    state.open_display(session,1,ctx);
    state.open_display(session,2,ctx);
    assert_eq!(state.windows.len(),original_windows+2);
    let first=state.windows[original_windows].id;
    let second=state.windows[original_windows+1].id;
    for (index,id) in [(original_windows,1),(original_windows+1,2)] {
        let window=&state.windows[index];
        assert!(window.sessions.is_empty());assert_eq!(window.pending.len(),1);
        assert_eq!(window.pending[0].target.connection.display().unwrap().id,id);
    }
    state.windows[original_windows].focus=false;
    state.windows[original_windows+1].focus=false;
    state.open_display(session,1,ctx);
    assert_eq!(state.windows.len(),original_windows+2);
    assert!(state.windows[original_windows].focus);
    assert!(!state.windows[original_windows+1].focus);
    // A completed window can change from its original source A to B. Its old
    // launch key must not steal a subsequent request for source A.
    let sibling=state.windows.iter_mut().flat_map(|w|&mut w.sessions).find(|s|s.id==sibling_id).unwrap();
    sibling.app.stream_target=Some(display_launch(source_target.as_ref().unwrap(),&generated_screen(1)).unwrap().target);
    sibling.app.displays.selected=Some(2);
    *sibling_stats.remote_screens.lock().unwrap()=uu_core::screen_control::RemoteScreens{current_id:2,screens:(0..3).map(|id| {
        let mut screen=generated_screen(id);screen.video_track_index=if id==2{0}else{-1};screen
    }).collect()};
    state.windows[original_windows].focus=false;
    let active_before=state.windows[0].active;
    assert!(state.attach(display_launch(source_target.as_ref().unwrap(),&generated_screen(1)).unwrap()));
    assert_eq!(state.windows.len(),original_windows+2);
    assert_eq!(state.windows[0].active,active_before);
    assert!(state.windows[original_windows].focus,"request A must focus pending A, not the live launch-A window displaying B");
    state.windows[0].focus=false;
    assert!(state.attach(display_launch(source_target.as_ref().unwrap(),&generated_screen(2)).unwrap()));
    assert_eq!(state.windows.len(),original_windows+2);
    assert!(state.windows[0].focus,"an already confirmed source B is reused regardless of its original launch key");
    state.windows.retain(|window|window.id!=first&&window.id!=second);
    assert_eq!(state.windows.len(),original_windows);
    *stats.remote_screens.lock().unwrap()=original_catalog;
    *stats.desktop_capture_source.lock().unwrap()=original_capture;
    state.windows.iter_mut().flat_map(|w|&mut w.sessions).find(|s|s.id==session).unwrap().app.displays.selected=original_selected;
    let sibling=state.windows.iter_mut().flat_map(|w|&mut w.sessions).find(|s|s.id==sibling_id).unwrap();
    sibling.app.stream_target=sibling_target;sibling.app.displays.selected=sibling_selected;
    *sibling_stats.remote_screens.lock().unwrap()=sibling_catalog;
    *sibling_stats.desktop_capture_source.lock().unwrap()=sibling_capture;
}

#[test]
fn fullscreen_navigation_follows_tab_order_and_wraps_without_crossing_windows() {
    use uu_core::hotkeys::Action as Command;
    let actions=Rc::new(RefCell::new(Vec::new()));
    let mut tabs=Tabs{groups:vec![Group{id:22,name:"B".into(),screens:vec![(7,"Second".into()),(2,"First".into()),(9,"Third".into())],selected:Some(7),can_open_displays:true,can_create_display:false},
        Group{id:11,name:"A".into(),screens:vec![],selected:None,can_open_displays:false,can_create_display:false}],active:22,actions:actions.clone(),local_ui:Default::default()};
    for command in [Command::NextDisplay,Command::PreviousDisplay,Command::NextComputer,Command::PreviousComputer] {tabs.navigate(command);}
    assert_eq!(*actions.borrow(),[Action::Select(22,Some(2)),Action::Select(22,Some(9)),Action::Select(11,None),Action::Select(11,None)]);
    actions.borrow_mut().clear();tabs.active=11;
    tabs.navigate(Command::NextDisplay);tabs.navigate(Command::NextComputer);
    assert_eq!(*actions.borrow(),[Action::Select(22,None)]);
    tabs.groups.pop();tabs.active=22;actions.borrow_mut().clear();tabs.navigate(Command::NextComputer);
    assert!(actions.borrow().is_empty());
}

#[test]
fn device_selector_requires_an_explicit_open_and_keeps_the_video_area() {
    let ctx=egui::Context::default();crate::setup_style(&ctx);
    let mut tabs=Tabs{groups:vec![Group{id:1,name:"A".into(),screens:vec![],selected:None,can_open_displays:false,can_create_display:false},Group{id:2,name:"B".into(),screens:vec![],selected:None,can_open_displays:false,can_create_display:false}],active:1,actions:Default::default(),local_ui:Default::default()};
    let screen=egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1280.,720.));
    let draw=|tabs:&Tabs,pointer| {
        let mut visible=false;
        let _=ctx.run(egui::RawInput{screen_rect:Some(screen),focused:true,
            events:vec![egui::Event::PointerMoved(pointer)],..Default::default()},|ctx| {
                visible=tabs.show_selector(ctx).is_some();
                assert_eq!(ctx.available_rect(),screen,"selector must not reserve a top panel");
            });
        visible
    };
    for _ in 0..3 {assert!(!draw(&tabs,egui::pos2(600.,3.)));}
    tabs.open_selector(&ctx);
    assert!(draw(&tabs,egui::pos2(600.,360.)));
    assert!(tabs.owns_input(&ctx));
    tabs.active=2;
    assert!(!draw(&tabs,egui::pos2(600.,3.)),"switching computers closes the previous selector");
    assert!(!tabs.owns_input(&ctx));
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
                can_open_displays: true,
                can_create_display: false,
            },
            Group {
                id: 22,
                name: "Generated B".into(),
                screens: vec![(2, "Monitor B".into())],
                selected: Some(2),
                can_open_displays: true,
                can_create_display: false,
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
    static FULLSCREEN_COMMANDS:std::cell::RefCell<std::collections::HashSet<egui::ViewportId>>=std::cell::RefCell::new(std::collections::HashSet::new());
    static HUD_COMMANDS:std::cell::RefCell<std::collections::HashSet<egui::ViewportId>>=std::cell::RefCell::new(std::collections::HashSet::new());
}
pub(super) fn apply_fullscreen_command(app:&mut App,ctx:&egui::Context) {
    if FULLSCREEN_COMMANDS.with(|commands|commands.borrow_mut().remove(&ctx.viewport_id())) {
        app.local_shortcut(uu_core::hotkeys::Action::Fullscreen,ctx);
    }
    if HUD_COMMANDS.with(|commands|commands.borrow_mut().remove(&ctx.viewport_id())) {
        app.local_shortcut(uu_core::hotkeys::Action::Stats,ctx);
    }
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
    hud_before: Option<[bool;2]>,
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
                    assert!(Rc::ptr_eq(&window.sessions[0].app.shortcuts,&window.sessions[1].app.shortcuts));
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
                    check_display_window_routing(state,self.first,ctx);
                    // Switch away without another UI pass: inactive sessions
                    // must pause observation immediately, including parked tabs.
                    state.apply(Action::Select(self.first,None),ctx);
                    let first=&state.windows[0].sessions.iter().find(|s|s.id==self.first).unwrap().app;
                    let texture=first.textures.values().next().expect("generated image is visible");
                    texture.set_continuity_active(true,60);texture.record_new_image();texture.record_new_image();
                    assert!(texture.continuity().is_some());
                    state.apply(Action::Select(self.second_id,None),ctx);
                    assert!(state.windows[0].sessions.iter().find(|s|s.id==self.first).unwrap().app.textures.values().all(|t|t.continuity().is_none()));
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
                    FULLSCREEN_COMMANDS.with(|commands|commands.borrow_mut().insert(state.windows[0].viewport()));
                    self.phase = 7;
                }
                7 if ctx.input_for(state.windows[0].viewport(),|i|i.viewport().fullscreen)==Some(true) => {
                    let window=eframe::viewport_window_for(ctx,state.windows[0].viewport()).unwrap();
                    let monitor=window.current_monitor().unwrap();
                    assert_eq!(window.inner_size(),monitor.size(),"fullscreen must cover the complete monitor");
                    assert!(state.windows[0].sessions.iter().all(|session|session.app.fullscreen));
                    state.apply(Action::Select(self.second_id,None),ctx);
                    self.phase=8;
                }
                8 if state.windows[0].sessions.iter().find(|s|s.id==self.second_id).unwrap().app.active_session => {
                    assert_eq!(ctx.input_for(state.windows[0].viewport(),|i|i.viewport().fullscreen),Some(true));
                    assert!(state.windows[0].sessions.iter().all(|session|session.app.fullscreen),"fullscreen is shared by parked and active sessions");
                    FULLSCREEN_COMMANDS.with(|commands|commands.borrow_mut().insert(state.windows[0].viewport()));
                    self.phase=9;
                }
                9 if ctx.input_for(state.windows[0].viewport(),|i|i.viewport().fullscreen)==Some(false) => {
                    state.apply(Action::Select(self.first,None),ctx);
                    ctx.send_viewport_cmd_to(state.windows[0].viewport(),egui::ViewportCommand::Screenshot);
                    self.phase=6;
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
                        assert_eq!(self.stats[index].res(),(320+index as u32*64,192),"each native window must retain its distinct generated source");
                        assert!(!session.app.textures.is_empty());
                    }
                    let first=state.windows.iter().find(|w|w.sessions.iter().any(|s|s.id==self.first)).unwrap();
                    let second=state.windows.iter().find(|w|w.sessions.iter().any(|s|s.id==self.second_id)).unwrap();
                    self.hud_before=Some([first.sessions[0].app.hud,second.sessions[0].app.hud]);
                    FULLSCREEN_COMMANDS.with(|commands|commands.borrow_mut().insert(first.viewport()));
                    HUD_COMMANDS.with(|commands|commands.borrow_mut().insert(first.viewport()));
                    self.phase=10;
                }
                10 if state.windows.iter().find(|w|w.sessions.iter().any(|s|s.id==self.first))
                    .is_some_and(|w|ctx.input_for(w.viewport(),|i|i.viewport().fullscreen)==Some(true)) => {
                    let first=state.windows.iter().find(|w|w.sessions.iter().any(|s|s.id==self.first)).unwrap();
                    let second=state.windows.iter().find(|w|w.sessions.iter().any(|s|s.id==self.second_id)).unwrap();
                    assert_eq!(ctx.input_for(second.viewport(),|i|i.viewport().fullscreen),Some(false));
                    assert!(first.sessions[0].app.fullscreen);assert!(!second.sessions[0].app.fullscreen);
                    let before=self.hud_before.unwrap();
                    assert_eq!(first.sessions[0].app.hud,!before[0]);
                    assert_eq!(second.sessions[0].app.hud,before[1]);
                    FULLSCREEN_COMMANDS.with(|commands|commands.borrow_mut().insert(first.viewport()));
                    self.phase=11;
                }
                11 if state.windows.iter().find(|w|w.sessions.iter().any(|s|s.id==self.first))
                    .is_some_and(|w|ctx.input_for(w.viewport(),|i|i.viewport().fullscreen)==Some(false)) => {
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
                    println!("two distinct generated sources: attach, tab selection, independent quality, group reorder, native detach, independent fullscreen/HUD and isolated close; display-window launch routing checked with catalog fixtures, epochs unchanged, decode errors=0");
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
                hud_before: None,
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
