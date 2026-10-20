//! Own noninteractive window station, never attaches or switches the user desktop.
use anyhow::{ensure, Context, Result};
use std::ptr::{null, null_mut};
use windows_sys::Win32::{
    Foundation::*,
    System::{StationsAndDesktops::*, Threading::GetCurrentThreadId},
};
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn check(value: bool, label: &str) -> Result<()> {
    ensure!(value, "{label}: {}", std::io::Error::last_os_error());
    Ok(())
}
pub struct Station {
    original: HWINSTA,
    original_desktop: HDESK,
    station: HWINSTA,
    desktop: HDESK,
}
impl Station {
    pub fn create() -> Result<Self> {
        unsafe {
            let original = GetProcessWindowStation();
            let original_desktop = GetThreadDesktop(GetCurrentThreadId());
            check(
                !original.is_null() && !original_desktop.is_null(),
                "cannot record process GUI context",
            )?;
            let name = format!("uu-client-video-fixture-{}", uuid::Uuid::new_v4());
            let name_w = wide(&name);
            // CWF_CREATE_ONLY: never open a preexisting station. No privilege
            // adjustment, alternate-user login, or interactive-desktop switch.
            let station = CreateWindowStationW(
                name_w.as_ptr(),
                1,
                0x0004 | 0x0008 | 0x0002 | 0x0020 | 0x0001,
                null(),
            );
            check(!station.is_null(), "cannot create isolated video station")?;
            let mut guard = Self {
                original,
                original_desktop,
                station,
                desktop: null_mut(),
            };
            check(
                SetProcessWindowStation(station) != 0,
                "cannot attach isolated station",
            )?;
            guard.desktop = CreateDesktopW(
                wide("Default").as_ptr(),
                null(),
                null(),
                0,
                DESKTOP_CREATEWINDOW
                    | DESKTOP_CREATEMENU
                    | DESKTOP_READOBJECTS
                    | DESKTOP_WRITEOBJECTS,
                null(),
            );
            check(!guard.desktop.is_null(), "cannot create isolated desktop")?;
            check(
                SetThreadDesktop(guard.desktop) != 0,
                "cannot attach isolated desktop",
            )?;
            let mut actual = [0u16; 512];
            let mut needed = 0;
            check(
                GetUserObjectInformationW(
                    GetProcessWindowStation(),
                    UOI_NAME,
                    actual.as_mut_ptr().cast(),
                    std::mem::size_of_val(&actual) as u32,
                    &mut needed,
                ) != 0,
                "cannot verify isolated station",
            )?;
            let end = actual
                .iter()
                .position(|&v| v == 0)
                .context("station name not terminated")?;
            ensure!(
                String::from_utf16(&actual[..end])? == name,
                "refusing video rendering outside our newly created station"
            );
            Ok(guard)
        }
    }
}
impl Drop for Station {
    fn drop(&mut self) {
        unsafe {
            // All GL contexts and our own window must be dropped first.
            let _ = SetProcessWindowStation(self.original);
            let _ = SetThreadDesktop(self.original_desktop);
            if !self.desktop.is_null() {
                let _ = CloseDesktop(self.desktop);
            }
            let _ = CloseWindowStation(self.station);
        }
    }
}
