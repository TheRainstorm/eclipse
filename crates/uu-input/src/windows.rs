//! Windows desktop input through the documented SendInput API.
use crate::{DesktopRect, HeldInput, InputError, InputEvent, InputInjector, MouseButton};
use windows_sys::Win32::UI::{Input::KeyboardAndMouse as kbm, WindowsAndMessaging as wm};

#[derive(Default)]
pub struct SendInputInjector {
    held: HeldInput,
    desktop_reset: bool,
}
impl SendInputInjector {
    pub fn new() -> Self {
        Self::default()
    }
    fn prepare_desktop(&mut self) -> Result<(), InputError> {
        let changed = crate::windows_desktop::sync_injection_desktop()
            .map_err(|e| InputError::Unavailable(crate::windows_desktop::access_error(&e)))?;
        self.desktop_reset |= changed;
        if self.desktop_reset {
            // Only release this session's successful presses before admitting
            // events on the new desktop. Failed releases remain owned for retry.
            self.release_owned()?;
            self.desktop_reset = false;
        }
        Ok(())
    }
    fn release_owned(&mut self) -> Result<(), InputError> {
        let mut error = None;
        for event in self.held.releases() {
            if let Err(e) = self.send(event) {
                error.get_or_insert(e);
            }
        }
        error.map_or(Ok(()), Err)
    }
}

fn virtual_screen() -> Result<DesktopRect, InputError> {
    // DXGI capture and normalized pointer coordinates use physical pixels.
    // A CLI host need not have a DPI-aware GUI, so query in a temporary
    // per-monitor context on this input thread, then restore its old context.
    let _coordinates = crate::windows_desktop::PhysicalCoordinates::enter()
        .map_err(|error| InputError::Unavailable(format!("physical desktop coordinates are unavailable: {error}")))?;
    let (left, top, width, height) = unsafe {
        (
            wm::GetSystemMetrics(wm::SM_XVIRTUALSCREEN),
            wm::GetSystemMetrics(wm::SM_YVIRTUALSCREEN),
            wm::GetSystemMetrics(wm::SM_CXVIRTUALSCREEN),
            wm::GetSystemMetrics(wm::SM_CYVIRTUALSCREEN),
        )
    };
    if width <= 0 || height <= 0 {
        return Err(InputError::Unavailable("no active virtual desktop".into()));
    }
    DesktopRect::new(left, top, width as u32, height as u32)
}
fn mouse(flags: u32, data: u32, dx: i32, dy: i32) -> kbm::INPUT {
    let mut input: kbm::INPUT = unsafe { std::mem::zeroed() };
    input.r#type = kbm::INPUT_MOUSE;
    input.Anonymous.mi = kbm::MOUSEINPUT {
        dx,
        dy,
        mouseData: data,
        dwFlags: flags,
        time: 0,
        dwExtraInfo: 0,
    };
    input
}
fn packets(event: InputEvent, screen: DesktopRect) -> Result<Vec<kbm::INPUT>, InputError> {
    let mut out = Vec::with_capacity(2);
    match event {
        InputEvent::MouseMoveAbsolute { x, y } => {
            let (x, y) = screen.windows_absolute(x, y);
            out.push(mouse(
                kbm::MOUSEEVENTF_MOVE | kbm::MOUSEEVENTF_ABSOLUTE | kbm::MOUSEEVENTF_VIRTUALDESK,
                0,
                x,
                y,
            ));
        }
        InputEvent::MouseMoveRelative { dx, dy } => {
            out.push(mouse(kbm::MOUSEEVENTF_MOVE, 0, dx, dy))
        }
        InputEvent::MouseButton { button, pressed } => {
            let (down, up, data) = match button {
                MouseButton::Left => (kbm::MOUSEEVENTF_LEFTDOWN, kbm::MOUSEEVENTF_LEFTUP, 0),
                MouseButton::Right => (kbm::MOUSEEVENTF_RIGHTDOWN, kbm::MOUSEEVENTF_RIGHTUP, 0),
                MouseButton::Middle => (kbm::MOUSEEVENTF_MIDDLEDOWN, kbm::MOUSEEVENTF_MIDDLEUP, 0),
                MouseButton::Back => (kbm::MOUSEEVENTF_XDOWN, kbm::MOUSEEVENTF_XUP, 1),
                MouseButton::Forward => (kbm::MOUSEEVENTF_XDOWN, kbm::MOUSEEVENTF_XUP, 2),
            };
            out.push(mouse(if pressed { down } else { up }, data, 0, 0));
        }
        InputEvent::MouseScroll { dx, dy } => {
            if dx != 0 {
                out.push(mouse(kbm::MOUSEEVENTF_HWHEEL, dx as u32, 0, 0));
            }
            if dy != 0 {
                out.push(mouse(kbm::MOUSEEVENTF_WHEEL, dy as u32, 0, 0));
            }
        }
        InputEvent::Key { key, pressed } => {
            let scan = key.mapping()?.win;
            if scan == 0 || scan == 0xffff {
                return Err(InputError::UnsupportedKey(key.0));
            }
            let mut input: kbm::INPUT = unsafe { std::mem::zeroed() };
            input.r#type = kbm::INPUT_KEYBOARD;
            let mut flags = if pressed { 0 } else { kbm::KEYEVENTF_KEYUP };
            if key.0 == 0x48 {
                // Pause uses a multi-byte E1 hardware sequence; let Windows
                // generate its virtual-key form instead of sending a partial scan.
                input.Anonymous.ki.wVk = 0x13;
            } else {
                input.Anonymous.ki.wScan = scan & 0xff;
                flags |= kbm::KEYEVENTF_SCANCODE;
                if scan & 0xff00 == 0xe000 {
                    flags |= kbm::KEYEVENTF_EXTENDEDKEY;
                } else if scan > 0xff {
                    return Err(InputError::UnsupportedKey(key.0));
                }
            }
            input.Anonymous.ki.dwFlags = flags;
            out.push(input);
        }
    }
    Ok(out)
}
impl SendInputInjector {
    fn send(&mut self, event: InputEvent) -> Result<(), InputError> {
        if !self.held.should_emit(event) {
            return Ok(());
        }
        let mut inputs = packets(event, virtual_screen()?)?;
        if !inputs.is_empty() {
            let sent = unsafe {
                kbm::SendInput(
                    inputs.len() as u32,
                    inputs.as_mut_ptr(),
                    std::mem::size_of::<kbm::INPUT>() as i32,
                )
            };
            if sent != inputs.len() as u32 {
                return Err(InputError::Backend(
                    format!("SendInput accepted {sent}/{} events (Windows error {}); check the active desktop and target window permissions",inputs.len(),std::io::Error::last_os_error()),
                ));
            }
        }
        self.held.applied(event);
        Ok(())
    }
}
impl InputInjector for SendInputInjector {
    fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
        self.prepare_desktop()?;
        self.send(event)
    }
    fn release_all(&mut self) -> Result<(), InputError> {
        // No desktop access is needed when this session owns no presses.
        if self.held.releases().is_empty() {return Ok(());}
        self.prepare_desktop()?;
        self.release_owned()
    }
}
impl Drop for SendInputInjector {
    fn drop(&mut self) {
        let _ = self.release_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KeyCode;
    #[test]
    fn empty_release_does_not_require_an_interactive_desktop() {
        // This also runs in a service/SSH test process without desktop access.
        assert!(SendInputInjector::new().release_all().is_ok());
    }
    fn screen() -> DesktopRect {
        DesktopRect::new(-1920, 0, 3840, 1080).unwrap()
    }
    #[test]
    fn hid_keys_translate_to_scan_codes_and_extended_flags_without_injecting() {
        for (usage, scan, extended) in [
            (4, 0x1e, false),
            (0x33, 0x27, false),
            (0xe4, 0x1d, true),
            (0x58, 0x1c, true),
        ] {
            let key = packets(
                InputEvent::Key {
                    key: KeyCode(usage),
                    pressed: true,
                },
                screen(),
            )
            .unwrap();
            let key = unsafe { key[0].Anonymous.ki };
            assert_eq!(key.wScan, scan);
            assert_eq!(key.dwFlags & kbm::KEYEVENTF_EXTENDEDKEY != 0, extended);
            assert_ne!(key.dwFlags & kbm::KEYEVENTF_SCANCODE, 0);
        }
    }
    #[test]
    fn extra_buttons_and_both_wheel_axes_keep_their_platform_data() {
        for (button, data) in [(MouseButton::Back, 1), (MouseButton::Forward, 2)] {
            let packet = packets(
                InputEvent::MouseButton {
                    button,
                    pressed: true,
                },
                screen(),
            )
            .unwrap();
            let mouse = unsafe { packet[0].Anonymous.mi };
            assert_eq!(mouse.mouseData, data);
            assert_eq!(mouse.dwFlags, kbm::MOUSEEVENTF_XDOWN);
        }
        let packet = packets(InputEvent::MouseScroll { dx: 120, dy: -120 }, screen()).unwrap();
        assert_eq!(
            unsafe { packet[0].Anonymous.mi.dwFlags },
            kbm::MOUSEEVENTF_HWHEEL
        );
        assert_eq!(unsafe { packet[1].Anonymous.mi.mouseData } as i32, -120);
    }
}
