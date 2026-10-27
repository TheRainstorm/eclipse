//! Local shortcuts own their complete key transition, including key-up after
//! Ctrl/Alt are released. Auto-repeat must never toggle a setting repeatedly.
use std::collections::HashSet;
use eframe::egui::{Key, Modifiers};

pub use uu_core::hotkeys::Action;
use uu_core::hotkeys::{Bindings,Modifiers as ChordModifiers};
#[derive(Debug, PartialEq)]
pub enum KeyAction { Forward, Consumed, Trigger(Action) }
#[derive(Default)]
pub struct Shortcuts { held: HashSet<Key>, bindings:Bindings, blocked:bool }
impl Shortcuts {
    pub fn configure(&mut self,bindings:&Bindings,modifiers:Modifiers) {if self.bindings!=*bindings{self.bindings=bindings.clone();self.blocked=modifiers.any();}}
    pub fn clear(&mut self) { self.held.clear(); }
    pub fn key(&mut self, key: Key, pressed: bool, repeat: bool, modifiers: Modifiers, streaming: bool, nested_passthrough: bool) -> KeyAction {
        if !pressed {
            return if self.held.remove(&key) { KeyAction::Consumed } else { KeyAction::Forward };
        }
        if self.held.contains(&key) { return KeyAction::Consumed; }
        if self.blocked {
            if modifiers.any() {self.held.insert(key);return KeyAction::Consumed;}
            self.blocked=false;
        }
        // If this key was already sent remotely before the chord formed,
        // preserve its repeat/release rather than claiming half a transition.
        if repeat { return KeyAction::Forward; }
        let action=crate::vk_for(key).and_then(|vk|self.bindings.find(vk,ChordModifiers {
            ctrl:modifiers.ctrl,alt:modifiers.alt,shift:modifiers.shift,meta:modifiers.mac_cmd
        },nested_passthrough,streaming,false));
        if let Some(action) = action {
            self.held.insert(key);
            KeyAction::Trigger(action)
        } else { KeyAction::Forward }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    const CHORD: Modifiers = Modifiers { ctrl: true, alt: true, ..Modifiers::NONE };
    #[test]
    fn rebound_shortcuts_keep_old_release_and_do_not_repeat_or_steal_a_held_chord() {
        let mut keys=Shortcuts::default();
        assert_eq!(keys.key(Key::S,true,false,CHORD,true,false),KeyAction::Trigger(Action::Stats));
        let mut bindings=Bindings::default();bindings.set(Action::Stats,vec![uu_core::hotkeys::Chord::ctrl_alt(0x41)]);
        keys.configure(&bindings,CHORD);
        assert_eq!(keys.key(Key::S,false,false,Modifiers::NONE,true,false),KeyAction::Consumed);
        assert_eq!(keys.key(Key::A,true,false,CHORD,true,false),KeyAction::Consumed);
        assert_eq!(keys.key(Key::A,false,false,Modifiers::NONE,true,false),KeyAction::Consumed);
        assert_eq!(keys.key(Key::S,true,false,Modifiers::NONE,true,false),KeyAction::Forward);
        assert_eq!(keys.key(Key::A,true,false,CHORD,true,false),KeyAction::Trigger(Action::Stats));
        assert_eq!(keys.key(Key::A,true,true,CHORD,true,false),KeyAction::Consumed);
        assert_eq!(keys.key(Key::A,false,false,Modifiers::NONE,true,false),KeyAction::Consumed);
        assert_eq!(keys.key(Key::S,true,false,CHORD,true,false),KeyAction::Forward);
    }
    #[test]
    fn stats_toggle_once_and_owns_release_even_after_modifiers_are_up() {
        let mut keys = Shortcuts::default();
        assert_eq!(keys.key(Key::S, true, false, CHORD, true, false), KeyAction::Trigger(Action::Stats));
        assert_eq!(keys.key(Key::S, true, true, CHORD, true, false), KeyAction::Consumed);
        assert_eq!(keys.key(Key::S, false, false, Modifiers::NONE, true, false), KeyAction::Consumed);
        assert_eq!(keys.key(Key::S, true, false, CHORD, true, false), KeyAction::Trigger(Action::Stats));
    }
    #[test]
    fn remote_key_held_before_chord_keeps_its_release() {
        let mut keys = Shortcuts::default();
        assert_eq!(keys.key(Key::S, true, false, Modifiers::NONE, true, false), KeyAction::Forward);
        assert_eq!(keys.key(Key::S, true, true, CHORD, true, false), KeyAction::Forward);
        assert_eq!(keys.key(Key::S, false, false, CHORD, true, false), KeyAction::Forward);
    }
    #[test]
    fn exit_and_fullscreen_alias_own_repeat_and_release() {
        for (key, expected) in [(Key::Q, Action::Exit), (Key::X, Action::Fullscreen), (Key::Z, Action::Capture),
            (Key::P, Action::MouseMode), (Key::C, Action::CursorMode)] {
            let mut keys = Shortcuts::default();
            assert_eq!(keys.key(key, true, false, CHORD, true, false), KeyAction::Trigger(expected));
            assert_eq!(keys.key(key, true, true, CHORD, true, false), KeyAction::Consumed);
            assert_eq!(keys.key(key, false, false, Modifiers::NONE, true, false), KeyAction::Consumed);
        }
        assert_eq!(Shortcuts::default().key(Key::Q, true, false, CHORD, false, false), KeyAction::Forward);
        for key in [Key::P,Key::C] {
            assert_eq!(Shortcuts::default().key(key,true,false,CHORD,false,false),KeyAction::Forward);
            assert_eq!(Shortcuts::default().key(key,true,false,Modifiers::CTRL,true,false),KeyAction::Forward);
        }
    }
    #[test]
    fn ordinary_remote_keys_and_f1_are_preserved() {
        let mut keys = Shortcuts::default();
        assert_eq!(keys.key(Key::S, true, false, Modifiers::CTRL, true, false), KeyAction::Forward);
        assert_eq!(keys.key(Key::F1, true, false, Modifiers::NONE, true, false), KeyAction::Forward);
        assert_eq!(keys.key(Key::S, true, false, CHORD, false, false), KeyAction::Forward);
        assert_eq!(keys.key(Key::F, true, false, CHORD, false, false), KeyAction::Trigger(Action::Fullscreen));
    }
    #[test]
    fn f12_toggles_nested_shortcuts_and_passthrough_forwards_other_chords() {
        let mut keys = Shortcuts::default();
        assert_eq!(
            keys.key(Key::F12, true, false, CHORD, true, false),
            KeyAction::Trigger(Action::ToggleNested)
        );
        assert_eq!(keys.key(Key::F12, true, true, CHORD, true, false), KeyAction::Consumed);
        assert_eq!(keys.key(Key::F12, false, false, Modifiers::NONE, true, false), KeyAction::Consumed);
        assert_eq!(
            Shortcuts::default().key(Key::S, true, false, CHORD, true, true),
            KeyAction::Forward
        );
        assert_eq!(
            Shortcuts::default().key(Key::F12, true, false, CHORD, true, true),
            KeyAction::Trigger(Action::ToggleNested)
        );
    }

    #[test]
    fn mute_and_menu_are_distinct_and_toggle_only_on_first_press() {
        for (key, action) in [(Key::M, Action::Mute), (Key::H, Action::Menu)] {
            let mut keys = Shortcuts::default();
            assert_eq!(keys.key(key, true, false, CHORD, true, false), KeyAction::Trigger(action));
            assert_eq!(keys.key(key, true, true, Modifiers::NONE, true, false), KeyAction::Consumed);
            assert_eq!(keys.key(key, false, false, Modifiers::NONE, false, false), KeyAction::Consumed);
            assert_eq!(keys.key(key, true, false, CHORD, false, false), KeyAction::Forward);
        }
    }
}
