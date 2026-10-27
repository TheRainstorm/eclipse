//! System clipboard runs off the UI thread. Only the foreground stream may
//! read/write text. A separately permitted host uses a peer-leased active state.
//! Clipboard contents never appear in logs or command arguments.
use std::sync::Arc;
use uu_core::clipboard::Clipboard;

pub fn start(control: Arc<Clipboard>, window: isize) -> std::io::Result<()> {
    start_following(
        control,
        Arc::new(std::sync::atomic::AtomicIsize::new(window)),
    )
}
pub fn start_following(
    control: Arc<Clipboard>,
    window: Arc<std::sync::atomic::AtomicIsize>,
) -> std::io::Result<()> {
    start_role(control, window, true)
}

/// Only an explicit native-host checkbox may call this; readiness is leased to its peer.
pub fn start_host(control: Arc<Clipboard>, window: isize) -> std::io::Result<()> {
    start_role(
        control,
        Arc::new(std::sync::atomic::AtomicIsize::new(window)),
        false,
    )
}

/// A permitted background host owns a message-only clipboard window on its
/// private worker. It never borrows a GUI window or creates a visible surface.
pub fn start_host_headless(control: Arc<Clipboard>) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::thread::Builder::new()
            .name("uu-host-clipboard".into())
            .spawn(move || {
                use windows_sys::Win32::UI::WindowsAndMessaging::*;
                let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
                let window = unsafe {
                    CreateWindowExW(
                        0,
                        class.as_ptr(),
                        std::ptr::null(),
                        0,
                        0,
                        0,
                        0,
                        0,
                        HWND_MESSAGE,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null(),
                    )
                };
                if window.is_null() {
                    control
                        .available
                        .store(false, std::sync::atomic::Ordering::Release);
                    control.status("后台剪贴板所有者初始化失败");
                    return;
                }
                struct Window(windows_sys::Win32::Foundation::HWND);
                impl Drop for Window {
                    fn drop(&mut self) {
                        unsafe {
                            DestroyWindow(self.0);
                        }
                    }
                }
                let window = Window(window);
                run_system(
                    control,
                    WindowsSystem {
                        owner: Arc::new(std::sync::atomic::AtomicIsize::new(window.0 as isize)),
                        foreground_only: false,
                        sequence: None,
                        pump_messages: true,
                    },
                    false,
                );
            })?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        start_host(control, 0)
    }
}
fn start_role(
    control: Arc<Clipboard>,
    owner: Arc<std::sync::atomic::AtomicIsize>,
    foreground_only: bool,
) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let window = owner.load(std::sync::atomic::Ordering::Acquire);
        if window == 0
            || unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsWindow(window as _) } == 0
        {
            control
                .available
                .store(false, std::sync::atomic::Ordering::Release);
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "剪贴板所有者窗口尚未就绪",
            ));
        }
        // Use the caller's authorized desktop explicitly. A worker must not
        // inherit a stale startup desktop after an RDP/session transition.
        let desktop = unsafe {
            windows_sys::Win32::System::StationsAndDesktops::GetThreadDesktop(
                windows_sys::Win32::System::Threading::GetCurrentThreadId(),
            )
        } as usize;
        if desktop == 0 {
            return Err(std::io::Error::last_os_error());
        }
        std::thread::Builder::new()
            .name("uu-clipboard".into())
            .spawn(move || {
                if unsafe {
                    windows_sys::Win32::System::StationsAndDesktops::SetThreadDesktop(desktop as _)
                } == 0
                {
                    control
                        .available
                        .store(false, std::sync::atomic::Ordering::Release);
                    control.status("无法打开当前桌面的剪贴板");
                    return;
                }
                run(control, owner, foreground_only)
            })?;
    }
    #[cfg(target_os = "linux")]
    {
        let _ = (&owner, foreground_only);
        std::thread::Builder::new()
            .name("uu-clipboard".into())
            .spawn(move || run_linux(control, !foreground_only))?;
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (&owner, foreground_only);
        control.status("当前构建未启用系统剪贴板");
    }
    Ok(())
}
#[cfg(any(windows, target_os = "linux", test))]
fn canonical(text: &str) -> String {
    text.replace("\r\n", "\n")
}

mod formats;
use formats::Value;

/// Retain only a content fingerprint through focus/permission pauses.
#[derive(Default, Clone, Copy)]
struct TextTracker {
    fingerprint: Option<[u8; 32]>,
    observed: bool,
}
impl TextTracker {
    fn observe_fingerprint(&mut self, next: Option<[u8; 32]>, publish_initial: bool) -> bool {
        let publish = next != self.fingerprint && (publish_initial || self.observed);
        self.fingerprint = next;
        self.observed = true;
        publish
    }
    #[cfg(test)]
    fn observe(&mut self, text: Option<&str>, publish_initial: bool) -> bool {
        self.observe_fingerprint(
            text.map(|text| Value::Text(text.into()).fingerprint()),
            publish_initial,
        )
    }
    #[cfg(test)]
    fn applied(&mut self, text: &str) {
        self.observe(Some(text), false);
    }
}
enum Read {
    Unchanged,
    Empty,
    Value(Value),
}
trait SystemClipboard {
    fn token(&self) -> isize;
    fn active(&self, token: isize) -> bool;
    fn read(&mut self, token: isize, images: bool, force: bool) -> Result<Read, &'static str>;
    fn write(
        &mut self,
        token: isize,
        value: &Value,
        original: &uu_core::clipboard::Content,
    ) -> Result<(), &'static str>;
}
fn run_system(control: Arc<Clipboard>, mut system: impl SystemClipboard, publish_initial: bool) {
    use std::sync::atomic::Ordering::{Acquire, Release};
    control.available.store(true, Release);
    let mut tracker = TextTracker::default();
    let mut was_active = false;
    let mut previous_images = false;
    while !control.stopped.load(Acquire) {
        let token = system.token();
        let active = control.ready() && control.supported.load(Acquire) && system.active(token);
        let images = control.images_supported.load(Acquire);
        if active {
            let generation = control.generation.load(Acquire);
            let allowed = |system: &dyn SystemClipboard| {
                control.ready()
                    && control.supported.load(Acquire)
                    && control.generation.load(Acquire) == generation
                    && system.token() == token
                    && system.active(token)
            };
            let read = system.read(token, images, !was_active || images != previous_images);
            let mut local_changed = false;
            if allowed(&system) {
                match read {
                    Ok(Read::Value(value)) => {
                        let previous = tracker;
                        if tracker.observe_fingerprint(Some(value.fingerprint()), publish_initial)
                            && !control.read_only.load(Acquire)
                        {
                            local_changed = true;
                            match value.encode() {
                                Ok(content) => {
                                    if allowed(&system) {
                                        *control.local.lock().unwrap() = Some(content);
                                        control.reset_generation.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                                    } else {
                                        tracker = previous;
                                    }
                                }
                                Err(_) => control
                                    .status("图片无法同步：最大8MiB PNG、16M像素，或内容格式无效"),
                            }
                        }
                        if !allowed(&system) {
                            tracker = previous;
                        }
                    }
                    Ok(Read::Empty) => {
                        tracker.observe_fingerprint(None, false);
                    }
                    Ok(Read::Unchanged) => {}
                    Err(message) => control.status(message),
                }
            }
            let remote = control.remote.lock().unwrap().take();
            if let Some(content) = remote.filter(|_| !local_changed && allowed(&system)) {
                match Value::decode(&content) {
                    Ok(value) => {
                        let fingerprint = value.fingerprint();
                        if tracker.fingerprint != Some(fingerprint) && allowed(&system) {
                            match system.write(token, &value, &content) {
                                Ok(()) => {
                                    tracker.observe_fingerprint(Some(fingerprint), false);
                                    control.status(match value {
                                        Value::Text(_) => "文本剪贴板已同步",
                                        Value::Image(_) => "图片剪贴板已同步",
                                    });
                                }
                                Err(message) => {
                                    let mut pending = control.remote.lock().unwrap();
                                    if pending.is_none() && allowed(&system) {
                                        *pending = Some(content);
                                    }
                                    control.status(message);
                                }
                            }
                        }
                    }
                    Err(_) => control.status("收到无效或超限图片，已跳过"),
                }
            }
        }
        was_active = active;
        previous_images = images;
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    control.available.store(false, Release);
    control.status("剪贴板已停止");
}
#[cfg(target_os = "linux")]
struct LinuxSystem {
    clipboard: arboard::Clipboard,
    last_image: bool,
    next_image: std::time::Instant,
}
#[cfg(target_os = "linux")]
impl SystemClipboard for LinuxSystem {
    fn token(&self) -> isize {
        0
    }
    fn active(&self, _: isize) -> bool {
        true
    }
    fn read(&mut self, _: isize, images: bool, force: bool) -> Result<Read, &'static str> {
        let mut image_checked = false;
        if images {
            if force || std::time::Instant::now() >= self.next_image {
                image_checked = true;
                self.next_image = std::time::Instant::now() + std::time::Duration::from_millis(500);
                match self.clipboard.get_image() {
                    Ok(image) => {
                        let width = image.width.try_into().map_err(|_| "剪贴板图片尺寸超限")?;
                        let height = image.height.try_into().map_err(|_| "剪贴板图片尺寸超限")?;
                        let pixels = formats::Pixels {
                            width,
                            height,
                            rgba: image.bytes.into_owned(),
                        };
                        pixels.validate().map_err(|_| "剪贴板图片尺寸超限")?;
                        self.last_image = true;
                        return Ok(Read::Value(Value::Image(pixels)));
                    }
                    Err(arboard::Error::ContentNotAvailable) => self.last_image = false,
                    Err(_) => return Err("剪贴板图片读取繁忙或格式不可用"),
                }
            } else if self.last_image {
                return Ok(Read::Unchanged);
            }
        } else {
            self.last_image = false;
        }
        match self.clipboard.get_text() {
            Ok(text) if text.len() <= uu_core::clipboard::MAX_TEXT && !text.contains('\0') => {
                Ok(Read::Value(Value::Text(text)))
            }
            Ok(_) => Ok(Read::Empty),
            Err(arboard::Error::ContentNotAvailable) => Ok(if images && !image_checked {
                Read::Unchanged
            } else {
                Read::Empty
            }),
            Err(_) => Err("剪贴板读取繁忙，正在重试"),
        }
    }
    fn write(
        &mut self,
        _: isize,
        value: &Value,
        _: &uu_core::clipboard::Content,
    ) -> Result<(), &'static str> {
        match value {
            Value::Text(text) => {
                self.clipboard
                    .set_text(text.clone())
                    .map_err(|_| "本地剪贴板写入繁忙，正在重试")?;
                self.last_image = false;
            }
            Value::Image(image) => {
                self.clipboard
                    .set_image(arboard::ImageData {
                        width: image.width as usize,
                        height: image.height as usize,
                        bytes: std::borrow::Cow::Borrowed(&image.rgba),
                    })
                    .map_err(|_| "本地图片剪贴板写入繁忙，正在重试")?;
                self.last_image = true;
                self.next_image = std::time::Instant::now() + std::time::Duration::from_millis(500);
            }
        }
        Ok(())
    }
}
#[cfg(target_os = "linux")]
fn run_linux(control: Arc<Clipboard>, suppress_initial: bool) {
    let clipboard = match arboard::Clipboard::new() {
        Ok(clipboard) => clipboard,
        Err(_) => {
            control
                .available
                .store(false, std::sync::atomic::Ordering::Release);
            control.status("无法打开系统剪贴板，请检查Wayland/X11会话");
            return;
        }
    };
    run_system(
        control,
        LinuxSystem {
            clipboard,
            last_image: false,
            next_image: std::time::Instant::now(),
        },
        !suppress_initial,
    )
}
#[cfg(windows)]
struct WindowsSystem {
    owner: Arc<std::sync::atomic::AtomicIsize>,
    foreground_only: bool,
    sequence: Option<u32>,
    pump_messages: bool,
}
#[cfg(windows)]
impl SystemClipboard for WindowsSystem {
    fn token(&self) -> isize {
        if self.pump_messages {
            use windows_sys::Win32::UI::WindowsAndMessaging::*;
            let mut message = unsafe { std::mem::zeroed() };
            for _ in 0..32 {
                if unsafe {
                    PeekMessageW(
                        &mut message,
                        self.owner.load(std::sync::atomic::Ordering::Acquire) as _,
                        0,
                        0,
                        PM_REMOVE,
                    )
                } == 0
                {
                    break;
                }
                unsafe {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
        self.owner.load(std::sync::atomic::Ordering::Acquire)
    }
    fn active(&self, token: isize) -> bool {
        self.token() == token && (!self.foreground_only || native::foreground(token))
    }
    fn read(&mut self, token: isize, images: bool, force: bool) -> Result<Read, &'static str> {
        let sequence = native::sequence();
        if !force && self.sequence == Some(sequence) {
            return Ok(Read::Unchanged);
        }
        if images {
            if let Some(pixels) = native::read_image(token, self.foreground_only)
                .map_err(|_| "剪贴板图片读取繁忙或格式不可用")?
            {
                self.sequence = Some(sequence);
                return Ok(Read::Value(Value::Image(pixels)));
            }
        }
        let text =
            native::read(token, self.foreground_only).map_err(|_| "剪贴板读取繁忙，正在重试")?;
        self.sequence = Some(sequence);
        Ok(text
            .map(Value::Text)
            .map(Read::Value)
            .unwrap_or(Read::Empty))
    }
    fn write(
        &mut self,
        token: isize,
        value: &Value,
        original: &uu_core::clipboard::Content,
    ) -> Result<(), &'static str> {
        if !self.active(token) {
            return Err("剪贴板窗口已切换，已暂停写入");
        }
        match (value, original) {
            (Value::Text(text), _) => native::write(token, text, self.foreground_only),
            (Value::Image(pixels), uu_core::clipboard::Content::Image(image)) => {
                native::write_image(token, pixels, image, self.foreground_only)
            }
            _ => return Err("剪贴板内容类型不一致"),
        }
        .map_err(|_| "本地剪贴板写入繁忙，正在重试")?;
        self.sequence = Some(native::sequence());
        Ok(())
    }
}
#[cfg(windows)]
fn run(control: Arc<Clipboard>, owner: Arc<std::sync::atomic::AtomicIsize>, foreground_only: bool) {
    run_system(
        control,
        WindowsSystem {
            owner,
            foreground_only,
            sequence: None,
            pump_messages: false,
        },
        foreground_only,
    )
}

#[cfg(windows)]
mod native {
    use windows_sys::Win32::{
        Foundation::{GlobalFree, HWND},
        System::{DataExchange::*, Memory::*},
        UI::WindowsAndMessaging::{GetForegroundWindow, IsWindow},
    };
    const FORMAT: u32 = 13;
    struct Open;
    impl Drop for Open {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }
    pub fn foreground(window: isize) -> bool {
        window != 0 && unsafe { GetForegroundWindow() == window as HWND }
    }
    pub fn sequence() -> u32 {
        unsafe { GetClipboardSequenceNumber() }
    }
    fn open(window: isize, foreground_only: bool) -> Result<Open, ()> {
        if window == 0
            || unsafe { IsWindow(window as HWND) } == 0
            || (foreground_only && !foreground(window))
        {
            return Err(());
        }
        if unsafe { OpenClipboard(window as HWND) } == 0 {
            return Err(());
        }
        Ok(Open)
    }
    fn png_format() -> u32 {
        unsafe { RegisterClipboardFormatW([80u16, 78, 71, 0].as_ptr()) }
    }
    fn blob(format: u32, limit: usize) -> Result<Option<Vec<u8>>, ()> {
        unsafe {
            if format == 0 || IsClipboardFormatAvailable(format) == 0 {
                return Ok(None);
            }
            let handle = GetClipboardData(format);
            if handle.is_null() {
                return Err(());
            }
            let size = GlobalSize(handle);
            if size == 0 || size > limit {
                return Err(());
            }
            let pointer = GlobalLock(handle) as *const u8;
            if pointer.is_null() {
                return Err(());
            }
            let bytes = std::slice::from_raw_parts(pointer, size).to_vec();
            GlobalUnlock(handle);
            Ok(Some(bytes))
        }
    }
    pub fn read_image(
        window: isize,
        foreground_only: bool,
    ) -> Result<Option<super::formats::Pixels>, ()> {
        let raw = {
            let _open = open(window, foreground_only)?;
            if let Some(png) = blob(png_format(), uu_core::clipboard::MAX_IMAGE_BYTES)? {
                Some((true, png))
            } else if let Some(dib) =
                blob(17, uu_core::clipboard::MAX_IMAGE_PIXELS as usize * 4 + 4096)?
            {
                Some((false, dib))
            } else if let Some(dib) =
                blob(8, uu_core::clipboard::MAX_IMAGE_PIXELS as usize * 4 + 4096)?
            {
                Some((false, dib))
            } else {
                None
            }
        };
        let Some((png, data)) = raw else {
            return Ok(None);
        };
        // Decode after releasing the global OS clipboard lock.
        if png {
            if data.len() < 24 {
                return Err(());
            }
            let width = u32::from_be_bytes(data[16..20].try_into().map_err(|_| ())?);
            let height = u32::from_be_bytes(data[20..24].try_into().map_err(|_| ())?);
            let image = uu_core::clipboard::Image::new(width, height, data).map_err(|_| ())?;
            match super::Value::decode(&uu_core::clipboard::Content::Image(image))
                .map_err(|_| ())?
            {
                super::Value::Image(pixels) => Ok(Some(pixels)),
                _ => Err(()),
            }
        } else {
            super::formats::dib_decode(&data).map(Some).map_err(|_| ())
        }
    }
    struct Allocation(*mut std::ffi::c_void);
    impl Allocation {
        fn new(bytes: &[u8]) -> Result<Self, ()> {
            unsafe {
                let allocation = Self(GlobalAlloc(GMEM_MOVEABLE, bytes.len()));
                if allocation.0.is_null() {
                    return Err(());
                }
                let pointer = GlobalLock(allocation.0) as *mut u8;
                if pointer.is_null() {
                    return Err(());
                }
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len());
                GlobalUnlock(allocation.0);
                Ok(allocation)
            }
        }
        fn publish(&mut self, format: u32) -> bool {
            if format == 0 {
                return false;
            }
            unsafe {
                if SetClipboardData(format, self.0).is_null() {
                    false
                } else {
                    self.0 = std::ptr::null_mut();
                    true
                }
            }
        }
    }
    impl Drop for Allocation {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    GlobalFree(self.0);
                }
            }
        }
    }
    pub fn write_image(
        window: isize,
        pixels: &super::formats::Pixels,
        image: &uu_core::clipboard::Image,
        foreground_only: bool,
    ) -> Result<(), ()> {
        let dib = super::formats::dib_encode(pixels).map_err(|_| ())?;
        let mut dib = Allocation::new(&dib)?;
        let mut png = Allocation::new(&image.png)?;
        let _open = open(window, foreground_only)?;
        if foreground_only && !foreground(window) {
            return Err(());
        }
        if unsafe { EmptyClipboard() } == 0 {
            return Err(());
        }
        let dib_ok = dib.publish(17);
        let png_ok = png.publish(png_format());
        if dib_ok || png_ok {
            Ok(())
        } else {
            Err(())
        }
    }
    pub fn read(window: isize, foreground_only: bool) -> Result<Option<String>, ()> {
        let _open = open(window, foreground_only)?;
        unsafe {
            if IsClipboardFormatAvailable(FORMAT) == 0 {
                return Ok(None);
            }
            let handle = GetClipboardData(FORMAT);
            if handle.is_null() {
                return Err(());
            }
            let size = GlobalSize(handle);
            if size == 0 || size > uu_core::clipboard::MAX_TEXT * 2 + 2 || size % 2 != 0 {
                return Ok(None);
            }
            let pointer = GlobalLock(handle) as *const u16;
            if pointer.is_null() {
                return Err(());
            }
            let units = std::slice::from_raw_parts(pointer, size / 2);
            let end = units.iter().position(|n| *n == 0).unwrap_or(units.len());
            let value = String::from_utf16(&units[..end]).map_err(|_| ());
            GlobalUnlock(handle);
            let value = value?;
            if value.len() > uu_core::clipboard::MAX_TEXT {
                return Ok(None);
            }
            Ok(Some(value))
        }
    }
    pub fn write(window: isize, value: &str, foreground_only: bool) -> Result<(), ()> {
        if value.len() > uu_core::clipboard::MAX_TEXT || value.contains('\0') {
            return Err(());
        }
        let units: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let handle = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2);
            if handle.is_null() {
                return Err(());
            }
            let pointer = GlobalLock(handle) as *mut u16;
            if pointer.is_null() {
                GlobalFree(handle);
                return Err(());
            }
            std::ptr::copy_nonoverlapping(units.as_ptr(), pointer, units.len());
            GlobalUnlock(handle);
            let opened = open(window, foreground_only);
            if opened.is_err() || (foreground_only && !foreground(window)) {
                GlobalFree(handle);
                return Err(());
            }
            let _open = opened.unwrap();
            if EmptyClipboard() == 0 || SetClipboardData(FORMAT, handle).is_null() {
                GlobalFree(handle);
                return Err(());
            }
            Ok(()) // Windows owns the movable allocation after successful SetClipboardData.
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn newline_normalization_prevents_cross_platform_clipboard_echoes() {
        assert_eq!(canonical("中文\r\n🙂"), canonical("中文\n🙂"));
        assert_ne!(canonical("a\rb"), canonical("a\nb"));
    }
    #[test]
    fn clipboard_baseline_handles_non_text_and_pauses_without_republishing_stale_values() {
        let mut host = TextTracker::default();
        assert!(!host.observe(None, false));
        assert!(host.observe(Some("first copied text"), false));
        assert!(!host.observe(Some("first copied text"), false));
        host.applied("synced\r\nvalue");
        assert!(!host.observe(Some("synced\nvalue"), false));
        // A pause observes nothing and retains only the prior fingerprint.
        assert!(host.observe(Some("new host copy while paused"), false));
        let mut client = TextTracker::default();
        assert!(client.observe(Some("initial consent"), true));
        client.applied("synced value");
        assert!(!client.observe(Some("synced value"), true));
        assert!(client.observe(Some("new local copy"), true));
    }
}
