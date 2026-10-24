//! Native compositor capture. Only use interfaces advertised on this user's
//! Wayland connection; other compositors retain the system portal path.
use crate::{backend::Monitor, Capture, CaptureError, CaptureSource, DesktopRect, OpenOptions};
use std::{
    fs::File,
    os::fd::{AsFd, AsRawFd, FromRawFd},
    sync::{
        atomic::{AtomicBool, Ordering},
        OnceLock,
    },
    time::{Duration, Instant},
};
use uu_media::{frame::PixelFormat, RawFrame, Timestamp};
use wayland_client::{
    protocol::{wl_buffer, wl_callback, wl_output, wl_registry, wl_shm, wl_shm_pool},
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
};
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1 as OutputManager,
    zxdg_output_v1::{self, ZxdgOutputV1},
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1 as CopyFrame},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1 as CopyManager,
};

fn error(e: impl std::fmt::Display) -> CaptureError {
    CaptureError::Backend(format!("Wayland 原生采集：{e}"))
}
#[derive(Default)]
struct Output {
    global: u32,
    proxy: Option<wl_output::WlOutput>,
    name: String,
    position: (i32, i32),
    pixels: (u32, u32),
    logical: Option<(i32, i32)>,
    scale: i32,
    rotated: bool,
    transform: Option<wl_output::Transform>,
}
impl Output {
    fn size(&self) -> (u32, u32) {
        if self.rotated {
            (self.pixels.1, self.pixels.0)
        } else {
            self.pixels
        }
    }
    fn target(&self) -> uu_input::wayland::OutputTarget {
        let (w, h) = self.size();
        let scale = self.scale.max(1);
        uu_input::wayland::OutputTarget {
            name: Some(self.name.clone()),
            position: Some(self.position),
            logical_size: Some(self.logical.unwrap_or((w as i32 / scale, h as i32 / scale))),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    format: wl_shm::Format,
    width: u32,
    height: u32,
    stride: u32,
}
impl Layout {
    fn length(self) -> Result<usize, CaptureError> {
        if !matches!(
            self.format,
            wl_shm::Format::Argb8888
                | wl_shm::Format::Xrgb8888
                | wl_shm::Format::Abgr8888
                | wl_shm::Format::Xbgr8888
        ) {
            return Err(error("合成器未提供支持的共享内存像素格式"));
        }
        let length = (self.stride as usize).checked_mul(self.height as usize);
        if !(1..=8192).contains(&self.width)
            || !(1..=8192).contains(&self.height)
            || self.stride < self.width * 4
            || self.stride > i32::MAX as u32
        {
            return Err(error("屏幕缓冲区尺寸或行跨度无效"));
        }
        length
            .filter(|n| *n <= 256 * 1024 * 1024)
            .ok_or_else(|| error("屏幕缓冲区过大"))
    }
}
#[derive(Default)]
struct State {
    outputs: Vec<Output>,
    manager: Option<CopyManager>,
    shm: Option<wl_shm::WlShm>,
    output_manager: Option<OutputManager>,
    selected: Option<u32>,
    lost: bool,
    sync_done: bool,
    layout: Option<Layout>,
    formats_done: bool,
    ready: bool,
    failed: bool,
    inverted: bool,
    released: bool,
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
                "wl_output" => s.outputs.push(Output {
                    global: name,
                    proxy: Some(r.bind(name, version.min(4), q, name)),
                    scale: 1,
                    ..Default::default()
                }),
                "wl_shm" => s.shm = Some(r.bind(name, 1, q, ())),
                "zwlr_screencopy_manager_v1" => {
                    s.manager = Some(r.bind(name, version.min(3), q, ()))
                }
                "zxdg_output_manager_v1" => {
                    s.output_manager = Some(r.bind(name, version.min(3), q, ()))
                }
                _ => {}
            },
            wl_registry::Event::GlobalRemove { name } => {
                if s.selected == Some(name) {
                    s.lost = true;
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
        let Some(o) = s.outputs.iter_mut().find(|o| o.global == *id) else {
            return;
        };
        match e {
            wl_output::Event::Name { name } => o.name = name,
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                ..
            } if flags.contains(wl_output::Mode::Current) && width > 0 && height > 0 => {
                o.pixels = (width as u32, height as u32)
            }
            wl_output::Event::Geometry {
                x,
                y,
                transform: WEnum::Value(t),
                ..
            } => {
                o.position = (x, y);
                o.transform = Some(t);
                o.rotated = matches!(
                    t,
                    wl_output::Transform::_90
                        | wl_output::Transform::_270
                        | wl_output::Transform::Flipped90
                        | wl_output::Transform::Flipped270
                );
            }
            wl_output::Event::Scale { factor } => o.scale = factor.max(1),
            _ => {}
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
        let Some(o) = s.outputs.iter_mut().find(|o| o.global == *id) else {
            return;
        };
        match e {
            zxdg_output_v1::Event::Name { name } => o.name = name,
            zxdg_output_v1::Event::LogicalPosition { x, y } => o.position = (x, y),
            zxdg_output_v1::Event::LogicalSize { width, height } if width > 0 && height > 0 => {
                o.logical = Some((width, height))
            }
            _ => {}
        }
    }
}
impl Dispatch<CopyFrame, ()> for State {
    fn event(
        s: &mut Self,
        _: &CopyFrame,
        e: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            zwlr_screencopy_frame_v1::Event::Buffer {
                format: WEnum::Value(format),
                width,
                height,
                stride,
            } => {
                s.layout = Some(Layout {
                    format,
                    width,
                    height,
                    stride,
                })
            }
            zwlr_screencopy_frame_v1::Event::BufferDone => s.formats_done = true,
            zwlr_screencopy_frame_v1::Event::Flags {
                flags: WEnum::Value(flags),
            } => s.inverted = flags.contains(zwlr_screencopy_frame_v1::Flags::YInvert),
            zwlr_screencopy_frame_v1::Event::Ready { .. } => s.ready = true,
            zwlr_screencopy_frame_v1::Event::Failed => s.failed = true,
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
impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn event(
        s: &mut Self,
        _: &wl_buffer::WlBuffer,
        e: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(e, wl_buffer::Event::Release) {
            s.released = true;
        }
    }
}
wayland_client::delegate_noop!(State: ignore OutputManager);
wayland_client::delegate_noop!(State: ignore CopyManager);
wayland_client::delegate_noop!(State: ignore wl_shm::WlShm);
wayland_client::delegate_noop!(State: ignore wl_shm_pool::WlShmPool);

struct Session {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
}
impl Session {
    fn discover(stop: &AtomicBool) -> Result<Self, CaptureError> {
        let connection = Connection::connect_to_env().map_err(error)?;
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        let mut this = Self {
            connection,
            queue,
            state: State::default(),
        };
        this.sync(stop)?;
        if let Some(manager) = &this.state.output_manager {
            for o in &this.state.outputs {
                manager.get_xdg_output(o.proxy.as_ref().unwrap(), &this.queue.handle(), o.global);
            }
        }
        this.sync(stop)?;
        this.state
            .outputs
            .retain(|o| o.pixels.0 > 0 && o.pixels.1 > 0);
        for o in &mut this.state.outputs {
            if o.name.is_empty() {
                o.name = format!("Wayland Output {}", o.global);
            }
        }
        this.state.outputs.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(this)
    }
    fn dispatch(&mut self, stop: &AtomicBool, deadline: Instant) -> Result<(), CaptureError> {
        if stop.load(Ordering::Acquire) {
            return Err(error("采集已取消"));
        }
        if Instant::now() >= deadline {
            return Err(error("合成器响应超时"));
        }
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(error)?;
        self.connection.flush().map_err(error)?;
        if let Some(read) = self.queue.prepare_read() {
            let mut fd = libc::pollfd {
                fd: self.connection.as_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one initialized pollfd referring to this live connection.
            let ready = unsafe { libc::poll(&mut fd, 1, 30) };
            if ready < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error(e));
                }
            } else if ready > 0 {
                read.read().map_err(error)?;
            }
        }
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(error)?;
        if self.state.lost {
            return Err(error("共享显示器已移除，请切换显示器或重新连接"));
        }
        if stop.load(Ordering::Acquire) {
            return Err(error("采集已取消"));
        }
        Ok(())
    }
    fn sync(&mut self, stop: &AtomicBool) -> Result<(), CaptureError> {
        self.state.sync_done = false;
        self.connection.display().sync(&self.queue.handle(), ());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !self.state.sync_done {
            self.dispatch(stop, deadline)?;
        }
        Ok(())
    }
}

/// Capabilities, not pixels. Cache the compositor's protocol set for this
/// process; each capture still validates its own live connection and output.
pub fn available() -> bool {
    if std::env::var("UU_WAYLAND_CAPTURE").as_deref() == Ok("portal") {
        return false;
    }
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    if let Some(available) = AVAILABLE.get() {
        return *available;
    }
    // A temporary socket/backend failure is not proof that the compositor
    // lacks this protocol; a later connection must be able to probe again.
    let Ok(session) = Session::discover(&AtomicBool::new(false)) else {
        return false;
    };
    let available = session.state.manager.is_some() && session.state.shm.is_some();
    let _ = AVAILABLE.set(available);
    available
}
pub fn monitors() -> Result<Vec<Monitor>, CaptureError> {
    let session = Session::discover(&AtomicBool::new(false))?;
    Ok(session
        .state
        .outputs
        .iter()
        .enumerate()
        .map(|(index, o)| {
            let (width, height) = o.size();
            Monitor {
                name: o.name.clone(),
                x: o.position.0,
                y: o.position.1,
                width,
                height,
                primary: index == 0,
            }
        })
        .collect())
}

struct Buffer {
    layout: Layout,
    pixels: memmap2::MmapMut,
    buffer: wl_buffer::WlBuffer,
    pool: wl_shm_pool::WlShmPool,
}
impl Buffer {
    fn new(
        layout: Layout,
        shm: &wl_shm::WlShm,
        q: &QueueHandle<State>,
    ) -> Result<Self, CaptureError> {
        let length = layout.length()?;
        // SAFETY: fresh anonymous fd, immediately transferred to its File owner.
        let fd = unsafe {
            libc::memfd_create(
                c"eclipse-screencopy".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if fd < 0 {
            return Err(error(std::io::Error::last_os_error()));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        file.set_len(length as u64).map_err(error)?;
        // Prevent size changes through the compositor's duplicated fd.
        let sealed = unsafe {
            libc::fcntl(
                file.as_raw_fd(),
                libc::F_ADD_SEALS,
                libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL,
            )
        };
        if sealed < 0 {
            return Err(error(std::io::Error::last_os_error()));
        }
        // SAFETY: fixed-size private file; compositor writes only during an
        // outstanding Copy, and we read only after the matching Ready event.
        let pixels =
            unsafe { memmap2::MmapOptions::new().len(length).map_mut(&file) }.map_err(error)?;
        let pool = shm.create_pool(file.as_fd(), length as i32, q, ());
        let buffer = pool.create_buffer(
            0,
            layout.width as i32,
            layout.height as i32,
            layout.stride as i32,
            layout.format,
            q,
            (),
        );
        Ok(Self {
            layout,
            pixels,
            buffer,
            pool,
        })
    }
}
impl Drop for Buffer {
    fn drop(&mut self) {
        self.buffer.destroy();
        self.pool.destroy();
    }
}
struct Frame(CopyFrame);
impl Drop for Frame {
    fn drop(&mut self) {
        self.0.destroy();
    }
}

pub struct WaylandCapture {
    session: Session,
    options: OpenOptions,
    output: wl_output::WlOutput,
    target: uu_input::wayland::OutputTarget,
    transform: wl_output::Transform,
    buffer: Option<Buffer>,
    size: (u32, u32),
    started: Instant,
}
impl WaylandCapture {
    pub fn open(source: &CaptureSource, options: OpenOptions) -> Result<Self, CaptureError> {
        let mut session = Session::discover(&options.cancelled)?;
        if session.state.manager.is_none() || session.state.shm.is_none() {
            return Err(CaptureError::NoBackend);
        }
        let selected = match source {
            CaptureSource::Monitor(name)
            | CaptureSource::Desktop {
                initial_monitor: Some(name),
            } => session.state.outputs.iter().find(|o| &o.name == name),
            CaptureSource::Primary
            | CaptureSource::Desktop {
                initial_monitor: None,
            } => session.state.outputs.first(),
            _ => None,
        }
        .ok_or_else(|| error("没有找到指定的活动显示器"))?;
        let (output, target, global, transform) = (
            selected.proxy.clone().unwrap(),
            selected.target(),
            selected.global,
            selected.transform.unwrap_or(wl_output::Transform::Normal),
        );
        session.state.selected = Some(global);
        let mut this = Self {
            session,
            options,
            output,
            target,
            transform,
            buffer: None,
            size: (0, 0),
            started: Instant::now(),
        };
        let first = this.read_frame()?;
        this.size = (first.width, first.height);
        Ok(this)
    }
    fn read_frame(&mut self) -> Result<RawFrame, CaptureError> {
        let s = &mut self.session;
        let deadline = Instant::now() + Duration::from_secs(2);
        if self.buffer.is_some() {
            while !s.state.released {
                s.dispatch(&self.options.cancelled, deadline)?;
            }
        }
        s.state.layout = None;
        s.state.formats_done = false;
        s.state.ready = false;
        s.state.failed = false;
        s.state.inverted = false;
        let frame = Frame(s.state.manager.as_ref().unwrap().capture_output(
            1,
            &self.output,
            &s.queue.handle(),
            (),
        ));
        while !s.state.failed
            && !(s.state.layout.is_some() && (frame.0.version() < 3 || s.state.formats_done))
        {
            // Version 3 may advertise only DMA-BUF; do not wait for a buffer
            // event that cannot arrive in this CPU-backed implementation.
            if s.state.formats_done && s.state.layout.is_none() {
                return Err(error("合成器不支持共享内存采集"));
            }
            s.dispatch(&self.options.cancelled, deadline)?;
        }
        if s.state.failed {
            return Err(CaptureError::Suspended("合成器暂时拒绝屏幕采集".into()));
        }
        let layout = s.state.layout.unwrap();
        layout.length()?;
        if self.buffer.as_ref().is_none_or(|b| b.layout != layout) {
            self.buffer = Some(Buffer::new(
                layout,
                s.state.shm.as_ref().unwrap(),
                &s.queue.handle(),
            )?);
        }
        s.state.released = false;
        frame.0.copy(&self.buffer.as_ref().unwrap().buffer);
        while !s.state.ready && !s.state.failed {
            s.dispatch(&self.options.cancelled, deadline)?;
        }
        if s.state.failed {
            return Err(CaptureError::Suspended("合成器暂时拒绝屏幕采集".into()));
        }
        let (width, height, pixels) = unpack(
            &self.buffer.as_ref().unwrap().pixels,
            layout,
            s.state.inverted,
            self.transform,
        )?;
        Ok(RawFrame {
            width,
            height,
            format: PixelFormat::Bgra8,
            stride: width * 4,
            data: pixels.into(),
            pts: Timestamp(self.started.elapsed().as_micros().min(u64::MAX as u128) as u64),
            gpu: false,
            dma_buf: None,
            d3d11: None,
        })
    }
}
fn unpack(
    bytes: &[u8],
    layout: Layout,
    inverted: bool,
    transform: wl_output::Transform,
) -> Result<(u32, u32, Vec<u8>), CaptureError> {
    let length = layout.length()?;
    if bytes.len() < length {
        return Err(error("屏幕缓冲区不完整"));
    }
    let row = layout.width as usize * 4;
    let mut pixels = vec![0; row * layout.height as usize];
    let rgba = matches!(
        layout.format,
        wl_shm::Format::Abgr8888 | wl_shm::Format::Xbgr8888
    );
    use wl_output::Transform as T;
    let rotation = match transform {
        T::_90 | T::Flipped90 => 1,
        T::_180 | T::Flipped180 => 2,
        T::_270 | T::Flipped270 => 3,
        _ => 0,
    };
    let flipped = matches!(
        transform,
        T::Flipped | T::Flipped90 | T::Flipped180 | T::Flipped270
    );
    let (width, height) = if rotation % 2 == 1 {
        (layout.height, layout.width)
    } else {
        (layout.width, layout.height)
    };
    if transform != T::Normal {
        for y in 0..layout.height as usize {
            let source_y = if inverted {
                layout.height as usize - 1 - y
            } else {
                y
            };
            for x in 0..layout.width as usize {
                let (mut dx, dy) = match rotation {
                    1 => (layout.height as usize - 1 - y, x),
                    2 => (
                        layout.width as usize - 1 - x,
                        layout.height as usize - 1 - y,
                    ),
                    3 => (y, layout.width as usize - 1 - x),
                    _ => (x, y),
                };
                if flipped {
                    dx = width as usize - 1 - dx;
                }
                let input = source_y * layout.stride as usize + x * 4;
                let output = (dy * width as usize + dx) * 4;
                pixels[output..output + 4].copy_from_slice(&bytes[input..input + 4]);
                if rgba {
                    pixels.swap(output, output + 2);
                }
                pixels[output + 3] = 255;
            }
        }
        return Ok((width, height, pixels));
    }
    for (y, dst) in pixels.chunks_exact_mut(row).enumerate() {
        let source = if inverted {
            layout.height as usize - 1 - y
        } else {
            y
        } * layout.stride as usize;
        dst.copy_from_slice(&bytes[source..source + row]);
        for p in dst.chunks_exact_mut(4) {
            if rgba {
                p.swap(0, 2);
            }
            p[3] = 255;
        }
    }
    Ok((width, height, pixels))
}
impl Capture for WaylandCapture {
    fn resolution(&self) -> (u32, u32) {
        self.size
    }
    fn desktop_rect(&self) -> Option<DesktopRect> {
        Some(DesktopRect {
            x: 0,
            y: 0,
            width: self.size.0,
            height: self.size.1,
        })
    }
    fn input_injector(&self) -> Option<Box<dyn uu_input::InputInjector>> {
        if !self.options.allow_input {
            return None;
        }
        match uu_input::wayland::WlrootsInjector::connect(self.size, self.target.clone()) {
            Ok(input) => Some(Box::new(input)),
            Err(e) => {
                tracing::warn!(%e,"native Wayland capture has no input backend");
                None
            }
        }
    }
    fn next_frame(&mut self) -> Result<RawFrame, CaptureError> {
        let frame = self.read_frame()?;
        if (frame.width, frame.height) != self.size {
            return Err(error("显示器尺寸已变化，请重新连接"));
        }
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shm_formats_padding_and_inversion_preserve_pixel_identity() {
        for format in [
            wl_shm::Format::Xrgb8888,
            wl_shm::Format::Argb8888,
            wl_shm::Format::Xbgr8888,
            wl_shm::Format::Abgr8888,
        ] {
            let layout = Layout {
                format,
                width: 1,
                height: 2,
                stride: 8,
            };
            let bytes = [1, 2, 3, 0, 99, 99, 99, 99, 4, 5, 6, 0, 99, 99, 99, 99];
            let mut expected = vec![4, 5, 6, 255, 1, 2, 3, 255];
            if matches!(format, wl_shm::Format::Xbgr8888 | wl_shm::Format::Abgr8888) {
                for p in expected.chunks_exact_mut(4) {
                    p.swap(0, 2);
                }
            }
            assert_eq!(
                unpack(&bytes, layout, true, wl_output::Transform::Normal)
                    .unwrap()
                    .2,
                expected
            );
            assert!(unpack(&bytes[..15], layout, false, wl_output::Transform::Normal).is_err());
        }
    }
}
