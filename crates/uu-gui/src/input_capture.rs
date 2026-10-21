//! User intent survives temporary UI/focus/control-channel interruptions.
#[derive(Default)]
pub struct Capture {
    requested: bool,
    was_active: bool,
}
#[derive(Clone, Copy)]
pub struct Conditions {
    pub ready: bool,
    pub focused: bool,
    pub view_only: bool,
    pub local_ui: bool,
}
impl Capture {
    pub fn request(&mut self) {
        self.requested = true;
    }
    pub fn release(&mut self) {
        self.requested = false;
    }
    pub fn toggle(&mut self) {
        self.requested = !self.requested;
    }
    pub fn requested(&self) -> bool {
        self.requested
    }
    pub fn active(&self, conditions: Conditions) -> bool {
        self.requested
            && conditions.ready
            && conditions.focused
            && !conditions.view_only
            && !conditions.local_ui
    }
    /// Release on a real stop, never when only the OS capture method changes.
    pub fn stopped(&mut self, conditions: Conditions) -> bool {
        let active = self.active(conditions);
        let stopped = self.was_active && !active;
        self.was_active = active;
        stopped
    }
    pub fn pointer(&self, conditions: Conditions, over_local_controls: bool) -> bool {
        self.active(conditions) && !over_local_controls
    }
    pub fn system(&self, conditions: Conditions, fullscreen: bool, enabled: bool) -> bool {
        self.active(conditions) && fullscreen && enabled
    }
}
/// Only this session's forwarded downs may produce remote releases.
/// Repeated downs are allowed; duplicate/unowned ups are discarded.
pub struct RemoteKeys {
    down: [bool; 256],
}
impl Default for RemoteKeys {
    fn default() -> Self {
        Self { down: [false; 256] }
    }
}
impl RemoteKeys {
    pub fn event(&mut self, vk: u32, pressed: bool) -> bool {
        let Some(down) = self.down.get_mut(vk as usize) else {
            return false;
        };
        let held = std::mem::replace(down, pressed);
        pressed || held
    }
    pub fn held(&self) -> Vec<u32> {
        let mut keys: Vec<_> = self
            .down
            .iter()
            .enumerate()
            .filter_map(|(vk, &down)| down.then_some(vk as u32))
            .collect();
        // Raise ordinary keys first, then their modifiers, as a chord unwinds.
        keys.sort_by_key(|vk| matches!(*vk,0x10..=0x12|0x5b|0x5c|0xa0..=0xa5));
        keys
    }
    pub fn is_empty(&self) -> bool {
        !self.down.iter().any(|&down| down)
    }
    pub fn clear(&mut self) {
        self.down.fill(false);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    const READY: Conditions = Conditions {
        ready: true,
        focused: true,
        view_only: false,
        local_ui: false,
    };
    #[test]
    fn fullscreen_requests_capture_even_after_explicit_release() {
        let mut capture = Capture::default();
        assert!(!capture.system(READY, true, true));
        // Entering fullscreen and activating the picture use the same request.
        capture.request();
        assert!(capture.system(READY, true, true));
        assert!(capture.active(READY));
        assert!(!capture.system(READY, false, true));
        assert!(!capture.system(READY, true, false));
        capture.release();
        capture.request();
        assert!(capture.system(READY, true, true));
    }
    #[test]
    fn menu_close_focus_return_and_control_recovery_restore_capture() {
        let mut capture = Capture::default();
        capture.request();
        for paused in [
            Conditions {
                local_ui: true,
                ..READY
            },
            Conditions {
                focused: false,
                ..READY
            },
            Conditions {
                ready: false,
                ..READY
            },
            Conditions {
                view_only: true,
                ..READY
            },
        ] {
            assert!(!capture.active(paused));
            assert!(!capture.system(paused, true, true));
            assert!(capture.requested());
            assert!(capture.system(READY, true, true));
        }
        // Overlapping pauses: closing a menu while unfocused does not capture.
        assert!(!capture.system(
            Conditions {
                focused: false,
                local_ui: true,
                ..READY
            },
            true,
            true
        ));
        assert!(!capture.system(
            Conditions {
                focused: false,
                ..READY
            },
            true,
            true
        ));
        assert!(capture.system(READY, true, true));
    }
    #[test]
    fn explicit_release_is_not_undone_by_temporary_ui_transitions() {
        let mut capture = Capture::default();
        capture.request();
        capture.toggle();
        for conditions in [
            READY,
            Conditions {
                local_ui: true,
                ..READY
            },
            Conditions {
                focused: false,
                ..READY
            },
            Conditions {
                ready: false,
                ..READY
            },
            READY,
        ] {
            assert!(!capture.active(conditions));
            assert!(!capture.requested());
        }
        capture.toggle();
        assert!(capture.system(READY, true, true));
        capture.release();
        assert!(!capture.active(READY));
    }
    #[test]
    fn leaving_fullscreen_releases_system_capture_but_preserves_remote_typing() {
        let mut capture = Capture::default();
        capture.request();
        assert!(capture.system(READY, true, true));
        assert!(!capture.system(READY, false, true));
        assert!(capture.active(READY));
        assert!(capture.pointer(READY, false));
        // The setting controls system keys, not ordinary remote text entry.
        assert!(!capture.system(READY, true, false));
        assert!(capture.active(READY));
    }
    #[test]
    fn pointer_hover_cannot_change_keyboard_ownership() {
        let mut capture = Capture::default();
        capture.request();
        // Hover is a pointer-only gate. It is deliberately absent from Conditions.
        for pointer_over_controls in [false, true, true, false] {
            assert_eq!(
                capture.pointer(READY, pointer_over_controls),
                !pointer_over_controls
            );
            assert!(capture.active(READY));
            assert!(capture.system(READY, true, true));
        }
    }
    #[test]
    fn modifier_stays_down_across_repeated_chords_until_its_real_release() {
        for modifier in [0x5b, 0x5c, 0xa4, 0xa5] {
            let mut keys = RemoteKeys::default();
            assert!(keys.event(modifier, true));
            for key in [0x25, 0x27, 9, 9, 0x25] {
                assert!(keys.event(key, true));
                assert!(keys.event(key, false));
                assert!(!keys.event(key, false));
                assert_eq!(keys.held(), vec![modifier]);
            }
            assert!(keys.event(modifier, false));
            assert!(!keys.event(modifier, false));
            assert!(keys.is_empty());
        }
    }
    #[test]
    fn release_all_has_no_unowned_or_duplicate_modifier_ups() {
        let mut keys = RemoteKeys::default();
        assert!(keys.held().is_empty());
        assert!(!keys.event(0x5b, false));
        keys.event(0xa4, true);
        keys.event(9, true);
        keys.event(0x5b, true);
        let release = keys.held();
        assert_eq!(release.first(), Some(&9));
        for key in release {
            assert!(keys.event(key, false));
            assert!(!keys.event(key, false));
        }
        assert!(keys.held().is_empty());
        assert!(!keys.event(0x5b, false));
        assert!(!keys.event(0xa4, false));
    }
    #[test]
    fn changing_system_capture_or_resuming_input_does_not_release_held_keys() {
        let mut capture = Capture::default();
        capture.request();
        assert!(!capture.stopped(READY));
        for fullscreen in [false, true, true, false, true] {
            let _ = capture.system(READY, fullscreen, true);
            assert!(!capture.stopped(READY));
        }
        let paused = Conditions {
            focused: false,
            ..READY
        };
        assert!(capture.stopped(paused));
        assert!(!capture.stopped(paused));
        assert!(!capture.stopped(READY));
        capture.release();
        assert!(capture.stopped(READY));
        assert!(!capture.stopped(READY));
    }
}
