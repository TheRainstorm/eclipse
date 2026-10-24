//! Bind only windowless capture/input workers to the OS input desktop.
//! This never changes the visible desktop or grants access to a secure desktop.
use std::{cell::RefCell, io, ptr};
use windows_sys::Win32::{
    Foundation::HANDLE,
    System::{StationsAndDesktops::*, Threading::GetCurrentThreadId},
    UI::HiDpi::{
        SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    },
};

/// Scope physical pixel queries to this thread and restore its caller's DPI
/// context on every exit path. The raw context also keeps this guard !Send.
#[must_use]
pub struct PhysicalCoordinates {
    previous: DPI_AWARENESS_CONTEXT,
}
impl PhysicalCoordinates {
    pub fn enter() -> io::Result<Self> {
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if previous.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { previous })
    }
}
impl Drop for PhysicalCoordinates {
    fn drop(&mut self) {
        unsafe {
            SetThreadDpiAwarenessContext(self.previous);
        }
    }
}

struct Binding {
    original: HDESK,
    owned: HDESK,
    access: u32,
}
impl Drop for Binding {
    fn drop(&mut self) {
        // SetThreadDesktop's handle must remain valid while it is assigned.
        unsafe {
            if !self.owned.is_null() && SetThreadDesktop(self.original) != 0 {
                CloseDesktop(self.owned);
            }
        }
    }
}
thread_local! {
    static BINDING: RefCell<Binding> = RefCell::new(Binding {
        original: unsafe { GetThreadDesktop(GetCurrentThreadId()) },
        owned: ptr::null_mut(),
        access: 0,
    });
}

fn name(desktop: HDESK) -> io::Result<Vec<u16>> {
    let mut length = 0;
    unsafe {
        GetUserObjectInformationW(desktop as HANDLE, UOI_NAME, ptr::null_mut(), 0, &mut length);
    }
    if length == 0 || length > 4096 {
        return Err(io::Error::last_os_error());
    }
    let mut value = vec![0u16; (length as usize + 1) / 2];
    if unsafe {
        GetUserObjectInformationW(
            desktop as HANDLE,
            UOI_NAME,
            value.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(value)
}

/// Returns true when this worker changes desktop. Capture workers call this
/// before opening DXGI duplication. GUI threads must not call it.
pub fn sync_input_desktop() -> io::Result<bool> {
    sync_desktop(DESKTOP_READOBJECTS | DESKTOP_WRITEOBJECTS | DESKTOP_SWITCHDESKTOP)
}

/// SendInput also requires journal playback access on the attached desktop.
/// This is a desktop handle right, not a journal hook or a token privilege.
pub fn sync_injection_desktop() -> io::Result<bool> {
    sync_desktop(
        DESKTOP_READOBJECTS
            | DESKTOP_WRITEOBJECTS
            | DESKTOP_SWITCHDESKTOP
            | DESKTOP_JOURNALPLAYBACK,
    )
}

fn sync_desktop(access: u32) -> io::Result<bool> {
    let input = unsafe { OpenInputDesktop(0, 0, access) };
    if input.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut retained = false;
    let result = (|| {
        let current = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
        let changed = name(current)? != name(input)?;
        BINDING.with(|binding| {
            let mut binding = binding.borrow_mut();
            // The desktop name alone does not describe the assigned handle's
            // access. Bind our checked handle on first use and upgrade a capture
            // binding before injecting, even when the desktop did not change.
            if !changed && current == binding.owned && binding.access & access == access {
                return Ok(false);
            }
            if unsafe { SetThreadDesktop(input) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let old = std::mem::replace(&mut binding.owned, input);
            binding.access = access;
            retained = true;
            if !old.is_null() {
                unsafe {
                    CloseDesktop(old);
                }
            }
            Ok(changed)
        })
    })();
    if !retained {
        unsafe {
            CloseDesktop(input);
        }
    }
    result
}

pub fn access_error(error: &io::Error) -> String {
    format!("无法访问当前输入桌面（{error}）；锁屏或 UAC 安全桌面需要安装并启动 Windows 后台服务；后台服务还需要已登录的控制台会话和有效显示输出")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_coordinates_restore_the_callers_context_after_nested_and_failed_queries() {
        use windows_sys::Win32::UI::HiDpi::{
            AreDpiAwarenessContextsEqual, GetThreadDpiAwarenessContext,
            DPI_AWARENESS_CONTEXT_UNAWARE,
        };
        std::thread::spawn(|| unsafe {
            let original = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_UNAWARE);
            assert!(!original.is_null());
            let result = (|| -> io::Result<()> {
                let _outer = PhysicalCoordinates::enter()?;
                {
                    let _inner = PhysicalCoordinates::enter()?;
                }
                assert_ne!(
                    AreDpiAwarenessContextsEqual(
                        GetThreadDpiAwarenessContext(),
                        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
                    ),
                    0
                );
                Err(io::Error::other("owned query failure"))
            })();
            assert!(result.is_err());
            assert_ne!(
                AreDpiAwarenessContextsEqual(
                    GetThreadDpiAwarenessContext(),
                    DPI_AWARENESS_CONTEXT_UNAWARE
                ),
                0
            );
            SetThreadDpiAwarenessContext(original);
        })
        .join()
        .unwrap();
    }

    #[test]
    #[ignore = "requires an accessible interactive input desktop; never switches the visible desktop"]
    fn windowless_worker_follows_input_and_restores_its_original_desktop() {
        std::thread::spawn(|| unsafe {
            let original = GetThreadDesktop(GetCurrentThreadId());
            let input = OpenInputDesktop(
                0,
                0,
                DESKTOP_READOBJECTS | DESKTOP_WRITEOBJECTS | DESKTOP_SWITCHDESKTOP,
            );
            assert!(
                !input.is_null(),
                "interactive desktop is required for this explicit fixture"
            );
            let input_name = name(input).unwrap();
            CloseDesktop(input);
            let fixture_name: Vec<u16> = format!("eclipse-owned-{}\0", uuid::Uuid::new_v4())
                .encode_utf16()
                .collect();
            let fixture = CreateDesktopW(
                fixture_name.as_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                DESKTOP_READOBJECTS
                    | DESKTOP_WRITEOBJECTS
                    | DESKTOP_CREATEWINDOW
                    | DESKTOP_SWITCHDESKTOP,
                ptr::null(),
            );
            assert!(!fixture.is_null());
            assert_ne!(SetThreadDesktop(fixture), 0);
            assert!(sync_input_desktop().unwrap());
            for _ in 0..128 {
                assert!(!sync_input_desktop().unwrap());
            }
            assert_eq!(
                name(GetThreadDesktop(GetCurrentThreadId())).unwrap(),
                input_name
            );
            // Explicitly run the TLS guard while the private original is valid.
            BINDING.with(|slot| {
                let previous = std::mem::replace(
                    &mut *slot.borrow_mut(),
                    Binding {
                        original,
                        owned: ptr::null_mut(),
                        access: 0,
                    },
                );
                drop(previous);
            });
            assert_eq!(
                name(GetThreadDesktop(GetCurrentThreadId())).unwrap(),
                name(fixture).unwrap()
            );
            assert_ne!(SetThreadDesktop(original), 0);
            assert_ne!(CloseDesktop(fixture), 0);
        })
        .join()
        .unwrap();
    }

    #[test]
    #[ignore = "requires accessible interactive desktop; sends only mouse packets with no action flags"]
    fn injection_binding_upgrades_same_desktop_access_without_mouse_actions() {
        std::thread::spawn(|| {
            use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT, INPUT_MOUSE};
            fn action_free_packet() -> u32 {
                let mut packet: INPUT = unsafe { std::mem::zeroed() };
                packet.r#type = INPUT_MOUSE;
                // All mouse action flags are zero: no pointer movement, click,
                // button release, scrolling, keyboard input or consent action.
                unsafe { SendInput(1, &packet, std::mem::size_of::<INPUT>() as i32) }
            }
            assert!(!sync_input_desktop().unwrap());
            assert_eq!(
                action_free_packet(),
                0,
                "capture binding lacks playback access"
            );
            assert_eq!(io::Error::last_os_error().raw_os_error(), Some(5));
            // Equal desktop names must still permit upgrading the handle.
            assert!(!sync_injection_desktop().unwrap());
            assert_eq!(
                action_free_packet(),
                1,
                "injection binding must permit SendInput"
            );
            assert!(!sync_input_desktop().unwrap());
            assert!(!sync_injection_desktop().unwrap());
            assert_eq!(
                action_free_packet(),
                1,
                "capture sync must not downgrade access"
            );
        })
        .join()
        .unwrap();
    }
}
