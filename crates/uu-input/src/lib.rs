//! Synthetic input for the controlled side.
//!
//! The official Windows client injects through a kernel driver (`uuycinput`)
//! plus ViGEm for gamepads. On Linux we reproduce the same observable effect
//! with `uinput` (Wayland/X11 both) or XTest for XWayland surfaces.

use thiserror::Error;

#[cfg(unix)]
pub mod linux;
#[cfg(windows)]
pub mod windows;

#[derive(Debug, Error)]
pub enum InputError {
    #[error("input backend unavailable: {0}")]
    Unavailable(String),
    #[error("permission denied opening the input device")]
    PermissionDenied,
    #[error("backend error: {0}")]
    Backend(String),
}

/// Logical mouse buttons, decoupled from platform constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// A key identified by its USB HID usage code, which maps cleanly onto both
/// uinput's `KEY_*` and Windows' virtual-key space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyCode(pub u16);

impl KeyCode {
    pub const ESC: KeyCode = KeyCode(0x29);
    pub const TAB: KeyCode = KeyCode(0x2b);
    pub const ENTER: KeyCode = KeyCode(0x28);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    MouseMoveAbsolute { x: i32, y: i32 },
    MouseMoveRelative { dx: i32, dy: i32 },
    MouseButton { button: MouseButton, pressed: bool },
    MouseScroll { dx: i32, dy: i32 },
    Key { key: KeyCode, pressed: bool },
}

/// Platform input sink.
pub trait InputInjector: Send {
    /// Screen geometry used to clamp absolute coordinates.
    fn set_screen(&mut self, width: u32, height: u32) -> Result<(), InputError>;
    fn inject(&mut self, event: InputEvent) -> Result<(), InputError>;
    /// Release everything currently held (safety net on disconnect).
    fn release_all(&mut self) -> Result<(), InputError>;
}

/// Build the best injector for this host.
pub fn default_injector() -> Result<Box<dyn InputInjector>, InputError> {
    #[cfg(unix)]
    {
        linux::UinputInjector::new().map(|i| Box::new(i) as Box<dyn InputInjector>)
    }
    #[cfg(windows)]
    {
        Ok(Box::new(windows::SendInputInjector::new()))
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(InputError::Unavailable("no backend for this target".into()))
    }
}
