//! Ownership of native key transitions. The hook never performs network I/O.
use crate::shortcuts::Action;

#[derive(Debug, PartialEq)]
pub enum Decision {
    Forward,
    Consume,
    Remote { vk: u32, pressed: bool },
    Shortcut(Action),
}
/// Right Alt can be AltGr: Windows then also reports a synthetic Ctrl.
/// Reserve it for remote text input rather than local Ctrl+Alt actions.
#[cfg_attr(not(any(windows,test)),allow(dead_code))]
pub fn shortcut_chord(ctrl: bool, alt: bool, right_alt: bool) -> bool {
    ctrl && alt && !right_alt
}
/// Track physical modifier transitions even while capture is disabled. Windows
/// does not update GetAsyncKeyState for a key swallowed by the low-level hook.
/// Window messages query their own queue-time state; the hook keeps its own state.
#[cfg_attr(not(any(windows,test)),allow(dead_code))]
pub struct Modifiers { down: u8 }
#[cfg_attr(not(any(windows,test)),allow(dead_code))]
impl Modifiers {
    pub const fn new() -> Self { Self { down: 0 } }
    pub fn observe(&mut self, vk: u32, pressed: bool) {
        let bit = match vk { 0xa0..=0xa5 => 1 << (vk-0xa0), 0x5b => 1 << 6, 0x5c => 1 << 7, _ => return };
        if pressed { self.down |= bit; } else { self.down &= !bit; }
    }
    pub fn shortcut_chord(&self) -> bool {
        shortcut_chord(self.down & 0x0c != 0, self.down & 0x30 != 0, self.down & 0x20 != 0)
    }
    pub fn win(&self) -> bool { self.down & 0xc0 != 0 }
    #[cfg(test)]
    pub fn shift(&self) -> bool { self.down & 0x03 != 0 }
    pub fn from_key_state(mut is_down:impl FnMut(u32)->bool)->Self {
        let mut modifiers=Self::new();
        for vk in [0xa0,0xa1,0xa2,0xa3,0xa4,0xa5,0x5b,0x5c] {
            modifiers.observe(vk,is_down(vk));
        }
        modifiers
    }
}
/// Win messages arriving at a window have already been offered to the shell.
/// Only a low-level hook may forward these remotely without dual activation.
#[cfg_attr(not(any(windows,test)),allow(dead_code))]
pub fn local_system_key(vk:u32,win_down:bool)->bool {win_down||matches!(vk,0x5b|0x5c)}
#[cfg_attr(not(any(windows,test)),allow(dead_code))]
pub struct Keys {
    held: [u8; 256],
}
#[cfg_attr(not(any(windows,test)),allow(dead_code))]
impl Keys {
    pub const fn new() -> Self {
        Self { held: [0; 256] }
    }
    pub fn down(&self, vk: usize) -> bool {
        self.held.get(vk) == Some(&1)
    }
    #[cfg(test)]
    pub fn event(
        &mut self,
        vk: u32,
        pressed: bool,
        active: bool,
        ctrl_alt: bool,
        already_down: bool,
    ) -> Decision {
        self.event_with_shortcuts(vk, pressed, active, active, ctrl_alt, already_down)
    }
    pub fn event_with_shortcuts(
        &mut self,
        vk: u32,
        pressed: bool,
        remote_active: bool,
        shortcuts_active: bool,
        ctrl_alt: bool,
        already_down: bool,
    ) -> Decision {
        let altgr = self.down(0xa5);
        let Some(held) = self.held.get_mut(vk as usize) else {
            return Decision::Forward;
        };
        if !pressed {
            return match std::mem::take(held) {
                1 => Decision::Remote { vk, pressed: false },
                2 => Decision::Consume,
                _ => Decision::Forward,
            };
        }
        if *held == 2 {
            return Decision::Consume;
        }
        if *held == 1 {
            return if remote_active {
                Decision::Remote { vk, pressed: true }
            } else {
                Decision::Consume
            };
        }
        // Do not steal the release of a key already pressed before capture.
        if already_down {
            return Decision::Forward;
        }
        if shortcuts_active && ctrl_alt && !altgr {
            let action = match vk {
                0x53 => Some(Action::Stats),
                0x4d => Some(Action::Mute),
                0x48 => Some(Action::Menu),
                0x46 | 0x58 => Some(Action::Fullscreen),
                0x51 => Some(Action::Exit),
                0x5a => Some(Action::Capture),
                0x50 => Some(Action::MouseMode),
                0x43 => Some(Action::CursorMode),
                _ => None,
            };
            if let Some(action) = action {
                *held = 2;
                return Decision::Shortcut(action);
            }
        }
        if !remote_active { return Decision::Forward; }
        *held = 1;
        Decision::Remote { vk, pressed: true }
    }
}
/// WM_KEY* uses generic modifiers; scan code and extended bit retain sides.
#[cfg_attr(not(any(windows,test)),allow(dead_code))]
pub fn sided_vk(vk:u32,lparam:usize)->u32 {
    match vk {
        0x10=>if (lparam>>16)&0xff==0x36 {0xa1}else{0xa0},
        0x11=>if lparam&(1<<24)!=0 {0xa3}else{0xa2},
        0x12=>if lparam&(1<<24)!=0 {0xa5}else{0xa4},
        _=>vk,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_fullscreen_chord_keeps_ownership_across_capture_changes() {
        let mut modifiers=Modifiers::new();
        let mut hook=Keys::new();
        let mut window=Keys::new();
        for vk in [0xa2,0xa4] {
            modifiers.observe(vk,true);
            assert_eq!(hook.event_with_shortcuts(vk,true,true,true,modifiers.shortcut_chord(),false),Decision::Remote{vk,pressed:true});
        }
        assert_eq!(hook.event_with_shortcuts(0x46,true,true,true,modifiers.shortcut_chord(),false),Decision::Shortcut(Action::Fullscreen));
        // The hook swallowed Ctrl/Alt: Windows async state and the other
        // route's ownership table both say "up" after leaving fullscreen.
        assert!(!window.down(0xa2));assert!(!window.down(0xa4));
        assert_eq!(hook.event_with_shortcuts(0x46,true,false,true,modifiers.shortcut_chord(),false),Decision::Consume);
        assert_eq!(hook.event_with_shortcuts(0x46,false,false,true,modifiers.shortcut_chord(),false),Decision::Consume);
        // A second tap while the same modifiers remain held is still local.
        assert_eq!(hook.event_with_shortcuts(0x46,true,false,true,modifiers.shortcut_chord(),false),Decision::Shortcut(Action::Fullscreen));
        assert_eq!(hook.event_with_shortcuts(0x46,false,true,true,modifiers.shortcut_chord(),false),Decision::Consume);
        // An unreserved letter still traverses the ordinary window route.
        assert_eq!(hook.event_with_shortcuts(0x41,true,false,true,modifiers.shortcut_chord(),false),Decision::Forward);
        assert_eq!(window.event_with_shortcuts(0x41,true,true,true,modifiers.shortcut_chord(),false),Decision::Remote{vk:0x41,pressed:true});
        for vk in [0xa4,0xa2] {
            modifiers.observe(vk,false);
            assert_eq!(hook.event_with_shortcuts(vk,false,false,true,modifiers.shortcut_chord(),false),Decision::Remote{vk,pressed:false});
        }
        assert!(!modifiers.shortcut_chord());
    }
    #[test]
    fn client_commands_are_local_without_remote_control_or_system_capture() {
        for (vk,action) in [(0x46,Action::Fullscreen),(0x58,Action::Fullscreen),(0x51,Action::Exit),
            (0x53,Action::Stats),(0x4d,Action::Mute),(0x48,Action::Menu),(0x5a,Action::Capture),(0x50,Action::MouseMode),(0x43,Action::CursorMode)] {
            let mut keys=Keys::new();
            assert_eq!(keys.event_with_shortcuts(vk,true,false,true,true,false),Decision::Shortcut(action));
            assert_eq!(keys.event_with_shortcuts(vk,true,false,false,false,false),Decision::Consume);
            assert_eq!(keys.event_with_shortcuts(vk,false,false,false,false,false),Decision::Consume);
            assert_eq!(keys.event_with_shortcuts(vk,true,false,false,true,false),Decision::Forward);
        }
        let mut keys=Keys::new();
        assert_eq!(keys.event_with_shortcuts(0x46,true,false,true,true,true),Decision::Forward);
        assert_eq!(keys.event_with_shortcuts(0x46,false,false,true,true,false),Decision::Forward);
    }
    #[test]
    fn modifiers_observed_before_capture_preserve_altgr_and_release_order() {
        let mut modifiers=Modifiers::new();
        modifiers.observe(0xa2,true);modifiers.observe(0xa5,true);
        assert!(!modifiers.shortcut_chord());
        let mut keys=Keys::new();
        assert_eq!(keys.event_with_shortcuts(0x46,true,true,true,modifiers.shortcut_chord(),false),Decision::Remote{vk:0x46,pressed:true});
        modifiers.observe(0xa4,true); // Right Alt remains held: still AltGr.
        assert!(!modifiers.shortcut_chord());
        modifiers.observe(0xa5,false);
        assert!(modifiers.shortcut_chord());
        modifiers.observe(0xa3,true);modifiers.observe(0xa2,false);
        assert!(modifiers.shortcut_chord());
        modifiers.observe(0xa3,false);
        assert!(!modifiers.shortcut_chord());
        modifiers.observe(0xa0,true);modifiers.observe(0x5c,true);
        assert!(modifiers.win()&&modifiers.shift());
        modifiers.observe(0xa0,false);modifiers.observe(0x5c,false);
        assert!(!modifiers.win()&&!modifiers.shift());
    }
    #[test]
    fn altgr_text_never_exits_or_toggles_the_local_stream() {
        for vk in [0x51,0x53,0x4d,0x48,0x46,0x58,0x5a,0x50,0x43] {
            let mut keys=Keys::new();
            assert_eq!(keys.event(0xa2,true,true,false,false),Decision::Remote {vk:0xa2,pressed:true});
            assert_eq!(keys.event(0xa5,true,true,false,false),Decision::Remote {vk:0xa5,pressed:true});
            assert_eq!(keys.event(vk,true,true,true,false),Decision::Remote {vk,pressed:true});
            assert_eq!(keys.event(vk,false,true,true,false),Decision::Remote {vk,pressed:false});
        }
        // Also protect capture enabled while Right Alt was already held.
        assert!(!shortcut_chord(true,true,true));
        assert!(shortcut_chord(true,true,false));
        let mut keys=Keys::new();
        assert_eq!(keys.event(0x51,true,true,shortcut_chord(true,true,true),false),Decision::Remote {vk:0x51,pressed:true});
        assert_eq!(keys.event(0x51,false,true,false,false),Decision::Remote {vk:0x51,pressed:false});
        assert_eq!(keys.event(0x51,true,true,shortcut_chord(true,true,false),false),Decision::Shortcut(Action::Exit));
    }
    #[test]
    fn captured_mute_and_menu_chords_own_repeats_and_release_after_focus_loss() {
        for (vk, action) in [(0x4d, Action::Mute), (0x48, Action::Menu)] {
            let mut keys = Keys::new();
            assert_eq!(keys.event(vk, true, true, true, false), Decision::Shortcut(action));
            assert_eq!(keys.event(vk, true, true, false, false), Decision::Consume);
            assert_eq!(keys.event(vk, false, false, false, false), Decision::Consume);
            assert_eq!(keys.event(vk, true, true, false, false), Decision::Remote {vk, pressed:true});
        }
    }
    #[test]
    fn native_keys_preserve_oem_punctuation_numpad_functions_and_modifier_sides() {
        for vk in [0xba,0xbb,0xbc,0xbd,0xbe,0xbf,0xc0,0xdb,0xdc,0xdd,0xde,0xe2,0x60,0x6f,0x70,0x87] {
            assert_eq!(sided_vk(vk,0),vk);
            let mut keys=Keys::new();assert_eq!(keys.event(vk,true,true,false,false),Decision::Remote{vk,pressed:true});
            assert_eq!(keys.event(vk,false,true,false,false),Decision::Remote{vk,pressed:false});
        }
        assert_eq!(sided_vk(0x10,0x36<<16),0xa1);assert_eq!(sided_vk(0x10,0x2a<<16),0xa0);
        assert_eq!(sided_vk(0x11,1<<24),0xa3);assert_eq!(sided_vk(0x12,1<<24),0xa5);
    }
    #[test]
    fn capture_owns_every_virtual_key_without_system_chord_exceptions() {
        // The hook must swallow each down/repeat/up, including injected input.
        // Win+Shift+arrows, Win alone and Alt+Tab all use the same path.
        for vk in 1..=255 {
            let mut keys=Keys::new();
            assert_eq!(keys.event(vk,true,true,false,false),Decision::Remote{vk,pressed:true});
            assert_eq!(keys.event(vk,true,true,false,true),Decision::Remote{vk,pressed:true});
            assert_eq!(keys.event(vk,false,true,false,true),Decision::Remote{vk,pressed:false});
        }
    }
    #[test]
    fn win_chords_are_exclusively_hook_owned_or_entirely_local() {
        for win in [0x5b,0x5c] {
            for chord in [vec![win],vec![win,0xa0,0x25],vec![win,0xa1,0x27],vec![win,0x44],vec![win,0xa2,0x25]] {
                for capture in [false,true] {
                    let mut modifiers=Modifiers::new();
                    let mut hook=Keys::new();let mut window=Keys::new();
                    for (vk,pressed) in chord.iter().copied().map(|vk|(vk,true))
                        .chain(chord.iter().rev().copied().map(|vk|(vk,false))) {
                        modifiers.observe(vk,pressed);
                        let event=hook.event(vk,pressed,capture,false,false);
                        if capture {assert_eq!(event,Decision::Remote{vk,pressed});}
                        else {
                            assert_eq!(event,Decision::Forward);
                            let remote=!local_system_key(vk,modifiers.win());
                            assert_eq!(window.event_with_shortcuts(vk,pressed,remote,false,false,false),Decision::Forward);
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn window_message_modifier_snapshot_does_not_follow_later_hook_events() {
        let message=Modifiers::from_key_state(|_|false);
        let later_hook=Modifiers::from_key_state(|vk|[0x5b,0xa2,0xa4].contains(&vk));
        assert!(!message.win());assert!(!message.shortcut_chord());
        assert!(later_hook.win());assert!(later_hook.shortcut_chord());
        let message=Modifiers::from_key_state(|vk|vk==0x5c);
        assert!(local_system_key(0x27,message.win()));
    }
    #[test]
    fn capture_changes_preserve_release_ownership_for_entire_system_chord() {
        let mut keys=Keys::new();
        for vk in [0x5b,0xa0,0x27] {
            assert_eq!(keys.event(vk,true,true,false,false),Decision::Remote{vk,pressed:true});
        }
        for vk in [0x27,0xa0,0x5b] {
            assert_eq!(keys.event(vk,false,false,false,true),Decision::Remote{vk,pressed:false});
        }
        let mut modifiers=Modifiers::new();
        for vk in [0x5c,0xa2,0xa4] {modifiers.observe(vk,true);}
        let snapshot=Modifiers::from_key_state(|vk|[0x5c,0xa2,0xa4].contains(&vk));
        assert!(snapshot.win());assert!(snapshot.shortcut_chord());
    }
    #[test]
    fn captured_system_key_owns_release_after_focus_loss_and_preserves_existing_keys() {
        let mut k = Keys::new();
        assert_eq!(
            k.event(0xa4, true, true, false, false),
            Decision::Remote {
                vk: 0xa4,
                pressed: true
            }
        );
        assert_eq!(
            k.event(9, true, true, false, false),
            Decision::Remote {
                vk: 9,
                pressed: true
            }
        );
        assert_eq!(
            k.event(9, false, false, false, false),
            Decision::Remote {
                vk: 9,
                pressed: false
            }
        );
        assert_eq!(
            k.event(0xa4, false, false, false, false),
            Decision::Remote {
                vk: 0xa4,
                pressed: false
            }
        );
        assert_eq!(k.event(0x11, true, true, false, true), Decision::Forward);
        assert_eq!(k.event(0x11, false, true, false, false), Decision::Forward);
    }
    #[test]
    fn local_escape_chord_never_sends_its_letter_or_repeat_remotely() {
        let mut k = Keys::new();
        assert_eq!(
            k.event(0x51, true, true, true, false),
            Decision::Shortcut(Action::Exit)
        );
        assert_eq!(k.event(0x51, true, true, true, false), Decision::Consume);
        assert_eq!(k.event(0x51, false, false, false, false), Decision::Consume);
        assert_eq!(k.event(0x51, true, false, true, false), Decision::Forward);
    }
}
