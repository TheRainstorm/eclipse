//! Input injection into an explicitly authorized interactive desktop.
//! HID usages are translated through the shared keycode dataset; they are
//! neither Windows scan codes nor Linux evdev values themselves.
use thiserror::Error;

mod geometry;
mod held;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub mod wayland;
#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub mod windows_desktop;
pub use geometry::DesktopRect;
#[cfg(target_os = "linux")]
use held::release_owned;
pub use held::HeldInput;

#[derive(Debug, Error)]
pub enum InputError {
    #[error("input backend unavailable: {0}")]
    Unavailable(String),
    #[error("permission denied opening the input device")]
    PermissionDenied,
    #[error("unsupported keyboard HID usage: {0:#x}")]
    UnsupportedKey(u16),
    #[error("backend error: {0}")]
    Backend(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// Keyboard/keypad usage on standard USB HID page 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyCode(pub u16);
impl KeyCode {
    pub const ESC: Self = Self(0x29);
    pub const TAB: Self = Self(0x2b);
    pub const ENTER: Self = Self(0x28);

    pub fn mapping(self) -> Result<keycode::KeyMap, InputError> {
        if self.0 < 4 {
            return Err(InputError::UnsupportedKey(self.0));
        }
        keycode::KeyMap::from_usb_code(7, self.0).map_err(|_| InputError::UnsupportedKey(self.0))
    }

    pub fn from_windows_scan(scan: u16) -> Option<Self> {
        if scan == 0 || scan == 0xffff {
            return None;
        }
        let key = keycode::KeyMap::from_key_mapping(keycode::KeyMapping::Win(scan)).ok()?;
        let usage = Self(key.usb);
        let keyboard = usage.mapping().ok()?;
        (keyboard.win == scan).then_some(usage)
    }

    pub fn from_w3c_code(code: &str) -> Option<Self> {
        let code: keycode::KeyMappingCode = code.parse().ok()?;
        let key = keycode::KeyMap::from(code);
        let usage = Self(key.usb);
        (usage.mapping().ok()?.code == key.code).then_some(usage)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    /// Global desktop pixels, including negative monitor origins on Windows.
    MouseMoveAbsolute {
        x: i32,
        y: i32,
    },
    MouseMoveRelative {
        dx: i32,
        dy: i32,
    },
    MouseButton {
        button: MouseButton,
        pressed: bool,
    },
    /// Wheel units: 120 per detent; positive y scrolls up, positive x right.
    MouseScroll {
        dx: i32,
        dy: i32,
    },
    Key {
        key: KeyCode,
        pressed: bool,
    },
}

pub trait InputInjector: Send {
    fn inject(&mut self, event: InputEvent) -> Result<(), InputError>;
    /// Release only successfully pressed keys/buttons owned by this injector.
    fn release_all(&mut self) -> Result<(), InputError>;
}

/// Never elevates permissions or opens a kernel input device implicitly.
pub fn default_injector() -> Result<Box<dyn InputInjector>, InputError> {
    #[cfg(target_os = "linux")]
    {
        linux::XTestInjector::connect(None).map(|i| Box::new(i) as Box<dyn InputInjector>)
    }
    #[cfg(windows)]
    {
        Ok(Box::new(windows::SendInputInjector::new()))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        Err(InputError::Unavailable("no backend for this target".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn standard_mapping_covers_punctuation_modifiers_and_extended_keys() {
        for (hid, evdev, scan) in [
            (4, 30, 0x1e),
            (0x33, 39, 0x27),
            (0x38, 53, 0x35),
            (0xe1, 42, 0x2a),
            (0xe5, 54, 0x36),
            (0xe4, 97, 0xe01d),
            (0xe6, 100, 0xe038),
            (0x58, 96, 0xe01c),
            (0x53, 69, 0xe045),
        ] {
            let key = KeyCode(hid).mapping().unwrap();
            assert_eq!((key.evdev, key.win), (evdev, scan));
            assert_eq!(KeyCode::from_windows_scan(scan), Some(KeyCode(hid)));
        }
        for usage in [0, 1, 2, 3, 0xffff] {
            assert!(KeyCode(usage).mapping().is_err());
        }
    }
}

pub mod gamepad;
