//! Live sessions belong to a workspace; native windows only present a group.
//! Moving a group never recreates its controller, decoder, or media transport.
use crate::{
    sessions::{ConnectionTarget, DisplayTarget, StreamTarget, VirtualDisplayTarget},
    App,
};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};
mod inventory;
mod budget;
use budget::Candidate as BudgetCandidate;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    pub target: StreamTarget,
    pub payload: Option<zeroize::Zeroizing<String>>,
}
impl Launch {
    pub fn new(
        mut target: StreamTarget,
        payload: Option<&str>,
    ) -> anyhow::Result<Self> {
        target.connection.validate().map_err(anyhow::Error::msg)?;
        let connection = match &mut target.connection {
            ConnectionTarget::NativeDisplay {connection,..}|ConnectionTarget::NativeVirtual {connection,..}=>connection.as_mut(),
            connection=>connection,
        };
        match connection {
            ConnectionTarget::NativeInvitation(path)
            | ConnectionTarget::NativeInternet(path)
            | ConnectionTarget::NativeProfile { snapshot: path, .. } => {
                *path = path.canonicalize()?
            }
            _ => {}
        }
        Ok(Self {
            target,
            payload: payload.map(|s| zeroize::Zeroizing::new(s.into())),
        })
    }
    fn plugin_payload(&self) -> anyhow::Result<Option<zeroize::Zeroizing<String>>> {
        self.target.connection.validate().map_err(anyhow::Error::msg)?;
        anyhow::ensure!(self.target.alias.len() <= 512, "{}", uu_i18n::format!("设备名称过长"));
        match (&self.target.connection, &self.payload) {
            (ConnectionTarget::Plugin{plugin,identity}, Some(payload)) => {
                anyhow::ensure!(!plugin.is_empty()&&plugin.len()<=64&&plugin.bytes().all(|b|b.is_ascii_lowercase()||b.is_ascii_digit()||b==b'-')&&!identity.is_empty()&&identity.len()<=256&&!identity.chars().any(char::is_control),"invalid plugin target identity");
                anyhow::ensure!(!payload.is_empty()&&payload.len()<=crate::sessions::MAX_PLUGIN_PAYLOAD,"invalid plugin launch payload size");
                Ok(Some(payload.clone()))
            }
            (ConnectionTarget::Plugin{..}, None) => anyhow::bail!("plugin launch payload missing"),
            (_, None) => Ok(None),
            (_, Some(_)) => anyhow::bail!("{}", uu_i18n::format!("连接方式与凭据不匹配")),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Select(u64, Option<i32>),
    Reorder {
        source: u64,
        target: u64,
        after: bool,
    },
    ReorderScreen {
        session: u64,
        source: i32,
        target: i32,
        after: bool,
    },
    Detach(u64),
    Merge(u64),
    OpenDisplay { session: u64, screen: i32 },
    OpenAllDisplays(u64),
    CreateDisplay(u64),
    CreateDisplayWithSpec { session:u64, spec:uu_core::virtual_display::Spec },
    CreatePersistentDisplayWithSpec { session:u64, spec:uu_core::virtual_display::Spec },
    ManageDisplays(u64),
    Close(u64),
    ToggleLocal,
    CloseGroup(u64),
    MergeGroup(u64),
    AddDisplay(u64),
    OpenSource { session:u64, source:u64, new_window:bool },
    MoveView { source:u64, target:u64, after:bool },
    Drop { source:u64, group:bool, point:Option<(i32,i32)> },
    RemoveDisplay { session:u64, display:uu_core::virtual_display::LeaseId },
}
#[derive(Clone)]
pub(super) struct View {
    screen:i32,
    session:u64,
    fixed:bool,
    active:bool,
    can_create:bool,
    can_detach:bool,
    can_merge:bool,
}
#[derive(Clone)]
pub struct Group {
    pub(super) last_session: u64,
    pub(super) members:Vec<u64>,
    pub(super) views:Vec<View>,
    pub(super) id: u64,
    pub(super) name: String,
    pub(super) screens: Vec<(i32, String)>,
    pub(super) selected: Option<i32>,
    pub(super) can_open_displays: bool,
    pub(super) can_create_display:bool,
    pub(super) can_detach:bool,
    pub(super) fixed_view:bool,
    pub(super) can_merge:bool,
}
impl Group {
    pub(super) fn view_session(&self,screen:i32)->Option<u64> {self.views.iter().find(|v|v.screen==screen).map(|v|v.session)}
    pub(super) fn creation_session(&self)->u64 {self.views.iter().find(|view|view.can_create).map_or(self.id,|view|view.session)}
    pub(super) fn command_session(&self)->u64 {self.views.iter().find(|view|view.active).map_or_else(||if self.contains(self.last_session){self.last_session}else{self.id},|view|view.session)}
    pub(super) fn activate(&self)->Action {Action::Select(self.command_session(),None)}
    pub(super) fn contains(&self,id:u64)->bool {self.id==id || self.members.contains(&id)}
    pub(super) fn screen_action(&self,screen:i32)->Action {
        self.views.iter().find(|view|view.screen==screen&&view.fixed).or_else(||self.views.iter().find(|view|view.screen==screen)).map_or(Action::Select(self.id,if self.fixed_view {None}else{Some(screen)}),|view|Action::Select(view.session,None))
    }
    pub(super) fn view_group(&self,screen:i32)->Option<Self> {
        let view=self.views.iter().find(|view|view.screen==screen && view.fixed)?;
        let mut group=self.clone();group.id=view.session;group.last_session=view.session;group.can_detach=view.can_detach;group.can_merge=view.can_merge;group.fixed_view=true;group.can_open_displays=false;group.views.clear();Some(group)
    }
    pub(super) fn can_open_display(&self,screen:i32)->bool {
        self.can_open_displays && (self.can_detach || self.selected!=Some(screen))
    }
}
#[derive(Clone)]
pub struct Tabs {
    pub(super) audio_groups:Rc<RefCell<std::collections::HashMap<ConnectionTarget,Rc<crate::audio_controls::Group>>>>,
    pub(super) sources: Vec<(u64,Vec<SourceChoice>)>,
    pub(super) modal: Rc<Cell<bool>>,
    pub(super) presets:crate::virtual_display_presets::Presets,
    pub(super) groups: Vec<Group>,
    pub(super) active: u64,
    pub(super) actions: Rc<RefCell<Vec<Action>>>,
    pub(super) local_ui: Rc<Cell<bool>>,
}
#[derive(Clone, Copy)]
pub(super) struct Drag(pub(super) u64);
#[derive(Clone, Copy)]
pub(super) struct DragScreen {
    pub(super) session: u64,
    pub(super) screen: i32,
}
impl Tabs {
    pub(super) fn audio_group(&self,target:&StreamTarget)->Option<Rc<crate::audio_controls::Group>> {
        self.audio_groups.borrow().get(&base_key(target)).cloned()
    }
    pub fn navigate(&self, action: uu_core::hotkeys::Action) {
        use uu_core::hotkeys::Action as Command;
        let Some(current)=self.groups.iter().position(|group|group.contains(self.active)) else {return;};
        let previous=matches!(action,Command::PreviousDisplay|Command::PreviousComputer);
        let selection=match action {
            Command::PreviousComputer|Command::NextComputer if self.groups.len()>1 => {
                Some(self.groups[adjacent(self.groups.len(),Some(current),previous)].activate())
            },
            Command::PreviousDisplay|Command::NextDisplay => {
                let group=&self.groups[current];
                if group.screens.len()<2 {return;}
                let selected=group.screens.iter().position(|screen|Some(screen.0)==group.selected);
                Some(group.screen_action(group.screens[adjacent(group.screens.len(),selected,previous)].0))
            },
            _=>None,
        };
        if let Some(selection)=selection {self.actions.borrow_mut().push(selection);}
    }
    pub fn owns_input(&self, ctx: &egui::Context) -> bool {
        self.modal.get() || self.local_ui.get()
            || egui::DragAndDrop::has_payload_of_type::<Drag>(ctx)
            || egui::DragAndDrop::has_payload_of_type::<DragScreen>(ctx)
    }
    pub fn show(&self, ui: &mut egui::Ui) {
        self.show_with_controls(ui, 245.);
    }
    pub fn show_with_controls(&self, ui: &mut egui::Ui, controls_width: f32) {
        let rects=crate::stream_tabs_ui::show(self,ui,controls_width);
        #[cfg(test)]
        {
            gui_probe::observe_tab_rects(ui.ctx(),&rects);
            tests::observe_tab_rects(ui.ctx(),rects);
        }
        #[cfg(not(test))]
        let _=rects;
    }

}
fn adjacent(length:usize,current:Option<usize>,previous:bool)->usize {
    match current {
        Some(index) if previous=>(index+length-1)%length,
        Some(index)=>(index+1)%length,
        None if previous=>length-1,
        None=>0,
    }
}
struct Session {
    id: u64,
    activated:Option<std::time::Instant>,
    app: App,
}
fn active_budget_candidate(session:&Session)->Option<BudgetCandidate> {
    let app=&session.app;
    if app.exit_requested||app.controller.closing() {return None;}
    let target=app.stream_target.as_ref()?;
    if !target.is_native(){return None;}
    let handle=app.controller.as_ref()?;
    let (mut width,mut height)=handle.stats.res();
    let screens=app.remote_screens();
    if width==0||height==0 {
        if let Some(screen)=app.displays.selected.and_then(|id|screens.screens.iter().find(|screen|screen.id==id)).or_else(||screens.screens.iter().find(|screen|screen.id==screens.current_id)) {
            width=screen.width;height=screen.height;
        }
    }
    let width=width.max(320);let height=height.max(200);let fps=app.target_fps.clamp(5,240) as u64;
    let codec=handle.stats.codec.lock().map(|codec|codec.to_ascii_lowercase()).unwrap_or_default();
    let codec=if codec.contains("av1"){uu_core::native::VideoCodec::Av1}else if codec.contains("265")||codec.contains("hevc"){uu_core::native::VideoCodec::Hevc}else if codec.contains("pyrowave"){uu_core::native::VideoCodec::Pyrowave}else{uu_core::native::VideoCodec::H264};
    let auto=app.preferences.bitrate==6;
    let fixed=if auto {None} else {app.native_quality_request().bitrate_kbps.or_else(||Some(uu_core::bitrate::recommended_kbps(width,height,fps as u32,codec)))};
    let loss=handle.stats.network_loss.lock().ok().and_then(|loss|loss.as_ref().filter(|loss|loss.fresh()).and_then(|loss|loss.loss_ppm));
    let rtt=handle.stats.rtt_updated_ms.load(std::sync::atomic::Ordering::Relaxed);
    let rtt_ms=(rtt>0&&uu_core::time::now_ms().saturating_sub(rtt)<5_000).then(||handle.stats.rtt_ms.load(std::sync::atomic::Ordering::Relaxed));
    Some(BudgetCandidate{session:session.id,weight:u64::from(width)*u64::from(height)*fps,fixed,budget_kbps:app.preferences.network_budget_mbps.saturating_mul(1000),loss_ppm:loss,rtt_ms,deferred:false})
}
fn deferred_budget_candidate(session:&Session,sources:&[SourceRecord])->Option<BudgetCandidate> {
    let app=&session.app;
    if app.exit_requested||app.controller.closing()||app.controller.is_some()||!app.connect_deferred {return None;}
    let target=app.stream_target.as_ref()?;
    if !target.is_native(){return None;}
    let device=base_key(target);
    let source=target.connection.display().and_then(|display|sources.iter().find(|source|source.device==device&&source.active&&if display.source.is_empty(){source.screen.id==display.id}else{source.screen.display_name==display.source}).map(|source|&source.screen));
    deferred_budget_plan(session.id,&app.preferences.client,target,source,app.local_display())
}
fn deferred_budget_plan(session:u64,settings:&super::settings::ClientSettings,target:&StreamTarget,source:Option<&uu_core::screen_control::RemoteScreen>,local:uu_core::client_settings::LocalDisplay)->Option<BudgetCandidate> {
    if !target.is_native(){return None;}
    let mut width=1920u32;let mut height=1080u32;let mut refresh=None;
    if let Some(virtual_display)=target.connection.virtual_display() {
        width=virtual_display.spec.width;height=virtual_display.spec.height;
        refresh=Some(virtual_display.spec.refresh_millihz/1000).filter(|hz|*hz>1);
    } else if let Some(source)=source {
        width=source.width;height=source.height;refresh=Some(source.refresh_hz).filter(|hz|*hz>1);
    }
    let request=settings.resolved_request(refresh,local,240,None);
    if let Some([bounded_width,bounded_height])=request.bounds {width=bounded_width;height=bounded_height;}
    let width=width.max(320);let height=height.max(200);let fps=u64::from(request.fps);
    let codec=match settings.codec {
        super::settings::CodecPreference::H264=>uu_core::native::VideoCodec::H264,
        super::settings::CodecPreference::Hevc=>uu_core::native::VideoCodec::Hevc,
        super::settings::CodecPreference::Av1=>uu_core::native::VideoCodec::Av1,
        super::settings::CodecPreference::Pyrowave=>uu_core::native::VideoCodec::Pyrowave,
        super::settings::CodecPreference::Auto=>uu_core::native::VideoCodec::H264,
    };
    let auto=settings.bitrate==6;
    let fixed=if auto {None} else {request.bitrate_kbps.or_else(||Some(uu_core::bitrate::recommended_kbps(width,height,fps as u32,codec)))};
    Some(BudgetCandidate{session,weight:u64::from(width)*u64::from(height)*fps,fixed,budget_kbps:settings.network_budget_mbps.saturating_mul(1000),loss_ppm:None,rtt_ms:None,deferred:true})
}

/// A window owns its explicit views, including views that have never connected.
/// A media Session can outlive a removed view only while its cleanup settles.
#[derive(Clone)]
struct OwnedView {
    id:u64,
    launch:Launch,
    source:Option<DisplayTarget>,
    label:String,
    discover_initial:bool,
    // Unopened automatic suggestions may retire; requested views own their tab.
    discovered:bool,
}
#[derive(Clone,Copy,PartialEq,Eq,PartialOrd,Ord)]
pub(super) enum SourceKind { Existing, Persistent, Temporary }
#[derive(Clone)]
struct SourceRecord {
    id:u64,
    device:ConnectionTarget,
    screen:uu_core::screen_control::RemoteScreen,
    label:String,
    kind:SourceKind,
    active:bool,
    managed:Option<uu_core::virtual_display::LeaseId>,
    removable:bool,
}
#[derive(Clone)]
pub(super) struct SourceChoice {
    pub id:u64,
    pub label:String,
    pub kind:SourceKind,
    pub active:bool,
    pub owned:bool,
    pub current_view:Option<u64>,
    pub managed:Option<uu_core::virtual_display::LeaseId>,
    pub removable:bool,
}
fn session_title(app:&App)->String {
    let target=app.stream_target.as_ref().expect("stream session target");
    if let Some(display)=target.connection.virtual_display() {format!("{} · {}",target.alias,uu_i18n::text(if display.persistent {"持久虚拟屏"}else{"临时虚拟屏"}))}
    else if target.connection.display().is_some() {format!("{} · {}",target.alias,app.display_label())}
    else {target.alias.clone()}
}
fn base_key(target:&StreamTarget)->ConnectionTarget {
    StreamTarget{connection:target.connection.base().clone(),alias:String::new(),view_only:target.view_only}.key()
}
fn display_launch(target:&StreamTarget,screen:&uu_core::screen_control::RemoteScreen)->anyhow::Result<Launch> {
    anyhow::ensure!(target.is_native(),"independent displays require a native connection");
    Launch::new(StreamTarget {
        connection:ConnectionTarget::NativeDisplay {
            connection:Box::new(target.connection.base().clone()),
            display:DisplayTarget{id:screen.id,source:screen.display_name.clone()},
        },
        alias:target.alias.clone(),view_only:target.view_only,
    },None)
}
fn virtual_launch(target:&StreamTarget,spec:uu_core::virtual_display::Spec)->anyhow::Result<Launch> {
    anyhow::ensure!(target.is_native(),"temporary displays require a native connection");
    Launch::new(StreamTarget {
        connection:ConnectionTarget::NativeVirtual {
            connection:Box::new(target.connection.base().clone()),
            virtual_display:VirtualDisplayTarget::new(spec),
        },
        alias:target.alias.clone(),view_only:target.view_only,
    },None)
}
struct Window {
    audio_groups:Rc<RefCell<std::collections::HashMap<ConnectionTarget,Rc<crate::audio_controls::Group>>>>,
    views:Vec<OwnedView>,
    sources:Vec<(ConnectionTarget,Vec<SourceChoice>)>,
    discovered_sources:std::collections::HashSet<(ConnectionTarget,String)>,
    merge_available:bool,
    close_confirmation:bool,
    modal:Rc<Cell<bool>>,
    shortcuts:Rc<RefCell<crate::shortcuts::Shortcuts>>,
    vsync_session: Option<u64>,
    id: u64,
    sessions: Vec<Session>,
    pending: Vec<Launch>,
    active: u64,
    visited: Vec<u64>,
    closing: bool,
    close_hidden: bool,
    platform: Option<Rc<crate::platform::Platform>>,
    sizing: Rc<RefCell<crate::window::WindowSizing>>,
    restore_maximized: Rc<Cell<bool>>,
    focus: bool,
    position: Option<egui::Pos2>,
    origin: Option<(i32, i32)>,
    local_ui: Rc<Cell<bool>>,
}
impl Window {
    fn record_launch(&mut self, launch:&Launch, next:&mut u64)->u64 {
        if let Some(view)=self.views.iter().find(|v|v.launch.target.key()==launch.target.key()) {return view.id;}
        let id=*next;*next+=1;
        let source=launch.target.connection.display().cloned();
        let label=source.as_ref().map(|s|s.source.clone()).filter(|s|!s.is_empty()).unwrap_or_else(||
            uu_i18n::text(if launch.target.connection.virtual_display().is_some(){"正在创建虚拟显示器…"}else{"正在连接显示器…"}).into());
        self.views.push(OwnedView { id,launch:launch.clone(),source,label,discovered:false,
            discover_initial:launch.target.is_native()&&!launch.target.connection.is_fixed_display_view() });
        id
    }
    fn close_views(&mut self, ids:&[u64]) {
        let keys:Vec<_>=self.views.iter().filter(|v|ids.contains(&v.id)).map(|v|v.launch.target.key()).collect();
        let bulk=ids.len()>1;
        self.views.retain(|v|!ids.contains(&v.id));
        self.pending.retain(|l|!keys.contains(&l.target.key()));
        for session in self.sessions.iter_mut().filter(|s|ids.contains(&s.id)) {
            session.app.deactivate_session();
            if bulk {session.app.disconnect_without_preview();}else{session.app.disconnect();}
        }
        self.select_remaining();
        if self.views.is_empty() {
            self.closing=true;
            self.close_confirmation=false;
            self.modal.set(false);
        }
    }
    fn close_all(&mut self) {
        self.closing=true;self.close_confirmation=false;self.modal.set(false);
        let ids:Vec<_>=self.views.iter().map(|v|v.id).collect();self.close_views(&ids);
        self.pending.clear();
    }
    fn request_close(&mut self) {
        let devices:std::collections::HashSet<_>=self.views.iter().map(|v|base_key(&v.launch.target)).collect();
        if devices.len()>1 {
            self.close_confirmation=true;self.modal.set(true);
            for session in &mut self.sessions {session.app.release_session_input();}
        } else {self.close_all();}
    }
    fn poll_closing(&mut self,ctx:&egui::Context) {
        if !self.closing {return;}
        if !self.close_hidden {
            ctx.send_viewport_cmd_to(self.viewport(),egui::ViewportCommand::Visible(false));
            self.close_hidden=true;
            // Give the native event loop a pass to hide the window before any
            // completed controller drops its decoder/GPU resources.
            return;
        }
        // The child callback owns its current GL context. The coordinator
        // only schedules it; GPU cleanup must not run in the root's context.
        ctx.request_repaint_of(self.viewport());
    }
    fn new(id: u64, pending: Vec<Launch>, position: Option<egui::Pos2>) -> Self {
        Self {
            audio_groups:Default::default(),
            views:Vec::new(), sources:Vec::new(), discovered_sources:Default::default(), merge_available:false,
            close_confirmation:false,modal:Default::default(),
            shortcuts:Default::default(),
            id,
            sessions: Vec::new(),
            pending,
            active: 0,
            visited: Vec::new(),
            closing: false,
            close_hidden: false,
            platform: None,
            sizing: Default::default(),
            restore_maximized: Default::default(),
            focus: true,
            position,
            origin: None,
            local_ui: Default::default(),
            vsync_session: None,
        }
    }
    fn viewport(&self) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(("stream-window", self.id))
    }
    fn tabs(&self, actions: Rc<RefCell<Vec<Action>>>) -> Tabs {
        let mut groups:Vec<(ConnectionTarget,Group)>=Vec::new();
        let mut recent=std::collections::HashMap::new();
        for session in self.sessions.iter().filter(|s|!s.app.exit_requested) {
            if let Some(target)=&session.app.stream_target {
                let value=recent.entry(base_key(target)).or_insert((session.id,session.activated));
                if session.activated>value.1 {*value=(session.id,session.activated);}
            }
        }
        for view in &self.views {
            let identity=base_key(&view.launch.target);
            let session=self.sessions.iter().find(|s|s.id==view.id&&!s.app.exit_requested);
            let can_create=session.is_some_and(|s|!s.app.controller.closing()&&s.app.controller.as_ref().is_some_and(|h|
                h.stats.virtual_displays.available.load(std::sync::atomic::Ordering::Acquire)&&h.stats.native_displays.initial_supported()));
            let can_open=session.is_some_and(|s|!s.app.controller.closing()&&s.app.controller.as_ref().is_some_and(|h|
                h.stats.capabilities.allows(uu_core::capabilities::Capability::Displays)));
            let at=groups.iter().position(|(key,_)|*key==identity).unwrap_or_else(|| {
                let last_session=recent.get(&identity).map_or(view.id,|v|v.0);
                groups.push((identity,Group { id:view.id,last_session,name:view.launch.target.alias.clone(),
                    members:Vec::new(),views:Vec::new(),screens:Vec::new(),selected:None,
                    can_open_displays:false,can_create_display:false,can_detach:false,fixed_view:true,can_merge:self.merge_available }));
                groups.len()-1
            });
            let group=&mut groups[at].1;
            let screen=group.screens.len() as i32;
            let active=view.id==self.active;
            let can_detach=self.views.len()>1&&!self.closing;
            group.members.push(view.id);
            group.screens.push((screen,view.label.clone()));
            group.views.push(View { screen,session:view.id,fixed:true,active,can_create,can_detach,can_merge:self.merge_available });
            group.can_open_displays|=can_open;
            group.can_create_display|=can_create;
            group.can_detach|=can_detach;
            if active { group.selected=Some(screen);group.last_session=view.id; }
        }
        let sources=groups.iter().map(|(key,group)| (group.id,self.sources.iter().find(|(device,_)|device==key).map(|(_,values)|values.clone()).unwrap_or_default())).collect();
        Tabs {
            audio_groups:self.audio_groups.clone(),
            presets:self.sessions.iter().find(|s|s.id==self.active).map(|s|s.app.preferences.virtual_display_presets.clone()).unwrap_or_default(),
            sources,modal:self.modal.clone(),active:self.active,actions,local_ui:self.local_ui.clone(),
            groups:groups.into_iter().map(|(_,group)|group).collect(),
        }
    }
    fn remember_visit(&mut self,id:u64) {
        self.visited.retain(|previous|*previous!=id);
        self.visited.push(id);
    }
    fn select_remaining(&mut self) {
        self.visited.retain(|id|self.views.iter().any(|view|view.id==*id));
        if self.views.iter().any(|view|view.id==self.active) {return;}
        if let Some(id)=self.visited.last().copied().or_else(||self.views.first().map(|view|view.id)) {self.select(id);}
        else {self.active=0;}
    }
    fn select(&mut self, id: u64) {
        if !self.views.iter().any(|view|view.id==id) {return;}
        self.remember_visit(id);
        if let Some(view)=self.views.iter_mut().find(|view|view.id==id) {view.discovered=false;}
        let Some(view)=self.views.iter().find(|v|v.id==id) else{return;};
        if let Some(session)=self.sessions.iter_mut().find(|s|s.id==id&&!s.app.exit_requested) {
            session.activated=Some(std::time::Instant::now());
        } else if !self.pending.iter().any(|l|l.target.key()==view.launch.target.key()) {
            self.pending.push(view.launch.clone());
        }
        if self.active == id {
            self.route_cached_audio(id);
            return;
        }
        if let Some(old) = self.sessions.iter_mut().find(|s| s.id == self.active) {
            old.app.deactivate_session();
        }
        self.active = id;
        self.route_cached_audio(id);
        if let Some(platform) = &self.platform {
            platform.select_session(id);
        }
        self.sizing.borrow_mut().begin_view();
        for session in &mut self.sessions {
            session.app.active_session = session.id == id;
            if session.app.active_session {
                if let Some(handle) = session.app.controller.as_ref() {
                    let context = session.app.context.clone();
                    handle.frames.set_waker(move || context.request_video_frame());
                }
            }
        }
    }
    fn route_cached_audio(&mut self,_id:u64) {
        use std::sync::atomic::Ordering::{Acquire,Release,Relaxed};
        let mut groups=self.audio_groups.borrow_mut();
        groups.retain(|key,_|self.views.iter().any(|v|base_key(&v.launch.target)==*key));
        for view in &self.sessions {
            if let Some(target)=&view.app.stream_target {
                groups.entry(base_key(target)).or_insert_with(||Rc::new(crate::audio_controls::Group::new(crate::audio_controls::Playback{muted:view.app.preferences.muted,volume:view.app.preferences.volume})));
            }
        }
        for (key,group) in groups.iter() {
            let eligible=|s:&&Session|s.app.stream_target.as_ref().is_some_and(|t|base_key(t)==*key)
                && !s.app.exit_requested && !s.app.controller.closing()
                && s.app.controller.as_ref().is_some_and(|h|!h.stopped.load(Acquire)&&h.stats.capabilities.allows(uu_core::capabilities::Capability::Audio));
            // Keep a working audio source when switching video tabs. Only
            // replace it after disconnect, failure or a permission change.
            let owner=self.sessions.iter().filter(eligible).find(|s|Some(s.id)==group.owner.get())
                .or_else(||self.sessions.iter().filter(eligible).find(|s|s.id==self.active))
                .or_else(||self.sessions.iter().find(eligible));
            group.owner.set(owner.map(|s|s.id));
            *group.stats.borrow_mut()=owner.and_then(|s|s.app.controller.as_ref()).map(|h|Arc::downgrade(&h.stats)).unwrap_or_default();
            let playback=group.playback.get();
            // Stop the previous source before enabling its replacement, even
            // when the replacement appears earlier in the tab order.
            for view in self.sessions.iter().filter(|s|Some(s.id)!=group.owner.get()&&s.app.stream_target.as_ref().is_some_and(|t|base_key(t)==*key)) {
                if let Some(handle)=view.app.controller.as_ref() {handle.stats.audio.parked.store(true,Release);}
            }
            for view in self.sessions.iter_mut().filter(|s|s.app.stream_target.as_ref().is_some_and(|t|base_key(t)==*key)) {
                view.app.audio_parked=Some(view.id)!=group.owner.get();
                if let Some(handle)=view.app.controller.as_ref() {
                    handle.stats.audio.parked.store(view.app.audio_parked,Release);
                    handle.stats.audio.muted.store(playback.muted,Relaxed);
                    handle.stats.audio.volume.store(playback.volume,Relaxed);
                }
            }
        }
    }
    fn resume_local_desktop(&mut self) {
        self.select_remaining();
        for session in &mut self.sessions {
            session.app.active_session=session.id==self.active&&!session.app.exit_requested;
            if session.app.active_session {
                if let Some(handle)=session.app.controller.as_ref() {
                    let context=session.app.context.clone();
                    handle.frames.set_waker(move||context.request_video_frame());
                }
            }
        }
        self.route_cached_audio(self.active);
        self.sizing.borrow_mut().begin_view();
    }
    fn take_session(&mut self,id:u64)->Session {
        let index=self.sessions.iter().position(|session|session.id==id).expect("live session");
        let mut session=self.sessions.remove(index);
        session.app.audio_parked=true;
        if let Some(handle)=session.app.controller.as_ref() {handle.stats.audio.parked.store(true,std::sync::atomic::Ordering::Release);}
        session.app.deactivate_session();
        session.app.context.suspend();
        self.select_remaining();
        session
    }
    fn adopt_session(&mut self,mut session:Session) {
        if let Some(active)=self.sessions.iter_mut().find(|s|s.id==self.active) {active.app.deactivate_session();}
        session.app.window_sizing=self.sizing.clone();
        session.app.shortcuts=self.shortcuts.clone();
        session.app.restore_maximized=self.restore_maximized.clone();
        session.app.active_session=false;
        let id=session.id;
        session.activated=Some(std::time::Instant::now());
        self.sessions.push(session);
        // Activation belongs to the destination viewport's next pass, after
        // its platform/context and wakeups have been bound by show().
        self.active=id;
        self.remember_visit(id);
        self.route_cached_audio(id);
        self.sizing.borrow_mut().begin_view();
        self.focus=true;
    }
    fn show(
        &mut self,
        ctx: &egui::Context,
        gl: &Option<Arc<eframe::glow::Context>>,
        vsync: bool,
        local_desktop: bool,
        next: &mut u64,
        actions: Rc<RefCell<Vec<Action>>>,
        gamepad: &Rc<RefCell<crate::gamepad::Reader>>,
    ) {
        #[cfg(test)]
        tests::observe_screenshot(ctx);
        if self.closing {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            if self.close_hidden {
                for session in &mut self.sessions {
                    session.app.poll_controller(ctx);
                    if session.app.audio_save_after.take().is_some() {session.app.persist_preferences();}
                }
                self.sessions.retain(|s|s.app.controller.is_some());
            }
            ctx.request_repaint_of(egui::ViewportId::ROOT);
            return;
        }
        if local_desktop {
            let tabs=self.tabs(actions);
            for session in &mut self.sessions {
                session.app.workspace_tabs=Some(tabs.clone());
                session.app.active_session=false;
                session.app.workspace_poll(ctx);
                session.app.poll_controller(ctx);
                session.app.poll_virtual_display_state();
            }
            self.route_cached_audio(self.active);
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        if self.platform.is_none() {
            let Some(native) = eframe::viewport_window(ctx) else {
                ctx.request_repaint();
                return;
            };
            self.platform = Some(Rc::new(crate::platform::Platform::for_window(
                native.as_ref(),
                ctx,
                self.origin,
            )));
            eframe::request_vsync(ctx, vsync);
        }
        // Sessions can join an existing surface as well as a newly created
        // one. Retarget every moved session before it can receive local input.
        for session in &mut self.sessions {
            if !Rc::ptr_eq(&session.app.platform,self.platform.as_ref().unwrap()) || session.app.context.viewport()!=ctx.viewport_id() {
                #[cfg(test)]
                let old_viewport=session.app.context.viewport();
                session.app.shortcuts=self.shortcuts.clone();
                session.app.platform = self.platform.as_ref().unwrap().clone();
                session.app.context.bind(ctx);
                session.app.clipboard_window.store(
                    session.app.platform.window_id(),
                    std::sync::atomic::Ordering::Release,
                );
                if let Some(handle) = session.app.controller.as_ref() {
                    let context = session.app.context.clone();
                    handle.frames.set_waker(move || context.request_video_frame());
                }
                #[cfg(test)]
                tests::surface_bound(session.id,old_viewport,ctx.viewport_id(),session.app.controller.is_some());
            }
        }
        // Fullscreen belongs to this native window. Parked sessions must read
        // the same state before a late controller attach or a tab switch.
        if let Some(fullscreen)=ctx.input(|i|i.viewport().fullscreen) {
            for session in &mut self.sessions {session.app.fullscreen=fullscreen;}
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.request_close();
            if self.closing {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                ctx.request_repaint_of(egui::ViewportId::ROOT);
                return;
            }
        }
        let had_pending=!self.pending.is_empty();
        for launch in std::mem::take(&mut self.pending) {
            let plugin_payload = match launch.plugin_payload() {
                Ok(value) => value,
                Err(_) => continue,
            };
            let id = self.record_launch(&launch,next);
            let selected=self.active==0||self.active==id;
            let deferred=launch.target.is_native();
            let mut app = App::new_session(
                ctx,
                gl.clone(),
                self.platform.as_ref().unwrap().clone(),
                Some(launch.target),
                vsync,
                plugin_payload,
                deferred,
            );
            app.fullscreen=ctx.input(|input|input.viewport().fullscreen.unwrap_or(false));
            app.window_sizing = self.sizing.clone();
            app.shortcuts=self.shortcuts.clone();
            app.gamepad = gamepad.clone();
            app.restore_maximized = self.restore_maximized.clone();
            app.audio_parked=true;
            if !selected {app.deactivate_session();}
            self.sessions.push(Session { id, activated:None, app });
            if selected {self.select(id);}
        }
        if had_pending {ctx.request_repaint_of(egui::ViewportId::ROOT);}
        self.route_cached_audio(self.active);
        let tabs = self.tabs(actions);
        let mut active = None;
        for (index, session) in self.sessions.iter_mut().enumerate() {
            session.app.workspace_tabs = Some(tabs.clone());
            session.app.active_session = session.id == self.active && !self.closing && !session.app.exit_requested;
            if session.app.active_session {
                active = Some(index);
            } else {
                session.app.workspace_poll(ctx);
                session.app.poll_controller(ctx);
                session.app.poll_virtual_display_state();
                if session.app.exit_requested && session.app.audio_save_after.take().is_some() {
                    session.app.persist_preferences();
                }
            }
        }
        if let Some(index) = active {
            let app = &mut self.sessions[index].app;
            app.context.bind(ctx);
            if self.vsync_session != Some(self.active) {
                eframe::request_vsync(ctx, app.preferences.vsync);
                self.vsync_session = Some(self.active);
            }
            self.platform.as_ref().unwrap().select_session(self.active);
            app.update_frame(ctx, eframe::viewport_cpu_usage(ctx));
            #[cfg(test)]
            tests::apply_fullscreen_command(app,ctx);
        } else {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label(uu_i18n::text("正在释放连接…"));
            });
        }
        let closed:Vec<_>=self.sessions.iter().filter(|s|s.app.exit_requested).map(|s|s.id).collect();
        self.views.retain(|v|!closed.contains(&v.id));
        self.sessions.retain(|s| !s.app.exit_requested || s.app.controller.is_some());
        self.select_remaining();
        self.route_cached_audio(self.active);
        if self.close_confirmation {
            if ctx.input(|i|i.key_pressed(egui::Key::Escape)) {self.close_confirmation=false;self.modal.set(false);}
        }
        if self.close_confirmation {
            egui::Window::new(uu_i18n::text("退出此窗口的所有串流？")).id("workspace-close-confirmation".into())
                .collapsible(false).resizable(false).show(ctx,|ui| {
                    ui.label(uu_i18n::text("此窗口包含多台电脑。退出将断开此窗口的所有屏幕。"));
                    ui.horizontal(|ui| {
                        if ui.add(crate::theme::primary_button(uu_i18n::text("退出所有串流"))).clicked() {self.close_all();}
                        if ui.add(crate::theme::secondary_button(uu_i18n::text("取消"))).clicked() {self.close_confirmation=false;self.modal.set(false);}
                    });
                });
        }
        if std::mem::take(&mut self.focus) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.platform.as_ref().unwrap().focus_window();
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

struct State {
    sources:Vec<SourceRecord>,
    inquiries:std::collections::HashMap<u64,(std::time::Instant,Option<u64>)>,
    input_owner: Option<u64>,
    gamepad: Rc<RefCell<crate::gamepad::Reader>>,
    windows: Vec<Window>,
    gl: Option<Arc<eframe::glow::Context>>,
    vsync: bool,
    next: u64,
    actions: Rc<RefCell<Vec<Action>>>,
    local_desktop: bool,
    restore_window: Option<u64>,
    budget:budget::Controller,
}
impl Workspace {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        launch: Launch,
        owner: uu_core::local_workspace::Owner,
        vsync: bool,
        origin: Option<(i32, i32)>,
    ) -> Self {
        crate::setup_style(&cc.egui_ctx);
        if let Some(gl) = &cc.gl {
            crate::video_texture::enable_gpu_video(gl);
        }
        let context = cc.egui_ctx.clone();
        owner.set_waker(move || context.request_repaint_of(egui::ViewportId::ROOT));
        let mut window = Window::new(1, vec![launch], None);
        window.origin = origin;
        STATE.with(|state| {
            *state.borrow_mut() = Some(State {
                sources:Vec::new(),
                inquiries:Default::default(),
                windows: vec![window],
                gl: cc.gl.clone(),
                vsync,
                next: 2,
                actions: Default::default(),
                gamepad: Default::default(),
                input_owner: None,
                local_desktop: false,
                restore_window: None,
                budget:Default::default(),
            })
        });
        Self { owner }
    }
}
// Glow's UI and window lifetimes are confined to the event-loop thread. The
// deferred callback stores only an ID; session objects never cross threads.
std::thread_local! {static STATE:RefCell<Option<State>>=const {RefCell::new(None)};}
pub struct Workspace {
    owner: uu_core::local_workspace::Owner,
}
impl State {
    fn has_deferred_connect(&self)->bool {
        self.windows.iter().flat_map(|window|&window.sessions).any(|session|session.app.connect_deferred&&session.app.controller.is_none()&&!session.app.exit_requested)
    }
    fn rebalance_budgets(&mut self,force:bool) {
        let windows = &self.windows;
        let sources = &self.sources;
        let Some(updates) = self.budget.rebalance(std::time::Instant::now(), force, || {
            let mut groups = std::collections::HashMap::new();
            for window in windows {
                for session in &window.sessions {
                    let Some(target) = session.app.stream_target.as_ref() else { continue; };
                    let candidate = active_budget_candidate(session)
                        .or_else(|| deferred_budget_candidate(session, sources));
                    if let Some(candidate) = candidate {
                        groups.entry(base_key(target)).or_insert_with(Vec::new).push(candidate);
                    }
                }
            }
            groups
        }) else { return; };
        let mut deferred=Vec::new();
        for budget::Update { session: id, bitrate_kbps: target, deferred_device: device } in updates {
            for window in &mut self.windows {
                if let Some(session)=window.sessions.iter_mut().find(|session|session.id==id) {
                    if session.app.budget_alloc_kbps!=target {
                        session.app.budget_alloc_kbps=target;
                        if target.is_some()&&session.app.controller.is_some(){session.app.send_capture_settings();}
                    }
                    if let Some(device)=device {deferred.push((id,device));}
                    break;
                }
            }
        }
        for (id,device) in deferred {
            if self.windows.iter().any(|window|window.pending.iter().any(|launch|base_key(&launch.target)==device)){continue;}
            if let Some(session)=self.windows.iter_mut().flat_map(|window|&mut window.sessions).find(|session|session.id==id) {
                if session.app.connect_deferred {session.app.connect();}
            }
        }
    }
    fn toggle_local_desktop(&mut self,ctx:&egui::Context) {
        if self.windows.iter().all(|window|window.closing) {return;}
        if self.local_desktop {
            for window in self.windows.iter().filter(|window|!window.closing) {
                ctx.send_viewport_cmd_to(window.viewport(),egui::ViewportCommand::Visible(true));
            }
            self.local_desktop=false;
            let selected=self.restore_window.and_then(|id|self.windows.iter().position(|window|window.id==id))
                .or_else(||self.windows.iter().position(|window|!window.views.is_empty()))
                .or_else(||self.windows.iter().position(|window|!window.closing));
            if let Some(index)=selected {
                let window=&mut self.windows[index];
                window.resume_local_desktop();window.focus=true;
                ctx.send_viewport_cmd_to(window.viewport(),egui::ViewportCommand::Focus);
            }
            self.restore_window=None;
        } else {
            self.restore_window=self.input_owner.and_then(|id|self.windows.iter().find(|window|window.views.iter().any(|view|view.id==id)).map(|window|window.id))
                .or_else(||self.windows.iter().find(|window|window.sessions.iter().any(|session|session.id==window.active&&!session.app.exit_requested)).map(|window|window.id))
                .or_else(||self.windows.first().map(|window|window.id));
            for window in &mut self.windows {
                for session in &mut window.sessions {session.app.deactivate_session();}
                ctx.send_viewport_cmd_to(window.viewport(),egui::ViewportCommand::Visible(false));
            }
            self.input_owner=None;
            self.local_desktop=true;
        }
        ctx.request_repaint();
    }
    fn displaying_session(&self,identity:&ConnectionTarget,display:&DisplayTarget,excluding:Option<u64>)->Option<u64> {
        self.windows.iter().filter(|w|!w.closing).flat_map(|w|&w.views).find(|v|
            Some(v.id)!=excluding&&base_key(&v.launch.target)==*identity&&v.source.as_ref().is_some_and(|s|
                if !display.source.is_empty(){s.source==display.source}else{s.id==display.id})).map(|v|v.id)
    }
    fn move_view(&mut self,id:u64,target_id:u64,ctx:&egui::Context) {
        let Some(source_index)=self.windows.iter().position(|w|!w.closing&&w.views.iter().any(|v|v.id==id)) else{return;};
        if self.windows[source_index].id==target_id {return;}
        if !self.windows.iter().any(|w|w.id==target_id&&!w.closing) {return;}
        let source=&mut self.windows[source_index];
        let at=source.views.iter().position(|v|v.id==id).unwrap();
        let view=source.views.remove(at);
        let audio_key=base_key(&view.launch.target);
        let audio=source.audio_groups.borrow().get(&audio_key).map(|group| {
            let mut playback=group.playback.get();
            // A second independent window starts silent. A whole-device
            // merge installs its shared state before moving individual tabs.
            playback.muted|=source.views.iter().any(|v|base_key(&v.launch.target)==audio_key);
            playback
        });
        source.pending.retain(|l|l.target.key()!=view.launch.target.key());
        let session=source.sessions.iter().any(|s|s.id==id).then(||source.take_session(id));
        source.select_remaining();
        ctx.request_repaint_of(source.viewport());
        if self.input_owner==Some(id) {self.input_owner=None;}
        let target=self.windows.iter_mut().find(|w|w.id==target_id).unwrap();
        if let Some(playback)=audio {
            target.audio_groups.borrow_mut().entry(audio_key).or_insert_with(||Rc::new(crate::audio_controls::Group::new(playback)));
        }
        target.views.push(view);
        if let Some(session)=session {target.adopt_session(session);}else{target.select(id);}
        target.focus=true;ctx.request_repaint_of(target.viewport());
    }
    fn detach(&mut self,id:u64,ctx:&egui::Context) {
        let Some(source)=self.windows.iter().find(|w|!w.closing&&w.views.iter().any(|v|v.id==id)) else{return;};
        if source.views.len()<2 {return;}
        let position=ctx.input_for(source.viewport(),|i|i.viewport().outer_rect.map(|r|r.min+egui::vec2(36.,36.)));
        let target=self.next;self.next+=1;
        self.windows.push(Window::new(target,Vec::new(),position));
        self.move_view(id,target,ctx);
    }
    fn oldest_other_window(&self,source_id:u64)->Option<u64> {
        self.windows.iter().filter(|w|w.id!=source_id&&!w.closing&&(!w.views.is_empty()||!w.pending.is_empty()))
            .map(|w|w.id).min()
    }
    fn merge(&mut self,id:u64,ctx:&egui::Context) {
        let Some(source)=self.windows.iter().find(|w|w.views.iter().any(|v|v.id==id)).map(|w|w.id) else{return;};
        if let Some(target)=self.oldest_other_window(source) {self.move_view(id,target,ctx);}
    }
    fn group_ids(&self,id:u64)->Vec<u64> {
        let Some(window)=self.windows.iter().find(|w|!w.closing&&w.views.iter().any(|v|v.id==id)) else{return Vec::new();};
        let device=base_key(&window.views.iter().find(|v|v.id==id).unwrap().launch.target);
        window.views.iter().filter(|v|base_key(&v.launch.target)==device).map(|v|v.id).collect()
    }
    fn merge_group(&mut self,id:u64,ctx:&egui::Context) {
        let ids=self.group_ids(id);
        let Some(source)=self.windows.iter().find(|w|w.views.iter().any(|v|v.id==id)).map(|w|w.id) else{return;};
        if let Some(target)=self.oldest_other_window(source) {
            let window=self.windows.iter().find(|w|w.id==source).unwrap();
            if let Some(view)=window.views.iter().find(|v|v.id==id) {
                let key=base_key(&view.launch.target);
                let playback=window.audio_groups.borrow().get(&key).map(|g|g.playback.get());
                if let Some(playback)=playback {
                    self.windows.iter().find(|w|w.id==target).unwrap().audio_groups.borrow_mut()
                        .entry(key).or_insert_with(||Rc::new(crate::audio_controls::Group::new(playback)));
                }
            }
            for view in ids {self.move_view(view,target,ctx);}
        }
    }
    fn fork_virtual_display(&mut self,session:u64,spec:Option<uu_core::virtual_display::Spec>,persistent:bool) {
        let Some(index)=self.windows.iter().position(|w|!w.closing&&w.views.iter().any(|v|v.id==session)) else{return;};
        let Some(source)=self.windows[index].sessions.iter_mut().find(|s|s.id==session&&!s.app.exit_requested&&!s.app.controller.closing()) else{return;};
        if !source.app.controller.as_ref().is_some_and(|h|h.stats.virtual_displays.available.load(std::sync::atomic::Ordering::Acquire)) {
            source.app.displays.notice=uu_i18n::text("远端未提供虚拟显示器管理，请更新后台并授予管理服务权限").into();return;
        }
        let target=source.app.stream_target.as_ref().unwrap();
        match virtual_launch(target,spec.unwrap_or_else(||source.app.preferences.virtual_display_presets.default_spec())) {
            Ok(mut launch)=>{
                if let ConnectionTarget::NativeVirtual{virtual_display,..}=&mut launch.target.connection {virtual_display.persistent=persistent;}
                let id=self.windows[index].record_launch(&launch,&mut self.next);
                self.windows[index].select(id);self.windows[index].focus=true;
            },
            Err(error)=>source.app.displays.notice=error.to_string(),
        }
    }
    fn open_source(&mut self,session:u64,source:u64,new_window:bool,ctx:&egui::Context) {
        let Some(record)=self.sources.iter().find(|s|s.id==source&&s.active).cloned() else{return;};
        if self.source_owned(&record) {return;}
        let Some(index)=self.windows.iter().position(|w|!w.closing&&w.views.iter().any(|v|v.id==session)) else{return;};
        let view=self.windows[index].views.iter().find(|v|v.id==session).unwrap();
        if base_key(&view.launch.target)!=record.device {return;}
        let Ok(launch)=display_launch(&view.launch.target,&record.screen) else{return;};
        let index=if new_window {
            let window_id=self.next;self.next+=1;
            self.windows.push(Window::new(window_id,Vec::new(),None));self.windows.len()-1
        }else{index};
        let id=self.windows[index].record_launch(&launch,&mut self.next);
        self.windows[index].views.iter_mut().find(|v|v.id==id).unwrap().label=record.label;
        self.windows[index].select(id);self.windows[index].focus=true;
        ctx.request_repaint_of(self.windows[index].viewport());
    }
    fn add_display(&mut self,id:u64,ctx:&egui::Context) {
        let Some(device)=self.windows.iter().flat_map(|w|&w.views).find(|v|v.id==id).map(|v|base_key(&v.launch.target)) else{return;};
        let choice=self.sources.iter().filter(|s|s.device==device&&s.active&&!self.source_owned(s)).min_by_key(|s|(s.kind,s.id)).map(|s|s.id);
        if let Some(source)=choice {self.open_source(id,source,false,ctx);}else{
            let manager=self.group_ids(id).into_iter().find(|id|self.windows.iter().flat_map(|w|&w.sessions).any(|s|s.id==*id&&s.app.controller.as_ref().is_some_and(|h|h.stats.virtual_displays.available.load(std::sync::atomic::Ordering::Acquire))));
            self.fork_virtual_display(manager.unwrap_or(id),None,false);
        }
    }
    fn open_display(&mut self,session:u64,screen:i32,ctx:&egui::Context) {
        let Some(runtime)=self.windows.iter().flat_map(|w|&w.sessions).find(|s|s.id==session) else{return;};
        let Some(screen)=runtime.app.remote_screens().screens.into_iter().find(|s|s.id==screen) else{return;};
        let device=base_key(runtime.app.stream_target.as_ref().unwrap());
        let display=DisplayTarget{id:screen.id,source:screen.display_name.clone()};
        if let Some(existing)=self.displaying_session(&device,&display,None) {self.detach(existing,ctx);return;}
        if let Some(source)=self.sources.iter().find(|s|s.device==device&&s.screen.display_name==screen.display_name).map(|s|s.id) {self.open_source(session,source,true,ctx);}
    }
    fn attach(&mut self,launch:Launch)->bool {
        if launch.plugin_payload().is_err() {return false;}
        let key=launch.target.key();let device=base_key(&launch.target);let requested=launch.target.connection.display();
        for window in self.windows.iter_mut().filter(|w|!w.closing) {
            if let Some(view)=window.views.iter().find(|v|v.launch.target.key()==key ||
                requested.is_some_and(|d|base_key(&v.launch.target)==device&&v.source.as_ref().is_some_and(|s|!d.source.is_empty()&&s.source==d.source)) ||
                (!launch.target.connection.is_fixed_display_view()&&base_key(&v.launch.target)==device)) {
                let id=view.id;window.select(id);window.focus=true;return true;
            }
            if window.pending.iter().any(|l|l.target.key()==key) {window.focus=true;return true;}
        }
        let index=if launch.target.connection.is_fixed_display_view()||!self.windows.iter().any(|w|!w.closing) {
            let id=self.next;self.next+=1;self.windows.push(Window::new(id,Vec::new(),None));self.windows.len()-1
        }else{self.windows.iter().position(|w|!w.closing).unwrap()};
        let id=self.windows[index].record_launch(&launch,&mut self.next);
        self.windows[index].select(id);self.windows[index].focus=true;true
    }
    fn cache_display(&mut self,session:u64,screen:i32,ctx:&egui::Context)->bool {
        let Some(index)=self.windows.iter().position(|w|w.views.iter().any(|v|v.id==session)) else{return false;};
        let Some(runtime)=self.windows[index].sessions.iter().find(|s|s.id==session) else{return false;};
        if !runtime.app.stream_target.as_ref().is_some_and(StreamTarget::is_native) {return false;}
        let device=base_key(runtime.app.stream_target.as_ref().unwrap());
        let Some(screen)=runtime.app.remote_screens().screens.into_iter().find(|s|s.id==screen) else{return true;};
        if screen.kind==uu_core::screen_control::PORTAL_CHOICE_KIND {return false;}
        if let Some(id)=self.windows[index].views.iter().find(|v|base_key(&v.launch.target)==device&&v.source.as_ref().is_some_and(|s|s.source==screen.display_name)).map(|v|v.id) {
            self.windows[index].select(id);ctx.request_repaint_of(self.windows[index].viewport());return true;
        }
        if let Some(source)=self.sources.iter().find(|s|s.device==device&&s.screen.display_name==screen.display_name).map(|s|s.id) {self.open_source(session,source,false,ctx);}
        true
    }
    fn close(&mut self,ids:&[u64],ctx:&egui::Context) {
        for window in &mut self.windows {
            if window.views.iter().any(|v|ids.contains(&v.id)) {window.close_views(ids);ctx.request_repaint_of(window.viewport());}
        }
        if self.input_owner.is_some_and(|id|ids.contains(&id)) {self.input_owner=None;}
    }
    fn apply(&mut self,action:Action,ctx:&egui::Context) {
        #[cfg(test)] eprintln!("generated workspace action={action:?}");
        match action {
            Action::Select(id,screen)=>{
                if screen.is_some_and(|s|self.cache_display(id,s,ctx)) {return;}
                if let Some(window)=self.windows.iter_mut().find(|w|w.views.iter().any(|v|v.id==id)) {
                    window.select(id);window.focus=true;
                    if let Some(screen)=screen {if let Some(session)=window.sessions.iter_mut().find(|s|s.id==id) {session.app.select_native_display(screen);}}
                    ctx.request_repaint_of(window.viewport());
                }
            },
            Action::Close(id)=>self.close(&[id],ctx),
            Action::ToggleLocal=>self.toggle_local_desktop(ctx),
            Action::CloseGroup(id)=>{let ids=self.group_ids(id);self.close(&ids,ctx);},
            Action::Detach(id)=>self.detach(id,ctx),
            Action::Merge(id)=>self.merge(id,ctx),
            Action::MergeGroup(id)=>self.merge_group(id,ctx),
            Action::AddDisplay(id)=>self.add_display(id,ctx),
            Action::OpenSource{session,source,new_window}=>self.open_source(session,source,new_window,ctx),
            Action::OpenDisplay{session,screen}=>self.open_display(session,screen,ctx),
            Action::CreateDisplay(id)=>self.fork_virtual_display(id,None,false),
            Action::CreateDisplayWithSpec{session,spec}=>self.fork_virtual_display(session,Some(spec),false),
            Action::CreatePersistentDisplayWithSpec{session,spec}=>self.fork_virtual_display(session,Some(spec),true),
            Action::ManageDisplays(id)=>if let Some(s)=self.windows.iter_mut().flat_map(|w|&mut w.sessions).find(|s|s.id==id) {s.app.manage_remote_virtual_displays();},
            Action::RemoveDisplay{session,display}=>{
                let allowed=self.sources.iter().find(|s|s.managed==Some(display))
                    .is_some_and(|source|source.removable&&!self.source_owned(source)&&self.windows.iter().flat_map(|w|&w.views)
                        .any(|view|view.id==session&&base_key(&view.launch.target)==source.device));
                if allowed {if let Some(s)=self.windows.iter_mut().flat_map(|w|&mut w.sessions).find(|s|s.id==session) {s.app.remove_remote_virtual_display(display);}}
            },
            Action::OpenAllDisplays(id)=>{for view in self.group_ids(id) {self.detach(view,ctx);}},
            Action::MoveView{source,target,after}=>{
                let Some(index)=self.windows.iter().position(|w|!w.closing&&w.views.iter().any(|v|v.id==target)) else{return;};
                let destination=self.windows[index].id;self.move_view(source,destination,ctx);
                let window=self.windows.iter_mut().find(|w|w.id==destination).unwrap();
                if source!=target {if let Some(at)=window.views.iter().position(|v|v.id==source) {
                    let view=window.views.remove(at);let at=window.views.iter().position(|v|v.id==target).unwrap();
                    window.views.insert(at+usize::from(after),view);
                }}
            },
            Action::Drop{source,group,point}=>{
                let Some(origin)=self.windows.iter().find(|w|w.views.iter().any(|v|v.id==source)).map(|w|w.id) else{return;};
                let ids=if group{self.group_ids(source)}else{vec![source]};
                let destination=point.and_then(|point|self.windows.iter().filter(|w|w.id!=origin&&!w.closing&&!w.views.is_empty())
                    .find(|w|w.platform.as_ref().is_some_and(|p|p.contains_screen_point(point))).map(|w|w.id));
                if let Some(destination)=destination {for id in ids {self.move_view(id,destination,ctx);}}
                else if self.windows.iter().find(|w|w.id==origin).is_some_and(|w|w.views.len()>ids.len()) {
                    let destination=self.next;self.next+=1;self.windows.push(Window::new(destination,Vec::new(),None));
                    for id in ids {self.move_view(id,destination,ctx);}
                }
            },
            Action::Reorder{source,target,after}=>{
                let ids=self.group_ids(source);
                let Some(index)=self.windows.iter().position(|w|!w.closing&&w.views.iter().any(|v|v.id==target)) else{return;};
                let destination=self.windows[index].id;
                for id in &ids {self.move_view(*id,destination,ctx);}
                let window=self.windows.iter_mut().find(|w|w.id==destination).unwrap();
                let Some(target)=window.views.iter().find(|v|v.id==target).map(|v|base_key(&v.launch.target)) else{return;};
                if window.views.iter().any(|v|ids.contains(&v.id)&&base_key(&v.launch.target)==target) {return;}
                let (moving,mut remaining):(Vec<_>,Vec<_>)=std::mem::take(&mut window.views).into_iter().partition(|v|ids.contains(&v.id));
                let at=if after {remaining.iter().rposition(|v|base_key(&v.launch.target)==target).unwrap()+1}else{remaining.iter().position(|v|base_key(&v.launch.target)==target).unwrap()};
                remaining.splice(at..at,moving);window.views=remaining;
            },
            Action::ReorderScreen{session,source,target,after}=>{
                if let Some(window)=self.windows.iter_mut().find(|w|w.views.iter().any(|v|v.id==session)) {
                    let tabs=window.tabs(Default::default());
                    if let Some(group)=tabs.groups.iter().find(|g|g.contains(session)) {
                        let (Some(source),Some(target))=(group.views.iter().find(|v|v.screen==source),group.views.iter().find(|v|v.screen==target)) else{return;};
                        let action=Action::MoveView{source:source.session,target:target.session,after};self.apply(action,ctx);
                    }
                }
            },
        }
    }
}
impl eframe::App for Workspace {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            let state = state.as_mut().expect("workspace UI thread");
            let hidden=crate::boss_key::hidden();
            for session in state.windows.iter_mut().flat_map(|w|w.sessions.iter_mut()) {
                if session.app.boss_hidden!=hidden {
                    session.app.boss_hidden=hidden;
                    if hidden{session.app.release_session_input();}
                }
            }
            if hidden{state.input_owner=None;}
            for window in &state.windows {crate::boss_key::normal_visibility(window.viewport(),!window.closing&&!state.local_desktop);}
            crate::boss_key::apply();
            if crate::platform::take_global_toggle(){state.toggle_local_desktop(ctx);}
            state.sync_owned_views();
            let force_budget=state.has_deferred_connect();
            state.rebalance_budgets(force_budget);
            while let Some(request) = self.owner.try_recv() {
                let accepted = serde_json::from_slice::<Launch>(&request.bytes)
                    .ok()
                    .is_some_and(|launch| state.attach(launch));
                request.finish(accepted);
            }
            let actions = std::mem::take(&mut *state.actions.borrow_mut());
            for action in actions {
                state.apply(action, ctx);
            }
            state.sync_owned_views();
            for window in &mut state.windows {window.poll_closing(ctx);}
            state
                .windows
                .retain(|w| !w.views.is_empty() || !w.sessions.is_empty() || !w.pending.is_empty());
            for window in &mut state.windows {
                let id = window.id;
                let viewport = window.viewport();
                let title=window.sessions.iter().find(|s|s.id==window.active).map(|s|format!("{} · {}",session_title(&s.app),crate::brand::NAME))
                    .unwrap_or_else(||uu_i18n::format!("串流 · {}",crate::brand::NAME));
                let mut builder = egui::ViewportBuilder::default()
                    .with_title(title)
                    .with_inner_size([1100., 760.])
                    .with_min_inner_size([320., 200.])
                    .with_visible(!window.closing&&!state.local_desktop&&!crate::boss_key::hidden())
                    .with_decorations(false)
                    .with_icon(crate::brand::window_icon());
                if let Some(position) = window.position.take() {
                    builder = builder.with_position(position);
                }
                ctx.show_viewport_deferred(viewport, builder, move |ctx, _| {
                    STATE.with(|state| {
                        let mut state = state.borrow_mut();
                        let state = state.as_mut().expect("workspace UI thread");
                        let focused = !state.local_desktop&&!crate::boss_key::hidden()&&ctx.input(|i| i.focused);
                        let current = state.windows.iter().find(|w| w.id == id).and_then(|w|(w.active!=0).then_some(w.active));
                        let old_here = state.windows.iter().find(|w| w.id == id).is_some_and(|w| {
                            w.sessions.iter().any(|s| Some(s.id) == state.input_owner)
                        });
                        if focused && current != state.input_owner || !focused && old_here {
                            if let Some(previous) = state
                                .windows
                                .iter_mut()
                                .flat_map(|w| w.sessions.iter_mut())
                                .find(|s| Some(s.id) == state.input_owner)
                            {
                                previous.app.release_session_input();
                            }
                            state.input_owner = if focused { current } else { None };
                        }
                        let State {
                            windows,
                            gl,
                            vsync,
                            next,
                            actions,
                            gamepad,
                            input_owner,
                            local_desktop,
                            ..
                        } = state;
                        if let Some(window) = windows.iter_mut().find(|w| w.id == id) {
                            window.show(ctx, gl, *vsync, *local_desktop||crate::boss_key::hidden(), next, actions.clone(), gamepad);
                            if focused {
                                *input_owner = (window.active!=0).then_some(window.active);
                            }
                            if window.sessions.is_empty() && window.pending.is_empty()
                                || !actions.borrow().is_empty()
                            {
                                ctx.request_repaint_of(egui::ViewportId::ROOT);
                            }
                        }
                    });
                });
            }
            if state.windows.is_empty() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        });
    }
    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        STATE.with(|state| {
            if let Some(mut state) = state.borrow_mut().take() {
                for window in &mut state.windows {
                    for session in &mut window.sessions {
                        session.app.disconnect();
                    }
                }
                // One shared grace period for all displays. Sequential six-second
                // waits made closing two streams take twice as long.
                let deadline=std::time::Instant::now()+std::time::Duration::from_secs(6);
                while std::time::Instant::now()<deadline && state.windows.iter().flat_map(|w|&w.sessions).any(|s|
                    s.app.controller.as_ref().is_some_and(|h|!h.stopped.load(std::sync::atomic::Ordering::Acquire)))
                {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                for window in &mut state.windows {
                    for session in &mut window.sessions {
                        session.app.finish_exit(gl,false);
                    }
                }
            }
        });
    }
}

#[cfg(test)]
#[path = "stream_workspace_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "stream_workspace_gui_probe.rs"]
mod gui_probe;
