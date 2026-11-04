//! Compact connection chrome. Source pills select displays; closing ends only
//! the owning connection. All operations are queued for the workspace owner.
use crate::stream_workspace::{Action, Drag, DragScreen, Group, Tabs};
use eframe::egui::{self, Color32, Rect, Response, Sense, Stroke};

const HEIGHT: f32 = crate::theme::TOOLBAR_HEIGHT + 4.;

/// Hit regions belong to the frame in which they were painted. Native-window
/// tests use the same controls as people, including the actual popup buttons.
#[derive(Clone)]
pub(super) struct StripRects {
    #[cfg(test)]
    pub bounds: Rect,
    #[cfg(test)]
    pub devices: Vec<(u64, Rect)>,
    #[cfg(test)]
    pub groups: Vec<(u64, Rect)>,
    #[cfg(test)]
    pub screens: Vec<(u64, i32, Rect)>,
    #[cfg(test)]
    pub close: Vec<(u64, Rect)>,
    #[cfg(test)]
    pub create: Vec<(u64, Rect)>,
    #[cfg(test)]
    pub blank: Option<Rect>,
    #[cfg(test)]
    pub detach: Vec<(u64, Rect)>,
    #[cfg(test)]
    pub merge: Vec<(u64, Rect)>,
    #[cfg(test)]
    pub disconnect: Vec<(u64, Rect)>,
    #[cfg(test)]
    pub open_display: Vec<(u64, i32, Rect)>,
}
impl Default for StripRects {
    fn default() -> Self {
        Self {
            #[cfg(test)] bounds: Rect::NOTHING,
            #[cfg(test)] devices: vec![],
            #[cfg(test)] groups: vec![],
            #[cfg(test)] screens: vec![],
            #[cfg(test)] close: vec![],
            #[cfg(test)] create: vec![],
            #[cfg(test)] blank: None,
            #[cfg(test)] detach: vec![],
            #[cfg(test)] merge: vec![],
            #[cfg(test)] disconnect: vec![],
            #[cfg(test)] open_display: vec![],
        }
    }
}
macro_rules! record {
    ($rects:ident.$field:ident, $value:expr) => {
        #[cfg(test)] { $rects.$field.push($value); }
    };
}

fn label(ui: &mut egui::Ui, text: &str, selected: bool, device: bool, width_limit: f32) -> Response {
    let color = crate::theme::tab_text_color(ui.visuals(),selected);
    let mut font=if device {crate::theme::device_title_font(ui)}else{egui::TextStyle::Button.resolve(ui.style())};
    if !device {font.size=12.;}
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font.clone(), color);
    let leading = if device { 22. } else { 0. };
    // Give each title its measured width. The scroll viewport, rather than a
    // short fixed cap, limits how much of one long label can be shown at once.
    let width=if device {(galley.size().x+leading+16.).max(70.)}
        else {((galley.size().x+16.).max(72.)*0.6).clamp(72.,144.)}.min(width_limit);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, crate::theme::TOOLBAR_HEIGHT), Sense::click_and_drag());
    let face=crate::theme::paint_tab(ui,&response,selected,"");
    // Drag/hit ownership remains the stable slot; glyphs follow its painted face.
    if device {
        uu_plugin_sdk::ui::icon(ui.painter(), Rect::from_center_size(egui::pos2(face.left()+12.,face.center().y), egui::vec2(18.,18.)), uu_plugin_sdk::ui::Icon::Desktop, color);
    }
    let text_rect = Rect::from_min_max(egui::pos2(face.left()+8.+leading,face.top()),egui::pos2(face.right()-6.,face.bottom()));
    let galley = if galley.size().x>text_rect.width() {
        let mut job=egui::text::LayoutJob::simple_singleline(text.to_owned(),font,color);
        job.wrap.max_width=text_rect.width();
        job.wrap.max_rows=1;
        ui.painter().layout_job(job)
    } else { galley };
    let position = egui::pos2(text_rect.left(), face.center().y - galley.size().y/2.);
    ui.painter().with_clip_rect(text_rect.intersect(ui.clip_rect())).galley(position, galley, color);
    response.on_hover_text(text)
}

#[derive(Clone, Copy)]
enum Control { Add }
fn control(ui: &mut egui::Ui, kind: Control, enabled: bool) -> Response {
    let enabled=enabled&&ui.is_enabled();
    let response=ui.add_enabled(enabled,crate::theme::toolbar_button("",crate::theme::ButtonKind::Ghost).min_size(egui::vec2(crate::theme::TOOLBAR_FACE,crate::theme::TOOLBAR_FACE)));
    let rect=crate::theme::toolbar_face_rect(ui,&response);
    let hovered = enabled && (response.hovered() || response.is_pointer_button_down_on());
    let color = if !enabled { crate::theme::palette(ui.visuals()).muted }
        else if hovered { match kind { Control::Add => crate::theme::palette(ui.visuals()).accent } }
        else { crate::theme::palette(ui.visuals()).muted };
    let fill = if enabled&&response.is_pointer_button_down_on(){crate::theme::palette(ui.visuals()).pressed}else if hovered { match kind {
        Control::Add => crate::theme::palette(ui.visuals()).tint,
    }} else { Color32::TRANSPARENT };
    if hovered {ui.painter().rect_filled(rect,12.,fill);}
    let center=rect.center();
    let stroke=Stroke::new(1.5,color);
    match kind {
        Control::Add => {
            ui.painter().line_segment([center-egui::vec2(4.5,0.),center+egui::vec2(4.5,0.)],stroke);
            ui.painter().line_segment([center-egui::vec2(0.,4.5),center+egui::vec2(0.,4.5)],stroke);
        },

    }
    response
}

fn connection_menu(ui: &mut egui::Ui, group: &Group, actions: &mut Vec<Action>, _rects: &mut StripRects) {
    crate::theme::compact_menu_style(ui);
    ui.set_min_width(180.);
    if group.can_open_displays && group.screens.len()>1 && crate::theme::menu_action(ui,true,uu_i18n::text("所有屏幕独立打开")).clicked() {
        actions.push(Action::OpenAllDisplays(group.command_session())); ui.close_menu();
    }
    if group.can_merge {
        let merge=crate::theme::menu_action(ui,true,uu_i18n::text("合并到原本窗口"));
        record!(_rects.merge,(group.command_session(),merge.rect));
        if merge.clicked() {actions.push(Action::Merge(group.command_session()));ui.close_menu();}
    } else {
        let detach=crate::theme::menu_action(ui,group.can_detach,uu_i18n::text("移到独立窗口"))
            .on_disabled_hover_text(uu_i18n::text("已在独立窗口中"));
        record!(_rects.detach,(group.command_session(),detach.rect));
        if detach.clicked() { actions.push(Action::Detach(group.command_session())); ui.close_menu(); }
    }
    ui.separator();
    let disconnect=crate::theme::menu_action(ui,true,uu_i18n::text("断开此会话"));
    record!(_rects.disconnect,(group.command_session(),disconnect.rect));
    if disconnect.clicked() { actions.push(Action::Close(group.command_session())); ui.close_menu(); }
}

fn insertion(ui: &egui::Ui, rect: Rect, after: bool) {
    let x=if after {rect.right()+1.} else {rect.left()-1.};
    ui.painter().line_segment([egui::pos2(x,rect.top()+3.),egui::pos2(x,rect.bottom()-3.)],Stroke::new(2.,crate::theme::palette(ui.visuals()).accent));
}

pub(super) fn show(tabs: &Tabs, ui: &mut egui::Ui, controls_width: f32) -> StripRects {
    tabs.local_ui.set(false);
    let width=(ui.available_width()-controls_width-ui.spacing().item_spacing.x).max(0.);
    // Keep the device name and its close control visible together.
    if width<124. {return StripRects::default();}
    let active_id=ui.make_persistent_id(("workspace-visible-device",ui.ctx().viewport_id()));
    let reveal_active=tabs.groups.iter().any(|group|group.contains(tabs.active)) && ui.ctx().data_mut(|data| {
        if data.get_temp::<u64>(active_id)==Some(tabs.active) {false}
        else {data.insert_temp(active_id,tabs.active);true}
    });
    let mut rects=StripRects::default();
    let mut actions=vec![];
    let _area=egui::ScrollArea::horizontal().id_salt("workspace-device-groups")
        .max_width(width).max_height(HEIGHT)
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .auto_shrink([true,true]).show(ui,|ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing=egui::vec2(6.,0.);
                for group in &tabs.groups {
                    ui.push_id(group.id,|ui| {
                        let active=group.contains(tabs.active);
                        let frame=egui::Frame::none().fill(if active {crate::theme::palette(ui.visuals()).card} else {crate::theme::palette(ui.visuals()).card.gamma_multiply(0.55)})
                            .rounding(8.).stroke(Stroke::new(0.75,crate::theme::palette(ui.visuals()).border))
                            .inner_margin(egui::Margin::symmetric(4.,2.));
                        let response=frame.show(ui,|ui| {
                            ui.horizontal_centered(|ui| {
                                ui.spacing_mut().item_spacing=egui::vec2(2.,0.);
                                let name=label(ui,&group.name,active,true,(width-crate::theme::TOOLBAR_HEIGHT-14.).max(70.));
                                record!(rects.devices,(group.id,name.rect));
                                name.dnd_set_drag_payload(Drag(group.command_session()));
                                if name.clicked() { actions.push(group.activate()); }
                                if name.dragged() { ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing); }
                                name.context_menu(|ui|connection_menu(ui,group,&mut actions,&mut rects));
                                if name.context_menu_opened() { tabs.local_ui.set(true); }
                                // Reveal the connection header once per activation. Its
                                // sources can be wider than the viewport; preserve
                                // manual scrolling after revealing the name once.
                                if active && reveal_active {name.scroll_to_me(None);}
                                if !group.screens.is_empty() {
                                    let (divider,_)=ui.allocate_exact_size(egui::vec2(7.,22.),Sense::hover());
                                    ui.painter().line_segment([divider.center()-egui::vec2(0.,6.),divider.center()+egui::vec2(0.,6.)],Stroke::new(1.,crate::theme::palette(ui.visuals()).border));
                                }
                                for (screen,text) in &group.screens {
                                    let response=label(ui,text,active && group.selected==Some(*screen),false,(width-12.).max(72.));
                                    record!(rects.screens,(group.id,*screen,response.rect));
                                    if response.clicked() { actions.push(group.screen_action(*screen)); }
                                    if let Some(view)=group.view_group(*screen).or_else(||group.fixed_view.then(||group.clone())) {
                                        response.context_menu(|ui|connection_menu(ui,&view,&mut actions,&mut rects));
                                    } else if group.can_open_displays {
                                        response.context_menu(|ui| {
                                            let open=ui.add_enabled(group.can_open_display(*screen),crate::theme::ghost_button(uu_i18n::text("在独立窗口中打开")))
                                                .on_disabled_hover_text(uu_i18n::text("已在独立窗口中"));
                                            record!(rects.open_display,(group.id,*screen,open.rect));
                                            if open.clicked() { actions.push(Action::OpenDisplay{session:group.id,screen:*screen}); ui.close_menu(); }
                                        });
                                    }
                                    if response.context_menu_opened() { tabs.local_ui.set(true); }
                                    if let Some(view)=group.view_group(*screen).or_else(||group.fixed_view.then(||group.clone())) {response.dnd_set_drag_payload(Drag(view.id));}
                                    else {
                                        response.dnd_set_drag_payload(DragScreen{session:group.id,screen:*screen});
                                        if let Some(drag)=response.dnd_hover_payload::<DragScreen>().filter(|drag|drag.session==group.id && drag.screen!=*screen) {
                                            let after=ui.input(|i|i.pointer.hover_pos().is_some_and(|p|p.x>response.rect.center().x));
                                            insertion(ui,response.rect,after);
                                            if response.dnd_release_payload::<DragScreen>().is_some() { actions.push(Action::ReorderScreen{session:group.id,source:drag.screen,target:*screen,after}); }
                                        }
                                    }
                                }
                                if group.can_open_displays || group.can_create_display {
                                    let add=control(ui,Control::Add,group.can_create_display)
                                        .on_hover_text(uu_i18n::text(if group.can_create_display {
                                            "新建虚拟显示器；默认断开后移除，源机可以保留"
                                        } else { "远端未提供虚拟显示器管理，请更新后台并授予管理服务权限" }));
                                    record!(rects.create,(group.id,add.rect));
                                    if add.clicked() { actions.push(Action::CreateDisplayWithSpec{session:group.creation_session(),spec:tabs.presets.default_spec()}); }
                                    if group.can_create_display {
                                        add.context_menu(|ui| {
                                            if let Some(spec)=crate::virtual_display_presets::create_menu(ui,&tabs.presets) {actions.push(Action::CreateDisplayWithSpec{session:group.creation_session(),spec});}
                                            ui.separator();
                                            if ui.add(crate::theme::ghost_button(uu_i18n::text("管理虚拟显示器"))).clicked() {actions.push(Action::ManageDisplays(group.creation_session()));ui.close_menu();}
                                        });
                                    }
                                    if add.context_menu_opened() {tabs.local_ui.set(true);}
                                }
                            });
                        }).response;
                        record!(rects.groups,(group.id,response.rect));
                        if let Some(drag)=response.dnd_hover_payload::<Drag>().filter(|drag|drag.0!=group.id) {
                            let after=ui.input(|i|i.pointer.hover_pos().is_some_and(|p|p.x>response.rect.center().x));
                            insertion(ui,response.rect,after);
                            if response.dnd_release_payload::<Drag>().is_some() {actions.push(Action::Reorder{source:drag.0,target:group.id,after});}
                        }
                    });
                }
            });
        });
    #[cfg(test)] {rects.bounds=_area.inner_rect;}
    tabs.actions.borrow_mut().extend(actions);
    if let Some(drag)=egui::DragAndDrop::payload::<Drag>(ui.ctx()) {
        let outside=ui.input(|i|i.pointer.any_released() && i.pointer.latest_pos().is_none_or(|p|!i.screen_rect().contains(p)));
        if outside && tabs.groups.iter().any(|group|group.contains(drag.0) && group.can_detach) {
            tabs.actions.borrow_mut().push(Action::Detach(drag.0));egui::DragAndDrop::clear_payload(ui.ctx());
        }
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(ctx:&egui::Context,tabs:&Tabs,time:f64,width:f32,controls:f32,events:Vec<egui::Event>)->StripRects {
        let mut rects=StripRects::default();
        let _=ctx.run(egui::RawInput {
            screen_rect:Some(Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(width,80.))),
            time:Some(time),events,..Default::default()
        },|ctx| {
            egui::CentralPanel::default().show(ctx,|ui| {
                ui.horizontal_centered(|ui| {rects=show(tabs,ui,controls);});
            });
        });
        rects
    }
    fn header(rects:&StripRects,id:u64)->Rect {
        rects.devices.iter().find(|item|item.0==id).unwrap().1
    }
    #[test]
    fn active_connection_reveals_its_header_once_and_preserves_manual_scroll() {
        let ctx=egui::Context::default();
        ctx.style_mut(|style|style.scroll_animation=egui::style::ScrollAnimation::none());
        let mut tabs=Tabs {presets:Default::default(),
            groups:(1..=4).map(|id|Group {last_session:id,members:vec![],views:vec![],
                id,name:format!("Generated computer {id} with a long title"),
                screens:vec![(1,"Main display".into()),(2,"Temporary virtual display".into())],
                selected:Some(1),can_open_displays:true,can_create_display:true,can_detach:true,fixed_view:false,can_merge:false,
            }).collect(),
            active:1,actions:Default::default(),local_ui:Default::default(),
        };
        frame(&ctx,&tabs,0.,440.,140.,vec![]);
        let before=frame(&ctx,&tabs,0.2,440.,140.,vec![]);
        assert!(header(&before,4).left()>before.bounds.right());
        tabs.active=4;
        frame(&ctx,&tabs,0.4,440.,140.,vec![]);
        frame(&ctx,&tabs,0.6,440.,140.,vec![]);
        let revealed=frame(&ctx,&tabs,0.8,440.,140.,vec![]);
        assert!(revealed.bounds.contains_rect(header(&revealed,4)),"active name and close must both be visible: {:?}",header(&revealed,4));
        let events=vec![egui::Event::PointerMoved(revealed.bounds.center()),egui::Event::MouseWheel {
            unit:egui::MouseWheelUnit::Point,delta:egui::vec2(10000.,0.),modifiers:Default::default(),
        }];
        frame(&ctx,&tabs,1.,440.,140.,events);
        let mut manual=frame(&ctx,&tabs,1.2,440.,140.,vec![]);
        for index in 0..8 {manual=frame(&ctx,&tabs,1.4+index as f64*0.2,440.,140.,vec![]);}
        assert!(header(&manual,4).left()>manual.bounds.right(),"the active tab must not pull manual scrolling back");
        tabs.groups[3].selected=Some(2);
        let source_changed=frame(&ctx,&tabs,3.,440.,140.,vec![]);
        assert_eq!(header(&source_changed,4),header(&manual,4));
        // Even a viewport just above the strip threshold keeps name + close
        // together; the title is elided before the close control is clipped.
        tabs.active=3;
        // Forty-point close controls raise the minimum header width.
        // Keep this case just above that threshold, not below the strip guard.
        frame(&ctx,&tabs,3.2,320.,170.,vec![]);
        frame(&ctx,&tabs,3.4,320.,170.,vec![]);
        let narrow=frame(&ctx,&tabs,3.6,320.,170.,vec![]);
        assert!(narrow.bounds.contains_rect(header(&narrow,3)));
        assert!(tabs.actions.borrow().is_empty());
    }
    #[test]
    fn readable_fixed_source_activates_its_connection_and_routes_the_real_window_menu() {
        let ctx=egui::Context::default();
        let name="Generated workstation with a descriptive name";
        let source="Display 12 (VDD by MTT DISPLAY16)";
        let mut tabs=Tabs {presets:Default::default(),
            groups:vec![Group{last_session:1,members:vec![],views:vec![],id:1,name:name.into(),screens:vec![(12,source.into())],selected:Some(12),
                can_open_displays:false,can_create_display:true,can_detach:false,fixed_view:true,can_merge:true}],
            active:1,actions:Default::default(),local_ui:Default::default(),
        };
        let rects=frame(&ctx,&tabs,0.,1100.,180.,vec![]);
        let text_width=|text:&str,size:f32|ctx.fonts(|fonts|fonts.layout_no_wrap(text.into(),egui::FontId::proportional(size),Color32::WHITE).size().x);
        assert!(rects.devices[0].1.width()+0.1>=text_width(name,12.5)+38.);
        assert!(rects.screens[0].2.width()<=144.1,"long source captions must remain compact");
        assert!(rects.close.is_empty(),"the hostname has no inline disconnect action");
        assert_eq!(rects.create.len(),1,"a fixed view retains its independent virtual-display creation entry");
        let point=rects.screens[0].2.center();
        let button=|point,button,pressed|vec![egui::Event::PointerMoved(point),egui::Event::PointerButton{pos:point,button,pressed,modifiers:Default::default()}];
        frame(&ctx,&tabs,0.2,1100.,180.,button(point,egui::PointerButton::Primary,true));
        frame(&ctx,&tabs,0.4,1100.,180.,button(point,egui::PointerButton::Primary,false));
        assert!(matches!(&tabs.actions.borrow()[..],[Action::Select(1,None)]),"fixed source must select the connection without requesting a display switch");
        tabs.actions.borrow_mut().clear();
        frame(&ctx,&tabs,0.6,1100.,180.,button(point,egui::PointerButton::Secondary,true));
        frame(&ctx,&tabs,0.8,1100.,180.,button(point,egui::PointerButton::Secondary,false));
        let popup=frame(&ctx,&tabs,1.,1100.,180.,vec![]);
        assert_eq!(popup.merge.len(),1);assert!(popup.detach.is_empty());
        let merge=popup.merge[0].1.center();
        frame(&ctx,&tabs,1.2,1100.,180.,button(merge,egui::PointerButton::Primary,true));
        frame(&ctx,&tabs,1.4,1100.,180.,button(merge,egui::PointerButton::Primary,false));
        assert!(matches!(&tabs.actions.borrow()[..],[Action::Merge(1)]));
        tabs.actions.borrow_mut().clear();
        // Returning home changes window ownership, not the fixed source.
        tabs.groups[0].can_merge=false;tabs.groups[0].can_detach=true;
        frame(&ctx,&tabs,1.6,1100.,180.,button(point,egui::PointerButton::Secondary,true));
        frame(&ctx,&tabs,1.8,1100.,180.,button(point,egui::PointerButton::Secondary,false));
        let home=frame(&ctx,&tabs,2.,1100.,180.,vec![]);
        assert!(home.merge.is_empty());assert_eq!(home.detach.len(),1);
    }
}
