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
                0x4d => Some(Action::Menu),
                0x46 | 0x58 => Some(Action::Fullscreen),
                0x51 => Some(Action::Exit),
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
#[cfg(test)]
mod tests {
    use super::*;
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
