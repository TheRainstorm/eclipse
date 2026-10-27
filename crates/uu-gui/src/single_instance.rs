//! Reopen the existing Windows launcher; stream children keep separate windows.
#[cfg(windows)]
pub struct Launcher(windows_sys::Win32::Foundation::HANDLE);
#[cfg(not(windows))]
pub struct Launcher;
#[cfg(windows)]
impl Drop for Launcher {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// None means an existing launcher was notified; no new GUI should be created.
pub fn launcher() -> std::io::Result<Option<Launcher>> {
    #[cfg(windows)]
    {
        windows::claim()
    }
    #[cfg(not(windows))]
    {
        Ok(Some(Launcher))
    }
}
#[cfg(windows)]
mod windows {
    use super::Launcher;
    use std::{io, path::Path, time::Duration};
    use windows_sys::Win32::{Foundation::*, System::Threading::*, UI::WindowsAndMessaging::*};
    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }
    fn mutex(name: &str) -> io::Result<(Launcher, bool)> {
        unsafe {
            let handle = CreateMutexW(std::ptr::null(), 0, wide(name).as_ptr());
            let existed = GetLastError() == ERROR_ALREADY_EXISTS;
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            Ok((Launcher(handle), existed))
        }
    }
    unsafe fn existing_launcher() -> Option<HWND> {
        // Exact application title only: do not enumerate other window titles.
        let window = FindWindowW(std::ptr::null(), wide(super::super::brand::NAME).as_ptr());
        if window.is_null() {
            return None;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(window, &mut pid);
        if pid == 0 || pid == GetCurrentProcessId() {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut path = vec![0u16; 32768];
        let mut length = path.len() as u32;
        let queried = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) != 0;
        CloseHandle(process);
        if !queried {
            return None;
        }
        let image = String::from_utf16_lossy(&path[..length as usize]);
        let current = std::env::current_exe().ok()?;
        let expected = current.file_name()?.to_str()?;
        Path::new(&image)
            .file_name()?
            .to_str()?
            .eq_ignore_ascii_case(expected)
            .then_some(window)
    }
    unsafe fn activate(window: HWND) {
        if IsIconic(window) != 0 {
            ShowWindow(window, SW_RESTORE);
        } else if IsWindowVisible(window) == 0 {
            ShowWindow(window, SW_SHOW);
        }
        if SetForegroundWindow(window) == 0 {
            let info = FLASHWINFO {
                cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
                hwnd: window,
                dwFlags: FLASHW_TRAY,
                uCount: 3,
                dwTimeout: 0,
            };
            FlashWindowEx(&info);
        }
    }
    pub fn claim() -> io::Result<Option<Launcher>> {
        unsafe {
            // Also recognize pre-mutex releases, so upgrading does not create
            // a second launcher beside an older version already on screen.
            if let Some(window) = existing_launcher() {
                activate(window);
                return Ok(None);
            }
        }
        // Local isolates Windows logon sessions; do not scope by exe path or
        // revision, otherwise updating would silently start a second launcher.
        let (guard, existed) = mutex(r"Local\uu-client.launcher.v1")?;
        if !existed {
            return Ok(Some(guard));
        }
        drop(guard);
        for _ in 0..40 {
            if let Some(window) = unsafe { existing_launcher() } {
                unsafe {
                    activate(window);
                }
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                wide(uu_i18n::text("客户端正在启动或退出，请稍后再次打开。")).as_ptr(),
                wide(super::super::brand::NAME).as_ptr(),
                MB_OK | MB_ICONINFORMATION,
            );
        }
        Ok(None)
    }
    #[cfg(test)]
    #[test]
    fn launcher_mutex_lives_until_last_owner_closes_and_does_not_lock_streams() {
        let name = format!(r"Local\uu-client-launcher-test-{}", uuid::Uuid::new_v4());
        let (first, existed) = mutex(&name).unwrap();
        assert!(!existed);
        let (second, existed) = mutex(&name).unwrap();
        assert!(existed);
        drop(second);
        drop(first);
        let (_reopened, existed) = mutex(&name).unwrap();
        assert!(!existed);
    }
}
