//! A display is identified by the remote source ID, never packet arrival order.
use eframe::egui;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use uu_core::screen_control::RemoteScreens;

pub(super) type CaptureProfile = (u32, usize, u32, usize, bool);

#[derive(Default)]
pub(super) struct Displays {
    pub native_frame_epoch:u64,
    native_request_epoch:u64,
    pub selected: Option<i32>,
    pending: Option<Pending>,
    queued: Option<i32>,
    initial_selection_checked: bool,
    remember_on_confirmation: Option<i32>,
    blocked: bool,
    unbound: bool,
    pub notice: String,
    pub menu_open: bool,
    pub anchor: egui::Pos2,
    pub layout: super::display_layout::DisplayLayout,
    pub layout_path: Option<std::path::PathBuf>,
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
    fn restore_selection(&mut self, screens: &RemoteScreens, catalog_current: bool, control_ready: bool) -> Option<i32> {
        if self.initial_selection_checked || self.pending.is_some() || !catalog_current || !control_ready || screens.screens.is_empty() {return None;}
        self.initial_selection_checked = true;
        self.layout.remembered(&screens.screens).filter(|id| *id != screens.current_id)
    }
    fn remember_display(&mut self, screens: &RemoteScreens, catalog_current: bool, has_frame: bool) -> bool {
        if !self.ready() || !catalog_current || !has_frame || self.selected != Some(screens.current_id) {return false;}
        if self.remember_on_confirmation != self.selected && self.layout.last_selected().is_some() {return false;}
        self.remember_on_confirmation = None;
        screens.selected().is_some_and(|screen| self.layout.remember_selected(screen))
    }
    pub fn ready(&self) -> bool {
        !self.blocked && self.pending.is_none()
    }
    pub fn switching(&self) -> bool {
        self.pending.is_some()
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
        self.unbound
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
            self.blocked = false;
            self.notice.clear();
        }
    }
    fn native_frame(&mut self,catalog_epoch:u64,has_frame:bool)->bool{
        let fresh=self.native_frame_epoch!=self.native_request_epoch && catalog_epoch==self.native_frame_epoch;
        if fresh && has_frame {if let Some(p)=self.pending.as_mut(){p.frame=true;}self.complete();}
        self.ready()
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
    pub(super) fn prepare_displays(&mut self) {
        self.displays = Displays::default();
        self.selected_track = 0;
        let Some(path) = self.config.as_ref().zip(self.stream_target.as_ref())
            .and_then(|(config, target)| super::display_layout::DisplayLayout::path(config, target)) else {return;};
        match super::display_layout::DisplayLayout::load(&path) {
            Ok(layout) => {self.displays.layout = layout; self.displays.layout_path = Some(path);}
            // Preserve an unreadable original instead of overwriting it when
            // the first catalog arrives with an otherwise empty layout.
            Err(error) => self.set_settings_notice(uu_i18n::format!("显示器排序读取失败：{error}")),
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
    pub(super) fn reorder_display_tab(&mut self, source: i32, target: i32, after: bool) {
        if self.displays.layout.move_tab(source, target, after) {
            self.save_display_layout();
        }
    }
    pub(super) fn display_label(&self) -> String {
        self.displays
            .selected
            .and_then(|id| self.displays.layout.number(id))
            .map(|number| uu_i18n::format!("显示屏 {number}"))
            .unwrap_or_else(|| uu_i18n::text("显示器").into())
    }
    pub(super) fn select_remote_screen(&mut self,id:i32){self.select_native_display(id);}
    fn select_native_display(&mut self,id:i32){
        let screens=self.remote_screens();
        if !screens.screens.iter().any(|s|s.id==id){return;}
        self.displays.initial_selection_checked=true;
        if self.displays.defer_selection(id){return;}
        if self.displays.selected==Some(id) && self.displays.ready(){return;}
        if !self.controller.as_ref().is_some_and(|h|h.stats.control_open.load(std::sync::atomic::Ordering::Acquire)!=0){return;}
        self.release_all_keys();self.release_pointer_capture(&self.context.clone());
        self.cursor=super::cursor::Cursor::default();
        let binding=self.controller.as_ref().is_some_and(|h|h.stats.display_track_binding.load(std::sync::atomic::Ordering::Acquire));
        let track=screens.screens.iter().find(|s|s.id==id).and_then(|s|s.assigned_track()).unwrap_or(0);
        let request=self.displays.begin(id,track,Instant::now());
        if binding {self.textures.remove(&track);self.unpresented.remove(&track);}
        self.displays.native_request_epoch=self.displays.native_frame_epoch;
        let sent=self.controller.as_ref().and_then(|h|h.native_input.as_ref()).is_some_and(|input|input.select_display(request,id).is_ok());
        if !sent {
            self.displays.pending=None;self.displays.blocked=false;
            self.displays.remember_on_confirmation=None;
            self.displays.selected=Some(screens.current_id);
            self.displays.notice=uu_i18n::text("切换失败：控制连接不可用").into();
        }
        self.displays.menu_open=false;self.stream_menu.open=false;
        self.context.memory_mut(|m|m.close_popup());
    }
    fn poll_native_displays(&mut self,tracks:&[u32]){
        let screens=self.remote_screens();
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
                self.displays.pending=None;self.displays.blocked=false;
                if !response.accepted {self.displays.remember_on_confirmation=None;}
                self.displays.selected=Some(screens.current_id);
                self.displays.notice=if response.accepted{String::new()}else{uu_i18n::text("被控端未允许切换到该显示器").into()};
            }
        }
        if self.displays.pending.is_some(){
            // Acknowledgment alone is insufficient: require the catalog and an
            // actual frame from the new media generation, even at identical size.
            let bound=screens.screens.iter().find(|s|Some(s.id)==self.displays.selected).and_then(|s|s.assigned_track());
            let ready=if binding {self.displays.pending.as_ref().is_some_and(|p|p.accepted)&&bound.is_some_and(|track|tracks.contains(&track))}
                else {self.displays.native_frame(catalog_epoch,tracks.contains(&0)||self.textures.contains_key(&0))};
            if ready {
                if binding {self.displays.pending=None;self.displays.blocked=false;self.displays.notice.clear();}
                if !binding {self.displays.selected=Some(screens.current_id);}
            }
        }else if catalog_current && !screens.screens.is_empty(){
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
        let remembered=self.displays.remember_display(&screens,catalog_current,has_frame);
        if remembered {h.stats.diagnostics.record(serde_json::json!({"event":"display_selection_confirmed","screen":screens.current_id}));}
        if let Some(id)=restore {h.stats.diagnostics.record(serde_json::json!({"event":"display_selection_restore","screen":id}));}
        if remembered {self.save_display_layout();}
        if let Some(id)=restore {self.select_native_display(id);}
        if let Some(id)=self.displays.queued_selection(){self.select_native_display(id);}
    }
    pub(super) fn poll_displays(&mut self, tracks: &[u32]) {
        self.poll_native_displays(tracks);
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
        let profile = (60, 0, 0, 0, false);
        d.bindings.insert(2, 42);
        d.settings_sent(2001, 42, 2, profile);
        d.capture_response(2000, true);
        assert!(!d.settings_match(42, 2, profile));
        d.capture_response(2001, true);
        assert!(d.settings_match(42, 2, profile));
        assert!(!d.settings_match(42, 0, profile));
        assert!(!d.settings_match(42, 2, (120, 0, 0, 0, false)));
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
