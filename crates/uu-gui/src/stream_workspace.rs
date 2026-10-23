//! Live sessions belong to a workspace; native windows only present a group.
//! Moving a group never recreates its controller, decoder, or media transport.
use crate::{
    sessions::{ConnectionTarget, StreamTarget},
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
    pub code: Option<zeroize::Zeroizing<String>>,
}
impl Launch {
    pub fn new(
        mut target: StreamTarget,
        assistance: Option<&uu_core::assistance::Request>,
    ) -> anyhow::Result<Self> {
        match &mut target.connection {
            ConnectionTarget::NativeInvitation(path)
            | ConnectionTarget::NativeInternet(path)
            | ConnectionTarget::NativeProfile { snapshot: path, .. } => {
                *path = path.canonicalize()?
            }
            _ => {}
        }
        Ok(Self {
            target,
            code: assistance.map(|r| zeroize::Zeroizing::new(r.code().into())),
        })
    }
    fn assistance(&self) -> anyhow::Result<Option<uu_core::assistance::Request>> {
        anyhow::ensure!(self.target.alias.len() <= 512, "设备名称过长");
        match (&self.target.connection, &self.code) {
            (ConnectionTarget::UuAssistance(id), Some(code)) => {
                Ok(Some(uu_core::assistance::Request::new(id, code)?))
            }
            (ConnectionTarget::UuAssistance(_), None) => anyhow::bail!("远程协助需要验证码"),
            (_, None) => Ok(None),
            (_, Some(_)) => anyhow::bail!("连接方式与凭据不匹配"),
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
    Close(u64),
}
#[derive(Clone)]
pub struct Group {
    id: u64,
    name: String,
    screens: Vec<(i32, String)>,
    selected: Option<i32>,
}
#[derive(Clone)]
pub struct Tabs {
    groups: Vec<Group>,
    active: u64,
    actions: Rc<RefCell<Vec<Action>>>,
    local_ui: Rc<Cell<bool>>,
}
#[derive(Clone, Copy)]
struct Drag(u64);
#[derive(Clone, Copy)]
struct DragScreen {
    session: u64,
    screen: i32,
}
impl Tabs {
    pub fn multiple(&self) -> bool {
        self.groups.len() > 1
    }
    pub fn owns_input(&self, ctx: &egui::Context) -> bool {
        self.local_ui.get()
            || egui::DragAndDrop::has_payload_of_type::<Drag>(ctx)
            || egui::DragAndDrop::has_payload_of_type::<DragScreen>(ctx)
    }
    pub fn show(&self, ui: &mut egui::Ui) {
        self.local_ui.set(false);
        let width = (ui.available_width() - 245.).max(90.);
        let mut actions = Vec::new();
        egui::ScrollArea::horizontal()
            .id_salt("workspace-device-groups")
            .max_width(width)
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .auto_shrink([true, true])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for group in &self.groups {
                        ui.push_id(group.id, |ui| {
                            let frame = egui::Frame::none()
                                .fill(if group.id == self.active {
                                    egui::Color32::WHITE
                                } else {
                                    egui::Color32::from_rgb(226, 232, 240)
                                })
                                .rounding(5.)
                                .inner_margin(egui::Margin::symmetric(4., 0.));
                            let response = frame
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        let label = ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&group.name).size(12.),
                                            )
                                            .sense(egui::Sense::click_and_drag()),
                                        );
                                        label.dnd_set_drag_payload(Drag(group.id));
                                        if label.clicked() {
                                            actions.push(Action::Select(group.id, None));
                                        }
                                        if label.dragged() {
                                            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                                        }
                                        label.context_menu(|ui| {
                                            if ui.button("移到独立窗口").clicked() {
                                                actions.push(Action::Detach(group.id));
                                                ui.close_menu();
                                            }
                                            if ui.button("断开此设备").clicked() {
                                                actions.push(Action::Close(group.id));
                                                ui.close_menu();
                                            }
                                        });
                                        if label.context_menu_opened() {
                                            self.local_ui.set(true);
                                        }
                                        for (id, label) in &group.screens {
                                            let selected = group.id == self.active
                                                && group.selected == Some(*id);
                                            let response = ui.add(
                                                egui::Button::new(label)
                                                    .selected(selected)
                                                    .sense(egui::Sense::click_and_drag()),
                                            );
                                            if response.clicked() {
                                                actions.push(Action::Select(group.id, Some(*id)));
                                            }
                                            response.dnd_set_drag_payload(DragScreen {
                                                session: group.id,
                                                screen: *id,
                                            });
                                            if let Some(drag) = response
                                                .dnd_hover_payload::<DragScreen>()
                                                .filter(|drag| {
                                                    drag.session == group.id && drag.screen != *id
                                                })
                                            {
                                                let after = ui.input(|i| {
                                                    i.pointer.hover_pos().is_some_and(|p| {
                                                        p.x > response.rect.center().x
                                                    })
                                                });
                                                if response
                                                    .dnd_release_payload::<DragScreen>()
                                                    .is_some()
                                                {
                                                    actions.push(Action::ReorderScreen {
                                                        session: group.id,
                                                        source: drag.screen,
                                                        target: *id,
                                                        after,
                                                    });
                                                }
                                            }
                                        }
                                    })
                                })
                                .response;
                            if let Some(drag) = response
                                .dnd_hover_payload::<Drag>()
                                .filter(|drag| drag.0 != group.id)
                            {
                                let after = ui.input(|i| {
                                    i.pointer
                                        .hover_pos()
                                        .is_some_and(|p| p.x > response.rect.center().x)
                                });
                                let x = if after {
                                    response.rect.right()
                                } else {
                                    response.rect.left()
                                };
                                ui.painter().line_segment(
                                    [
                                        egui::pos2(x, response.rect.top()),
                                        egui::pos2(x, response.rect.bottom()),
                                    ],
                                    egui::Stroke::new(2., crate::theme::ACCENT),
                                );
                                if response.dnd_release_payload::<Drag>().is_some() {
                                    actions.push(Action::Reorder {
                                        source: drag.0,
                                        target: group.id,
                                        after,
                                    });
                                }
                            }
                        });
                    }
                });
            });
        self.actions.borrow_mut().extend(actions);
        if let Some(drag) = egui::DragAndDrop::payload::<Drag>(ui.ctx()) {
            let detach = ui.input(|i| {
                i.pointer.any_released()
                    && i.pointer
                        .latest_pos()
                        .is_none_or(|p| !i.screen_rect().contains(p))
            });
            if detach {
                self.actions.borrow_mut().push(Action::Detach(drag.0));
                egui::DragAndDrop::clear_payload(ui.ctx());
            }
        }
    }
}
struct Session {
    id: u64,
    app: App,
}
struct Window {
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
                    name: s.app.stream_target.as_ref().unwrap().alias.clone(),
                    screens: s
                        .app
                        .remote_screens()
                        .screens
                        .iter()
                        .map(|screen| (screen.id, s.app.displays.layout.label(screen)))
                        .collect(),
                    selected: s.app.displays.selected,
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
                    handle.frames.set_waker(move || context.request_repaint());
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
                session.app.platform = self.platform.as_ref().unwrap().clone();
                session.app.context = window_context::Context::new(ctx);
                session.app.clipboard_window.store(
                    session.app.platform.window_id(),
                    std::sync::atomic::Ordering::Release,
                );
                if let Some(handle) = &session.app.controller {
                    let context = session.app.context.clone();
                    handle.frames.set_waker(move || context.request_repaint());
                }
            }
            eframe::request_vsync(ctx, vsync);
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
            let assistance = match launch.assistance() {
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
                assistance,
            );
            app.window_sizing = self.sizing.clone();
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
        } else {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label("正在释放连接…");
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
    fn attach(&mut self, launch: Launch) -> bool {
        if launch.assistance().is_err() {
            return false;
        }
        let key = launch.target.key();
        for window in &mut self.windows {
            if let Some(session) = window
                .sessions
                .iter()
                .find(|s| s.app.stream_target.as_ref().is_some_and(|t| t.key() == key))
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
                    .position(|w| w.sessions.len() > 1 && w.sessions.iter().any(|s| s.id == id))
                else {
                    return;
                };
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
                let mut builder = egui::ViewportBuilder::default()
                    .with_title(format!("串流 · {}", crate::brand::NAME))
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
