//! The launcher owns its notification icon; streaming windows never own one.
//! Native callbacks only restore the window or queue an application command.
use eframe::egui;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Open,
    Exit,
}

pub const fn supported() -> bool {
    cfg!(windows)
}

#[cfg(windows)]
pub use windows::Tray;
#[cfg(not(windows))]
pub struct Tray;
#[cfg(not(windows))]
impl Tray {
    pub fn new(_: isize, _: &egui::Context) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "notification area unavailable",
        ))
    }
    pub fn ensure(&self) -> bool {
        false
    }
    pub fn take_action(&self) -> Option<Action> {
        None
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{cell::Cell, io, mem::size_of, ptr, rc::Rc};
    use windows_sys::Win32::{
        Foundation::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::{Shell::*, WindowsAndMessaging::*},
    };
    const CALLBACK: u32 = WM_APP + 1;
    const OPEN: usize = 1;
    const EXIT: usize = 2;
    // winuser.h's keyboard activation notification (not exposed by windows-sys).
    const KEY_SELECT: u32 = NIN_SELECT | 1;
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    struct State {
        target: HWND,
        icon: HICON,
        context: egui::Context,
        action: Cell<Option<Action>>,
        taskbar_created: u32,
    }
    impl State {
        fn data(&self, window: HWND) -> NOTIFYICONDATAW {
            let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
            data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
            data.hWnd = window;
            data.uID = 1;
            data.hIcon = self.icon;
            data.uCallbackMessage = CALLBACK;
            data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
            for (slot, value) in data.szTip.iter_mut().zip(crate::brand::NAME.encode_utf16()) {
                *slot = value;
            }
            data
        }
        fn add(&self, window: HWND) -> bool {
            let mut data = self.data(window);
            unsafe {
                if Shell_NotifyIconW(NIM_ADD, &data) == 0
                    && Shell_NotifyIconW(NIM_MODIFY, &data) == 0
                {
                    return false;
                }
                data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                if Shell_NotifyIconW(NIM_SETVERSION, &data) != 0 {
                    return true;
                }
                Shell_NotifyIconW(NIM_DELETE, &data);
            }
            false
        }
        fn publish(&self, action: Action) {
            // An explicit quit cannot be replaced by a later activation.
            if self.action.get() == Some(Action::Exit) {
                return;
            }
            self.action.set(Some(action));
            if action == Action::Open {
                unsafe {
                    ShowWindow(
                        self.target,
                        if IsIconic(self.target) != 0 {
                            SW_RESTORE
                        } else {
                            SW_SHOW
                        },
                    );
                    SetForegroundWindow(self.target);
                }
            }
            self.context.request_repaint();
        }
        unsafe fn menu(&self, window: HWND) {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }
            AppendMenuW(
                menu,
                MF_STRING,
                OPEN,
                wide(uu_i18n::text("打开 Eclipse")).as_ptr(),
            );
            AppendMenuW(
                menu,
                MF_STRING,
                EXIT,
                wide(uu_i18n::text("退出 Eclipse")).as_ptr(),
            );
            SetMenuDefaultItem(menu, OPEN as u32, 0);
            let mut point = POINT { x: 0, y: 0 };
            GetCursorPos(&mut point);
            // Win32 requires foreground ownership for outside-click dismissal.
            SetForegroundWindow(window);
            TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                window,
                ptr::null(),
            );
            PostMessageW(window, WM_NULL, 0, 0);
            DestroyMenu(menu);
        }
    }
    unsafe extern "system" fn procedure(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == WM_NCCREATE {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        let state = GetWindowLongPtrW(window, GWLP_USERDATA) as *const State;
        if !state.is_null() {
            // Keep the callback state alive across the menu's nested message loop.
            Rc::increment_strong_count(state);
            let state = Rc::from_raw(state);
            if message == state.taskbar_created && message != 0 {
                if !state.add(window) {
                    state.publish(Action::Open);
                }
                return 0;
            }
            match message {
                CALLBACK => {
                    match lparam as u32 & 0xffff {
                        NIN_SELECT | KEY_SELECT | WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                            state.publish(Action::Open)
                        }
                        WM_CONTEXTMENU => state.menu(window),
                        _ => {}
                    }
                    return 0;
                }
                WM_COMMAND => {
                    match wparam & 0xffff {
                        OPEN => state.publish(Action::Open),
                        EXIT => state.publish(Action::Exit),
                        _ => {}
                    }
                    return 0;
                }
                WM_NCDESTROY => {
                    SetWindowLongPtrW(window, GWLP_USERDATA, 0);
                }
                _ => {}
            }
        }
        DefWindowProcW(window, message, wparam, lparam)
    }

    pub struct Tray {
        window: HWND,
        state: Rc<State>,
        class: Vec<u16>,
        instance: HINSTANCE,
    }
    impl Tray {
        pub fn new(target: isize, context: &egui::Context) -> io::Result<Self> {
            unsafe {
                let target = target as HWND;
                if IsWindow(target) == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid launcher window",
                    ));
                }
                let instance = GetModuleHandleW(ptr::null());
                // Resource 1 is the same Eclipse icon embedded by build.rs.
                let icon = LoadImageW(
                    instance,
                    1usize as _,
                    IMAGE_ICON,
                    GetSystemMetrics(SM_CXSMICON),
                    GetSystemMetrics(SM_CYSMICON),
                    0,
                ) as HICON;
                if icon.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let class = wide(&format!("Eclipse.Tray.{}", uuid::Uuid::new_v4()));
                let mut definition: WNDCLASSW = std::mem::zeroed();
                definition.lpfnWndProc = Some(procedure);
                definition.hInstance = instance;
                definition.lpszClassName = class.as_ptr();
                if RegisterClassW(&definition) == 0 {
                    let error = io::Error::last_os_error();
                    DestroyIcon(icon);
                    return Err(error);
                }
                let state = Rc::new(State {
                    target,
                    icon,
                    context: context.clone(),
                    action: Cell::new(None),
                    taskbar_created: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
                });
                if state.taskbar_created == 0 {
                    let error = io::Error::last_os_error();
                    UnregisterClassW(class.as_ptr(), instance);
                    DestroyIcon(icon);
                    return Err(error);
                }
                // A hidden top-level window receives Explorer's restart broadcast;
                // HWND_MESSAGE windows do not receive that broadcast.
                let window = CreateWindowExW(
                    WS_EX_TOOLWINDOW,
                    class.as_ptr(),
                    ptr::null(),
                    WS_POPUP,
                    0,
                    0,
                    0,
                    0,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    instance,
                    Rc::as_ptr(&state) as _,
                );
                if window.is_null() {
                    let error = io::Error::last_os_error();
                    UnregisterClassW(class.as_ptr(), instance);
                    DestroyIcon(icon);
                    return Err(error);
                }
                let tray = Self {
                    window,
                    state,
                    class,
                    instance,
                };
                if !tray.state.add(window) {
                    return Err(io::Error::other("Windows notification area unavailable"));
                }
                Ok(tray)
            }
        }
        /// Check the recovery path before hiding the only launcher window.
        pub fn ensure(&self) -> bool {
            unsafe {
                Shell_NotifyIconW(NIM_MODIFY, &self.state.data(self.window)) != 0
                    || self.state.add(self.window)
            }
        }
        pub fn take_action(&self) -> Option<Action> {
            self.state.action.take()
        }
    }
    impl Drop for Tray {
        fn drop(&mut self) {
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &self.state.data(self.window));
                DestroyWindow(self.window);
                DestroyIcon(self.state.icon);
                UnregisterClassW(self.class.as_ptr(), self.instance);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        #[ignore = "run alone with private XDG directories and an interactive Windows shell"]
        fn launcher_close_restores_and_explicit_tray_exit_finishes() {
            use std::time::{Duration, Instant};
            struct Directory(std::path::PathBuf);
            impl Drop for Directory {
                fn drop(&mut self) {
                    let _ = std::fs::remove_dir_all(&self.0);
                }
            }
            let directory = Directory(
                std::env::temp_dir()
                    .join(format!("eclipse-tray-launcher-{}", uuid::Uuid::new_v4())),
            );
            std::fs::create_dir(&directory.0).unwrap();
            std::env::set_var("XDG_DATA_HOME", directory.0.join("data"));
            std::env::set_var("XDG_STATE_HOME", directory.0.join("state"));
            uu_core::install_crypto_provider();
            let mut settings = crate::settings::Settings::default();
            settings.close_to_tray = true;
            settings.lan_discovery = false;
            settings.wallpaper_enabled = false;
            settings
                .save(&crate::settings::Settings::path().unwrap())
                .unwrap();
            let passed = Rc::new(Cell::new(false));
            let completed = passed.clone();
            struct Probe {
                app: crate::App,
                phase: u8,
                started: Instant,
                passed: Rc<Cell<bool>>,
            }
            impl eframe::App for Probe {
                fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
                    assert!(
                        self.started.elapsed() < Duration::from_secs(20),
                        "launcher tray transition timed out"
                    );
                    self.app.update_frame(ctx, None);
                    let visible = eframe::viewport_window(ctx)
                        .is_some_and(|window| window.is_visible() == Some(true));
                    match self.phase {
                        0 if visible => {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            self.phase = 1;
                        }
                        1 | 3 | 5 if self.app.launcher_hidden && !visible => {
                            assert!(
                                !self.app.exit_requested,
                                "hiding must not request host shutdown"
                            );
                            let tray = self.app.tray.as_ref().unwrap();
                            assert!(tray.ensure());
                            unsafe {
                                match self.phase {
                                    1 => {
                                        SendMessageW(tray.window, WM_COMMAND, OPEN, 0);
                                    }
                                    // Reopening the existing process shows its HWND
                                    // without delivering a tray activation command.
                                    3 => {
                                        ShowWindow(self.app.platform.window_id() as HWND, SW_SHOW);
                                    }
                                    _ => {
                                        SendMessageW(tray.window, WM_COMMAND, EXIT, 0);
                                    }
                                }
                            }
                            self.phase += 1;
                        }
                        2 | 4 if !self.app.launcher_hidden && visible => {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            self.phase += 1;
                        }
                        6 if self.app.exit_requested => {
                            assert!(self.app.tray.is_none());
                            self.passed.set(true);
                            self.phase = 7;
                        }
                        _ => {}
                    }
                    ctx.request_repaint_after(Duration::from_millis(10));
                }
                fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
                    eframe::App::on_exit(&mut self.app, gl);
                }
            }
            let options = eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default()
                    .with_inner_size([800., 600.])
                    .with_decorations(false)
                    .with_icon(crate::brand::window_icon()),
                vsync: false,
                event_loop_builder: Some(Box::new(|builder| {
                    use winit::platform::windows::EventLoopBuilderExtWindows;
                    builder.with_any_thread(true);
                })),
                ..Default::default()
            };
            eframe::run_native(
                "Eclipse owned tray launcher fixture",
                options,
                Box::new(move |cc| {
                    Ok(Box::new(Probe {
                        app: crate::App::new(cc, None, None, false, None),
                        phase: 0,
                        started: Instant::now(),
                        passed: completed,
                    }))
                }),
            )
            .unwrap();
            assert!(
                passed.get(),
                "the real launcher must hide, reopen and finish explicit quit"
            );
        }

        #[test]
        #[ignore = "requires an interactive Windows notification area"]
        fn notification_icon_restores_queues_quit_recovers_and_cleans_its_window() {
            unsafe {
                let target = CreateWindowExW(
                    WS_EX_TOOLWINDOW,
                    wide("STATIC").as_ptr(),
                    wide("Eclipse owned tray fixture").as_ptr(),
                    WS_OVERLAPPEDWINDOW,
                    20,
                    20,
                    320,
                    200,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    GetModuleHandleW(ptr::null()),
                    ptr::null(),
                );
                assert!(!target.is_null());
                struct Window(HWND);
                impl Drop for Window {
                    fn drop(&mut self) {
                        unsafe {
                            DestroyWindow(self.0);
                        }
                    }
                }
                let _target = Window(target);
                let context = egui::Context::default();
                let tray = Tray::new(target as isize, &context).unwrap();
                let notification = tray.window;
                assert!(tray.ensure());
                ShowWindow(target, SW_HIDE);
                SendMessageW(
                    notification,
                    CALLBACK,
                    0,
                    ((1u32 << 16) | NIN_SELECT) as isize,
                );
                assert_eq!(tray.take_action(), Some(Action::Open));
                assert_ne!(IsWindowVisible(target), 0);
                Shell_NotifyIconW(NIM_DELETE, &tray.state.data(notification));
                SendMessageW(notification, tray.state.taskbar_created, 0, 0);
                assert!(
                    tray.ensure(),
                    "the owned icon is recreated after its taskbar broadcast"
                );
                SendMessageW(notification, WM_COMMAND, EXIT, 0);
                SendMessageW(notification, WM_COMMAND, OPEN, 0);
                assert_eq!(tray.take_action(), Some(Action::Exit));
                drop(tray);
                assert_eq!(IsWindow(notification), 0);
            }
        }
    }
}
