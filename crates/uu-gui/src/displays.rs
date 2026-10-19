//! A display is identified by the remote source ID, never packet arrival order.
use eframe::egui;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use uu_core::screen_control::RemoteScreens;

pub(super) type CaptureProfile = (u32, usize, u32, usize, bool);

#[derive(Default)]
pub(super) struct Displays {
    pub selected: Option<i32>,
    pending: Option<Pending>,
    queued: Option<i32>,
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
        self.notice = "正在切换显示器…".into();
        request
    }
    fn begin_activation(&mut self, id: i32, now: Instant) -> u64 {
        let request = self.begin(id, 0, now);
        let p = self.pending.as_mut().unwrap();
        p.track = None;
        p.activation = true;
        self.unbound = true;
        self.notice = "正在切换采集源…".into();
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
            self.blocked = true;
            self.notice = format!("切换失败（远端错误 {code}），请重新选择显示器");
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
            self.blocked = true;
            self.notice = "远端拒绝了目标显示器设置，请重新选择".into();
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
                        "等待远端切屏确认"
                    } else if p.track.is_none() {
                        "等待采集源通知"
                    } else if !p.settings_accepted {
                        "等待画质确认"
                    } else {
                        "等待目标画面"
                    }
                })
                .unwrap_or("未完成");
            self.pending = None;
            self.blocked = true;
            self.notice = format!("显示器切换超时（{stage}），可重试或切回其它显示器");
        }
    }
}

impl super::App {
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
                self.settings_notice = format!("显示器排序保存失败：{error}");
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
            .map(|number| format!("显示屏 {number}"))
            .unwrap_or_else(|| "显示器".into())
    }
    pub(super) fn select_remote_screen(&mut self, id: i32) {
        let screens = self.remote_screens();
        let Some(screen) = screens.screens.iter().find(|s| s.id == id).cloned() else {
            return;
        };
        if self.displays.defer_selection(id) {
            self.displays.menu_open = false;
            self.stream_menu.open = false;
            self.context.memory_mut(|m| m.close_popup());
            return;
        }
        // An activation timeout/rejection leaves the old advertised track
        // uncertain. Explicitly select the requested source before trusting it.
        let target = screen.assigned_track().filter(|_| !self.displays.waiting_binding());
        if self.displays.selected == Some(id)
            && self.displays.ready()
            && target.is_some_and(|track| self.textures.contains_key(&track))
        {
            return;
        }
        if !self.controller.as_ref().is_some_and(|h| {
            h.stats
                .control_open
                .load(std::sync::atomic::Ordering::Acquire)
                != 0
        }) {
            return;
        }
        // Release against the old source before changing its input binding.
        self.release_all_keys();
        self.release_pointer_capture(&self.context.clone());
        self.remote_focus = false;
        self.cursor = super::cursor::Cursor::default();
        self.selected_track = target.unwrap_or(self.selected_track);
        if let Some(h) = &self.controller {
            h.stats.select_video_track(self.selected_track);
        }
        let reuse = target.is_some_and(|t| self.displays.bindings.get(&t) == Some(&id));
        let warm = reuse
            && self.textures.contains_key(&self.selected_track)
            && self
                .displays
                .last_frames
                .get(&self.selected_track)
                .is_some_and(|t| t.elapsed() < Duration::from_millis(500));
        if !reuse {
            self.textures.remove(&self.selected_track);
            self.unpresented.remove(&self.selected_track);
        }
        let settings_match = target.is_some_and(|track| {
            self.displays
                .settings_match(id, track, self.capture_profile())
        });
        let request = if let Some(track) = target {
            self.displays.begin(id, track, Instant::now())
        } else {
            self.displays.begin_activation(id, Instant::now())
        };
        if warm {
            self.displays.pending.as_mut().unwrap().frame = true;
        }
        let command = target
            .map(|track| uu_core::screen_control::send_video_track(request, track))
            .unwrap_or_else(|| uu_core::screen_control::activate_screen(request, id));
        if let Some(h) = &self.controller {
            h.stats.diagnostics.record(serde_json::json!({"event":"select_display","source":id,"track":target,"request":request,"activation":target.is_none()}));
        }
        if self
            .controller
            .as_ref()
            .unwrap()
            .input
            .send(command)
            .is_err()
        {
            self.displays.pending = None;
            self.displays.notice = "切换失败：连接已关闭".into();
        }
        if target.is_some() && !settings_match {
            self.send_capture_settings_through(true);
        }
        self.displays.menu_open = false;
        self.stream_menu.open = false;
        self.context.memory_mut(|m| m.close_popup());
    }
    pub(super) fn poll_displays(&mut self, tracks: &[u32]) {
        let received = self.controller.as_ref().is_some_and(|h| {
            h.stats
                .remote_screens_revision
                .load(std::sync::atomic::Ordering::Acquire)
                != 0
        });
        let screens = self.remote_screens();
        if self.displays.layout.sync(&screens.screens) {
            self.save_display_layout();
        }
        let bindings: HashMap<_, _> = screens
            .screens
            .iter()
            .filter_map(|s| s.assigned_track().map(|t| (t, s.id)))
            .collect();
        for (track, source) in &self.displays.bindings {
            if bindings.get(track) != Some(source) {
                self.textures.remove(track);
                self.unpresented.remove(track);
                self.displays.last_frames.remove(track);
            }
        }
        self.displays
            .profiles
            .retain(|id, (track, _)| bindings.get(track) == Some(id));
        self.displays.bindings = bindings;
        if received && screens.screens.is_empty() {
            if self.displays.selected.is_some() {
                self.release_all_keys();
                self.release_pointer_capture(&self.context.clone());
            }
            self.displays.selected = None;
            self.displays.pending = None;
            self.displays.queued = None;
            self.displays.blocked = true;
            self.displays.notice = "远端当前没有可用显示器，等待重新连接显示器…".into();
            self.remote_focus = false;
            self.textures.clear();
        }
        if !screens.screens.is_empty() {
            let selected = self
                .displays
                .selected
                .and_then(|id| screens.screens.iter().find(|s| s.id == id));
            if selected.is_none() {
                let next = screens.initial_screen().unwrap();
                let previous = self.displays.selected;
                if previous.is_some() {
                    self.release_all_keys();
                    self.release_pointer_capture(&self.context.clone());
                    self.remote_focus = false;
                }
                self.displays.selected = Some(next.id);
                self.selected_track = next.assigned_track().unwrap_or(self.selected_track);
                if previous.is_some() || self.displays.blocked {
                    // Invalidate cached frames when an existing source vanished.
                    self.displays.blocked = true;
                    self.select_remote_screen(next.id);
                }
            } else if let Some(screen) = selected {
                if let Some(track) = screen.assigned_track() {
                    if self.displays.waiting_binding() && self.displays.switching() {
                        self.displays.bind_track(track);
                        self.selected_track = track;
                        self.textures.remove(&track);
                        self.unpresented.remove(&track);
                        self.send_capture_settings_through(true);
                    } else if self.selected_track != track {
                        self.select_remote_screen(screen.id);
                    }
                }
            }
        }
        if let Some(h) = &self.controller {
            h.stats.select_video_track(self.selected_track);
            let response = h
                .stats
                .settings_response
                .load(std::sync::atomic::Ordering::Acquire);
            self.displays
                .capture_response(response >> 1, response & 1 != 0);
            for (id, case, code) in h.stats.display_responses.lock().unwrap().drain(..) {
                self.displays.response(id, case, code);
            }
        }
        for track in tracks {
            self.displays.last_frames.insert(*track, Instant::now());
            self.displays.frame(*track);
        }
        let was_switching = self.displays.switching();
        self.displays.tick(Instant::now());
        if was_switching && !self.displays.switching() && self.displays.blocked {
            if let Some(h) = &self.controller {
                h.stats.diagnostics.record(serde_json::json!({"event":"switch_timeout","source":self.displays.selected,"track":self.selected_track,"stage":self.displays.notice}));
            }
        }
        if let Some(id) = self.displays.queued_selection() {
            self.select_remote_screen(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
