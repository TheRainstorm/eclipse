//! Connection chrome and the optional floating control menu.
use super::ui::{icon, icon_button, icon_label_button, Icon};
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Stroke};

const MENU_INK: Color32 = Color32::from_rgb(29, 32, 37);
const MENU_WIDTH: f32 = 236.0;

const HANDLE_SIZE: f32 = 54.0;
const DOCK_DISTANCE: f32 = 24.0;
const DOCK_DELAY: f64 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Edge { Right, Left, Top, Bottom }

/// Position always describes the complete button inside the viewport. Folding
/// changes only its painted/interactive rectangle, so restoring never loses it.
#[derive(Default)]
pub(super) struct FloatingHandle {
    position: Option<Pos2>,
    docked: Option<Edge>,
    idle_since: f64,
}
impl FloatingHandle {
    fn size(screen: Rect) -> egui::Vec2 {
        egui::vec2(HANDLE_SIZE.min(screen.width()), HANDLE_SIZE.min(screen.height()))
    }
    fn place(&mut self, screen: Rect) {
        let size = Self::size(screen);
        let position = self.position.get_or_insert(egui::pos2(screen.right()-size.x-8.0, screen.top()+8.0));
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
    fn show(&mut self, ctx: &egui::Context, open: &mut bool) -> (Rect,bool) {
        let screen=ctx.screen_rect();
        let now=ctx.input(|i|i.time);
        if self.position.is_none() {self.idle_since=now;}
        self.place(screen);
        let (_,hit)=self.rectangles(screen);
        let busy=*open || ctx.input(|i|i.pointer.any_down() || i.pointer.hover_pos().is_some_and(|p|hit.contains(p)));
        if busy || self.nearest_edge(screen).is_none() {self.idle_since=now;}
        if self.docked.is_none() && !busy && self.nearest_edge(screen).is_some() {
            let remaining=DOCK_DELAY-(now-self.idle_since);
            if remaining<=0.0 {self.docked=self.nearest_edge(screen); self.place(screen);}
            else {ctx.request_repaint_after(std::time::Duration::from_secs_f64(remaining));}
        }
        let (full,hit)=self.rectangles(screen);
        let response=egui::Area::new("floating-control-handle".into())
            .order(egui::Order::Foreground).constrain(false).movable(false).fixed_pos(hit.min)
            .show(ctx, |ui| {
                let (rect,response)=ui.allocate_exact_size(hit.size(),egui::Sense::click_and_drag());
                // Area can round its origin at fractional DPI. Paint at the
                // actual hit rectangle, with the hidden portion still clipped.
                let full=full.translate(rect.min-hit.min);
                let painter=ui.painter().with_clip_rect(screen);
                painter.rect(full.shrink(1.0),17.0,Color32::from_rgb(235,237,240),Stroke::new(2.0,Color32::from_rgb(170,176,184)));
                painter.rect(full.shrink(4.0),14.0,Color32::WHITE,Stroke::new(1.5,
                    if response.hovered() || *open {Color32::from_rgb(162,191,230)} else {Color32::from_rgb(205,212,222)}));
                if self.docked.is_none() {
                    for y in [-5.0,5.0] {
                        let pill=Rect::from_center_size(full.center()+egui::vec2(0.0,y),egui::vec2(17.0,6.0));
                        painter.rect_stroke(pill,3.0,Stroke::new(1.4,MENU_INK));
                        painter.circle_filled(pill.center()+egui::vec2(if y<0.0 {-4.5}else{4.5},0.0),1.4,MENU_INK);
                    }
                }
                response.on_hover_text("拖动调整位置 · 靠边停留 3 秒自动收起 · 点击恢复按钮 / 展开菜单")
            }).inner;
        if response.dragged() {
            let folded=self.docked.take().is_some();
            if folded {
                if let Some(pointer)=response.interact_pointer_pos() {self.position=Some(pointer-Self::size(screen)/2.0);}
            } else {self.position=Some(self.position.unwrap()+response.drag_delta());}
            self.place(screen);
            self.idle_since=now;
            *open=false;
            ctx.request_repaint();
        } else if response.clicked() {
            if self.docked.take().is_none() {*open=!*open;}
            self.idle_since=now;
            ctx.request_repaint();
        }
        if response.drag_stopped() {self.idle_since=now;}
        (response.rect,response.dragged() || response.drag_stopped())
    }
}

/// Every menu entry dispatches the same action as Ctrl+Alt+F/X.
fn fullscreen_menu_action(ui:&mut egui::Ui,fullscreen:bool)->Option<super::shortcuts::Action> {
    menu_row(ui,if fullscreen {"退出全屏"} else {"进入全屏"},Some(Icon::Max),
        false,false,"Ctrl+Alt+F / X",false).clicked().then_some(super::shortcuts::Action::Fullscreen)
}

fn popup_frame(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::popup(ui.style())
        .fill(Color32::from_rgb(249, 249, 249))
        .stroke(Stroke::new(1.0, Color32::from_rgb(215, 215, 215)))
        .rounding(7.0)
        .inner_margin(egui::Margin::same(4.0))
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
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), egui::Sense::click());
    let ink = if !ui.is_enabled() {
        Color32::from_gray(160)
    } else if label == "退出串流" {
        Color32::from_rgb(187, 65, 55)
    } else {
        MENU_INK
    };
    if ui.is_enabled() && (response.hovered() || active) {
        ui.painter()
            .rect_filled(rect, 3.0, Color32::from_rgb(233, 233, 233));
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
    } else if selected {
        ui.painter()
            .circle_filled(egui::pos2(rect.left() + 16.0, rect.center().y), 1.8, ink);
    }
    ui.painter().text(
        egui::pos2(rect.left() + 36.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(14.0),
        ink,
    );
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
    } else if !hint.is_empty() {
        ui.painter().text(
            egui::pos2(rect.right() - 12.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            hint,
            egui::FontId::proportional(12.0),
            Color32::from_gray(145),
        );
    }
    response
}
fn separator(ui: &mut egui::Ui) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 9.0), egui::Sense::hover());
    ui.painter().line_segment(
        [rect.left_center(), rect.right_center()],
        Stroke::new(1.0, Color32::from_gray(233)),
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
    let id = ui.make_persistent_id(("side-menu", &label));
    let open = ui.memory(|m| m.is_popup_open(id));
    let response = menu_row(ui, &label, Some(kind), false, true, "", open);
    if response.clicked() {
        ui.memory_mut(|m| m.toggle_popup(id));
    } else if response.hovered() && ui.memory(|m| m.any_popup_open()) {
        ui.memory_mut(|m| m.open_popup(id));
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
        blocked.push(area.response.rect);
    }
    egui::InnerResponse::new(inner, response)
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

impl super::App {
    pub(super) fn connection_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("connection-top")
            .exact_height(super::window::CONNECTION_BAR_HEIGHT)
            .frame(
                egui::Frame::none()
                    .fill(Color32::from_rgb(218, 223, 229))
                    .inner_margin(egui::Margin::symmetric(10.0, 2.0)),
            )
            .show(ctx, |ui| {
                if self.mouse_captured {ui.disable();}
                ui.horizontal_centered(|ui| {
                    let (brand,drag)=ui.allocate_exact_size(egui::vec2(22.0,26.0),egui::Sense::drag());
                    super::ui::logo(ui.painter(),brand);
                    if drag.drag_started(){ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);}
                    let title = self
                        .selected
                        .and_then(|i| self.devices.get(i))
                        .map(|d| d.alias.as_str())
                        .unwrap_or("远程桌面");
                    let roomy=ui.available_width()>740.0;
                    let wide=ui.available_width()>1100.0;
                    if ui.available_width()>520.0 {ui.add_sized([76.0,24.0],egui::Label::new(RichText::new(title).size(12.0)).truncate());}
                    ui.separator();
                    if self.supports_remote(uu_core::capabilities::Capability::Displays){self.display_tabs(ui);}
                    if (self.view_only || (self.is_native()&&!self.supports_remote(uu_core::capabilities::Capability::Input))) && (roomy || self.is_native()) {
                        ui.label(RichText::new("仅观看").color(Color32::from_rgb(98, 125, 160)));
                    }
                    if wide && !self.view_only && self.controller.is_some() && !self.input_ready() && (!self.is_native()||self.native_input_available()) {
                        ui.label(
                            RichText::new(if self.displays.switching(){"正在切换显示器…"}else{"输入连接中…"})
                                .size(12.0)
                                .color(Color32::from_gray(110)),
                        )
                        .on_hover_text("视频和输入分别建立连接，输入就绪后可操作");
                    }
                    if let Some(controller) = self.controller.as_ref().filter(|_|roomy && !self.is_native()) {
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
                        if icon_button(ui, Icon::Close, "退出串流").clicked() {
                            self.disconnect();
                        }
                        if icon_button(ui, Icon::Max, "最大化 / 还原窗口").clicked() {
                            self.toggle_maximized(ctx);
                        }
                        if icon_button(ui, Icon::Min, "最小化").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                        let control = icon_label_button(ui, Icon::Settings, "设置");
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
                            if response.on_hover_text(format!("{}{} · {} × {} · {} Hz\n拖动调整标签顺序；编号为本地标记，重连后保留",label,if screen.primary{" · 主屏"}else{""},screen.width,screen.height,screen.refresh_hz)).clicked(){chosen=Some(screen.id);}
                        }
                        if screens.screens.is_empty(){ui.label("等待显示器…");}
                    });
                });
            let response=ui.add_sized([26.0,28.0],egui::Button::new(RichText::new("+").size(20.0)).frame(false)).on_hover_text("选择远端显示器");
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
                self.displays.selected==Some(screen.id),false,if screen.primary {"主屏"}else{""},false)
                .on_hover_text(format!("{} · {} × {} · {} Hz · {}",screen.display_name,screen.width,screen.height,screen.refresh_hz,screen.kind_label()));
            if response.clicked(){self.select_remote_screen(screen.id);ui.close_menu();}
        }
        if screens.screens.is_empty(){ui.add_enabled(false,egui::Label::new("等待远端显示器列表…"));}
        if !self.displays.notice.is_empty(){separator(ui);ui.label(&self.displays.notice);}
        separator(ui);
        if ui.button("显示器模式设置…").clicked(){self.displays.menu_open=false;self.open_settings();}
    }

    fn confirm_quality(&mut self, ui: &mut egui::Ui) {
        self.persist_preferences();
        self.send_capture_settings();
        self.stream_menu.quality_detail = None;
        ui.memory_mut(|m| m.close_popup());
    }

    fn quality_menu(&mut self, ui: &mut egui::Ui) {
        for (index, (label, _, _)) in super::BITRATE_PRESETS.iter().enumerate() {
            let custom = index == 4;
            let response = menu_row(
                ui,
                label,
                None,
                self.bitrate_idx == index,
                custom,
                "",
                false,
            );
            if custom && (response.hovered() || response.clicked()) {
                self.stream_menu.quality_detail = Some((QualityDetail::Bitrate, response.rect));
            } else if !custom && response.hovered() {
                self.stream_menu.quality_detail = None;
            }
            if !custom && response.clicked() {
                self.bitrate_idx = index;
                self.confirm_quality(ui);
            }
        }
        separator(ui);
        for fps in [144, 120, 90, 60, 30] {
            let response = menu_row(
                ui,
                &format!("{fps} 帧"),
                None,
                self.target_fps == fps,
                false,
                "",
                false,
            );
            if response.hovered() {
                self.stream_menu.quality_detail = None;
            }
            if response.clicked() {
                self.target_fps = fps;
                self.fps_draft = fps;
                self.confirm_quality(ui);
            }
        }
        let custom_fps = ![30, 60, 90, 120, 144].contains(&self.target_fps);
        let label = if custom_fps {
            format!("自定义 · {} 帧", self.target_fps)
        } else {
            "自定义帧率".into()
        };
        let response = menu_row(ui, &label, None, custom_fps, true, "", false);
        if response.hovered() || response.clicked() {
            self.stream_menu.quality_detail = Some((QualityDetail::FrameRate, response.rect));
        }
        separator(ui);
        let response = menu_row(ui, "分辨率", None, false, true, "", false);
        if response.hovered() || response.clicked() {
            self.stream_menu.quality_detail = Some((QualityDetail::Resolution, response.rect));
        }
    }

    fn quality_detail(&mut self, ctx: &egui::Context, blocked: &mut Vec<Rect>) {
        let Some((kind, row)) = self.stream_menu.quality_detail else {
            return;
        };
        let screen = ctx.screen_rect().shrink(4.0);
        let mut position = child_position(row, screen, 204.0);
        if ctx.memory(|m| m.area_rect(egui::Id::new("stream-control-menu")))
            .is_some_and(|root| root.center().x > row.center().x)
            && row.left() - screen.left() >= 217.0
        {
            position.x = row.left() - 217.0;
        }
        let response = egui::Area::new("quality-detail".into())
            .order(egui::Order::Foreground)
            .fixed_pos(position)
            .constrain_to(screen)
            .show(ctx, |ui| {
                popup_frame(ui).show(ui, |ui| {
                    ui.set_width(204.0);
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                    ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
                        match kind {
                            QualityDetail::Bitrate => {
                                ui.add_space(5.0);
                                ui.horizontal(|ui| {
                                    ui.label("码率");
                                    ui.add(
                                        egui::DragValue::new(&mut self.custom_mbps)
                                            .range(1..=100)
                                            .suffix(" Mbps"),
                                    );
                                });
                                ui.add_space(5.0);
                                separator(ui);
                                if menu_row(ui, "确认", None, false, false, "", false).clicked() {
                                    self.bitrate_idx = 4;
                                    self.confirm_quality(ui);
                                }
                            }
                            QualityDetail::FrameRate => {
                                ui.add_space(5.0);
                                ui.horizontal(|ui| {
                                    ui.label("帧率");
                                    ui.add(
                                        egui::DragValue::new(&mut self.fps_draft)
                                            .range(5..=144)
                                            .suffix(" FPS"),
                                    );
                                });
                                ui.add_space(5.0);
                                separator(ui);
                                if menu_row(ui, "确认", None, false, false, "", false).clicked() {
                                    self.target_fps = self.fps_draft.clamp(5, 144);
                                    self.confirm_quality(ui);
                                }
                            }
                            QualityDetail::Resolution => {
                                for (index, label) in [
                                    "保持远端桌面分辨率",
                                    "输出 1920 × 1080",
                                    "输出 2560 × 1440",
                                    "输出 3840 × 2160",
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
                                ui.small("输出档位限制串流编码尺寸，保持宽高比；切换会短暂重连，被控端桌面分辨率不变。");
                            }
                        }
                    });
                });
            });
        blocked.push(response.response.rect);
    }

    /// Draw first so the stream can exclude every control rectangle from input.
    pub(super) fn stream_controls(&mut self, ctx: &egui::Context) -> Vec<Rect> {
        let mut blocked = Vec::new();
        if self.mouse_captured {return blocked;}
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
            if !self.local_dialog_open() {ctx.memory_mut(|m| m.close_popup());}
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
                                    if self.supports_remote(uu_core::capabilities::Capability::Displays) {
                                    side_submenu(
                                        ui,
                                        &mut blocked,
                                        Icon::Desktop,
                                        self.display_label(),
                                        |ui| self.display_menu(ui),
                                    );
                                    }
                                    if self.supports_remote(uu_core::capabilities::Capability::Quality) {
                                    quality_open = side_submenu(
                                        ui,
                                        &mut blocked,
                                        Icon::Picture,
                                        "画质",
                                        |ui| self.quality_menu(ui),
                                    )
                                    .inner
                                    .is_some();
                                    }
                                    side_submenu(ui, &mut blocked, Icon::Max, "窗口", |ui| {
                                        if let Some(action)=fullscreen_menu_action(ui,self.fullscreen) {
                                            self.local_shortcut(action,ctx);
                                        }
                                        separator(ui);
                                        ui.checkbox(
                                            &mut self.stream_menu.visible,
                                            "全屏显示悬浮入口",
                                        );
                                    });
                                    if self.supports_remote(uu_core::capabilities::Capability::Audio) {
                                    side_submenu(ui, &mut blocked, Icon::Play, "声音", |ui| {
                                        let mut changed=false;
                                        if let Some(h) = &self.controller {
                                            use std::sync::atomic::Ordering::Relaxed;
                                            let audio = &h.stats.audio;
                                            let mut muted = audio.muted.load(Relaxed);
                                            if ui.checkbox(&mut muted, "静音").changed() { audio.muted.store(muted,Relaxed);self.preferences.muted=muted;changed=true; }
                                            let mut volume=audio.volume.load(Relaxed);
                                            if ui.add(egui::Slider::new(&mut volume,0..=100).text("音量")).changed() {audio.volume.store(volume,Relaxed);self.preferences.volume=volume;changed=true;}
                                            ui.label(audio.status.lock().unwrap().as_str());
                                            ui.label(format!("播放排队 {} ms · 接收 {} 包",audio.queue_ms.load(Relaxed),audio.packets.load(Relaxed)));
                                            ui.small(format!("音频包 {} ms · 接收→解码 {:.1} ms",audio.packet_ms.load(Relaxed),audio.receive_decode_us.load(Relaxed) as f32/1000.0));
                                            ui.small(format!("设备回调块 {:.1} ms · 欠载 {} 次",audio.output_block_us.load(Relaxed) as f32/1000.0,audio.underruns.load(Relaxed)));
                                            ui.small("这些是客户端阶段数据，不是端到端声音延迟。");
                                        }
                                        if changed{self.audio_save_after=Some(std::time::Instant::now()+std::time::Duration::from_millis(250));}
                                    });
                                    }
                                    if self.supports_remote(uu_core::capabilities::Capability::Input) {
                                    side_submenu(ui, &mut blocked, Icon::Desktop, "鼠标", |ui| {
                                        let old=(self.preferences.relative_mouse,self.preferences.cursor_mode);
                                        ui.selectable_value(&mut self.preferences.relative_mouse,false,"桌面模式 · 绝对定位");
                                        ui.selectable_value(&mut self.preferences.relative_mouse,true,"游戏模式 · 相对移动");
                                        if !self.is_native() {
                                        separator(ui);
                                        for mode in super::settings::CursorMode::ALL {
                                            ui.selectable_value(&mut self.preferences.cursor_mode,mode,mode.label());
                                        }
                                        ui.small("Ctrl+Alt+C 切换光标显示");
                                        ui.small(&self.settings_feedback);
                                        }
                                        ui.small("Ctrl+Alt+P 切换桌面／游戏模式");
                                        ui.small("Ctrl+Alt+Z 释放／捕获键鼠");
                                        if old!=(self.preferences.relative_mouse,self.preferences.cursor_mode) {
                                            self.release_pointer_capture(ctx);self.persist_preferences();
                                            if old.1.remote()!=self.preferences.cursor_mode.remote() {self.send_capture_settings();}ctx.memory_mut(|m|m.close_popup());
                                        }
                                    });
                                    }
                                    if self.supports_remote(uu_core::capabilities::Capability::Input) || self.native_input_available() {
                                    side_submenu(ui, &mut blocked, Icon::Person, "安全", |ui| {
                                        if self.native_input_available()&&!self.supports_remote(uu_core::capabilities::Capability::Input)
                                            &&ui.button("请求键鼠控制").clicked() {
                                            self.view_only=false;
                                            if let Some(sender)=self.controller.as_ref().and_then(|h|h.native_input.as_ref()){sender.set_enabled(true);}
                                            ui.close_menu();
                                        }
                                        if ui
                                            .checkbox(&mut self.view_only, "仅观看，禁止远程输入")
                                            .changed()
                                        {
                                            self.release_all_keys();
                                            for message in self
                                                .pointer
                                                .release_all(self.input_screen(self.selected_track))
                                            {
                                                self.send_input(message, 15);
                                            }
                                            if let Some(sender)=self.controller.as_ref().and_then(|h|h.native_input.as_ref()){sender.set_enabled(!self.view_only);}
                                        }
                                    });
                                    }
                                    if self.supports_remote(uu_core::capabilities::Capability::Clipboard) {
                                    side_submenu(ui,&mut blocked,Icon::Folder,"剪贴板",|ui| {
                                        if self.is_native(){ui.checkbox(&mut self.native_clipboard_enabled,"本次同步文本剪贴板");}
                                        else if ui.checkbox(&mut self.preferences.clipboard,"同步文本剪贴板").changed(){self.persist_preferences();}
                                        ui.small("仅同步当前前台串流，支持最多30 KiB文本；图片和文件不传输。");
                                        if let Some(h)=&self.controller {ui.label(h.stats.clipboard.status.lock().unwrap().as_str());}
                                    });
                                    }
                                    if !self.is_native() {
                                    unavailable(ui, Icon::Link, "外设", "外设透传尚未接入");
                                    unavailable(
                                        ui,
                                        Icon::Folder,
                                        "工具",
                                        "文件、终端和端口映射尚未接入",
                                    );
                                    }
                                    if menu_row(ui,"软件设置",Some(Icon::Settings),false,false,"",false).clicked(){self.open_settings();}
                                    if menu_row(ui,"关于软件",Some(Icon::Info),false,false,"",false).clicked(){self.open_about();}
                                    side_submenu(ui, &mut blocked, Icon::Grid, "更多", |ui| {
                                        if menu_row(
                                            ui,
                                            "串流统计",
                                            None,
                                            self.hud,
                                            false,
                                            "Ctrl+Alt+S",
                                            false,
                                        )
                                        .clicked()
                                        {
                                            self.hud = !self.hud;self.persist_preferences();
                                        }
                                        if menu_row(
                                            ui,
                                            "隐藏悬浮入口",
                                            None,
                                            false,
                                            false,
                                            "Ctrl+Alt+H",
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
                                    if let Some(action)=fullscreen_menu_action(ui,self.fullscreen) {
                                        self.local_shortcut(action,ctx);
                                    }
                                    ui.scope(|ui| {
                                        if menu_row(
                                            ui,
                                            "退出串流",
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
            self.quality_detail(ctx, &mut blocked);
        } else {
            self.stream_menu.quality_detail = None;
        }
        blocked.push(menu.response.rect);
        let outside_click = ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| !blocked.iter().any(|r| r.contains(p)))
        });
        if outside_click || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.stream_menu.open = false;
            ctx.memory_mut(|m| m.close_popup());
        }
        if !self.stream_menu.open {
            ctx.memory_mut(|m| m.close_popup());
        }
        blocked
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct HandleUi {
        context: egui::Context,
        handle: FloatingHandle,
        open: bool,
        screen: Rect,
    }
    impl HandleUi {
        fn new() -> Self {Self {context:Default::default(),handle:Default::default(),open:false,
            screen:Rect::from_min_size(Pos2::ZERO,egui::vec2(1280.0,720.0))}}
        fn frame(&mut self,time:f64,events:Vec<egui::Event>)->(Rect,bool) {
            let mut output=(Rect::NOTHING,false);
            let input=egui::RawInput{time:Some(time),screen_rect:Some(self.screen),events,..Default::default()};
            let _=self.context.run(input,|ctx|output=self.handle.show(ctx,&mut self.open));
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
    fn floating_handle_waits_then_folds_and_requires_two_clicks_to_open_menu() {
        let mut ui=HandleUi::new();
        ui.frame(0.0,vec![]);
        assert_eq!(ui.frame(2.9,vec![]).0.width(),HANDLE_SIZE);
        let folded=ui.frame(3.1,vec![]).0;
        assert_eq!(folded.right(),1280.0);
        assert_eq!(folded.width(),HANDLE_SIZE/3.0);
        ui.frame(4.0,vec![egui::Event::PointerMoved(folded.center())]);
        assert!(ui.handle.docked.is_some());
        assert!(!ui.open);
        ui.click(4.1,folded.center());
        assert!(ui.handle.docked.is_none());
        assert!(!ui.open,"first click only restores the complete button");
        let full=ui.frame(4.2,vec![]).0;
        assert_eq!(full.width(),HANDLE_SIZE);
        ui.click(4.3,full.center());
        assert!(ui.open);
        ui.frame(10.0,vec![egui::Event::PointerMoved(egui::pos2(400.0,400.0))]);
        assert!(ui.handle.docked.is_none(),"an open menu must not fold");
        ui.click(10.1,full.center());
        assert!(!ui.open);
        ui.frame(10.2,vec![egui::Event::PointerMoved(egui::pos2(400.0,400.0))]);
        assert!(ui.handle.docked.is_none());
        ui.frame(13.3,vec![]);
        assert!(ui.handle.docked.is_some(),"restored button can fold again");
    }
    #[test]
    fn dragging_handle_moves_it_without_opening_menu_and_can_redock_on_every_edge() {
        for (edge,position) in [(Edge::Left,egui::pos2(4.0,300.0)),
            (Edge::Right,egui::pos2(1222.0,300.0)),(Edge::Top,egui::pos2(500.0,4.0)),
            (Edge::Bottom,egui::pos2(500.0,662.0))] {
            let mut ui=HandleUi::new();
            ui.frame(0.0,vec![]);
            let initial=ui.frame(0.1,vec![]).0.center();
            ui.frame(0.2,vec![egui::Event::PointerMoved(initial),button(initial,true)]);
            let center=position+egui::Vec2::splat(HANDLE_SIZE/2.0);
            assert!(ui.frame(0.3,vec![egui::Event::PointerMoved(center)]).1);
            assert!(ui.frame(0.4,vec![button(center,false)]).1);
            assert!(!ui.open,"dragging must not count as a click");
            ui.frame(0.5,vec![egui::Event::PointerMoved(egui::pos2(800.0,500.0))]);
            assert!(ui.handle.docked.is_none());
            let folded=ui.frame(3.6,vec![]).0;
            assert_eq!(ui.handle.docked,Some(edge));
            assert!((folded.area()-HANDLE_SIZE*HANDLE_SIZE/3.0).abs()<1.0);
            ui.screen=Rect::from_min_size(Pos2::ZERO,egui::vec2(800.0,500.0));
            let resized=ui.frame(3.7,vec![]).0;
            assert!(ui.screen.contains_rect(resized));
            ui.click(3.8,resized.center());
            let restored=ui.frame(3.9,vec![]).0;
            assert!(ui.screen.contains_rect(restored));
        }
    }
    #[test]
    fn floating_handle_stays_expanded_in_center_and_while_hovered_near_edge() {
        let mut ui=HandleUi::new();
        ui.handle.position=Some(egui::pos2(500.0,300.0));
        ui.frame(0.0,vec![]);
        ui.frame(30.0,vec![]);
        assert!(ui.handle.docked.is_none());
        ui.handle.position=Some(egui::pos2(500.0,4.0));
        ui.frame(31.0,vec![egui::Event::PointerMoved(egui::pos2(525.0,25.0))]);
        ui.frame(60.0,vec![]);
        assert!(ui.handle.docked.is_none());
        ui.frame(60.1,vec![egui::Event::PointerMoved(egui::pos2(400.0,400.0))]);
        ui.frame(63.2,vec![]);
        assert_eq!(ui.handle.docked,Some(Edge::Top));
    }
    #[test]
    fn folded_handle_can_be_dragged_directly_and_initial_delay_starts_when_shown() {
        let mut ui=HandleUi::new();
        ui.frame(30.0,vec![]);
        assert!(ui.handle.docked.is_none());
        let folded=ui.frame(33.1,vec![]).0;
        assert!(ui.handle.docked.is_some());
        ui.frame(33.2,vec![egui::Event::PointerMoved(folded.center()),button(folded.center(),true)]);
        let destination=egui::pos2(600.0,300.0);
        assert!(ui.frame(33.3,vec![egui::Event::PointerMoved(destination)]).1);
        ui.frame(33.4,vec![button(destination,false)]);
        assert!(ui.handle.docked.is_none());
        assert!(!ui.open);
        assert_eq!(ui.frame(33.5,vec![]).0.center(),destination);
        ui.frame(40.0,vec![egui::Event::PointerMoved(egui::pos2(400.0,400.0))]);
        assert!(ui.handle.docked.is_none());
    }
    #[test]
    fn fullscreen_menu_click_dispatches_stream_fullscreen_for_both_states() {
        for fullscreen in [false,true] {
            let context=egui::Context::default();
            let position=egui::pos2(64.0,24.0);
            let mut actions=Vec::new();
            for events in [vec![],vec![egui::Event::PointerMoved(position),
                egui::Event::PointerButton{pos:position,button:egui::PointerButton::Primary,pressed:true,modifiers:Default::default()}],
                vec![egui::Event::PointerButton{pos:position,button:egui::PointerButton::Primary,pressed:false,modifiers:Default::default()}]] {
                let input=egui::RawInput {screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(236.0,80.0))),events,..Default::default()};
                let _=context.run(input,|ctx| {
                    egui::CentralPanel::default().show(ctx,|ui| {
                        if let Some(action)=fullscreen_menu_action(ui,fullscreen){actions.push(action);}
                    });
                });
            }
            assert_eq!(actions,vec![super::super::shortcuts::Action::Fullscreen]);
        }
    }
}
