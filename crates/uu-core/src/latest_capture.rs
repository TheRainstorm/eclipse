//! A single owner, one replaceable pending image, and acknowledged generations.
//! Source::next must use a bounded platform wait, never publish a cached image.
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc, Condvar, Mutex,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Running,
    Paused,
    Retiring,
    Stopped,
}
pub enum Failure {
    Paused(String),
    Failed(String),
}
pub trait Source: Send + 'static {
    type Frame: Clone + Send + 'static;
    type Config: Clone + Send + 'static;
    /// A single owner cadence; None preserves the original continuous source.
    fn cadence(_config: &Self::Config) -> Option<Duration> {
        None
    }
    fn configure(&mut self, config: Self::Config) -> Result<Option<Self::Frame>, String>;
    fn next(&mut self, wait: Duration) -> Result<Option<Self::Frame>, Failure>;
}
#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub produced: u64,
    pub replaced: u64,
    pub pauses: u64,
    pub configurations: u64,
    pub owner_thread: String,
    pub owner_native_thread: Option<u32>,
    pub generation: u64,
    pub phase: String,
    pub joined: bool,
    pub owner_pacing: bool,
    pub ticks: u64,
    pub repeat_opportunities: u64,
    pub no_frame_opportunities: u64,
    pub skipped_periods: u64,
}
#[derive(Clone)]
pub struct Lease {
    current: Arc<AtomicU64>,
    generation: u64,
}
impl Lease {
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn valid(&self) -> bool {
        self.current.load(Ordering::Acquire) == self.generation
    }
}
#[derive(Clone)]
pub struct Packet<T: Clone> {
    pub frame: T,
    pub lease: Lease,
}
/// One coalescing publication. Repeat is permission for this period, not a
/// claim that Source::next captured another image. NoFrame has no valid cache.
pub enum Delivery<T: Clone> {
    Fresh(Packet<T>),
    Repeat,
    NoFrame,
}
#[derive(Clone, Copy)]
enum Pulse {
    Fresh,
    Repeat,
    NoFrame,
}
struct Cadence {
    period: Duration,
    next: Instant,
}
impl Cadence {
    fn new(period: Duration, now: Instant) -> Self {
        Self {
            period,
            next: now + period,
        }
    }
    fn advance(&mut self, now: Instant) -> u64 {
        self.next += self.period;
        if self.next > now {
            return 0;
        }
        let missed = (now.duration_since(self.next).as_nanos() / self.period.as_nanos() + 1)
            .min(u128::from(u64::MAX)) as u64;
        self.next = match u32::try_from(missed)
            .ok()
            .and_then(|n| self.period.checked_mul(n))
            .and_then(|delta| self.next.checked_add(delta))
        {
            Some(next) => next,
            None => now + self.period,
        };
        missed
    }
}
type Reply = mpsc::SyncSender<Result<(), String>>;
struct State<T: Clone, C> {
    phase: Phase,
    generation: u64,
    slot: Option<Packet<T>>,
    command: Option<(C, Reply)>,
    stop: bool,
    pulse: Option<Pulse>,
    have_image: bool,
    interrupted: bool,
    error: Option<String>,
    summary: Summary,
}
struct Shared<T: Clone, C> {
    state: Mutex<State<T, C>>,
    changed: Condvar,
    current: Arc<AtomicU64>,
}
pub struct Owner<S: Source> {
    shared: Arc<Shared<S::Frame, S::Config>>,
    thread: Option<JoinHandle<()>>,
    done: Option<mpsc::Receiver<()>>,
}
const JOIN_LIMIT: Duration = Duration::from_secs(2);
const SOURCE_WAIT: Duration = Duration::from_millis(16);
impl<S: Source> Owner<S> {
    pub fn start(mut source: S, config: S::Config) -> Result<Self, String> {
        let (reply, ready) = mpsc::sync_channel(1);
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                phase: Phase::Retiring,
                generation: 1,
                slot: None,
                command: Some((config, reply)),
                stop: false,
                pulse: None,
                have_image: false,
                interrupted: false,
                error: None,
                summary: Summary::default(),
            }),
            changed: Condvar::new(),
            current: Arc::new(AtomicU64::new(0)),
        });
        let worker = shared.clone();
        let (done, finished) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("EclipseCapture".into())
            .spawn(move || {
                worker.state.lock().unwrap().summary.owner_native_thread = native_thread_id();
                worker.state.lock().unwrap().summary.owner_thread =
                    format!("{:?}", std::thread::current().id());
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    produce(&mut source, &worker)
                }));
                if run.is_err() {
                    fail(&worker, "capture owner panicked".into());
                }
                drop(source); // All capture, desktop and GPU leases leave on their owner.
                let mut state = worker.state.lock().unwrap();
                state.slot = None;
                state.pulse = None;
                state.have_image = false;
                state.phase = Phase::Stopped;
                worker.current.store(0, Ordering::Release);
                if let Some((_, reply)) = state.command.take() {
                    let _ = reply.send(Err("capture owner stopped".into()));
                }
                drop(state);
                worker.changed.notify_all();
                let _ = done.send(());
            })
            .map_err(|e| e.to_string())?;
        let mut result = Self {
            shared,
            thread: Some(thread),
            done: Some(finished),
        };
        match ready.recv_timeout(JOIN_LIMIT) {
            Ok(Ok(())) => Ok(result),
            other => {
                let error = format!("capture configuration failed: {other:?}");
                let _ = result.stop();
                Err(error)
            }
        }
    }
    pub fn configure(&self, config: S::Config) -> Result<(), String> {
        self.configure_with_limit(config, JOIN_LIMIT)
    }
    fn configure_with_limit(&self, config: S::Config, limit: Duration) -> Result<(), String> {
        let (reply, received) = mpsc::sync_channel(1);
        {
            let mut state = self.shared.state.lock().unwrap();
            if state.stop || state.phase == Phase::Stopped {
                return Err(state
                    .error
                    .clone()
                    .unwrap_or_else(|| "capture owner stopped".into()));
            }
            if state.command.is_some() {
                return Err("capture configuration already pending".into());
            }
            state.generation = state
                .generation
                .checked_add(1)
                .ok_or("capture generation overflow")?;
            state.phase = Phase::Retiring;
            state.slot = None;
            state.pulse = None;
            state.have_image = false;
            self.shared.current.store(0, Ordering::Release);
            state.command = Some((config, reply));
        }
        self.shared.changed.notify_all();
        match received.recv_timeout(limit) {
            Ok(result) => result,
            Err(error) => {
                let message = format!("capture configuration acknowledgement: {error}");
                fail(&self.shared, message.clone());
                Err(message)
            }
        }
    }
    /// Wait only for the first image. A paced encoder can request zero wait
    /// after that and explicitly repeat its own leased last image.
    pub fn latest(&self, wait: Duration) -> Result<Option<Packet<S::Frame>>, Failure> {
        let state = self.shared.state.lock().unwrap();
        let (mut state, _) = self
            .shared
            .changed
            .wait_timeout_while(state, wait, |s| {
                s.slot.is_none() && s.phase == Phase::Running && !s.stop
            })
            .unwrap();
        if let Some(error) = &state.error {
            return Err(Failure::Failed(error.clone()));
        }
        if state.stop || state.phase == Phase::Stopped {
            return Err(Failure::Failed("capture owner stopped".into()));
        }
        if state.phase != Phase::Running {
            return Err(Failure::Paused("capture generation not running".into()));
        }
        Ok(state.slot.take())
    }
    /// Only the owner can issue repeat opportunities. Spurious notifications
    /// cannot turn an empty slot into a repeated image.
    pub fn next_event(&self, wait: Duration) -> Result<Option<Delivery<S::Frame>>, Failure> {
        receive_event(&self.shared, wait)
    }
    /// Control requests interrupt consumption without minting a frame/repeat.
    pub fn interrupt_handle(&self) -> Arc<dyn Fn() + Send + Sync> {
        let shared = self.shared.clone();
        Arc::new(move || {
            shared.state.lock().unwrap().interrupted = true;
            shared.changed.notify_all();
        })
    }
    pub fn summary(&self) -> Summary {
        let state = self.shared.state.lock().unwrap();
        let mut summary = state.summary.clone();
        summary.generation = state.generation;
        summary.phase = format!("{:?}", state.phase);
        summary
    }
    pub fn stop(&mut self) -> Result<Summary, String> {
        {
            let mut state = self.shared.state.lock().unwrap();
            state.stop = true;
            state.slot = None;
            state.pulse = None;
            state.have_image = false;
            self.shared.current.store(0, Ordering::Release);
        }
        self.shared.changed.notify_all();
        if let Some(done) = self.done.take() {
            if done.recv_timeout(JOIN_LIMIT).is_err() {
                // Do not call an unfinished owner joined or clean. Its leases
                // remain owned by that thread; no caller can reuse its pixels.
                self.thread.take();
                return Err("capture owner did not exit within two seconds; not joined".into());
            }
            if let Some(thread) = self.thread.take() {
                thread.join().map_err(|_| "capture owner join failed")?;
            }
            self.shared.state.lock().unwrap().summary.joined = true;
        }
        let state = self.shared.state.lock().unwrap();
        if let Some(error) = &state.error {
            return Err(error.clone());
        }
        let mut summary = state.summary.clone();
        summary.generation = state.generation;
        summary.phase = format!("{:?}", state.phase);
        Ok(summary)
    }
}
impl<S: Source> Drop for Owner<S> {
    fn drop(&mut self) {
        if self.done.is_some() {
            let _ = self.stop();
        }
    }
}
fn receive_event<T: Clone, C>(
    shared: &Shared<T, C>,
    wait: Duration,
) -> Result<Option<Delivery<T>>, Failure> {
    let state = shared.state.lock().unwrap();
    let (mut state, _) = shared
        .changed
        .wait_timeout_while(state, wait, |s| {
            s.pulse.is_none()
                && !s.interrupted
                && s.error.is_none()
                && !s.stop
                && s.phase != Phase::Stopped
        })
        .unwrap();
    if let Some(error) = &state.error {
        return Err(Failure::Failed(error.clone()));
    }
    if state.stop || state.phase == Phase::Stopped {
        return Err(Failure::Failed("capture owner stopped".into()));
    }
    if state.interrupted {
        state.interrupted = false;
        return Ok(None);
    }
    let Some(pulse) = state.pulse.take() else {
        return Ok(None);
    };
    if state.phase != Phase::Running {
        return Err(Failure::Paused("capture generation not running".into()));
    }
    Ok(Some(match pulse {
        Pulse::Fresh => match state.slot.take() {
            Some(frame) => Delivery::Fresh(frame),
            None => Delivery::NoFrame,
        },
        Pulse::Repeat => Delivery::Repeat,
        Pulse::NoFrame => Delivery::NoFrame,
    }))
}
fn native_thread_id() -> Option<u32> {
    #[cfg(windows)]
    {
        Some(unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() })
    }
    #[cfg(not(windows))]
    {
        None
    }
}
fn fail<T: Clone, C>(shared: &Shared<T, C>, error: String) {
    let mut state = shared.state.lock().unwrap();
    state.error = Some(error);
    state.stop = true;
    state.slot = None;
    state.pulse = None;
    state.have_image = false;
    state.phase = Phase::Stopped;
    shared.current.store(0, Ordering::Release);
    drop(state);
    shared.changed.notify_all();
}
fn produce<S: Source>(source: &mut S, shared: &Shared<S::Frame, S::Config>) {
    let mut cadence: Option<Cadence> = None;
    loop {
        let command = {
            let mut state = shared.state.lock().unwrap();
            if state.stop {
                break;
            }
            state.command.take()
        };
        if let Some((config, reply)) = command {
            let period = S::cadence(&config);
            if period.is_some_and(|p| p.is_zero()) {
                let error = "capture owner pacing interval must be positive".to_owned();
                fail(shared, error.clone());
                let _ = reply.send(Err(error));
                break;
            }
            let confirmed = match source.configure(config) {
                Ok(frame) => frame,
                Err(error) => {
                    fail(shared, error.clone());
                    let _ = reply.send(Err(error));
                    break;
                }
            };
            let mut state = shared.state.lock().unwrap();
            if state.stop {
                let _ = reply.send(Err("capture stopped during configuration".into()));
                break;
            }
            cadence = period.map(|p| Cadence::new(p, Instant::now()));
            state.summary.owner_pacing = cadence.is_some();
            state.have_image = false;
            state.pulse = None;
            state.summary.configurations += 1;
            state.phase = Phase::Running;
            shared.current.store(state.generation, Ordering::Release);
            // Only an image explicitly confirmed by the source owner after
            // configuration may enter this generation. Never retag an old slot.
            if let Some(frame) = confirmed {
                state.have_image = true;
                if cadence.is_some() {
                    state.pulse = Some(Pulse::Fresh);
                }
                state.summary.produced += 1;
                state.slot = Some(Packet {
                    frame,
                    lease: Lease {
                        current: shared.current.clone(),
                        generation: state.generation,
                    },
                });
            }
            drop(state);
            shared.changed.notify_all();
            let _ = reply.send(Ok(()));
        }
        if let Some(pacer) = &cadence {
            let state = shared.state.lock().unwrap();
            let (state, _) = shared
                .changed
                .wait_timeout_while(
                    state,
                    pacer.next.saturating_duration_since(Instant::now()),
                    |s| !s.stop && s.command.is_none(),
                )
                .unwrap();
            if state.stop {
                break;
            }
            if state.command.is_some() {
                continue;
            }
        }
        // At the deadline, acquire only what is ready. An extra 16ms platform
        // wait would add a second cadence on top of the owner's period.
        match source.next(if cadence.is_some() {
            Duration::ZERO
        } else {
            SOURCE_WAIT
        }) {
            Ok(Some(frame)) => {
                let mut state = shared.state.lock().unwrap();
                if state.stop {
                    break;
                }
                if state.command.is_some() || state.phase == Phase::Retiring {
                    continue;
                }
                state.phase = Phase::Running;
                shared.current.store(state.generation, Ordering::Release);
                if state.slot.is_some() {
                    state.summary.replaced += 1;
                }
                state.have_image = true;
                if cadence.is_some() {
                    state.pulse = Some(Pulse::Fresh);
                }
                state.summary.produced += 1;
                state.slot = Some(Packet {
                    frame,
                    lease: Lease {
                        current: shared.current.clone(),
                        generation: state.generation,
                    },
                });
                drop(state);
                shared.changed.notify_all();
            }
            Ok(None) => {
                if cadence.is_some() {
                    let mut state = shared.state.lock().unwrap();
                    if !state.stop && state.command.is_none() && state.phase == Phase::Running {
                        // Preserve a pending fresh image if the consumer is slow.
                        if state.slot.is_some() {
                            state.pulse = Some(Pulse::Fresh);
                        } else if state.have_image {
                            state.pulse = Some(Pulse::Repeat);
                            state.summary.repeat_opportunities += 1;
                        } else {
                            state.pulse = Some(Pulse::NoFrame);
                            state.summary.no_frame_opportunities += 1;
                        }
                        drop(state);
                        shared.changed.notify_all();
                    }
                }
            }
            Err(Failure::Paused(_)) => {
                let mut state = shared.state.lock().unwrap();
                if state.phase != Phase::Paused {
                    state.generation += 1;
                    state.summary.pauses += 1;
                }
                state.phase = Phase::Paused;
                state.slot = None;
                state.pulse = None;
                state.have_image = false;
                if cadence.is_some() {
                    state.pulse = Some(Pulse::NoFrame);
                }
                shared.current.store(0, Ordering::Release);
                shared.changed.notify_all();
                // Recovery APIs may return immediately while a desktop is
                // unavailable. Use a cancellable condition wait, not a poll loop.
                let _ = shared
                    .changed
                    .wait_timeout_while(state, Duration::from_millis(25), |s| {
                        !s.stop && s.command.is_none()
                    })
                    .unwrap();
            }
            Err(Failure::Failed(error)) => {
                fail(shared, error);
                break;
            }
        }
        if let Some(pacer) = &mut cadence {
            let missed = pacer.advance(Instant::now());
            let mut state = shared.state.lock().unwrap();
            state.summary.ticks += 1;
            state.summary.skipped_periods += missed;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    enum Input {
        Image(Arc<Vec<u8>>),
        Pause,
        Error,
        Fence(mpsc::SyncSender<()>),
    }
    struct Controlled {
        rx: mpsc::Receiver<Input>,
        configured: mpsc::Sender<u32>,
    }
    impl Source for Controlled {
        type Frame = Arc<Vec<u8>>;
        type Config = u32;
        fn configure(&mut self, c: u32) -> Result<Option<Self::Frame>, String> {
            self.configured.send(c).unwrap();
            Ok(None)
        }
        fn next(&mut self, wait: Duration) -> Result<Option<Self::Frame>, Failure> {
            loop {
                match self.rx.recv_timeout(wait) {
                    Ok(Input::Image(frame)) => return Ok(Some(frame)),
                    Ok(Input::Pause) => return Err(Failure::Paused("desktop".into())),
                    Ok(Input::Error) => {
                        return Err(Failure::Failed("injected capture failure".into()))
                    }
                    Ok(Input::Fence(reply)) => {
                        reply.send(()).unwrap();
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => return Ok(None),
                    Err(_) => return Err(Failure::Failed("source closed".into())),
                }
            }
        }
    }
    fn setup() -> (Owner<Controlled>, mpsc::Sender<Input>, mpsc::Receiver<u32>) {
        let (tx, rx) = mpsc::channel();
        let (configured, configs) = mpsc::channel();
        (
            Owner::start(Controlled { rx, configured }, 60).unwrap(),
            tx,
            configs,
        )
    }
    fn fence(tx: &mpsc::Sender<Input>) {
        let (reply, rx) = mpsc::sync_channel(0);
        tx.send(Input::Fence(reply)).unwrap();
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }
    #[test]
    fn blocked_encoder_keeps_only_latest_and_never_mutates_inflight_pixels() {
        let (mut owner, tx, configs) = setup();
        assert_eq!(configs.recv().unwrap(), 60);
        tx.send(Input::Image(Arc::new(vec![1; 64]))).unwrap();
        let held = owner
            .latest(Duration::from_secs(1))
            .unwrap_or_else(|_| panic!("first image"))
            .unwrap();
        for value in 2..=20 {
            tx.send(Input::Image(Arc::new(vec![value; 64]))).unwrap();
        }
        fence(&tx);
        let newest = owner
            .latest(Duration::ZERO)
            .unwrap_or_else(|_| panic!("latest image"))
            .unwrap();
        assert_eq!(*newest.frame, vec![20; 64]);
        assert_eq!(*held.frame, vec![1; 64]);
        assert!(held.lease.valid());
        assert_eq!(owner.summary().replaced, 18);
        assert!(owner.stop().unwrap().joined);
        assert!(!held.lease.valid());
    }
    #[test]
    fn pause_clears_pending_invalidates_inflight_and_resumes_a_new_generation() {
        let (mut owner, tx, _) = setup();
        tx.send(Input::Image(Arc::new(vec![1]))).unwrap();
        let old = owner
            .latest(Duration::from_secs(1))
            .unwrap_or_else(|_| panic!())
            .unwrap();
        tx.send(Input::Pause).unwrap();
        fence(&tx);
        assert!(!old.lease.valid());
        assert!(matches!(
            owner.latest(Duration::ZERO),
            Err(Failure::Paused(_))
        ));
        tx.send(Input::Image(Arc::new(vec![2]))).unwrap();
        fence(&tx);
        let new = owner
            .latest(Duration::from_secs(1))
            .unwrap_or_else(|_| panic!())
            .unwrap();
        assert!(new.lease.valid());
        assert!(!old.lease.valid());
        assert!(owner.stop().unwrap().joined);
    }
    #[test]
    fn reconfiguration_ack_retires_old_generation_and_stop_wakes_platform_wait() {
        let (mut owner, tx, configs) = setup();
        configs.recv().unwrap();
        tx.send(Input::Image(Arc::new(vec![1]))).unwrap();
        let old = owner
            .latest(Duration::from_secs(1))
            .unwrap_or_else(|_| panic!())
            .unwrap();
        owner.configure(120).unwrap();
        assert_eq!(configs.recv().unwrap(), 120);
        assert!(!old.lease.valid());
        assert!(
            owner
                .latest(Duration::ZERO)
                .unwrap_or_else(|_| panic!())
                .is_none(),
            "old pending image crossed configuration"
        );
        tx.send(Input::Image(Arc::new(vec![2]))).unwrap();
        fence(&tx);
        let fresh = owner
            .latest(Duration::ZERO)
            .unwrap_or_else(|_| panic!())
            .unwrap();
        assert!(fresh.lease.valid());
        assert_eq!(*fresh.frame, vec![2]);
        let start = std::time::Instant::now();
        assert!(owner.stop().unwrap().joined);
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn capture_failure_wakes_consumer_and_cannot_claim_clean_exit() {
        let (mut owner, tx, _) = setup();
        tx.send(Input::Error).unwrap();
        let result = owner.latest(Duration::from_secs(1));
        assert!(matches!(result, Err(Failure::Failed(_))));
        assert!(owner.stop().is_err());
        assert!(owner.summary().joined);
    }
}

#[cfg(test)]
mod timeout_tests {
    use super::*;
    struct Blocking {
        release: mpsc::Receiver<()>,
    }
    impl Source for Blocking {
        type Frame = u32;
        type Config = u32;
        fn configure(&mut self, c: u32) -> Result<Option<Self::Frame>, String> {
            if c == 99 {
                self.release.recv().unwrap();
            }
            Ok(None)
        }
        fn next(&mut self, wait: Duration) -> Result<Option<u32>, Failure> {
            std::thread::sleep(wait);
            Ok(None)
        }
    }
    #[test]
    fn configuration_timeout_retires_generation_before_late_acknowledgement() {
        let (release, rx) = mpsc::sync_channel(1);
        let mut owner = Owner::start(Blocking { release: rx }, 60).unwrap();
        assert!(owner
            .configure_with_limit(99, Duration::from_millis(50))
            .is_err());
        assert!(matches!(
            owner.latest(Duration::ZERO),
            Err(Failure::Failed(_))
        ));
        release.send(()).unwrap();
        assert!(owner.stop().is_err());
        assert!(owner.summary().joined);
    }
}

#[cfg(test)]
mod pacing_tests {
    use super::*;
    enum Action {
        Frame(u32),
        Pause,
    }
    struct Paced {
        actions: mpsc::Receiver<Action>,
        configs: mpsc::Sender<u32>,
    }
    impl Source for Paced {
        type Frame = Arc<u32>;
        type Config = (u32, bool);
        fn cadence(c: &Self::Config) -> Option<Duration> {
            Some(if c.0 == 0 {
                Duration::ZERO
            } else {
                Duration::from_secs_f64(1.0 / f64::from(c.0))
            })
        }
        fn configure(&mut self, c: Self::Config) -> Result<Option<Self::Frame>, String> {
            self.configs.send(c.0).unwrap();
            Ok(c.1.then(|| Arc::new(c.0)))
        }
        fn next(&mut self, wait: Duration) -> Result<Option<Self::Frame>, Failure> {
            assert_eq!(
                wait,
                Duration::ZERO,
                "owner deadline must not stack a platform wait"
            );
            match self.actions.try_recv() {
                Ok(Action::Frame(v)) => Ok(Some(Arc::new(v))),
                Ok(Action::Pause) => Err(Failure::Paused("desktop transition".into())),
                Err(mpsc::TryRecvError::Empty) => Ok(None),
                Err(_) => Err(Failure::Failed("fixture closed".into())),
            }
        }
    }
    fn setup(fps: u32, image: bool) -> (Owner<Paced>, mpsc::Sender<Action>, mpsc::Receiver<u32>) {
        let (tx, rx) = mpsc::channel();
        let (configs, seen) = mpsc::channel();
        (
            Owner::start(
                Paced {
                    actions: rx,
                    configs,
                },
                (fps, image),
            )
            .unwrap(),
            tx,
            seen,
        )
    }
    fn event(owner: &Owner<Paced>) -> Delivery<Arc<u32>> {
        owner
            .next_event(Duration::from_secs(1))
            .unwrap_or_else(|_| panic!("delivery failed"))
            .expect("delivery timeout")
    }
    #[test]
    fn deadline_skips_expired_periods_without_burst_catchup() {
        let base = Instant::now();
        let p = Duration::from_millis(10);
        let mut c = Cadence::new(p, base);
        assert_eq!(c.advance(base + p), 0);
        assert_eq!(c.next, base + Duration::from_millis(20));
        assert_eq!(c.advance(base + Duration::from_millis(105)), 8);
        assert_eq!(c.next, base + Duration::from_millis(110));
        assert!(c.next > base + Duration::from_millis(105));
    }
    #[test]
    fn first_confirmed_image_is_immediately_available_then_explicit_repeat() {
        let (mut owner, _tx, _) = setup(60, true);
        let Delivery::Fresh(first) = owner
            .next_event(Duration::ZERO)
            .unwrap_or_else(|_| panic!())
            .unwrap()
        else {
            panic!("first image")
        };
        assert_eq!(*first.frame, 60);
        assert!(first.lease.valid());
        assert!(matches!(event(&owner), Delivery::Repeat));
        assert!(matches!(event(&owner), Delivery::Repeat));
        let s = owner.stop().unwrap();
        assert!(s.joined && s.owner_pacing);
        assert!(s.repeat_opportunities >= 2);
        assert_eq!(s.produced, 1);
    }
    #[test]
    fn no_image_is_a_bounded_no_frame_pulse_not_a_fake_capture() {
        let (mut owner, _tx, _) = setup(60, false);
        assert!(matches!(event(&owner), Delivery::NoFrame));
        assert!(matches!(event(&owner), Delivery::NoFrame));
        let s = owner.stop().unwrap();
        assert_eq!(s.produced, 0);
        assert!(s.no_frame_opportunities >= 2);
    }
    #[test]
    fn fresh_capture_publication_has_no_second_consumer_period() {
        let (mut owner, tx, _) = setup(60, false);
        tx.send(Action::Frame(7)).unwrap();
        let Delivery::Fresh(frame) = event(&owner) else {
            panic!("fresh")
        };
        assert_eq!(*frame.frame, 7);
        // Consuming the actual coalesced pulse with zero timeout is immediate;
        // no second periodic timer exists in receive_event.
        assert!(owner
            .next_event(Duration::ZERO)
            .unwrap_or_else(|_| panic!())
            .is_none());
        owner.stop().unwrap();
    }
    #[test]
    fn control_interrupt_and_spurious_wake_do_not_mint_repeats() {
        let (mut owner, _tx, _) = setup(1, false);
        owner.shared.changed.notify_all();
        assert!(owner
            .next_event(Duration::from_millis(5))
            .unwrap_or_else(|_| panic!())
            .is_none());
        let wake = owner.interrupt_handle();
        wake();
        assert!(owner
            .next_event(Duration::ZERO)
            .unwrap_or_else(|_| panic!())
            .is_none());
        assert_eq!(owner.summary().repeat_opportunities, 0);
        owner.stop().unwrap();
    }
    #[test]
    fn stop_wakes_waiting_consumer_and_joins_without_waiting_full_period() {
        let (mut owner, _tx, _) = setup(1, false);
        let shared = owner.shared.clone();
        let entered = mpsc::sync_channel(1);
        let waiter = std::thread::spawn(move || {
            entered.0.send(()).unwrap();
            matches!(
                receive_event(&shared, Duration::from_secs(5)),
                Err(Failure::Failed(_))
            )
        });
        entered.1.recv().unwrap();
        assert!(owner.stop().unwrap().joined);
        assert!(waiter.join().unwrap());
    }
    #[test]
    fn fps_update_and_cancel_rollback_retire_all_previous_leases() {
        let (mut owner, _tx, configs) = setup(60, true);
        assert_eq!(configs.recv().unwrap(), 60);
        let Delivery::Fresh(old) = event(&owner) else {
            panic!()
        };
        owner.configure((120, true)).unwrap();
        assert_eq!(configs.recv().unwrap(), 120);
        assert!(!old.lease.valid());
        let Delivery::Fresh(new) = event(&owner) else {
            panic!()
        };
        assert_eq!(*new.frame, 120);
        // This is the same acknowledged rollback used when a quality request
        // closes after owner reconfiguration but before encoder commit.
        owner.configure((60, true)).unwrap();
        assert_eq!(configs.recv().unwrap(), 60);
        assert!(!new.lease.valid());
        let Delivery::Fresh(back) = event(&owner) else {
            panic!()
        };
        assert_eq!(*back.frame, 60);
        assert!(back.lease.valid());
        assert_eq!(owner.summary().configurations, 3);
        owner.stop().unwrap();
    }
    #[test]
    fn pause_invalidates_held_frame_and_resume_publishes_new_generation() {
        let (mut owner, tx, _) = setup(60, true);
        let Delivery::Fresh(old) = event(&owner) else {
            panic!()
        };
        tx.send(Action::Pause).unwrap();
        assert!(matches!(
            owner.next_event(Duration::from_secs(1)),
            Err(Failure::Paused(_))
        ));
        assert!(!old.lease.valid());
        tx.send(Action::Frame(9)).unwrap();
        let Delivery::Fresh(new) = event(&owner) else {
            panic!()
        };
        assert_eq!(*new.frame, 9);
        assert!(new.lease.valid());
        assert_ne!(old.lease.generation(), new.lease.generation());
        owner.stop().unwrap();
    }
    #[test]
    fn slow_consumer_keeps_single_replaceable_latest_and_held_image_immutable() {
        let (mut owner, tx, _) = setup(240, true);
        let Delivery::Fresh(held) = event(&owner) else {
            panic!()
        };
        for v in 1..=5 {
            tx.send(Action::Frame(v)).unwrap();
        }
        std::thread::sleep(Duration::from_millis(80));
        let Delivery::Fresh(latest) = event(&owner) else {
            panic!()
        };
        assert_eq!(*latest.frame, 5);
        assert_eq!(*held.frame, 240);
        assert!(owner.summary().replaced >= 4);
        owner.stop().unwrap();
        assert!(!held.lease.valid());
    }
}
