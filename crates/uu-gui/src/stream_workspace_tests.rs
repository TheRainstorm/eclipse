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
fn owned_state()->State {
    State {sources:vec![],inquiries:Default::default(),input_owner:None,gamepad:Default::default(),windows:vec![Window::new(1,vec![],None)],gl:None,vsync:false,next:2,actions:Default::default(),local_desktop:false,restore_window:None,budget:Default::default(),budget_refresh:None}
}
fn owned_target(name:&str)->StreamTarget {
    StreamTarget {connection:ConnectionTarget::NativeDevice(format!("generated-{name}")),alias:name.into(),view_only:true}
}
fn own_generated(state:&mut State,window:usize,device:&StreamTarget,screen:i32)->u64 {
    let launch=display_launch(device,&generated_screen(screen)).unwrap();
    state.windows[window].record_launch(&launch,&mut state.next)
}
#[test]
fn owned_lazy_views_detach_leave_navigation_and_close_only_their_window_group() {
    let mut state=owned_state();let device=owned_target("A");
    let a=own_generated(&mut state,0,&device,0);let b=own_generated(&mut state,0,&device,1);let c=own_generated(&mut state,0,&device,2);
    assert!(state.windows[0].sessions.is_empty());assert!(state.windows[0].pending.is_empty());
    state.windows[0].select(a);assert_eq!(state.windows[0].pending.len(),1);
    let ctx=egui::Context::default();state.detach(c,&ctx);
    assert_eq!(state.windows[0].views.iter().map(|v|v.id).collect::<Vec<_>>(),[a,b]);
    assert_eq!(state.windows[1].views.iter().map(|v|v.id).collect::<Vec<_>>(),[c]);
    let actions=Rc::new(RefCell::new(Vec::new()));let tabs=state.windows[0].tabs(actions.clone());
    tabs.navigate(uu_core::hotkeys::Action::NextDisplay);
    assert_eq!(&*actions.borrow(),&[Action::Select(b,None)]);
    state.apply(Action::CloseGroup(a),&ctx);
    assert!(state.windows[0].views.is_empty());assert!(state.windows[0].pending.is_empty());
    assert_eq!(state.windows[1].views[0].id,c);
    state.apply(Action::Close(c),&ctx);assert!(state.windows.iter().all(|w|w.views.is_empty()&&w.pending.is_empty()));
}
#[test]
fn closing_the_last_view_marks_the_window_for_immediate_release() {
    let mut state=owned_state();
    let device=owned_target("A");
    let view=own_generated(&mut state,0,&device,0);
    let ctx=egui::Context::default();
    state.apply(Action::CloseGroup(view),&ctx);
    let window=&state.windows[0];
    assert!(window.views.is_empty());
    assert!(window.pending.is_empty());
    assert!(window.closing);
}
fn catalog_state(screens:Vec<uu_core::screen_control::RemoteScreen>,current:i32)->(State,Arc<uu_core::media::MediaStats>,egui::Context,u64) {
    use raw_window_handle::{DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle};
    struct NoWindow;
    impl HasWindowHandle for NoWindow {
        fn window_handle(&self)->Result<WindowHandle<'_>,HandleError>{Err(HandleError::Unavailable)}
    }
    impl HasDisplayHandle for NoWindow {
        fn display_handle(&self)->Result<DisplayHandle<'_>,HandleError>{Err(HandleError::Unavailable)}
    }
    let ctx=egui::Context::default();
    let platform=Rc::new(crate::platform::Platform::for_window(&NoWindow,&ctx,None));
    let target=owned_target("A");
    let mut app=App::new_session(&ctx,None,platform.clone(),Some(target.clone()),false,None,false);
    let stats=Arc::new(uu_core::media::MediaStats::default());
    stats.capabilities.enable(uu_core::capabilities::Capability::Displays,true);
    *stats.remote_screens.lock().unwrap()=uu_core::screen_control::RemoteScreens{current_id:current,screens};
    let (input,_)=uu_core::native_input::channel(false);
    let (_,events)=std::sync::mpsc::channel();
    let frames=uu_core::media::frame_channel().1;
    let (raw,_)=tokio::sync::mpsc::unbounded_channel();
    let (close,_)=tokio::sync::mpsc::unbounded_channel();
    app.controller=Some(uu_core::controller::ControllerHandle{stopped:Arc::new(false.into()),events,frames,input:raw.clone(),native_input:Some(input),settings:raw,close,stats:stats.clone()});
    app.displays.selected=Some(current);
    let launch=Launch::new(target,None).unwrap();
    let mut state=owned_state();
    let id=state.windows[0].record_launch(&launch,&mut state.next);
    state.windows[0].sessions.push(Session{id,activated:None,app});
    state.windows[0].active=id;
    state.windows[0].platform=Some(platform);
    (state,stats,ctx,id)
}
#[test]
fn remembered_second_display_keeps_host_order_and_only_connects_the_selection() {
    let (mut state,stats,_,selected)=catalog_state(vec![generated_screen(0),generated_screen(1)],1);
    state.sync_owned_views();
    let window=&state.windows[0];
    assert_eq!(window.views.iter().map(|v|v.source.as_ref().unwrap().id).collect::<Vec<_>>(),[0,1]);
    assert_eq!(window.active,selected);
    assert_eq!(window.sessions.iter().map(|s|s.id).collect::<Vec<_>>(),[selected]);
    assert!(window.pending.is_empty(),"discovering the earlier display must not connect it");
    state.windows[0].views.reverse(); // A deliberate user reorder survives refresh.
    state.sync_owned_views();
    assert_eq!(state.windows[0].views.iter().map(|v|v.source.as_ref().unwrap().id).collect::<Vec<_>>(),[1,0]);
    assert_eq!(stats.native_media_epoch.load(Ordering::Acquire),0);
}
#[test]
fn failed_virtual_tab_disconnect_button_preserves_the_other_stream_and_window() {
    let (mut state,stats,ctx,live)=catalog_state(vec![generated_screen(0)],0);
    let target=state.windows[0].sessions[0].app.stream_target.as_ref().unwrap().clone();
    let launch=virtual_launch(&target,uu_core::virtual_display::Spec::default()).unwrap();
    let failed=state.windows[0].record_launch(&launch,&mut state.next);
    let platform=state.windows[0].platform.as_ref().unwrap().clone();
    let mut app=App::new_session(&ctx,None,platform,Some(launch.target),false,None,false);
    app.status="generated virtual display preparation failure".into();
    app.connect_deferred=false;
    state.windows[0].sessions.push(Session{id:failed,activated:None,app});
    state.windows[0].active=failed;
    let tabs=state.windows[0].tabs(state.actions.clone());
    state.windows[0].sessions[1].app.workspace_tabs=Some(tabs);
    let view=egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1280.,720.));
    let mut render=|events|ctx.run(egui::RawInput{screen_rect:Some(view),events,..Default::default()},|ctx|{
        state.windows[0].sessions[1].app.stream_ui(ctx,&[]);
    });
    let mut output=render(vec![]);
    for _ in 0..2 {output=render(vec![]);}
    let point=output.shapes.iter().find_map(|shape|if let egui::Shape::Text(text)=&shape.shape {
        (text.galley.job.text==uu_i18n::text("断开此会话")).then_some(text.pos+text.galley.size()/2.)
    } else {None}).expect("failed tab disconnect button");
    render(vec![egui::Event::PointerMoved(point),egui::Event::PointerButton{pos:point,button:egui::PointerButton::Primary,pressed:true,modifiers:Default::default()}]);
    let output=render(vec![egui::Event::PointerButton{pos:point,button:egui::PointerButton::Primary,pressed:false,modifiers:Default::default()}]);
    assert!(output.viewport_output.values().all(|viewport|!viewport.commands.iter().any(|command|matches!(command,egui::ViewportCommand::Close))),"tab action must never close the native window");
    let actions=state.actions.take();
    assert_eq!(actions,[Action::Close(failed)]);
    for action in actions {state.apply(action,&ctx);}
    assert_eq!(state.windows[0].views.iter().map(|view|view.id).collect::<Vec<_>>(),[live]);
    assert!(!state.windows[0].closing);
    let live_session=state.windows[0].sessions.iter().find(|session|session.id==live).unwrap();
    assert!(!live_session.app.exit_requested && !live_session.app.disconnecting);
    assert!(Arc::ptr_eq(&stats,&live_session.app.controller.as_ref().unwrap().stats));
}
#[test]
fn group_shortcut_menu_and_window_close_hide_before_cleanup_and_reject_late_frames() {
    for entry in ["shortcut","device-menu","window"] {
        let (mut state,_,ctx,first)=catalog_state(vec![generated_screen(0),generated_screen(1)],0);
        state.sync_owned_views();
        let second=state.windows[0].views.iter().find(|v|v.id!=first).unwrap().id;
        let platform=state.windows[0].platform.as_ref().unwrap().clone();
        let target=state.windows[0].views.iter().find(|v|v.id==second).unwrap().launch.target.clone();
        let app=App::new_session(&ctx,None,platform,Some(target),false,None,true);
        state.windows[0].sessions.push(Session{id:second,activated:None,app});
        let mut stops=Vec::new();
        let mut closes=Vec::new();
        let mut frames=Vec::new();
        for session in &mut state.windows[0].sessions {
            let (tx,rx)=uu_core::media::frame_channel();
            let stopped=Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (close,receive)=tokio::sync::mpsc::unbounded_channel();
            let (raw,_)=tokio::sync::mpsc::unbounded_channel();
            session.app.controller=Some(uu_core::controller::ControllerHandle {
                stopped:stopped.clone(),events:std::sync::mpsc::channel().1,frames:rx,
                input:raw.clone(),native_input:None,settings:raw,close,stats:Arc::new(Default::default()),
            });
            stops.push(stopped);closes.push(receive);frames.push(tx);
        }
        state.windows[0].select(second);
        let tabs=state.windows[0].tabs(state.actions.clone());
        state.windows[0].sessions[1].app.workspace_tabs=Some(tabs);
        match entry {
            "shortcut"=>{
                state.windows[0].sessions[1].app.local_shortcut(crate::shortcuts::Action::Exit,&ctx);
                let actions=std::mem::take(&mut *state.actions.borrow_mut());
                for action in actions {state.apply(action,&ctx);}
            },
            "device-menu"=>state.apply(Action::CloseGroup(second),&ctx),
            _=>state.windows[0].request_close(),
        }
        assert!(state.windows[0].closing,"{entry}");
        assert!(state.windows[0].views.is_empty()&&state.windows[0].pending.is_empty());
        assert_eq!(state.windows[0].active,0);
        assert!(closes.iter_mut().all(|rx|rx.try_recv().is_ok()));
        for tx in &frames {tx.send((0,2,2,vec![255;16])).unwrap();}
        let output=ctx.run(Default::default(),|ctx| {
            state.windows[0].poll_closing(ctx);
            let window=&mut state.windows[0];
            window.show(ctx,&None,false,false,&mut state.next,state.actions.clone(),&state.gamepad);
        });
        assert!(output.viewport_output.values().any(|out|out.commands.iter().any(|cmd|matches!(cmd,egui::ViewportCommand::Visible(false)))));
        assert!(state.windows[0].sessions.iter().all(|s|s.app.controller.is_some()&&s.app.textures.is_empty()),
            "{entry}: retain the cleanup owners without uploading late frames");
        for stopped in stops {stopped.store(true,Ordering::Release);}
        let _=ctx.run(Default::default(),|ctx| {
            state.windows[0].show(ctx,&None,false,false,&mut state.next,state.actions.clone(),&state.gamepad);
        });
        assert!(state.windows[0].sessions.is_empty(),"{entry}: release after the real completion signal");
    }
}
#[test]
fn later_catalog_sources_are_added_once_and_do_not_reappear_after_close() {
    let (mut state,stats,ctx,id)=catalog_state(vec![generated_screen(0)],0);
    state.sync_owned_views();
    assert_eq!(state.windows[0].views.len(),1,"the first catalog has one source");

    *stats.remote_screens.lock().unwrap()=uu_core::screen_control::RemoteScreens{current_id:0,screens:vec![generated_screen(0),generated_screen(1)]};
    state.sync_owned_views();
    assert_eq!(state.windows[0].views.len(),2,"a later catalog adds the new source once");
    let added=state.windows[0].views.iter().find(|view|view.id!=id).unwrap().id;
    state.close(&[added],&ctx);
    state.sync_owned_views();
    assert_eq!(state.windows[0].views.len(),1,"a source closed by the user stays closed");
    assert_eq!(stats.native_media_epoch.load(Ordering::Acquire),0);
}
#[test]
fn committed_virtual_view_replaces_discovered_duplicate_and_retires_unused_pool_tabs() {
    let (mut state,stats,_,base)=catalog_state((0..4).map(generated_screen).collect(),0);
    state.sync_owned_views();
    let target=state.windows[0].sessions[0].app.stream_target.as_ref().unwrap().clone();
    let launch=virtual_launch(&target,uu_core::virtual_display::Spec::default()).unwrap();
    let requested=state.windows[0].record_launch(&launch,&mut state.next);
    state.windows[0].views.iter_mut().find(|view|view.id==requested).unwrap().source=Some(DisplayTarget{id:2,source:generated_screen(2).display_name});
    assert_eq!(state.windows[0].views.len(),5,"the reported intermediate state has a duplicate and an unused target");
    *stats.remote_screens.lock().unwrap()=uu_core::screen_control::RemoteScreens{current_id:0,screens:(0..3).map(generated_screen).collect()};
    state.sync_owned_views();
    assert_eq!(state.windows[0].views.len(),3);
    assert_eq!(state.windows[0].views.iter().filter(|view|view.source.as_ref().is_some_and(|source|source.id==2)).count(),1);
    assert!(state.windows[0].views.iter().any(|view|view.id==requested&&view.launch.target.connection.virtual_display().is_some()));
    assert_eq!(state.windows[0].sessions.iter().map(|session|session.id).collect::<Vec<_>>(),[base]);
    assert!(state.windows[0].pending.is_empty());
    state.sync_owned_views();assert_eq!(state.windows[0].views.len(),3,"polling must not resurrect retired pool tabs");
}
#[test]
fn pending_virtual_launch_reserves_discovery_until_its_exact_source_is_bound() {
    let (mut state,stats,_,_)=catalog_state((0..2).map(generated_screen).collect(),0);
    state.sync_owned_views();
    let target=state.windows[0].sessions[0].app.stream_target.as_ref().unwrap().clone();
    let launch=virtual_launch(&target,uu_core::virtual_display::Spec::default()).unwrap();
    let requested=state.windows[0].record_launch(&launch,&mut state.next);
    state.windows[0].pending.push(launch);
    *stats.remote_screens.lock().unwrap()=uu_core::screen_control::RemoteScreens{current_id:0,screens:(0..4).map(generated_screen).collect()};
    state.sync_owned_views();assert_eq!(state.windows[0].views.len(),3);
    assert!(state.windows[0].views.iter().all(|view|view.source.as_ref().is_none_or(|source|source.id<2)));
    state.windows[0].pending.clear();
    state.windows[0].views.iter_mut().find(|view|view.id==requested).unwrap().source=Some(DisplayTarget{id:2,source:generated_screen(2).display_name});
    *stats.remote_screens.lock().unwrap()=uu_core::screen_control::RemoteScreens{current_id:0,screens:(0..3).map(generated_screen).collect()};
    state.sync_owned_views();assert_eq!(state.windows[0].views.len(),3);
    assert_eq!(state.windows[0].views.iter().filter(|view|view.source.as_ref().is_some_and(|source|source.id==2)).count(),1);
}
#[test]
fn a_user_opened_display_keeps_its_tab_when_the_source_goes_offline() {
    let (mut state,stats,_,base)=catalog_state((0..2).map(generated_screen).collect(),0);
    state.sync_owned_views();
    let requested=state.windows[0].views.iter().find(|view|view.id!=base).unwrap().id;
    state.windows[0].select(requested);state.windows[0].pending.clear();state.windows[0].select(base);
    *stats.remote_screens.lock().unwrap()=uu_core::screen_control::RemoteScreens{current_id:0,screens:vec![generated_screen(0)]};
    state.sync_owned_views();
    assert!(state.windows[0].views.iter().any(|view|view.id==requested&&!view.discovered));
}
#[test]
fn local_desktop_toggle_preserves_window_and_view_identity_for_restore() {
    let mut state=owned_state();let device=owned_target("A");let ctx=egui::Context::default();
    let a=own_generated(&mut state,0,&device,0);state.windows[0].select(a);
    state.toggle_local_desktop(&ctx);
    assert!(state.local_desktop);assert_eq!(state.restore_window,Some(1));assert_eq!(state.windows[0].views[0].id,a);assert_eq!(state.windows[0].active,a);
    state.toggle_local_desktop(&ctx);
    assert!(!state.local_desktop);assert!(state.restore_window.is_none());assert_eq!(state.windows[0].views[0].id,a);assert_eq!(state.windows[0].active,a);
}
#[test]
fn deferred_initial_budget_uses_source_geometry_and_reserves_manual_targets() {
    let base=ConnectionTarget::NativeDevice("01234567-89ab-4cde-8fab-0123456789ab".into());
    let target=|id,source:&str|StreamTarget{connection:ConnectionTarget::NativeDisplay{connection:Box::new(base.clone()),display:DisplayTarget{id,source:source.into()}},alias:"Generated computer".into(),view_only:true};
    let mut settings=uu_core::client_settings::ClientSettings::default();
    assert_eq!(settings.bitrate,6);
    let mut large_screen=generated_screen(1);large_screen.width=3840;large_screen.height=2160;large_screen.refresh_hz=120;large_screen.display_name="Large".into();
    let mut small_screen=generated_screen(2);small_screen.width=1920;small_screen.height=1080;small_screen.refresh_hz=60;small_screen.display_name="Small".into();
    let large=deferred_budget_plan(1,&settings,&target(1,"Large"),Some(&large_screen),Default::default()).unwrap();
    let small=deferred_budget_plan(2,&settings,&target(2,"Small"),Some(&small_screen),Default::default()).unwrap();
    assert!(large.deferred&&small.deferred);
    assert!(large.fixed.is_none()&&small.fixed.is_none());
    assert!(large.weight>small.weight);
    let allocations=uu_core::client_settings::allocate_bitrate_budget(100_000,&[(large.weight,large.fixed),(small.weight,small.fixed)]);
    assert!(allocations.iter().sum::<u32>()<=100_000);
    assert!(allocations[0]>allocations[1]);
    settings.bitrate=4;settings.custom_mbps=20;
    let manual=deferred_budget_plan(3,&settings,&target(2,"Small"),Some(&small_screen),Default::default()).unwrap();
    assert_eq!(manual.fixed,Some(20_000));
    let allocations=uu_core::client_settings::allocate_bitrate_budget(100_000,&[(large.weight,large.fixed),(manual.weight,manual.fixed)]);
    assert_eq!(allocations[1],20_000);assert!(allocations[0]+allocations[1]<=100_000);
    settings.resolution=uu_core::client_settings::LOCAL_RESOLUTION;settings.follow_remote_fps=false;
    settings.fps=165;settings.limit_fps_to_local=true;
    let local=uu_core::client_settings::LocalDisplay{size:Some([3072,1920]),refresh_millihz:Some(60_000)};
    let plan=deferred_budget_plan(4,&settings,&target(1,"Large"),Some(&large_screen),local).unwrap();
    assert_eq!(plan.weight,3072*1920*60,"initial budget must use the same local bounds and cap as the media offer");
}
#[test]
fn initial_quality_and_runtime_monitor_changes_keep_the_requested_rate_when_capped() {
    let (mut state,_,ctx,_)=catalog_state(vec![generated_screen(0)],0);
    let app=&mut state.windows[0].sessions[0].app;
    app.preferences.client=uu_core::client_settings::ClientSettings {
        fps:165,follow_remote_fps:false,limit_fps_to_local:true,resolution:uu_core::client_settings::LOCAL_RESOLUTION,..Default::default()
    };
    app.target_fps=165;app.resolution_idx=app.preferences.resolution;
    let key=egui::Id::new(("test-local-display",app.context.viewport()));
    let set=|size,refresh|ctx.data_mut(|data|data.insert_temp(key,uu_core::client_settings::LocalDisplay{size:Some(size),refresh_millihz:Some(refresh)}));
    set([3072,1920],60_000);
    let first=app.client_stats("generated-initial-quality");
    let request=first.native_quality_request.get().unwrap();
    assert_eq!(request.fps,60);assert_eq!(request.bounds,Some([3072,1920]));assert_eq!(app.target_fps,60);
    app.persist_preferences();assert_eq!(app.preferences.fps,165,"saving another setting must preserve the uncapped selection");
    set([2560,1440],144_000);
    assert!(app.resolve_frame_rate());assert_eq!(app.target_fps,144);
    assert_eq!(app.native_quality_request().bounds,Some([2560,1440]));
    app.preferences.limit_fps_to_local=false;app.resolve_frame_rate();assert_eq!(app.target_fps,165);
    app.preferences.follow_local_fps=true;app.resolve_frame_rate();assert_eq!(app.target_fps,144);
    app.controller=None;
}
#[test]
fn budget_congestion_controller_uses_loss_and_rtt_with_hysteresis() {
    let start=Instant::now();let mut state=BudgetState::default();
    state.observe(Some(25_000),None,start);assert!(state.factor<1.);
    let reduced=state.factor;
    state.observe(Some(0),Some(20),start+Duration::from_secs(1));assert_eq!(state.factor,reduced,"a healthy sample cannot immediately undo backoff");
    state.observe(None,None,start+Duration::from_secs(6));assert_eq!(state.factor,reduced,"missing loss evidence cannot increase the factor");
    state.observe(Some(0),Some(20),start+Duration::from_secs(7));assert!(state.factor>reduced);
    state.observe(Some(0),Some(300),start+Duration::from_secs(8));assert!(state.factor<reduced+0.05);
}
#[test]
fn owned_views_merge_into_oldest_other_live_window_and_never_restore_retired_homes() {
    let mut state=owned_state();let device=owned_target("A");let ctx=egui::Context::default();
    let a=own_generated(&mut state,0,&device,0);let b=own_generated(&mut state,0,&device,1);let c=own_generated(&mut state,0,&device,2);
    state.detach(c,&ctx);let original=state.windows[0].id;let detached=state.windows[1].id;
    assert_eq!(state.oldest_other_window(detached),Some(original));
    state.merge(c,&ctx);assert_eq!(state.windows[0].views.iter().map(|v|v.id).collect::<Vec<_>>(),[a,b,c]);
    assert!(state.windows[1].views.is_empty());
    state.windows.retain(|w|!w.views.is_empty());
    let before=state.next;assert_eq!(state.oldest_other_window(original),None);
    assert_eq!(state.next,before);assert_eq!(state.windows.len(),1);
    state.windows[0].request_close();assert!(state.windows[0].closing);assert!(state.windows[0].views.is_empty());
}
#[test]
fn add_display_reserves_unopened_views_and_prioritizes_active_physical_sources() {
    let mut state=owned_state();let device=owned_target("A");let ctx=egui::Context::default();
    let a=own_generated(&mut state,0,&device,0);
    for (id,screen,kind,active) in [(100,1,SourceKind::Persistent,true),(101,2,SourceKind::Physical,true),(102,3,SourceKind::Physical,false)] {
        state.sources.push(SourceRecord{id,device:base_key(&device),screen:generated_screen(screen),label:format!("Source {screen}"),kind,active,managed:None,removable:false});
    }
    state.add_display(a,&ctx);
    assert_eq!(state.windows[0].views.len(),2);assert_eq!(state.windows[0].views[1].source.as_ref().unwrap().source,"Generated source 2");
    assert!(state.source_owned(&state.sources[1]));
    let windows=state.windows.len();state.open_source(a,101,true,&ctx);
    assert_eq!(state.windows.len(),windows,"unopened or pending views already own their source");
    state.add_display(a,&ctx);assert_eq!(state.windows[0].views.last().unwrap().source.as_ref().unwrap().source,"Generated source 1");
    let id=state.windows[0].views[1].id;state.close(&[id],&ctx);
    state.open_source(a,101,false,&ctx);
    assert_ne!(state.windows[0].views.last().unwrap().id,id,"a retired view identity cannot be reused by an old cleanup");
}
#[test]
fn multicomputer_close_is_confirmed_and_ui_screen_keys_never_truncate_view_ids() {
    let mut state=owned_state();state.next=i32::MAX as u64+100;
    let a=own_generated(&mut state,0,&owned_target("A"),0);let b=own_generated(&mut state,0,&owned_target("B"),1);
    state.windows[0].select(a);let tabs=state.windows[0].tabs(Default::default());
    assert_eq!(tabs.groups[0].screens[0].0,0);assert_eq!(tabs.groups[1].screens[0].0,0);
    assert_eq!(tabs.groups[1].screen_action(0),Action::Select(b,None));
    state.windows[0].request_close();assert!(state.windows[0].close_confirmation);assert!(state.windows[0].modal.get());
    assert!(!state.windows[0].closing);assert_eq!(state.windows[0].views.len(),2);
    state.windows[0].close_all();assert!(state.windows[0].views.is_empty());assert!(state.windows[0].pending.is_empty());
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
    let tabs=Tabs{sources:vec![],modal:Default::default(),presets:Default::default(),groups:vec![group],active:2,actions:actions.clone(),local_ui:Default::default()};
    tabs.navigate(uu_core::hotkeys::Action::PreviousDisplay);
    assert_eq!(&actions.borrow()[..],&[Action::Select(1,None)]);
    actions.borrow_mut().clear();
    let context=egui::Context::default();
    let render=|events| {
        let mut regions=None;
        let _=context.run(egui::RawInput{events,focused:true,screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1100.,200.))),..Default::default()},|ctx| {
            egui::CentralPanel::default().show(ctx,|ui|{let rects=crate::stream_tabs_ui::show(&tabs,ui,160.);regions=Some(rects);});
        });regions.unwrap()
    };
    let regions=render(vec![]);assert!(regions.close.is_empty());
    let device=regions.devices[0].1.center();
    let button=|pos,button,pressed|egui::Event::PointerButton{pos,button,pressed,modifiers:Default::default()};
    render(vec![egui::Event::PointerMoved(device),button(device,egui::PointerButton::Secondary,true)]);
    render(vec![button(device,egui::PointerButton::Secondary,false)]);
    let close=render(vec![]).disconnect[0].1.center();
    render(vec![egui::Event::PointerMoved(close),button(close,egui::PointerButton::Primary,true)]);
    render(vec![button(close,egui::PointerButton::Primary,false)]);
    assert_eq!(&actions.borrow()[..],&[Action::CloseGroup(2)],"the hostname closes all views of this device in this window");
}

#[test]
fn computer_navigation_uses_its_last_view_and_falls_back_when_it_was_closed() {
    let group=Group{last_session:12,id:11,name:"Remembered computer".into(),screens:vec![],selected:None,
        members:vec![11,12],views:vec![],can_open_displays:false,can_create_display:false,can_detach:true,fixed_view:false,can_merge:false};
    let mut other=group.clone();other.id=22;other.last_session=22;other.members=vec![22];
    let mut tabs=Tabs{sources:vec![],modal:Default::default(),presets:Default::default(),groups:vec![group,other],active:22,actions:Default::default(),local_ui:Default::default()};
    assert_eq!(tabs.groups[0].activate(),Action::Select(12,None),"hostname selection shares the same rule");
    for command in [uu_core::hotkeys::Action::NextComputer,uu_core::hotkeys::Action::PreviousComputer] {tabs.navigate(command);}
    assert_eq!(std::mem::take(&mut *tabs.actions.borrow_mut()),[Action::Select(12,None),Action::Select(12,None)]);
    tabs.groups[0].members.retain(|id|*id!=12);
    tabs.navigate(uu_core::hotkeys::Action::NextComputer);
    assert_eq!(std::mem::take(&mut *tabs.actions.borrow_mut()),[Action::Select(11,None)]);
}

#[test]
fn independent_display_ipc_creates_separate_pending_windows_and_reuses_a_stable_source() {
    let mut state=State{sources:vec![],inquiries:Default::default(),input_owner:None,gamepad:Default::default(),windows:vec![Window::new(1,Vec::new(),None)],gl:None,vsync:false,next:2,actions:Default::default(),local_desktop:false,restore_window:None,budget:Default::default(),budget_refresh:None};
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
fn merge_uses_live_pending_windows_and_ignores_retired_or_closing_containers() {
    let mut state=State{sources:vec![],inquiries:Default::default(),input_owner:None,gamepad:Default::default(),windows:vec![Window::new(2,Vec::new(),None)],gl:None,vsync:false,next:3,actions:Default::default(),local_desktop:false,restore_window:None,budget:Default::default(),budget_refresh:None};
    assert_eq!(state.oldest_other_window(2),None);assert_eq!(state.windows.len(),1);assert_eq!(state.next,3);
    let third=state.next;state.next+=1;
    state.windows.push(Window::new(third,vec![launch("generated-only.json".into(),"Pending")],None));
    assert_eq!(state.oldest_other_window(2),Some(third));
    state.windows.iter_mut().find(|w|w.id==third).unwrap().closing=true;
    assert_eq!(state.oldest_other_window(2),None);assert_eq!(state.next,4);
}

// Real encrypted generated streams keep their controllers; only the source
// inventory and view binding are temporary fixtures. Cold intents are removed
// before the next viewport pass, so this never creates desktop resources.
fn check_owned_live_transitions(state:&mut State,first:u64,second:u64,ctx:&egui::Context) {
    let home=state.windows.iter().find(|w|w.views.iter().any(|v|v.id==first)).unwrap().id;
    let window=state.windows.iter().find(|w|w.id==home).unwrap();
    let saved_views=window.views.clone();let saved_active=window.active;
    let saved_sources=state.sources.clone();
    let target=window.sessions.iter().find(|s|s.id==first).unwrap().app.stream_target.clone().unwrap();
    let virtual_target=virtual_launch(&target,Default::default()).unwrap().target;
    let creation=virtual_target.connection.virtual_display().unwrap().id;
    let mut saved=Vec::new();
    for (id,current) in [(first,0),(second,16)] {
        let source=state.windows.iter_mut().flat_map(|w|&mut w.sessions).find(|s|s.id==id).unwrap();
        let stats=source.app.controller.as_ref().unwrap().stats.clone();
        let displays=std::mem::take(&mut source.app.displays);
        let old_target=source.app.stream_target.replace(target.clone());
        let old_catalog=std::mem::replace(&mut *stats.remote_screens.lock().unwrap(),uu_core::screen_control::RemoteScreens {
            current_id:current,screens:[0,1,16].into_iter().map(|id|{let mut s=generated_screen(id);s.video_track_index=if id==current{0}else{-1};s}).collect()
        });
        let capture=stats.desktop_capture_source.lock().unwrap().take();
        let capability=stats.capabilities.allows(uu_core::capabilities::Capability::Displays);
        stats.capabilities.enable(uu_core::capabilities::Capability::Displays,true);
        source.app.displays.selected=Some(current);assert!(source.app.pin_current_display_view());
        if id==second {source.app.stream_target=Some(virtual_target.clone());}
        saved.push((id,displays,old_target,stats,old_catalog,capture,capability));
    }
    {
        let window=state.windows.iter_mut().find(|w|w.id==home).unwrap();
        let view=window.views.iter_mut().find(|v|v.id==second).unwrap();
        view.launch.target=virtual_target;view.source=Some(DisplayTarget{id:16,source:generated_screen(16).display_name});
        view.discover_initial=false;
        window.views.iter_mut().find(|v|v.id==first).unwrap().discover_initial=true;
    }
    state.sync_owned_views();
    let window=state.windows.iter().find(|w|w.id==home).unwrap();
    assert_eq!(window.views.len(),3);assert_eq!(window.sessions.len(),2);assert!(window.pending.is_empty());
    let lazy=window.views.iter().find(|v|v.id!=first&&v.id!=second).unwrap().id;
    let epochs:Vec<_>=saved.iter().map(|s|s.3.native_media_epoch.load(Ordering::Acquire)).collect();
    for _ in 0..12 {
        state.apply(Action::Select(first,Some(16)),ctx);
        let window=state.windows.iter().find(|w|w.id==home).unwrap();assert_eq!(window.active,second);assert!(window.pending.is_empty());
        assert!(saved[0].3.audio.parked.load(Ordering::Acquire));assert!(!saved[1].3.audio.parked.load(Ordering::Acquire));
        state.apply(Action::Select(first,Some(0)),ctx);assert_eq!(state.windows.iter().find(|w|w.id==home).unwrap().active,first);
    }
    state.detach(second,ctx);
    let moved=state.windows.iter().flat_map(|w|&w.sessions).find(|s|s.id==second).unwrap();
    assert!(Arc::ptr_eq(&moved.app.controller.as_ref().unwrap().stats,&saved[1].3));
    assert_eq!(moved.app.stream_target.as_ref().unwrap().connection.virtual_display().unwrap().id,creation);
    let original=state.windows.iter().find(|w|w.id==home).unwrap();
    assert_eq!(original.views.iter().map(|v|v.id).collect::<Vec<_>>(),[first,lazy]);
    let queued=Rc::new(RefCell::new(Vec::new()));original.tabs(queued.clone()).navigate(uu_core::hotkeys::Action::PreviousDisplay);
    assert_eq!(&*queued.borrow(),&[Action::Select(lazy,None)]);
    state.merge(second,ctx);
    state.detach(lazy,ctx);
    let unopened=state.windows.iter().find(|w|w.views.iter().any(|v|v.id==lazy)).unwrap();
    assert!(unopened.sessions.is_empty());assert_eq!(unopened.pending.len(),1);
    state.apply(Action::Close(lazy),ctx);
    state.windows.retain(|w|!w.views.is_empty()||!w.sessions.is_empty()||!w.pending.is_empty());
    assert_eq!(state.windows.len(),1);
    for (index,record) in saved.iter().enumerate() {
        let runtime=state.windows[0].sessions.iter().find(|s|s.id==record.0).unwrap();
        assert!(Arc::ptr_eq(&runtime.app.controller.as_ref().unwrap().stats,&record.3));
        assert_eq!(record.3.native_media_epoch.load(Ordering::Acquire),epochs[index]);
    }
    let available=saved[0].3.virtual_displays.available.swap(true,Ordering::AcqRel);
    state.fork_virtual_display(first,None,false);state.fork_virtual_display(first,None,true);
    let virtual_views:Vec<_>=state.windows[0].views.iter().filter(|v|v.id!=first&&v.id!=second).collect();
    assert_eq!(virtual_views.len(),2);
    assert_ne!(virtual_views[0].launch.target.key(),virtual_views[1].launch.target.key());
    assert!(!virtual_views[0].launch.target.connection.virtual_display().unwrap().persistent);
    assert!(virtual_views[1].launch.target.connection.virtual_display().unwrap().persistent);
    let cold:Vec<_>=virtual_views.iter().map(|v|v.id).collect();state.close(&cold,ctx);
    saved[0].3.virtual_displays.available.store(available,Ordering::Release);
    for (id,displays,target,stats,catalog,capture,capability) in saved {
        let source=state.windows[0].sessions.iter_mut().find(|s|s.id==id).unwrap();
        source.app.displays=displays;source.app.stream_target=target;
        *stats.remote_screens.lock().unwrap()=catalog;*stats.desktop_capture_source.lock().unwrap()=capture;
        stats.capabilities.enable(uu_core::capabilities::Capability::Displays,capability);
    }
    state.windows[0].views=saved_views;state.windows[0].pending.clear();state.sources=saved_sources;
    state.windows[0].select(saved_active);
}

fn check_close_returns_to_live_stream_before_cleanup(state:&mut State,session:u64,ctx:&egui::Context) {
    let window=state.windows.iter_mut().find(|window|window.sessions.iter().any(|source|source.id==session)).unwrap();
    assert_eq!(window.sessions.len(),1,"exercise one surviving stream plus a closing tab");
    let origin=&window.sessions[0].app;
    let target=origin.stream_target.clone();
    let stats=origin.controller.as_ref().unwrap().stats.clone();
    let epoch=stats.native_media_epoch.load(Ordering::Acquire);
    let selected=origin.displays.selected;
    let mut closing=App::new_session(ctx,None,window.platform.as_ref().unwrap().clone(),None,false,None,false);
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
    let launch=virtual_launch(closing.stream_target.as_ref().unwrap(),Default::default()).unwrap();
    let id=window.record_launch(&launch,&mut state.next);
    window.sessions.push(Session{id,activated:None,app:closing});
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
    let mut tabs=Tabs{sources:vec![],modal:Default::default(),presets:Default::default(),groups:vec![Group{last_session:22,members:vec![],views:vec![],id:22,name:"B".into(),screens:vec![(7,"Second".into()),(2,"First".into()),(9,"Third".into())],selected:Some(7),can_open_displays:true,can_create_display:false,can_detach:true,fixed_view:false,can_merge:false},
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
fn device_and_monitor_widgets_route_clicks_and_drags_to_their_session() {
    let context = egui::Context::default();
    crate::setup_style(&context);
    let actions = Rc::new(RefCell::new(Vec::new()));
    let tabs = Tabs {sources:vec![],modal:Default::default(),presets:Default::default(),
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
    assert!(matches!(&actions.borrow()[..],[Action::Drop{source:22,group:true,..}]));
}

#[test]
fn device_context_menu_merges_or_closes_the_group_without_detach_commands() {
    let context=egui::Context::default();
    crate::setup_style(&context);
    let actions=Rc::new(RefCell::new(Vec::new()));
    let tabs=Tabs{sources:vec![],modal:Default::default(),presets:Default::default(),groups:vec![Group{last_session:22,members:vec![],views:vec![],id:22,name:"Generated B".into(),screens:vec![(2,"Monitor B".into())],selected:Some(2),can_open_displays:true,can_create_display:false,can_detach:true,fixed_view:false,can_merge:true}],active:22,actions:actions.clone(),local_ui:Default::default()};
    let viewport=egui::ViewportId::from_hash_of("generated-child");
    let render=|events|context.run(egui::RawInput{viewport_id:viewport,viewports:[(viewport,egui::ViewportInfo::default())].into_iter().collect(),events,screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1300.,300.))),focused:true,..Default::default()},|ctx|{egui::TopBottomPanel::top("groups").show(ctx,|ui|tabs.show(ui));});
    fn text(shape:&egui::epaint::Shape,label:&str)->Option<egui::Pos2>{match shape{egui::epaint::Shape::Text(text) if text.galley.job.text==label=>Some(text.pos+text.galley.rect.center().to_vec2()),egui::epaint::Shape::Vec(shapes)=>shapes.iter().find_map(|shape|text(shape,label)),_=>None}}
    let locate=|output:&egui::FullOutput,label:&str|output.shapes.iter().find_map(|shape|text(&shape.shape,label)).unwrap_or_else(||panic!("missing {label}"));
    let button=|pos,button,pressed|egui::Event::PointerButton{pos,button,pressed,modifiers:Default::default()};
    for (label,expected) in [("合并所有显示器到原窗口",Action::MergeGroup(22)),("断开此会话",Action::CloseGroup(22))] {
        let output=render(Vec::new());let device=locate(&output,"Generated B");
        render(vec![egui::Event::PointerMoved(device),button(device,egui::PointerButton::Secondary,true)]);
        render(vec![button(device,egui::PointerButton::Secondary,false)]);
        let output=render(Vec::new());
        assert!(!output.shapes.iter().any(|s|text(&s.shape,"移到独立窗口").is_some()));
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
    let tabs=Tabs{sources:vec![],modal:Default::default(),presets:Default::default(),groups:vec![Group{last_session:22,members:vec![],views:vec![],id:22,name:"Generated fixed view".into(),screens:vec![(2,"Bound monitor".into())],selected:Some(2),can_open_displays:false,can_create_display:false,can_detach:false,fixed_view:true,can_merge:true}],active:22,actions:actions.clone(),local_ui:Default::default()};
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
        assert_eq!(std::mem::take(&mut *actions.borrow_mut()),[if source_menu{Action::Merge(22)}else{Action::MergeGroup(22)}]);
    }
}

#[test]
fn display_pills_open_their_own_source_and_keep_connection_ownership() {
    let context=egui::Context::default();crate::setup_style(&context);
    let actions=Rc::new(RefCell::new(Vec::new()));
    let views=vec![View{screen:7,session:31,fixed:true,active:false,can_create:false,can_detach:true,can_merge:false},View{screen:16,session:22,fixed:true,active:true,can_create:true,can_detach:true,can_merge:false}];
    let tabs=Tabs{sources:vec![(31,vec![SourceChoice{id:100,label:"Spare physical".into(),kind:SourceKind::Physical,active:true,owned:false,current_view:None,managed:None,removable:false},SourceChoice{id:101,label:"Owned unopened".into(),kind:SourceKind::Physical,active:true,owned:true,current_view:None,managed:None,removable:false}])],modal:Default::default(),presets:Default::default(),groups:vec![Group{last_session:22,members:vec![31,22],views,id:31,name:"Generated computer".into(),screens:vec![(7,"Physical".into()),(16,"Temporary".into())],selected:Some(16),can_open_displays:true,can_create_display:true,can_detach:true,fixed_view:true,can_merge:false}],active:22,actions:actions.clone(),local_ui:Default::default()};
    let render=|events|context.run(egui::RawInput{events,focused:true,screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1300.,700.))),..Default::default()},|ctx|{egui::TopBottomPanel::top("groups").show(ctx,|ui|tabs.show(ui));});
    let region=|find:fn(&crate::stream_tabs_ui::StripRects)->egui::Rect|TAB_RECTS.with(|r|find(r.borrow().get(&egui::ViewportId::ROOT).unwrap()).center());
    let button=|pos,button,pressed|egui::Event::PointerButton{pos,button,pressed,modifiers:Default::default()};
    let click=|pos,kind|{render(vec![egui::Event::PointerMoved(pos),button(pos,kind,true)]);render(vec![button(pos,kind,false)]);};
    render(vec![]);let screen=region(|r|r.screens.iter().find(|(_,id,_)|*id==7).unwrap().2);
    click(screen,egui::PointerButton::Secondary);render(vec![]);
    click(region(|r|r.detach[0].1),egui::PointerButton::Primary);
    assert_eq!(std::mem::take(&mut *actions.borrow_mut()),[Action::Detach(31)]);
    render(vec![]);click(region(|r|r.create[0].1),egui::PointerButton::Primary);
    assert_eq!(std::mem::take(&mut *actions.borrow_mut()),[Action::AddDisplay(22)]);
    render(vec![]);click(region(|r|r.create[0].1),egui::PointerButton::Secondary);render(vec![]);
    fn text(shape:&egui::Shape,label:&str)->Option<egui::Pos2> {match shape {
        egui::Shape::Text(t) if t.galley.text()==label=>Some(t.pos+t.galley.rect.center().to_vec2()),
        egui::Shape::Vec(s)=>s.iter().find_map(|s|text(s,label)),_=>None,
    }}
    let locate=|label|render(vec![]).shapes.iter().find_map(|s|text(&s.shape,label)).unwrap_or_else(||panic!("missing {label}"));
    click(locate("Owned unopened"),egui::PointerButton::Primary);assert!(actions.borrow().is_empty());
    click(locate("Spare physical"),egui::PointerButton::Primary);
    assert_eq!(std::mem::take(&mut *actions.borrow_mut()),[Action::OpenSource{session:22,source:100,new_window:false}]);
    render(vec![]);click(region(|r|r.screens.iter().find(|(_,id,_)|*id==16).unwrap().2),egui::PointerButton::Secondary);render(vec![]);
    click(region(|r|r.disconnect[0].1),egui::PointerButton::Primary);
    assert_eq!(std::mem::take(&mut *actions.borrow_mut()),[Action::Close(22)]);
    render(vec![]);click(region(|r|r.devices[0].1),egui::PointerButton::Secondary);render(vec![]);
    click(region(|r|r.disconnect[0].1),egui::PointerButton::Primary);
    assert_eq!(std::mem::take(&mut *actions.borrow_mut()),[Action::CloseGroup(22)]);
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
enum PointerAction { Detach, Merge, Disconnect }
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
            let buttons=if script.step<3 {if matches!(script.action,PointerAction::Disconnect) {&rects.devices}else{&rects.view_screens}} else {match script.action {PointerAction::Detach=>&rects.detach,PointerAction::Merge=>&rects.merge,PointerAction::Disconnect=>&rects.disconnect}};
            let Some((_,rect))=buttons.iter().find(|(id,_)|*id==script.session) else{return false;};
            let position=rect.center();
            eprintln!("generated pointer action={:?} session={} viewport={:?} step={} rect={rect:?}",script.action,script.session,script.viewport,script.step);
            input.focused=true;
            input.events.push(egui::Event::PointerMoved(position));
            let button=if script.step<3 {egui::PointerButton::Secondary} else {egui::PointerButton::Primary};
            let stage=if script.step<3 {script.step} else {script.step-3};
            if stage<2 {input.events.push(egui::Event::PointerButton{pos:position,button,pressed:stage==0,modifiers:Default::default()});}
            script.step+=1;
            script.step==5
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
    hidden_window:Option<winit::window::WindowId>,
    hidden_since:Option<Instant>,
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
                    check_owned_live_transitions(state,self.first,self.second_id,ctx);
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
                    assert_eq!(state.windows[0].views[0].id, self.second_id);
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
                    self.hidden_window=Some(eframe::viewport_window_for(ctx,state.windows[0].viewport()).unwrap().id());
                    self.hidden_since=Some(Instant::now());
                    state.toggle_local_desktop(ctx);
                    self.phase=40;
                }
                40=>{
                    let window=eframe::viewport_window_for(ctx,state.windows[0].viewport()).expect("returning to local must keep the native window declared");
                    assert_eq!(Some(window.id()),self.hidden_window,"hiding must not replace the native window");
                    if self.hidden_since.unwrap().elapsed()>=Duration::from_millis(300) {
                        assert_eq!(window.is_visible(),Some(false));
                        assert!(state.windows[0].sessions.iter().all(|s|!s.app.active_session));
                        state.toggle_local_desktop(ctx);self.phase=41;
                    }
                }
                41 if eframe::viewport_window_for(ctx,state.windows[0].viewport()).is_some_and(|window|window.is_visible()==Some(true))=>{
                    assert_eq!(Some(eframe::viewport_window_for(ctx,state.windows[0].viewport()).unwrap().id()),self.hidden_window,"restoring must reuse the same native window");
                    for (index,session) in state.windows[0].sessions.iter().enumerate() {
                        assert!(Arc::ptr_eq(&session.app.controller.as_ref().unwrap().stats,&self.stats[index]));
                        assert_eq!(self.stats[index].native_media_epoch.load(Ordering::Acquire),self.epochs[index]);
                    }
                    FULLSCREEN_COMMANDS.with(|commands|commands.borrow_mut().insert(state.windows[0].viewport()));
                    self.phase=7;
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
                    click_session(state.windows[0].viewport(),self.second_id,PointerAction::Disconnect,ctx);
                    self.phase = 5;
                    *self.passed.lock().unwrap() = true;
                    println!("two distinct generated sources: attach, tab selection, independent quality, group reorder, context-menu detach and disconnect, independent fullscreen/HUD; display-window launch routing checked with catalog fixtures, epochs unchanged, decode errors=0");
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
                hidden_window:None,
                hidden_since:None,
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
