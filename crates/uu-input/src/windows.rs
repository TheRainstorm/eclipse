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
    // The caller keeps the physical DPI context through SendInput as well.
    // On mixed-DPI desktops SendInput maps absolute coordinates using its
    // caller's context, so restoring it after this query changes the scale.
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
fn unicode_packets(text: &str) -> Vec<kbm::INPUT> {
    text.encode_utf16().flat_map(|unit| [false,true].map(|released| {
        let mut input:kbm::INPUT=unsafe {std::mem::zeroed()};
        input.r#type=kbm::INPUT_KEYBOARD;
        input.Anonymous.ki.wScan=unit;
        input.Anonymous.ki.dwFlags=kbm::KEYEVENTF_UNICODE | if released {kbm::KEYEVENTF_KEYUP}else{0};
        input
    })).collect()
}
#[cfg(test)]
#[test]
fn unicode_input_preserves_utf16_surrogates_and_has_balanced_transitions() {
    let packets=unicode_packets("中🙂");
    let units:Vec<u16>="中🙂".encode_utf16().collect();
    assert_eq!(packets.len(),units.len()*2);
    for (pair,unit) in packets.chunks_exact(2).zip(units) {
        let down=unsafe{pair[0].Anonymous.ki};let up=unsafe{pair[1].Anonymous.ki};
        assert_eq!((down.wVk,down.wScan,down.dwFlags),(0,unit,kbm::KEYEVENTF_UNICODE));
        assert_eq!((up.wVk,up.wScan,up.dwFlags),(0,unit,kbm::KEYEVENTF_UNICODE|kbm::KEYEVENTF_KEYUP));
    }
}
impl SendInputInjector {
    fn send(&mut self, event: InputEvent) -> Result<(), InputError> {
        if !self.held.should_emit(event) {
            return Ok(());
        }
        let _coordinates = crate::windows_desktop::PhysicalCoordinates::enter()
            .map_err(|error| InputError::Unavailable(format!("physical desktop coordinates are unavailable: {error}")))?;
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
    fn supports_text(&self)->bool {true}
    fn text(&mut self,value:&str)->Result<(),InputError> {
        if value.len()>16*1024 || value.contains('\0') {return Err(InputError::Backend("invalid text input".into()));}
        self.prepare_desktop()?;
        for inputs in unicode_packets(value).chunks(128) {
            let sent=unsafe {kbm::SendInput(inputs.len() as u32,inputs.as_ptr(),std::mem::size_of::<kbm::INPUT>() as i32)};
            if sent!=inputs.len() as u32 {return Err(InputError::Backend("Unicode input was not fully accepted".into()));}
        }
        Ok(())
    }
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
    #[test]
    #[ignore = "explicit interactive cursor-only check: moves across attached monitors and restores the original position; never clicks"]
    fn absolute_mouse_preserves_physical_coordinates_and_restores_caller_dpi() {
        std::thread::spawn(|| unsafe {
            use windows_sys::Win32::{
                Foundation::{POINT, RECT},
                Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR},
                UI::HiDpi::{AreDpiAwarenessContextsEqual, GetThreadDpiAwarenessContext,
                    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_UNAWARE},
            };
            struct Caller(DPI_AWARENESS_CONTEXT);
            impl Drop for Caller { fn drop(&mut self) { unsafe { SetThreadDpiAwarenessContext(self.0); } } }
            struct Cursor(POINT);
            impl Drop for Cursor {
                fn drop(&mut self) { unsafe {
                    let _physical = crate::windows_desktop::PhysicalCoordinates::enter().unwrap();
                    assert_ne!(wm::SetPhysicalCursorPos(self.0.x, self.0.y), 0);
                } }
            }
            unsafe extern "system" fn monitor(_: HMONITOR, _: HDC, rect: *mut RECT, data: isize) -> i32 {
                (*(data as *mut Vec<RECT>)).push(*rect);
                1
            }
            let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_UNAWARE);
            assert!(!previous.is_null());
            let _caller = Caller(previous);
            let mut monitors = Vec::<RECT>::new();
            let mut position: POINT = std::mem::zeroed();
            {
                let _physical = crate::windows_desktop::PhysicalCoordinates::enter().unwrap();
                assert_ne!(wm::GetPhysicalCursorPos(&mut position), 0);
                assert_ne!(EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(),
                    Some(monitor), (&mut monitors as *mut Vec<RECT>) as isize), 0);
            }
            let _cursor = Cursor(position);
            assert!(!monitors.is_empty());
            let mut injector = SendInputInjector::new();
            for rect in monitors {
                for fraction in [1, 2, 3] {
                    let x = rect.left + (rect.right - rect.left) * fraction / 4;
                    let y = rect.top + (rect.bottom - rect.top) / 2;
                    injector.inject(InputEvent::MouseMoveAbsolute { x, y }).unwrap();
                    assert_ne!(AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(),
                        DPI_AWARENESS_CONTEXT_UNAWARE), 0);
                    std::thread::sleep(std::time::Duration::from_millis(15));
                    let _physical = crate::windows_desktop::PhysicalCoordinates::enter().unwrap();
                    let mut actual: POINT = std::mem::zeroed();
                    assert_ne!(wm::GetPhysicalCursorPos(&mut actual), 0);
                    println!("requested=({x},{y}) physical=({},{})", actual.x, actual.y);
                    assert!((actual.x - x).abs() <= 1 && (actual.y - y).abs() <= 1,
                        "absolute pointer escaped the physical capture coordinate system");
                }
            }
        }).join().unwrap();
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
