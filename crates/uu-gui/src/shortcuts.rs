//! Local shortcuts own their complete key transition, including key-up after
//! Ctrl/Alt are released. Auto-repeat must never toggle a setting repeatedly.
use std::collections::HashSet;
use eframe::egui::{Key, Modifiers};

#[derive(Debug, PartialEq)]
pub enum Action { Stats, Menu, Fullscreen }
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
        let action = if modifiers.ctrl && modifiers.alt {
            match key {
                Key::S if streaming => Some(Action::Stats),
                Key::M if streaming => Some(Action::Menu),
                Key::F => Some(Action::Fullscreen),
                _ => None,
            }
        } else { None };
        if let Some(action) = action {
            self.held.insert(key);
            if repeat { KeyAction::Consumed } else { KeyAction::Trigger(action) }
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
    fn ordinary_remote_keys_and_f1_are_preserved() {
        let mut keys = Shortcuts::default();
        assert_eq!(keys.key(Key::S, true, false, Modifiers::CTRL, true), KeyAction::Forward);
        assert_eq!(keys.key(Key::F1, true, false, Modifiers::NONE, true), KeyAction::Forward);
        assert_eq!(keys.key(Key::S, true, false, CHORD, false), KeyAction::Forward);
        assert_eq!(keys.key(Key::F, true, false, CHORD, false), KeyAction::Trigger(Action::Fullscreen));
    }
}
