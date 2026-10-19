//! Ownership of native key transitions. The hook never performs network I/O.
use crate::shortcuts::Action;

#[derive(Debug, PartialEq)]
pub enum Decision {
    Forward,
    Consume,
    Remote { vk: u32, pressed: bool },
    Shortcut(Action),
}
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
    pub fn event(
        &mut self,
        vk: u32,
        pressed: bool,
        active: bool,
        ctrl_alt: bool,
        already_down: bool,
    ) -> Decision {
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
            return if active {
                Decision::Remote { vk, pressed: true }
            } else {
                Decision::Consume
            };
        }
        // Do not steal the release of a key already pressed before capture.
        if !active || already_down {
            return Decision::Forward;
        }
        if ctrl_alt {
            let action = match vk {
                0x53 => Some(Action::Stats),
                0x4d => Some(Action::Mute),
                0x48 => Some(Action::Menu),
                0x46 | 0x58 => Some(Action::Fullscreen),
                0x51 => Some(Action::Exit),
                0x5a => Some(Action::Capture),
                _ => None,
            };
            if let Some(action) = action {
                *held = 2;
                return Decision::Shortcut(action);
            }
        }
        *held = 1;
        Decision::Remote { vk, pressed: true }
    }
}
#[cfg_attr(not(any(windows,test)),allow(dead_code))]
pub fn our_mask(vk:u32,extra_info:usize)->bool {vk==0xff && extra_info==0x5555574d}
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
    fn remote_or_accessibility_input_is_not_filtered_with_our_own_mask() {
        assert!(our_mask(0xff,0x5555574d));
        assert!(!our_mask(0x41,0));assert!(!our_mask(0x41,0x5555574d));assert!(!our_mask(0xff,0));
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
