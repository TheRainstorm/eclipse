//! Live sessions belong to a workspace; native windows only present a group.
//! Moving a group never recreates its controller, decoder, or media transport.
use crate::{
    sessions::{ConnectionTarget, DisplayTarget, StreamTarget},
    window_context, App,
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
            ConnectionTarget::NativeDisplay {connection,..}=>connection.as_mut(),
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
    OpenDisplay { session: u64, screen: i32 },
    OpenAllDisplays(u64),
    CreateDisplay(u64),
    ManageDisplays(u64),
    Close(u64),
}
#[derive(Clone)]
pub struct Group {
    pub(super) id: u64,
    pub(super) name: String,
    pub(super) screens: Vec<(i32, String)>,
    pub(super) selected: Option<i32>,
    pub(super) can_open_displays: bool,
    pub(super) can_create_display:bool,
    pub(super) can_detach:bool,
}
impl Group {
    pub(super) fn can_open_display(&self,screen:i32)->bool {
        self.can_open_displays && (self.can_detach || self.selected!=Some(screen))
    }
}
#[derive(Clone)]
pub struct Tabs {
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
                    if ui.selectable_label(group.id==self.active,&group.name).clicked() {selected=Some(Action::Select(group.id,None));}
                    ui.indent(group.id,|ui| {
                        if group.can_open_displays && ui.add_enabled(group.can_create_display,egui::Button::new(uu_i18n::text("+ 新建临时虚拟显示器")))
                            .on_disabled_hover_text(uu_i18n::text("远端未提供虚拟显示器管理，请更新后台并授予管理服务权限")).clicked() {selected=Some(Action::CreateDisplay(group.id));}
                        if group.can_create_display && ui.small_button(uu_i18n::text("管理虚拟显示器")).clicked() {selected=Some(Action::ManageDisplays(group.id));}
                        for (screen,label) in &group.screens {
                            ui.horizontal(|ui| {
                                if ui.selectable_label(group.id==self.active && group.selected==Some(*screen),label).clicked() {selected=Some(Action::Select(group.id,Some(*screen)));}
                                if group.can_open_displays && ui.add_enabled(group.can_open_display(*screen),egui::Button::new(uu_i18n::text("独立窗口")).small())
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
        let Some(current)=self.groups.iter().position(|group|group.id==self.active) else {return;};
        let previous=matches!(action,Command::PreviousDisplay|Command::PreviousComputer);
        let selection=match action {
            Command::PreviousComputer|Command::NextComputer if self.groups.len()>1 => {
                Some(Action::Select(self.groups[adjacent(self.groups.len(),Some(current),previous)].id,None))
            },
            Command::PreviousDisplay|Command::NextDisplay => {
                let group=&self.groups[current];
                if group.screens.len()<2 {return;}
                let selected=group.screens.iter().position(|screen|Some(screen.0)==group.selected);
                Some(Action::Select(group.id,Some(group.screens[adjacent(group.screens.len(),selected,previous)].0)))
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
}
fn session_title(app:&App)->String {
    let target=app.stream_target.as_ref().expect("stream session target");
    if target.connection.display().is_some() {format!("{} · {}",target.alias,app.display_label())}
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
struct Window {
    shortcuts:Rc<RefCell<crate::shortcuts::Shortcuts>>,
    vsync_session: Option<u64>,
    id: u64,
    sessions: Vec<Session>,
    pending: Vec<Launch>,
    active: u64,
    closing: bool,
    platform: Option<Rc<crate::platform::Platform>>,
    sizing: Rc<RefCell<crate::window::WindowSizing>>,
    restore_maximized: Rc<Cell<bool>>,
    focus: bool,
    position: Option<egui::Pos2>,
    origin: Option<(i32, i32)>,
    local_ui: Rc<Cell<bool>>,
}
impl Window {
    fn new(id: u64, pending: Vec<Launch>, position: Option<egui::Pos2>) -> Self {
        Self {
            shortcuts:Default::default(),
            id,
            sessions: Vec::new(),
            pending,
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
        }
    }
    fn viewport(&self) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(("stream-window", self.id))
    }
    fn tabs(&self, actions: Rc<RefCell<Vec<Action>>>) -> Tabs {
        Tabs {
            active: self.active,
            actions,
            local_ui: self.local_ui.clone(),
            groups: self
                .sessions
                .iter()
                .map(|s| Group {
                    id: s.id,
                    name: session_title(&s.app),
                    screens: s
                        .app
                        .remote_screens()
                        .screens
                        .iter()
                        .map(|screen| (screen.id, s.app.displays.layout.label(screen)))
                        .collect(),
                    selected: s.app.displays.selected,
                    can_detach:self.sessions.len()>1,
                    can_open_displays: s.app.stream_target.as_ref().is_some_and(StreamTarget::is_native)
                        && s.app.controller.as_ref().is_some_and(|h|h.stats.capabilities.allows(uu_core::capabilities::Capability::Displays)),
                    can_create_display:s.app.controller.as_ref().is_some_and(|h|h.stats.virtual_displays.available.load(std::sync::atomic::Ordering::Acquire)),
                })
                .collect(),
        }
    }
    fn select(&mut self, id: u64) {
        if self.active == id {
            return;
        }
        if let Some(old) = self.sessions.iter_mut().find(|s| s.id == self.active) {
            old.app.deactivate_session();
        }
        self.active = id;
        if let Some(platform) = &self.platform {
            platform.select_session(id);
        }
        self.sizing.borrow_mut().begin_stream();
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
            for session in &mut self.sessions {
                session.app.shortcuts=self.shortcuts.clone();
                session.app.platform = self.platform.as_ref().unwrap().clone();
                session.app.context = window_context::Context::new(ctx);
                session.app.clipboard_window.store(
                    session.app.platform.window_id(),
                    std::sync::atomic::Ordering::Release,
                );
                if let Some(handle) = &session.app.controller {
                    let context = session.app.context.clone();
                    handle.frames.set_waker(move || context.request_video_frame());
                }
            }
            eframe::request_vsync(ctx, vsync);
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
            self.pending.clear();
        }
        for launch in std::mem::take(&mut self.pending) {
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
            app.window_sizing = self.sizing.clone();
            app.shortcuts=self.shortcuts.clone();
            app.gamepad = gamepad.clone();
            app.restore_maximized = self.restore_maximized.clone();
            self.sessions.push(Session { id, app });
            self.select(id);
        }
        let tabs = self.tabs(actions);
        let mut active = None;
        for (index, session) in self.sessions.iter_mut().enumerate() {
            session.app.workspace_tabs = Some(tabs.clone());
            session.app.active_session = session.id == self.active && !self.closing;
            if session.app.active_session {
                active = Some(index);
            } else {
                session.app.workspace_poll(ctx);
                session.app.poll_controller(ctx);
                if session.app.exit_requested && session.app.audio_save_after.take().is_some() {
                    session.app.persist_preferences();
                }
            }
        }
        if let Some(index) = active {
            let app = &mut self.sessions[index].app;
            app.context = window_context::Context::new(ctx);
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
        if !self.sessions.iter().any(|s| s.id == self.active) {
            if let Some(first) = self.sessions.first() {
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
        let existing=self.windows.iter().filter(|w|!w.closing).flat_map(|w|&w.sessions).find(|s| {
            !s.app.exit_requested && !s.app.disconnecting && s.app.displays.ready()
                && s.app.stream_target.as_ref().is_some_and(|t|base_key(t)==identity)
                && requested.resolve(&s.app.remote_screens().screens).is_ok_and(|id|s.app.displays.selected==Some(id))
        }).map(|s|s.id);
        if let Some(existing)=existing {self.apply(Action::Detach(existing),ctx);return;}
        let key=launch.target.key();
        if let Some(window)=self.windows.iter_mut().filter(|w|!w.closing).find(|w|w.pending.iter().any(|l|l.target.key()==key)) {
            window.focus=true;return;
        }
        let position=self.windows.iter().find(|w|w.sessions.iter().any(|s|s.id==session)).and_then(|w|ctx.input_for(w.viewport(),|i|i.viewport().outer_rect.map(|r|r.min+egui::vec2(36.,36.))));
        let id=self.next;self.next+=1;
        self.windows.push(Window::new(id,vec![launch],position));
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
                            base_key(target)==identity &&
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
            self.windows.push(Window::new(id,vec![launch],None));
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
    fn apply(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::OpenDisplay{session,screen}=>self.open_display(session,screen,ctx),
            Action::CreateDisplay(id)=>{if let Some(session)=self.windows.iter_mut().flat_map(|w|&mut w.sessions).find(|s|s.id==id){session.app.create_remote_virtual_display();}},
            Action::ManageDisplays(id)=>{if let Some(session)=self.windows.iter_mut().flat_map(|w|&mut w.sessions).find(|s|s.id==id){session.app.manage_remote_virtual_displays();}},
            Action::OpenAllDisplays(session)=>{
                let screens=self.windows.iter().flat_map(|w|&w.sessions).find(|s|s.id==session)
                    .map(|s|s.app.remote_screens().screens.iter().map(|s|s.id).collect::<Vec<_>>()).unwrap_or_default();
                for screen in screens {self.open_display(session,screen,ctx);}
            },
            Action::Select(id, screen) => {
                for window in &mut self.windows {
                    if window.sessions.iter().any(|s| s.id == id) {
                        window.select(id);
                        if let Some(screen) = screen {
                            if let Some(s) = window.sessions.iter_mut().find(|s| s.id == id) {
                                s.app.select_remote_screen(screen);
                            }
                        }
                        ctx.request_repaint_of(window.viewport());
                        break;
                    }
                }
            }
            Action::Close(id) => {
                for window in &mut self.windows {
                    if let Some(session) = window.sessions.iter_mut().find(|s| s.id == id) {
                        session.app.disconnect();
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
                    if source != target {
                        let old = window.sessions.iter().position(|s| s.id == source).unwrap();
                        let session = window.sessions.remove(old);
                        let to = window.sessions.iter().position(|s| s.id == target).unwrap()
                            + usize::from(after);
                        window.sessions.insert(to, session);
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
            Action::Detach(id) => {
                let Some(index) = self
                    .windows
                    .iter()
                    .position(|w| !w.closing && w.sessions.iter().any(|s| s.id == id))
                else {
                    return;
                };
                if self.windows[index].sessions.len()==1 {
                    self.windows[index].select(id);
                    self.windows[index].focus=true;
                    return;
                }
                let source = &mut self.windows[index];
                let at = source.sessions.iter().position(|s| s.id == id).unwrap();
                let mut session = source.sessions.remove(at);
                session.app.deactivate_session();
                if source.active == id {
                    let next = source.sessions[0].id;
                    source.select(next);
                }
                let id = self.next;
                self.next += 1;
                let position = ctx.input_for(source.viewport(), |i| {
                    i.viewport()
                        .outer_rect
                        .map(|r| r.min + egui::vec2(36., 36.))
                });
                let mut window = Window::new(id, Vec::new(), position);
                window.active = session.id;
                session.app.window_sizing = window.sizing.clone();
                session.app.shortcuts=window.shortcuts.clone();
                session.app.restore_maximized = window.restore_maximized.clone();
                // Native handles and wakeups are retargeted when the new native
                // surface exists, before this session can receive input again.
                session.app.active_session = false;
                window.sessions.push(session);
                self.windows.push(window);
            }
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
