//! Shared rendering for the service page and the floating host session center.
use eframe::egui;
use uu_core::host_admin::{Request, SessionCenter};

fn card(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    let frame=crate::theme::card(ui.visuals());
    let width=(ui.available_width()-frame.inner_margin.sum().x-frame.outer_margin.sum().x-2.*frame.stroke.width).max(1.);
    frame.show(ui,|ui|{ui.set_width(width);contents(ui);});
}

pub(super) fn show(ui: &mut egui::Ui, center: &SessionCenter) -> Vec<Request> {
    let mut actions = Vec::new();
    let mut policy = center.input.policy;
    card(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.strong(uu_i18n::text("共享控制"));
            if ui.add_enabled(!center.sessions.is_empty(), crate::theme::secondary_button(uu_i18n::text("全部仅观看")).small()).clicked() {
                actions.push(Request::AllViewOnly);
            }
        });
        ui.horizontal_wrapped(|ui| {
            crate::theme::switch(ui, &mut policy.game_room, uu_i18n::text("共享游戏厅"))
                .on_hover_text(uu_i18n::text("新加入的会话仅观看；由你逐项开启键盘、鼠标和手柄。"));
            crate::theme::switch(ui, &mut policy.local_priority, uu_i18n::text("本机键鼠优先"));
        });
        if policy.game_room {ui.weak(uu_i18n::text("新会话默认仅观看"));}
        ui.horizontal_wrapped(|ui| {
            if policy.local_priority {
                crate::theme::switch(ui, &mut policy.automatic_resume, uu_i18n::text("空闲 3 秒后恢复"))
                    .on_hover_text(uu_i18n::text("本机空闲 3 秒后自动恢复远程键鼠"));
            }
            if center.input.keyboard_mouse_paused {
                if ui.add(crate::theme::secondary_button(uu_i18n::text("恢复键鼠")).small()).clicked() {
                    actions.push(Request::PauseHostInput { paused: false });
                }
            } else if ui.add(crate::theme::secondary_button(uu_i18n::text("暂停远程键鼠")).small()).clicked() {
                actions.push(Request::PauseHostInput { paused: true });
            }
        });
        if center.input.keyboard_mouse_paused {ui.weak(uu_i18n::text("远程键鼠已暂停，手柄不受影响"));}
        if policy.local_priority && !center.input.local_activity_supported {
            ui.weak(uu_i18n::text("本机输入检测尚未就绪；仍可手动暂停远程键鼠。"));
        }
    });
    if policy != center.input.policy { actions.push(Request::SetHostInput { policy }); }
    ui.add_space(10.);
    if center.sessions.is_empty() {
        ui.weak(uu_i18n::text("当前没有连接的会话。设备连接后，可在这里查看和管理会话。"));
    }
    for session in &center.sessions {
        ui.push_id(("host-session-center", &session.id), |ui| {
            card(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.strong(&session.name);
                    ui.weak(&session.status);
                    if ui.add(crate::theme::secondary_button(uu_i18n::text("断开此会话")).small()).clicked() {
                        actions.push(Request::Disconnect { session: session.id.clone() });
                    }
                });
                let Some(control) = &session.control else {
                    ui.weak(uu_i18n::text("此会话尚未建立控制通道。"));
                    return;
                };
                ui.horizontal_wrapped(|ui| {
                    let mut input = control.input;
                    if crate::sharing_ui::input_permissions(ui, &mut input, control.max_input) {
                        actions.push(Request::SessionPermissions { session: session.id.clone(), input });
                    }
                    if control.gamepads > 0 { ui.weak(uu_i18n::format!("手柄 {} 个", control.gamepads)); }
                });
                if !control.keyboard_mouse_active && (control.input.keyboard || control.input.mouse) && !center.input.keyboard_mouse_paused {
                    ui.weak(uu_i18n::text("键鼠控制权由当前取得控制的一方使用。"));
                }
                if let Some(error) = &control.gamepad_error { ui.colored_label(crate::theme::palette(ui.visuals()).warning, error); }
                if let Some(error) = &control.quality_error { ui.colored_label(crate::theme::palette(ui.visuals()).warning, error); }
                if !control.displays.is_empty() {
                    let mut selected = control.display;
                    let label = selected.and_then(|id| control.displays.iter().find(|screen| screen.id == id))
                        .map(|screen| screen.display_name.clone()).unwrap_or_else(|| uu_i18n::text("当前显示器").into());
                    crate::theme::Select::from_id_salt("display").selected_text(label).show_ui(ui, |ui| {
                        for screen in &control.displays {
                            crate::theme::selectable_value(ui, &mut selected, Some(screen.id), screen.display_name.clone());
                        }
                    });
                    if selected != control.display {
                        if let Some(id) = selected { actions.push(Request::SessionDisplay { session: session.id.clone(), id }); }
                    }
                }
                if control.fps > 0 && control.bitrate_kbps > 0 {
                    let draft = egui::Id::new(("host-session-quality", &session.id));
                    let current=(control.fps,control.bitrate_kbps);
                    let (previous,mut value)=ui.data_mut(|data|data.get_temp::<((u32,u32),(u32,u32))>(draft)).unwrap_or((current,current));
                    if value==previous {value=current;}
                    let (mut fps, mut bitrate)=value;
                    ui.horizontal_wrapped(|ui| {
                        ui.label(uu_i18n::text("帧率"));
                        crate::theme::number_input(ui, egui::DragValue::new(&mut fps).range(5..=240).suffix(" FPS"));
                        ui.label(uu_i18n::text("码率"));
                        crate::theme::number_input(ui, egui::DragValue::new(&mut bitrate).range(64..=1_000_000).suffix(" kbps"));
                        if ui.add(crate::theme::secondary_button(uu_i18n::text("应用")).small()).clicked() {
                            actions.push(Request::SessionQuality { session: session.id.clone(), update: uu_core::native_quality::Update {
                                fps, bitrate_kbps: Some(bitrate), rate_control: None,
                            } });
                        }
                    });
                    ui.data_mut(|data| data.insert_temp(draft, (current,(fps, bitrate))));
                }
            });
        });
        ui.add_space(8.);
    }
    actions
}

#[cfg(test)]
pub(super) fn fixture() -> SessionCenter {
    use uu_core::{device_trust::{InputPermissions,Permissions},host_admin::SessionControl,shared_host::SessionControlSnapshot};
    let screen=uu_core::screen_control::RemoteScreen{id:0,refresh_hz:120,kind:0,width:1920,height:1080,dpi_scale:None,video_track_index:0,display_name:"1 · 游戏显示器".into(),primary:true};
    SessionCenter {input:uu_core::host_input::Snapshot{policy:uu_core::host_input::Policy{game_room:true,..Default::default()},keyboard_mouse_paused:false,manually_paused:false,local_activity_supported:true},
        sessions:[("guest-a","访客 Alice",true),("guest-b","访客 Bob",false)].into_iter().map(|(id,name,pad)|SessionControl {
            id:id.into(),peer:id.into(),name:name.into(),status:"1920×1080 · 60 FPS".into(),permissions:Permissions::CONTROL,
            control:Some(SessionControlSnapshot{input:InputPermissions{gamepad:pad,..Default::default()},max_input:InputPermissions::ALL,
                fps:60,bitrate_kbps:18000,display:Some(0),displays:vec![screen.clone()],keyboard_mouse_active:false,gamepads:usize::from(pad),gamepad_error:None,quality_error:None}),
        }).collect()}
}

impl crate::App {
    pub(super) fn host_session_window(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.workspace.host_sessions_requested) { self.host_sessions_open=true;self.host_sessions_focus=true; }
        if !self.host_sessions_open || self.stream_target.is_some() || self.workspace_tabs.is_some() { return; }
        let id = egui::ViewportId::from_hash_of("host-session-center");
        if std::mem::take(&mut self.host_sessions_focus) {
            ctx.send_viewport_cmd_to(id,egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd_to(id,egui::ViewportCommand::Focus);
        }
        ctx.show_viewport_immediate(id, egui::ViewportBuilder::default()
            .with_title(uu_i18n::text("悬浮会话中心 · Eclipse"))
            .with_inner_size([450., 600.]).with_min_inner_size([360., 220.])
            .with_window_level(egui::WindowLevel::AlwaysOnTop)
            .with_visible(true), |ctx, _| {
                if ctx.input(|input| input.viewport().close_requested()) { self.host_sessions_open = false; }
                egui::CentralPanel::default().frame(egui::Frame::none().fill(crate::theme::palette(&ctx.style().visuals).base).inner_margin(12.)).show(ctx, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        #[cfg(windows)] self.workspace.background_service.session_center(ui, &mut self.workspace.host);
                        #[cfg(not(windows))] self.workspace.host.session_center(ui);
                    });
                });
            });
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}
