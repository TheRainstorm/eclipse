//! Local shortcuts own their complete key transition, including key-up after
//! Ctrl/Alt are released. Auto-repeat must never toggle a setting repeatedly.
use std::collections::HashSet;
use eframe::egui::{Key, Modifiers};

#[derive(Debug, PartialEq)]
pub enum Action { Stats, Menu, Mute, Fullscreen, Exit, Capture, MouseMode, CursorMode }
#[derive(Debug, PartialEq)]
pub enum KeyAction { Forward, Consumed, Trigger(Action) }
#[derive(Default)]
pub struct Shortcuts { held: HashSet<Key> }
impl Shortcuts {
    pub fn clear(&mut self) { self.held.clear(); }
    pub fn key(&mut self, key: Key, pressed: bool, repeat: bool, modifiers: Modifiers, streaming: bool) -> KeyAction {
        if !pressed {
            return if self.held.remove(&key) { KeyAction::Consumed } else { KeyAction::Forward };
        }
        if self.held.contains(&key) { return KeyAction::Consumed; }
        // If this key was already sent remotely before the chord formed,
        // preserve its repeat/release rather than claiming half a transition.
        if repeat { return KeyAction::Forward; }
        let action = if modifiers.ctrl && modifiers.alt {
            match key {
                Key::S if streaming => Some(Action::Stats),
                Key::M if streaming => Some(Action::Mute),
                Key::H if streaming => Some(Action::Menu),
                Key::F | Key::X => Some(Action::Fullscreen),
                Key::Q if streaming => Some(Action::Exit),
                Key::Z if streaming => Some(Action::Capture),
                Key::P if streaming => Some(Action::MouseMode),
                Key::C if streaming => Some(Action::CursorMode),
                _ => None,
            }
        } else { None };
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
    fn stats_toggle_once_and_owns_release_even_after_modifiers_are_up() {
        let mut keys = Shortcuts::default();
        assert_eq!(keys.key(Key::S, true, false, CHORD, true), KeyAction::Trigger(Action::Stats));
        assert_eq!(keys.key(Key::S, true, true, CHORD, true), KeyAction::Consumed);
        assert_eq!(keys.key(Key::S, false, false, Modifiers::NONE, true), KeyAction::Consumed);
        assert_eq!(keys.key(Key::S, true, false, CHORD, true), KeyAction::Trigger(Action::Stats));
    }
    #[test]
    fn remote_key_held_before_chord_keeps_its_release() {
        let mut keys = Shortcuts::default();
        assert_eq!(keys.key(Key::S, true, false, Modifiers::NONE, true), KeyAction::Forward);
        assert_eq!(keys.key(Key::S, true, true, CHORD, true), KeyAction::Forward);
        assert_eq!(keys.key(Key::S, false, false, CHORD, true), KeyAction::Forward);
    }
    #[test]
    fn exit_and_fullscreen_alias_own_repeat_and_release() {
        for (key, expected) in [(Key::Q, Action::Exit), (Key::X, Action::Fullscreen), (Key::Z, Action::Capture),
            (Key::P, Action::MouseMode), (Key::C, Action::CursorMode)] {
            let mut keys = Shortcuts::default();
            assert_eq!(keys.key(key, true, false, CHORD, true), KeyAction::Trigger(expected));
            assert_eq!(keys.key(key, true, true, CHORD, true), KeyAction::Consumed);
            assert_eq!(keys.key(key, false, false, Modifiers::NONE, true), KeyAction::Consumed);
        }
        assert_eq!(Shortcuts::default().key(Key::Q, true, false, CHORD, false), KeyAction::Forward);
        for key in [Key::P,Key::C] {
            assert_eq!(Shortcuts::default().key(key,true,false,CHORD,false),KeyAction::Forward);
            assert_eq!(Shortcuts::default().key(key,true,false,Modifiers::CTRL,true),KeyAction::Forward);
        }
    }
    #[test]
    fn ordinary_remote_keys_and_f1_are_preserved() {
        let mut keys = Shortcuts::default();
        assert_eq!(keys.key(Key::S, true, false, Modifiers::CTRL, true), KeyAction::Forward);
        assert_eq!(keys.key(Key::F1, true, false, Modifiers::NONE, true), KeyAction::Forward);
        assert_eq!(keys.key(Key::S, true, false, CHORD, false), KeyAction::Forward);
        assert_eq!(keys.key(Key::F, true, false, CHORD, false), KeyAction::Trigger(Action::Fullscreen));
    }
    #[test]
    fn mute_and_menu_are_distinct_and_toggle_only_on_first_press() {
        for (key, action) in [(Key::M, Action::Mute), (Key::H, Action::Menu)] {
            let mut keys = Shortcuts::default();
            assert_eq!(keys.key(key, true, false, CHORD, true), KeyAction::Trigger(action));
            assert_eq!(keys.key(key, true, true, Modifiers::NONE, true), KeyAction::Consumed);
            assert_eq!(keys.key(key, false, false, Modifiers::NONE, false), KeyAction::Consumed);
            assert_eq!(keys.key(key, true, false, CHORD, false), KeyAction::Forward);
        }
    }
}
