use crate::{InputError, InputEvent, InputInjector, KeyCode, MouseButton};

#[derive(Default)]
pub(crate) struct HeldInput {
    keys: Vec<KeyCode>,
    buttons: Vec<MouseButton>,
}
impl HeldInput {
    pub fn should_emit(&self, event: InputEvent) -> bool {
        match event {
            InputEvent::Key {
                key,
                pressed: false,
            } => self.keys.contains(&key),
            InputEvent::MouseButton {
                button,
                pressed: false,
            } => self.buttons.contains(&button),
            InputEvent::MouseButton {
                button,
                pressed: true,
            } => !self.buttons.contains(&button),
            _ => true,
        }
    }
    /// Call only after the backend accepted the transition.
    pub fn applied(&mut self, event: InputEvent) {
        match event {
            InputEvent::Key { key, pressed } => update(&mut self.keys, key, pressed),
            InputEvent::MouseButton { button, pressed } => {
                update(&mut self.buttons, button, pressed)
            }
            _ => {}
        }
    }
    pub fn releases(&self) -> Vec<InputEvent> {
        self.buttons
            .iter()
            .rev()
            .map(|&button| InputEvent::MouseButton {
                button,
                pressed: false,
            })
            .chain(self.keys.iter().rev().map(|&key| InputEvent::Key {
                key,
                pressed: false,
            }))
            .collect()
    }
}
fn update<T: PartialEq>(held: &mut Vec<T>, value: T, pressed: bool) {
    if pressed {
        if !held.contains(&value) {
            held.push(value);
        }
    } else {
        held.retain(|old| *old != value);
    }
}
pub(crate) fn release_owned(
    injector: &mut impl InputInjector,
    events: Vec<InputEvent>,
) -> Result<(), InputError> {
    let mut first = None;
    for event in events {
        if let Err(error) = injector.inject(event) {
            if first.is_none() {
                first = Some(error);
            }
        }
    }
    first.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_release_does_not_release_other_keys_or_duplicate_repeat_ownership() {
        let mut held = HeldInput::default();
        assert!(!held.should_emit(InputEvent::Key {
            key: KeyCode(4),
            pressed: false
        }));
        let shift = InputEvent::Key {
            key: KeyCode(0xe1),
            pressed: true,
        };
        let letter = InputEvent::Key {
            key: KeyCode(4),
            pressed: true,
        };
        held.applied(shift);
        held.applied(letter);
        held.applied(letter);
        held.applied(InputEvent::MouseButton {
            button: MouseButton::Back,
            pressed: true,
        });
        assert_eq!(
            held.releases(),
            vec![
                InputEvent::MouseButton {
                    button: MouseButton::Back,
                    pressed: false
                },
                InputEvent::Key {
                    key: KeyCode(4),
                    pressed: false
                },
                InputEvent::Key {
                    key: KeyCode(0xe1),
                    pressed: false
                }
            ]
        );
        for release in held.releases() {
            held.applied(release);
        }
        assert!(held.releases().is_empty());
    }
}
