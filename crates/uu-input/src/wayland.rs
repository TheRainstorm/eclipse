//! wlroots input for a compositor-authorized monitor capture. The caller must
//! already hold compositor capture access and a native control grant. This backend
//! never chooses an arbitrary output or opens a kernel input device.
use crate::{HeldInput, InputError, InputEvent, InputInjector, MouseButton};
use std::{
    collections::HashSet,
    ffi::CStr,
    io::{Read, Write},
    os::fd::{AsFd, AsRawFd, FromRawFd},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};
use wayland_client::{
    protocol::{wl_callback, wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat},
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1 as OutputManager,
    zxdg_output_v1::{self, ZxdgOutputV1},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1 as KeyboardManager,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1 as Keyboard,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1 as PointerManager,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1 as Pointer,
};

fn backend(e: impl std::fmt::Display) -> InputError {
    InputError::Backend(format!("Wayland: {e}"))
}
fn unavailable(e: &str) -> InputError {
    InputError::Unavailable(format!("Wayland: {e}"))
}

#[derive(Clone, Debug, Default)]
pub struct OutputTarget {
    pub name: Option<String>,
    pub position: Option<(i32, i32)>,
    pub logical_size: Option<(i32, i32)>,
}
#[derive(Default, Clone)]
struct OutputInfo {
    name: Option<String>,
    position: Option<(i32, i32)>,
    size: Option<(i32, i32)>,
}
fn select_output(outputs: &[OutputInfo], target: &OutputTarget) -> Result<usize, InputError> {
    let names: Vec<_> = outputs
        .iter()
        .enumerate()
        .filter(|(_, o)| target.name.is_some() && o.name == target.name)
        .map(|(i, _)| i)
        .collect();
    if names.len() == 1 {
        return Ok(names[0]);
    }
    let geometry: Vec<_> = outputs
        .iter()
        .enumerate()
        .filter(|(_, o)| {
            target.position.is_some()
                && target.logical_size.is_some()
                && o.position == target.position
                && o.size == target.logical_size
        })
        .map(|(i, _)| i)
        .collect();
    if geometry.len() == 1 {
        return Ok(geometry[0]);
    }
    // Older portals omit mapping metadata. A single compositor output is
    // unambiguous; matching just dimensions with multiple outputs is not.
    if outputs.len() == 1 && (target.position.is_none() || target.logical_size.is_none()) {
        return Ok(0);
    }
    Err(unavailable(
        "无法唯一对应已授权的共享屏幕，请检查门户的屏幕映射信息",
    ))
}
struct Output {
    global: u32,
    proxy: wl_output::WlOutput,
    info: OutputInfo,
}
#[derive(Default)]
struct State {
    seats: Vec<wl_seat::WlSeat>,
    keyboard_capable: bool,
    km: Option<KeyboardManager>,
    pm: Option<PointerManager>,
    om: Option<OutputManager>,
    outputs: Vec<Output>,
    selected: Option<u32>,
    output_lost: bool,
    keymap: Option<Vec<u8>>,
    locked: u32,
    group: u32,
    sync_done: bool,
}
impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        s: &mut Self,
        r: &wl_registry::WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        q: &QueueHandle<Self>,
    ) {
        match e {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => match interface.as_str() {
                "wl_seat" => s.seats.push(r.bind(name, version.min(5), q, ())),
                "wl_output" => s.outputs.push(Output {
                    global: name,
                    proxy: r.bind(name, version.min(4), q, name),
                    info: OutputInfo::default(),
                }),
                "zxdg_output_manager_v1" => s.om = Some(r.bind(name, version.min(3), q, ())),
                "zwp_virtual_keyboard_manager_v1" => s.km = Some(r.bind(name, 1, q, ())),
                "zwlr_virtual_pointer_manager_v1" => {
                    s.pm = Some(r.bind(name, version.min(2), q, ()))
                }
                _ => {}
            },
            wl_registry::Event::GlobalRemove { name } => {
                if s.selected == Some(name) {
                    s.output_lost = true;
                }
                s.outputs.retain(|o| o.global != name);
            }
            _ => {}
        }
    }
}
impl Dispatch<wl_output::WlOutput, u32> for State {
    fn event(
        s: &mut Self,
        _: &wl_output::WlOutput,
        e: wl_output::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let Some(o) = s.outputs.iter_mut().find(|o| o.global == *id) {
            if let wl_output::Event::Name { name } = e {
                o.info.name = Some(name);
            }
        }
    }
}
impl Dispatch<ZxdgOutputV1, u32> for State {
    fn event(
        s: &mut Self,
        _: &ZxdgOutputV1,
        e: zxdg_output_v1::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let Some(o) = s.outputs.iter_mut().find(|o| o.global == *id) {
            match e {
                zxdg_output_v1::Event::LogicalPosition { x, y } => o.info.position = Some((x, y)),
                zxdg_output_v1::Event::LogicalSize { width, height } => {
                    o.info.size = Some((width, height))
                }
                zxdg_output_v1::Event::Name { name } => o.info.name = Some(name),
                _ => {}
            }
        }
    }
}
impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        s: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        e: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_keyboard::Event::Keymap {
                format: wayland_client::WEnum::Value(wl_keyboard::KeymapFormat::XkbV1),
                fd,
                size,
            } if (1..=4 * 1024 * 1024).contains(&size) => {
                let mut bytes = Vec::new();
                if std::fs::File::from(fd)
                    .take(u64::from(size))
                    .read_to_end(&mut bytes)
                    .is_ok()
                    && bytes.len() == size as usize
                    && bytes.last() == Some(&0)
                {
                    s.keymap = Some(bytes);
                }
            }
            wl_keyboard::Event::Modifiers {
                mods_locked, group, ..
            } => {
                s.locked = mods_locked;
                s.group = group;
            }
            _ => {}
        }
    }
}
impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn event(
        s: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        s.sync_done = true;
    }
}
impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        s: &mut Self,
        _: &wl_seat::WlSeat,
        e: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: wayland_client::WEnum::Value(caps),
        } = e
        {
            s.keyboard_capable = caps.contains(wl_seat::Capability::Keyboard);
        }
    }
}
wayland_client::delegate_noop!(State: ignore OutputManager);
wayland_client::delegate_noop!(State: ignore KeyboardManager);
wayland_client::delegate_noop!(State: ignore Keyboard);
wayland_client::delegate_noop!(State: ignore PointerManager);
wayland_client::delegate_noop!(State: ignore Pointer);

fn dispatch(
    conn: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
    wait_ms: i32,
) -> Result<(), InputError> {
    queue.dispatch_pending(state).map_err(backend)?;
    conn.flush().map_err(backend)?;
    if let Some(read) = queue.prepare_read() {
        let mut fd = libc::pollfd {
            fd: conn.as_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized pollfd; the connection owns its live fd.
        let ready = unsafe { libc::poll(&mut fd, 1, wait_ms) };
        if ready < 0 {
            return Err(backend(std::io::Error::last_os_error()));
        }
        if ready > 0 {
            read.read().map_err(backend)?;
        }
    }
    queue.dispatch_pending(state).map_err(backend)?;
    if state.output_lost {
        return Err(unavailable("共享屏幕已移除，请重新连接"));
    }
    Ok(())
}
fn roundtrip(
    conn: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
    stop: &AtomicBool,
) -> Result<(), InputError> {
    state.sync_done = false;
    conn.display().sync(&queue.handle(), ());
    let until = Instant::now() + Duration::from_secs(2);
    while !state.sync_done {
        if stop.load(Ordering::Acquire) || Instant::now() >= until {
            return Err(unavailable("合成器响应超时"));
        }
        dispatch(conn, queue, state, 30)?;
    }
    Ok(())
}

/// Confined to the Wayland worker; XKB objects are never shared between threads.
struct KeyState {
    state: *mut xkbcommon_dl::xkb_state,
    bytes: Vec<u8>,
    down: HashSet<u32>,
}
impl KeyState {
    fn new(bytes: Option<&[u8]>, locked: u32, group: u32) -> Result<Self, InputError> {
        let x = xkbcommon_dl::xkbcommon_option().ok_or_else(|| unavailable("缺少 libxkbcommon"))?;
        let source = bytes
            .map(CStr::from_bytes_with_nul)
            .transpose()
            .map_err(backend)?;
        // SAFETY: checked allocations; XKB copies map input; all refs balanced.
        unsafe {
            let context =
                (x.xkb_context_new)(xkbcommon_dl::xkb_context_flags::XKB_CONTEXT_NO_FLAGS);
            if context.is_null() {
                return Err(unavailable("XKB context"));
            }
            let map = if let Some(text) = source {
                (x.xkb_keymap_new_from_string)(
                    context,
                    text.as_ptr(),
                    xkbcommon_dl::xkb_keymap_format::XKB_KEYMAP_FORMAT_TEXT_V1,
                    xkbcommon_dl::xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS,
                )
            } else {
                (x.xkb_keymap_new_from_names)(
                    context,
                    std::ptr::null(),
                    xkbcommon_dl::xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS,
                )
            };
            (x.xkb_context_unref)(context);
            if map.is_null() {
                return Err(unavailable("XKB keymap"));
            }
            let text = (x.xkb_keymap_get_as_string)(
                map,
                xkbcommon_dl::xkb_keymap_format::XKB_KEYMAP_FORMAT_TEXT_V1,
            );
            let state = (x.xkb_state_new)(map);
            (x.xkb_keymap_unref)(map);
            if text.is_null() || state.is_null() {
                if !text.is_null() {
                    libc::free(text as *mut _);
                }
                if !state.is_null() {
                    (x.xkb_state_unref)(state);
                }
                return Err(unavailable("XKB state"));
            }
            let bytes = CStr::from_ptr(text).to_bytes_with_nul().to_vec();
            libc::free(text as *mut _);
            (x.xkb_state_update_mask)(state, 0, 0, locked, 0, 0, group);
            Ok(Self {
                state,
                bytes,
                down: HashSet::new(),
            })
        }
    }
    fn key(&mut self, code: u32, pressed: bool) -> [u32; 4] {
        let changed = if pressed {
            self.down.insert(code)
        } else {
            self.down.remove(&code)
        };
        let x = xkbcommon_dl::xkbcommon_handle();
        use xkbcommon_dl::xkb_state_component as C;
        // SAFETY: exclusively owned live state; Wayland keycodes are evdev+8.
        unsafe {
            if changed {
                (x.xkb_state_update_key)(
                    self.state,
                    code + 8,
                    if pressed {
                        xkbcommon_dl::xkb_key_direction::XKB_KEY_DOWN
                    } else {
                        xkbcommon_dl::xkb_key_direction::XKB_KEY_UP
                    },
                );
            }
            [
                (x.xkb_state_serialize_mods)(self.state, C::XKB_STATE_MODS_DEPRESSED),
                (x.xkb_state_serialize_mods)(self.state, C::XKB_STATE_MODS_LATCHED),
                (x.xkb_state_serialize_mods)(self.state, C::XKB_STATE_MODS_LOCKED),
                (x.xkb_state_serialize_layout)(self.state, C::XKB_STATE_LAYOUT_EFFECTIVE),
            ]
        }
    }
}
impl Drop for KeyState {
    fn drop(&mut self) {
        unsafe {
            (xkbcommon_dl::xkbcommon_handle().xkb_state_unref)(self.state);
        }
    }
}

struct Command {
    event: InputEvent,
    until: Instant,
    reply: mpsc::SyncSender<Result<(), InputError>>,
}
pub struct WlrootsInjector {
    tx: mpsc::SyncSender<Command>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    held: HeldInput,
}
impl WlrootsInjector {
    pub fn connect(pixels: (u32, u32), target: OutputTarget) -> Result<Self, InputError> {
        if pixels.0 == 0 || pixels.1 == 0 {
            return Err(unavailable("共享屏幕尺寸无效"));
        }
        let (ready, rx) = mpsc::sync_channel(1);
        let (tx, commands) = mpsc::sync_channel(32);
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let worker = std::thread::Builder::new()
            .name("wayland-input".into())
            .spawn(move || {
                let result = Worker::open(pixels, &target, &cancelled);
                match result {
                    Ok(mut worker) => {
                        if ready.send(Ok(())).is_ok() {
                            worker.run(commands, &cancelled);
                        }
                    }
                    Err(e) => {
                        let _ = ready.send(Err(e));
                    }
                }
            })
            .map_err(backend)?;
        match rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(Self {
                tx,
                stop,
                worker: Some(worker),
                held: HeldInput::default(),
            }),
            result => {
                stop.store(true, Ordering::Release);
                Err(match result {
                    Ok(Err(e)) => e,
                    _ => unavailable("初始化输入超时"),
                })
            }
        }
    }
}
impl InputInjector for WlrootsInjector {
    fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
        if !self.held.should_emit(event) {
            return Ok(());
        }
        let (reply, rx) = mpsc::sync_channel(1);
        self.tx
            .try_send(Command {
                event,
                until: Instant::now() + Duration::from_millis(250),
                reply,
            })
            .map_err(|_| unavailable("输入会话已关闭或队列繁忙"))?;
        rx.recv_timeout(Duration::from_millis(300))
            .map_err(|_| unavailable("输入处理超时"))??;
        self.held.applied(event);
        Ok(())
    }
    fn release_all(&mut self) -> Result<(), InputError> {
        crate::release_owned(self, self.held.releases())
    }
}
impl Drop for WlrootsInjector {
    fn drop(&mut self) {
        let _ = self.release_all();
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.worker.take() {
            let _ = t.join();
        }
    }
}
struct Worker {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    keyboard: Keyboard,
    pointer: Pointer,
    keys: KeyState,
    pixels: (u32, u32),
    started: Instant,
    wheel: (i32, i32),
}
impl Worker {
    fn open(
        pixels: (u32, u32),
        target: &OutputTarget,
        stop: &AtomicBool,
    ) -> Result<Self, InputError> {
        let conn = Connection::connect_to_env().map_err(backend)?;
        let mut queue = conn.new_event_queue();
        let q = queue.handle();
        let mut state = State::default();
        let _registry = conn.display().get_registry(&q, ());
        roundtrip(&conn, &mut queue, &mut state, stop)?;
        if state.seats.len() != 1 {
            return Err(unavailable("无法唯一对应共享屏幕的输入 seat"));
        }
        let _outputs: Vec<_> = state
            .om
            .as_ref()
            .map(|m| {
                state
                    .outputs
                    .iter()
                    .map(|o| m.get_xdg_output(&o.proxy, &q, o.global))
                    .collect()
            })
            .unwrap_or_default();
        roundtrip(&conn, &mut queue, &mut state, stop)?;
        // A headless seat can have no physical keyboard. get_keyboard before
        // the advertised capability is a fatal Wayland protocol violation.
        let _seat_keyboard = state
            .keyboard_capable
            .then(|| state.seats[0].get_keyboard(&q, ()));
        if _seat_keyboard.is_some() {
            roundtrip(&conn, &mut queue, &mut state, stop)?;
        }
        let index = select_output(
            &state
                .outputs
                .iter()
                .map(|o| o.info.clone())
                .collect::<Vec<_>>(),
            target,
        )?;
        state.selected = Some(state.outputs[index].global);
        let pm = state
            .pm
            .as_ref()
            .ok_or_else(|| unavailable("合成器未提供虚拟指针协议"))?;
        if pm.version() < 2 {
            return Err(unavailable("需要支持屏幕映射的虚拟指针 v2"));
        }
        let pointer = pm.create_virtual_pointer_with_output(
            Some(&state.seats[0]),
            Some(&state.outputs[index].proxy),
            &q,
            (),
        );
        let keyboard = state
            .km
            .as_ref()
            .ok_or_else(|| unavailable("合成器未提供虚拟键盘协议"))?
            .create_virtual_keyboard(&state.seats[0], &q, ());
        let keys = KeyState::new(state.keymap.as_deref(), state.locked, state.group)?;
        // SAFETY: anonymous CLOEXEC fd owned by File immediately; keymap never
        // touches a named file or exposes content to another login session.
        let fd = unsafe { libc::memfd_create(c"uu-input-keymap".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err(backend(std::io::Error::last_os_error()));
        }
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.write_all(&keys.bytes).map_err(backend)?;
        keyboard.keymap(1, file.as_fd(), keys.bytes.len() as u32);
        roundtrip(&conn, &mut queue, &mut state, stop)?;
        Ok(Self {
            conn,
            queue,
            state,
            keyboard,
            pointer,
            keys,
            pixels,
            started: Instant::now(),
            wheel: (0, 0),
        })
    }
    fn emit(&mut self, event: InputEvent) -> Result<(), InputError> {
        let time = self.started.elapsed().as_millis() as u32;
        match event {
            InputEvent::MouseMoveAbsolute { x, y } => self.pointer.motion_absolute(
                time,
                x.clamp(0, self.pixels.0.saturating_sub(1) as i32) as u32,
                y.clamp(0, self.pixels.1.saturating_sub(1) as i32) as u32,
                self.pixels.0,
                self.pixels.1,
            ),
            InputEvent::MouseMoveRelative { dx, dy } => {
                self.pointer.motion(time, dx as f64, dy as f64)
            }
            InputEvent::MouseButton { button, pressed } => self.pointer.button(
                time,
                match button {
                    MouseButton::Left => 0x110,
                    MouseButton::Right => 0x111,
                    MouseButton::Middle => 0x112,
                    MouseButton::Back => 0x113,
                    MouseButton::Forward => 0x114,
                },
                if pressed {
                    wl_pointer::ButtonState::Pressed
                } else {
                    wl_pointer::ButtonState::Released
                },
            ),
            InputEvent::MouseScroll { dx, dy } => {
                self.wheel.0 = self.wheel.0.saturating_add(dx.clamp(-1920, 1920));
                self.wheel.1 = self.wheel.1.saturating_sub(dy.clamp(-1920, 1920));
                self.pointer.axis_source(wl_pointer::AxisSource::Wheel);
                for (axis, value) in [
                    (wl_pointer::Axis::HorizontalScroll, self.wheel.0),
                    (wl_pointer::Axis::VerticalScroll, self.wheel.1),
                ] {
                    let steps = value / 120;
                    if steps != 0 {
                        self.pointer
                            .axis_discrete(time, axis, f64::from(steps) * 15., steps);
                    }
                }
                self.wheel.0 %= 120;
                self.wheel.1 %= 120;
            }
            InputEvent::Key { key, pressed } => {
                let code = key.mapping()?.evdev as u32;
                let mods = self.keys.key(code, pressed);
                self.keyboard.key(time, code, u32::from(pressed));
                self.keyboard.modifiers(mods[0], mods[1], mods[2], mods[3]);
            }
        }
        self.pointer.frame();
        self.conn.flush().map_err(backend)
    }
    fn run(&mut self, commands: mpsc::Receiver<Command>, stop: &AtomicBool) {
        while !stop.load(Ordering::Acquire) {
            if dispatch(&self.conn, &mut self.queue, &mut self.state, 0).is_err() {
                break;
            }
            match commands.recv_timeout(Duration::from_millis(30)) {
                Ok(c) => {
                    let result = if Instant::now() > c.until || stop.load(Ordering::Acquire) {
                        Err(unavailable("过期输入已取消"))
                    } else {
                        self.emit(c.event)
                    };
                    let failed = result.is_err();
                    let _ = c.reply.send(result);
                    if failed {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
        }
        self.keyboard.destroy();
        self.pointer.destroy();
        let _ = self.conn.flush();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_selection_uses_identity_or_exact_logical_geometry_and_rejects_ambiguity() {
        let outputs = [
            OutputInfo {
                name: Some("OUT-1".into()),
                position: Some((-1280, 0)),
                size: Some((1280, 720)),
            },
            OutputInfo {
                name: Some("OUT-2".into()),
                position: Some((0, 0)),
                size: Some((1280, 720)),
            },
        ];
        assert_eq!(
            select_output(
                &outputs,
                &OutputTarget {
                    name: Some("OUT-2".into()),
                    ..Default::default()
                }
            )
            .unwrap(),
            1
        );
        assert_eq!(
            select_output(
                &outputs,
                &OutputTarget {
                    position: Some((-1280, 0)),
                    logical_size: Some((1280, 720)),
                    ..Default::default()
                }
            )
            .unwrap(),
            0
        );
        assert!(select_output(
            &outputs,
            &OutputTarget {
                logical_size: Some((1280, 720)),
                ..Default::default()
            }
        )
        .is_err());
        assert_eq!(
            select_output(&outputs[..1], &OutputTarget::default()).unwrap(),
            0
        );
        assert!(select_output(&[], &OutputTarget::default()).is_err());
        assert!(select_output(
            &[outputs[0].clone(), outputs[0].clone()],
            &OutputTarget {
                name: Some("OUT-1".into()),
                ..Default::default()
            }
        )
        .is_err());
    }
    #[test]
    fn xkb_state_tracks_both_shifts_repeats_and_owned_modifier_releases() {
        let mut keys = KeyState::new(None, 0, 0).unwrap();
        let left = keys.key(42, true);
        assert_ne!(left[0], 0);
        assert_eq!(keys.key(42, true), left);
        assert_eq!(keys.key(54, true), left);
        assert_eq!(keys.key(42, false), left);
        assert_eq!(keys.key(54, false)[0], 0);
        assert_eq!(keys.key(30, true)[0], 0);
        assert_eq!(keys.key(30, false)[0], 0);
    }
}
