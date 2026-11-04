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
fn temporary_display_launch_is_a_distinct_connection_with_an_unwrapped_origin() {
    let base=ConnectionTarget::NativeDevice("01234567-89ab-4cde-8fab-0123456789ab".into());
    let target=StreamTarget{connection:base.clone(),alias:"Generated computer".into(),view_only:true};
    let spec=uu_core::virtual_display::Spec{width:3840,height:2160,refresh_millihz:60_000,hdr:true};
    let first=virtual_launch(&target,spec).unwrap();
    let second=virtual_launch(&first.target,spec).unwrap();
    assert_eq!(first.target.connection.base(),&base);
    assert_eq!(second.target.connection.base(),&base);
    assert_eq!(first.target.alias,target.alias);
    assert!(first.target.view_only&&second.target.view_only);
    assert_eq!(first.target.connection.virtual_display().unwrap().spec,spec);
    assert_ne!(first.target.key(),second.target.key());
    assert_ne!(first.target.key(),target.key());
    let explicit=display_launch(&first.target,&generated_screen(1)).unwrap();
    assert_eq!(explicit.target.connection.base(),&base,"viewing an owned source must not copy its creation intent");
    assert!(explicit.target.connection.virtual_display().is_none());
    for launch in [first,second] {
        let decoded:Launch=serde_json::from_slice(&serde_json::to_vec(&launch).unwrap()).unwrap();
        assert_eq!(decoded.target.key(),launch.target.key());
        assert!(decoded.plugin_payload().unwrap().is_none());
    }
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
fn aggregated_device_commands_keep_individual_view_ownership() {
    let group=Group{last_session:1,id:1,name:"Generated device".into(),screens:vec![(0,"Physical".into()),(16,"Virtual".into())],selected:Some(16),
        members:vec![1,2],views:vec![View{screen:0,session:1,fixed:false,active:false,can_create:true,can_detach:true,can_merge:false},View{screen:16,session:2,fixed:true,active:true,can_create:false,can_detach:false,can_merge:true}],
        can_open_displays:true,can_create_display:true,can_detach:false,fixed_view:false,can_merge:true};
    assert!(group.contains(2));assert_eq!(group.command_session(),2);assert_eq!(group.creation_session(),1);
    assert_eq!(group.screen_action(0),Action::Select(1,None));assert_eq!(group.screen_action(16),Action::Select(2,None));
    let virtual_menu=group.view_group(16).unwrap();assert_eq!(virtual_menu.command_session(),2);assert!(virtual_menu.can_merge);
    assert!(!virtual_menu.can_open_displays,"a fixed view menu must not inherit device-wide source-opening actions");
    let actions=Rc::new(RefCell::new(Vec::new()));
    let tabs=Tabs{presets:Default::default(),groups:vec![group],active:2,actions:actions.clone(),local_ui:Default::default()};
    tabs.navigate(uu_core::hotkeys::Action::PreviousDisplay);
    assert_eq!(&actions.borrow()[..],&[Action::Select(1,None)]);
    actions.borrow_mut().clear();
    let context=egui::Context::default();
    let render=|events| {
        let mut close=None;
        let _=context.run(egui::RawInput{events,focused:true,screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1100.,200.))),..Default::default()},|ctx| {
            egui::CentralPanel::default().show(ctx,|ui|{let rects=crate::stream_tabs_ui::show(&tabs,ui,160.);close=Some(rects.close[0].1.center());});
        });close.unwrap()
    };
    let close=render(vec![]);
    let button=|pressed|egui::Event::PointerButton{pos:close,button:egui::PointerButton::Primary,pressed,modifiers:Default::default()};
    render(vec![egui::Event::PointerMoved(close),button(true)]);render(vec![button(false)]);
    assert_eq!(&actions.borrow()[..],&[Action::Close(2)],"the aggregate device header closes only its active virtual view");
}

#[test]
fn computer_navigation_uses_its_last_view_and_falls_back_when_it_was_closed() {
    let group=Group{last_session:12,id:11,name:"Remembered computer".into(),screens:vec![],selected:None,
        members:vec![11,12],views:vec![],can_open_displays:false,can_create_display:false,can_detach:true,fixed_view:false,can_merge:false};
    let mut other=group.clone();other.id=22;other.last_session=22;other.members=vec![22];
    let mut tabs=Tabs{presets:Default::default(),groups:vec![group,other],active:22,actions:Default::default(),local_ui:Default::default()};
    assert_eq!(tabs.groups[0].activate(),Action::Select(12,None),"hostname selection shares the same rule");
    for command in [uu_core::hotkeys::Action::NextComputer,uu_core::hotkeys::Action::PreviousComputer] {tabs.navigate(command);}
    assert_eq!(std::mem::take(&mut *tabs.actions.borrow_mut()),[Action::Select(12,None),Action::Select(12,None)]);
    tabs.groups[0].members.retain(|id|*id!=12);
    tabs.navigate(uu_core::hotkeys::Action::NextComputer);
    assert_eq!(std::mem::take(&mut *tabs.actions.borrow_mut()),[Action::Select(11,None)]);
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

#[test]
fn merge_recreates_a_retired_home_and_avoids_a_closing_container() {
    let mut state=State{input_owner:None,gamepad:Default::default(),windows:vec![Window::new(2,Vec::new(),None)],gl:None,vsync:false,next:3,actions:Default::default()};
    assert_eq!(state.merge_target(2,Some(1)),Some(1));
    assert_eq!(state.windows.iter().map(|window|window.id).collect::<Vec<_>>(),[2,1]);
    assert_eq!(state.next,3,"restoring an old identity does not allocate or reuse another ID");
    state.windows.iter_mut().find(|window|window.id==1).unwrap().closing=true;
    let third=state.next;state.next+=1;
    state.windows.push(Window::new(third,vec![launch("generated-only.json".into(),"Pending")],None));
    assert_eq!(state.merge_target(2,Some(1)),Some(third));
    state.windows.iter_mut().find(|window|window.id==third).unwrap().closing=true;
    let fallback=state.next;
    assert_eq!(state.merge_target(2,Some(1)),Some(fallback));
    assert_ne!(fallback,1,"a closing surface retains its identity until cleanup completes");
    assert!(state.windows.iter().find(|window|window.id==fallback).is_some_and(|window|!window.closing));
    let mut alone=State{input_owner:None,gamepad:Default::default(),windows:vec![Window::new(1,Vec::new(),None)],gl:None,vsync:false,next:2,actions:Default::default()};
    assert_eq!(alone.merge_target(1,None),None);
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
    let epoch=stats.native_media_epoch.load(Ordering::Acquire);
    state.apply(Action::Select(session,Some(2)),ctx);
    assert_eq!(state.windows[0].active,sibling_id,"selecting an already viewed source focuses its existing connection");
    let source=state.windows[0].sessions.iter().find(|s|s.id==session).unwrap();
    assert_eq!(source.app.displays.selected,Some(0),"the originating connection must retain its physical source");
    assert_eq!(stats.native_media_epoch.load(Ordering::Acquire),epoch);
    state.windows[0].select(active_before);
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

// The encrypted synthetic hosts remain live; only their monitor catalogs are
// generated here, and every cold launch intent is removed before an IO pass.
fn check_cached_display_switches(state:&mut State,first:u64,ctx:&egui::Context) {
    let home=state.windows.iter().find(|w|w.sessions.iter().any(|s|s.id==first)).unwrap().id;
    let window=state.windows.iter_mut().find(|w|w.id==home).unwrap();
    let active=window.active;let count=window.sessions.len();
    let source=window.sessions.iter_mut().find(|s|s.id==first).unwrap();
    let target=source.app.stream_target.clone().unwrap();
    let first_displays=std::mem::take(&mut source.app.displays);source.app.displays.selected=Some(0);
    let a=source.app.controller.as_ref().unwrap().stats.clone();
    let cap=a.capabilities.allows(uu_core::capabilities::Capability::Displays);
    a.capabilities.enable(uu_core::capabilities::Capability::Displays,true);
    let a_catalog=std::mem::replace(&mut *a.remote_screens.lock().unwrap(),uu_core::screen_control::RemoteScreens{current_id:0,screens:(0..2).map(generated_screen).collect()});
    let a_capture=a.desktop_capture_source.lock().unwrap().take();
    let a_parked=a.audio.parked.load(Ordering::Acquire);let a_desired=source.app.audio_parked;
    let epoch=a.native_media_epoch.load(Ordering::Acquire);
    let windows=state.windows.len();
    for _ in 0..12 {state.apply(Action::Select(first,Some(1)),ctx);}
    assert_eq!(state.windows.len(),windows,"ordinary switching must not create a native window");
    let window=state.windows.iter_mut().find(|w|w.id==home).unwrap();
    assert_eq!(window.sessions.len(),count);assert_eq!(window.pending.len(),1,"one cold source has one pending connection");
    assert_eq!(window.pending[0].target.connection.display().unwrap().source,generated_screen(1).display_name);
    assert_eq!(window.cached_launches.len(),1);
    assert_eq!(window.sessions.iter().find(|s|s.id==first).unwrap().app.displays.selected,Some(0),"cold launch must retain the current source and frame");
    window.pending.clear();window.cached_launches.clear();
    let sibling=window.sessions.iter_mut().find(|s|s.id!=first).unwrap();let second=sibling.id;
    let second_target=sibling.app.stream_target.replace(target.clone());
    let second_displays=std::mem::take(&mut sibling.app.displays);sibling.app.displays.selected=Some(1);
    let second_home=sibling.home_window;let second_browser=sibling.browsable;sibling.browsable=true;sibling.home_window=None;
    let b=sibling.app.controller.as_ref().unwrap().stats.clone();
    let b_catalog=std::mem::replace(&mut *b.remote_screens.lock().unwrap(),uu_core::screen_control::RemoteScreens{current_id:1,screens:(0..2).map(generated_screen).collect()});
    let b_capture=b.desktop_capture_source.lock().unwrap().take();let b_parked=b.audio.parked.load(Ordering::Acquire);let b_desired=sibling.app.audio_parked;
    let b_cap=b.capabilities.allows(uu_core::capabilities::Capability::Displays);
    b.capabilities.enable(uu_core::capabilities::Capability::Displays,true);
    assert!(sibling.app.pin_current_display_view());
    sibling.app.stream_target=Some(display_launch(&target,&generated_screen(1)).unwrap().target);
    let second_epoch=b.native_media_epoch.load(Ordering::Acquire);
    // The fixture just changed these two formerly unrelated device identities.
    // Activate through the production transition before checking shared audio.
    window.select(first);
    for _ in 0..24 {
        state.apply(Action::Select(first,Some(1)),ctx);
        let window=state.windows.iter().find(|w|w.id==home).unwrap();assert_eq!(window.active,second);
        assert!(a.audio.parked.load(Ordering::Acquire));assert!(!b.audio.parked.load(Ordering::Acquire));
        state.apply(Action::Select(first,Some(0)),ctx);
        assert_eq!(state.windows.iter().find(|w|w.id==home).unwrap().active,first);
        assert!(!a.audio.parked.load(Ordering::Acquire));assert!(b.audio.parked.load(Ordering::Acquire));
    }
    assert_eq!(a.native_media_epoch.load(Ordering::Acquire),epoch);
    assert_eq!(b.native_media_epoch.load(Ordering::Acquire),second_epoch);
    state.detach(second,ctx);
    let independent=state.windows.iter().find(|w|w.sessions.iter().any(|s|s.id==second)).unwrap();
    assert_ne!(independent.id,home);assert_eq!(independent.tabs(Default::default()).groups[0].screens.len(),1);
    assert!(Arc::ptr_eq(&independent.sessions[0].app.controller.as_ref().unwrap().stats,&b));
    state.merge(second,ctx);
    let window=state.windows.iter_mut().find(|w|w.id==home).unwrap();
    assert_eq!(window.sessions.len(),count);assert_eq!(window.tabs(Default::default()).groups.len(),1);
    assert_eq!(window.tabs(Default::default()).groups[0].screens.len(),2,"merging must retain the device browser");
    assert!(window.pending.is_empty());assert!(window.cached_launches.is_empty());
    assert!(a.audio.parked.load(Ordering::Acquire),"merge must park the previous same-device audio owner");
    assert!(!b.audio.parked.load(Ordering::Acquire));
    // A failed cached view with no catalog must keep ownership of Close and
    // reconnect commands rather than borrowing the healthy catalog owner.
    let failed=window.sessions.iter_mut().find(|s|s.id==second).unwrap();
    failed.app.displays.selected=None;
    *b.remote_screens.lock().unwrap()=Default::default();
    let tabs=window.tabs(Default::default());
    assert_eq!(tabs.groups.len(),1);
    assert_eq!(tabs.groups[0].command_session(),second);
    assert!(window.sessions.iter().find(|s|s.id==first).unwrap().app.controller.is_some());
    // The catalog is device-wide: a failed first connection cannot hide an
    // unvisited third screen supplied by a healthy sibling.
    window.sessions.iter_mut().find(|s|s.id==first).unwrap().app.disconnecting=true;
    window.sessions.iter_mut().find(|s|s.id==second).unwrap().app.displays.selected=Some(1);
    *b.remote_screens.lock().unwrap()=uu_core::screen_control::RemoteScreens{current_id:1,screens:(0..3).map(generated_screen).collect()};
    let tabs=window.tabs(Default::default());assert_eq!(tabs.groups[0].id,second);
    assert_eq!(tabs.groups[0].screens.len(),3);
    window.sessions.iter_mut().find(|s|s.id==first).unwrap().app.disconnecting=false;
    // Explicit views share the same audio ownership rule after merging.
    window.sessions.iter_mut().find(|s|s.id==second).unwrap().browsable=false;
    window.select(first);assert!(!a.audio.parked.load(Ordering::Acquire));assert!(b.audio.parked.load(Ordering::Acquire));
    window.select(second);assert!(a.audio.parked.load(Ordering::Acquire));assert!(!b.audio.parked.load(Ordering::Acquire));
    let source=window.sessions.iter_mut().find(|s|s.id==first).unwrap();source.app.displays=first_displays;
    let sibling=window.sessions.iter_mut().find(|s|s.id==second).unwrap();sibling.app.displays=second_displays;sibling.app.stream_target=second_target;sibling.home_window=second_home;sibling.browsable=second_browser;
    *a.remote_screens.lock().unwrap()=a_catalog;*a.desktop_capture_source.lock().unwrap()=a_capture;
    *b.remote_screens.lock().unwrap()=b_catalog;*b.desktop_capture_source.lock().unwrap()=b_capture;
    a.capabilities.enable(uu_core::capabilities::Capability::Displays,cap);
    b.capabilities.enable(uu_core::capabilities::Capability::Displays,b_cap);
    window.select(active);
    window.sessions.iter_mut().find(|s|s.id==first).unwrap().app.audio_parked=a_desired;
    window.sessions.iter_mut().find(|s|s.id==second).unwrap().app.audio_parked=b_desired;
    a.audio.parked.store(a_parked,Ordering::Release);b.audio.parked.store(b_parked,Ordering::Release);
    state.windows.retain(|w|!w.sessions.is_empty()||!w.pending.is_empty());
    assert_eq!(state.windows.len(),windows);
}

fn check_same_device_views_keep_home_and_route_each_connection(state:&mut State,first:u64,second:u64,ctx:&egui::Context) {
    let original_windows=state.windows.len();let home=state.windows.iter().find(|w|w.sessions.iter().any(|s|s.id==first)).unwrap().id;
    let original_active=state.windows.iter().find(|w|w.id==home).unwrap().active;
    let source=state.windows.iter_mut().flat_map(|w|&mut w.sessions).find(|s|s.id==first).unwrap();
    let first_target=source.app.stream_target.clone();
    let first_displays=std::mem::take(&mut source.app.displays);
    source.app.displays.selected=Some(0);
    let first_stats=source.app.controller.as_ref().unwrap().stats.clone();
    let first_catalog=std::mem::replace(&mut *first_stats.remote_screens.lock().unwrap(),uu_core::screen_control::RemoteScreens{current_id:0,screens:(0..2).map(generated_screen).collect()});
    let first_capture=first_stats.desktop_capture_source.lock().unwrap().take();
    let old_control=first_stats.control_open.swap(1,Ordering::AcqRel);
    let old_binding=first_stats.display_track_binding.swap(false,Ordering::AcqRel);
    let old_capability=first_stats.capabilities.allows(uu_core::capabilities::Capability::Displays);
    first_stats.capabilities.enable(uu_core::capabilities::Capability::Displays,true);
    // Owned queue observes routing without asking the generated host to switch
    // to fixture-only monitor names or changing a real display topology.
    let (input,_requests)=uu_core::native_input::channel(false);
    let original_input=source.app.controller.as_mut().unwrap().native_input.replace(input);
    state.open_display(first,0,ctx);
    assert_eq!(state.windows.iter().find(|w|w.id==home).unwrap().sessions.len(),2,"opening the selected monitor cannot move its ordinary source session out of home");
    let original=state.windows.iter().flat_map(|w|&w.sessions).find(|s|s.id==first).unwrap();
    assert_eq!(original.app.displays.selected,Some(1),"the original window keeps the other physical monitor");
    assert!(Arc::ptr_eq(&original.app.controller.as_ref().unwrap().stats,&first_stats));
    assert_eq!(state.windows.last().unwrap().pending[0].target.connection.display().unwrap().id,0);
    state.windows.truncate(original_windows);
    let source=state.windows.iter_mut().flat_map(|w|&mut w.sessions).find(|s|s.id==first).unwrap();
    source.app.displays=Default::default();source.app.displays.selected=Some(0);
    state.apply(Action::OpenAllDisplays(first),ctx);
    assert!(state.windows.iter().find(|w|w.id==home).unwrap().sessions.iter().any(|s|s.id==first));
    assert_eq!(state.windows.len(),original_windows+1,"open-all leaves one monitor in home and forks only the other");
    assert_eq!(state.windows.last().unwrap().pending[0].target.connection.display().unwrap().id,1);
    let original=state.windows.iter().flat_map(|w|&w.sessions).find(|s|s.id==first).unwrap();
    assert_eq!(original.app.fixed_display_target().unwrap().id,0);
    state.windows.truncate(original_windows);
    let source=state.windows.iter_mut().flat_map(|w|&mut w.sessions).find(|s|s.id==first).unwrap();
    source.app.displays=Default::default();source.app.displays.selected=Some(0);
    first_stats.remote_screens.lock().unwrap().screens.push(generated_screen(16));
    let virtual_target=virtual_launch(first_target.as_ref().unwrap(),Default::default()).unwrap().target;
    let virtual_id=virtual_target.connection.virtual_display().unwrap().id;
    let sibling=state.windows.iter_mut().flat_map(|w|&mut w.sessions).find(|s|s.id==second).unwrap();
    let second_target=sibling.app.stream_target.replace(virtual_target);
    let second_displays=std::mem::take(&mut sibling.app.displays);let second_home=sibling.home_window;
    sibling.app.displays.bind_created_virtual(DisplayTarget{id:16,source:generated_screen(16).display_name});sibling.app.displays.selected=Some(16);
    let second_stats=sibling.app.controller.as_ref().unwrap().stats.clone();
    let second_catalog=std::mem::replace(&mut *second_stats.remote_screens.lock().unwrap(),uu_core::screen_control::RemoteScreens{current_id:16,screens:[0,1,16].into_iter().map(generated_screen).collect()});
    let second_capture=second_stats.desktop_capture_source.lock().unwrap().take();
    state.windows.iter_mut().find(|w|w.id==home).unwrap().select(second);
    let tabs=state.windows.iter().find(|w|w.id==home).unwrap().tabs(Default::default());
    assert_eq!(tabs.groups.len(),1,"a virtual connection is a view of the same device, not another device group");
    let group=&tabs.groups[0];assert!(group.contains(first)&&group.contains(second));
    assert_eq!(group.command_session(),second,"device controls apply only to the active view");
    let virtual_key=group.views.iter().find(|view|view.session==second).unwrap().screen;
    assert!(virtual_key<0,"per-connection view identity must not collide with remote catalog IDs");
    assert_eq!(group.screen_action(virtual_key),Action::Select(second,None));
    assert_eq!(group.screen_action(0),Action::Select(first,None));
    assert_eq!(group.view_group(virtual_key).unwrap().command_session(),second);
    assert_eq!(group.screens.len(),3,"the shared catalog's virtual source must not add a second pill");
    state.detach(second,ctx);
    assert!(state.windows.iter().find(|w|w.id==home).unwrap().sessions.iter().any(|s|s.id==first));
    let moved=state.windows.iter().flat_map(|w|&w.sessions).find(|s|s.id==second).unwrap();
    assert_eq!(moved.app.stream_target.as_ref().unwrap().connection.virtual_display().unwrap().id,virtual_id);
    assert!(Arc::ptr_eq(&moved.app.controller.as_ref().unwrap().stats,&second_stats));
    state.merge(second,ctx);
    let window=state.windows.iter().find(|w|w.id==home).unwrap();
    assert_eq!(window.sessions.len(),2);assert_eq!(window.tabs(Default::default()).groups.len(),1);
    let original=window.sessions.iter().find(|s|s.id==first).unwrap();assert!(Arc::ptr_eq(&original.app.controller.as_ref().unwrap().stats,&first_stats));
    state.windows.retain(|w|!w.sessions.is_empty()||!w.pending.is_empty());
    let source=state.windows.iter_mut().flat_map(|w|&mut w.sessions).find(|s|s.id==first).unwrap();
    source.app.displays=first_displays;source.app.stream_target=first_target;
    source.app.controller.as_mut().unwrap().native_input=original_input;
    *first_stats.remote_screens.lock().unwrap()=first_catalog;*first_stats.desktop_capture_source.lock().unwrap()=first_capture;
    first_stats.control_open.store(old_control,Ordering::Release);first_stats.display_track_binding.store(old_binding,Ordering::Release);first_stats.capabilities.enable(uu_core::capabilities::Capability::Displays,old_capability);
    let sibling=state.windows.iter_mut().flat_map(|w|&mut w.sessions).find(|s|s.id==second).unwrap();
    sibling.app.displays=second_displays;sibling.app.stream_target=second_target;sibling.home_window=second_home;
    *second_stats.remote_screens.lock().unwrap()=second_catalog;*second_stats.desktop_capture_source.lock().unwrap()=second_capture;
    state.windows.iter_mut().find(|w|w.id==home).unwrap().select(original_active);
}

fn check_temporary_display_fork(state:&mut State,session:u64,ctx:&egui::Context) {
    // The hosts below provide only generated video, without virtual hardware.
    // Exercise launch ownership here and discard pending intents before they
    // can establish a new connection or submit a display creation request.
    let origin=state.windows.iter().position(|w|w.sessions.iter().any(|s|s.id==session)).unwrap();
    let origin_id=state.windows[origin].id;
    let source=state.windows[origin].sessions.iter().find(|s|s.id==session).unwrap();
    let selected=source.app.displays.selected;
    let key=source.app.stream_target.as_ref().unwrap().key();
    let stats=source.app.controller.as_ref().unwrap().stats.clone();
    let available=stats.virtual_displays.available.swap(true,Ordering::AcqRel);
    let other=state.next;state.next+=1;
    state.windows.insert(0,Window::new(other,vec![],None));
    state.apply(Action::CreateDisplay(session),ctx);
    state.apply(Action::CreateDisplay(session),ctx);
    let origin=state.windows.iter_mut().find(|w|w.id==origin_id).unwrap();
    assert_eq!(origin.pending.len(),2,"new temporary streams must join the clicked connection's window");
    assert_ne!(origin.pending[0].target.key(),origin.pending[1].target.key());
    for launch in &origin.pending {
        assert!(launch.target.connection.virtual_display().is_some());
        assert_eq!(base_key(&launch.target),base_key(origin.sessions.iter().find(|s|s.id==session).unwrap().app.stream_target.as_ref().unwrap()));
    }
    let source=origin.sessions.iter().find(|s|s.id==session).unwrap();
    assert_eq!(source.app.displays.selected,selected);
    assert_eq!(source.app.stream_target.as_ref().unwrap().key(),key);
    assert!(Arc::ptr_eq(&source.app.controller.as_ref().unwrap().stats,&stats));
    let launch=origin.pending[0].target.clone();
    let source=origin.sessions.iter_mut().find(|s|s.id==session).unwrap();
    let saved_displays=std::mem::take(&mut source.app.displays);
    let saved_target=source.app.stream_target.replace(launch);
    let saved_track=source.app.selected_track;
    source.app.prepare_displays();
    let tabs=origin.tabs(Default::default());
    let preparing=tabs.groups.iter().find(|group|group.id==session).unwrap();
    assert_eq!(preparing.screens.len(),1,"a pending virtual view is visible inside its device group");
    assert_eq!(preparing.screen_action(preparing.screens[0].0),Action::Select(session,None));
    assert!(!preparing.can_open_displays&&!preparing.can_create_display);
    assert!(preparing.can_detach,"a connecting virtual stream can still be moved without reopening it");
    let source=origin.sessions.iter_mut().find(|s|s.id==session).unwrap();
    let preparing_title=session_title(&source.app);
    assert!(preparing_title.ends_with(uu_i18n::text("临时虚拟屏")));
    source.app.displays.layout.sync(&[generated_screen(1)]);
    source.app.displays.selected=Some(1);
    assert_eq!(session_title(&source.app),preparing_title,"first source binding must not grow the header or displace its close control");
    source.app.stream_target=saved_target;source.app.displays=saved_displays;source.app.selected_track=saved_track;
    origin.pending.clear();
    assert!(state.windows.iter().find(|w|w.id==other).unwrap().pending.is_empty());
    state.windows.retain(|w|w.id!=other);
    stats.virtual_displays.available.store(available,Ordering::Release);
}

fn check_close_returns_to_live_stream_before_cleanup(state:&mut State,session:u64,ctx:&egui::Context) {
    let window=state.windows.iter_mut().find(|window|window.sessions.iter().any(|source|source.id==session)).unwrap();
    assert_eq!(window.sessions.len(),1,"exercise one surviving stream plus a closing tab");
    let origin=&window.sessions[0].app;
    let target=origin.stream_target.clone();
    let stats=origin.controller.as_ref().unwrap().stats.clone();
    let epoch=stats.native_media_epoch.load(Ordering::Acquire);
    let selected=origin.displays.selected;
    let mut closing=App::new_session(ctx,None,window.platform.as_ref().unwrap().clone(),None,false,None);
    closing.stream_target=target;
    closing.shortcuts=window.shortcuts.clone();
    closing.window_sizing=window.sizing.clone();
    let stopped=Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (close,mut close_requests)=tokio::sync::mpsc::unbounded_channel();
    closing.controller=Some(uu_core::controller::ControllerHandle {
        stopped:stopped.clone(),events:std::sync::mpsc::channel().1,
        frames:uu_core::media::frame_channel().1,input:tokio::sync::mpsc::unbounded_channel().0,native_input:None,
        settings:tokio::sync::mpsc::unbounded_channel().0,close,stats:Arc::new(Default::default()),
    });
    let id=state.next;state.next+=1;
    window.sessions.push(Session{id,activated:None,app:closing,home_window:None,browsable:false});
    window.select(id);
    let previous_owner=state.input_owner.replace(id);
    state.apply(Action::Close(id),ctx);
    let window=state.windows.iter_mut().find(|window|window.sessions.iter().any(|source|source.id==session)).unwrap();
    let closing=window.sessions.iter().find(|source|source.id==id).unwrap();
    assert!(closing.app.exit_requested&&closing.app.disconnecting);
    assert!(!closing.app.active_session);
    assert!(!stopped.load(Ordering::Acquire),"the controller's cleanup has deliberately not finished");
    assert!(closing.app.controller.is_some(),"retain the cleanup owner while hiding its tab");
    assert!(close_requests.try_recv().is_ok());
    assert_eq!(window.active,session,"closing an active tab returns to its sibling immediately");
    let survivor=&window.sessions.iter().find(|source|source.id==session).unwrap().app;
    assert!(survivor.active_session);
    assert!(Arc::ptr_eq(&survivor.controller.as_ref().unwrap().stats,&stats));
    assert_eq!(stats.native_media_epoch.load(Ordering::Acquire),epoch);
    assert_eq!(survivor.displays.selected,selected);
    let tabs=window.tabs(Default::default());
    assert_eq!(tabs.groups.iter().map(|group|group.id).collect::<Vec<_>>(),[session]);
    assert!(!tabs.groups[0].can_detach,"a retiring tab cannot make the survivor appear merged");
    window.select(id);
    assert_eq!(window.active,session,"a stale selection cannot reactivate the closing tab");
    // A local command belongs to the native window, including its key-up.
    // Finishing the old controller must not clear the survivor's shared keys.
    window.shortcuts.borrow_mut().configure(&Default::default(),egui::Modifiers::NONE);
    let modifiers=egui::Modifiers{ctrl:true,alt:true,..egui::Modifiers::NONE};
    assert_eq!(window.shortcuts.borrow_mut().key(egui::Key::S,true,false,modifiers,true,false),
        crate::shortcuts::KeyAction::Trigger(uu_core::hotkeys::Action::Stats));
    stopped.store(true,Ordering::Release);
    let retiring=window.sessions.iter_mut().find(|source|source.id==id).unwrap();
    retiring.app.poll_controller(ctx);
    assert!(retiring.app.controller.is_none());
    assert_eq!(window.shortcuts.borrow_mut().key(egui::Key::S,false,false,egui::Modifiers::NONE,true,false),
        crate::shortcuts::KeyAction::Consumed,"a late controller stop cannot forward the local command's physical key-up");
    let survivor=window.sessions.iter().find(|source|source.id==session).unwrap();
    assert!(Arc::ptr_eq(&survivor.app.controller.as_ref().unwrap().stats,&stats));
    assert_eq!(stats.native_media_epoch.load(Ordering::Acquire),epoch);
    // This deterministic cleanup handle never owned a worker or a display.
    window.sessions.retain(|source|source.id!=id);
    state.input_owner=previous_owner;
}

#[test]
fn fullscreen_navigation_follows_tab_order_and_wraps_without_crossing_windows() {
    use uu_core::hotkeys::Action as Command;
    let actions=Rc::new(RefCell::new(Vec::new()));
    let mut tabs=Tabs{presets:Default::default(),groups:vec![Group{last_session:22,members:vec![],views:vec![],id:22,name:"B".into(),screens:vec![(7,"Second".into()),(2,"First".into()),(9,"Third".into())],selected:Some(7),can_open_displays:true,can_create_display:false,can_detach:true,fixed_view:false,can_merge:false},
        Group{last_session:11,members:vec![],views:vec![],id:11,name:"A".into(),screens:vec![],selected:None,can_open_displays:false,can_create_display:false,can_detach:true,fixed_view:false,can_merge:false}],active:22,actions:actions.clone(),local_ui:Default::default()};
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
    let mut tabs=Tabs{presets:Default::default(),groups:vec![Group{last_session:1,members:vec![],views:vec![],id:1,name:"A".into(),screens:vec![],selected:None,can_open_displays:false,can_create_display:false,can_detach:true,fixed_view:false,can_merge:false},Group{last_session:2,members:vec![],views:vec![],id:2,name:"B".into(),screens:vec![],selected:None,can_open_displays:false,can_create_display:false,can_detach:true,fixed_view:false,can_merge:false}],active:1,actions:Default::default(),local_ui:Default::default()};
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
    let tabs = Tabs {presets:Default::default(),
        groups: vec![
            Group {last_session:11,members:vec![],views:vec![],
                id: 11,
                name: "Generated A".into(),
                screens: vec![(1, "Monitor A".into())],
                selected: Some(1),
                can_open_displays: true,
                can_create_display: false,
                can_detach: true,
                fixed_view:false,can_merge:false,
            },
            Group {last_session:22,members:vec![],views:vec![],
                id: 22,
                name: "Generated B".into(),
                screens: vec![(2, "Monitor B".into())],
                selected: Some(2),
                can_open_displays: true,
                can_create_display: false,
                can_detach: true,
                fixed_view:false,can_merge:false,
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

#[test]
fn device_context_menu_routes_detach_and_disconnect_from_a_child_viewport() {
    let context=egui::Context::default();
    crate::setup_style(&context);
    let actions=Rc::new(RefCell::new(Vec::new()));
    let tabs=Tabs{presets:Default::default(),groups:vec![Group{last_session:22,members:vec![],views:vec![],id:22,name:"Generated B".into(),screens:vec![(2,"Monitor B".into())],selected:Some(2),can_open_displays:true,can_create_display:false,can_detach:true,fixed_view:false,can_merge:false}],active:22,actions:actions.clone(),local_ui:Default::default()};
    let viewport=egui::ViewportId::from_hash_of("generated-child");
    let render=|events|context.run(egui::RawInput{viewport_id:viewport,viewports:[(viewport,egui::ViewportInfo::default())].into_iter().collect(),events,screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1300.,300.))),focused:true,..Default::default()},|ctx|{egui::TopBottomPanel::top("groups").show(ctx,|ui|tabs.show(ui));});
    fn text(shape:&egui::epaint::Shape,label:&str)->Option<egui::Pos2>{match shape{egui::epaint::Shape::Text(text) if text.galley.job.text==label=>Some(text.pos+text.galley.rect.center().to_vec2()),egui::epaint::Shape::Vec(shapes)=>shapes.iter().find_map(|shape|text(shape,label)),_=>None}}
    let locate=|output:&egui::FullOutput,label:&str|output.shapes.iter().find_map(|shape|text(&shape.shape,label)).unwrap_or_else(||panic!("missing {label}"));
    let button=|pos,button,pressed|egui::Event::PointerButton{pos,button,pressed,modifiers:Default::default()};
    for (label,expected) in [("移到独立窗口",Action::Detach(22)),("断开此会话",Action::Close(22))] {
        let output=render(Vec::new());let device=locate(&output,"Generated B");
        render(vec![egui::Event::PointerMoved(device),button(device,egui::PointerButton::Secondary,true)]);
        render(vec![button(device,egui::PointerButton::Secondary,false)]);
        let output=render(Vec::new());
        assert!(tabs.owns_input(&context),"open context menu owns local input");
        let action=locate(&output,uu_i18n::text(label));
        render(vec![egui::Event::PointerMoved(action),button(action,egui::PointerButton::Primary,true)]);
        render(vec![button(action,egui::PointerButton::Primary,false)]);
        assert_eq!(std::mem::take(&mut *actions.borrow_mut()),[expected]);
        render(Vec::new());
    }
}

#[test]
fn fixed_source_context_menu_returns_to_home_and_source_click_only_selects_its_session() {
    let context=egui::Context::default();crate::setup_style(&context);
    let actions=Rc::new(RefCell::new(Vec::new()));
    let tabs=Tabs{presets:Default::default(),groups:vec![Group{last_session:22,members:vec![],views:vec![],id:22,name:"Generated fixed view".into(),screens:vec![(2,"Bound monitor".into())],selected:Some(2),can_open_displays:false,can_create_display:false,can_detach:false,fixed_view:true,can_merge:true}],active:22,actions:actions.clone(),local_ui:Default::default()};
    let viewport=egui::ViewportId::from_hash_of("fixed-generated-child");
    let render=|events|context.run(egui::RawInput{viewport_id:viewport,viewports:[(viewport,egui::ViewportInfo::default())].into_iter().collect(),events,focused:true,screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1300.,300.))),..Default::default()},|ctx|{egui::TopBottomPanel::top("groups").show(ctx,|ui|tabs.show(ui));});
    let region=|find:fn(&crate::stream_tabs_ui::StripRects)->egui::Rect|TAB_RECTS.with(|rects|find(rects.borrow().get(&viewport).unwrap()).center());
    let button=|pos,button,pressed|egui::Event::PointerButton{pos,button,pressed,modifiers:Default::default()};
    render(Vec::new());let source=region(|rects|rects.screens[0].2);
    render(vec![egui::Event::PointerMoved(source),button(source,egui::PointerButton::Primary,true)]);
    render(vec![button(source,egui::PointerButton::Primary,false)]);
    assert_eq!(std::mem::take(&mut *actions.borrow_mut()),[Action::Select(22,None)]);
    for source_menu in [false,true] {
        render(Vec::new());let anchor=if source_menu {region(|rects|rects.screens[0].2)}else{region(|rects|rects.devices[0].1)};
        render(vec![egui::Event::PointerMoved(anchor),button(anchor,egui::PointerButton::Secondary,true)]);
        render(vec![button(anchor,egui::PointerButton::Secondary,false)]);render(Vec::new());
        assert!(tabs.owns_input(&context));let merge=region(|rects|rects.merge[0].1);
        render(vec![egui::Event::PointerMoved(merge),button(merge,egui::PointerButton::Primary,true)]);
        render(vec![button(merge,egui::PointerButton::Primary,false)]);
        assert_eq!(std::mem::take(&mut *actions.borrow_mut()),[Action::Merge(22)]);
    }
}

#[test]
fn display_pills_open_their_own_source_and_keep_connection_ownership() {
    let context=egui::Context::default();crate::setup_style(&context);
    let actions=Rc::new(RefCell::new(Vec::new()));
    let tabs=Tabs{presets:Default::default(),groups:vec![Group{last_session:22,members:vec![],views:vec![],id:22,name:"Generated computer".into(),screens:vec![(7,"Physical".into()),(16,"Temporary".into())],selected:Some(16),can_open_displays:true,can_create_display:true,can_detach:false,fixed_view:false,can_merge:false}],active:22,actions:actions.clone(),local_ui:Default::default()};
    let render=|events|context.run(egui::RawInput{events,focused:true,screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1300.,300.))),..Default::default()},|ctx|{egui::TopBottomPanel::top("groups").show(ctx,|ui|tabs.show(ui));});
    let region=|find:fn(&crate::stream_tabs_ui::StripRects)->egui::Rect|TAB_RECTS.with(|rects|find(rects.borrow().get(&egui::ViewportId::ROOT).unwrap()).center());
    let button=|pos,button,pressed|egui::Event::PointerButton{pos,button,pressed,modifiers:Default::default()};
    render(Vec::new());
    let screen=region(|r|r.screens.iter().find(|(_,id,_)|*id==7).unwrap().2);
    render(vec![egui::Event::PointerMoved(screen),button(screen,egui::PointerButton::Secondary,true)]);
    render(vec![button(screen,egui::PointerButton::Secondary,false)]);render(Vec::new());
    let independent=region(|r|r.open_display[0].2);
    render(vec![egui::Event::PointerMoved(independent),button(independent,egui::PointerButton::Primary,true)]);
    render(vec![button(independent,egui::PointerButton::Primary,false)]);
    assert_eq!(std::mem::take(&mut *actions.borrow_mut()),[Action::OpenDisplay{session:22,screen:7}]);
    render(Vec::new());let current=region(|r|r.screens.iter().find(|(_,id,_)|*id==16).unwrap().2);
    render(vec![egui::Event::PointerMoved(current),button(current,egui::PointerButton::Secondary,true)]);
    render(vec![button(current,egui::PointerButton::Secondary,false)]);render(Vec::new());
    let independent=region(|r|r.open_display[0].2);
    render(vec![egui::Event::PointerMoved(independent),button(independent,egui::PointerButton::Primary,true)]);
    render(vec![button(independent,egui::PointerButton::Primary,false)]);
    assert!(actions.borrow().is_empty(),"the current source of an independent window must not offer another silent focus action");
    let outside=egui::pos2(1200.,200.);
    render(vec![egui::Event::PointerMoved(outside),button(outside,egui::PointerButton::Primary,true)]);
    render(vec![button(outside,egui::PointerButton::Primary,false)]);
    render(Vec::new());let device=region(|r|r.devices[0].1);
    render(vec![egui::Event::PointerMoved(device),button(device,egui::PointerButton::Secondary,true)]);
    render(vec![button(device,egui::PointerButton::Secondary,false)]);render(Vec::new());
    let detach=region(|r|r.detach[0].1);
    render(vec![egui::Event::PointerMoved(detach),button(detach,egui::PointerButton::Primary,true)]);
    render(vec![button(detach,egui::PointerButton::Primary,false)]);
    assert!(actions.borrow().is_empty(),"an already independent connection must not queue a silent detach");
    let outside=egui::pos2(1200.,200.);
    render(vec![egui::Event::PointerMoved(outside),button(outside,egui::PointerButton::Primary,true)]);
    render(vec![button(outside,egui::PointerButton::Primary,false)]);render(Vec::new());
    let create=region(|r|r.create[0].1);
    render(vec![egui::Event::PointerMoved(create),button(create,egui::PointerButton::Primary,true)]);
    render(vec![button(create,egui::PointerButton::Primary,false)]);
    assert!(matches!(&std::mem::take(&mut *actions.borrow_mut())[..],[Action::CreateDisplayWithSpec{session:22,..}]),"creation stays under the clicked device and carries the visible preset");
    render(Vec::new());let close=region(|r|r.close[0].1);
    render(vec![egui::Event::PointerMoved(close),button(close,egui::PointerButton::Primary,true)]);
    render(vec![button(close,egui::PointerButton::Primary,false)]);
    assert_eq!(std::mem::take(&mut *actions.borrow_mut()),[Action::Close(22)],"close ends this connection, not one source or another window");
}

std::thread_local! {
    static SCREENSHOTS:std::cell::RefCell<Option<std::collections::HashMap<egui::ViewportId,Arc<egui::ColorImage>>>>=const {std::cell::RefCell::new(None)};
    static FULLSCREEN_COMMANDS:std::cell::RefCell<std::collections::HashSet<egui::ViewportId>>=std::cell::RefCell::new(std::collections::HashSet::new());
    static HUD_COMMANDS:std::cell::RefCell<std::collections::HashSet<egui::ViewportId>>=std::cell::RefCell::new(std::collections::HashSet::new());
    static TAB_RECTS:RefCell<std::collections::HashMap<egui::ViewportId,crate::stream_tabs_ui::StripRects>>=RefCell::new(Default::default());
    static POINTER_SCRIPT:RefCell<Option<PointerScript>>=const {RefCell::new(None)};
    static SURFACE_BINDINGS:RefCell<Vec<(u64,egui::ViewportId,egui::ViewportId)>>=const {RefCell::new(Vec::new())};
}
pub(super) fn surface_bound(session:u64,old:egui::ViewportId,new:egui::ViewportId,waker:bool) {
    eprintln!("generated surface binding session={session} old={old:?} new={new:?} frame_waker={waker}");
    if waker {SURFACE_BINDINGS.with(|bindings|bindings.borrow_mut().push((session,old,new)));}
}
pub(super) fn observe_tab_rects(ctx:&egui::Context,rects:crate::stream_tabs_ui::StripRects) {
    TAB_RECTS.with(|slots|slots.borrow_mut().insert(ctx.viewport_id(),rects));
}
#[derive(Clone,Copy,Debug)]
enum PointerAction { Detach, Merge, Disconnect, CloseButton }
struct PointerScript {
    viewport:egui::ViewportId,
    session:u64,
    action:PointerAction,
    step:u8,
}
fn click_session(viewport:egui::ViewportId,session:u64,action:PointerAction,ctx:&egui::Context) {
    POINTER_SCRIPT.with(|script|*script.borrow_mut()=Some(PointerScript{viewport,session,action,step:0}));
    ctx.request_repaint_of(viewport);
}
fn pointer_script_done()->bool {POINTER_SCRIPT.with(|script|script.borrow().is_none())}
fn inject_pointer_script(input:&mut egui::RawInput) {
    POINTER_SCRIPT.with(|slot| {
        let mut slot=slot.borrow_mut();
        let Some(script)=slot.as_mut().filter(|script|script.viewport==input.viewport_id) else{return;};
        let finished=TAB_RECTS.with(|rects| {
            let rects=rects.borrow();let Some(rects)=rects.get(&script.viewport) else{return false;};
            let direct=matches!(script.action,PointerAction::CloseButton);
            let buttons=if direct {&rects.close} else if script.step<3 {&rects.devices} else {match script.action {PointerAction::Detach=>&rects.detach,PointerAction::Merge=>&rects.merge,PointerAction::Disconnect=>&rects.disconnect,PointerAction::CloseButton=>unreachable!()}};
            let Some((_,rect))=buttons.iter().find(|(id,_)|*id==script.session) else{return false;};
            let position=rect.center();
            eprintln!("generated pointer action={:?} session={} viewport={:?} step={} rect={rect:?}",script.action,script.session,script.viewport,script.step);
            input.focused=true;
            input.events.push(egui::Event::PointerMoved(position));
            let button=if !direct && script.step<3 {egui::PointerButton::Secondary} else {egui::PointerButton::Primary};
            let stage=if direct {script.step} else if script.step<3 {script.step} else {script.step-3};
            if stage<2 {input.events.push(egui::Event::PointerButton{pos:position,button,pressed:stage==0,modifiers:Default::default()});}
            script.step+=1;
            direct && script.step==2 || !direct && script.step==5
        });
        if finished {*slot=None;}
    });
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
    ui_started:Option<Instant>,
    roundtrip_done:bool,
    tail_state:Option<String>,
}
impl eframe::App for Probe {
    fn raw_input_hook(&mut self,_ctx:&egui::Context,input:&mut egui::RawInput) {
        inject_pointer_script(input);
    }
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
                5 => {
                    let tail=format!("script_done={} pending_actions={} windows={:?}",pointer_script_done(),state.actions.borrow().len(),
                        state.windows.iter().map(|window|(window.id,window.active,window.sessions.iter().map(|session|(session.id,session.app.exit_requested,session.app.disconnecting,
                            session.app.controller.as_ref().map(|handle|handle.stopped.load(Ordering::Acquire)),session.app.context.viewport())).collect::<Vec<_>>())).collect::<Vec<_>>());
                    if self.tail_state.as_ref()!=Some(&tail) {eprintln!("generated close tail {tail}");self.tail_state=Some(tail);}
                }
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
                    check_cached_display_switches(state,self.first,ctx);
                    check_temporary_display_fork(state,self.first,ctx);
                    check_same_device_views_keep_home_and_route_each_connection(state,self.first,self.second_id,ctx);
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
                7 if ctx.input_for(state.windows[0].viewport(),|i|i.viewport().fullscreen)==Some(true)
                    && eframe::viewport_window_for(ctx,state.windows[0].viewport()).is_some_and(|window|window.current_monitor().is_some_and(|monitor|window.inner_size()==monitor.size())) => {
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
                        self.ui_started=Some(Instant::now());
                        click_session(state.windows[0].viewport(),self.first,PointerAction::Detach,ctx);
                        self.phase=20;
                    }
                },
                20 if pointer_script_done() && state.windows.len()==2=> {
                    assert!(self.ui_started.take().unwrap().elapsed()<Duration::from_secs(2),"child context menu must wake its hidden coordinator promptly");
                    assert!(!Rc::ptr_eq(&state.windows[0].sizing,&state.windows[1].sizing));
                    self.phase=3;
                },
                3 if state.windows.len() == 2
                    && state.windows[1].platform.is_some()
                    && self
                        .stats
                        .iter()
                        .zip(&self.counts)
                        .all(|(s, old)| s.decoded_total.load(Ordering::Acquire) > old + 8) =>
                {
                    if !self.roundtrip_done {
                        let window=state.windows.iter().find(|window|window.sessions.iter().any(|session|session.id==self.first)).unwrap();
                        assert!(window.tabs(Default::default()).groups.iter().find(|group|group.id==self.first).unwrap().can_merge);
                        click_session(window.viewport(),self.first,PointerAction::Merge,ctx);
                        self.phase=30;return;
                    }
                    check_close_returns_to_live_stream_before_cleanup(state,self.first,ctx);
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
                30 if pointer_script_done() && state.windows.len()==1 && state.windows[0].sessions.len()==2 => {
                    assert_eq!(state.windows[0].id,1,"Merge returns to the original logical container");
                    if state.windows[0].sessions.iter().any(|session|session.app.context.viewport()!=state.windows[0].viewport()) {
                        assert!(!state.windows[0].sessions.iter().find(|session|session.id==self.first).unwrap().app.active_session,
                            "a moved session cannot become active before its destination surface pass");
                        ctx.request_repaint_of(state.windows[0].viewport());return;
                    }
                    for (index,id) in [self.first,self.second_id].into_iter().enumerate() {
                        let session=state.windows[0].sessions.iter().find(|session|session.id==id).unwrap();
                        assert!(Arc::ptr_eq(&session.app.controller.as_ref().unwrap().stats,&self.stats[index]));
                        assert_eq!(self.stats[index].native_media_epoch.load(Ordering::Acquire),self.epochs[index]);
                        assert_eq!(session.app.context.viewport(),state.windows[0].viewport(),"merging onto an existing surface must rebind commands and wakeups");
                        assert!(Rc::ptr_eq(&session.app.platform,state.windows[0].platform.as_ref().unwrap()));
                    }
                    assert!(SURFACE_BINDINGS.with(|bindings|bindings.borrow().iter().any(|(session,old,new)|*session==self.first&&old!=new&&*new==state.windows[0].viewport())),
                        "Merge must retarget the real frame publisher wakeup to the existing destination viewport");
                    click_session(state.windows[0].viewport(),self.first,PointerAction::Detach,ctx);
                    self.phase=31;
                }
                31 if pointer_script_done() && state.windows.len()==2 => {self.roundtrip_done=true;self.phase=3;}
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
                    let window=state.windows.iter().find(|w|w.sessions.iter().any(|s|s.id==self.first)).unwrap();
                    click_session(window.viewport(),self.first,PointerAction::Disconnect,ctx);
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
                    click_session(state.windows[0].viewport(),self.second_id,PointerAction::CloseButton,ctx);
                    self.phase = 5;
                    *self.passed.lock().unwrap() = true;
                    println!("two distinct generated sources: attach, tab selection, independent quality, group reorder, context-menu detach and disconnect, close button, independent fullscreen/HUD; display-window launch routing checked with catalog fixtures, epochs unchanged, decode errors=0");
                }
                _ => {}
            }
        });
        // While waiting for a child UI action, the coordinator must be woken
        // by that action. A slow watchdog catches a missing wakeup without
        // turning the probe's normal polling into a false success.
        ctx.request_repaint_after(if matches!(self.phase,20|4|5){Duration::from_secs(2)}else{Duration::from_millis(10)});
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
                ui_started:None,
                roundtrip_done:false,
                tail_state:None,
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
