//! `uinput`-based injection.
//!
//! We create one combined virtual pointer+keyboard device. That covers
//! Wayland sessions (where XTest cannot reach native surfaces) and X11
//! alike, which is why the kernel-level route is the default here rather
//! than XTest.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::Path;

use crate::{InputError, InputEvent, InputInjector, KeyCode, MouseButton};

const UINPUT_PATH: &str = "/dev/uinput";

// ioctl request numbers from <linux/uinput.h>. Encoded as _IOW('U', n, int).
const fn iow_u(n: u8) -> libc::c_ulong {
    // _IOW('U', n, int) => dir=W(1), size=4, type=0x55, nr=n
    ((1u64 << 30) | ((4u64) << 16) | ((0x55u64) << 8) | (n as u64)) as libc::c_ulong
}
const UI_SET_EVBIT: libc::c_ulong = iow_u(100);
const UI_SET_KEYBIT: libc::c_ulong = iow_u(101);
const UI_SET_RELBIT: libc::c_ulong = iow_u(102);
const UI_SET_ABSBIT: libc::c_ulong = iow_u(103);
const UI_DEV_CREATE: libc::c_ulong = iow_u(1);
const UI_DEV_DESTROY: libc::c_ulong = iow_u(2);

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0x00;
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_WHEEL: u16 = 0x08;
const REL_HWHEEL: u16 = 0x06;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;

const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_SIDE: u16 = 0x113;
const BTN_EXTRA: u16 = 0x114;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InputEventRaw {
    time: libc::timeval,
    type_: u16,
    code: u16,
    value: i32,
}

/// Linux input device name is capped by the kernel at 80 bytes.
const UINPUT_MAX_NAME_SIZE: usize = 80;

#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [u8; UINPUT_MAX_NAME_SIZE],
    ff_effects_max: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

pub struct UinputInjector {
    file: File,
    width: u32,
    height: u32,
    pressed_keys: Vec<u16>,
    pressed_buttons: Vec<u16>,
}

fn set_bit(fd: i32, req: libc::c_ulong, bit: u16) -> Result<(), InputError> {
    // SAFETY: fd is a valid uinput fd owned by the caller; the ioctl takes an
    // int argument by value as the kernel expects for UI_SET_*BIT.
    let rc = unsafe { libc::ioctl(fd, req, bit as libc::c_int) };
    if rc < 0 {
        return Err(InputError::Backend(format!(
            "ioctl {req:#x} bit {bit} failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

impl UinputInjector {
    pub fn new() -> Result<Self, InputError> {
        if !Path::new(UINPUT_PATH).exists() {
            return Err(InputError::Unavailable(format!("{UINPUT_PATH} is missing")));
        }
        let file = OpenOptions::new()
            .write(true)
            .open(UINPUT_PATH)
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::PermissionDenied => InputError::PermissionDenied,
                _ => InputError::Backend(e.to_string()),
            })?;
        let fd = file.as_raw_fd();

        set_bit(fd, UI_SET_EVBIT, EV_KEY)?;
        set_bit(fd, UI_SET_EVBIT, EV_REL)?;
        set_bit(fd, UI_SET_EVBIT, EV_ABS)?;
        set_bit(fd, UI_SET_EVBIT, EV_SYN)?;

        for b in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE, BTN_EXTRA] {
            set_bit(fd, UI_SET_KEYBIT, b)?;
        }
        // A practical subset of the HID keyboard range.
        for k in 0x7000u16..0x7004u16 {
            let _ = k;
        }
        for k in KEY_RANGE {
            set_bit(fd, UI_SET_KEYBIT, *k)?;
        }
        for r in [REL_X, REL_Y, REL_WHEEL, REL_HWHEEL] {
            set_bit(fd, UI_SET_RELBIT, r)?;
        }
        for a in [ABS_X, ABS_Y] {
            set_bit(fd, UI_SET_ABSBIT, a)?;
        }

        let mut setup = UinputSetup {
            id: InputId {
                bustype: 0x03, // BUS_USB
                vendor: 0x0555,
                product: 0x0055,
                version: 1,
            },
            name: [0; UINPUT_MAX_NAME_SIZE],
            ff_effects_max: 0,
        };
        let name = b"uu-remote virtual input";
        setup.name[..name.len()].copy_from_slice(name);

        // SAFETY: UI_DEV_SETUP expects a UinputSetup pointer of matching layout.
        const UI_DEV_SETUP: libc::c_ulong = iow_u(3);
        let rc = unsafe { libc::ioctl(fd, UI_DEV_SETUP, &setup as *const UinputSetup) };
        if rc < 0 {
            return Err(InputError::Backend(format!(
                "UI_DEV_SETUP failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        // SAFETY: UI_DEV_CREATE takes no argument.
        let rc = unsafe { libc::ioctl(fd, UI_DEV_CREATE) };
        if rc < 0 {
            return Err(InputError::Backend(format!(
                "UI_DEV_CREATE failed: {}",
                std::io::Error::last_os_error()
            )));
        }

        tracing::info!("uinput virtual device created");
        Ok(Self {
            file,
            width: 1920,
            height: 1080,
            pressed_keys: Vec::new(),
            pressed_buttons: Vec::new(),
        })
    }

    fn emit(&mut self, type_: u16, code: u16, value: i32) -> Result<(), InputError> {
        let ev = InputEventRaw { time: libc::timeval { tv_sec: 0, tv_usec: 0 }, type_, code, value };
        // SAFETY: `ev` is a plain-old-data struct; writing its bytes to the
        // uinput fd is exactly what the kernel's write handler expects.
        let bytes = unsafe {
            std::slice::from_raw_parts(&ev as *const InputEventRaw as *const u8, std::mem::size_of::<InputEventRaw>())
        };
        self.file
            .write_all(bytes)
            .map_err(|e| InputError::Backend(e.to_string()))
    }

    fn sync(&mut self) -> Result<(), InputError> {
        self.emit(EV_SYN, SYN_REPORT, 0)
    }

    fn scale_x(&self, x: i32) -> i32 {
        ((x as i64 * 65535) / self.width.max(1) as i64).clamp(0, 65535) as i32
    }

    fn scale_y(&self, y: i32) -> i32 {
        ((y as i64 * 65535) / self.height.max(1) as i64).clamp(0, 65535) as i32
    }

    fn button_code(button: MouseButton) -> u16 {
        match button {
            MouseButton::Left => BTN_LEFT,
            MouseButton::Right => BTN_RIGHT,
            MouseButton::Middle => BTN_MIDDLE,
            MouseButton::Back => BTN_SIDE,
            MouseButton::Forward => BTN_EXTRA,
        }
    }
}

/// HID usage ids we map for the keyboard, covering the common block plus
/// modifier keys. Extended keys are passed through unchanged when they fall
/// inside the Linux `KEY_*` range.
const KEY_RANGE: &[u16] = &[
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26,
    27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49,
    50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72,
    73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 96, 97, 98, 99, 100, 101,
    102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 113, 114, 115, 116, 117, 118, 119, 120,
    121, 122, 123, 124, 125, 127, 128, 129, 130, 131, 132, 133, 134, 135, 136, 137, 138, 139,
    158, 159, 160, 161, 162, 163, 164, 165, 166, 167, 168, 169, 170, 171, 172, 173, 174, 175,
    176, 177, 178, 179, 180, 181, 183, 184, 185, 186, 187, 188, 189, 190, 191, 192, 193, 194,
    200, 201, 202, 203, 204, 205, 206, 207, 208, 209,
];

impl InputInjector for UinputInjector {
    fn set_screen(&mut self, width: u32, height: u32) -> Result<(), InputError> {
        self.width = width.max(1);
        self.height = height.max(1);
        Ok(())
    }

    fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
        match event {
            InputEvent::MouseMoveAbsolute { x, y } => {
                let (sx, sy) = (self.scale_x(x), self.scale_y(y));
                self.emit(EV_ABS, ABS_X, sx)?;
                self.emit(EV_ABS, ABS_Y, sy)?;
                self.sync()
            }
            InputEvent::MouseMoveRelative { dx, dy } => {
                self.emit(EV_REL, REL_X, dx)?;
                self.emit(EV_REL, REL_Y, dy)?;
                self.sync()
            }
            InputEvent::MouseButton { button, pressed } => {
                let code = Self::button_code(button);
                if pressed {
                    if !self.pressed_buttons.contains(&code) {
                        self.pressed_buttons.push(code);
                    }
                } else {
                    self.pressed_buttons.retain(|c| *c != code);
                }
                self.emit(EV_KEY, code, i32::from(pressed))?;
                self.sync()
            }
            InputEvent::MouseScroll { dx, dy } => {
                if dy != 0 {
                    self.emit(EV_REL, REL_WHEEL, dy)?;
                }
                if dx != 0 {
                    self.emit(EV_REL, REL_HWHEEL, dx)?;
                }
                self.sync()
            }
            InputEvent::Key { key, pressed } => {
                let KeyCode(code) = key;
                if pressed {
                    if !self.pressed_keys.contains(&code) {
                        self.pressed_keys.push(code);
                    }
                } else {
                    self.pressed_keys.retain(|c| *c != code);
                }
                self.emit(EV_KEY, code, i32::from(pressed))?;
                self.sync()
            }
        }
    }

    fn release_all(&mut self) -> Result<(), InputError> {
        for code in std::mem::take(&mut self.pressed_buttons) {
            self.emit(EV_KEY, code, 0)?;
        }
        for code in std::mem::take(&mut self.pressed_keys) {
            self.emit(EV_KEY, code, 0)?;
        }
        self.sync()
    }
}

impl Drop for UinputInjector {
    fn drop(&mut self) {
        let _ = self.release_all();
        // SAFETY: destroying a device we created through the same fd.
        unsafe { libc::ioctl(self.file.as_raw_fd(), UI_DEV_DESTROY) };
    }
}
