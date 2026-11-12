//! Process-wide ownership for a physical capture source.
//!
//! Windows DXGI permits one desktop duplication per output per process. Each
//! native session still owns its encoder, transport and quality policy, but
//! physical frames are acquired once and fanned out through bounded latest-frame
//! slots. Closing one viewer never invalidates another viewer's source.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering},
        Arc, Condvar, Mutex, OnceLock, Weak,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use uu_capture::{Capture, CaptureError, CaptureSource, DesktopMapping, DesktopRect, OpenOptions};
use uu_media::{encoder::InputDevice, hdr::Metadata as HdrMetadata, RawFrame};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum SourceKey {
    Monitor(String),
    Window(u64),
    Primary,
    Synthetic { width: u32, height: u32, fps: u32 },
}
impl SourceKey {
    fn from_source(source: &CaptureSource) -> Self {
        match source {
            CaptureSource::Monitor(name) | CaptureSource::Desktop { initial_monitor: Some(name) } => Self::Monitor(name.clone()),
            CaptureSource::Window(id) => Self::Window(*id),
            CaptureSource::Primary | CaptureSource::Desktop { initial_monitor: None } => Self::Primary,
            CaptureSource::Synthetic { width, height, fps } => Self::Synthetic { width: *width, height: *height, fps: *fps },
        }
    }
    fn needs_external_pacing(&self) -> bool {
        !matches!(self, Self::Synthetic { .. })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key { source: SourceKey }
impl Key { fn new(source: &CaptureSource) -> Self { Self { source: SourceKey::from_source(source) } } }

#[derive(Default)]
struct Metadata {
    resolution: (u32, u32),
    hdr: Option<HdrMetadata>,
    cursor: Option<uu_capture::cursor::Stream>,
    mapping: Option<DesktopMapping>,
    input_device: Option<InputDevice>,
    frame_info: Option<uu_capture::frame_info::FrameInfo>,
}
struct Slot {
    frame: Option<RawFrame>,
    error: Option<String>,
    closed: bool,
}
struct Subscriber {
    id: u64,
    fps: AtomicU32,
    slot: Mutex<Slot>,
    changed: Condvar,
}
impl Subscriber {
    fn new(id: u64, fps: u32) -> Arc<Self> {
        Arc::new(Self {
            id,
            fps: AtomicU32::new(fps.clamp(1, 240)),
            slot: Mutex::new(Slot { frame: None, error: None, closed: false }),
            changed: Condvar::new(),
        })
    }
    fn publish(&self, frame: &RawFrame) {
        let mut slot = self.slot.lock().unwrap();
        if slot.closed { return; }
        slot.error = None;
        slot.frame = Some(frame.clone());
        self.changed.notify_one();
    }
    fn suspend(&self, reason: &str) {
        let mut slot = self.slot.lock().unwrap();
        if slot.closed { return; }
        slot.frame = None;
        slot.error = Some(reason.into());
        self.changed.notify_all();
    }
    fn close(&self, error: Option<String>) {
        let mut slot = self.slot.lock().unwrap();
        if slot.closed { return; }
        slot.frame = None;
        slot.closed = true;
        slot.error = error;
        self.changed.notify_all();
    }
    fn take(&self, wait: Option<Duration>) -> Result<Option<RawFrame>, CaptureError> {
        let deadline = wait.map(|limit| Instant::now() + limit);
        let mut slot = self.slot.lock().unwrap();
        loop {
            if slot.closed {
                let message = slot.error.clone().unwrap_or_else(|| uu_i18n::text("共享采集源已经关闭").into());
                return Err(CaptureError::Backend(message));
            }
            if let Some(error) = &slot.error { return Err(CaptureError::Suspended(error.clone())); }
            if let Some(frame) = slot.frame.take() { return Ok(Some(frame)); }
            slot = match deadline {
                None => self.changed.wait(slot).unwrap(),
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline { return Ok(None); }
                    let (slot, timeout) = self.changed.wait_timeout(slot, deadline - now).unwrap();
                    if timeout.timed_out() && slot.frame.is_none() && !slot.closed { return Ok(None); }
                    slot
                }
            };
        }
    }
}

struct Owner {
    key: Key,
    source: CaptureSource,
    first_options: OpenOptions,
    stop: Arc<AtomicBool>,
    subscribers: Mutex<HashMap<u64, Arc<Subscriber>>>,
    next_subscriber: AtomicU64,
    metadata: Mutex<Metadata>,
    ready: Mutex<ReadyState>,
    ready_changed: Condvar,
    gpu: AtomicU8,
    gpu_vendor: AtomicU16,
    thread: Mutex<Option<JoinHandle<()>>>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadyPhase { Opening, Running, Failed, Stopped }
struct ReadyState { phase: ReadyPhase, error: Option<String> }
const GPU_UNSET: u8 = 0;
const GPU_OFF: u8 = 1;
const GPU_ON: u8 = 2;

impl Owner {
    fn new(key: Key, source: CaptureSource, options: OpenOptions) -> Arc<Self> {
        Arc::new(Self {
            key,
            source,
            first_options: options,
            stop: Arc::new(AtomicBool::new(false)),
            subscribers: Mutex::new(HashMap::new()),
            next_subscriber: AtomicU64::new(1),
            metadata: Mutex::new(Metadata::default()),
            ready: Mutex::new(ReadyState { phase: ReadyPhase::Opening, error: None }),
            ready_changed: Condvar::new(),
            gpu: AtomicU8::new(GPU_UNSET),
            gpu_vendor: AtomicU16::new(0),
            thread: Mutex::new(None),
        })
    }
    fn subscribe(self: &Arc<Self>, fps: u32) -> Arc<Subscriber> {
        let id = self.next_subscriber.fetch_add(1, Ordering::Relaxed);
        let subscriber = Subscriber::new(id, fps);
        self.subscribers.lock().unwrap().insert(id, subscriber.clone());
        subscriber
    }
    fn remove(&self, id: u64) -> bool {
        let mut subscribers = self.subscribers.lock().unwrap();
        subscribers.remove(&id);
        subscribers.is_empty()
    }
    fn target_fps(&self) -> u32 {
        self.subscribers.lock().unwrap().values().map(|subscriber| subscriber.fps.load(Ordering::Acquire)).max().unwrap_or(1).clamp(1, 240)
    }
    fn set_fps(&self, id: u64, fps: u32) {
        if let Some(subscriber) = self.subscribers.lock().unwrap().get(&id) {
            subscriber.fps.store(fps.clamp(1, 240), Ordering::Release);
        }
    }
    fn set_gpu(&self, enabled: bool, vendor: Option<u16>) {
        if enabled {
            self.gpu.store(GPU_ON, Ordering::Release);
            if let Some(vendor) = vendor { self.gpu_vendor.store(vendor, Ordering::Release); }
        } else if self.gpu.load(Ordering::Acquire) == GPU_UNSET {
            self.gpu.store(GPU_OFF, Ordering::Release);
        }
    }
    fn publish(&self, frame: &RawFrame) {
        let subscribers: Vec<_> = self.subscribers.lock().unwrap().values().cloned().collect();
        for subscriber in subscribers { subscriber.publish(frame); }
    }
    fn suspend(&self, reason: &str) {
        self.metadata.lock().unwrap().frame_info = None;
        let subscribers: Vec<_> = self.subscribers.lock().unwrap().values().cloned().collect();
        for subscriber in subscribers { subscriber.suspend(reason); }
    }
    fn close(&self, error: Option<String>) {
        let subscribers: Vec<_> = self.subscribers.lock().unwrap().values().cloned().collect();
        for subscriber in subscribers { subscriber.close(error.clone()); }
        let mut ready = self.ready.lock().unwrap();
        ready.phase = ReadyPhase::Stopped;
        if ready.error.is_none() { ready.error = error; }
        self.ready_changed.notify_all();
    }
    fn wait_ready(&self) -> Result<(), CaptureError> {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut ready = self.ready.lock().unwrap();
        loop {
            match ready.phase {
                ReadyPhase::Running => return Ok(()),
                ReadyPhase::Failed | ReadyPhase::Stopped => {
                    return Err(CaptureError::Backend(ready.error.clone().unwrap_or_else(|| uu_i18n::text("共享采集源启动失败").into())));
                }
                ReadyPhase::Opening => {}
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(CaptureError::Backend(uu_i18n::text("共享采集源启动超时").into()));
            }
            let (next, timeout) = self.ready_changed.wait_timeout(ready, deadline - now).unwrap();
            ready = next;
            if timeout.timed_out() && ready.phase == ReadyPhase::Opening {
                return Err(CaptureError::Backend(uu_i18n::text("共享采集源启动超时").into()));
            }
        }
    }
    fn start(self: &Arc<Self>) -> Result<(), CaptureError> {
        let owner = Arc::downgrade(self);
        let thread = std::thread::Builder::new().name("EclipseSharedCapture".into()).spawn(move || run_owner(owner))
            .map_err(|error| CaptureError::Backend(uu_i18n::format!("共享采集线程启动失败：{error}")))?;
        *self.thread.lock().unwrap() = Some(thread);
        Ok(())
    }
    fn stop_and_join(&self) {
        self.stop.store(true, Ordering::Release);
        self.ready_changed.notify_all();
        if let Some(thread) = self.thread.lock().unwrap().take() {
            let _ = thread.join();
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) { self.stop_and_join(); }
}

fn run_owner(owner: Weak<Owner>) {
    let Some(initial) = owner.upgrade() else { return; };
    let configured_fps = initial.first_options.fps.clamp(1, 240);
    let options = OpenOptions {
        hdr: initial.first_options.hdr,
        allow_input: false,
        fps: configured_fps,
        cancelled: initial.stop.clone(),
    };
    let capture = uu_capture::open_with_options(&initial.source, options);
    let capture = match capture {
        Ok(capture) => capture,
        Err(error) => {
            fail(&initial, error.to_string());
            drop(initial);
            return;
        }
    };
    drop(initial);
    run_capture(owner, capture, configured_fps);
}

fn run_capture(owner: Weak<Owner>, mut capture: Box<dyn Capture>, configured_fps: u32) {
    let Some(initial) = owner.upgrade() else { return; };
    if let Err(error) = capture.set_frame_rate(configured_fps) {
        fail(&initial, error.to_string());
        drop(initial);
        return;
    }
    {
        let mut metadata = initial.metadata.lock().unwrap();
        metadata.resolution = capture.resolution();
        metadata.hdr = capture.hdr_metadata();
        metadata.cursor = capture.cursor();
        metadata.mapping = capture.desktop_mapping();
        metadata.input_device = capture.encoder_input_device();
    }
    {
        let mut ready = initial.ready.lock().unwrap();
        ready.phase = ReadyPhase::Running;
        ready.error = None;
    }
    initial.ready_changed.notify_all();
    drop(initial);

    let mut configured_fps = configured_fps;
    let mut applied_gpu = GPU_UNSET;
    let mut applied_vendor = 0;
    let mut next = Instant::now();
    loop {
        let Some(owner) = owner.upgrade() else { break; };
        if owner.stop.load(Ordering::Acquire) { break; }
        let fps = owner.target_fps();
        if fps != configured_fps {
            if let Err(error) = capture.set_frame_rate(fps) { fail(&owner, error.to_string()); break; }
            configured_fps = fps;
            next = Instant::now();
        }
        let gpu = owner.gpu.load(Ordering::Acquire);
        let vendor = owner.gpu_vendor.load(Ordering::Acquire);
        if gpu != applied_gpu || (gpu == GPU_ON && vendor != applied_vendor) {
            capture.prefer_gpu(gpu == GPU_ON, (gpu == GPU_ON && vendor != 0).then_some(vendor));
            applied_gpu = gpu;
            applied_vendor = vendor;
        }
        let period = Duration::from_secs_f64(1.0 / f64::from(configured_fps));
        if owner.key.source.needs_external_pacing() {
            let now = Instant::now();
            if next > now { std::thread::sleep(next - now); }
            else if now.duration_since(next) >= period { next = now; }
        }
        let frame = match capture.next_frame() {
            Ok(frame) => frame,
            Err(CaptureError::Suspended(error)) => {
                // The source owns desktop recovery. Retain it across sign-in,
                // UAC and hotplug waits, but retire every subscriber's old image.
                owner.suspend(&error);
                std::thread::sleep(Duration::from_millis(25));
                next = Instant::now();
                continue;
            }
            Err(error) => { fail(&owner, error.to_string()); break; }
        };
        {
            let mut metadata = owner.metadata.lock().unwrap();
            metadata.resolution = capture.resolution();
            metadata.hdr = capture.hdr_metadata();
            metadata.frame_info = capture.frame_info();
        }
        owner.publish(&frame);
        next += period;
    }
    if let Some(owner) = owner.upgrade() {
        owner.close(None);
    }
}

fn fail(owner: &Arc<Owner>, error: String) {
    {
        let mut ready = owner.ready.lock().unwrap();
        ready.phase = ReadyPhase::Failed;
        ready.error = Some(error.clone());
    }
    owner.ready_changed.notify_all();
    owner.close(Some(error));
}

pub struct SharedCapture {
    owner: Arc<Owner>,
    subscriber: Arc<Subscriber>,
}
impl SharedCapture {
    fn acquire(source: &CaptureSource, options: OpenOptions) -> Result<Box<dyn Capture>, CaptureError> {
        let key = Key::new(source);
        let (owner, subscriber) = {
            let mut pool = pool().lock().unwrap();
            let owner = if let Some(owner) = pool.get(&key) {
                owner.clone()
            } else {
                let owner = Owner::new(key.clone(), source.clone(), OpenOptions {
                    hdr: options.hdr,
                    allow_input: options.allow_input,
                    fps: options.fps,
                    cancelled: options.cancelled.clone(),
                });
                pool.insert(key.clone(), owner.clone());
                owner
            };
            let subscriber = owner.subscribe(options.fps);
            if owner.thread.lock().unwrap().is_none() {
                if let Err(error) = owner.start() {
                    pool.remove(&key);
                    return Err(error);
                }
            }
            (owner, subscriber)
        };
        if let Err(error) = owner.wait_ready() {
            let mut pool = pool().lock().unwrap();
            let empty = owner.remove(subscriber.id);
            if empty { pool.remove(&key); }
            drop(pool);
            if empty { owner.stop_and_join(); }
            return Err(error);
        }
        Ok(Box::new(Self { owner, subscriber }))
    }
    fn metadata(&self) -> std::sync::MutexGuard<'_, Metadata> { self.owner.metadata.lock().unwrap() }
}
impl Capture for SharedCapture {
    fn cursor(&self) -> Option<uu_capture::cursor::Stream> { self.metadata().cursor.clone() }
    fn enable_overlap(&mut self) -> Result<(), CaptureError> { Ok(()) }
    fn wait_updated_frame(&mut self, wait: Duration) -> Result<Option<RawFrame>, CaptureError> {
        self.subscriber.take(Some(wait))
    }
    fn frame_info(&self) -> Option<uu_capture::frame_info::FrameInfo> { self.metadata().frame_info }
    fn hdr_metadata(&self) -> Option<HdrMetadata> { self.metadata().hdr }
    fn set_frame_rate(&mut self, fps: u32) -> Result<(), CaptureError> {
        if !(1..=240).contains(&fps) { return Err(CaptureError::Backend(uu_i18n::text("共享采集帧率无效").into())); }
        self.owner.set_fps(self.subscriber.id, fps);
        Ok(())
    }
    fn prefer_gpu(&mut self, enabled: bool, vendor: Option<u16>) { self.owner.set_gpu(enabled, vendor); }
    fn encoder_input_device(&self) -> Option<InputDevice> { self.metadata().input_device.clone() }
    fn resolution(&self) -> (u32, u32) { self.metadata().resolution }
    fn desktop_rect(&self) -> Option<DesktopRect> { self.desktop_mapping().and_then(|mapping| mapping.get()) }
    fn desktop_mapping(&self) -> Option<DesktopMapping> { self.metadata().mapping.clone() }
    fn next_frame(&mut self) -> Result<RawFrame, CaptureError> {
        self.subscriber.take(None)?.ok_or_else(|| CaptureError::Suspended(uu_i18n::text("共享采集源暂无新画面").into()))
    }
}
impl Drop for SharedCapture {
    fn drop(&mut self) {
        let mut pool = pool().lock().unwrap_or_else(|error| error.into_inner());
        let empty = self.owner.remove(self.subscriber.id);
        if empty {
            pool.remove(&self.owner.key);
            self.owner.stop_and_join();
        }
    }
}

fn pool() -> &'static Mutex<HashMap<Key, Arc<Owner>>> {
    static POOL: OnceLock<Mutex<HashMap<Key, Arc<Owner>>>> = OnceLock::new();
    POOL.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Open a physical source through the process-wide owner on Windows. Other
/// platforms keep their existing per-session backends and authorization model.
#[cfg(windows)]
pub fn open_with_options(source: &CaptureSource, options: OpenOptions) -> Result<Box<dyn Capture>, CaptureError> {
    if !matches!(source, CaptureSource::Synthetic { .. }) {
        return SharedCapture::acquire(source, options);
    }
    uu_capture::open_with_options(source, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn options(fps: u32) -> OpenOptions {
        OpenOptions { fps, ..Default::default() }
    }

    #[test]
    fn two_viewers_share_one_source_and_closing_one_keeps_the_other() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let source = CaptureSource::Synthetic { width: 96, height: 64, fps: 120 };
        let mut first = SharedCapture::acquire(&source, options(120)).unwrap();
        let mut second = SharedCapture::acquire(&source, OpenOptions { hdr: true, ..options(120) }).unwrap();
        assert_eq!(pool().lock().unwrap().len(), 1, "same source must have one physical owner");
        let a = first.next_frame().unwrap();
        let b = second.next_frame().unwrap();
        assert_eq!((a.width, a.height), (b.width, b.height));
        drop(first);
        assert_eq!(pool().lock().unwrap().len(), 1, "closing one viewer must not release the shared source");
        let next = second.next_frame().unwrap();
        assert_ne!(next.pts, b.pts, "remaining viewer must continue receiving frames");
        drop(second);
        assert!(pool().lock().unwrap().is_empty(), "last viewer must release the shared owner");
    }

    #[test]
    fn overlapping_sources_stay_independent() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let first = CaptureSource::Synthetic { width: 64, height: 48, fps: 120 };
        let second = CaptureSource::Synthetic { width: 80, height: 48, fps: 120 };
        let mut a = SharedCapture::acquire(&first, options(120)).unwrap();
        let mut b = SharedCapture::acquire(&second, options(120)).unwrap();
        assert_eq!(pool().lock().unwrap().len(), 2);
        assert_eq!(a.resolution(), (64, 48));
        assert_eq!(b.resolution(), (80, 48));
        let _ = a.next_frame().unwrap();
        let _ = b.next_frame().unwrap();
        drop(a);
        drop(b);
        assert!(pool().lock().unwrap().is_empty());
    }

    #[test]
    fn equivalent_primary_and_monitor_spellings_share_the_same_identity() {
        assert_eq!(Key::new(&CaptureSource::Primary), Key::new(&CaptureSource::Desktop { initial_monitor: None }));
        assert_eq!(Key::new(&CaptureSource::Monitor("display-a".into())), Key::new(&CaptureSource::Desktop { initial_monitor: Some("display-a".into()) }));
        assert_ne!(Key::new(&CaptureSource::Monitor("display-a".into())), Key::new(&CaptureSource::Monitor("display-b".into())));
    }

    #[test]
    fn paused_source_retires_queued_images_and_recovers_for_both_viewers() {
        struct Source(std::sync::mpsc::Receiver<Result<RawFrame, CaptureError>>);
        impl Capture for Source {
            fn resolution(&self) -> (u32, u32) { (2, 2) }
            fn next_frame(&mut self) -> Result<RawFrame, CaptureError> {
                self.0.recv_timeout(Duration::from_millis(25))
                    .unwrap_or_else(|_| Err(CaptureError::Suspended("waiting for fixture desktop".into())))
            }
        }
        let source = CaptureSource::Primary;
        let owner = Owner::new(Key::new(&source), source, options(60));
        let first = owner.subscribe(60);
        let second = owner.subscribe(60);
        let (send, receive) = std::sync::mpsc::channel();
        let weak = Arc::downgrade(&owner);
        *owner.thread.lock().unwrap() = Some(std::thread::spawn(move || run_capture(weak, Box::new(Source(receive)), 60)));
        owner.wait_ready().unwrap();
        let picture = |pts| RawFrame::new_i420(2, 2, vec![0; 6], uu_media::Timestamp(pts));
        send.send(Ok(picture(1))).unwrap();
        assert_eq!(first.take(Some(Duration::from_secs(1))).unwrap().unwrap().pts.0, 1);
        // The second viewer deliberately leaves this old picture queued.
        send.send(Err(CaptureError::Suspended("desktop switch".into()))).unwrap();
        assert!(matches!(first.take(Some(Duration::from_secs(1))), Err(CaptureError::Suspended(_))));
        assert!(matches!(second.take(Some(Duration::from_secs(1))), Err(CaptureError::Suspended(_))));
        send.send(Ok(picture(2))).unwrap();
        for subscriber in [&first, &second] {
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                match subscriber.take(Some(Duration::from_millis(20))) {
                    Ok(Some(frame)) => { assert_eq!(frame.pts.0, 2); break; }
                    Err(CaptureError::Suspended(_)) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(1)),
                    other => panic!("source did not recover: {other:?}"),
                }
            }
        }
        send.send(Err(CaptureError::Backend("device removed".into()))).unwrap();
        assert!(matches!(first.take(Some(Duration::from_secs(1))), Err(CaptureError::Backend(reason)) if reason.contains("device removed")));
        owner.stop_and_join();
    }
}
