//! `SendInput`-based injection.
//!
//! Note: this module is written against the Windows API but is not yet
//! compile-verified from the Linux development host; enabling the Windows
//! target is a build-matrix task tracked in `docs/roadmap.md`.

use crate::{InputError, InputEvent, InputInjector, KeyCode, MouseButton};

pub struct SendInputInjector {
    screen: (u32, u32),
}

impl SendInputInjector {
    pub fn new() -> Self {
        Self { screen: (1920, 1080) }
    }

    fn normalize(&self, x: i32, y: i32) -> (i32, i32) {
        // MOUSEEVENTF_ABSOLUTE maps onto a 0..65535 virtual desktop range.
        let nx = ((x as i64 * 65535) / self.screen.0.max(1) as i64).clamp(0, 65535) as i32;
        let ny = ((y as i64 * 65535) / self.screen.1.max(1) as i64).clamp(0, 65535) as i32;
        (nx, ny)
    }
}

impl Default for SendInputInjector {
    fn default() -> Self {
        Self::new()
    }
}

impl InputInjector for SendInputInjector {
    fn set_screen(&mut self, width: u32, height: u32) -> Result<(), InputError> {
        self.screen = (width.max(1), height.max(1));
        Ok(())
    }

    fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse as kbm;
        // SAFETY: all structs below are plain-old-data with the layouts the
        // Win32 API documents; the call only reads them during the call.
        unsafe {
            match event {
                InputEvent::MouseMoveAbsolute { x, y } => {
                    let (nx, ny) = self.normalize(x, y);
                    let mut input: kbm::INPUT = std::mem::zeroed();
                    input.r#type = kbm::INPUT_MOUSE;
                    input.Anonymous.mi.dx = nx;
                    input.Anonymous.mi.dy = ny;
                    input.Anonymous.mi.dwFlags =
                        kbm::MOUSEEVENTF_MOVE | kbm::MOUSEEVENTF_ABSOLUTE | kbm::MOUSEEVENTF_VIRTUALDESK;
                    if kbm::SendInput(1, &mut input, std::mem::size_of::<kbm::INPUT>() as i32) == 0 {
                        return Err(InputError::Backend("SendInput(move) failed".into()));
                    }
                }
                InputEvent::MouseMoveRelative { dx, dy } => {
                    let mut input: kbm::INPUT = std::mem::zeroed();
                    input.r#type = kbm::INPUT_MOUSE;
                    input.Anonymous.mi.dx = dx;
                    input.Anonymous.mi.dy = dy;
                    input.Anonymous.mi.dwFlags = kbm::MOUSEEVENTF_MOVE;
                    if kbm::SendInput(1, &mut input, std::mem::size_of::<kbm::INPUT>() as i32) == 0 {
                        return Err(InputError::Backend("SendInput(move) failed".into()));
                    }
                }
                InputEvent::MouseButton { button, pressed } => {
                    let flag = match (button, pressed) {
                        (MouseButton::Left, true) => kbm::MOUSEEVENTF_LEFTDOWN,
                        (MouseButton::Left, false) => kbm::MOUSEEVENTF_LEFTUP,
                        (MouseButton::Right, true) => kbm::MOUSEEVENTF_RIGHTDOWN,
                        (MouseButton::Right, false) => kbm::MOUSEEVENTF_RIGHTUP,
                        (MouseButton::Middle, true) => kbm::MOUSEEVENTF_MIDDLEDOWN,
                        (MouseButton::Middle, false) => kbm::MOUSEEVENTF_MIDDLEUP,
                        (MouseButton::Back, true) => kbm::MOUSEEVENTF_XDOWN,
                        (MouseButton::Back, false) => kbm::MOUSEEVENTF_XUP,
                        (MouseButton::Forward, true) => kbm::MOUSEEVENTF_XDOWN,
                        (MouseButton::Forward, false) => kbm::MOUSEEVENTF_XUP,
                    };
                    let mut input: kbm::INPUT = std::mem::zeroed();
                    input.r#type = kbm::INPUT_MOUSE;
                    input.Anonymous.mi.dwFlags = flag;
                    if kbm::SendInput(1, &mut input, std::mem::size_of::<kbm::INPUT>() as i32) == 0 {
                        return Err(InputError::Backend("SendInput(button) failed".into()));
                    }
                }
                InputEvent::MouseScroll { dx: _, dy } => {
                    let mut input: kbm::INPUT = std::mem::zeroed();
                    input.r#type = kbm::INPUT_MOUSE;
                    input.Anonymous.mi.mouseData = (dy * 120) as u32;
                    input.Anonymous.mi.dwFlags = kbm::MOUSEEVENTF_WHEEL;
                    if kbm::SendInput(1, &mut input, std::mem::size_of::<kbm::INPUT>() as i32) == 0 {
                        return Err(InputError::Backend("SendInput(wheel) failed".into()));
                    }
                }
                InputEvent::Key { key, pressed } => {
                    let KeyCode(code) = key;
                    let mut input: kbm::INPUT = std::mem::zeroed();
                    input.r#type = kbm::INPUT_KEYBOARD;
                    input.Anonymous.ki.wScan = code;
                    input.Anonymous.ki.dwFlags = if pressed { 0 } else { kbm::KEYEVENTF_KEYUP }
                        | kbm::KEYEVENTF_SCANCODE;
                    if kbm::SendInput(1, &mut input, std::mem::size_of::<kbm::INPUT>() as i32) == 0 {
                        return Err(InputError::Backend("SendInput(key) failed".into()));
                    }
                }
            }
        }
        Ok(())
    }

    fn release_all(&mut self) -> Result<(), InputError> {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse as kbm;
        let mut inputs: Vec<kbm::INPUT> = Vec::new();
        // SAFETY: zeroed POD followed by explicit field initialisation.
        unsafe {
            for flag in [
                kbm::MOUSEEVENTF_LEFTUP,
                kbm::MOUSEEVENTF_RIGHTUP,
                kbm::MOUSEEVENTF_MIDDLEUP,
            ] {
                let mut input: kbm::INPUT = std::mem::zeroed();
                input.r#type = kbm::INPUT_MOUSE;
                input.Anonymous.mi.dwFlags = flag;
                inputs.push(input);
            }
            if !inputs.is_empty() {
                kbm::SendInput(
                    inputs.len() as u32,
                    inputs.as_mut_ptr(),
                    std::mem::size_of::<kbm::INPUT>() as i32,
                );
            }
        }
        Ok(())
    }
}
