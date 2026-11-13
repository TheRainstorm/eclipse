//! One launcher per configuration store; versions sharing a store
//! reuse its launcher, while portable profiles remain independent.
use std::io;
#[cfg(any(windows, target_os = "linux", test))]
use std::path::{Component, Path, PathBuf};

#[cfg(any(windows, target_os = "linux", test))]
fn profile_key(directory: &Path) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    let absolute = if directory.is_absolute() { directory.to_path_buf() }
        else { std::env::current_dir()?.join(directory) };
    let mut normalized = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::CurDir => {},
            Component::ParentDir => { normalized.pop(); },
            _ => normalized.push(part.as_os_str()),
        }
    }
    // The first launch can precede creation of data/. Resolve its existing
    // ancestor so the key remains the same after the store has been created.
    let mut ancestor = normalized.as_path();
    let mut tail = Vec::new();
    while !ancestor.try_exists()? {
        tail.push(ancestor.file_name().ok_or_else(|| io::Error::other("configuration directory unavailable"))?);
        ancestor = ancestor.parent().ok_or_else(|| io::Error::other("configuration directory unavailable"))?;
    }
    let mut path = ancestor.canonicalize()?;
    for part in tail.into_iter().rev() { path.push(part); }
    #[cfg(windows)]
    let identity = path.to_string_lossy().to_lowercase().into_bytes();
    #[cfg(not(windows))]
    let identity = path.as_os_str().as_encoded_bytes().to_vec();
    Ok(format!("{:x}", Sha256::digest(identity)))
}
#[cfg(windows)]
pub struct Launcher(windows_sys::Win32::Foundation::HANDLE);
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::Launcher;
#[cfg(not(any(windows, target_os = "linux")))]
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
pub fn launcher(config: &uu_core::Config) -> std::io::Result<Option<Launcher>> {
    #[cfg(windows)]
    {
        windows::claim(config)
    }
    #[cfg(target_os = "linux")]
    { linux::claim(config) }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = config;
        Ok(Some(Launcher))
    }
}

pub fn take_activation(context: &eframe::egui::Context) -> bool {
    #[cfg(target_os = "linux")]
    { linux::take_activation(context) }
    #[cfg(not(target_os = "linux"))]
    { let _ = context; false }
}

#[cfg(windows)]
pub struct WindowMarker { window: isize, property: Vec<u16> }
#[cfg(not(windows))]
pub struct WindowMarker;

/// Legacy releases find the literal title "Eclipse" before checking their
/// global mutex. Give independent profiles a stable, distinct title so an old
/// executable can still open its ordinary launcher alongside a portable copy.
pub fn window_title(config: &uu_core::Config) -> String {
    if config.profile_dir().is_some() { format!("{} · Portable", super::brand::NAME) }
    else { super::brand::NAME.into() }
}

/// Register only the home window, never a stream child or a settings window.
pub fn mark_window(window: isize, config: &uu_core::Config) -> io::Result<Option<WindowMarker>> {
    #[cfg(windows)]
    { windows::mark_window(window, config) }
    #[cfg(not(windows))]
    { let _ = (window, config); Ok(None) }
}

#[cfg(windows)]
impl Drop for WindowMarker {
    fn drop(&mut self) {
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::RemovePropW;
            RemovePropW(self.window as _, self.property.as_ptr());
            RemovePropW(self.window as _, windows::wide(windows::PROFILE_PROPERTY).as_ptr());
        }
    }
}
#[cfg(windows)]
mod windows {
    use super::{Launcher, WindowMarker};
    use std::{io, path::Path, time::Duration};
    use windows_sys::Win32::{Foundation::*, System::Threading::*, UI::WindowsAndMessaging::*};
    pub(super) const PROFILE_PROPERTY: &str = "EclipseLauncherProfile.v1";
    pub(super) fn wide(value: &str) -> Vec<u16> {
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
    unsafe fn same_application(window: HWND) -> bool {
        let mut pid = 0;
        GetWindowThreadProcessId(window, &mut pid);
        if pid == 0 || pid == GetCurrentProcessId() {
            return false;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return false;
        }
        let mut path = vec![0u16; 32768];
        let mut length = path.len() as u32;
        let queried = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) != 0;
        CloseHandle(process);
        if !queried {
            return false;
        }
        let image = String::from_utf16_lossy(&path[..length as usize]);
        let Ok(current) = std::env::current_exe() else { return false; };
        Path::new(&image).file_name().zip(current.file_name())
            .is_some_and(|(actual, expected)| actual.to_string_lossy().eq_ignore_ascii_case(&expected.to_string_lossy()))
    }
    unsafe fn existing_launcher(property: &[u16], recognize_legacy: bool, title: &str) -> Option<HWND> {
        let mut window = std::ptr::null_mut();
        loop {
            // Look only at exact Eclipse titles, including multiple portable
            // launchers. A different profile must never steal this launch.
            window = FindWindowExW(std::ptr::null_mut(), window, std::ptr::null(), wide(title).as_ptr());
            if window.is_null() { return None; }
            let matching = !GetPropW(window, property.as_ptr()).is_null();
            let legacy = recognize_legacy && GetPropW(window, wide(PROFILE_PROPERTY).as_ptr()).is_null();
            if (matching || legacy) && same_application(window) { return Some(window); }
        }
    }
    pub fn mark_window(window: isize, config: &uu_core::Config) -> io::Result<Option<WindowMarker>> {
        if window == 0 { return Ok(None); }
        let property = wide(&format!("{PROFILE_PROPERTY}.{}", super::profile_key(&config.data_dir)?));
        unsafe {
            if SetPropW(window as _, wide(PROFILE_PROPERTY).as_ptr(), 1usize as _) == 0 {
                return Err(io::Error::last_os_error());
            }
            if SetPropW(window as _, property.as_ptr(), 1usize as _) == 0 {
                let error = io::Error::last_os_error();
                RemovePropW(window as _, wide(PROFILE_PROPERTY).as_ptr());
                return Err(error);
            }
        }
        Ok(Some(WindowMarker { window, property }))
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
    pub fn claim(config: &uu_core::Config) -> io::Result<Option<Launcher>> {
        let key = super::profile_key(&config.data_dir)?;
        let property = wide(&format!("{PROFILE_PROPERTY}.{key}"));
        let recognize_legacy = config.profile_dir().is_none();
        let title = super::window_title(config);
        unsafe {
            // Also recognize pre-mutex releases, so upgrading does not create
            // a second launcher beside an older version already on screen.
            if let Some(window) = existing_launcher(&property, recognize_legacy, &title) {
                activate(window);
                return Ok(None);
            }
        }
        // A store owns its launcher, independent of executable revision/path.
        let (guard, existed) = mutex(&format!(r"Local\Eclipse.Launcher.v3.{key}"))?;
        if !existed {
            return Ok(Some(guard));
        }
        drop(guard);
        for _ in 0..40 {
            if let Some(window) = unsafe { existing_launcher(&property, recognize_legacy, &title) } {
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
    #[cfg(test)]
    #[test]
    fn hidden_launcher_properties_identify_only_their_profile_and_are_removed_on_drop() {
        let root = std::env::temp_dir().join(format!("eclipse-window-profile-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let config = uu_core::Config { data_dir:root.join("one/data"), state_dir:root.join("one/state"), signaling_host:String::new() };
        let own = wide(&format!("{PROFILE_PROPERTY}.{}", super::profile_key(&config.data_dir).unwrap()));
        let other = wide(&format!("{PROFILE_PROPERTY}.{}", super::profile_key(&root.join("two/data")).unwrap()));
        unsafe {
            let window = CreateWindowExW(0, wide("STATIC").as_ptr(), wide("Eclipse profile fixture").as_ptr(),
                0, 0, 0, 1, 1, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null());
            assert!(!window.is_null(), "{}", io::Error::last_os_error());
            let marker = mark_window(window as isize, &config).unwrap().unwrap();
            assert!(!GetPropW(window, own.as_ptr()).is_null());
            assert!(GetPropW(window, other.as_ptr()).is_null());
            drop(marker);
            assert!(GetPropW(window, own.as_ptr()).is_null());
            assert!(GetPropW(window, wide(PROFILE_PROPERTY).as_ptr()).is_null());
            assert_ne!(DestroyWindow(window), 0);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launcher_identity_tracks_the_store_and_survives_first_creation() {
        let root = std::env::temp_dir().join(format!("eclipse-launcher-profile-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("eclipse-data/data");
        let before = profile_key(&path).unwrap();
        std::fs::create_dir_all(&path).unwrap();
        assert_eq!(before, profile_key(&path).unwrap());
        assert_eq!(before, profile_key(&root.join("other/../eclipse-data/./data")).unwrap());
        assert_ne!(before, profile_key(&root.join("another-version/eclipse-data/data")).unwrap());
        let mut config = uu_core::Config { data_dir:path.clone(), state_dir:root.join("eclipse-data/state"), signaling_host:String::new() };
        assert_ne!(window_title(&config), super::super::brand::NAME);
        config.data_dir = root.join("user/uu-remote");
        assert_eq!(window_title(&config), super::super::brand::NAME);
        #[cfg(windows)]
        assert_eq!(before, profile_key(&PathBuf::from(path.to_string_lossy().to_uppercase())).unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }
}
