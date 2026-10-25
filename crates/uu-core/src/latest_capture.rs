//! A single owner, one replaceable pending image, and acknowledged generations.
//! Source::next must use a bounded platform wait, never publish a cached image.
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc, Condvar, Mutex,
};
use std::thread::JoinHandle;
use std::time::Duration;

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
    fn configure(&mut self, config: Self::Config) -> Result<(), String>;
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
type Reply = mpsc::SyncSender<Result<(), String>>;
struct State<T: Clone, C> {
    phase: Phase,
    generation: u64,
    slot: Option<Packet<T>>,
    command: Option<(C, Reply)>,
    stop: bool,
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
    state.phase = Phase::Stopped;
    shared.current.store(0, Ordering::Release);
    drop(state);
    shared.changed.notify_all();
}
fn produce<S: Source>(source: &mut S, shared: &Shared<S::Frame, S::Config>) {
    let mut last: Option<S::Frame> = None;
    loop {
        let command = {
            let mut state = shared.state.lock().unwrap();
            if state.stop {
                break;
            }
            state.command.take()
        };
        if let Some((config, reply)) = command {
            let applied = source.configure(config);
            if let Err(error) = &applied {
                fail(shared, error.clone());
                let _ = reply.send(applied);
                break;
            }
            let mut state = shared.state.lock().unwrap();
            if state.stop {
                let _ = reply.send(Err("capture stopped during configuration".into()));
                break;
            }
            state.summary.configurations += 1;
            state.phase = Phase::Running;
            shared.current.store(state.generation, Ordering::Release);
            // A quality change can reuse this desktop's last immutable image,
            // with its original observation times. A desktop pause clears last.
            if let Some(frame) = &last {
                state.slot = Some(Packet {
                    frame: frame.clone(),
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
        match source.next(SOURCE_WAIT) {
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
                state.summary.produced += 1;
                last = Some(frame.clone());
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
            Ok(None) => {}
            Err(Failure::Paused(_)) => {
                last = None;
                let mut state = shared.state.lock().unwrap();
                if state.phase != Phase::Paused {
                    state.generation += 1;
                    state.summary.pauses += 1;
                }
                state.phase = Phase::Paused;
                state.slot = None;
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
        fn configure(&mut self, c: u32) -> Result<(), String> {
            self.configured.send(c).unwrap();
            Ok(())
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
        let seed = owner
            .latest(Duration::ZERO)
            .unwrap_or_else(|_| panic!())
            .unwrap();
        assert!(seed.lease.valid());
        assert_eq!(seed.frame, old.frame);
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
        fn configure(&mut self, c: u32) -> Result<(), String> {
            if c == 99 {
                self.release.recv().unwrap();
            }
            Ok(())
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
