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
    /// Intent is independent of the remote grant: a waiting viewer must keep
    /// requesting control, while focus/UI changes immediately relinquish it.
    pub fn wants_control(&self, conditions: Conditions) -> bool {
        self.requested && conditions.focused && !conditions.view_only && !conditions.local_ui
    }
    pub fn active(&self, conditions: Conditions) -> bool {
        conditions.ready && self.wants_control(conditions)
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
/// UU's host expires a held key unless it keeps receiving the local typematic
/// downs, unlike hosts that own key-repeat timing themselves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyChange {
    None,
    Press,
    Repeat,
    Release,
}
const MODIFIER_REPEAT_INTERVAL: std::time::Duration=std::time::Duration::from_millis(30);
fn is_modifier(vk:u32)->bool {matches!(vk,0x10..=0x12|0x5b|0x5c|0xa0..=0xa5)}
pub struct RemoteKeys {
    down: [bool; 256],
    last_modifier_repeat: Option<std::time::Instant>,
    forwarded_repeats: u64,
}
impl Default for RemoteKeys {
    fn default() -> Self {
        Self { down: [false; 256], last_modifier_repeat: None, forwarded_repeats: 0 }
    }
}
impl RemoteKeys {
    pub fn event(&mut self, vk: u32, pressed: bool) -> KeyChange {
        let Some(down) = self.down.get_mut(vk as usize) else {
            return KeyChange::None;
        };
        match (*down, pressed) {
            (false, true) => {
                *down=true;
                if is_modifier(vk) {self.last_modifier_repeat=Some(std::time::Instant::now());}
                KeyChange::Press
            }
            (true, true) if is_modifier(vk) => KeyChange::None,
            (true, true) => {
                self.forwarded_repeats=self.forwarded_repeats.saturating_add(1);
                KeyChange::Repeat
            }
            (true, false) => {
                *down=false;
                if !self.down.iter().enumerate().any(|(vk,down)|*down&&is_modifier(vk as u32)) {
                    self.last_modifier_repeat=None;
                }
                KeyChange::Release
            }
            (false, false) => KeyChange::None,
        }
    }
    /// One refresh timer for all held modifiers. Windows may stop their
    /// typematic events after a chord key, while UU's host needs regular
    /// presses to keep the modifier down.
    pub fn modifier_repeats(&mut self, now: std::time::Instant, uu_compat: bool) -> Vec<u32> {
        // Native hosts retain successful downs until release/reset. Reinjecting
        // modifiers there starts a new shell chord after e.g. Win+V, so the
        // eventual Win up can open Start and dismiss the clipboard flyout.
        if !uu_compat { return Vec::new(); }
        let held: Vec<u32>=self.down.iter().enumerate()
            .filter_map(|(vk,down)|(*down&&is_modifier(vk as u32)).then_some(vk as u32)).collect();
        if held.is_empty() {self.last_modifier_repeat=None;return held;}
        let Some(last)=self.last_modifier_repeat else {self.last_modifier_repeat=Some(now);return Vec::new();};
        if now.duration_since(last)<MODIFIER_REPEAT_INTERVAL {return Vec::new();}
        self.last_modifier_repeat=Some(now);
        self.forwarded_repeats=self.forwarded_repeats.saturating_add(held.len() as u64);
        held
    }
    pub fn forwarded_repeats(&self) -> u64 {self.forwarded_repeats}
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
        self.last_modifier_repeat=None;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_chords_keep_one_modifier_down_until_release_without_keepalives() {
        for modifier in [0x5b, 0x5c, 0xa0, 0xa2, 0xa4, 0xa5] {
            let mut remote = RemoteKeys::default();
            assert_eq!(remote.event(modifier,true),KeyChange::Press);
            assert_eq!(remote.event(0x56,true),KeyChange::Press);
            assert_eq!(remote.event(0x56,false),KeyChange::Release);
            let now=std::time::Instant::now();
            for tick in 1..=100 {
                assert!(remote.modifier_repeats(now+MODIFIER_REPEAT_INTERVAL*tick,false).is_empty());
                assert_eq!(remote.event(modifier,true),KeyChange::None);
            }
            assert_eq!(remote.held(),vec![modifier]);
            assert_eq!(remote.forwarded_repeats(),0);
            assert_eq!(remote.event(modifier,false),KeyChange::Release);
            assert_eq!(remote.event(modifier,false),KeyChange::None);
            assert_eq!(remote.event(0x41,true),KeyChange::Press);
            assert_eq!(remote.event(0x41,true),KeyChange::Repeat);
            remote.clear();
            assert!(remote.modifier_repeats(now+MODIFIER_REPEAT_INTERVAL*101,false).is_empty());
            assert_eq!(remote.event(0x41,false),KeyChange::None);
        }
    }
    #[test]
    fn waiting_for_remote_grant_keeps_intent_but_focus_ui_and_view_only_release_ownership(){
        let mut capture=Capture::default();capture.request();
        let waiting=Conditions{ready:false,focused:true,view_only:false,local_ui:false};
        assert!(capture.wants_control(waiting));assert!(!capture.active(waiting));
        for paused in [Conditions{focused:false,..waiting},Conditions{local_ui:true,..waiting},Conditions{view_only:true,..waiting}] {assert!(!capture.wants_control(paused));}
        capture.release();assert!(!capture.wants_control(waiting));
    }
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
            assert_eq!(keys.event(modifier, true), KeyChange::Press);
            for key in [0x25, 0x27, 9, 9, 0x25] {
                assert_eq!(keys.event(key, true), KeyChange::Press);
                assert_eq!(keys.event(key, false), KeyChange::Release);
                assert_eq!(keys.event(key, false), KeyChange::None);
                assert_eq!(keys.held(), vec![modifier]);
            }
            assert_eq!(keys.event(modifier, false), KeyChange::Release);
            assert_eq!(keys.event(modifier, false), KeyChange::None);
            assert!(keys.is_empty());
        }
    }
    #[test]
    fn typematic_repeats_keep_uus_held_state_alive() {
        use crate::keyboard_capture::{Decision,Keys};
        // Ordinary keys forward Windows repeats; held modifiers use one
        // client-side refresh timer because Windows may stop repeating them.
        for modifier in [0x5b,0x5c,0xa4,0xa5] {
            let mut hook=Keys::new();
            let mut remote=RemoteKeys::default();
            let mut sent=Vec::new();
            let mut accept=|vk,down,previous| {
                match hook.event(vk,down,true,false,previous) {
                    Decision::Remote{vk,pressed}=>{
                        if remote.event(vk,pressed)!=KeyChange::None {sent.push((vk,pressed));}
                    },
                    decision=>panic!("captured transition escaped hook: {decision:?}"),
                }
            };
            accept(modifier,true,false);
            for key in [0x25,0x27,9] {
                accept(key,true,false);
                for _ in 0..30 {accept(key,true,true);}
                accept(key,false,true);
            }
            accept(modifier,false,true);
            assert_eq!(sent.iter().filter(|(vk,_)|*vk==modifier).count(),2);
            assert_eq!(sent.iter().filter(|(vk,pressed)|*vk!=modifier&&!pressed).count(),3);
            assert_eq!(remote.forwarded_repeats(),90);
            assert!(remote.is_empty());
        }
        let mut remote=RemoteKeys::default();
        assert_eq!(remote.event(0xa4,true),KeyChange::Press);
        let due=std::time::Instant::now()+MODIFIER_REPEAT_INTERVAL;
        assert_eq!(remote.modifier_repeats(due,true),vec![0xa4]);
        assert_eq!(remote.modifier_repeats(due,true),Vec::<u32>::new());
        assert_eq!(remote.modifier_repeats(due+MODIFIER_REPEAT_INTERVAL,true),vec![0xa4]);
        assert_eq!(remote.event(0xa4,true),KeyChange::None);
        assert_eq!(remote.event(0xa4,false),KeyChange::Release);
        assert!(remote.modifier_repeats(due+MODIFIER_REPEAT_INTERVAL*2,true).is_empty());
    }

    #[test]
    fn release_all_has_no_unowned_or_duplicate_modifier_ups() {
        let mut keys = RemoteKeys::default();
        assert!(keys.held().is_empty());
        assert_eq!(keys.event(0x5b, false),KeyChange::None);
        assert_eq!(keys.event(0xa4, true),KeyChange::Press);
        assert_eq!(keys.event(9, true),KeyChange::Press);
        assert_eq!(keys.event(0x5b, true),KeyChange::Press);
        let release = keys.held();
        assert_eq!(release.first(), Some(&9));
        for key in release {
            assert_eq!(keys.event(key, false),KeyChange::Release);
            assert_eq!(keys.event(key, false),KeyChange::None);
        }
        assert!(keys.held().is_empty());
        assert_eq!(keys.event(0x5b, false),KeyChange::None);
        assert_eq!(keys.event(0xa4, false),KeyChange::None);
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
