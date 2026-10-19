//! A display is identified by the remote source ID, never packet arrival order.
use eframe::egui;
use std::time::{Duration, Instant};
use uu_core::screen_control::{RemoteScreen, RemoteScreens};

#[derive(Default)]
pub(super) struct Displays {
    pub selected: Option<i32>,
    pending: Option<Pending>,
    blocked: bool,
    pub notice: String,
    pub menu_open: bool,
    pub anchor: egui::Pos2,
    next_request: u64,
}
struct Pending {
    request: u64,
    track: u32,
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
    fn begin(&mut self, id: i32, track: u32, now: Instant) -> u64 {
        self.next_request = self.next_request.wrapping_add(1);
        let request = 10_000 + self.next_request;
        self.selected = Some(id);
        self.blocked = true;
        self.pending = Some(Pending {
            request,
            track,
            sent: now,
            accepted: false,
            frame: false,
            settings_request: None,
            settings_accepted: true,
        });
        self.notice = "正在切换显示器…".into();
        request
    }
    fn frame(&mut self, track: u32) {
        if let Some(p) = self.pending.as_mut().filter(|p| p.track == track) {
            p.frame = true;
        }
        self.complete();
    }
    fn response(&mut self, id: u64, case: u32, code: i32) {
        if case != 11 || !self.pending.as_ref().is_some_and(|p| p.request == id) {
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
    fn capture_response(&mut self, id: u64, accepted: bool) {
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
            self.pending = None;
            self.blocked = true;
            self.notice = "显示器切换未完成，可重新选择或切回其它显示器".into();
        }
    }
}

pub(super) fn label(screens: &RemoteScreens, screen: &RemoteScreen) -> String {
    let number = screens
        .screens
        .iter()
        .position(|s| s.id == screen.id)
        .unwrap_or(0)
        + 1;
    if screen.display_name.is_empty() {
        format!("显示屏 {number}")
    } else {
        format!("显示屏 {number}（{}）", screen.display_name)
    }
}
impl super::App {
    pub(super) fn remote_screens(&self) -> RemoteScreens {
        self.controller
            .as_ref()
            .map(|h| h.stats.remote_screens.lock().unwrap().clone())
            .unwrap_or_default()
    }
    pub(super) fn display_label(&self) -> String {
        let screens = self.remote_screens();
        self.displays
            .selected
            .and_then(|id| screens.screens.iter().position(|s| s.id == id))
            .map(|i| format!("显示屏 {}", i + 1))
            .unwrap_or_else(|| "显示器".into())
    }
    pub(super) fn select_remote_screen(&mut self, id: i32) {
        let screens = self.remote_screens();
        let Some(screen) = screens.screens.iter().find(|s| s.id == id).cloned() else {
            return;
        };
        if screen.video_track_index >= 5 {
            self.displays.notice = "此显示器超出当前支持的5路视频轨道范围".into();
            return;
        }
        if self.displays.selected == Some(id)
            && self.displays.ready()
            && self.textures.contains_key(&screen.video_track_index)
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
        self.selected_track = screen.video_track_index;
        if let Some(h) = &self.controller {
            h.stats.select_video_track(self.selected_track);
        }
        self.textures.remove(&self.selected_track);
        self.unpresented.remove(&self.selected_track);
        self.frame_count.remove(&self.selected_track);
        self.fps.remove(&self.selected_track);
        let request = self
            .displays
            .begin(id, screen.video_track_index, Instant::now());
        let sent = self.controller.as_ref().unwrap().input.send(
            uu_core::screen_control::send_video_track(request, screen.video_track_index),
        );
        if sent.is_err() {
            self.displays.pending = None;
            self.displays.notice = "切换失败：连接已关闭".into();
        }
        // Apply the saved quality/FPS to this source only. Send alongside the
        // track request instead of waiting in the coalesced settings timer.
        self.send_capture_settings_through(true);
        self.displays.menu_open = false;
        self.stream_menu.open = false;
        self.context.memory_mut(|m| m.close_popup());
        self.window_sizing.begin_stream();
    }
    pub(super) fn poll_displays(&mut self, tracks: &[u32]) {
        let received = self.controller.as_ref().is_some_and(|h| {
            h.stats
                .remote_screens_revision
                .load(std::sync::atomic::Ordering::Acquire)
                != 0
        });
        let screens = self.remote_screens();
        if received && screens.screens.is_empty() {
            if self.displays.selected.is_some() {
                self.release_all_keys();
                self.release_pointer_capture(&self.context.clone());
            }
            self.displays.selected = None;
            self.displays.pending = None;
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
                let next = screens
                    .selected()
                    .or_else(|| screens.screens.iter().find(|s| s.primary))
                    .unwrap_or(&screens.screens[0]);
                let previous = self.displays.selected;
                if previous.is_some() {
                    self.release_all_keys();
                    self.release_pointer_capture(&self.context.clone());
                    self.remote_focus = false;
                }
                self.displays.selected = Some(next.id);
                self.selected_track = next.video_track_index;
                if previous.is_some() || self.displays.blocked {
                    // Invalidate cached frames when an existing source vanished.
                    self.displays.blocked = true;
                    self.select_remote_screen(next.id);
                }
            } else if let Some(screen) = selected {
                if self.selected_track != screen.video_track_index {
                    self.select_remote_screen(screen.id);
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
            self.displays.frame(*track);
        }
        self.displays.tick(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
