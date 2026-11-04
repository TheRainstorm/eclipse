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

#[derive(Serialize, Deserialize)]
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
    ManageDisplays(u64),
    Close(u64),
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
    pub(super) fn creation_session(&self)->u64 {self.views.iter().find(|view|view.can_create).map_or(self.id,|view|view.session)}
    pub(super) fn command_session(&self)->u64 {self.views.iter().find(|view|view.active).map_or(self.id,|view|view.session)}
    pub(super) fn contains(&self,id:u64)->bool {self.id==id || self.members.contains(&id)}
    pub(super) fn screen_action(&self,screen:i32)->Action {
        self.views.iter().find(|view|view.screen==screen&&view.fixed).or_else(||self.views.iter().find(|view|view.screen==screen)).map_or(Action::Select(self.id,if self.fixed_view {None}else{Some(screen)}),|view|Action::Select(view.session,None))
    }
    pub(super) fn view_group(&self,screen:i32)->Option<Self> {
        let view=self.views.iter().find(|view|view.screen==screen && view.fixed)?;
        let mut group=self.clone();group.id=view.session;group.can_detach=view.can_detach;group.can_merge=view.can_merge;group.fixed_view=true;group.can_open_displays=false;group.views.clear();Some(group)
    }
    pub(super) fn can_open_display(&self,screen:i32)->bool {
        self.can_open_displays && (self.can_detach || self.selected!=Some(screen))
    }
}
#[derive(Clone)]
pub struct Tabs {
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
    fn selector_id(ctx:&egui::Context)->egui::Id {
        egui::Id::new(("workspace-device-selector",ctx.viewport_id()))
    }
    pub fn open_selector(&self,ctx:&egui::Context) {
        let id=Self::selector_id(ctx);
        ctx.data_mut(|data|data.insert_temp(id,self.active));
    }
    pub fn show_selector(&self,ctx:&egui::Context)->Option<egui::Rect> {
        let id=Self::selector_id(ctx);
        let owner=ctx.data(|data|data.get_temp::<u64>(id));
        if owner!=Some(self.active) {
            if owner.is_some(){ctx.data_mut(|data|data.remove::<u64>(id));}
            return None;
        }
        let mut open=true;let mut selected=None;
        let window=egui::Window::new(uu_i18n::text("设备与屏幕"))
            .id(id).open(&mut open).collapsible(false).resizable(false).default_width(360.)
            .show(ctx,|ui|egui::ScrollArea::vertical().max_height(440.).show(ui,|ui| {
                for group in &self.groups {
                    if ui.selectable_label(group.contains(self.active),&group.name).clicked() {selected=Some(Action::Select(group.id,None));}
                    ui.indent(group.id,|ui| {
                        if group.can_open_displays||group.can_create_display {
                            let add=ui.add_enabled(group.can_create_display,egui::Button::new(uu_i18n::text("+ 新建临时虚拟显示器")))
                                .on_disabled_hover_text(uu_i18n::text("远端未提供虚拟显示器管理，请更新后台并授予管理服务权限"));
                            if add.clicked() {selected=Some(Action::CreateDisplayWithSpec{session:group.creation_session(),spec:self.presets.default_spec()});}
                            if group.can_create_display {add.context_menu(|ui| {if let Some(spec)=crate::virtual_display_presets::create_menu(ui,&self.presets) {selected=Some(Action::CreateDisplayWithSpec{session:group.creation_session(),spec});}});}
                        }
                        if group.can_create_display && ui.small_button(uu_i18n::text("管理虚拟显示器")).clicked() {selected=Some(Action::ManageDisplays(group.creation_session()));}
                        for (screen,label) in &group.screens {
                            ui.horizontal(|ui| {
                                if ui.selectable_label(group.contains(self.active) && group.selected==Some(*screen),label).clicked() {selected=Some(group.screen_action(*screen));}
                                if let Some(view)=group.view_group(*screen) {
                                    if ui.add_enabled(view.can_detach,egui::Button::new(uu_i18n::text("独立窗口")).small()).on_disabled_hover_text(uu_i18n::text("已在独立窗口中")).clicked() {selected=Some(Action::Detach(view.id));}
                                } else if group.can_open_displays && ui.add_enabled(group.can_open_display(*screen),egui::Button::new(uu_i18n::text("独立窗口")).small())
                                    .on_disabled_hover_text(uu_i18n::text("已在独立窗口中")).clicked() {selected=Some(Action::OpenDisplay{session:group.id,screen:*screen});}
                            });
                        }
                        if group.can_open_displays && group.screens.len()>1 && ui.button(uu_i18n::text("所有屏幕独立打开")).clicked() {selected=Some(Action::OpenAllDisplays(group.id));}
                    });
                    ui.separator();
                }
            }));
        if let Some(selection)=selected {self.actions.borrow_mut().push(selection);open=false;}
        if !open {ctx.data_mut(|data|data.remove::<u64>(id));}
        window.map(|window|window.response.rect)
    }
    pub fn navigate(&self, action: uu_core::hotkeys::Action) {
        use uu_core::hotkeys::Action as Command;
        let Some(current)=self.groups.iter().position(|group|group.contains(self.active)) else {return;};
        let previous=matches!(action,Command::PreviousDisplay|Command::PreviousComputer);
        let selection=match action {
            Command::PreviousComputer|Command::NextComputer if self.groups.len()>1 => {
                Some(Action::Select(self.groups[adjacent(self.groups.len(),Some(current),previous)].id,None))
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
        let selector_id=Self::selector_id(ctx);
        self.local_ui.get()
            || ctx.data(|data|data.get_temp::<u64>(selector_id))==Some(self.active)
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
    app: App,
    // A presentation destination, never connection or virtual-lease ownership.
    // Keep the ID after its empty native container retires so Merge can restore it.
    home_window:Option<u64>,
    // Normal device browsing keeps visited sources alive in this container.
    browsable:bool,
}
fn session_title(app:&App)->String {
    let target=app.stream_target.as_ref().expect("stream session target");
    if target.connection.virtual_display().is_some() {format!("{} · {}",target.alias,uu_i18n::text("临时虚拟屏"))}
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
    shortcuts:Rc<RefCell<crate::shortcuts::Shortcuts>>,
    vsync_session: Option<u64>,
    id: u64,
    sessions: Vec<Session>,
    pending: Vec<Launch>,
    cached_launches:std::collections::HashMap<ConnectionTarget,(bool,u32)>,
    active: u64,
    closing: bool,
    platform: Option<Rc<crate::platform::Platform>>,
    sizing: Rc<RefCell<crate::window::WindowSizing>>,
    restore_maximized: Rc<Cell<bool>>,
    focus: bool,
    position: Option<egui::Pos2>,
    origin: Option<(i32, i32)>,
    local_ui: Rc<Cell<bool>>,
    // Consumed by the initial explicit-display launch only. Later sessions
    // created in this window choose this container as home when detached.
    home_window:Option<u64>,
}
impl Window {
    fn new(id: u64, pending: Vec<Launch>, position: Option<egui::Pos2>) -> Self {
        Self {
            shortcuts:Default::default(),
            id,
            sessions: Vec::new(),
            pending,
            cached_launches:Default::default(),
            active: 0,
            closing: false,
            platform: None,
            sizing: Default::default(),
            restore_maximized: Default::default(),
            focus: true,
            position,
            origin: None,
            local_ui: Default::default(),
            vsync_session: None,
            home_window:None,
        }
    }
    fn viewport(&self) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(("stream-window", self.id))
    }
    fn tabs(&self, actions: Rc<RefCell<Vec<Action>>>) -> Tabs {
        let mut groups:Vec<(ConnectionTarget,Group,Vec<(String,i32)>)>=Vec::new();
        let mut browsers=std::collections::HashMap::new();
        for source in self.sessions.iter().filter(|s|s.browsable && !s.app.exit_requested && s.home_window.is_none_or(|home|home==self.id)) {
            if let Some(target)=&source.app.stream_target {
                let healthy=!source.app.disconnecting && source.app.controller.as_ref().is_some_and(|h|!h.stopped.load(std::sync::atomic::Ordering::Acquire)) && !source.app.remote_screens().screens.is_empty();
                let entry=browsers.entry(base_key(target)).or_insert((source.id,healthy));
                if healthy && !entry.1 {*entry=(source.id,true);}
            }
        }
        for session in self.sessions.iter().filter(|session|!session.app.exit_requested) {
            let target=session.app.stream_target.as_ref().expect("stream target");
            let identity=base_key(target);
            let preparing=target.connection.is_fixed_display_view() && session.app.displays.preparing_initial_target();
            let fixed=session.app.is_fixed_display_view();
            let browser=browsers.get(&identity).is_some_and(|(id,_)|*id==session.id);
            let can_merge=session.home_window.is_some_and(|home|home!=self.id)&&!session.app.disconnecting;
            let live=self.sessions.iter().filter(|s|!s.app.exit_requested&&!s.app.disconnecting).count();
            let can_detach=!can_merge&&!session.app.disconnecting&&(!fixed||live>1||(browser&&session.app.remote_screens().screens.len()>1))&&(fixed||session.app.displays.ready());
            let can_open=browser && !preparing && target.is_native() && session.app.controller.as_ref().is_some_and(|h|h.stats.capabilities.allows(uu_core::capabilities::Capability::Displays));
            let can_create=!preparing && session.app.controller.as_ref().is_some_and(|h|h.stats.virtual_displays.available.load(std::sync::atomic::Ordering::Acquire)&&h.stats.native_displays.initial_supported());
            let at=groups.iter().position(|(key,_,_)|*key==identity).unwrap_or_else(|| {
                groups.push((identity,Group{id:session.id,name:target.alias.clone(),screens:Vec::new(),selected:None,members:Vec::new(),views:Vec::new(),can_open_displays:false,can_create_display:false,can_detach:false,fixed_view:true,can_merge:false},Vec::new()));groups.len()-1
            });
            let (_,group,sources)=&mut groups[at];
            group.members.push(session.id);group.can_create_display|=can_create;
            if browser {group.id=session.id;group.fixed_view=false;group.can_open_displays|=can_open;}
            if session.id==self.active || group.members.len()==1 {group.can_detach=can_detach;group.can_merge=can_merge;}
            let catalog=if browser {session.app.remote_screens()}else{session.app.visible_remote_screens()};
            let mut selected=None;
            for screen in catalog.screens.iter().filter(|screen|!preparing&&(browser||!fixed||session.app.displays.selected==Some(screen.id))) {
                let bound=fixed && session.app.displays.selected==Some(screen.id);
                let key=if bound {-(i32::try_from(session.id).unwrap_or(i32::MAX))} else {screen.id};
                let key=if let Some((_,existing))=sources.iter_mut().find(|(name,_)|name==&screen.display_name) {
                    if bound {
                        if let Some((old,label))=group.screens.iter_mut().find(|(old,_)|*old==*existing) {*old=key;*label=session.app.displays.layout.label(screen);}
                        if group.selected==Some(*existing) {group.selected=Some(key);}
                        for view in &mut group.views {if view.screen==*existing {view.screen=key;}}
                        *existing=key;
                    }
                    *existing
                } else {sources.push((screen.display_name.clone(),key));group.screens.push((key,session.app.displays.layout.label(screen)));key};
                if session.app.displays.selected==Some(screen.id) {selected=Some(key);}
            }
            if preparing {
                let placeholder=-(i32::try_from(session.id).unwrap_or(i32::MAX));
                if let Some(target)=session.app.fixed_display_target() {
                    if let Some((_,existing))=sources.iter_mut().find(|(name,_)|name==&target.source) {
                        group.screens.retain(|(key,_)|*key!=*existing);
                        if group.selected==Some(*existing) {group.selected=Some(placeholder);}
                        for view in &mut group.views {if view.screen==*existing {view.screen=placeholder;}}
                        *existing=placeholder;
                    }
                }
                group.screens.push((placeholder,uu_i18n::text(if target.connection.virtual_display().is_some(){"正在创建临时虚拟显示器…"}else{"正在准备指定显示器…"}).into()));selected=Some(placeholder);
            }
            if let Some(screen)=selected {
                group.views.push(View{screen,session:session.id,fixed,active:session.id==self.active,can_create,can_detach,can_merge});
                if session.id==self.active {group.selected=Some(screen);}
            } else if session.id==self.active {
                // A failed/preparing view still owns its commands. Its absent
                // catalog must never route Close to a healthy sibling.
                group.views.push(View{screen:-(i32::try_from(session.id).unwrap_or(i32::MAX)),session:session.id,fixed,active:true,can_create,can_detach,can_merge});
            }
        }
        Tabs {
            presets:self.sessions.iter().find(|s|s.id==self.active).map(|s|s.app.preferences.virtual_display_presets.clone()).unwrap_or_default(),
            active:self.active,actions,local_ui:self.local_ui.clone(),groups:groups.into_iter().map(|(_,group,_)|group).collect(),
        }
    }
    fn select(&mut self, id: u64) {
        if !self.sessions.iter().any(|session|session.id==id&&!session.app.exit_requested) {return;}
        if self.active == id {
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
                if let Some(handle) = &session.app.controller {
                    let context = session.app.context.clone();
                    handle.frames.set_waker(move || context.request_video_frame());
                }
            }
        }
    }
    fn route_cached_audio(&mut self,id:u64) {
        let owner=self.sessions.iter().find(|s|s.id==id).and_then(|s|s.app.stream_target.as_ref()).map(base_key);
        if let Some(owner)=owner {
            for view in self.sessions.iter_mut().filter(|s|s.app.stream_target.as_ref().is_some_and(|t|base_key(t)==owner)) {
                view.app.audio_parked=view.id!=id;
                if let Some(handle)=&view.app.controller {handle.stats.audio.parked.store(view.app.audio_parked,std::sync::atomic::Ordering::Release);}
            }
        }
    }
    fn take_session(&mut self,id:u64)->Session {
        let index=self.sessions.iter().position(|session|session.id==id).expect("live session");
        let mut session=self.sessions.remove(index);
        session.app.audio_parked=false;
        if let Some(handle)=&session.app.controller {handle.stats.audio.parked.store(false,std::sync::atomic::Ordering::Release);}
        session.app.deactivate_session();
        session.app.context.suspend();
        if self.active==id {
            if let Some(next)=self.sessions.iter().find(|session|!session.app.exit_requested&&!session.app.disconnecting).map(|session|session.id) {self.select(next);}
        }
        session
    }
    fn adopt_session(&mut self,mut session:Session) {
        if let Some(active)=self.sessions.iter_mut().find(|s|s.id==self.active) {active.app.deactivate_session();}
        session.app.window_sizing=self.sizing.clone();
        session.app.shortcuts=self.shortcuts.clone();
        session.app.restore_maximized=self.restore_maximized.clone();
        session.app.active_session=false;
        let id=session.id;
        self.sessions.push(session);
        // Activation belongs to the destination viewport's next pass, after
        // its platform/context and wakeups have been bound by show().
        self.active=id;
        self.route_cached_audio(id);
        self.sizing.borrow_mut().begin_view();
        self.focus=true;
    }
    fn show(
        &mut self,
        ctx: &egui::Context,
        gl: &Option<Arc<eframe::glow::Context>>,
        vsync: bool,
        next: &mut u64,
        actions: Rc<RefCell<Vec<Action>>>,
        gamepad: &Rc<RefCell<crate::gamepad::Reader>>,
    ) {
        #[cfg(test)]
        tests::observe_screenshot(ctx);
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
                if let Some(handle) = &session.app.controller {
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
            self.closing = true;
            for session in &mut self.sessions {
                session.app.deactivate_session();
                session.app.disconnect();
            }
            self.pending.clear();self.cached_launches.clear();
        }
        for launch in std::mem::take(&mut self.pending) {
            let cached=self.cached_launches.remove(&launch.target.key());
            let browsable=cached.is_some() || !launch.target.connection.is_fixed_display_view();
            let plugin_payload = match launch.plugin_payload() {
                Ok(value) => value,
                Err(_) => continue,
            };
            let id = *next;
            *next += 1;
            if let Some(old) = self.sessions.iter_mut().find(|s| s.id == self.active) {
                old.app.deactivate_session();
            }
            let mut app = App::new_session(
                ctx,
                gl.clone(),
                self.platform.as_ref().unwrap().clone(),
                Some(launch.target),
                vsync,
                plugin_payload,
            );
            app.fullscreen=ctx.input(|input|input.viewport().fullscreen.unwrap_or(false));
            if let Some((muted,volume))=cached {
                app.preferences.muted=muted;app.settings_draft.muted=muted;
                app.preferences.volume=volume;app.settings_draft.volume=volume;
                if let Some(handle)=&app.controller {handle.stats.audio.muted.store(muted,std::sync::atomic::Ordering::Release);handle.stats.audio.volume.store(volume,std::sync::atomic::Ordering::Release);}
            }
            app.window_sizing = self.sizing.clone();
            app.shortcuts=self.shortcuts.clone();
            app.gamepad = gamepad.clone();
            app.restore_maximized = self.restore_maximized.clone();
            let home_window=if app.stream_target.as_ref().is_some_and(|target|target.connection.display().is_some()) {self.home_window.take()}else{None};
            self.sessions.push(Session { id, app,home_window,browsable });
            self.select(id);
        }
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
        self.sessions
            .retain(|s| !s.app.exit_requested || s.app.controller.is_some());
        if !self.sessions.iter().any(|s| s.id == self.active&&!s.app.exit_requested) {
            if let Some(first) = self.sessions.iter().find(|session|!session.app.exit_requested&&!session.app.disconnecting) {
                self.select(first.id);
            }
        }
        if std::mem::take(&mut self.focus) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.platform.as_ref().unwrap().focus_window();
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

struct State {
    input_owner: Option<u64>,
    gamepad: Rc<RefCell<crate::gamepad::Reader>>,
    windows: Vec<Window>,
    gl: Option<Arc<eframe::glow::Context>>,
    vsync: bool,
    next: u64,
    actions: Rc<RefCell<Vec<Action>>>,
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
                windows: vec![window],
                gl: cc.gl.clone(),
                vsync,
                next: 2,
                actions: Default::default(),
                gamepad: Default::default(),
                input_owner: None,
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
    fn displaying_session(&self,identity:&ConnectionTarget,display:&DisplayTarget,excluding:Option<u64>)->Option<u64> {
        self.windows.iter().filter(|w|!w.closing).flat_map(|w|&w.sessions).find(|s| {
            Some(s.id)!=excluding && !s.app.exit_requested && !s.app.disconnecting && s.app.displays.ready()
                && s.app.stream_target.as_ref().is_some_and(|t|base_key(t)==*identity)
                && display.resolve(&s.app.remote_screens().screens).is_ok_and(|id|s.app.displays.selected==Some(id))
        }).map(|s|s.id)
    }
    fn detach(&mut self,id:u64,ctx:&egui::Context) {
        let Some(index)=self.windows.iter().position(|w|!w.closing&&w.sessions.iter().any(|s|s.id==id&&!s.app.exit_requested&&!s.app.disconnecting)) else{return;};
        let source=&self.windows[index];
        let session=source.sessions.iter().find(|s|s.id==id).unwrap();
        let browsing=session.browsable&&session.home_window.is_none_or(|home|home==source.id);
        let only=source.sessions.iter().filter(|s|!s.app.exit_requested&&!s.app.disconnecting).count()==1;
        let other=(browsing&&only).then(||session.app.remote_screens().screens.into_iter().find(|screen|Some(screen.id)!=session.app.displays.selected)).flatten().map(|screen|screen.id);
        if let Some(other)=other {
            // Keep another source in the original container. The selected
            // controller itself moves; neither connection needs renegotiation.
            if !self.cache_display(id,other,ctx) {return;}
        } else if !session.app.is_fixed_display_view() && session.app.remote_screens().screens.len()>1 {
            if let Some(screen)=session.app.displays.selected {self.open_display(id,screen,ctx);return;}
        } else if session.app.is_fixed_display_view() && only {
            self.windows[index].select(id);self.windows[index].focus=true;return;
        }
        let source=&mut self.windows[index];
        let session=source.sessions.iter_mut().find(|s|s.id==id).unwrap();
        if session.home_window.is_some_and(|home|home!=source.id) {
            source.select(id);source.focus=true;return;
        }
        if !session.app.pin_current_display_view() {return;}
        let source_id=source.id;
        let position=ctx.input_for(source.viewport(),|input|input.viewport().outer_rect.map(|rect|rect.min+egui::vec2(36.,36.)));
        let mut session=source.take_session(id);
        session.home_window.get_or_insert(source_id);
        ctx.request_repaint_of(source.viewport());
        if self.input_owner==Some(id) {self.input_owner=None;}
        let window_id=self.next;self.next+=1;
        let mut window=Window::new(window_id,Vec::new(),position);
        window.adopt_session(session);
        self.windows.push(window);
    }
    fn merge(&mut self,id:u64,ctx:&egui::Context) {
        let Some(source_index)=self.windows.iter().position(|w|!w.closing&&w.sessions.iter().any(|s|s.id==id&&!s.app.exit_requested&&!s.app.disconnecting)) else{return;};
        let source_id=self.windows[source_index].id;
        let home=self.windows[source_index].sessions.iter().find(|s|s.id==id).unwrap().home_window;
        if home==Some(source_id) {self.windows[source_index].select(id);self.windows[source_index].focus=true;return;}
        let Some(target_id)=self.merge_target(source_id,home) else {
            self.windows[source_index].select(id);self.windows[source_index].focus=true;return;
        };
        let mut session=self.windows[source_index].take_session(id);
        session.home_window=Some(target_id);
        ctx.request_repaint_of(self.windows[source_index].viewport());
        if self.input_owner==Some(id) {self.input_owner=None;}
        let target=self.windows.iter_mut().find(|window|window.id==target_id).unwrap();
        target.adopt_session(session);
        ctx.request_repaint_of(target.viewport());
    }
    fn merge_target(&mut self,source_id:u64,home:Option<u64>)->Option<u64> {
        Some(if let Some(home)=home.filter(|home|!self.windows.iter().any(|w|w.id==*home&&w.closing)) {
            // An emptied native container may already be retired. Recreate its
            // logical identity, without recreating the moved media connection.
            if !self.windows.iter().any(|w|w.id==home) {self.windows.push(Window::new(home,Vec::new(),None));}
            home
        } else if let Some(window)=self.windows.iter().find(|w|w.id!=source_id&&!w.closing&&(!w.pending.is_empty()||w.sessions.iter().any(|s|!s.app.exit_requested&&!s.app.disconnecting))) {
            window.id
        } else if home.is_some() {
            let id=self.next;self.next+=1;self.windows.push(Window::new(id,Vec::new(),None));id
        } else {
            return None;
        })
    }
    fn fork_virtual_display(&mut self,session:u64,spec:Option<uu_core::virtual_display::Spec>) {
        let Some(window)=self.windows.iter_mut().find(|window|!window.closing&&window.sessions.iter().any(|source|source.id==session)) else{return;};
        let source=window.sessions.iter_mut().find(|source|source.id==session).unwrap();
        if source.app.exit_requested || source.app.disconnecting {return;}
        if !source.app.controller.as_ref().is_some_and(|handle|handle.stats.virtual_displays.available.load(std::sync::atomic::Ordering::Acquire)) {
            source.app.displays.notice=uu_i18n::text("远端未提供虚拟显示器管理，请更新后台并授予管理服务权限").into();
            return;
        }
        let Some(target)=source.app.stream_target.as_ref() else{return;};
        match virtual_launch(target,spec.unwrap_or_else(||source.app.preferences.virtual_display_presets.default_spec())) {
            Ok(launch)=>{
                // The new connection creates and owns its own temporary screen.
                // Keep the origin's selected source, controller and leases intact.
                window.pending.push(launch);
                window.focus=true;
            },
            Err(error)=>source.app.displays.notice=error.to_string(),
        }
    }
    fn open_display(&mut self,session:u64,screen:i32,ctx:&egui::Context) {
        let Some(source)=self.windows.iter().flat_map(|w|&w.sessions).find(|s|s.id==session) else{return;};
        let screens=source.app.remote_screens();
        let Some(screen)=screens.screens.iter().find(|s|s.id==screen) else{return;};
        let target=source.app.stream_target.as_ref().expect("stream target");
        let identity=base_key(target);
        let launch=match display_launch(target,screen) {
            Ok(launch)=>launch,
            Err(error)=>{
                if let Some(source)=self.windows.iter_mut().flat_map(|w|&mut w.sessions).find(|s|s.id==session) {source.app.displays.notice=error.to_string();}
                return;
            },
        };
        let requested=launch.target.connection.display().expect("display launch");
        // Reuse a live view of this exact source, including an ordinary device
        // tab already displaying it. Pending views deduplicate by stable key.
        let existing=self.windows.iter().filter(|w|!w.closing).flat_map(|w|&w.sessions).find(|candidate| {
            candidate.app.is_fixed_display_view() && !candidate.app.exit_requested && !candidate.app.disconnecting && candidate.app.displays.ready()
                && candidate.app.stream_target.as_ref().is_some_and(|target|base_key(target)==identity)
                && requested.resolve(&candidate.app.remote_screens().screens).is_ok_and(|id|candidate.app.displays.selected==Some(id))
        }).map(|candidate|candidate.id);
        if let Some(existing)=existing {self.apply(Action::Detach(existing),ctx);return;}
        let key=launch.target.key();
        if let Some(window)=self.windows.iter_mut().filter(|w|!w.closing).find(|w|w.pending.iter().any(|l|l.target.key()==key)) {
            window.focus=true;return;
        }
        // Opening the selected source must keep the original window alive.
        // Prefer its other source, while a separate connection owns the new view.
        if let Some(source)=self.windows.iter_mut().flat_map(|w|&mut w.sessions).find(|s|s.id==session) {
            if !source.app.is_fixed_display_view() && source.app.displays.selected==Some(screen.id) {
                if let Some(other)=screens.screens.iter().find(|other|other.id!=screen.id) {source.app.select_remote_screen(other.id);}
            }
        }
        let position=self.windows.iter().find(|w|w.sessions.iter().any(|s|s.id==session)).and_then(|w|ctx.input_for(w.viewport(),|i|i.viewport().outer_rect.map(|r|r.min+egui::vec2(36.,36.))));
        let id=self.next;self.next+=1;
        let home=self.windows.iter().find(|window|window.sessions.iter().any(|source|source.id==session)).map(|window|window.id);
        let mut window=Window::new(id,vec![launch],position);window.home_window=home;
        self.windows.push(window);
    }
    fn attach(&mut self, launch: Launch) -> bool {
        if launch.plugin_payload().is_err() {
            return false;
        }
        let key = launch.target.key();
        let requested=launch.target.connection.display();
        let identity=base_key(&launch.target);
        for window in &mut self.windows {
            if let Some(session) = window
                .sessions
                .iter()
                .find(|s| {
                    !s.app.exit_requested && !s.app.disconnecting && s.app.stream_target.as_ref().is_some_and(|target| {
                        if let Some(display)=requested {
                            s.app.is_fixed_display_view() && base_key(target)==identity &&
                                ((s.app.displays.ready()&&display.resolve(&s.app.remote_screens().screens).is_ok_and(|id|s.app.displays.selected==Some(id)))
                                || (target.key()==key&&s.app.displays.preparing_initial_target()))
                        }else{target.key()==key}
                    })
                })
            {
                let id = session.id;
                window.select(id);
                window.focus = true;
                return true;
            }
            if window.pending.iter().any(|l| l.target.key() == key) {
                window.focus = true;
                return true;
            }
        }
        if launch.target.connection.display().is_some() {
            let id=self.next;self.next+=1;
            let home=self.windows.iter().find(|window|!window.closing).map(|window|window.id);
            let mut window=Window::new(id,vec![launch],None);window.home_window=home;
            self.windows.push(window);
            return true;
        }
        if let Some(window) = self.windows.iter_mut().find(|w| !w.closing) {
            window.pending.push(launch);
            window.focus = true;
            true
        } else {
            false
        }
    }
    fn cache_display(&mut self,session:u64,screen:i32,ctx:&egui::Context)->bool {
        let Some(index)=self.windows.iter().position(|w|!w.closing&&w.sessions.iter().any(|s|s.id==session&&!s.app.exit_requested&&!s.app.disconnecting)) else{return false;};
        let source=self.windows[index].sessions.iter().find(|s|s.id==session).unwrap();
        if !source.browsable || source.home_window.is_some_and(|home|home!=self.windows[index].id) || !source.app.displays.ready() {return false;}
        let Some(target)=source.app.stream_target.as_ref().filter(|t|t.is_native()) else{return false;};
        if !source.app.controller.as_ref().is_some_and(|h|h.stats.capabilities.allows(uu_core::capabilities::Capability::Displays)) {return false;}
        let catalog=source.app.remote_screens();
        let Some(display)=catalog.screens.iter().find(|s|s.id==screen) else{return true;};
        if source.app.displays.selected==Some(screen) {self.windows[index].select(session);return true;}
        let launch=match display_launch(target,display) {Ok(launch)=>launch,Err(_)=>return false};
        let audio=(source.app.preferences.muted,source.app.preferences.volume);
        let key=launch.target.key();
        if let Some((window,id))=self.windows.iter().filter(|w|!w.closing).find_map(|w|w.sessions.iter().find(|s|!s.app.exit_requested&&!s.app.disconnecting&&s.app.stream_target.as_ref().is_some_and(|t|t.key()==key)).map(|s|(w.id,s.id))) {
            let window=self.windows.iter_mut().find(|w|w.id==window).unwrap();window.select(id);window.focus=true;
            ctx.request_repaint_of(window.viewport());return true;
        }
        if let Some(window)=self.windows.iter_mut().find(|w|!w.closing&&w.pending.iter().any(|l|l.target.key()==key)) {
            window.focus=true;ctx.request_repaint_of(window.viewport());return true;
        }
        let window=&mut self.windows[index];
        let source=window.sessions.iter_mut().find(|s|s.id==session).unwrap();
        if !source.app.pin_current_display_view() {return false;}
        if let Some(handle)=&source.app.controller {handle.stats.diagnostics.record(serde_json::json!({"event":"display_view_open","screen":screen,"reuse_transport":false}));}
        window.cached_launches.insert(key,audio);window.pending.push(launch);window.focus=true;
        ctx.request_repaint_of(window.viewport());true
    }
    fn apply(&mut self, action: Action, ctx: &egui::Context) {
        #[cfg(test)]
        eprintln!("generated workspace action={action:?}");
        match action {
            Action::OpenDisplay{session,screen}=>self.open_display(session,screen,ctx),
            Action::CreateDisplay(id)=>self.fork_virtual_display(id,None),
            Action::CreateDisplayWithSpec{session,spec}=>self.fork_virtual_display(session,Some(spec)),
            Action::ManageDisplays(id)=>{if let Some(session)=self.windows.iter_mut().flat_map(|w|&mut w.sessions).find(|s|s.id==id){session.app.manage_remote_virtual_displays();}},
            Action::OpenAllDisplays(session)=>{
                let screens=self.windows.iter_mut().flat_map(|w|&mut w.sessions).find(|s|s.id==session)
                    .map(|s| {let selected=s.app.displays.selected;s.app.pin_current_display_view();s.app.remote_screens().screens.iter().filter(|screen|Some(screen.id)!=selected).map(|screen|screen.id).collect::<Vec<_>>()}).unwrap_or_default();
                for screen in screens {self.open_display(session,screen,ctx);}
            },
            Action::Select(id, screen) => {
                let existing=screen.and_then(|screen|self.windows.iter().flat_map(|w|&w.sessions).find(|s|s.id==id).and_then(|source| {
                    let target=source.app.stream_target.as_ref()?;
                    let screens=source.app.remote_screens();let screen=screens.screens.iter().find(|s|s.id==screen)?;
                    self.displaying_session(&base_key(target),&DisplayTarget{id:screen.id,source:screen.display_name.clone()},Some(id))
                }));
                if existing.is_none() && screen.is_some_and(|screen|self.cache_display(id,screen,ctx)) {return;}
                let (id,screen)=existing.map_or((id,screen),|existing|(existing,None));
                for window in &mut self.windows {
                    if window.sessions.iter().any(|s| s.id == id&&!s.app.exit_requested) {
                        window.select(id);
                        window.focus=true;
                        if let Some(screen) = screen {
                            if let Some(s) = window.sessions.iter_mut().find(|s| s.id == id) {
                                s.app.select_native_display(screen);
                            }
                        }
                        ctx.request_repaint_of(window.viewport());
                        break;
                    }
                }
            }
            Action::Close(id) => {
                for window in &mut self.windows {
                    if let Some(session) = window.sessions.iter_mut().find(|s| s.id == id&&!s.app.exit_requested) {
                        session.app.deactivate_session();
                        session.app.disconnect();
                        if self.input_owner==Some(id) {self.input_owner=None;}
                        if !window.sessions.iter().any(|session|session.id==window.active&&!session.app.exit_requested) {
                            if let Some(next)=window.sessions.iter().find(|session|!session.app.exit_requested&&!session.app.disconnecting).map(|session|session.id) {
                                window.select(next);
                            }
                        }
                        ctx.request_repaint_of(window.viewport());
                        break;
                    }
                }
            }
            Action::Reorder {
                source,
                target,
                after,
            } => {
                if let Some(window) = self.windows.iter_mut().find(|w| {
                    w.sessions.iter().any(|s| s.id == source)
                        && w.sessions.iter().any(|s| s.id == target)
                }) {
                    let source_key=base_key(window.sessions.iter().find(|s|s.id==source).unwrap().app.stream_target.as_ref().unwrap());
                    let target_key=base_key(window.sessions.iter().find(|s|s.id==target).unwrap().app.stream_target.as_ref().unwrap());
                    if source_key!=target_key {
                        let (moving,mut remaining):(Vec<_>,Vec<_>)=std::mem::take(&mut window.sessions).into_iter()
                            .partition(|session|base_key(session.app.stream_target.as_ref().unwrap())==source_key);
                        let position=if after {remaining.iter().rposition(|s|base_key(s.app.stream_target.as_ref().unwrap())==target_key).unwrap()+1}
                            else {remaining.iter().position(|s|base_key(s.app.stream_target.as_ref().unwrap())==target_key).unwrap()};
                        remaining.splice(position..position,moving);window.sessions=remaining;
                    }
                }
            }
            Action::ReorderScreen {
                session,
                source,
                target,
                after,
            } => {
                if let Some(session) = self
                    .windows
                    .iter_mut()
                    .flat_map(|w| w.sessions.iter_mut())
                    .find(|s| s.id == session)
                {
                    session.app.reorder_display_tab(source, target, after);
                }
            }
            Action::Detach(id)=>self.detach(id,ctx),
            Action::Merge(id)=>self.merge(id,ctx),
        }
    }
}
impl eframe::App for Workspace {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            let state = state.as_mut().expect("workspace UI thread");
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
            state
                .windows
                .retain(|w| !w.sessions.is_empty() || !w.pending.is_empty());
            for window in &mut state.windows {
                let id = window.id;
                let viewport = window.viewport();
                let title=window.sessions.iter().find(|s|s.id==window.active).map(|s|format!("{} · {}",session_title(&s.app),crate::brand::NAME))
                    .unwrap_or_else(||uu_i18n::format!("串流 · {}",crate::brand::NAME));
                let mut builder = egui::ViewportBuilder::default()
                    .with_title(title)
                    .with_inner_size([1100., 760.])
                    .with_min_inner_size([320., 200.])
                    .with_decorations(false)
                    .with_icon(crate::brand::window_icon());
                if let Some(position) = window.position.take() {
                    builder = builder.with_position(position);
                }
                ctx.show_viewport_deferred(viewport, builder, move |ctx, _| {
                    STATE.with(|state| {
                        let mut state = state.borrow_mut();
                        let state = state.as_mut().expect("workspace UI thread");
                        let focused = ctx.input(|i| i.focused);
                        let current = state.windows.iter().find(|w| w.id == id).map(|w| w.active);
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
                        } = state;
                        if let Some(window) = windows.iter_mut().find(|w| w.id == id) {
                            window.show(ctx, gl, *vsync, next, actions.clone(), gamepad);
                            if focused {
                                *input_owner = Some(window.active);
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
                for window in &mut state.windows {
                    for session in &mut window.sessions {
                        eframe::App::on_exit(&mut session.app, gl);
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
