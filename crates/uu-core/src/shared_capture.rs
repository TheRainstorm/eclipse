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
use uu_capture::{Capture, CaptureError, CaptureSource, DesktopMapping, DesktopRect, FrameTiming, OpenOptions};
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
}
#[derive(Clone, Debug)]
struct PublishedFrame {
    frame: RawFrame,
    source: Option<uu_capture::frame_info::FrameInfo>,
    timing: FrameTiming,
}
#[derive(Default)]
struct DeliveryCadence {
    source_fps: u32,
    requested_fps: u32,
    credit: u32,
}
impl DeliveryCadence {
    fn accepts(&mut self, source_fps: u32, requested_fps: u32) -> bool {
        if self.source_fps == 0 || self.requested_fps != requested_fps {
            // A new subscription, explicit rate change, or resumed desktop
            // delivers the first available image without an extra period.
            self.source_fps = source_fps;
            self.requested_fps = requested_fps;
            self.credit = 0;
            return true;
        }
        if self.source_fps != source_fps {
            self.credit = self.credit * source_fps / self.source_fps;
            self.source_fps = source_fps;
        }
        // Count owner opportunities rather than comparing independent wall
        // clocks. Equal-rate readers receive every tick despite wake jitter;
        // fractional rates distribute deliveries without catch-up bursts.
        self.credit += requested_fps.min(source_fps);
        if self.credit >= source_fps {
            self.credit -= source_fps;
            true
        } else {
            false
        }
    }
}
struct Slot {
    frame: Option<PublishedFrame>,
    error: Option<String>,
    closed: bool,
    cadence: DeliveryCadence,
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
            slot: Mutex::new(Slot { frame: None, error: None, closed: false, cadence: DeliveryCadence::default() }),
            changed: Condvar::new(),
        })
    }
    fn publish(&self, frame: &PublishedFrame, source_fps: u32) {
        let mut slot = self.slot.lock().unwrap();
        if slot.closed { return; }
        if !slot.cadence.accepts(source_fps, self.fps.load(Ordering::Acquire)) { return; }
        slot.error = None;
        slot.frame = Some(frame.clone());
        self.changed.notify_one();
    }
    fn set_fps(&self, fps: u32) {
        let mut slot = self.slot.lock().unwrap();
        let fps = fps.clamp(1, 240);
        if self.fps.swap(fps, Ordering::AcqRel) != fps {
            // A quality change must not consume a delivery admitted under the
            // previous rate; the next owner tick starts the new cadence.
            slot.frame = None;
            slot.cadence = DeliveryCadence::default();
        }
    }
    fn suspend(&self, reason: &str) {
        let mut slot = self.slot.lock().unwrap();
        if slot.closed { return; }
        slot.frame = None;
        slot.error = Some(reason.into());
        slot.cadence = DeliveryCadence::default();
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
    fn take(&self, wait: Option<Duration>) -> Result<Option<PublishedFrame>, CaptureError> {
        let deadline = wait.map(|limit| Instant::now() + limit);
        let mut slot = self.slot.lock().unwrap();
        loop {
            if slot.closed {
                let message = slot.error.clone().unwrap_or_else(|| uu_i18n::text("共享采集源已经关闭").into());
                return Err(CaptureError::Backend(message));
            }
            // A suspension is a source notification, not a ready image that
            // can be polled repeatedly. The owner retries and publishes another
            // notification or fresh image; cadence-driven readers wait for it.
            if let Some(error) = slot.error.take() { return Err(CaptureError::Suspended(error)); }
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
            subscriber.set_fps(fps);
        }
    }
    fn set_gpu(&self, enabled: bool, vendor: Option<u16>) {
        let current = self.gpu.load(Ordering::Acquire);
        if (!enabled && current != GPU_UNSET)
            || (enabled && current == GPU_ON && vendor.is_none_or(|value| value == self.gpu_vendor.load(Ordering::Acquire))) {
            return;
        }
        let _ready = self.ready.lock().unwrap();
        if enabled {
            if let Some(vendor) = vendor { self.gpu_vendor.store(vendor, Ordering::Release); }
            self.gpu.store(GPU_ON, Ordering::Release);
        } else if self.gpu.load(Ordering::Acquire) == GPU_UNSET {
            self.gpu.store(GPU_OFF, Ordering::Release);
        }
        self.ready_changed.notify_all();
    }
    fn default_input_format(&self) {
        // Callers that do not select an encoder still receive CPU frames. The
        // encoder startup path chooses its format before requesting a frame.
        self.set_gpu(false, None);
    }
    fn wait_input_format(&self) {
        let mut ready = self.ready.lock().unwrap();
        while self.gpu.load(Ordering::Acquire) == GPU_UNSET && !self.stop.load(Ordering::Acquire) {
            ready = self.ready_changed.wait(ready).unwrap();
        }
    }
    fn publish(&self, frame: &RawFrame, source: Option<uu_capture::frame_info::FrameInfo>, timing: FrameTiming, source_fps: u32) {
        let frame = PublishedFrame { frame: frame.clone(), source, timing };
        let subscribers: Vec<_> = self.subscribers.lock().unwrap().values().cloned().collect();
        for subscriber in subscribers { subscriber.publish(&frame, source_fps); }
    }
    fn suspend(&self, reason: &str) {
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
        {
            let _ready = self.ready.lock().unwrap();
            self.stop.store(true, Ordering::Release);
            self.ready_changed.notify_all();
        }
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

/// One owner opportunity: a known duplicate may wait for a fresh source image
/// within this absolute cycle. Never retain the discarded pool lease across a
/// retry or hold the DXGI device lock while yielding to the encoder.
fn capture_in_cycle(capture: &mut dyn Capture, deadline: Instant, stop: &AtomicBool) -> Result<Option<PublishedFrame>, CaptureError> {
    loop {
        if stop.load(Ordering::Acquire) { return Ok(None); }
        let started = Instant::now();
        let acquired = capture.next_frame();
        if stop.load(Ordering::Acquire) { return Ok(None); }
        let frame = acquired?;
        let timing = capture.frame_timing().unwrap_or(FrameTiming { started, ready: Instant::now() });
        let source = capture.frame_info();
        if stop.load(Ordering::Acquire) { return Ok(None); }
        let wait = deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(1));
        if !source.is_some_and(|info| info.duplicate) || wait.is_zero() {
            return Ok(Some(PublishedFrame { frame, source, timing }));
        }
        drop(frame);
        std::thread::sleep(wait);
    }
}

/// Establish source phase once per generation, or after the caller explicitly
/// resets it on recovery/rate changes. Old static timestamps skip elapsed
/// periods instead of creating catch-up opportunities in the past.
fn source_phase_deadline(source: Option<uu_capture::frame_info::FrameInfo>, anchored: &mut Option<u64>, now: Instant, period: Duration, fallback: Instant) -> Instant {
    let Some(info) = source else { return fallback; };
    if *anchored == Some(info.generation) || info.present.is_none() || period.is_zero() { return fallback; }
    let Some(age_us) = info.present_age_us else { return fallback; };
    let age = Duration::from_micros(age_us);
    // The owner's validated 1..240 FPS bounds the remainder to at most 1s.
    let phase = Duration::from_nanos((age.as_nanos() % period.as_nanos()) as u64);
    *anchored = Some(info.generation);
    now + (period - phase)
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
    // Capturing a CPU picture before the encoder selects GPU input causes the
    // later format switch to clear the only image of an idle sign-in desktop.
    // Publish device metadata first, then acquire the first image in the chosen
    // format; an ordinary consumer chooses CPU on its first read.
    initial.wait_input_format();
    drop(initial);

    let mut configured_fps = configured_fps;
    let mut applied_gpu = GPU_UNSET;
    let mut applied_vendor = 0;
    let mut next = Instant::now();
    let mut anchored_generation = None;
    loop {
        let Some(owner) = owner.upgrade() else { break; };
        if owner.stop.load(Ordering::Acquire) { break; }
        let fps = owner.target_fps();
        if fps != configured_fps {
            if let Err(error) = capture.set_frame_rate(fps) { fail(&owner, error.to_string()); break; }
            configured_fps = fps;
            next = Instant::now();
            anchored_generation = None;
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
        let deadline = next + period;
        let captured = match capture_in_cycle(capture.as_mut(), deadline, &owner.stop) {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(CaptureError::Suspended(error)) => {
                // The source owns desktop recovery. Retain it across sign-in,
                // UAC and hotplug waits, but retire every subscriber's old image.
                owner.suspend(&error);
                std::thread::sleep(Duration::from_millis(25));
                next = Instant::now();
                anchored_generation = None;
                continue;
            }
            Err(error) => { fail(&owner, error.to_string()); break; }
        };
        let following = source_phase_deadline(captured.source, &mut anchored_generation, Instant::now(), period, deadline);
        {
            let mut metadata = owner.metadata.lock().unwrap();
            metadata.resolution = capture.resolution();
            metadata.hdr = capture.hdr_metadata();
        }
        owner.publish(&captured.frame, captured.source, captured.timing, configured_fps);
        next = following;
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
    last_frame_info: Option<uu_capture::frame_info::FrameInfo>,
    last_frame_timing: Option<FrameTiming>,
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
        Ok(Box::new(Self { owner, subscriber, last_frame_info: None, last_frame_timing: None }))
    }
    fn metadata(&self) -> std::sync::MutexGuard<'_, Metadata> { self.owner.metadata.lock().unwrap() }
    fn take(&mut self, wait: Option<Duration>) -> Result<Option<RawFrame>, CaptureError> {
        self.last_frame_info = None;
        self.last_frame_timing = None;
        self.subscriber.take(wait).map(|published| published.map(|published| {
            // The shared owner may already have captured the next image. Keep
            // source identity bound to this subscriber's actual delivery.
            self.last_frame_info = published.source;
            self.last_frame_timing = Some(published.timing);
            published.frame
        }))
    }
}
impl Capture for SharedCapture {
    fn cursor(&self) -> Option<uu_capture::cursor::Stream> { self.metadata().cursor.clone() }
    fn owns_frame_cadence(&self) -> bool { true }
    fn enable_overlap(&mut self) -> Result<(), CaptureError> { Ok(()) }
    fn wait_updated_frame(&mut self, wait: Duration) -> Result<Option<RawFrame>, CaptureError> {
        self.owner.default_input_format();
        self.take(Some(wait))
    }
    fn frame_info(&self) -> Option<uu_capture::frame_info::FrameInfo> { self.last_frame_info }
    fn frame_timing(&self) -> Option<FrameTiming> { self.last_frame_timing }
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
        self.owner.default_input_format();
        self.take(None)?.ok_or_else(|| CaptureError::Suspended(uu_i18n::text("共享采集源暂无新画面").into()))
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

    fn delivery_fixture() -> (Arc<Owner>, SharedCapture, SharedCapture) {
        let source = CaptureSource::Monitor("owned-delivery-metadata-fixture".into());
        let owner = Owner::new(Key::new(&source), source, options(60));
        let first = SharedCapture { subscriber: owner.subscribe(60), owner: owner.clone(), last_frame_info: None, last_frame_timing: None };
        let second = SharedCapture { subscriber: owner.subscribe(60), owner: owner.clone(), last_frame_info: None, last_frame_timing: None };
        (owner, first, second)
    }

    fn source_info(frame_id: u64, duplicate: bool) -> uu_capture::frame_info::FrameInfo {
        uu_capture::frame_info::FrameInfo {
            generation: 7,
            frame_id,
            duplicate,
            accumulated_frames: if duplicate { 0 } else { 1 },
            present: Some(uu_capture::frame_info::SourceTime { qpc_ticks: frame_id * 100, frequency_hz: 1_000_000 }),
            present_age_us: Some(50),
        }
    }

    struct RetryPixels {
        bytes: [u8; 6],
        live: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl AsRef<[u8]> for RetryPixels { fn as_ref(&self) -> &[u8] { &self.bytes } }
    impl Drop for RetryPixels { fn drop(&mut self) { self.live.fetch_sub(1, Ordering::AcqRel); } }
    struct CycleCapture {
        steps: std::collections::VecDeque<Option<uu_capture::frame_info::FrameInfo>>,
        source: Option<uu_capture::frame_info::FrameInfo>,
        live: Arc<std::sync::atomic::AtomicUsize>,
        stop: Arc<AtomicBool>,
        calls: usize,
        cancel_at: Option<usize>,
        suspend_at: Option<usize>,
    }
    impl CycleCapture {
        fn new(steps: impl IntoIterator<Item=Option<uu_capture::frame_info::FrameInfo>>) -> Self {
            Self { steps: steps.into_iter().collect(), source: None, live: Arc::default(), stop: Arc::default(), calls: 0, cancel_at: None, suspend_at: None }
        }
    }
    impl Capture for CycleCapture {
        fn resolution(&self) -> (u32, u32) { (2, 2) }
        fn frame_info(&self) -> Option<uu_capture::frame_info::FrameInfo> { self.source }
        fn next_frame(&mut self) -> Result<RawFrame, CaptureError> {
            assert_eq!(self.live.load(Ordering::Acquire), 0, "a retry retained the discarded frame lease");
            self.calls += 1;
            if self.suspend_at == Some(self.calls) { return Err(CaptureError::Suspended("owned desktop unavailable".into())); }
            self.source = self.steps.pop_front().unwrap_or(self.source);
            let mut frame = RawFrame::new_i420(2, 2, vec![0; 6], uu_media::Timestamp(self.calls as u64));
            self.live.fetch_add(1, Ordering::AcqRel);
            frame.data = bytes::Bytes::from_owner(RetryPixels { bytes: [0; 6], live: self.live.clone() });
            if self.cancel_at == Some(self.calls) { self.stop.store(true, Ordering::Release); }
            Ok(frame)
        }
    }

    #[test]
    fn cycle_retry_releases_old_pixels_and_returns_the_first_fresh_source() {
        let mut capture = CycleCapture::new([Some(source_info(7, true)), Some(source_info(8, false))]);
        let stop = capture.stop.clone();
        let image = capture_in_cycle(&mut capture, Instant::now() + Duration::from_secs(1), &stop).unwrap().unwrap();
        assert_eq!(capture.calls, 2);
        assert_eq!(image.source.unwrap().frame_id, 8);
        assert!(!image.source.unwrap().duplicate);
        assert_eq!(capture.live.load(Ordering::Acquire), 1);
        drop(image);
        assert_eq!(capture.live.load(Ordering::Acquire), 0);
    }

    #[test]
    fn unknown_source_and_expired_deadline_return_one_image_without_retrying() {
        for (source, deadline) in [(None, Instant::now() + Duration::from_secs(1)), (Some(source_info(7, true)), Instant::now())] {
            let mut capture = CycleCapture::new([source]);
            let stop = capture.stop.clone();
            let image = capture_in_cycle(&mut capture, deadline, &stop).unwrap().unwrap();
            assert_eq!(capture.calls, 1);
            assert_eq!(image.source, source);
        }
    }

    #[test]
    fn static_cycle_returns_one_repeat_at_its_original_deadline() {
        let mut capture = CycleCapture::new([Some(source_info(7, true))]);
        let stop = capture.stop.clone();
        let deadline = Instant::now() + Duration::from_millis(3);
        let image = capture_in_cycle(&mut capture, deadline, &stop).unwrap().unwrap();
        assert!(Instant::now() >= deadline);
        assert!(capture.calls <= 5, "the retry loop did not yield within the shared deadline");
        assert_eq!(image.source, Some(source_info(7, true)));
    }

    #[test]
    fn cancellation_before_or_during_acquisition_never_publishes_a_frame() {
        for cancel_before in [true, false] {
            let mut capture = CycleCapture::new([Some(source_info(7, true))]);
            capture.stop.store(cancel_before, Ordering::Release);
            capture.cancel_at = Some(1);
            let stop = capture.stop.clone();
            assert!(capture_in_cycle(&mut capture, Instant::now() + Duration::from_secs(1), &stop).unwrap().is_none());
            assert_eq!(capture.calls, usize::from(!cancel_before));
            assert_eq!(capture.live.load(Ordering::Acquire), 0);
        }
    }

    #[test]
    fn source_phase_is_initialized_once_and_resets_only_for_explicit_transitions() {
        let now = Instant::now();
        let period = Duration::from_millis(10);
        let original = now + period;
        let mut anchored = None;
        let mut info = source_info(7, false);
        info.present_age_us = Some(3000);
        assert_eq!(source_phase_deadline(Some(info), &mut anchored, now, period, original), now + Duration::from_millis(7));
        info.present_age_us = Some(1000);
        assert_eq!(source_phase_deadline(Some(info), &mut anchored, now, period, original), original, "ordinary images cannot restart the clock");
        info.generation += 1;
        assert_eq!(source_phase_deadline(Some(info), &mut anchored, now, period, original), now + Duration::from_millis(9));
        anchored = None; // The owner explicitly resets this on a frame-rate change.
        assert_eq!(source_phase_deadline(Some(info), &mut anchored, now, Duration::from_millis(20), original), now + Duration::from_millis(19));
        anchored = None;
        info.present_age_us = Some(25_000);
        assert_eq!(source_phase_deadline(Some(info), &mut anchored, now, period, original), now + Duration::from_millis(5), "old static source time cannot create catch-up debt");
        anchored = None;
        info.present_age_us = None;
        assert_eq!(source_phase_deadline(Some(info), &mut anchored, now, period, original), original);
        assert_eq!(anchored, None);
        assert_eq!(source_phase_deadline(None, &mut anchored, now, period, original), original);
    }

    #[test]
    fn suspended_capture_discards_the_cycle_and_recovery_can_establish_a_new_phase() {
        let mut info = source_info(7, false);
        info.generation = 2;
        info.present_age_us = Some(2000);
        let mut capture = CycleCapture::new([Some(info)]);
        capture.suspend_at = Some(1);
        let stop = capture.stop.clone();
        let deadline = Instant::now() + Duration::from_secs(1);
        assert!(matches!(capture_in_cycle(&mut capture, deadline, &stop), Err(CaptureError::Suspended(_))));
        assert_eq!(capture.live.load(Ordering::Acquire), 0);
        capture.suspend_at = None;
        let image = capture_in_cycle(&mut capture, deadline, &stop).unwrap().unwrap();
        let now = Instant::now();
        let mut anchored = None; // Same reset performed by run_capture on Suspended.
        assert_eq!(source_phase_deadline(image.source, &mut anchored, now, Duration::from_millis(10), deadline), now + Duration::from_millis(8));
        assert_eq!(anchored, Some(2));
    }

    fn publish_tick(owner: &Owner, id: u64, source_fps: u32) {
        let started = Instant::now();
        let frame = RawFrame::new_i420(2, 2, vec![0; 6], uu_media::Timestamp(id));
        owner.publish(&frame, Some(source_info(id, false)), FrameTiming { started, ready: Instant::now() }, source_fps);
    }

    #[test]
    fn subscriber_rates_follow_owner_ticks_without_accumulating_catch_up_work() {
        for (source_fps, requested_fps) in [(60, 60), (60, 30), (60, 50), (120, 24), (165, 60)] {
            let mut cadence = DeliveryCadence::default();
            let mut delivered = 0;
            for tick in 0..source_fps * 10 {
                delivered += u32::from(cadence.accepts(source_fps, requested_fps));
                let error = i64::from(delivered * source_fps) - i64::from((tick + 1) * requested_fps);
                assert!(error.unsigned_abs() < u64::from(source_fps), "rate drifted by a full delivery");
            }
            assert_eq!(delivered, requested_fps * 10);
        }
    }

    #[test]
    fn mixed_subscriber_rates_survive_quality_changes_and_removal_of_the_fastest() {
        let (owner, mut first, mut second) = delivery_fixture();
        assert!(first.owns_frame_cadence());
        second.set_frame_rate(30).unwrap();
        assert_eq!(owner.target_fps(), 60);
        let mut slow = 0;
        for id in 1..=60 {
            publish_tick(&owner, id, 60);
            assert_eq!(first.wait_updated_frame(Duration::ZERO).unwrap().unwrap().pts.0, id);
            slow += u32::from(second.wait_updated_frame(Duration::ZERO).unwrap().is_some());
        }
        assert_eq!(slow, 30);

        first.set_frame_rate(30).unwrap();
        assert_eq!(owner.target_fps(), 30);
        for id in 61..=90 {
            publish_tick(&owner, id, 30);
            for capture in [&mut first, &mut second] {
                assert_eq!(capture.wait_updated_frame(Duration::ZERO).unwrap().unwrap().pts.0, id);
            }
        }

        second.set_frame_rate(60).unwrap();
        let mut slow = 0;
        for id in 91..=150 {
            publish_tick(&owner, id, 60);
            slow += u32::from(first.wait_updated_frame(Duration::ZERO).unwrap().is_some());
            assert_eq!(second.wait_updated_frame(Duration::ZERO).unwrap().unwrap().pts.0, id);
        }
        assert_eq!(slow, 30);
        drop(second);
        assert_eq!(owner.target_fps(), 30);
        publish_tick(&owner, 151, 30);
        assert_eq!(first.next_frame().unwrap().pts.0, 151);
    }

    #[test]
    fn slow_consumer_receives_only_the_latest_eligible_delivery_once() {
        let (owner, mut first, mut second) = delivery_fixture();
        second.set_frame_rate(30).unwrap();
        for id in 1..=7 {
            publish_tick(&owner, id, 60);
            assert_eq!(first.next_frame().unwrap().pts.0, id);
        }
        assert_eq!(second.next_frame().unwrap().pts.0, 7);
        assert_eq!(second.frame_info().unwrap().frame_id, 7);
        assert!(second.wait_updated_frame(Duration::ZERO).unwrap().is_none(), "no stale queue may be replayed");
        publish_tick(&owner, 8, 60);
        assert!(second.wait_updated_frame(Duration::ZERO).unwrap().is_none());
        publish_tick(&owner, 9, 60);
        assert_eq!(second.next_frame().unwrap().pts.0, 9);
    }

    #[test]
    fn rate_change_discards_pending_old_rate_and_resume_delivers_the_first_image() {
        let (owner, mut first, mut second) = delivery_fixture();
        publish_tick(&owner, 1, 60);
        second.set_frame_rate(30).unwrap();
        assert!(second.wait_updated_frame(Duration::ZERO).unwrap().is_none());
        publish_tick(&owner, 2, 60);
        assert_eq!(second.next_frame().unwrap().pts.0, 2);
        publish_tick(&owner, 3, 60);
        assert!(second.wait_updated_frame(Duration::ZERO).unwrap().is_none());
        owner.suspend("owned desktop transition");
        for capture in [&mut first, &mut second] {
            assert!(matches!(capture.next_frame(), Err(CaptureError::Suspended(_))));
            assert_eq!(capture.frame_timing(), None);
            assert!(capture.wait_updated_frame(Duration::ZERO).unwrap().is_none(), "a consumed suspension cannot become a busy-loop delivery");
        }
        publish_tick(&owner, 4, 60);
        for capture in [&mut first, &mut second] {
            assert_eq!(capture.wait_updated_frame(Duration::ZERO).unwrap().unwrap().pts.0, 4);
            assert_eq!(capture.frame_info().unwrap().frame_id, 4);
        }
    }

    #[test]
    fn closing_a_rate_limited_subscriber_wakes_its_waiter_without_another_tick() {
        let (owner, _first, _second) = delivery_fixture();
        let waiting = owner.subscribe(30);
        let (send, receive) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || { let _ = send.send(waiting.take(None)); });
        owner.close(Some("owned source shutdown".into()));
        assert!(matches!(receive.recv_timeout(Duration::from_secs(1)).unwrap(), Err(CaptureError::Backend(_))));
        thread.join().unwrap();
        publish_tick(&owner, 1, 60);
        assert!(_first.subscriber.take(Some(Duration::ZERO)).is_err(), "publishing cannot reopen a closed slot");
    }

    #[test]
    fn each_subscriber_keeps_the_source_identity_of_its_delivered_image() {
        let (owner, mut first, mut second) = delivery_fixture();
        let picture = |pts| RawFrame::new_i420(2, 2, vec![0; 6], uu_media::Timestamp(pts));
        let old = source_info(11, false);
        let new = source_info(12, false);
        let base = Instant::now() - Duration::from_millis(100);
        let old_timing = FrameTiming { started: base, ready: base + Duration::from_millis(1) };
        let new_timing = FrameTiming { started: base + Duration::from_millis(16), ready: base + Duration::from_millis(17) };
        owner.publish(&picture(1000), Some(old), old_timing, 60);
        assert_eq!(first.next_frame().unwrap().pts.0, 1000);
        assert_eq!(first.frame_info(), Some(old));
        assert_eq!(first.frame_timing(), Some(old_timing));

        // A fast owner replaces the slow subscriber's pending image without
        // changing the identity already delivered to the other subscriber.
        owner.publish(&picture(2000), Some(new), new_timing, 60);
        assert_eq!(first.frame_info(), Some(old));
        assert_eq!(first.frame_timing(), Some(old_timing));
        assert_eq!(second.next_frame().unwrap().pts.0, 2000);
        assert_eq!(second.frame_info(), Some(new));
        assert_eq!(second.frame_timing(), Some(new_timing));
        assert_eq!(first.frame_info(), Some(old));
        assert_eq!(first.next_frame().unwrap().pts.0, 2000);
        assert_eq!(first.frame_info(), Some(new));
        assert_eq!(first.frame_timing(), Some(new_timing));

        owner.publish(&picture(3000), None, new_timing, 60);
        assert_eq!(first.next_frame().unwrap().pts.0, 3000);
        assert_eq!(first.frame_info(), None, "unknown source metadata cannot reuse an earlier image's identity");
    }

    #[test]
    fn repeated_images_preserve_the_published_source_identity_and_duplicate_flag() {
        let (owner, mut first, _second) = delivery_fixture();
        let picture = |pts| RawFrame::new_i420(2, 2, vec![0; 6], uu_media::Timestamp(pts));
        let original = source_info(11, false);
        let base = Instant::now() - Duration::from_millis(100);
        let timing = FrameTiming { started: base, ready: base + Duration::from_millis(1) };
        owner.publish(&picture(1000), Some(original), timing, 60);
        first.next_frame().unwrap();
        let repeated = source_info(11, true);
        owner.publish(&picture(2000), Some(repeated), timing, 60);
        assert_eq!(first.frame_info(), Some(original));
        assert_eq!(first.next_frame().unwrap().pts.0, 2000);
        assert_eq!(first.frame_info(), Some(repeated));
    }

    #[test]
    fn unsuccessful_delivery_clears_source_identity_and_recovery_publishes_a_new_one() {
        let (owner, mut first, _second) = delivery_fixture();
        let frame = RawFrame::new_i420(2, 2, vec![0; 6], uu_media::Timestamp(1000));
        let base = Instant::now() - Duration::from_millis(100);
        let timing = FrameTiming { started: base, ready: base + Duration::from_millis(1) };
        owner.publish(&frame, Some(source_info(11, false)), timing, 60);
        first.next_frame().unwrap();
        assert_eq!(first.frame_timing(), Some(timing));
        owner.suspend("owned desktop transition");
        assert!(matches!(first.next_frame(), Err(CaptureError::Suspended(_))));
        assert_eq!(first.frame_info(), None);
        assert_eq!(first.frame_timing(), None);

        owner.publish(&frame, Some(source_info(12, false)), timing, 60);
        first.next_frame().unwrap();
        assert_eq!(first.frame_info(), Some(source_info(12, false)));
        assert_eq!(first.frame_timing(), Some(timing));
        assert!(first.wait_updated_frame(Duration::ZERO).unwrap().is_none());
        assert_eq!(first.frame_info(), None);
        assert_eq!(first.frame_timing(), None);

        owner.publish(&frame, Some(source_info(13, false)), timing, 60);
        first.next_frame().unwrap();
        owner.close(Some("owned device removed".into()));
        assert!(matches!(first.next_frame(), Err(CaptureError::Backend(_))));
        assert_eq!(first.frame_info(), None);
        assert_eq!(first.frame_timing(), None);
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
        owner.default_input_format();
        let (send, receive) = std::sync::mpsc::channel();
        let weak = Arc::downgrade(&owner);
        *owner.thread.lock().unwrap() = Some(std::thread::spawn(move || run_capture(weak, Box::new(Source(receive)), 60)));
        owner.wait_ready().unwrap();
        let picture = |pts| RawFrame::new_i420(2, 2, vec![0; 6], uu_media::Timestamp(pts));
        send.send(Ok(picture(1))).unwrap();
        assert_eq!(first.take(Some(Duration::from_secs(1))).unwrap().unwrap().frame.pts.0, 1);
        // The second viewer deliberately leaves this old picture queued.
        send.send(Err(CaptureError::Suspended("desktop switch".into()))).unwrap();
        assert!(matches!(first.take(Some(Duration::from_secs(1))), Err(CaptureError::Suspended(_))));
        assert!(matches!(second.take(Some(Duration::from_secs(1))), Err(CaptureError::Suspended(_))));
        send.send(Ok(picture(2))).unwrap();
        for subscriber in [&first, &second] {
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                match subscriber.take(Some(Duration::from_millis(20))) {
                    Ok(Some(frame)) => { assert_eq!(frame.frame.pts.0, 2); break; }
                    Err(CaptureError::Suspended(_)) | Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(1)),
                    other => panic!("source did not recover: {other:?}"),
                }
            }
        }
        send.send(Err(CaptureError::Backend("device removed".into()))).unwrap();
        assert!(matches!(first.take(Some(Duration::from_secs(1))), Err(CaptureError::Backend(reason)) if reason.contains("device removed")));
        owner.stop_and_join();
    }

    #[test]
    fn first_capture_waits_for_input_format_and_pending_source_can_stop() {
        struct Source(std::sync::mpsc::Sender<(bool, Option<u16>)>, bool, Option<u16>);
        impl Capture for Source {
            fn resolution(&self) -> (u32, u32) { (2, 2) }
            fn prefer_gpu(&mut self, gpu: bool, vendor: Option<u16>) { self.1 = gpu; self.2 = vendor; }
            fn next_frame(&mut self) -> Result<RawFrame, CaptureError> {
                let _ = self.0.send((self.1, self.2));
                Err(CaptureError::Suspended("idle desktop".into()))
            }
        }
        for select_gpu in [true, false] {
            let owner = Owner::new(Key::new(&CaptureSource::Primary), CaptureSource::Primary, options(60));
            let _subscriber = owner.subscribe(60);
            let (sent, received) = std::sync::mpsc::channel();
            let weak = Arc::downgrade(&owner);
            *owner.thread.lock().unwrap() = Some(std::thread::spawn(move || run_capture(weak, Box::new(Source(sent, false, None)), 60)));
            owner.wait_ready().unwrap();
            assert!(received.try_recv().is_err(), "device readiness must not capture in an unselected format");
            if select_gpu {
                owner.set_gpu(true, Some(0x8086));
                assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap(), (true, Some(0x8086)));
            }
            owner.stop_and_join();
        }
    }
}
