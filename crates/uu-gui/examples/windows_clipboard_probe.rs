//! Real Windows clipboard IO, only inside a newly created noninteractive station.
#[path = "../src/clipboard.rs"]
mod clipboard;
#[cfg(windows)]
mod fixture {
    use crate::clipboard;
    use anyhow::{ensure, Context, Result};
    use std::{
        ptr::{null, null_mut},
        sync::{atomic::Ordering, Arc},
        time::{Duration, Instant},
    };
    use uu_core::clipboard::Clipboard;
    use windows_sys::Win32::{
        Foundation::*,
        System::{
            LibraryLoader::GetModuleHandleW, StationsAndDesktops::*, Threading::GetCurrentThreadId,
        },
        UI::WindowsAndMessaging::*,
    };
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }
    fn check(value: bool, label: &str) -> Result<()> {
        ensure!(value, "{label}: {}", std::io::Error::last_os_error());
        Ok(())
    }
    struct Station {
        original: HWINSTA,
        original_desktop: HDESK,
        station: HWINSTA,
        desktop: HDESK,
    }
    impl Station {
        fn create() -> Result<Self> {
            unsafe {
                let original = GetProcessWindowStation();
                let original_desktop = GetThreadDesktop(GetCurrentThreadId());
                check(
                    !original.is_null() && !original_desktop.is_null(),
                    "cannot record process GUI context",
                )?;
                let name = format!("uu-client-clipboard-{}", uuid::Uuid::new_v4());
                let name_w = wide(&name);
                // CWF_CREATE_ONLY: never open a preexisting station. No privilege
                // adjustment, alternate-user login, or interactive-desktop switch.
                let station = CreateWindowStationW(
                    name_w.as_ptr(),
                    1,
                    0x0004 | 0x0008 | 0x0002 | 0x0020 | 0x0001,
                    null(),
                );
                check(
                    !station.is_null(),
                    "cannot create isolated clipboard station",
                )?;
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
                    "refusing clipboard IO outside our newly created station"
                );
                Ok(guard)
            }
        }
    }
    impl Drop for Station {
        fn drop(&mut self) {
            unsafe {
                // All clipboard clients and our owner window must be dropped first.
                let _ = SetProcessWindowStation(self.original);
                let _ = SetThreadDesktop(self.original_desktop);
                if !self.desktop.is_null() {
                    let _ = CloseDesktop(self.desktop);
                }
                let _ = CloseWindowStation(self.station);
            }
        }
    }
    struct Owner(HWND);
    impl Drop for Owner {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }
    struct Backend(Arc<Clipboard>);
    impl Drop for Backend {
        fn drop(&mut self) {
            self.0.stopped.store(true, Ordering::Release);
            self.0.active.store(false, Ordering::Release);
            let deadline = Instant::now() + Duration::from_secs(3);
            while self.0.available.load(Ordering::Acquire) {
                if Instant::now() > deadline {
                    // Exiting this test process preserves isolation if a backend
                    // cannot stop; never restore another station under a live worker.
                    eprintln!("isolated clipboard backend did not stop");
                    std::process::exit(2);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    fn until(label: &str, mut predicate: impl FnMut() -> bool) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(4);
        while !predicate() {
            ensure!(
                Instant::now() < deadline,
                "isolated clipboard state did not converge: {label}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
    pub fn run() -> Result<()> {
        ensure!(
            std::env::args().skip(1).eq(["--isolated-window-station"]),
            "usage: windows_clipboard_probe --isolated-window-station"
        );
        let _station = Station::create()?;
        let owner = Owner(unsafe {
            CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                wide("owned clipboard fixture").as_ptr(),
                0,
                0,
                0,
                1,
                1,
                HWND_MESSAGE,
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            )
        });
        check(!owner.0.is_null(), "cannot create private clipboard owner")?;
        // Independent arboard driver and the actual production Win32 worker
        // access only the clipboard of the verified private window station.
        let mut system = arboard::Clipboard::new()?;
        system.set_text("Generated private baseline")?;
        let state = Arc::new(Clipboard::default());
        state.enabled.store(true, Ordering::Release);
        let backend = Backend(state.clone());
        clipboard::start_host(state.clone(), owner.0 as isize)?;
        until("backend startup", || {
            state.available.load(Ordering::Acquire)
        })?;
        state.active.store(true, Ordering::Release);
        std::thread::sleep(Duration::from_millis(150));
        ensure!(
            state.local.lock().unwrap().is_none(),
            "host initial clipboard was broadcast"
        );
        *state.remote.lock().unwrap() = Some("Generated remote 中文🙂\r\ntext".into());
        until("remote text write", || {
            system
                .get_text()
                .is_ok_and(|s| s == "Generated remote 中文🙂\r\ntext")
        })?;
        system.set_text("Generated host reply 日本語🙂")?;
        until("local text capture", || {
            state.local.lock().unwrap().as_deref() == Some("Generated host reply 日本語🙂")
        })?;
        state.local.lock().unwrap().take();
        state.active.store(false, Ordering::Release);
        std::thread::sleep(Duration::from_millis(150));
        *state.remote.lock().unwrap() = Some("Inactive generated text must not be written".into());
        std::thread::sleep(Duration::from_millis(200));
        ensure!(
            system.get_text()? == "Generated host reply 日本語🙂",
            "inactive clipboard write occurred"
        );
        state.remote.lock().unwrap().take();
        system.set_text("Generated host copy while inactive")?;
        std::thread::sleep(Duration::from_millis(150));
        ensure!(state.local.lock().unwrap().is_none(), "inactive host copy was captured");
        state.active.store(true, Ordering::Release);
        until("copy on host while viewer paused", || {
            state.local.lock().unwrap().as_deref()==Some("Generated host copy while inactive")
        })?;
        drop(backend);
        ensure!(
            !state.available.load(Ordering::Acquire),
            "backend did not release availability"
        );
        let controller = Arc::new(Clipboard::default());
        controller.enabled.store(true, Ordering::Release);
        controller.active.store(true, Ordering::Release);
        let controller_backend = Backend(controller.clone());
        clipboard::start(controller.clone(), owner.0 as isize)?;
        until("controller startup", || {
            controller.available.load(Ordering::Acquire)
        })?;
        *controller.remote.lock().unwrap() =
            Some("Background controller text must not be written".into());
        std::thread::sleep(Duration::from_millis(250));
        ensure!(
            system.get_text()? == "Generated host copy while inactive"
                && controller.local.lock().unwrap().is_none(),
            "controller foreground guard was bypassed"
        );
        drop(controller_backend);
        println!(
            "{}",
            serde_json::json!({"windows_clipboard":"passed","isolated_station":true,"interactive_clipboard":"untouched","controller_foreground_guard":true,"unicode_roundtrip":true,"initial_baseline_stayed_local":true,"inactive_write_suppressed":true,"clean_stop":true})
        );
        Ok(())
    }
}
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    fixture::run()
}
#[cfg(not(windows))]
fn main() {
    eprintln!("This fixture requires Windows and its own noninteractive window station");
    std::process::exit(2);
}
