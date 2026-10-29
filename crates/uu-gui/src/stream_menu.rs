//! Connection chrome and the optional floating control menu.
use super::ui::{icon, icon_button, icon_label_button, Icon};
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Stroke};

const MENU_INK: Color32 = super::theme::INK;
const MENU_WIDTH: f32 = 264.0;

// A 30-point face has 30.6% less area than the previous 36-point button.
const HANDLE_SIZE: f32 = 30.0;
const DOCK_DISTANCE: f32 = 24.0;
const DOCK_DELAY: f64 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Edge { Right, Left, Top, Bottom }

/// One owner for hover, click, drag and focus transitions. Position describes
/// the complete button; folding only clips its visible/interactive rectangle.
#[derive(Default)]
pub(super) struct FloatingHandle {
    position: Option<Pos2>,
    user_positioned: bool,
    docked: Option<Edge>,
    idle_since: f64,
    pinned: bool,
    blur_since: Option<f64>,
    suppressed: bool,
}
impl FloatingHandle {
    fn owns_input(&self, open: bool) -> bool { open && self.blur_since.is_none() }
    fn size(screen: Rect) -> egui::Vec2 {
        egui::vec2(HANDLE_SIZE.min(screen.width()), HANDLE_SIZE.min(screen.height()))
    }
    fn place(&mut self, screen: Rect) {
        let size = Self::size(screen);
        if !self.user_positioned {
            self.position=Some(egui::pos2(screen.right()-size.x,screen.top()+screen.height()*0.15));
        }
        let position = self.position.get_or_insert(egui::pos2(
            screen.right()-size.x, screen.top()+screen.height()*0.15));
        position.x = position.x.clamp(screen.left(), screen.right()-size.x);
        position.y = position.y.clamp(screen.top(), screen.bottom()-size.y);
        match self.docked {
            Some(Edge::Right) => position.x=screen.right()-size.x,
            Some(Edge::Left) => position.x=screen.left(),
            Some(Edge::Top) => position.y=screen.top(),
            Some(Edge::Bottom) => position.y=screen.bottom()-size.y,
            None => {},
        }
    }
    fn nearest_edge(&self, screen: Rect) -> Option<Edge> {
        let rect=Rect::from_min_size(self.position?, Self::size(screen));
        [(Edge::Right,screen.right()-rect.right()), (Edge::Left,rect.left()-screen.left()),
         (Edge::Top,rect.top()-screen.top()), (Edge::Bottom,screen.bottom()-rect.bottom())]
            .into_iter().min_by(|a,b|a.1.total_cmp(&b.1))
            .filter(|(_,distance)|*distance<=DOCK_DISTANCE).map(|(edge,_)|edge)
    }
    fn rectangles(&self, screen: Rect) -> (Rect,Rect) {
        let size=Self::size(screen);
        let mut position=self.position.unwrap();
        match self.docked {
            Some(Edge::Right)=>position.x=screen.right()-size.x/3.0,
            Some(Edge::Left)=>position.x=screen.left()-size.x*2.0/3.0,
            Some(Edge::Top)=>position.y=screen.top()-size.y*2.0/3.0,
            Some(Edge::Bottom)=>position.y=screen.bottom()-size.y/3.0,
            None=>{},
        }
        let full=Rect::from_min_size(position,size);
        (full,full.intersect(screen))
    }
    fn dismiss(&mut self, screen: Rect, open: &mut bool) {
        *open=false;
        self.pinned=false;
        self.blur_since=None;
        self.suppressed=true;
        self.docked=self.nearest_edge(screen);
    }
    fn show(&mut self, ctx: &egui::Context, open: &mut bool) -> (Rect,bool) {
        let screen=ctx.screen_rect();
        let now=ctx.input(|i|i.time);
        let first_frame=self.position.is_none();
        if first_frame {self.idle_since=now;self.suppressed=true;}
        self.place(screen);
        // Enter fullscreen already folded, including its first sizing pass.
        if first_frame {self.docked=self.nearest_edge(screen);}
        self.place(screen);
        let (_,hit)=self.rectangles(screen);
        let (hovered, down, focused)=ctx.input(|i|(
            i.pointer.hover_pos().is_some_and(|p|hit.contains(p)),i.pointer.any_down(),i.focused));
        if !hovered && !down { self.suppressed=false; }
        if !*open { self.pinned=false; self.blur_since=None; }
        if hovered && focused && !down && !self.suppressed {
            self.docked=None;
            *open=true;
        }
        let busy=*open || hovered || down;
        if busy || self.nearest_edge(screen).is_none() {self.idle_since=now;}
        if self.docked.is_none() && !busy && self.nearest_edge(screen).is_some() {
            let remaining=DOCK_DELAY-(now-self.idle_since);
            if remaining<=0.0 {self.docked=self.nearest_edge(screen);self.place(screen);}
            else {ctx.request_repaint_after(std::time::Duration::from_secs_f64(remaining));}
        }
        let (full,hit)=self.rectangles(screen);
        let response=egui::Area::new("floating-control-handle".into())
            .order(egui::Order::Foreground).constrain(false).movable(false).fixed_pos(hit.min)
            .show(ctx, |ui| {
                let (rect,response)=ui.allocate_exact_size(hit.size(),egui::Sense::click_and_drag());
                let full=full.translate(rect.min-hit.min);
                let painter=ui.painter().with_clip_rect(screen);
                let accent=super::theme::ACCENT;
                painter.rect(full.shrink(0.75),9.0,Color32::WHITE,
                    Stroke::new(1.2,if response.hovered() || *open {accent}else{Color32::from_rgb(177,195,218)}));
                if self.docked.is_none() {
                    for y in [-4.0,4.0] {
                        let pill=Rect::from_center_size(full.center()+egui::vec2(0.0,y),egui::vec2(14.0,4.0));
                        painter.rect_stroke(pill,2.0,Stroke::new(1.2,accent));
                        painter.circle_filled(pill.center()+egui::vec2(if y<0.0 {-3.0}else{3.0},0.0),1.1,accent);
                    }
                }
                response.on_hover_cursor(egui::CursorIcon::PointingHand)
            }).inner;
        if response.dragged() {
            self.user_positioned=true;
            let folded=self.docked.take().is_some();
            if folded {
                if let Some(pointer)=response.interact_pointer_pos() {self.position=Some(pointer-Self::size(screen)/2.0);}
            } else {self.position=Some(self.position.unwrap()+response.drag_delta());}
            self.place(screen);
            *open=false;
            self.pinned=false;
            self.blur_since=None;
            self.suppressed=true;
            self.idle_since=now;
            ctx.request_repaint();
        } else if response.clicked() {
            self.docked=None;
            *open=true;
            self.pinned=true;
            self.blur_since=None;
            self.suppressed=false;
            ctx.request_repaint();
        }
        if response.drag_stopped() {self.idle_since=now;}
        (response.rect,response.dragged() || response.drag_stopped())
    }
    fn finish_frame(&mut self, ctx: &egui::Context, open: &mut bool, regions: &[Rect]) {
        if !*open {return;}
        let (now,focused,inside,pressed,escape)=ctx.input(|i|(
            i.time,i.focused,i.pointer.hover_pos().is_some_and(|p|regions.iter().any(|r|r.contains(p))),
            i.pointer.any_pressed(),i.key_pressed(egui::Key::Escape)));
        if escape || (pressed && !inside) {
            if let Some(id)=ctx.memory(|m|m.focused()) {ctx.memory_mut(|m|m.surrender_focus(id));}
            self.dismiss(ctx.screen_rect(),open);return;
        }
        if focused && inside && pressed {self.pinned=true;}
        // Text editing owns keyboard focus even when the pointer leaves. A
        // previously clicked button alone must not keep the menu open forever.
        let editing=editing_in(ctx,regions);
        if focused && (inside || editing) {self.blur_since=None;return;}
        if !self.pinned {
            self.dismiss(ctx.screen_rect(),open);
        } else {
            let since=*self.blur_since.get_or_insert(now);
            let remaining=DOCK_DELAY-(now-since);
            if remaining<=0.0 {self.dismiss(ctx.screen_rect(),open);}
            else {ctx.request_repaint_after(std::time::Duration::from_secs_f64(remaining));}
        }
        if !*open {ctx.request_repaint();}
    }
}

/// Persisted TextEdit state is not proof that a visible menu owns focus.
fn editing_in(ctx: &egui::Context, regions: &[Rect]) -> bool {
    ctx.memory(|m|m.focused()).is_some_and(|id|
        egui::text_edit::TextEditState::load(ctx,id).is_some()
            && ctx.read_response(id).is_some_and(|response|
                regions.iter().any(|rect|rect.contains(response.rect.center()))))
}

fn pointer_in(ctx: &egui::Context, regions: &[Rect]) -> bool {
    ctx.input(|i|i.focused && i.pointer.hover_pos()
        .is_some_and(|p|regions.iter().any(|rect|rect.contains(p))))
}

/// Child panels follow the pointer across their row, panel and narrow bridge.
/// Only an active edit/drag may retain them after the pointer leaves.
fn retain_children(ctx: &egui::Context, regions: &[Rect]) -> bool {
    pointer_in(ctx,regions) || editing_in(ctx,regions) || ctx.is_using_pointer()
}

/// Only the narrow gap between adjacent controls belongs to the menu. This
/// keeps hover navigation continuous without swallowing a large video region.
fn hover_bridge(a: Rect, b: Rect) -> Option<Rect> {
    if a.right()<=b.left() || b.right()<=a.left() {
        let (left,right)=if a.right()<=b.left(){(a.right(),b.left())}else{(b.right(),a.left())};
        let (top,bottom)=(a.top().min(b.top()),a.bottom().max(b.bottom()));
        (bottom>top).then(||Rect::from_min_max(egui::pos2(left,top),egui::pos2(right,bottom)))
    } else {
        let (top,bottom)=if a.bottom()<=b.top(){(a.bottom(),b.top())}else if b.bottom()<=a.top(){(b.bottom(),a.top())}else{return None;};
        let (left,right)=(a.left().min(b.left()),a.right().max(b.right()));
        (right>left).then(||Rect::from_min_max(egui::pos2(left,top),egui::pos2(right,bottom)))
    }
}

fn popup_frame(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::popup(ui.style())
        .fill(Color32::WHITE)
        .stroke(Stroke::new(1.0, super::theme::BORDER))
        .rounding(10.0)
        .inner_margin(egui::Margin::same(6.0))
        .shadow(egui::epaint::Shadow {
            offset: egui::vec2(0.0, 3.0),
            blur: 10.0,
            spread: 0.0,
            color: Color32::from_black_alpha(22),
        })
}

/// Fixed gutters keep icons, selection dots, labels and arrows aligned.
fn menu_row(
    ui: &mut egui::Ui,
    label: &str,
    kind: Option<Icon>,
    selected: bool,
    arrow: bool,
    hint: &str,
    active: bool,
) -> egui::Response {
    menu_control(ui,label,kind,selected,arrow,hint,active,None)
}
fn menu_toggle(ui:&mut egui::Ui,label:&str,enabled:bool,hint:&str)->egui::Response {
    menu_control(ui,label,None,false,false,hint,false,Some(enabled))
}
fn menu_control(ui:&mut egui::Ui,label:&str,kind:Option<Icon>,selected:bool,arrow:bool,hint:&str,active:bool,toggle:Option<bool>)->egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), egui::Sense::click());
    let ink = if !ui.is_enabled() {
        Color32::from_gray(160)
    } else if label == uu_i18n::text("退出串流") {
        Color32::from_rgb(187, 65, 55)
    } else {
        MENU_INK
    };
    if ui.is_enabled() && (response.hovered() || active || selected) {
        ui.painter()
            .rect_filled(rect.shrink(1.), 7.0, super::theme::TINT);
    }
    if let Some(kind) = kind {
        icon(
            ui.painter(),
            Rect::from_center_size(
                egui::pos2(rect.left() + 16.0, rect.center().y),
                egui::vec2(16.0, 16.0),
            ),
            kind,
            ink,
        );
    }
    let label_x=rect.left()+if kind.is_some(){36.}else{13.};
    let end_space=if toggle.is_some(){56.}else if arrow||selected{30.}else{12.};
    let hint_galley=(!hint.is_empty()).then(||{
        let mut job=egui::text::LayoutJob::simple_singleline(hint.into(),egui::FontId::proportional(12.),super::theme::MUTED);
        job.wrap.max_width=(rect.width()*0.42).max(1.);job.wrap.max_rows=1;
        ui.fonts(|fonts|fonts.layout_job(job))
    });
    let hint_width=hint_galley.as_ref().map_or(0.,|g|g.size().x+18.);
    let mut job=egui::text::LayoutJob::simple_singleline(label.into(),egui::FontId::proportional(14.),if selected{super::theme::ACCENT}else{ink});
    job.wrap.max_width=(rect.right()-label_x-end_space-hint_width-6.).max(1.);job.wrap.max_rows=1;job.wrap.break_anywhere=true;
    let galley=ui.fonts(|fonts|fonts.layout_job(job));
    ui.painter().galley(egui::pos2(label_x,rect.center().y-galley.size().y/2.),galley,ink);
    if selected && !arrow {
        let c=egui::pos2(rect.right()-14.,rect.center().y);
        ui.painter().circle_filled(c,7.,super::theme::ACCENT);
        ui.painter().line_segment([c+egui::vec2(-3.,0.),c+egui::vec2(-1.,2.)],Stroke::new(1.4,Color32::WHITE));
        ui.painter().line_segment([c+egui::vec2(-1.,2.),c+egui::vec2(3.,-2.)],Stroke::new(1.4,Color32::WHITE));
    }
    if arrow {
        let center = egui::pos2(rect.right() - 13.0, rect.center().y);
        ui.painter().line_segment(
            [center - egui::vec2(2.0, 3.0), center + egui::vec2(1.0, 0.0)],
            Stroke::new(1.0, Color32::from_gray(125)),
        );
        ui.painter().line_segment(
            [
                center + egui::vec2(1.0, 0.0),
                center + egui::vec2(-2.0, 3.0),
            ],
            Stroke::new(1.0, Color32::from_gray(125)),
        );
    }
    if let Some(galley)=hint_galley {
        let pos=egui::pos2(rect.right()-end_space-galley.size().x,rect.center().y-galley.size().y/2.);
        ui.painter().rect_filled(Rect::from_min_size(pos,galley.size()).expand2(egui::vec2(5.,2.)),4.,Color32::from_rgb(238,243,249));
        ui.painter().galley(pos,galley,super::theme::MUTED);
    }
    if let Some(enabled)=toggle {
        let track=Rect::from_center_size(egui::pos2(rect.right()-25.,rect.center().y),egui::vec2(32.,18.));
        ui.painter().rect_filled(track,9.,if !ui.is_enabled(){Color32::from_gray(220)}else if enabled{super::theme::ACCENT}else{Color32::from_rgb(192,204,219)});
        ui.painter().circle_filled(track.center()+egui::vec2(if enabled{7.}else{-7.},0.),6.5,Color32::WHITE);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}
fn separator(ui: &mut egui::Ui) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 9.0), egui::Sense::hover());
    ui.painter().line_segment(
        [rect.left_center(), rect.right_center()],
        Stroke::new(1.0, Color32::from_rgb(235,240,244)),
    );
}
fn child_position(row: Rect, screen: Rect, width: f32) -> Pos2 {
    let x = if row.right() + width + 9.0 <= screen.right() {
        row.right() + 5.0
    } else {
        row.left() - width - 13.0
    };
    egui::pos2(x.max(screen.left()), row.top() - 4.0)
}
/// The root remains visible while the chosen panel opens beside it.
fn side_submenu<R>(
    ui: &mut egui::Ui,
    blocked: &mut Vec<Rect>,
    kind: Icon,
    label: impl Into<String>,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    let label = label.into();
    let id = ui.make_persistent_id((ui.ctx().viewport_id(),"side-menu", &label));
    let open = ui.memory(|m| m.is_popup_open(id));
    let response = menu_row(ui, &label, Some(kind), false, true, "", open);
    if response.clicked() || response.hovered() {
        if !open {let key=egui::Id::new((ui.ctx().viewport_id(),"stream-nested-menu"));ui.data_mut(|d|d.remove::<egui::Id>(key));}
        if response.clicked(){ui.memory_mut(|m|m.toggle_popup(id));}else{ui.memory_mut(|m|m.open_popup(id));}
    }
    let mut inner = None;
    if ui.memory(|m| m.is_popup_open(id)) {
        let screen = ui.ctx().screen_rect().shrink(4.0);
        let area = egui::Area::new(id)
            .order(egui::Order::Foreground)
            .fixed_pos(child_position(response.rect, screen, MENU_WIDTH))
            .constrain_to(screen)
            .show(ui.ctx(), |ui| {
                popup_frame(ui).show(ui, |ui| {
                    ui.set_width(MENU_WIDTH);
                    ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                    egui::ScrollArea::vertical()
                        .max_height((screen.height() - 16.0).max(64.0))
                        .show(ui, |ui| {
                            inner = Some(
                                ui.with_layout(
                                    egui::Layout::top_down_justified(egui::Align::Min),
                                    body,
                                )
                                .inner,
                            );
                        });
                });
            });
        blocked.push(response.rect);
        if let Some(bridge)=hover_bridge(response.rect,area.response.rect) {blocked.push(bridge);}
        blocked.push(area.response.rect);
    }
    egui::InnerResponse::new(inner, response)
}
fn menu_heading(ui:&mut egui::Ui,title:&str,status:&str) {
    ui.horizontal(|ui|{ui.add_space(7.);ui.label(RichText::new(title).size(11.5).color(super::theme::MUTED).strong());if !status.is_empty(){ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{ui.label(RichText::new(status).size(11.5).color(super::theme::ACCENT));});}});ui.add_space(4.);
}
fn nested_submenu<R>(ui:&mut egui::Ui,blocked:&mut Vec<Rect>,label:&str,status:&str,body:impl FnOnce(&mut egui::Ui)->R)->egui::InnerResponse<Option<R>> {
    let key=egui::Id::new((ui.ctx().viewport_id(),"stream-nested-menu"));let id=ui.make_persistent_id(("nested",label));
    let open=ui.data(|d|d.get_temp::<egui::Id>(key))==Some(id);
    let response=menu_row(ui,label,None,false,true,status,open);
    if response.hovered()||response.clicked(){ui.data_mut(|d|d.insert_temp(key,id));}
    let mut inner=None;
    if ui.data(|d|d.get_temp::<egui::Id>(key))==Some(id) {
        let screen=ui.ctx().screen_rect().shrink(4.);
        let area=egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(child_position(response.rect,screen,MENU_WIDTH)).constrain_to(screen).show(ui.ctx(),|ui|{
            popup_frame(ui).show(ui,|ui|{ui.set_width(MENU_WIDTH);ui.spacing_mut().item_spacing=egui::Vec2::ZERO;
                egui::ScrollArea::vertical().max_height((screen.height()-24.).max(1.)).show(ui,|ui|{inner=Some(ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min),body).inner);});
            });
        });
        blocked.push(response.rect);blocked.push(area.response.rect);if let Some(bridge)=hover_bridge(response.rect,area.response.rect){blocked.push(bridge);}
    }
    egui::InnerResponse::new(inner,response)
}
fn hud_toggle(ui:&mut egui::Ui,enabled:&mut bool,bindings:&uu_core::hotkeys::Bindings)->bool {
    let hint=bindings.label(uu_core::hotkeys::Action::Stats);
    if menu_toggle(ui,uu_i18n::text("启用 HUD"),*enabled,&hint).on_hover_text(uu_i18n::text("开关精简HUD；悬停查看诊断和历史曲线。")).clicked(){*enabled=!*enabled;true}else{false}
}
fn unavailable(ui: &mut egui::Ui, kind: Icon, label: &str, hint: &str) {
    ui.add_enabled_ui(false, |ui| {
        menu_row(ui, label, Some(kind), false, true, "", false)
    })
    .inner
    .on_disabled_hover_text(hint);
}
#[derive(Clone, Copy, PartialEq)]
pub(super) enum QualityDetail {
    Bitrate,
    Resolution,
    FrameRate,
    RateMode,
}

pub(super) struct StreamMenu {
    pub visible: bool,
    pub open: bool,
    pub anchor: Pos2,
    pub(super) trigger_rect: Option<Rect>,
    pub(super) quality_detail: Option<(QualityDetail, Rect)>,
    pub(super) floating: FloatingHandle,
}
impl Default for StreamMenu {
    fn default() -> Self {
        Self {
            visible: true,
            open: false,
            anchor: Pos2::ZERO,
            trigger_rect: None,
            quality_detail: None,
            floating: FloatingHandle::default(),
        }
    }
}

impl StreamMenu {
    /// A clicked menu may remain painted for its two-second dismissal timer.
    /// Once focus leaves, that visual delay must not stall remote input.
    pub(super) fn owns_input(&self, fullscreen: bool) -> bool {
        if fullscreen {self.floating.owns_input(self.open)} else {self.open}
    }
}

impl super::App {
    pub(super) fn connection_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("connection-top")
            .exact_height(super::window::CONNECTION_BAR_HEIGHT)
            .frame(
                egui::Frame::none()
                    .fill(super::theme::SIDEBAR)
                    .inner_margin(egui::Margin::symmetric(10.0, 2.0)),
            )
            .show(ctx, |ui| {
                if self.mouse_captured {ui.disable();}
                ui.horizontal_centered(|ui| {
                    let (brand,drag)=ui.allocate_exact_size(egui::vec2(22.0,26.0),egui::Sense::drag());
                    super::ui::logo(ui.painter(),brand);
                    if drag.drag_started(){ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);}
                    let title = self.stream_target.as_ref()
                        .map(|target|target.alias.as_str())
                        .unwrap_or(uu_i18n::text("远程桌面"));
                    let roomy=ui.available_width()>740.0;
                    let wide=ui.available_width()>1100.0;
                    if self.workspace_tabs.is_none() && ui.available_width()>520.0 {ui.add_sized([76.0,24.0],egui::Label::new(RichText::new(title).size(12.0)).truncate());}
                    ui.separator();
                    if self.workspace_tabs.is_some() || self.supports_remote(uu_core::capabilities::Capability::Displays){self.display_tabs(ui);}
                    if (self.view_only || (self.uses_engine_controls()&&!self.supports_remote(uu_core::capabilities::Capability::Input))) && (roomy || self.uses_engine_controls()) {
                        let (text, help) = if self.uses_engine_controls() && !self.view_only && self.native_input_available() {
                            let requested = self.controller.as_ref().and_then(|h|h.native_input.as_ref()).is_some_and(|s|s.requested());
                            if requested {(uu_i18n::text("等待控制权"), uu_i18n::text("主机一次允许一个控制端操作；当前控制端释放后会自动重试。"))}
                            else {(uu_i18n::text("控制已暂停"), uu_i18n::text("点击画面或恢复控制可重新申请，快捷键可在输入设置中调整。"))}
                        } else {(uu_i18n::text("仅观看"), uu_i18n::text("当前未启用远端输入；主机需允许控制并具有可用的输入后端。"))};
                        ui.label(RichText::new(text).color(Color32::from_rgb(98, 125, 160))).on_hover_text(help);
                        if !self.view_only && self.native_input_available() && !self.input_capture.requested()
                            && ui.small_button(uu_i18n::text("恢复控制")).on_hover_text(uu_i18n::text("重新请求此串流的键鼠控制")).clicked() {
                            self.input_capture.request();
                            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                            self.platform.focus_window();
                        }
                    }
                    if wide && !self.view_only && self.controller.is_some() && !self.input_ready() && (!self.uses_engine_controls()||self.displays.switching()) {
                        ui.label(
                            RichText::new(if self.displays.switching(){uu_i18n::text("正在切换显示器…")}else{uu_i18n::text("输入连接中…")})
                                .size(12.0)
                                .color(Color32::from_gray(110)),
                        )
                        .on_hover_text(uu_i18n::text("视频和输入分别建立连接，输入就绪后可操作"));
                    }
                    if let Some(controller) = self.controller.as_ref().filter(|_|roomy && !self.uses_engine_controls()) {
                        ui.label(
                            RichText::new(format!(
                                "{:.1} Mbps",
                                controller
                                    .stats
                                    .bitrate_kbps
                                    .load(std::sync::atomic::Ordering::Relaxed)
                                    as f32
                                    / 1000.0
                            ))
                            .size(11.0)
                            .color(Color32::from_rgb(51, 159, 126)),
                        );
                    }
                    let width = (ui.available_width() - 236.0).max(0.0);
                    let (_, drag) =
                        ui.allocate_exact_size(egui::vec2(width, 28.0), egui::Sense::drag());
                    if drag.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, Icon::Close, uu_i18n::text("退出串流")).clicked() {
                            self.disconnect();
                        }
                        if icon_button(ui, Icon::Max, uu_i18n::text("最大化 / 还原窗口")).clicked() {
                            self.toggle_maximized(ctx);
                        }
                        if icon_button(ui, Icon::Min, uu_i18n::text("最小化")).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                        let control = icon_label_button(ui, Icon::Settings, uu_i18n::text("设置"));
                        self.stream_menu.trigger_rect = Some(control.rect);
                        if control.clicked() {
                            self.displays.menu_open=false;
                            self.stream_menu.open = !self.stream_menu.open;
                            if !self.stream_menu.open {
                                ctx.memory_mut(|m| m.close_popup());
                            }
                            self.stream_menu.anchor = control.rect.left_bottom();
                        }
                        self.audio_button(ui);
                    });
                });
            });
    }

    fn display_tabs(&mut self, ui: &mut egui::Ui) {
        if let Some(tabs)=&self.workspace_tabs {tabs.show(ui);return;}
        let screens=self.remote_screens();
        let reserve=if ui.available_width()>660.0{345.0}else{250.0};
        let width=(ui.available_width()-reserve).clamp(75.0,540.0);
        let mut chosen=None;
        let mut reorder=None;
        ui.allocate_ui_with_layout(egui::vec2(width,30.0),egui::Layout::left_to_right(egui::Align::Center),|ui| {
            egui::ScrollArea::horizontal().id_salt("remote-display-tabs")
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .max_width((width-30.0).max(60.0)).auto_shrink([true,true]).show(ui,|ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x=2.0;
                        for screen in &screens.screens {
                            let selected=self.displays.selected==Some(screen.id);
                            let label=self.displays.layout.label(screen);
                            let (rect,response)=ui.allocate_exact_size(egui::vec2(140.0,30.0),egui::Sense::click_and_drag());
                            if selected || response.hovered() {ui.painter().rect_filled(rect,7.0,if selected{Color32::WHITE}else{Color32::from_rgb(232,235,239)});}
                            response.dnd_set_drag_payload(super::display_layout::DraggedDisplay(screen.id));
                            if response.dragged(){ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);}
                            let after=ui.input(|i|i.pointer.hover_pos().is_some_and(|p|p.x>rect.center().x));
                            if response.dnd_hover_payload::<super::display_layout::DraggedDisplay>().is_some_and(|s|s.0!=screen.id) {
                                let x=if after{rect.right()}else{rect.left()};
                                ui.painter().line_segment([egui::pos2(x,rect.top()+3.0),egui::pos2(x,rect.bottom()-3.0)],egui::Stroke::new(2.0,MENU_INK));
                                if let Some(dropped)=response.dnd_release_payload::<super::display_layout::DraggedDisplay>() {reorder=Some((dropped.0,screen.id,after));}
                            }
                            icon(ui.painter(),Rect::from_center_size(rect.left_center()+egui::vec2(16.0,0.0),egui::vec2(15.0,15.0)),Icon::Desktop,MENU_INK);
                            ui.painter().with_clip_rect(Rect::from_min_max(rect.min+egui::vec2(30.0,0.0),rect.max-egui::vec2(8.0,0.0))).text(rect.left_center()+egui::vec2(30.0,0.0),egui::Align2::LEFT_CENTER,&label,egui::FontId::proportional(12.0),MENU_INK);
                            if response.on_hover_text(uu_i18n::format!("{}{} · {} × {} · {} Hz\n拖动调整标签顺序；编号为本地标记，重连后保留",label,if screen.primary{uu_i18n::text(" · 主屏")}else{""},screen.width,screen.height,screen.refresh_hz)).clicked(){chosen=Some(screen.id);}
                        }
                        if screens.screens.is_empty(){ui.label(uu_i18n::text("等待显示器…"));}
                    });
                });
            let response=ui.add_sized([26.0,28.0],egui::Button::new(RichText::new("+").size(20.0)).frame(false)).on_hover_text(uu_i18n::text("选择远端显示器"));
            if response.clicked(){self.displays.menu_open=!self.displays.menu_open;self.stream_menu.open=false;self.context.memory_mut(|m|m.close_popup());self.displays.anchor=response.rect.left_bottom();self.release_all_keys();}
        });
        if let Some((source,target,after))=reorder{self.reorder_display_tab(source,target,after);}
        if let Some(id)=chosen{self.select_remote_screen(id);}
    }

    fn display_picker(&mut self,ctx:&egui::Context,blocked:&mut Vec<Rect>) {
        if !self.displays.menu_open || self.fullscreen{return;}
        let screen=ctx.screen_rect().shrink(4.0);
        let anchor=self.displays.anchor;
        let menu=egui::Area::new("remote-display-picker".into()).order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(anchor.x.min(screen.right()-324.0).max(screen.left()),anchor.y))
            .constrain_to(screen).show(ctx,|ui| {
                popup_frame(ui).show(ui,|ui| {
                    ui.set_width(310.0);ui.spacing_mut().item_spacing=egui::Vec2::ZERO;
                    egui::ScrollArea::vertical().max_height((screen.height()-60.0).max(80.0)).show(ui,|ui|self.display_menu(ui));
                });
            });
        blocked.push(menu.response.rect);
        if ctx.input(|i|i.key_pressed(egui::Key::Escape) || (i.pointer.any_pressed() && i.pointer.hover_pos().is_some_and(|p| !menu.response.rect.contains(p) && p.y>super::window::CONNECTION_BAR_HEIGHT))) {
            self.displays.menu_open=false;
        }
    }

    fn display_menu(&mut self, ui: &mut egui::Ui) {
        let screens=self.remote_screens();
        ui.set_min_width(260.0);
        for screen in &screens.screens {
            let response=menu_row(ui,&self.displays.layout.label(screen),None,
                self.displays.selected==Some(screen.id),false,if screen.primary {uu_i18n::text("主屏")}else{""},false)
                .on_hover_text(format!("{} · {} × {} · {} Hz · {}",screen.display_name,screen.width,screen.height,screen.refresh_hz,screen.kind_label()));
            if response.clicked(){self.select_remote_screen(screen.id);ui.close_menu();}
        }
        if screens.screens.is_empty(){ui.add_enabled(false,egui::Label::new(uu_i18n::text("等待远端显示器列表…")));}
        if !self.displays.notice.is_empty(){separator(ui);ui.label(&self.displays.notice);}
        separator(ui);
        if menu_row(ui,uu_i18n::text("显示器模式设置…"),None,false,false,"",false).on_hover_text(uu_i18n::text("设置远端物理或虚拟显示器的使用方式。")).clicked(){self.displays.menu_open=false;self.open_stream_settings();}
    }

    fn confirm_quality(&mut self, ui: &mut egui::Ui) {
        self.persist_preferences();
        self.send_capture_settings();
        self.stream_menu.quality_detail = None;
        ui.memory_mut(|m| m.close_popup());
    }

    fn picture_menu(&mut self, ui: &mut egui::Ui) {
        if self.uses_engine_controls() {
            let hdr=self.controller.as_ref().and_then(|h|*h.stats.native_hdr_response.lock().unwrap()).is_some();
            let displaying=eframe::glow_presentation::color_target_for(ui.ctx(),ui.ctx().viewport_id()).is_some_and(|c|c.hdr);
            let status=if self.controller.is_none(){"—"}else if hdr&&displaying{"HDR10"}else if hdr{"HDR → SDR"}else{"SDR"};
            let tip=if hdr&&displaying {uu_i18n::text("当前 HDR10 · 10 位 PQ / BT.2020")}else if hdr {uu_i18n::text("当前 HDR10 → SDR 映射")}else{uu_i18n::text("当前 SDR；HDR 需要两端显示输出和编解码支持")};
            ui.add_enabled_ui(cfg!(windows),|ui|{if menu_toggle(ui,uu_i18n::text("优先 HDR"),self.preferences.hdr,status).on_hover_text(format!("{tip}\n{}",uu_i18n::text("需要两端显示输出、采集和编解码支持；连接时协商，无法使用时回退SDR。"))).clicked(){self.preferences.hdr=!self.preferences.hdr;self.persist_preferences();self.send_capture_settings();}});
            separator(ui);
        }
        if self.stream_menu.quality_detail.is_some_and(|(kind,_)|!matches!(kind,QualityDetail::Resolution|QualityDetail::FrameRate)) {self.stream_menu.quality_detail=None;}
        let response=menu_row(ui,uu_i18n::text("分辨率"),None,false,true,"",false);
        if response.hovered()||response.clicked(){self.stream_menu.quality_detail=Some((QualityDetail::Resolution,response.rect));}
        separator(ui);
        let max_fps=self.client_support.max_fps;
        for fps in [30,60,120,165].into_iter().filter(move|fps|*fps<=max_fps) {
            let response=menu_row(ui,&uu_i18n::format!("{fps} 帧"),None,self.target_fps==fps,false,"",false);
            if response.hovered(){self.stream_menu.quality_detail=None;}
            if response.clicked(){self.target_fps=fps;self.fps_draft=fps;self.confirm_quality(ui);}
        }
        let response=menu_row(ui,uu_i18n::text("其它帧数"),None,![30,60,120,165].contains(&self.target_fps),true,"",false);
        if response.hovered()||response.clicked(){self.stream_menu.quality_detail=Some((QualityDetail::FrameRate,response.rect));}
        separator(ui);
        let response=menu_toggle(ui,uu_i18n::text("垂直同步"),self.preferences.vsync,"");
        if response.hovered(){self.stream_menu.quality_detail=None;}
        if response.clicked(){
            self.preferences.vsync=!self.preferences.vsync;
            eframe::request_vsync(ui.ctx(),self.preferences.vsync);
            self.persist_preferences();ui.close_menu();
        }
    }

    fn encoding_menu(&mut self, ui: &mut egui::Ui) {
        if self.stream_menu.quality_detail.is_some_and(|(kind,_)|!matches!(kind,QualityDetail::Bitrate|QualityDetail::RateMode)) {self.stream_menu.quality_detail=None;}
        let options: &[(super::settings::CodecPreference,&str)]=if self.uses_engine_controls() {
            &[(super::settings::CodecPreference::Auto,uu_i18n::text("自动协商")),(super::settings::CodecPreference::H264,"H.264"),
              (super::settings::CodecPreference::Hevc,"H.265 / HEVC"),(super::settings::CodecPreference::Av1,"AV1"),(super::settings::CodecPreference::Pyrowave,uu_i18n::text("PyroWave（实验性 / SDR）"))]
        }else{
            &[(super::settings::CodecPreference::H264,"H.264"),(super::settings::CodecPreference::Hevc,uu_i18n::text("H.265 / HEVC（优先）"))]
        };
        let actual=self.controller.as_ref().map(|h|h.stats.codec.lock().unwrap().trim_start_matches("video/").to_owned()).filter(|v|!v.is_empty());
        for (codec,label) in options {
            use uu_core::native::VideoCodec as V;
            let wire=match codec {super::settings::CodecPreference::Auto=>None,super::settings::CodecPreference::H264=>Some(V::H264),super::settings::CodecPreference::Hevc=>Some(V::Hevc),super::settings::CodecPreference::Av1=>Some(V::Av1),super::settings::CodecPreference::Pyrowave=>Some(V::Pyrowave)};
            if wire.is_some_and(|value|!self.client_support.codecs.contains(&value)){continue;}
            let selected=self.preferences.codec==*codec || (!self.uses_engine_controls()&&*codec==super::settings::CodecPreference::Hevc
                &&self.preferences.codec==super::settings::CodecPreference::Auto&&self.preferences.hevc_preferred());
            let hint=if *codec==super::settings::CodecPreference::Auto {actual.as_deref().unwrap_or("—")}else{""};
            let response=menu_row(ui,label,None,selected,false,hint,false);
            if response.hovered(){self.stream_menu.quality_detail=None;}
            if response.clicked(){self.preferences.codec=*codec;self.preferences.prefer_hevc=*codec==super::settings::CodecPreference::Hevc;
                if *codec==super::settings::CodecPreference::Pyrowave{self.preferences.rate_control=Some(uu_core::native_quality::RateMode::Cbr);}
                self.confirm_quality(ui);}
        }
        separator(ui);
        if self.controller.as_ref().is_some_and(|h|h.stats.native_rate_response.lock().unwrap().is_some()) {
            let value=match self.preferences.rate_control {None=>uu_i18n::text("跟随主机"),Some(uu_core::native_quality::RateMode::Cbr)=>"CBR",Some(uu_core::native_quality::RateMode::Vbr)=>"VBR"};
            let response=menu_row(ui,uu_i18n::text("编码模式"),None,false,true,value,self.stream_menu.quality_detail.is_some_and(|(v,_)|v==QualityDetail::RateMode));
            if response.hovered()||response.clicked(){self.stream_menu.quality_detail=Some((QualityDetail::RateMode,response.rect));}
            response.on_hover_text(uu_i18n::text("连接偏好：跟随主机、固定码率CBR或可变码率VBR。主机应用后可在HUD查看实际模式。"));
            separator(ui);
        }
        menu_heading(ui,uu_i18n::text("码率"),"");
        let native=!self.client_support.host_managed_bitrate;
        let actual=self.controller.as_ref().map(|h|{let(w,h)=h.stats.res();[w,h]});
        let codec=self.controller.as_ref().map(|h|h.stats.codec.lock().unwrap().clone());
        let recommended=super::settings::recommended_bitrate(&self.preferences,actual,codec.as_deref());
        let order=if native{[5,1,2,3,4]}else{[0,1,2,3,4]};
        for index in order {
            let label=super::BITRATE_PRESETS[index];
            let label=uu_i18n::text(label);
            let title=if index==5{uu_i18n::text("推荐码率")}else if index==0{uu_i18n::text("原画 · 服务端策略")}else{label};
            let hint=if index==5 {recommended.map_or_else(||uu_i18n::text("自动计算").into(),|kbps|format!("{:.1} Mbps",kbps as f64/1000.))}else if index==4{format!("{} Mbps",self.custom_mbps)}else{String::new()};
            let custom=index==4;
            let response=menu_row(ui,title,None,self.bitrate_idx==index||(native&&index==5&&self.bitrate_idx==0)||(!native&&index==0&&self.bitrate_idx==5),custom,&hint,false);
            if custom&&(response.hovered()||response.clicked()){self.stream_menu.quality_detail=Some((QualityDetail::Bitrate,response.rect));}
            else if response.hovered(){self.stream_menu.quality_detail=None;}
            if !custom&&response.clicked(){self.bitrate_idx=index;self.confirm_quality(ui);}
            response.on_hover_text(match index {
                5=>uu_i18n::text("按当前实际分辨率、帧率和编码计算LAN Quality推荐值，随参数改变。PyroWave需要较高带宽。"),
                0=>uu_i18n::text("跟随主机的画质策略；具体码率由主机决定。可选择推荐码率或自定义目标。"),
                4=>uu_i18n::text("输入自己的目标码率，单位Mbps。设置保存在当前串流偏好中。"),
                _=>uu_i18n::text("固定目标码率；画面复杂度和编码实现会使实际接收码率有所差异。"),
            });
        }
    }

    fn audio_menu(&mut self,ui:&mut egui::Ui){
        use std::sync::atomic::Ordering::Relaxed;
        let audio=self.controller.as_ref().map(|h|h.stats.audio.clone());
        let mut muted=audio.as_ref().map_or(self.preferences.muted,|a|a.muted.load(Relaxed));
        let mut volume=audio.as_ref().map_or(self.preferences.volume,|a|a.volume.load(Relaxed));
        let mut changed=false;
        if menu_toggle(ui,uu_i18n::text("启用声音"),!muted,"").on_hover_text(uu_i18n::text("仅控制本串流窗口的声音播放。")).clicked(){muted=!muted;changed=true;}
        ui.add_space(6.);
        changed|=super::audio_controls::volume_slider(ui,&mut volume);
        if let Some(audio)=&audio {
            ui.add_space(6.);
            let status=audio.status.lock().unwrap().clone();
            menu_heading(ui,uu_i18n::text("声音"),&status);
            ui.response().on_hover_text(uu_i18n::format!("播放排队 {} ms · 接收 {} 包",audio.queue_ms.load(Relaxed),audio.packets.load(Relaxed)));
        }
        if changed {
            if let Some(audio)=audio {audio.muted.store(muted,Relaxed);audio.volume.store(volume,Relaxed);}
            self.preferences.muted=muted;self.preferences.volume=volume;
            self.settings_draft.muted=muted;self.settings_draft.volume=volume;
            self.audio_save_after=Some(std::time::Instant::now()+std::time::Duration::from_millis(250));
        }
    }
    fn rate_mode_choices(&mut self,ui:&mut egui::Ui) {
        use uu_core::native_quality::RateMode;
        let response=self.controller.as_ref().and_then(|h|*h.stats.native_rate_response.lock().unwrap());
        menu_heading(ui,uu_i18n::text("编码模式"),response.map_or(uu_i18n::text("连接后协商"),|r|r.applied.label()));
        for (mode,label,tip) in [(None,uu_i18n::text("跟随主机"),uu_i18n::text("使用主机启动时保存的默认码率模式。")),(Some(RateMode::Cbr),uu_i18n::text("CBR · 固定码率"),uu_i18n::text("使用固定目标码率控制，适合带宽稳定、重视延迟的串流。PyroWave仅支持CBR。")),(Some(RateMode::Vbr),uu_i18n::text("VBR · 可变码率"),uu_i18n::text("按画面复杂度使用可变码率；具体行为取决于主机编码器。"))]{
            let supported=self.preferences.codec!=super::settings::CodecPreference::Pyrowave || mode==Some(RateMode::Cbr) || (mode.is_none()&&response.is_some_and(|r|r.default==RateMode::Cbr));
            ui.add_enabled_ui(supported,|ui|{if menu_row(ui,label,None,self.preferences.rate_control==mode,false,"",false).on_hover_text(tip).clicked(){self.preferences.rate_control=mode;self.confirm_quality(ui);}}).response.on_disabled_hover_text(uu_i18n::text("PyroWave仅支持固定码率CBR。"));
        }
    }

    fn quality_detail(&mut self, ctx: &egui::Context, blocked: &mut Vec<Rect>) {
        let Some((kind, row)) = self.stream_menu.quality_detail else {
            return;
        };
        let screen = ctx.screen_rect().shrink(4.0);
        let mut position = child_position(row, screen, MENU_WIDTH);
        if ctx.memory(|m| m.area_rect(egui::Id::new("stream-control-menu")))
            .is_some_and(|root| root.center().x > row.center().x)
            && row.left() - screen.left() >= MENU_WIDTH+13.
        {
            position.x = row.left() - MENU_WIDTH-13.;
        }
        let response = egui::Area::new("quality-detail".into())
            .order(egui::Order::Foreground)
            .fixed_pos(position)
            .constrain_to(screen)
            .show(ctx, |ui| {
                popup_frame(ui).show(ui, |ui| {
                    ui.set_width(MENU_WIDTH);
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                    ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
                        match kind {
                            QualityDetail::RateMode => self.rate_mode_choices(ui),
                            QualityDetail::Bitrate => {
                                ui.add_space(5.0);
                                ui.horizontal(|ui| {
                                    ui.label(uu_i18n::text("码率"));
                                    ui.add(
                                        egui::DragValue::new(&mut self.custom_mbps)
                                            .range(1..=1000)
                                            .suffix(" Mbps"),
                                    );
                                });
                                ui.add_space(5.0);
                                separator(ui);
                                if menu_row(ui, uu_i18n::text("确认"), None, false, false, "", false).clicked() {
                                    self.bitrate_idx = 4;
                                    self.confirm_quality(ui);
                                }
                            }
                            QualityDetail::FrameRate => {
                                let max_fps=self.client_support.max_fps;
                                for fps in [24,90,120,144].into_iter().filter(move|fps|*fps<=max_fps) {
                                    if menu_row(ui,&uu_i18n::format!("{fps} 帧"),None,self.target_fps==fps,false,"",false).clicked() {
                                        self.target_fps=fps;self.fps_draft=fps;self.confirm_quality(ui);
                                    }
                                }
                                separator(ui);
                                ui.label(uu_i18n::text("自定义帧数"));
                                ui.add_space(5.0);
                                ui.horizontal(|ui| {
                                    ui.label(uu_i18n::text("帧率"));
                                    ui.add(
                                        egui::DragValue::new(&mut self.fps_draft)
                                            .range(5..=self.client_support.max_fps.clamp(5,240))
                                            .suffix(" FPS"),
                                    );
                                });
                                ui.add_space(5.0);
                                separator(ui);
                                if menu_row(ui, uu_i18n::text("确认"), None, false, false, "", false).clicked() {
                                    self.target_fps = self.fps_draft.clamp(5,self.client_support.max_fps.clamp(5,240));
                                    self.confirm_quality(ui);
                                }
                            }
                            QualityDetail::Resolution => {
                                for (index, label) in [
                                    uu_i18n::text("保持远端桌面分辨率"),
                                    uu_i18n::text("输出 1920 × 1080"),
                                    uu_i18n::text("输出 2560 × 1440"),
                                    uu_i18n::text("输出 3840 × 2160"),
                                ]
                                .iter()
                                .enumerate()
                                {
                                    if menu_row(
                                        ui,
                                        label,
                                        None,
                                        self.resolution_idx == index,
                                        false,
                                        "",
                                        false,
                                    )
                                    .clicked()
                                    {
                                        self.resolution_idx = index;
                                        self.confirm_quality(ui);
                                    }
                                }
                                separator(ui);
                                ui.response().on_hover_text(uu_i18n::text("输出尺寸保持宽高比；更改会重新准备媒体，不改变远端桌面显示模式。"));
                            }

                        }
                    });
                });
            });
        if let Some(bridge)=hover_bridge(row,response.response.rect) {blocked.push(bridge);}
        blocked.push(response.response.rect);
    }

    /// Draw first so the stream can exclude every control rectangle from input.
    pub(super) fn stream_controls(&mut self, ctx: &egui::Context) -> Vec<Rect> {
        let mut blocked = Vec::new();
        if self.mouse_captured || self.local_dialog_open() {return blocked;}
        if !self.fullscreen {
            if let Some(rect) = self.stream_menu.trigger_rect {
                blocked.push(rect);
            }
        }
        if self.fullscreen && self.stream_menu.visible {
            let was_open = self.stream_menu.open;
            let (rect,dragging) = self.stream_menu.floating.show(ctx, &mut self.stream_menu.open);
            // Own the whole pointer gesture even if it leaves the old hit area.
            if dragging {blocked.push(ctx.screen_rect());}
            if was_open && !self.stream_menu.open { ctx.memory_mut(|m| m.close_popup()); }
            self.stream_menu.anchor = rect.left_bottom();
            blocked.push(rect);
        }
        if !self.stream_menu.open {
            self.stream_menu.quality_detail = None;
            // Settings own their ComboBox popup while their window is open.
            // Otherwise clear old stream children so no invisible popup traps input.
            if !self.local_dialog_open() && ctx.input(|i|i.focused) {ctx.memory_mut(|m| m.close_popup());}
            self.display_picker(ctx,&mut blocked);
            return blocked;
        }
        let screen = ctx.screen_rect();
        let anchor = self.stream_menu.anchor;
        let menu_id=egui::Id::new("stream-control-menu");
        let mut pivot=egui::Align2::LEFT_TOP;
        let mut available_height=(screen.bottom()-anchor.y-8.0).max(64.0);
        let mut y=anchor.y.min(screen.bottom()-386.0).max(screen.top()+6.0);
        if self.fullscreen {
            let below=(screen.bottom()-anchor.y-10.0).max(0.0);
            let above=(anchor.y-HANDLE_SIZE-screen.top()-10.0).max(0.0);
            let previous_height=ctx.memory(|memory|memory.area_rect(menu_id))
                .map(|rect|rect.height()).unwrap_or(screen.height());
            if below>=previous_height || below>=above {
                y=anchor.y+4.0;
                available_height=below;
            } else {
                // Anchor the menu's bottom to the button's top. Let egui
                // measure its actual contents instead of assuming row count.
                pivot=egui::Align2::LEFT_BOTTOM;
                y=anchor.y-HANDLE_SIZE-4.0;
                available_height=above;
            }
        }
        let position = egui::pos2(
            anchor.x.min(screen.right() - MENU_WIDTH - 16.0).max(screen.left() + 6.0),
            y,
        );
        let mut quality_open = false;
        let child_start=blocked.len();
        let menu = egui::Area::new(menu_id)
            .order(egui::Order::Foreground)
            .pivot(pivot)
            .fixed_pos(position)
            .constrain_to(screen)
            .show(ctx, |ui| {
                // Area remembers its previous content size. Give the scroll
                // region the current viewport height, rather than retaining
                // the initial 400-point default and hiding the final actions.
                ui.set_max_height(available_height.max(1.0));
                popup_frame(ui).show(ui, |ui| {
                    ui.set_width(MENU_WIDTH);
                    ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                    egui::ScrollArea::vertical()
                        .max_height((available_height-8.0).max(1.0))
                        .min_scrolled_height(0.0)
                        .show(ui, |ui| {
                            ui.with_layout(
                                egui::Layout::top_down_justified(egui::Align::Min),
                                |ui| {
                                    {
                                    side_submenu(
                                        ui,
                                        &mut blocked,
                                        Icon::Desktop,
                                        self.display_label(),
                                        |ui| self.display_menu(ui),
                                    );
                                    }
                                    {
                                    quality_open = side_submenu(
                                        ui,
                                        &mut blocked,
                                        Icon::Picture,
                                        uu_i18n::text("画面"),
                                        |ui| self.picture_menu(ui),
                                    )
                                    .inner
                                    .is_some();
                                    quality_open |= side_submenu(ui,&mut blocked,Icon::Encoding,uu_i18n::text("编码"),|ui|self.encoding_menu(ui)).inner.is_some();
                                    }
                                    {
                                    side_submenu(ui, &mut blocked, Icon::Volume, uu_i18n::text("声音"), |ui| self.audio_menu(ui));
                                    }
                                    {
                                    side_submenu(ui, &mut blocked, Icon::Keyboard, uu_i18n::text("输入"), |ui| {
                                        let old=(self.preferences.relative_mouse,self.preferences.cursor_mode);
                                        menu_heading(ui,uu_i18n::text("光标显示"),"");
                                        for mode in super::settings::CursorMode::ALL {
                                            if menu_row(ui,mode.label(),None,self.preferences.cursor_mode==mode,false,"",false).on_hover_text(uu_i18n::text("远端光标使用主机传输的位置与形状；旧主机不提供时保留本地指针。切换不会改变视频画质。")).clicked(){self.preferences.cursor_mode=mode;}
                                        }
                                        separator(ui);menu_heading(ui,uu_i18n::text("指针移动"),"");
                                        for (relative,label,tip) in [(false,uu_i18n::text("桌面模式 · 绝对定位"),uu_i18n::text("按画面中的位置定位鼠标，适合桌面操作。")),(true,uu_i18n::text("游戏模式 · 相对移动"),uu_i18n::text("发送鼠标移动增量；单击画面捕获，失焦或进入菜单时释放。"))]{if menu_row(ui,label,None,self.preferences.relative_mouse==relative,false,"",false).on_hover_text(tip).clicked(){self.preferences.relative_mouse=relative;}}
                                        if old!=(self.preferences.relative_mouse,self.preferences.cursor_mode) {self.release_pointer_capture(ctx);self.persist_preferences();}
                                        if self.uses_engine_controls(){separator(ui);if menu_toggle(ui,uu_i18n::text("转发手柄与陀螺仪"),self.preferences.gamepad,"").on_hover_text(uu_i18n::text("取得输入控制后转发，菜单和失焦时释放；最多4个手柄。")).clicked(){self.preferences.gamepad=!self.preferences.gamepad;self.persist_preferences();}}
                                    });
                                    }
                                    {
                                    side_submenu(ui, &mut blocked, Icon::Person, uu_i18n::text("安全"), |ui| {
                                        if self.native_input_available()&&!self.supports_remote(uu_core::capabilities::Capability::Input)
                                            &&menu_row(ui,uu_i18n::text("请求输入控制"),None,false,false,"",false).on_hover_text(uu_i18n::text("向允许输入的主机请求控制权。")).clicked() {
                                            self.view_only=false;
                                            self.input_capture.request();
                                            ui.close_menu();
                                        }
                                        if menu_toggle(ui,uu_i18n::text("仅观看，禁止远程输入"),self.view_only,"").on_hover_text(uu_i18n::text("启用时释放全部远端键鼠与手柄；关闭后按主机授权恢复控制。")).clicked()
                                        {
                                            self.view_only=!self.view_only;
                                            self.release_all_keys();
                                            for message in self
                                                .pointer
                                                .release_all(self.input_screen(self.selected_track))
                                            {
                                                self.send_input(message, 15);
                                            }

                                        }
                                    });
                                    }
                                    {
                                    side_submenu(ui,&mut blocked,Icon::Folder,uu_i18n::text("剪贴板"),|ui| {
                                        if self.uses_engine_controls(){if menu_toggle(ui,uu_i18n::text("同步文本和图片"),self.preferences.native_clipboard,"").on_hover_text(uu_i18n::text("仅同步当前前台串流；文本最多30KiB，图片最多8MiB PNG，双方均需允许。")).clicked(){self.preferences.native_clipboard=!self.preferences.native_clipboard;self.persist_preferences();}}
                                        else if menu_toggle(ui,uu_i18n::text("同步文本剪贴板"),self.preferences.clipboard,"").on_hover_text(uu_i18n::text("兼容连接仅支持最多30KiB文本；双方均需允许。")).clicked(){self.preferences.clipboard=!self.preferences.clipboard;self.persist_preferences();}
                                        if self.uses_engine_controls(){ui.response().on_hover_text(uu_i18n::text("仅同步当前前台串流；文本最多30KiB，图片最多8MiB PNG，双方均需允许，不传输文件。"));}
                                        else{ui.response().on_hover_text(uu_i18n::text("兼容连接仅支持最多30KiB的文本同步。"));}
                                        if let Some(h)=&self.controller {ui.label(h.stats.clipboard.status.lock().unwrap().as_str());}
                                    });
                                    }
                                    if !self.uses_engine_controls() {
                                    unavailable(ui, Icon::Link, uu_i18n::text("外设"), uu_i18n::text("外设透传尚未接入"));
                                    unavailable(
                                        ui,
                                        Icon::Folder,
                                        uu_i18n::text("工具"),
                                        uu_i18n::text("文件、终端和端口映射尚未接入"),
                                    );
                                    }
                                    if menu_row(ui,uu_i18n::text("连接详情"),Some(Icon::Link),false,false,"",false).clicked(){self.open_connection_details();}
                                    if menu_row(ui,uu_i18n::text("串流设置"),Some(Icon::Settings),false,false,"",false).clicked(){self.open_stream_settings();}
                                    side_submenu(ui, &mut blocked, Icon::Grid, uu_i18n::text("更多"), |ui| {
                                        if hud_toggle(ui,&mut self.hud,&self.preferences.hotkeys) {self.persist_preferences();}
                                        if menu_row(
                                            ui,
                                            uu_i18n::text("隐藏悬浮入口"),
                                            None,
                                            false,
                                            false,
                                            &self.preferences.hotkeys.label(uu_core::hotkeys::Action::Menu),
                                            false,
                                        )
                                        .clicked()
                                        {
                                            self.stream_menu.visible = false;
                                            self.stream_menu.open = false;
                                            ctx.memory_mut(|m| m.close_popup());
                                        }
                                    });
                                    separator(ui);
                                    ui.scope(|ui| {
                                        if menu_row(
                                            ui,
                                            uu_i18n::text("退出串流"),
                                            Some(Icon::Exit),
                                            false,
                                            false,
                                            "",
                                            false,
                                        )
                                        .clicked()
                                        {
                                            self.stream_menu.open = false;
                                            self.disconnect();
                                        }
                                    });
                                },
                            );
                        });
                });
            });
        if quality_open {
            let detail_start=blocked.len();
            self.quality_detail(ctx, &mut blocked);
            if self.stream_menu.quality_detail.is_some() {
                if let Some((_,row))=self.stream_menu.quality_detail {blocked.push(row);}
                if !retain_children(ctx,&blocked[detail_start..]) {
                    self.stream_menu.quality_detail=None;
                    ctx.request_repaint();
                }
            }
        } else {
            self.stream_menu.quality_detail = None;
        }
        if !retain_children(ctx,&blocked[child_start..]) {
            ctx.memory_mut(|m|m.close_popup());
            self.stream_menu.quality_detail=None;
        }
        if self.fullscreen {
            if let Some(handle)=blocked.first().copied() {
                if let Some(bridge)=hover_bridge(handle,menu.response.rect) {blocked.push(bridge);}
            }
        }
        blocked.push(menu.response.rect);
        let outside_click = ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| !blocked.iter().any(|r| r.contains(p)))
        });
        if self.fullscreen {
            self.stream_menu.floating.finish_frame(ctx,&mut self.stream_menu.open,&blocked);
        } else if outside_click || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.stream_menu.open = false;
        }
        if !self.stream_menu.open {
            ctx.memory_mut(|m| m.close_popup());
        }
        blocked
    }
}

#[cfg(all(test,target_os="linux"))]
#[path="../../../tools/native/owned_display.rs"]
mod owned_display;
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os="linux")]
    #[test]
    #[ignore="explicit screenshot on a launcher-owned Xvfb with generated controller state"]
    fn isolated_menu_preview(){
        use std::{sync::{Arc,atomic::{AtomicBool,Ordering}},path::PathBuf};
        super::owned_display::check(&std::env::var("DISPLAY").unwrap()).unwrap();
        let output=PathBuf::from(std::env::var_os("ECLIPSE_MENU_PREVIEW").expect("explicit screenshot destination"));
        assert!(!output.exists());
        struct Preview {app:super::super::App,output:PathBuf,frames:u32,saved:Arc<AtomicBool>}
        impl eframe::App for Preview {
            fn update(&mut self,ctx:&egui::Context,_:&mut eframe::Frame){
                if let Some(image)=ctx.input(|i|i.events.iter().find_map(|e|if let egui::Event::Screenshot{image,..}=e{Some(image.clone())}else{None})) {
                    let data:Vec<_>=image.pixels.iter().flat_map(|p|p.to_array()).collect();
                    image::save_buffer(&self.output,&data,image.size[0] as u32,image.size[1] as u32,image::ColorType::Rgba8).unwrap();
                    self.saved.store(true,Ordering::Release);ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                egui::CentralPanel::default().frame(egui::Frame::none().fill(super::super::theme::CANVAS).inner_margin(24.)).show(ctx,|ui|{
                    ui.heading("v0.20.1 · 菜单预览");ui.add_space(18.);
                    ui.columns(3,|columns|{
                        for (index,title) in ["画面","编码","声音"].into_iter().enumerate(){
                            popup_frame(&columns[index]).show(&mut columns[index],|ui|{
                                ui.set_width(MENU_WIDTH);ui.spacing_mut().item_spacing=egui::Vec2::ZERO;
                                menu_heading(ui,title,"");separator(ui);
                                match index {0=>self.app.picture_menu(ui),1=>self.app.encoding_menu(ui),_=>self.app.audio_menu(ui)}
                            });
                        }
                    });
                });
                self.frames+=1;if self.frames==4{ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);}
                ctx.request_repaint();
            }
        }
        let saved=Arc::new(AtomicBool::new(false));let check=saved.clone();
        let options=eframe::NativeOptions{viewport:egui::ViewportBuilder::default().with_inner_size([940.,580.]),vsync:false,
            event_loop_builder:Some(Box::new(|builder|{use winit::platform::x11::EventLoopBuilderExtX11;builder.with_any_thread(true);})),..Default::default()};
        eframe::run_native("Generated stream menus",options,Box::new(move|cc|{
            let mut app=super::super::App::new(cc,None,None,false,None);
            app.stream_target=Some(super::super::sessions::StreamTarget{connection:super::super::sessions::ConnectionTarget::NativeInvitation("generated-unused-invitation".into()),alias:"Generated preview".into(),view_only:true});
            app.preferences.codec=super::super::settings::CodecPreference::Auto;app.preferences.hdr=true;app.target_fps=165;
            let stats=Arc::new(uu_core::media::MediaStats::default());*stats.codec.lock().unwrap()="video/AV1".into();
            stats.audio.volume.store(72,Ordering::Relaxed);*stats.audio.status.lock().unwrap()="Opus".into();
            app.controller=Some(uu_core::controller::ControllerHandle {stopped:Arc::new(AtomicBool::new(true)),events:std::sync::mpsc::channel().1,
                frames:uu_core::media::frame_channel().1,input:tokio::sync::mpsc::unbounded_channel().0,native_input:None,
                settings:tokio::sync::mpsc::unbounded_channel().0,close:tokio::sync::mpsc::unbounded_channel().0,stats});
            Ok(Box::new(Preview{app,output,frames:0,saved}))
        })).unwrap();
        assert!(check.load(Ordering::Acquire));
    }
    #[test]
    fn more_has_direct_hud_toggle_and_uses_the_effective_binding() {
        for custom in [false,true] {
            let ctx=egui::Context::default();let mut enabled=false;let mut bindings=uu_core::hotkeys::Bindings::default();
            if custom {bindings.set(uu_core::hotkeys::Action::Stats,vec![uu_core::hotkeys::Chord{key:0x78,modifiers:Default::default()}]);}
            let hint=bindings.label(uu_core::hotkeys::Action::Stats);let mut time=0.;
            let mut frame=|events:Vec<egui::Event>| {
                time+=0.05;let mut blocked=Vec::new();let mut open=false;
                let output=ctx.run(egui::RawInput{time:Some(time),screen_rect:Some(Rect::from_min_size(Pos2::ZERO,egui::vec2(1280.,720.))),events,focused:true,..Default::default()},|ctx| {
                    egui::CentralPanel::default().show(ctx,|ui| {
                        open=side_submenu(ui,&mut blocked,Icon::Grid,"更多",|ui|hud_toggle(ui,&mut enabled,&bindings)).inner.is_some();
                    });
                });
                let text:std::collections::HashMap<_,_>=output.shapes.iter().filter_map(|s|if let egui::Shape::Text(t)=&s.shape {Some((t.galley.text().to_owned(),Rect::from_min_size(t.pos,t.galley.size())))}else{None}).collect();
                (open,blocked,text,enabled)
            };
            frame(vec![]);let (_,_,text,_)=frame(vec![]);let more=text["更多"].center();
            frame(vec![egui::Event::PointerMoved(more)]);let mut last=frame(vec![]);for _ in 0..3{last=frame(vec![]);}
            let (open,blocked,text,_)=last;assert!(open);assert!(text.contains_key(&hint),"effective hint must be visible: {hint} {text:?}");
            assert!(!text.contains_key("统计信息"));assert!(!text.contains_key("详细"));
            let toggle=text["启用 HUD"].center();assert!(blocked.iter().any(|r|r.contains(toggle)));
            frame(vec![egui::Event::PointerMoved(toggle)]);for _ in 0..3{frame(vec![]);}
            frame(vec![button(toggle,true)]);let (open,_,_,enabled)=frame(vec![button(toggle,false)]);assert!(open&&enabled);
        }
    }
    struct HandleUi {
        context: egui::Context,
        handle: FloatingHandle,
        open: bool,
        screen: Rect,
        focused: bool,
    }
    impl HandleUi {
        fn new() -> Self {
            let mut ui=Self {context:Default::default(),handle:Default::default(),open:false,
                screen:Rect::from_min_size(Pos2::ZERO,egui::vec2(1280.0,720.0)),focused:true};
            ui.frame(0.0,vec![]); // Complete egui Area's first sizing pass before input.
            ui
        }
        fn frame(&mut self,time:f64,events:Vec<egui::Event>)->(Rect,bool) {
            let mut output=(Rect::NOTHING,false);
            let input=egui::RawInput{time:Some(time),screen_rect:Some(self.screen),events,focused:self.focused,..Default::default()};
            let _=self.context.run(input,|ctx|{
                output=self.handle.show(ctx,&mut self.open);
                let menu=Rect::from_min_max(egui::pos2(1030.0,142.0),egui::pos2(1272.0,500.0));
                let mut regions=vec![output.0,menu];
                if let Some(bridge)=hover_bridge(output.0,menu) {regions.push(bridge);}
                self.handle.finish_frame(ctx,&mut self.open,&regions);
            });
            output
        }
        fn click(&mut self,time:f64,point:Pos2) {
            self.frame(time,vec![egui::Event::PointerMoved(point),button(point,true)]);
            self.frame(time+0.01,vec![button(point,false)]);
        }
    }
    fn button(pos:Pos2,pressed:bool)->egui::Event {
        egui::Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed,modifiers:Default::default()}
    }
    #[test]
    fn floating_handle_starts_right_at_fifteen_percent_and_hover_opens_without_click() {
        let mut ui=HandleUi::new();
        let initial=ui.frame(0.0,vec![]).0;
        assert_eq!(initial.right(),1280.0);
        assert_eq!(initial.top(),108.0);
        assert!(initial.area()<=36.0*36.0*0.70);
        ui.screen=Rect::from_min_size(Pos2::ZERO,egui::vec2(1600.0,1000.0));
        let resized=ui.frame(0.1,vec![]).0;
        assert_eq!(resized.top(),150.0,"default anchor follows the final fullscreen viewport");
        assert_eq!(resized.right(),1600.0);
        ui.screen=Rect::from_min_size(Pos2::ZERO,egui::vec2(1280.0,720.0));
        ui.frame(0.2,vec![]);
        assert!(!ui.open);
        assert_eq!(initial.width(),HANDLE_SIZE/3.0,"the very first frame is already docked");
        assert_eq!(ui.frame(1.9,vec![]).0.width(),HANDLE_SIZE/3.0);
        let folded=ui.frame(2.1,vec![]).0;
        assert_eq!(folded.width(),HANDLE_SIZE/3.0);
        ui.frame(2.2,vec![egui::Event::PointerMoved(folded.center())]);
        assert!(ui.open);
        assert!(ui.handle.docked.is_none());
        ui.frame(2.3,vec![egui::Event::PointerMoved(egui::pos2(1260.0,140.0))]);
        assert!(ui.open,"moving across the handle/menu gap must retain ownership");
        ui.frame(2.4,vec![egui::Event::PointerMoved(egui::pos2(1100.0,250.0))]);
        assert!(ui.open);
        ui.frame(2.5,vec![egui::Event::PointerMoved(egui::pos2(500.0,500.0))]);
        assert!(!ui.open,"hover-only menus close on the first frame outside");
        assert_eq!(ui.handle.docked,Some(Edge::Right));
    }
    #[test]
    fn clicking_keeps_menu_until_two_seconds_after_leaving_or_losing_focus() {
        for window_blur in [false,true] {
            let mut ui=HandleUi::new();
            let handle=ui.frame(0.0,vec![]).0;
            ui.click(0.1,handle.center());
            assert!(ui.open && ui.handle.pinned);
            assert!(ui.handle.owns_input(ui.open));
            if window_blur {ui.focused=false;}
            let events=if window_blur {vec![]}else{vec![egui::Event::PointerMoved(egui::pos2(500.0,500.0))]};
            ui.frame(1.0,events);
            assert!(ui.handle.blur_since.is_some(),"focus leaves before the visual dismissal timer");
            assert!(!ui.handle.owns_input(ui.open),"visual dismissal delay must not hold remote input");
            ui.frame(2.99,vec![]);
            assert!(ui.open);
            ui.frame(3.01,vec![]);
            assert!(!ui.open);
            assert_eq!(ui.handle.docked,Some(Edge::Right));
        }
    }
    #[test]
    fn dragging_handle_does_not_activate_menu_and_redocks_on_every_edge() {
        for (edge,position) in [(Edge::Left,egui::pos2(4.0,300.0)),
            (Edge::Right,egui::pos2(1246.0,300.0)),(Edge::Top,egui::pos2(500.0,4.0)),
            (Edge::Bottom,egui::pos2(500.0,686.0))] {
            let mut ui=HandleUi::new();
            let initial=ui.frame(0.0,vec![]).0.center();
            ui.frame(0.1,vec![egui::Event::PointerMoved(initial)]);
            ui.frame(0.2,vec![egui::Event::PointerMoved(initial),button(initial,true)]);
            let center=position+egui::Vec2::splat(HANDLE_SIZE/2.0);
            assert!(ui.frame(0.3,vec![egui::Event::PointerMoved(center)]).1);
            ui.frame(0.4,vec![button(center,false)]);
            assert!(!ui.open);
            ui.frame(0.5,vec![egui::Event::PointerMoved(egui::pos2(800.0,500.0))]);
            ui.frame(2.3,vec![]);
            assert!(ui.handle.docked.is_none());
            let folded=ui.frame(2.6,vec![]).0;
            assert_eq!(ui.handle.docked,Some(edge));
            assert!((folded.area()-HANDLE_SIZE*HANDLE_SIZE/3.0).abs()<1.0);
            ui.screen=Rect::from_min_size(Pos2::ZERO,egui::vec2(800.0,500.0));
            let resized=ui.frame(3.7,vec![]).0;
            assert!(ui.screen.contains_rect(resized));
        }
    }
    #[test]
    fn reentering_menu_resets_click_timeout_and_escape_does_not_immediately_reopen() {
        let mut ui=HandleUi::new();
        let handle=ui.frame(0.0,vec![]).0;
        ui.click(0.1,handle.right_center()-egui::vec2(2.0,0.0));
        ui.frame(1.0,vec![egui::Event::PointerMoved(egui::pos2(500.0,500.0))]);
        ui.frame(2.8,vec![egui::Event::PointerMoved(handle.right_center()-egui::vec2(2.0,0.0))]);
        ui.frame(4.0,vec![]);
        assert!(ui.open);
        ui.frame(4.1,vec![egui::Event::Key{key:egui::Key::Escape,physical_key:None,pressed:true,repeat:false,modifiers:Default::default()}]);
        assert!(!ui.open);
        ui.frame(4.2,vec![]);
        assert!(!ui.open,"Escape must suppress hover until the pointer leaves");
        ui.frame(4.3,vec![egui::Event::PointerMoved(egui::pos2(500.0,500.0))]);
        ui.frame(4.4,vec![egui::Event::PointerMoved(handle.right_center()-egui::vec2(2.0,0.0))]);
        assert!(ui.open);
    }
    #[test]
    fn outside_press_dismisses_clicked_menu_without_waiting_for_timeout() {
        let mut ui=HandleUi::new();
        let handle=ui.frame(0.0,vec![]).0;
        ui.click(0.1,handle.center());
        let outside=egui::pos2(500.0,500.0);
        ui.frame(0.3,vec![egui::Event::PointerMoved(outside),button(outside,true)]);
        assert!(!ui.open);
        assert!(!ui.handle.owns_input(ui.open));
        assert_eq!(ui.handle.docked,Some(Edge::Right));
    }
    #[test]
    fn entering_fullscreen_under_pointer_stays_folded_until_pointer_reenters() {
        let mut ui=HandleUi {context:Default::default(),handle:Default::default(),open:false,
            screen:Rect::from_min_size(Pos2::ZERO,egui::vec2(1280.0,720.0)),focused:true};
        let point=egui::pos2(1275.0,120.0);
        ui.frame(0.0,vec![egui::Event::PointerMoved(point)]);
        ui.frame(0.1,vec![]);
        assert!(!ui.open);
        ui.frame(0.2,vec![egui::Event::PointerMoved(egui::pos2(500.0,500.0))]);
        ui.frame(0.3,vec![egui::Event::PointerMoved(point)]);
        assert!(ui.open);
    }
    #[test]
    fn child_menu_retains_hover_path_but_closes_on_sibling_and_outside() {
        let ctx=egui::Context::default();
        let row=Rect::from_min_max(egui::pos2(700.0,100.0),egui::pos2(920.0,128.0));
        let panel=Rect::from_min_max(egui::pos2(450.0,96.0),egui::pos2(690.0,400.0));
        let regions=[row,panel,hover_bridge(row,panel).unwrap()];
        for (point,retained) in [(row.center(),true),(panel.center(),true),
            (egui::pos2(695.0,115.0),true),(egui::pos2(800.0,150.0),false),
            (egui::pos2(300.0,500.0),false)] {
            let _=ctx.run(egui::RawInput {events:vec![egui::Event::PointerMoved(point)],..Default::default()},|ctx|{
                assert_eq!(retain_children(ctx,&regions),retained);
            });
        }
    }
    #[test]
    fn submenu_bridge_covers_only_the_connecting_gap() {
        let row=Rect::from_min_max(egui::pos2(700.0,100.0),egui::pos2(920.0,128.0));
        let child=Rect::from_min_max(egui::pos2(450.0,96.0),egui::pos2(690.0,400.0));
        let gap=hover_bridge(row,child).unwrap();
        assert!(gap.contains(egui::pos2(695.0,115.0)));
        assert!(gap.contains(egui::pos2(695.0,200.0)),"diagonal movement into a child remains inside the narrow gap");
        assert!(!gap.contains(egui::pos2(650.0,200.0)));
        assert!(!gap.contains(egui::pos2(695.0,450.0)));
    }
}
