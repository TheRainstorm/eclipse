//! XTest input in the same authorized X11 session as desktop capture.
//! This backend never opens /dev/uinput or controls native Wayland surfaces.
use crate::{release_owned, HeldInput, InputError, InputEvent, InputInjector, MouseButton};
use x11rb::{
    connection::Connection,
    protocol::{
        xproto::{
            BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
            MOTION_NOTIFY_EVENT,
        },
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
};
fn backend(error: impl std::fmt::Display) -> InputError {
    InputError::Backend(error.to_string())
}

pub struct XTestInjector {
    conn: RustConnection,
    root: u32,
    width: i32,
    height: i32,
    held: HeldInput,
    wheel: (i32, i32),
}
impl XTestInjector {
    pub fn connect(display: Option<&str>) -> Result<Self, InputError> {
        if display.is_none() && std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()) {
            return Err(InputError::Unavailable(
                "native Wayland input requires a desktop portal".into(),
            ));
        }
        let (conn, index) =
            x11rb::connect(display).map_err(|e| InputError::Unavailable(e.to_string()))?;
        conn.xtest_get_version(2, 2)
            .map_err(backend)?
            .reply()
            .map_err(backend)?;
        let screen = &conn.setup().roots[index];
        if screen.width_in_pixels > 32767 || screen.height_in_pixels > 32767 {
            return Err(InputError::Unavailable(
                "XTest coordinates exceed the supported desktop bounds".into(),
            ));
        }
        let (root, width, height) = (
            screen.root,
            i32::from(screen.width_in_pixels),
            i32::from(screen.height_in_pixels),
        );
        Ok(Self {
            conn,
            root,
            width,
            height,
            held: HeldInput::default(),
            wheel: (0, 0),
        })
    }
    fn fake(&self, kind: u8, detail: u8, root: u32, x: i16, y: i16) -> Result<(), InputError> {
        self.conn
            .xtest_fake_input(kind, detail, 0, root, x, y, 0)
            .map_err(backend)?
            .check()
            .map_err(backend)
    }
    fn wheel_axis(&self, steps: i32, positive: u8, negative: u8) -> Result<(), InputError> {
        let button = if steps > 0 { positive } else { negative };
        for _ in 0..steps.unsigned_abs() {
            self.fake(BUTTON_PRESS_EVENT, button, 0, 0, 0)?;
            self.fake(BUTTON_RELEASE_EVENT, button, 0, 0, 0)?;
        }
        Ok(())
    }
    fn emit(&mut self, event: InputEvent) -> Result<(), InputError> {
        match event {
            InputEvent::MouseMoveAbsolute { x, y } => self.fake(
                MOTION_NOTIFY_EVENT,
                0,
                self.root,
                x.clamp(0, self.width - 1) as i16,
                y.clamp(0, self.height - 1) as i16,
            ),
            InputEvent::MouseMoveRelative { dx, dy } => self.fake(
                MOTION_NOTIFY_EVENT,
                1,
                0,
                dx.clamp(-32768, 32767) as i16,
                dy.clamp(-32768, 32767) as i16,
            ),
            InputEvent::MouseButton { button, pressed } => {
                let button = match button {
                    MouseButton::Left => 1,
                    MouseButton::Middle => 2,
                    MouseButton::Right => 3,
                    MouseButton::Back => 8,
                    MouseButton::Forward => 9,
                };
                self.fake(
                    if pressed {
                        BUTTON_PRESS_EVENT
                    } else {
                        BUTTON_RELEASE_EVENT
                    },
                    button,
                    0,
                    0,
                    0,
                )
            }
            InputEvent::MouseScroll { dx, dy } => {
                self.wheel.0 += dx.clamp(-1920, 1920);
                self.wheel.1 += dy.clamp(-1920, 1920);
                let (x, y) = (self.wheel.0 / 120, self.wheel.1 / 120);
                self.wheel.0 %= 120;
                self.wheel.1 %= 120;
                self.wheel_axis(x, 7, 6)?;
                self.wheel_axis(y, 4, 5)
            }
            InputEvent::Key { key, pressed } => {
                let code = key.mapping()?.xkb;
                if !(8..=255).contains(&code) {
                    return Err(InputError::UnsupportedKey(key.0));
                }
                self.fake(
                    if pressed {
                        KEY_PRESS_EVENT
                    } else {
                        KEY_RELEASE_EVENT
                    },
                    code as u8,
                    0,
                    0,
                    0,
                )
            }
        }
    }
}
impl InputInjector for XTestInjector {
    fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
        if !self.held.should_emit(event) {
            return Ok(());
        }
        self.emit(event)?;
        self.held.applied(event);
        Ok(())
    }
    fn release_all(&mut self) -> Result<(), InputError> {
        let events = self.held.releases();
        self.wheel = (0, 0);
        release_owned(self, events)
    }
}
impl Drop for XTestInjector {
    fn drop(&mut self) {
        let _ = self.release_all();
    }
}
