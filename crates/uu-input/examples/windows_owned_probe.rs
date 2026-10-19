//! Interactive OS delivery check. Only sends input while its own fixture owns focus.
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use anyhow::ensure;
    use std::{thread::sleep, time::Duration};
    use uu_input::{InputEvent as I, InputInjector, KeyCode, MouseButton};
    use windows_sys::Win32::{
        Foundation::{HWND, POINT, RECT},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::*,
            Input::KeyboardAndMouse::{GetAsyncKeyState, SetFocus},
            WindowsAndMessaging::*,
        },
    };
    ensure!(
        std::env::args().skip(1).collect::<Vec<_>>() == ["--owned-window"],
        "explicit --owned-window is required; this test briefly opens its own interactive window"
    );
    struct DpiContext(DPI_AWARENESS_CONTEXT);
    impl Drop for DpiContext {
        fn drop(&mut self) {
            unsafe {
                SetThreadDpiAwarenessContext(self.0);
            }
        }
    }
    let previous_dpi =
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    ensure!(
        !previous_dpi.is_null(),
        "cannot establish fixture physical-pixel coordinates"
    );
    let _dpi = DpiContext(previous_dpi);
    let watched = [
        0x1b, 0x41, 0x10, 0xa2, 0xa3, 0xa4, 0xa5, 0xba, 0xbf, 0x0d, 0x04, 0x05, 0x06,
    ];
    for vk in watched {
        ensure!(
            unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000 == 0,
            "a fixture key is already held; no test input was sent"
        );
    }
    struct Fixture {
        window: HWND,
        previous: HWND,
        pointer: POINT,
    }
    impl Fixture {
        fn owns_focus(&self) -> bool {
            let mut pid = 0;
            unsafe {
                GetWindowThreadProcessId(self.window, &mut pid);
            }
            pid == std::process::id() && unsafe { GetForegroundWindow() } == self.window
        }
        fn check_target(&self, event: I) -> anyhow::Result<()> {
            ensure!(
                self.owns_focus(),
                "focus left owned fixture; stopping input check"
            );
            if matches!(event, I::MouseButton { .. } | I::MouseScroll { .. }) {
                let mut point = POINT { x: 0, y: 0 };
                ensure!(
                    unsafe { GetCursorPos(&mut point) } != 0,
                    "cannot verify fixture pointer"
                );
                ensure!(
                    unsafe { WindowFromPoint(point) } == self.window,
                    "pointer does not hit owned fixture; no button or wheel event sent"
                );
            }
            Ok(())
        }
        fn drain(&self, observed: &mut Vec<(u32, usize, isize)>) {
            let mut message: MSG = unsafe { std::mem::zeroed() };
            unsafe {
                while PeekMessageW(&mut message, self.window, 0, 0, PM_REMOVE) != 0 {
                    observed.push((message.message, message.wParam, message.lParam));
                    // Do not run a text/IME or context-menu action from fixture input.
                    if !(WM_KEYFIRST..=WM_KEYLAST).contains(&message.message)
                        && !(WM_MOUSEFIRST..=WM_MOUSELAST).contains(&message.message)
                    {
                        DispatchMessageW(&message);
                    }
                }
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let ours = self.owns_focus();
            unsafe {
                if ours {
                    SetCursorPos(self.pointer.x, self.pointer.y);
                }
                DestroyWindow(self.window);
                if ours && IsWindow(self.previous) != 0 {
                    SetForegroundWindow(self.previous);
                }
            }
        }
    }
    let mut pointer = POINT { x: 0, y: 0 };
    let previous = unsafe { GetForegroundWindow() };
    ensure!(
        unsafe { GetCursorPos(&mut pointer) } != 0,
        "cannot preserve pointer position"
    );
    let title: Vec<u16> = "uu-client owned input fixture\0".encode_utf16().collect();
    unsafe extern "system" fn fixture_window(
        window: HWND,
        message: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        if message == WM_NCHITTEST {
            return HTCLIENT as isize;
        }
        unsafe { DefWindowProcW(window, message, wparam, lparam) }
    }
    let class: Vec<u16> = "UuOwnedInputFixture\0".encode_utf16().collect();
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let mut window_class: WNDCLASSW = unsafe { std::mem::zeroed() };
    window_class.lpfnWndProc = Some(fixture_window);
    window_class.hInstance = instance;
    window_class.lpszClassName = class.as_ptr();
    ensure!(
        unsafe { RegisterClassW(&window_class) } != 0,
        "cannot register owned fixture window class"
    );
    let window = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            480,
            280,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    ensure!(!window.is_null(), "cannot create owned input fixture");
    let fixture = Fixture {
        window,
        previous,
        pointer,
    };
    unsafe {
        ShowWindow(window, SW_SHOW);
        SetForegroundWindow(window);
        SetFocus(window);
    }
    sleep(Duration::from_millis(150));
    ensure!(
        fixture.owns_focus(),
        "owned fixture could not obtain focus; no input was sent"
    );
    let mut observed = Vec::new();
    fixture.drain(&mut observed);
    observed.clear();
    let mut injector = uu_input::windows::SendInputInjector::new();
    let mut emit = |event| -> anyhow::Result<()> {
        fixture.check_target(event)?;
        injector.inject(event)?;
        sleep(Duration::from_millis(15));
        fixture.drain(&mut observed);
        ensure!(fixture.owns_focus(), "focus changed during input check");
        Ok(())
    };
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    ensure!(
        unsafe { GetWindowRect(window, &mut rect) } != 0,
        "fixture geometry unavailable"
    );
    emit(I::MouseMoveAbsolute {
        x: (rect.left + rect.right) / 2,
        y: (rect.top + rect.bottom) / 2,
    })?;
    for button in [MouseButton::Middle, MouseButton::Back, MouseButton::Forward] {
        emit(I::MouseButton {
            button,
            pressed: true,
        })?;
        emit(I::MouseButton {
            button,
            pressed: false,
        })?;
    }
    emit(I::MouseScroll { dx: 120, dy: 120 })?;
    for hid in [0xe1, 0x33, 0x38, 0xe4, 0xe6, 0x58] {
        emit(I::Key {
            key: KeyCode(hid),
            pressed: true,
        })?;
        emit(I::Key {
            key: KeyCode(hid),
            pressed: false,
        })?;
    }
    drop(emit);
    for (message, wparam, lparam) in [
        (WM_MBUTTONDOWN, 0usize, 0isize),
        (WM_MBUTTONUP, 0, 0),
        (WM_XBUTTONDOWN, 1usize << 16, 0),
        (WM_XBUTTONUP, 2usize << 16, 0),
        (WM_MOUSEWHEEL, 120usize << 16, 0),
        (WM_MOUSEHWHEEL, 120usize << 16, 0),
    ] {
        ensure!(
            observed
                .iter()
                .any(|(msg, w, _)| *msg == message && (*w & 0xffff0000) == wparam),
            "owned fixture missed mouse event {message:#x}/{wparam:#x}/{lparam:#x}"
        );
    }
    for scan in [0x2a, 0x27, 0x35, 0x11d, 0x138, 0x11c] {
        ensure!(
            observed.iter().any(
                |(msg, _, flags)| (*msg == WM_KEYDOWN || *msg == WM_SYSKEYDOWN)
                    && ((*flags >> 16) & 0x1ff) == scan
            ),
            "owned fixture missed scan code {scan:#x}"
        );
    }
    ensure!(
        fixture.owns_focus(),
        "focus left owned fixture before release test"
    );
    let mut other = uu_input::windows::SendInputInjector::new();
    let escape = I::Key {
        key: KeyCode::ESC,
        pressed: true,
    };
    fixture.check_target(escape)?;
    other.inject(escape)?;
    for event in [
        I::Key {
            key: KeyCode(4),
            pressed: true,
        },
        I::Key {
            key: KeyCode(0xe1),
            pressed: true,
        },
        I::MouseButton {
            button: MouseButton::Middle,
            pressed: true,
        },
    ] {
        fixture.check_target(event)?;
        injector.inject(event)?;
    }
    injector.release_all()?;
    sleep(Duration::from_millis(30));
    fixture.drain(&mut observed);
    ensure!(
        unsafe { GetAsyncKeyState(0x1b) } as u16 & 0x8000 != 0,
        "release affected another injector's key"
    );
    for vk in [0x41, 0x10, 0x04] {
        ensure!(
            unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000 == 0,
            "owned key/button was not released"
        );
    }
    other.release_all()?;
    println!("{{\"windows_owned_input\":\"passed\",\"punctuation_scans\":true,\"extended_keys\":true,\"middle_and_side_buttons\":true,\"both_wheels\":true,\"owned_release\":true,\"messages\":{}}}",observed.len());
    Ok(())
}

#[cfg(not(windows))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("this explicit interactive fixture requires Windows")
}
