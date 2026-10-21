//! User intent survives temporary UI/focus/control-channel interruptions.
#[derive(Default)]
pub struct Capture {
    requested: bool,
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
    pub fn pointer(&self, conditions: Conditions, over_local_controls: bool) -> bool {
        self.active(conditions) && !over_local_controls
    }
    pub fn system(&self, conditions: Conditions, fullscreen: bool, enabled: bool) -> bool {
        self.active(conditions) && fullscreen && enabled
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
}
