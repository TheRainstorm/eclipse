//! Wayland capture and optional input share one compositor-authorized session.
//! PipeWire objects stay on their owning thread; only checked CPU frames cross it.
use crate::{Capture, CaptureError, CaptureSource, DesktopRect, OpenOptions};
use ashpd::desktop::{
    remote_desktop::{DeviceType, RemoteDesktop},
    screencast::{Screencast, Stream},
    PersistMode, Session,
};
use futures_util::{stream::BoxStream, StreamExt};
use std::{
    sync::{
        atomic::{AtomicBool,AtomicU32, Ordering},
        mpsc, Arc, Condvar, Mutex, OnceLock,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use uu_media::frame::RawFrame;
mod input;
mod pipewire;
mod request;

fn error(value: impl std::fmt::Display) -> CaptureError {
    CaptureError::Backend(format!("Wayland 屏幕共享：{value}"))
}
fn portal_error(value: ashpd::Error) -> CaptureError {
    match value {
        ashpd::Error::Response(_) => CaptureError::Denied,
        ashpd::Error::PortalNotFound(_) => {
            error("未找到桌面共享门户，请安装并启动适合当前桌面的 xdg-desktop-portal 后端")
        }
        other => error(other),
    }
}
fn runtime() -> Result<&'static tokio::runtime::Runtime, CaptureError> {
    // ashpd shares its D-Bus connection. Keep its executor alive across sessions.
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("desktop-portal")
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(error)
}
async fn cancelled(stop: &AtomicBool) {
    while !stop.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

#[derive(Default)]
struct Frames {
    latest: Option<RawFrame>,
    failure: Option<String>,
    portal_finished: bool,
}
#[derive(Default)]
struct FrameSlot {
    value: Mutex<Frames>,
    changed: Condvar,
}
impl FrameSlot {
    fn portal_finished(&self) {
        self.value.lock().unwrap().portal_finished = true;
        self.changed.notify_all();
    }
    fn wait_portal_closed(&self) {
        let state = self.value.lock().unwrap();
        let _ = self
            .changed
            .wait_timeout_while(state, Duration::from_millis(600), |state| {
                !state.portal_finished
            });
    }
    fn fail(&self, failure: impl ToString) {
        self.value.lock().unwrap().failure = Some(failure.to_string());
        self.changed.notify_all();
    }
    fn publish(&self, frame: RawFrame) {
        self.value.lock().unwrap().latest = Some(frame);
        self.changed.notify_all();
    }
    fn frame(&self, stop: &AtomicBool) -> Result<RawFrame, CaptureError> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut state = self.value.lock().unwrap();
        loop {
            if stop.load(Ordering::Acquire) {
                return Err(error("共享已取消"));
            }
            if let Some(failure) = &state.failure {
                return Err(error(failure));
            }
            if let Some(frame) = &state.latest {
                return Ok(frame.clone());
            }
            if Instant::now() >= deadline {
                return Err(error("PipeWire 未能在 10 秒内提供画面"));
            }
            state = self
                .changed
                .wait_timeout(state, Duration::from_millis(30))
                .unwrap()
                .0;
        }
    }
}

enum PortalSession {
    Remote(Session<'static, RemoteDesktop<'static>>),
    Cast(Session<'static, Screencast<'static>>),
}
impl PortalSession {
    async fn close(&self) {
        let close = async {
            match self {
                Self::Remote(s) => s.close().await,
                Self::Cast(s) => s.close().await,
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(2), close).await;
    }
    async fn closed(&self) -> Result<BoxStream<'_, ()>, ashpd::Error> {
        Ok(match self {
            Self::Remote(s) => s.receive_closed().await?.boxed(),
            Self::Cast(s) => s.receive_closed().await?.boxed(),
        })
    }
}
struct Opened {
    fd: std::os::fd::OwnedFd,
    stream: Stream,
    input: Option<tokio::sync::mpsc::Sender<input::Command>>,
    wlroots_input: bool,
}

async fn actor(
    options: OpenOptions,
    ready: mpsc::SyncSender<Result<Opened, CaptureError>>,
    frames: Arc<FrameSlot>,
) {
    struct Finished(Arc<FrameSlot>);
    impl Drop for Finished {
        fn drop(&mut self) {
            self.0.portal_finished();
        }
    }
    let _finished = Finished(frames.clone());
    let result = async {
        let cast = Screencast::new().await.map_err(portal_error)?;
        let remote = if options.allow_input {
            match RemoteDesktop::new().await {
                Ok(remote) => {
                    let devices = remote.available_device_types().await.map_err(portal_error)?;
                    (devices.contains(DeviceType::Keyboard) && devices.contains(DeviceType::Pointer))
                        .then_some(remote)
                },
                // A screencast-only compositor can still offer viewing.
                Err(ashpd::Error::PortalNotFound(_)) => None,
                Err(e) => return Err(portal_error(e)),
            }
        } else { None };
        let session = match &remote {
            Some(remote) => PortalSession::Remote(remote.create_session().await.map_err(portal_error)?),
            None => PortalSession::Cast(cast.create_session().await.map_err(portal_error)?),
        };
        let setup = async {
            let (selected, control) = match (&session, &remote) {
                (PortalSession::Remote(session), Some(remote)) => {
                    remote.select_devices(session, DeviceType::Keyboard | DeviceType::Pointer, None, PersistMode::DoNot)
                        .await.map_err(portal_error)?.response().map_err(portal_error)?;
                    request::select_sources(&cast, session).await?;
                    let response = remote.start(session, None).await.map_err(portal_error)?.response().map_err(portal_error)?;
                    let streams = response.streams().ok_or_else(|| error("系统未授权共享屏幕"))?;
                    let control = response.devices().contains(DeviceType::Keyboard) && response.devices().contains(DeviceType::Pointer);
                    (streams.first().cloned(), control)
                }
                (PortalSession::Cast(session), _) => {
                    request::select_sources(&cast, session).await?;
                    let response = cast.start(session, None).await.map_err(portal_error)?.response().map_err(portal_error)?;
                    (response.streams().first().cloned(), false)
                }
                _ => unreachable!(),
            };
            let stream = selected.ok_or_else(|| error("系统未返回共享屏幕"))?;
            let fd = match &session {
                PortalSession::Remote(s) => cast.open_pipe_wire_remote(s).await,
                PortalSession::Cast(s) => cast.open_pipe_wire_remote(s).await,
            }.map_err(portal_error)?;
            Ok::<_, CaptureError>((stream, fd, control))
        };
        let setup = tokio::select! {
            value = setup => value,
            _ = cancelled(&options.cancelled) => Err(error("共享授权已取消")),
            _ = tokio::time::sleep(Duration::from_secs(120)) => Err(error("等待系统屏幕共享授权超时")),
        };
        let result = async { match setup {
            Ok((stream, fd, control)) => {
                let (tx, mut rx) = tokio::sync::mpsc::channel::<input::Command>(16);
                let mut closed = session.closed().await.map_err(portal_error)?;
                let node = stream.pipe_wire_node_id();
                let wlroots_input = matches!(&session, PortalSession::Cast(_));
                if ready.send(Ok(Opened { fd, stream, input: control.then_some(tx), wlroots_input })).is_ok() {
                    loop {
                        tokio::select! {
                            _ = cancelled(&options.cancelled) => break,
                            _ = closed.next() => { frames.fail("系统已结束屏幕共享"); break; },
                            command = rx.recv(), if control => {
                                let Some(command) = command else { break; };
                                let response = match (&session, &remote) {
                                    (PortalSession::Remote(s), Some(remote)) => {
                                        tokio::time::timeout(Duration::from_millis(250), input::dispatch(remote, s, node, command.event)).await
                                            .map_err(|_| "系统键鼠事件处理超时".to_string()).and_then(|v| v.map_err(|e| e.to_string()))
                                    }
                                    _ => Err("系统未授权键鼠控制".into()),
                                };
                                let _ = command.reply.send(response);
                            },
                        }
                    }
                }
                Ok(())
            }
            Err(e) => Err(e),
        }}.await;
        session.close().await;
        result
    }.await;
    if let Err(e) = result {
        let _ = ready.send(Err(e));
    }
}

pub struct PortalCapture {
    fps:Arc<AtomicU32>,
    frames: Arc<FrameSlot>,
    options: OpenOptions,
    worker: Option<JoinHandle<()>>,
    input: Option<tokio::sync::mpsc::Sender<input::Command>>,
    input_target: Option<uu_input::wayland::OutputTarget>,
    logical_size: (u32, u32),
    pixels: (u32, u32),
    started: Instant,
}
impl PortalCapture {
    pub fn open(source: &CaptureSource, options: OpenOptions) -> Result<Self, CaptureError> {
        if !matches!(source, CaptureSource::Primary | CaptureSource::Desktop) {
            return Err(error("Wayland 请通过系统屏幕选择器选择共享屏幕"));
        }
        let frames = Arc::new(FrameSlot::default());
        let (ready, rx) = mpsc::sync_channel(1);
        runtime()?.spawn(actor(options.clone(), ready, frames.clone()));
        let mut capture = Self {
            fps:Arc::new(AtomicU32::new(options.fps.clamp(1,240))),
            frames,
            options,
            worker: None,
            input: None,
            input_target: Default::default(),
            logical_size: (0, 0),
            pixels: (0, 0),
            started: Instant::now(),
        };
        let deadline = Instant::now() + Duration::from_secs(125);
        let opened = loop {
            if capture.options.cancelled.load(Ordering::Acquire) {
                return Err(error("共享授权已取消"));
            }
            if Instant::now() >= deadline {
                return Err(error("等待桌面门户超时，请检查系统屏幕共享服务"));
            }
            match rx.recv_timeout(Duration::from_millis(30)) {
                Ok(result) => break result?,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => return Err(error("桌面门户意外退出")),
            }
        };
        let (frames, stop) = (capture.frames.clone(), capture.options.cancelled.clone());
        let node = opened.stream.pipe_wire_node_id();
        let fps = capture.fps.clone();
        capture.worker = Some(
            std::thread::Builder::new()
                .name("portal-pipewire".into())
                .spawn(move || {
                    if let Err(e) = pipewire::run(opened.fd, node, fps, stop, frames.clone()) {
                        frames.fail(e);
                    }
                })
                .map_err(error)?,
        );
        let frame = capture.frames.frame(&capture.options.cancelled)?;
        capture.pixels = (frame.width, frame.height);
        capture.logical_size = opened
            .stream
            .size()
            .filter(|(w, h)| *w > 0 && *h > 0)
            .map(|(w, h)| (w as u32, h as u32))
            .unwrap_or(capture.pixels);
        capture.input = opened.input;
        capture.input_target = (opened.wlroots_input && capture.options.allow_input).then(|| uu_input::wayland::OutputTarget {
            name: opened.stream.mapping_id().or_else(|| opened.stream.id()).map(str::to_owned),
            position: opened.stream.position(),
            logical_size: opened.stream.size(),
        });
        Ok(capture)
    }
}
impl Capture for PortalCapture {
    fn set_frame_rate(&mut self,fps:u32)->Result<(),CaptureError> {
        if !(1..=240).contains(&fps){return Err(error("帧率超出采集接口范围"));}
        self.fps.store(fps,Ordering::Release);Ok(())
    }
    fn resolution(&self) -> (u32, u32) {
        self.pixels
    }
    fn desktop_rect(&self) -> Option<DesktopRect> {
        Some(DesktopRect {
            x: 0,
            y: 0,
            width: self.pixels.0,
            height: self.pixels.1,
        })
    }
    fn input_injector(&self) -> Option<Box<dyn uu_input::InputInjector>> {
        if let Some(tx) = self.input.clone() {
            return Some(Box::new(input::Injector::new(tx, self.pixels, self.logical_size)));
        }
        // ScreenCast-only wlroots portals do not provide RemoteDesktop. The
        // same user's compositor may explicitly expose virtual input instead.
        // Reach this path only after a permitted monitor stream produced a frame.
        if let Some(target) = &self.input_target {
            match uu_input::wayland::WlrootsInjector::connect(self.pixels, target.clone()) {
                Ok(input) => return Some(Box::new(input)),
                Err(error) => tracing::warn!(%error, "authorized Wayland capture has no input backend"),
            }
        }
        None
    }
    fn next_frame(&mut self) -> Result<RawFrame, CaptureError> {
        let mut frame = self.frames.frame(&self.options.cancelled)?;
        if (frame.width, frame.height) != self.pixels {
            return Err(error("共享屏幕尺寸已变化，请重新连接"));
        }
        frame.pts =
            uu_media::Timestamp(self.started.elapsed().as_micros().min(u64::MAX as u128) as u64);
        Ok(frame)
    }
}
impl Drop for PortalCapture {
    fn drop(&mut self) {
        self.options.cancelled.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.frames.wait_portal_closed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(value: u8) -> RawFrame {
        RawFrame::new_i420(2, 2, vec![value; 6], uu_media::Timestamp(0))
    }
    #[test]
    fn newest_frame_is_reused_on_static_content_but_revocation_hides_cached_pixels() {
        let slot = FrameSlot::default();
        let stop = AtomicBool::new(false);
        slot.publish(frame(1));
        slot.publish(frame(2));
        assert_eq!(slot.frame(&stop).unwrap().data[0], 2);
        assert_eq!(slot.frame(&stop).unwrap().data[0], 2);
        stop.store(true, Ordering::Release);
        assert!(slot.frame(&stop).is_err());
        stop.store(false, Ordering::Release);
        slot.fail("closed");
        assert!(slot.frame(&stop).is_err());
    }
    #[test]
    fn cancellation_wakes_capture_without_waiting_for_the_first_frame_timeout() {
        let slot = Arc::new(FrameSlot::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (done, rx) = mpsc::sync_channel(1);
        let flag = stop.clone();
        let task = std::thread::spawn(move || {
            let _ = done.send(slot.frame(&flag).is_err());
        });
        stop.store(true, Ordering::Release);
        assert!(rx.recv_timeout(Duration::from_millis(250)).unwrap());
        task.join().unwrap();
    }
}
