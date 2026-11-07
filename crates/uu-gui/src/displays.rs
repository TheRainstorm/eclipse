//! A display is identified by the remote source ID, never packet arrival order.
use eframe::egui;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use uu_core::screen_control::RemoteScreens;

pub(super) type CaptureProfile = (u32, usize, u32, Option<[u32;2]>, bool);

#[derive(Default)]
pub(super) struct Displays {
    pub native_frame_epoch:u64,
    native_request_epoch:u64,
    pub selected: Option<i32>,
    pending: Option<Pending>,
    queued: Option<i32>,
    initial_selection_checked: bool,
    initial_display: Option<super::sessions::DisplayTarget>,
    fixed_display: Option<super::sessions::DisplayTarget>,
    pub(super) persistent_virtual: bool,
    awaiting_virtual: bool,
    remember_on_confirmation: Option<i32>,
    blocked: bool,
    unbound: bool,
    connection_error: Option<String>,
    pub notice: String,
    pub menu_open: bool,
    pub anchor: egui::Pos2,
    pub layout: super::display_layout::DisplayLayout,
    pub layout_path: Option<std::path::PathBuf>,
    // A selected source is device-wide even when its tab order belongs to an
    // explicit display window. Keep the device layout cached for that case.
    device_layout: super::display_layout::DisplayLayout,
    device_layout_path: Option<std::path::PathBuf>,
    next_request: u64,
    profiles: HashMap<i32, (u32, CaptureProfile)>,
    settings_sent: Option<(u64, i32, u32, CaptureProfile)>,
    bindings: HashMap<u32, i32>,
    last_frames: HashMap<u32, Instant>,
}
struct Pending {
    request: u64,
    track: Option<u32>,
    activation: bool,
    sent: Instant,
    accepted: bool,
    frame: bool,
    settings_request: Option<u64>,
    settings_accepted: bool,
}
impl Displays {
    pub(super) fn for_reconnect(&self)->Self {
        let mut next=Self::default();
        if let Some(target)=&self.fixed_display {next.set_initial_display(target.clone());}
        next.persistent_virtual=self.persistent_virtual;
        next
    }
    pub(super) fn for_retirement(&self) -> Self {
        let mut next = self.for_reconnect();
        if let Some(error) = &self.connection_error { next.connection_failed(error.clone()); }
        next
    }
    fn for_connection(&self,connection:&super::sessions::ConnectionTarget)->Self {
        let mut next=self.for_reconnect();
        if connection.virtual_display().is_some() && !(next.persistent_virtual && next.fixed_display.is_some()) {
            next=Self::default();
            next.await_virtual();
        }
        if let Some(display)=connection.display().cloned() {
            if !next.awaiting_virtual {next.set_initial_display(display);}
        }
        next
    }
    pub(super) fn initial_request(&self,connection:&super::sessions::ConnectionTarget)->Option<uu_core::native_displays::InitialRequest> {
        use uu_core::native_displays::InitialRequest;
        if let Some(target)=connection.virtual_display() {
            if !self.persistent_virtual || self.fixed_display.is_none() {
                return Some(InitialRequest::virtual_display(target.spec,target.persistent));
            }
        }
        self.fixed_display.clone().map(|target|InitialRequest::Existing{id:target.id,source:target.source})
    }
    fn reconcile_fixed_source(&mut self,screens:&RemoteScreens) {
        let Some(target)=self.fixed_display.clone() else{return;};
        let result=target.resolve(&screens.screens);
        if result==Ok(screens.current_id){return;}
        self.blocked=true;
        self.initial_display=Some(target);
        self.initial_selection_checked=false;
        self.selected=result.ok();
        if result.is_err(){self.notice=uu_i18n::text("固定显示器不可用，正在等待原显示器恢复；也可关闭此标签").into();}
    }
    fn permits_source(&self,id:i32,screens:&RemoteScreens)->bool {
        self.fixed_display.as_ref().is_none_or(|target|target.resolve(&screens.screens)==Ok(id))
    }
    fn await_virtual(&mut self) {
        self.awaiting_virtual=true;
        self.blocked=true;
        self.notice=uu_i18n::text("正在创建临时虚拟显示器…").into();
    }
    pub(super) fn bind_created_virtual(&mut self,target:super::sessions::DisplayTarget) {
        self.awaiting_virtual=false;
        self.initial_selection_checked=false;
        self.selected=None;
        self.set_initial_display(target);
    }
    pub(super) fn awaiting_virtual(&self)->bool {self.awaiting_virtual}
    fn set_initial_display(&mut self,target:super::sessions::DisplayTarget) {
        self.fixed_display=Some(target.clone());
        self.initial_display=Some(target);
        self.blocked=true;
        self.notice=uu_i18n::text("正在准备指定显示器…").into();
    }
    fn restore_selection(&mut self, screens: &RemoteScreens, catalog_current: bool, control_ready: bool) -> Option<i32> {
        if self.awaiting_virtual || self.initial_selection_checked || self.pending.is_some() || !catalog_current || !control_ready {return None;}
        if let Some(target)=&self.initial_display {
            match target.resolve(&screens.screens) {
                Ok(id)=>{
                    self.initial_selection_checked=true;
                    self.selected=Some(id);
                    self.remember_on_confirmation=Some(id);
                    return (id!=screens.current_id).then_some(id);
                }
                Err(notice)=>{self.notice=if self.fixed_display.is_some(){uu_i18n::text("固定显示器不可用，正在等待原显示器恢复；也可关闭此标签").into()}else{notice.into()};return None;}
            }
        }
        if screens.screens.is_empty(){return None;}
        self.initial_selection_checked = true;
        self.layout.remembered(&screens.screens).filter(|id| *id != screens.current_id)
    }
    fn confirm_initial_current(&mut self,screens:&RemoteScreens,catalog_current:bool,control_ready:bool,has_frame:bool) {
        // No selection RPC is needed when the host is already capturing the
        // requested source. Its current catalog and real frame still gate it.
        if self.pending.is_none() && self.initial_selection_checked && catalog_current && control_ready && has_frame
            && self.selected==Some(screens.current_id)
            && self.remember_on_confirmation==self.selected
            && self.initial_display.as_ref().is_some_and(|target|target.resolve(&screens.screens)==Ok(screens.current_id)) {
            self.initial_display=None;
            self.blocked=false;
            self.notice.clear();
        }
    }
    fn remember_display(&mut self, screens: &RemoteScreens, catalog_current: bool, has_frame: bool) -> bool {
        if !self.ready() || !catalog_current || !has_frame || self.selected != Some(screens.current_id) {return false;}
        if self.remember_on_confirmation != self.selected && self.layout.last_selected().is_some() {return false;}
        self.remember_on_confirmation = None;
        screens.selected().is_some_and(|screen| self.layout.remember_selected(screen))
    }
    pub fn ready(&self) -> bool {
        self.connection_error.is_none() && !self.awaiting_virtual && !self.blocked && self.pending.is_none()
    }
    pub(super) fn connection_error(&self) -> Option<&str> { self.connection_error.as_deref() }
    pub(super) fn connection_failed(&mut self, message: String) {
        // Keep the fixed source for an explicit reconnect. No late catalog,
        // progress or frame may turn a failed connection back into preparation.
        self.connection_error = Some(message);
        self.initial_display = None;
        self.awaiting_virtual = false;
        self.pending = None;
        self.queued = None;
        self.settings_sent = None;
        self.blocked = true;
        self.unbound = true;
        self.notice.clear();
    }
    pub(super) fn selected_target(&self, screens: &RemoteScreens) -> Option<super::sessions::DisplayTarget> {
        let id = self.selected?;
        let screen = screens.screens.iter().find(|screen| screen.id == id)?;
        Some(super::sessions::DisplayTarget { id, source: screen.display_name.clone() })
    }
    pub(super) fn confirmed_refresh(&self, screens:&RemoteScreens)->Option<u32> {
        self.ready().then(||self.selected.and_then(|id|screens.screens.iter().find(|s|s.id==id)).map(|s|s.refresh_hz)).flatten()
    }
    pub fn switching(&self) -> bool {
        self.pending.is_some()
    }
    pub(super) fn preparing_initial_target(&self)->bool {
        self.awaiting_virtual || self.initial_display.is_some()
    }
    // A source change is not cancellable on the host. Keep only the newest
    // selection until the in-flight source, settings and frame are confirmed.
    fn defer_selection(&mut self, id: i32) -> bool {
        if self.pending.is_none() { return false; }
        self.queued = (self.selected != Some(id)).then_some(id);
        true
    }
    fn queued_selection(&mut self) -> Option<i32> {
        if self.pending.is_some() { None } else { self.queued.take() }
    }
    fn begin(&mut self, id: i32, track: u32, now: Instant) -> u64 {
        self.next_request = self.next_request.wrapping_add(1);
        let request = 10_000 + self.next_request;
        self.selected = Some(id);
        self.remember_on_confirmation = Some(id);
        self.blocked = true;
        self.unbound = false;
        self.pending = Some(Pending {
            request,
            track: Some(track),
            activation: false,
            sent: now,
            accepted: false,
            frame: false,
            settings_request: None,
            settings_accepted: true,
        });
        self.notice = uu_i18n::text("正在切换显示器…").into();
        request
    }
    fn begin_activation(&mut self, id: i32, now: Instant) -> u64 {
        let request = self.begin(id, 0, now);
        let p = self.pending.as_mut().unwrap();
        p.track = None;
        p.activation = true;
        self.unbound = true;
        self.notice = uu_i18n::text("正在切换采集源…").into();
        request
    }
    pub fn waiting_binding(&self) -> bool {
        // The initial host frame can belong to another monitor. Keep it hidden
        // until this window's explicit first selection has been confirmed.
        self.connection_error.is_some() || self.awaiting_virtual || self.unbound || self.initial_display.is_some()
    }
    fn bind_track(&mut self, track: u32) {
        if let Some(p) = self
            .pending
            .as_mut()
            .filter(|p| p.activation && p.track.is_none())
        {
            p.track = Some(track);
            p.frame = false;
            self.unbound = false;
        }
    }
    fn frame(&mut self, track: u32) {
        if let Some(p) = self.pending.as_mut().filter(|p| p.track == Some(track)) {
            p.frame = true;
        }
        self.complete();
    }
    fn response(&mut self, id: u64, case: u32, code: i32) {
        if !self
            .pending
            .as_ref()
            .is_some_and(|p| p.request == id && case == if p.activation { 19 } else { 11 })
        {
            return;
        }
        if code != 0 {
            self.pending = None;
            self.remember_on_confirmation = None;
            self.blocked = true;
            self.notice = uu_i18n::format!("切换失败（远端错误 {code}），请重新选择显示器");
        } else {
            self.pending.as_mut().unwrap().accepted = true;
            self.complete();
        }
    }
    pub fn expect_settings(&mut self, id: u64) {
        if let Some(p) = self.pending.as_mut() {
            p.settings_request = Some(id);
            p.settings_accepted = false;
        }
    }
    pub fn settings_sent(&mut self, id: u64, screen: i32, track: u32, profile: CaptureProfile) {
        self.expect_settings(id);
        self.settings_sent = Some((id, screen, track, profile));
    }
    fn settings_match(&self, screen: i32, track: u32, profile: CaptureProfile) -> bool {
        self.profiles.get(&screen) == Some(&(track, profile))
    }
    fn capture_response(&mut self, id: u64, accepted: bool) {
        if let Some((_, screen, track, profile)) = self.settings_sent.filter(|s| s.0 == id) {
            self.settings_sent = None;
            if accepted && self.bindings.get(&track) == Some(&screen) {
                self.profiles.insert(screen, (track, profile));
            }
        }
        let Some(p) = self
            .pending
            .as_mut()
            .filter(|p| p.settings_request == Some(id) && !p.settings_accepted)
        else {
            return;
        };
        if accepted {
            p.settings_accepted = true;
            self.complete();
        } else {
            self.pending = None;
            self.remember_on_confirmation = None;
            self.blocked = true;
            self.notice = uu_i18n::text("远端拒绝了目标显示器设置，请重新选择").into();
        }
    }
    fn complete(&mut self) {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.accepted && p.frame && p.settings_accepted)
        {
            self.pending = None;
            self.initial_display = None;
            self.blocked = false;
            self.notice.clear();
        }
    }
    fn native_frame(&mut self,catalog_epoch:u64,has_frame:bool)->bool{
        let fresh=self.native_frame_epoch!=self.native_request_epoch && catalog_epoch==self.native_frame_epoch;
        if fresh && has_frame {if let Some(p)=self.pending.as_mut(){p.frame=true;}self.complete();}
        self.ready()
    }
    fn native_source_frame(&mut self,screens:&RemoteScreens,catalog_epoch:u64,has_frame:bool)->bool {
        self.selected==Some(screens.current_id) && self.native_frame(catalog_epoch,has_frame)
    }
    fn tick(&mut self, now: Instant) {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| now.duration_since(p.sent) > Duration::from_secs(8))
        {
            let stage = self
                .pending
                .as_ref()
                .map(|p| {
                    if !p.accepted {
                        uu_i18n::text("等待远端切屏确认")
                    } else if p.track.is_none() {
                        uu_i18n::text("等待采集源通知")
                    } else if !p.settings_accepted {
                        uu_i18n::text("等待画质确认")
                    } else {
                        uu_i18n::text("等待目标画面")
                    }
                })
                .unwrap_or(uu_i18n::text("未完成"));
            self.pending = None;
            self.remember_on_confirmation = None;
            self.blocked = true;
            self.notice = uu_i18n::format!("显示器切换超时（{stage}），可重试或切回其它显示器");
        }
    }
}

impl super::App {
    pub(super) fn is_fixed_display_view(&self)->bool {
        self.displays.fixed_display.is_some() || self.stream_target.as_ref().is_some_and(|target|target.connection.is_fixed_display_view())
    }
    pub(super) fn fixed_display_target(&self)->Option<super::sessions::DisplayTarget> {self.displays.fixed_display.clone()}
    pub(super) fn pin_current_display_view(&mut self)->bool {
        if self.is_fixed_display_view(){return true;}
        if !self.stream_target.as_ref().is_some_and(super::sessions::StreamTarget::is_native)
            || !self.controller.as_ref().is_some_and(|handle|handle.stats.capabilities.allows(uu_core::capabilities::Capability::Displays)) {return true;}
        if self.disconnecting || self.exit_requested {return false;}
        let screens=self.remote_screens();
        let Some(target)=self.displays.selected_target(&screens) else {return false;};
        self.pin_display_view(target)
    }
    pub(super) fn pin_display_view(&mut self,target:super::sessions::DisplayTarget)->bool {
        if self.is_fixed_display_view(){return true;}
        if !self.stream_target.as_ref().is_some_and(super::sessions::StreamTarget::is_native)
            || !self.controller.as_ref().is_some_and(|handle|handle.stats.capabilities.allows(uu_core::capabilities::Capability::Displays)) {return true;}
        if self.disconnecting || self.exit_requested {return false;}
        self.displays.fixed_display=Some(target);
        self.displays.queued=None;
        self.displays.menu_open=false;
        true
    }
    pub(super) fn visible_remote_screens(&self)->RemoteScreens {
        let mut screens=self.remote_screens();
        if self.is_fixed_display_view() {
            let selected=self.displays.fixed_display.as_ref().and_then(|target|target.resolve(&screens.screens).ok());
            screens.screens.retain(|screen|Some(screen.id)==selected);
        }
        screens
    }
    pub(super) fn prepare_displays(&mut self) {
        self.displays = self.stream_target.as_ref().map(|target|self.displays.for_connection(&target.connection)).unwrap_or_default();
        self.selected_track = 0;
        let Some(path) = self.config.as_ref().zip(self.stream_target.as_ref())
            .and_then(|(config, target)| super::display_layout::DisplayLayout::path(config, target)) else {return;};
        match super::display_layout::DisplayLayout::load(&path) {
            Ok(layout) => {self.displays.layout = layout; self.displays.layout_path = Some(path);}
            // Preserve an unreadable original instead of overwriting it when
            // the first catalog arrives with an otherwise empty layout.
            Err(error) => self.set_settings_notice(uu_i18n::format!("显示器排序读取失败：{error}")),
        }
        let device_path=self.config.as_ref().zip(self.stream_target.as_ref())
            .and_then(|(config, target)| super::display_layout::DisplayLayout::device_path(config, target));
        if let Some(device_path)=device_path.filter(|path|self.displays.layout_path.as_ref()!=Some(path)) {
            match super::display_layout::DisplayLayout::load(&device_path) {
                Ok(layout) => {
                    self.displays.device_layout=layout;
                    self.displays.device_layout_path=Some(device_path);
                }
                Err(error) => self.set_settings_notice(uu_i18n::format!("显示器排序读取失败：{error}")),
            }
        }
    }
    pub(super) fn remote_screens(&self) -> RemoteScreens {
        let mut screens = self
            .controller
            .as_ref()
            .map(|h| {
                h.stats
                    .remote_screens
                    .lock()
                    .unwrap()
                    .clone()
                    .with_desktop_capture(*h.stats.desktop_capture_source.lock().unwrap())
            })
            .unwrap_or_default();
        self.displays.layout.sort(&mut screens.screens);
        screens
    }
    fn save_display_layout(&mut self) {
        if let Some(path) = &self.displays.layout_path {
            if let Err(error) = self.displays.layout.save(path) {
                self.set_settings_notice(uu_i18n::format!("显示器排序保存失败：{error}"));
            }
        }
    }
    fn remember_device_display(&mut self,screens:&RemoteScreens) {
        let Some(path)=self.displays.device_layout_path.clone() else{return;};
        let Some(screen)=screens.selected() else{return;};
        if !self.displays.device_layout.remember_selected(screen) {return;}
        if let Err(error)=self.displays.device_layout.save(&path) {
            self.set_settings_notice(uu_i18n::format!("显示器排序保存失败：{error}"));
        }
    }
    pub(super) fn reorder_display_tab(&mut self, source: i32, target: i32, after: bool) {
        if self.is_fixed_display_view(){return;}
        if self.displays.layout.move_tab(source, target, after) {
            self.save_display_layout();
        }
    }
    pub(super) fn display_label(&self) -> String {
        self.displays
            .selected
            .and_then(|id| self.remote_screens().screens.into_iter().find(|screen|screen.id==id))
            .map(|screen| self.displays.layout.number_for(&screen))
            .map(|number| uu_i18n::format!("显示屏 {number}"))
            .unwrap_or_else(|| uu_i18n::text("显示器").into())
    }
    pub(super) fn screen_label(&self,screen:&uu_core::screen_control::RemoteScreen)->String {
        let friendly=self.controller.as_ref().and_then(|h|h.stats.virtual_displays.snapshot().and_then(|(_,displays)|displays.into_iter().find(|display|display.source==screen.display_name)).map(|display|display.name)
            .or_else(||h.stats.native_displays.display_label(&screen.display_name)));
        friendly.map_or_else(||self.displays.layout.label(screen),|name|self.displays.layout.label_named(screen,&name))
    }
    pub(super) fn select_remote_screen(&mut self,id:i32){
        if self.active_session {
            if let Some(tabs)=&self.workspace_tabs {
                tabs.actions.borrow_mut().push(super::stream_workspace::Action::Select(tabs.active,Some(id)));
                self.release_all_keys();self.release_pointer_capture(&self.context.clone());
                self.displays.menu_open=false;self.stream_menu.open=false;
                self.context.memory_mut(|memory|memory.close_popup());return;
            }
        }
        self.select_native_display(id);
    }
    pub(super) fn select_native_display(&mut self,id:i32){
        if self.displays.awaiting_virtual {return;}
        let screens=self.remote_screens();
        if !self.displays.permits_source(id,&screens){return;}
        if !screens.screens.iter().any(|s|s.id==id){return;}
        if self.displays.defer_selection(id){return;}
        if self.displays.selected==Some(id) && self.displays.ready(){return;}
        if !self.controller.as_ref().is_some_and(|h|h.stats.control_open.load(std::sync::atomic::Ordering::Acquire)!=0){return;}
        self.displays.initial_selection_checked=true;
        self.release_all_keys();self.release_pointer_capture(&self.context.clone());
        self.cursor=super::cursor::Cursor::default();
        let binding=self.controller.as_ref().is_some_and(|h|h.stats.display_track_binding.load(std::sync::atomic::Ordering::Acquire));
        let track=screens.screens.iter().find(|s|s.id==id).and_then(|s|s.assigned_track()).unwrap_or(0);
        let request=self.displays.begin(id,track,Instant::now());
        if binding {self.textures.remove(&track);self.unpresented.remove(&track);}
        self.displays.native_request_epoch=self.displays.native_frame_epoch;
        let sent=self.controller.as_ref().and_then(|h|h.native_input.as_ref()).is_some_and(|input|input.select_display(request,id).is_ok());
        if !sent {
            self.displays.pending=None;self.displays.blocked=self.displays.initial_display.is_some();
            self.displays.remember_on_confirmation=None;
            if self.displays.initial_display.is_none(){self.displays.selected=Some(screens.current_id);}
            self.displays.notice=uu_i18n::text("切换失败：控制连接不可用").into();
        }
        self.displays.menu_open=false;self.stream_menu.open=false;
        self.context.memory_mut(|m|m.close_popup());
    }
    fn poll_native_displays(&mut self,tracks:&[u32]){
        if self.displays.awaiting_virtual {return;}
        let screens=self.remote_screens();
        self.displays.reconcile_fixed_source(&screens);
        if self.displays.layout.sync(&screens.screens){self.save_display_layout();}
        let Some(h)=self.controller.as_ref()else{return;};
        let binding=h.stats.display_track_binding.load(std::sync::atomic::Ordering::Acquire);
        let catalog_epoch=h.stats.native_displays.catalog_epoch();
        let catalog_current=if binding {h.stats.remote_screens_revision.load(std::sync::atomic::Ordering::Acquire)>0}
            else {catalog_epoch>0 && catalog_epoch==self.displays.native_frame_epoch};
        let control_ready=h.stats.control_open.load(std::sync::atomic::Ordering::Acquire)!=0;
        for response in h.stats.native_displays.responses(){
            if !self.displays.pending.as_ref().is_some_and(|p|p.request==response.request){continue;}
            if response.accepted && (response.restart||binding) {self.displays.pending.as_mut().unwrap().accepted=true;}
            else {
                self.displays.pending=None;self.displays.blocked=self.displays.initial_display.is_some();
                if !response.accepted {self.displays.remember_on_confirmation=None;}
                if self.displays.initial_display.is_none(){self.displays.selected=Some(screens.current_id);}
                self.displays.notice=if response.accepted{String::new()}else{uu_i18n::text("被控端未允许切换到该显示器").into()};
            }
        }
        if self.displays.pending.is_some(){
            // Acknowledgment alone is insufficient: require the catalog and an
            // actual frame from the new media generation, even at identical size.
            let bound=screens.screens.iter().find(|s|Some(s.id)==self.displays.selected).and_then(|s|s.assigned_track());
            let ready=if binding {self.displays.pending.as_ref().is_some_and(|p|p.accepted)&&bound.is_some_and(|track|tracks.contains(&track))}
                else {self.displays.native_source_frame(&screens,catalog_epoch,tracks.contains(&0)||self.textures.contains_key(&0))};
            if ready {
                if binding {self.displays.pending=None;self.displays.initial_display=None;self.displays.blocked=false;self.displays.notice.clear();}
                if !binding {self.displays.selected=Some(screens.current_id);}
            }
        }else if self.displays.initial_display.is_none() && catalog_current && !screens.screens.is_empty(){
            self.displays.selected=Some(screens.current_id);
        }
        // The compositor may need local permission; use the same preparation
        // bound as native SDP rather than the interoperability RPC's 8 seconds.
        if self.displays.pending.as_ref().is_some_and(|p|p.sent.elapsed()>Duration::from_secs(125)){
            self.displays.pending=None;self.displays.blocked=true;
            self.displays.remember_on_confirmation=None;
            self.displays.notice=uu_i18n::text("等待新屏幕超时，请检查被控端授权或重新连接").into();
        }
        self.selected_track=if binding {screens.screens.iter().find(|s|Some(s.id)==self.displays.selected).or_else(||screens.initial_screen()).and_then(|s|s.assigned_track()).unwrap_or(0)}else{0};
        h.stats.select_video_track(self.selected_track);
        let restore=self.displays.restore_selection(&screens,catalog_current,control_ready);
        let has_frame=tracks.contains(&self.selected_track)||self.textures.contains_key(&self.selected_track);
        self.displays.confirm_initial_current(&screens,catalog_current,control_ready,has_frame);
        let remembered=self.displays.remember_display(&screens,catalog_current,has_frame);
        let confirmed_device_display=self.displays.ready() && catalog_current && has_frame && self.displays.selected==Some(screens.current_id);
        if remembered {h.stats.diagnostics.record(serde_json::json!({"event":"display_selection_confirmed","screen":screens.current_id}));}
        if let Some(id)=restore {h.stats.diagnostics.record(serde_json::json!({"event":"display_selection_restore","screen":id}));}
        if remembered {self.save_display_layout();}
        if let Some(id)=restore {self.select_native_display(id);}
        if let Some(id)=self.displays.queued_selection(){self.select_native_display(id);}
        if confirmed_device_display {
            // An explicit display tab has its own tab-order file, but the next
            // device connection still needs to reopen the last source the user
            // actually streamed. Persist that selection in the device layout.
            self.remember_device_display(&screens);
        }
    }
    pub(super) fn poll_displays(&mut self, tracks: &[u32]) {
        if self.supports_remote(uu_core::capabilities::Capability::Displays) {self.poll_native_displays(tracks);}
        let fps_changed=self.resolve_frame_rate();
        let local_size_changed=self.preferences.resolution==uu_core::client_settings::LOCAL_RESOLUTION
            && self.controller.as_ref().is_some_and(|h|h.stats.native_quality_request.get().is_some_and(|old|old.bounds!=self.native_quality_request().bounds));
        if fps_changed||local_size_changed {self.send_capture_settings();}
    }
    /// Resolve only the confirmed source. A pending switch must not change the
    /// old capture's quality using metadata for a different monitor.
    pub(super) fn resolve_frame_rate(&mut self)->bool {
        let refresh=self.confirmed_remote_refresh();
        let fps=self.preferences.resolved_fps_for_display(refresh,self.local_display(),self.client_support.max_fps);
        let changed=self.target_fps!=fps;
        self.target_fps=fps;
        if changed&&(self.preferences.follow_remote_fps||self.preferences.follow_local_fps) {self.fps_draft=fps;}
        // Persist the uncapped source rate, never the effective local limit.
        if self.preferences.follow_remote_fps&&!self.preferences.follow_local_fps {
            if let Some(refresh)=refresh.filter(|fps|*fps>=5) {self.preferences.fps=refresh.clamp(5,self.client_support.max_fps.clamp(5,240));}
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn catalog(current: i32) -> RemoteScreens {
        RemoteScreens {current_id: current, screens: (0..2).map(|id| uu_core::screen_control::RemoteScreen {
            id, display_name: format!("Source {id}"), refresh_hz: 60, kind: 0,
            width: 1920, height: 1080, dpi_scale: None, primary: id == 0,
            video_track_index: if id == current {0} else {-1},
        }).collect()}
    }
    fn explicit(id:i32,source:&str)->Displays {
        let mut displays=Displays::default();
        displays.set_initial_display(super::super::sessions::DisplayTarget{id,source:source.into()});
        displays
    }
    #[test]
    fn remote_frame_rate_follows_only_the_confirmed_display() {
        let mut screens=catalog(0);
        screens.screens[0].refresh_hz=144;screens.screens[1].refresh_hz=240;
        let mut displays=explicit(0,"Source 0");
        assert_eq!(displays.restore_selection(&screens,true,true),None);
        displays.confirm_initial_current(&screens,true,true,true);
        assert_eq!(displays.confirmed_refresh(&screens),Some(144));
        let request=displays.begin(1,0,Instant::now());
        assert_eq!(displays.confirmed_refresh(&screens),None);
        displays.response(request,11,0);
        assert_eq!(displays.confirmed_refresh(&screens),None);
        displays.frame(0);
        assert_eq!(displays.confirmed_refresh(&screens),Some(240));
        screens.screens.retain(|s|s.id==0);
        displays.reconcile_fixed_source(&screens);
        assert_eq!(displays.confirmed_refresh(&screens),None);
    }
    #[test]
    fn selected_target_can_be_pinned_before_the_first_frame_gate_opens() {
        let mut displays = Displays::default();
        displays.selected = Some(1);
        displays.blocked = true;
        assert!(!displays.ready());
        let target = displays.selected_target(&catalog(1)).unwrap();
        assert_eq!(target.id, 1);
        assert_eq!(target.source, "Source 1");
    }
    #[test]
    fn connection_failure_ends_preparation_and_reconnect_retains_the_fixed_source() {
        let mut view = explicit(1, "Source 1");
        assert!(view.preparing_initial_target() && !view.notice.is_empty());
        view.connection_failed("generated capture failure".into());
        assert!(!view.preparing_initial_target());
        assert_eq!(view.connection_error(), Some("generated capture failure"));
        assert!(view.notice.is_empty() && !view.ready() && view.waiting_binding());
        // Even a late matching catalog/frame cannot reopen input or show an
        // old image after the connection has failed.
        view.confirm_initial_current(&catalog(1), true, true, true);
        view.frame(0);
        assert!(!view.ready() && view.waiting_binding());
        let view = view.for_retirement();
        assert_eq!(view.connection_error(), Some("generated capture failure"));
        assert!(!view.preparing_initial_target() && view.notice.is_empty());
        let mut next = view.for_reconnect();
        assert!(next.connection_error().is_none() && next.preparing_initial_target());
        assert_eq!(next.fixed_display.as_ref().unwrap().source, "Source 1");
        assert_eq!(next.restore_selection(&catalog(1), true, true), None);
        next.confirm_initial_current(&catalog(1), true, true, true);
        assert!(next.ready());
    }
    #[test]
    fn persistent_virtual_reconnect_reuses_the_confirmed_source_and_temporary_recreates() {
        use super::super::sessions::{ConnectionTarget,VirtualDisplayTarget};
        use uu_core::native_displays::InitialRequest;
        let mut intent=VirtualDisplayTarget::new(Default::default());intent.persistent=true;
        let connection=ConnectionTarget::NativeVirtual{connection:Box::new(ConnectionTarget::NativeDevice("generated".into())),virtual_display:intent.clone()};
        let first=Displays::default().for_connection(&connection);
        assert!(first.awaiting_virtual());
        assert!(matches!(first.initial_request(&connection),Some(InitialRequest::VirtualPersistent{..})));
        let mut created=first;
        created.bind_created_virtual(super::super::sessions::DisplayTarget{id:1,source:"Source 1".into()});
        created.persistent_virtual=true;
        let mut next=created.for_connection(&connection);
        assert!(!next.awaiting_virtual() && !next.ready());
        assert_eq!(next.initial_request(&connection),Some(InitialRequest::Existing{id:1,source:"Source 1".into()}));
        assert_eq!(next.restore_selection(&catalog(0),true,true),Some(1));
        let mut missing=catalog(0);missing.screens.retain(|screen|screen.id!=1);
        next.reconcile_fixed_source(&missing);
        assert!(!next.ready());
        assert!(matches!(next.initial_request(&connection),Some(InitialRequest::Existing{..})),"a missing persistent source must never silently create another screen");
        // A successful lifetime change overrides the original creation intent.
        created.persistent_virtual=false;
        let temporary=created.for_connection(&connection);
        assert!(temporary.awaiting_virtual() && temporary.fixed_display.is_none());
        assert_eq!(connection.virtual_display().unwrap().id,intent.id);
    }
    #[test]
    fn fixed_view_reconnect_and_foreign_catalog_keep_the_same_source_and_frame_gate() {
        let mut view=explicit(1,"Source 1");
        assert_eq!(view.restore_selection(&catalog(1),true,true),None);
        view.confirm_initial_current(&catalog(1),true,true,true);
        assert!(view.ready());
        view.queued=Some(0);
        let mut restarted=view.for_reconnect();
        assert!(!restarted.ready() && restarted.queued.is_none());
        assert_eq!(restarted.restore_selection(&catalog(0),true,true),Some(1));
        assert!(restarted.permits_source(1,&catalog(0)));
        assert!(!restarted.permits_source(0,&catalog(0)));
        restarted.confirm_initial_current(&catalog(1),true,true,false);
        assert!(!restarted.ready());
        restarted.confirm_initial_current(&catalog(1),true,true,true);
        assert!(restarted.ready());
        restarted.reconcile_fixed_source(&catalog(0));
        assert!(!restarted.ready() && restarted.selected==Some(1));
        let mut missing=catalog(0);missing.screens.retain(|screen|screen.id!=1);
        restarted.reconcile_fixed_source(&missing);
        assert!(!restarted.ready() && restarted.selected.is_none());
        assert!(!restarted.permits_source(0,&missing));
        assert_eq!(restarted.restore_selection(&missing,true,true),None);
        assert_eq!(restarted.restore_selection(&catalog(1),true,true),None);
        restarted.confirm_initial_current(&catalog(1),true,true,true);
        assert!(restarted.ready() && restarted.selected==Some(1));
    }
    #[test]
    fn virtual_view_never_adopts_the_initial_physical_source_and_requires_its_created_source() {
        let mut d=Displays::default();
        d.layout.remember_selected(&catalog(0).screens[0]);
        d.await_virtual();
        assert!(!d.ready() && d.waiting_binding());
        assert_eq!(d.restore_selection(&catalog(0),true,true),None);
        d.confirm_initial_current(&catalog(0),true,true,true);
        assert!(!d.ready());
        assert!(!d.remember_display(&catalog(0),true,true));
        d.bind_created_virtual(super::super::sessions::DisplayTarget{id:1,source:"Source 1".into()});
        assert!(!d.awaiting_virtual() && !d.ready() && d.waiting_binding());
        assert_eq!(d.restore_selection(&catalog(0),true,true),Some(1));
        d.native_frame_epoch=1;d.native_request_epoch=1;
        let request=d.begin(1,0,Instant::now());
        d.response(request,11,0);
        assert!(!d.native_source_frame(&catalog(1),1,true));
        d.native_frame_epoch=2;
        assert!(!d.native_source_frame(&catalog(0),2,true));
        assert!(!d.native_source_frame(&catalog(1),2,false));
        assert!(d.native_source_frame(&catalog(1),2,true));
        assert!(d.ready() && !d.waiting_binding());
    }
    #[test]
    fn explicit_source_wins_over_saved_selection_and_waits_for_the_target_epoch() {
        let mut d=explicit(0,"Source 1");
        d.layout.remember_selected(&catalog(0).screens[0]);
        assert!(!d.ready());assert!(d.waiting_binding());assert!(d.preparing_initial_target());
        assert_eq!(d.restore_selection(&catalog(0),false,true),None);
        assert_eq!(d.restore_selection(&catalog(0),true,false),None);
        assert_eq!(d.restore_selection(&catalog(0),true,true),Some(1));
        assert_eq!(d.selected,Some(1));
        d.native_frame_epoch=4;d.native_request_epoch=4;
        let request=d.begin(1,0,Instant::now());
        d.response(request,11,0);
        assert!(!d.native_source_frame(&catalog(1),4,true));
        d.native_frame_epoch=5;
        assert!(!d.native_source_frame(&catalog(0),5,true)); // new epoch alone is insufficient
        assert!(!d.native_source_frame(&catalog(1),5,false));
        assert!(d.waiting_binding());
        assert!(d.native_source_frame(&catalog(1),5,true));
        assert!(!d.waiting_binding());
        assert!(!d.preparing_initial_target());
        // Confirmation retires startup, while the view's source remains fixed.
        assert!(!d.preparing_initial_target());
        assert!(d.permits_source(1,&catalog(1)));
        assert!(!d.permits_source(0,&catalog(1)));
        let mut missing=catalog(0);missing.screens.retain(|screen|screen.id!=1);
        assert!(!d.permits_source(0,&missing));
        assert!(!d.permits_source(1,&missing));
    }
    #[test]
    fn explicit_current_source_unlocks_only_after_current_catalog_control_and_frame() {
        let mut d=explicit(7,"Source 0");
        assert_eq!(d.restore_selection(&catalog(0),true,true),None);
        d.confirm_initial_current(&catalog(0),false,true,true);assert!(!d.ready());
        d.confirm_initial_current(&catalog(0),true,false,true);assert!(!d.ready());
        d.confirm_initial_current(&catalog(0),true,true,false);assert!(!d.ready());
        assert!(d.waiting_binding());
        d.confirm_initial_current(&catalog(0),true,true,true);
        assert!(d.ready());assert!(!d.waiting_binding());
        assert!(d.remember_display(&catalog(0),true,true));
    }
    #[test]
    fn missing_or_ambiguous_explicit_source_never_uses_the_numeric_fallback() {
        let mut d=explicit(0,"Missing");
        assert_eq!(d.restore_selection(&catalog(0),true,true),None);
        d.confirm_initial_current(&catalog(0),true,true,true);
        assert!(!d.ready());assert!(d.waiting_binding());assert!(uu_i18n::matches(&d.notice,"固定显示器不可用，正在等待原显示器恢复；也可关闭此标签"));
        assert_eq!(d.selected,None);
        let mut ambiguous=catalog(0);ambiguous.screens[1].display_name="Source 0".into();
        let mut d=explicit(0,"Source 0");
        assert_eq!(d.restore_selection(&ambiguous,true,true),None);
        assert!(!d.ready());assert!(d.waiting_binding());assert!(uu_i18n::matches(&d.notice,"固定显示器不可用，正在等待原显示器恢复；也可关闭此标签"));
        let mut id_only=explicit(1,"");
        assert_eq!(id_only.restore_selection(&catalog(0),true,true),Some(1));
    }
    #[test]
    fn independent_display_instances_keep_switches_and_confirmation_separate() {
        let mut left=explicit(0,"Source 0");
        let mut right=explicit(1,"Source 1");
        left.restore_selection(&catalog(0),true,true);
        left.confirm_initial_current(&catalog(0),true,true,true);
        assert!(left.ready());assert!(!right.ready());
        assert_eq!(right.restore_selection(&catalog(0),true,true),Some(1));
        right.native_frame_epoch=1;right.native_request_epoch=1;
        let right_request=right.begin(1,0,Instant::now());
        let left_request=left.begin(1,0,Instant::now());
        left.response(left_request,11,0);left.frame(0);
        assert!(left.ready());assert!(!right.ready());
        right.response(right_request,11,0);right.native_frame_epoch=2;
        assert!(right.native_source_frame(&catalog(1),2,true));
        left.begin(0,0,Instant::now());
        assert_eq!(left.selected,Some(0));assert!(!left.ready());
        assert_eq!(right.selected,Some(1));assert!(right.ready());
    }
    #[test]
    fn reconnect_restores_saved_source_once_after_current_catalog_and_control_are_ready() {
        let mut d = Displays::default();
        d.layout.remember_selected(&catalog(1).screens[1]);
        assert_eq!(d.restore_selection(&RemoteScreens::default(), true, true), None);
        assert_eq!(d.restore_selection(&catalog(0), false, true), None);
        assert_eq!(d.restore_selection(&catalog(0), true, false), None);
        assert_eq!(d.restore_selection(&catalog(0), true, true), Some(1));
        assert_eq!(d.restore_selection(&catalog(0), true, true), None);
        // A new connection reloads the saved source, regardless of the old
        // process's transient selected field or the host's default capture.
        let bytes = serde_json::to_vec(&d.layout).unwrap();
        let mut reconnected = Displays {layout: serde_json::from_slice(&bytes).unwrap(), ..Default::default()};
        assert_eq!(reconnected.restore_selection(&catalog(0), true, true), Some(1));
    }
    #[test]
    fn missing_saved_source_keeps_preference_until_a_new_choice_is_confirmed() {
        let mut d = Displays::default();
        d.layout.remember_selected(&catalog(1).screens[1]);
        let mut missing = catalog(0); missing.screens.truncate(1);
        d.selected = Some(0);
        assert_eq!(d.restore_selection(&missing, true, true), None);
        assert!(!d.remember_display(&missing, true, true));
        assert_eq!(d.layout.last_selected(), Some(1));
        d.begin(0, 0, Instant::now());
        d.pending.as_mut().unwrap().accepted = true;
        d.native_frame_epoch = 2;
        assert!(d.native_frame(2, true));
        assert!(d.remember_display(&missing, true, true));
        assert_eq!(d.layout.last_selected(), Some(0));
    }
    #[test]
    fn pending_rejected_and_stale_source_frames_do_not_overwrite_the_saved_display() {
        let mut d = Displays {native_frame_epoch: 4, native_request_epoch: 4, ..Default::default()};
        d.layout.remember_selected(&catalog(0).screens[0]);
        let request = d.begin(1, 0, Instant::now());
        assert!(!d.remember_display(&catalog(1), true, true));
        d.response(request, 11, 3);
        assert!(!d.remember_display(&catalog(1), true, true));
        let request = d.begin(1, 0, Instant::now());
        d.response(request, 11, 0);
        assert!(!d.native_frame(4, true));
        assert!(!d.remember_display(&catalog(1), true, true));
        d.native_frame_epoch = 5;
        assert!(!d.native_frame(4, true));
        assert!(!d.native_frame(5, false));
        assert!(d.native_frame(5, true));
        assert!(!d.remember_display(&catalog(1), false, true));
        assert!(!d.remember_display(&catalog(1), true, false));
        assert_eq!(d.layout.last_selected(), Some(0));
        assert!(d.remember_display(&catalog(1), true, true));
        assert_eq!(d.layout.last_selected(), Some(1));
    }
    #[test]
    fn native_switch_requires_acceptance_inventory_and_image_of_the_new_generation(){
        let mut d=Displays::default();d.native_frame_epoch=4;d.native_request_epoch=4;
        d.begin(7,0,Instant::now());
        d.pending.as_mut().unwrap().accepted=true;
        assert!(!d.native_frame(4,true)); // old source, same size and track
        d.native_frame_epoch=5;
        assert!(!d.native_frame(4,true)); // late old inventory
        assert!(!d.native_frame(5,false)); // negotiated but no image
        assert!(d.native_frame(5,true));
    }
    #[test]
    fn rapid_selections_wait_for_current_confirmation_and_keep_only_latest() {
        let mut d = Displays::default();
        assert!(!d.defer_selection(7));
        let request = d.begin_activation(42, Instant::now());
        assert!(d.defer_selection(7));
        assert!(d.defer_selection(9));
        assert_eq!(d.selected, Some(42));
        assert_eq!(d.queued_selection(), None);
        d.response(request, 19, 0);
        d.bind_track(0);
        d.expect_settings(2001);
        d.frame(0);
        assert_eq!(d.queued_selection(), None);
        d.capture_response(2001, true);
        assert_eq!(d.queued_selection(), Some(9));
        assert_eq!(d.queued_selection(), None);
        d.begin_activation(7, Instant::now());
        d.defer_selection(42);
        d.defer_selection(7); // clicking the in-flight target cancels the queued choice
        d.tick(Instant::now() + Duration::from_secs(9));
        assert_eq!(d.queued_selection(), None);
    }
    #[test]
    fn failed_activation_keeps_binding_uncertain_before_queued_retry() {
        let mut d = Displays::default();
        let request = d.begin_activation(42, Instant::now());
        d.defer_selection(7);
        d.response(request, 19, 3);
        assert_eq!(d.queued_selection(), Some(7));
        assert!(d.waiting_binding());
        assert!(!d.ready());
    }
    #[test]
    fn switching_needs_matching_reply_and_new_target_frame() {
        let mut d = Displays::default();
        let now = Instant::now();
        let id = d.begin(42, 3, now);
        d.response(id - 1, 11, 0);
        d.frame(0);
        assert!(!d.ready());
        d.frame(3);
        assert!(!d.ready());
        d.response(id, 11, 0);
        assert!(d.ready());
        assert_eq!(d.selected, Some(42));
        let id = d.begin(7, 0, now);
        d.response(id, 11, 0);
        assert!(!d.ready());
        d.frame(0);
        assert!(d.ready());
    }
    #[test]
    fn rejection_timeout_and_old_replies_never_enable_input() {
        let mut d = Displays::default();
        let now = Instant::now();
        let first = d.begin(42, 3, now);
        d.response(first, 11, 9);
        d.frame(3);
        assert!(!d.ready());
        assert!(d.notice.contains('9'));
        let second = d.begin(7, 0, now);
        d.response(first, 11, 0);
        d.frame(0);
        assert!(!d.ready());
        d.tick(now + Duration::from_secs(9));
        d.response(second, 11, 0);
        assert!(!d.ready());
        d.begin(42, 3, now);
        assert_ne!(d.pending.as_ref().unwrap().request, first);
    }
    #[test]
    fn source_settings_acknowledgement_is_required_before_input_resumes() {
        let mut d = Displays::default();
        let request = d.begin(42, 2, Instant::now());
        d.expect_settings(2001);
        d.response(request, 11, 0);
        d.frame(2);
        assert!(!d.ready());
        d.capture_response(2000, true);
        assert!(!d.ready());
        d.capture_response(2001, true);
        assert!(d.ready());
        let request = d.begin(7, 0, Instant::now());
        d.expect_settings(2002);
        d.response(request, 11, 0);
        d.frame(0);
        d.capture_response(2002, false);
        assert!(!d.ready());
    }
    #[test]
    fn unallocated_source_waits_for_activation_binding_settings_and_fresh_frame() {
        let mut d = Displays::default();
        let id = d.begin_activation(42, Instant::now());
        assert!(d.waiting_binding());
        d.frame(0);
        d.response(id, 11, 0);
        assert!(!d.ready());
        d.response(id, 19, 0);
        assert!(!d.ready());
        d.bind_track(7);
        d.expect_settings(2001);
        d.frame(0);
        d.capture_response(2001, true);
        assert!(!d.ready());
        d.frame(7);
        assert!(d.ready());
        let id = d.begin_activation(7, Instant::now());
        d.response(id, 19, 3);
        assert!(!d.ready());
        assert!(d.waiting_binding());
        d.begin_activation(42, Instant::now());
        d.tick(Instant::now() + Duration::from_secs(9));
        assert!(d.waiting_binding());
        assert!(!d.ready());
    }
    #[test]
    fn cached_profile_requires_matching_ack_and_source_binding() {
        let mut d = Displays::default();
        let profile = (60, 0, 0, None, false);
        d.bindings.insert(2, 42);
        d.settings_sent(2001, 42, 2, profile);
        d.capture_response(2000, true);
        assert!(!d.settings_match(42, 2, profile));
        d.capture_response(2001, true);
        assert!(d.settings_match(42, 2, profile));
        assert!(!d.settings_match(42, 0, profile));
        assert!(!d.settings_match(42, 2, (120, 0, 0, None, false)));
        let id = d.begin(42, 2, Instant::now());
        d.frame(2);
        d.response(id, 11, 0);
        assert!(d.ready());
        d.settings_sent(2002, 42, 2, profile);
        d.bindings.insert(2, 7);
        d.profiles.clear();
        d.capture_response(2002, true);
        assert!(!d.settings_match(42, 2, profile));
    }
}
