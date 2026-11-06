//! Driver preparation is a host configuration transaction, separate from leases.
//! A trusted provider owns pinned download, OS authorization, exact-resource
//! mutation and rollback. Status queries never install, reload or save anything.
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Mutex,
};

/// Initial reservation size, not a limit on sessions or later pool expansion.
pub const INITIAL_CAPACITY: u32 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Unsupported,
    NeedsDriver,
    NeedsCapacity,
    NeedsElevation,
    WaitingForIdle,
    Preparing,
    Ready,
    Failed,
    RecoveryPending,
}

/// Counts must come from the host, never from fields supplied by a remote peer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Observation {
    pub supported: bool,
    pub driver_present: bool,
    pub configured_capacity: Option<u32>,
    pub targets: u32,
    pub available: u32,
    pub active_driver_targets: u32,
    pub claimed_targets: u32,
    pub active_streams: u32,
    /// An elevated local administrator or the installed management service.
    pub authority: bool,
    pub recovery_pending: bool,
}

impl Observation {
    fn ready(self, requested: u32) -> bool {
        self.supported && self.driver_present && self.targets >= requested
    }
    fn busy(self) -> bool {
        self.active_streams != 0 || self.active_driver_targets != 0 || self.claimed_targets != 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub phase: Phase,
    pub configured_capacity: Option<u32>,
    pub targets: u32,
    pub available: u32,
    pub requested_capacity: u32,
    pub queued: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub message: String,
    /// The provider could not prove completion/rollback and retains its journal
    /// and exact claims. Neither polling nor a second request may repeat it.
    pub outcome: FailureOutcome,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureOutcome {
    Unchanged,
    AdmissionChanged,
    RecoveryPending,
}
impl Failure {
    pub fn unchanged(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            outcome: FailureOutcome::Unchanged,
        }
    }
    pub fn pending(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            outcome: FailureOutcome::RecoveryPending,
        }
    }
    fn admission_changed() -> Self {
        Self {
            message: "host authorization or idle state changed before driver preparation".into(),
            outcome: FailureOutcome::AdmissionChanged,
        }
    }
}
pub type Result<T> = std::result::Result<T, Failure>;

/// Implementations must verify authorization again at mutation checkpoints,
/// serialize machine-wide driver changes, claim every target and preserve all
/// original outputs. `apply` may not call pipe reload or replace unrelated XML.
pub trait Provider: Sync {
    type Package;
    fn observe(&self) -> Result<Observation>;
    fn download_pinned(&self) -> Result<Self::Package>;
    /// Called while the provider owns its driver locks/claims. Check only host
    /// activity and authorization; its own new targets are not foreign work.
    fn host_idle(&self) -> Result<()> {
        let observed = self.observe()?;
        if !observed.supported || !observed.authority || observed.active_streams != 0 {
            return Err(Failure::admission_changed());
        }
        Ok(())
    }
    fn apply(
        &self,
        requested_capacity: u32,
        package: Option<&Self::Package>,
        idle_checkpoint: &(dyn Fn() -> Result<()> + Sync),
    ) -> Result<()>;
}

#[derive(Default)]
struct State {
    failure: Option<Failure>,
}

/// One accepted preparation intent can wait for idle without interrupting a
/// stream. Its host worker calls `resume`; observing status never resumes it.
#[derive(Default)]
pub struct Coordinator {
    requested: AtomicU32,
    queued: AtomicBool,
    recovery_requested: AtomicBool,
    running: AtomicBool,
    blocked: AtomicBool,
    state: Mutex<State>,
}

impl Coordinator {
    pub fn in_flight(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }
    pub fn queued(&self) -> bool {
        self.recovery_queued() || (self.queued.load(Ordering::Acquire)
            && !self.blocked.load(Ordering::Acquire)
            && self.failure().is_none())
    }
    pub(super) fn queue_recovery(&self) {
        self.recovery_requested.store(true, Ordering::Release);
    }
    pub(super) fn recovery_queued(&self) -> bool {
        self.recovery_requested.load(Ordering::Acquire)
    }
    pub fn recovery_pending(&self) -> bool {
        self.blocked.load(Ordering::Acquire)
    }
    pub(super) fn recovered(&self) {
        self.blocked.store(false, Ordering::Release);
        self.queued.store(false, Ordering::Release);
        self.recovery_requested.store(false, Ordering::Release);
        if let Ok(mut state) = self.state.lock() {
            state.failure = None;
        }
    }
    fn complete(&self, observed: Observation) {
        self.queued.store(false, Ordering::Release);
        if !observed.ready(self.requested.load(Ordering::Acquire)) {
            self.queued.store(true, Ordering::Release);
        }
    }
    fn failure(&self) -> Option<Failure> {
        self.state.lock().ok().and_then(|s| s.failure.clone())
    }
    fn status_for(&self, observed: Observation) -> Status {
        let requested = self.requested.load(Ordering::Acquire).max(INITIAL_CAPACITY);
        let failure = self.failure();
        let phase = if self.blocked.load(Ordering::Acquire) || observed.recovery_pending {
            Phase::RecoveryPending
        } else if self.running.load(Ordering::Acquire) {
            Phase::Preparing
        } else if failure.is_some() {
            Phase::Failed
        } else if !observed.supported {
            Phase::Unsupported
        } else if observed.ready(requested) {
            Phase::Ready
        } else if !observed.authority {
            Phase::NeedsElevation
        } else if observed.busy() {
            Phase::WaitingForIdle
        } else if !observed.driver_present {
            Phase::NeedsDriver
        } else {
            Phase::NeedsCapacity
        };
        Status {
            phase,
            configured_capacity: observed.configured_capacity,
            targets: observed.targets,
            available: observed.available,
            requested_capacity: requested,
            queued: (self.recovery_queued() && phase == Phase::RecoveryPending)
                || (self.queued.load(Ordering::Acquire)
                && matches!(
                    phase,
                    Phase::NeedsDriver
                        | Phase::NeedsCapacity
                        | Phase::NeedsElevation
                        | Phase::WaitingForIdle
                        | Phase::Preparing
                )),
            error: failure.map(|f| f.message),
        }
    }
    /// Explicit recovery is another exclusive host mutation, not a status read.
    /// Keep session admission closed until its exact readback is established.
    pub fn recover<P: Provider>(
        &self,
        provider: &P,
        operation: impl FnOnce() -> Result<()>,
    ) -> Result<Status> {
        if self
            .running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return self.inspect(provider);
        }
        let mut flight = Flight {
            owner: self,
            finished: false,
        };
        let result = (|| {
            if let Err(failure) = provider.host_idle() {
                if failure.outcome != FailureOutcome::AdmissionChanged {
                    self.recovery_requested.store(false, Ordering::Release);
                }
                return Err(failure);
            }
            // One explicit recovery intent can wait for host admission. Once
            // accepted by the provider it is consumed even if recovery fails;
            // background polling cannot replay an uncertain operation.
            self.recovery_requested.store(false, Ordering::Release);
            operation()?;
            if provider.observe()?.recovery_pending {
                return Err(Failure::pending(
                    "the earlier capacity transaction has not been proven recovered",
                ));
            }
            Ok(())
        })();
        match result {
            Ok(()) => self.recovered(),
            Err(failure) => {
                self.blocked.fetch_or(
                    failure.outcome == FailureOutcome::RecoveryPending,
                    Ordering::AcqRel,
                );
                self.queued.store(false, Ordering::Release);
                if failure.outcome != FailureOutcome::AdmissionChanged {
                    self.state
                        .lock()
                        .map_err(|_| Failure::pending("preparation state is poisoned"))?
                        .failure = Some(failure);
                }
            }
        }
        flight.finished = true;
        self.running.store(false, Ordering::Release);
        self.inspect(provider)
    }
    pub fn inspect<P: Provider>(&self, provider: &P) -> Result<Status> {
        Ok(self.status_for(provider.observe()?))
    }
    /// Register idle-only growth without delaying a lease from an existing slot.
    /// Unlike a new explicit request this does not clear an earlier failure.
    pub fn queue(&self, minimum: u32) {
        if self.blocked.load(Ordering::Acquire) {
            return;
        }
        self.requested
            .fetch_max(minimum.max(INITIAL_CAPACITY), Ordering::AcqRel);
        self.queued.store(true, Ordering::Release);
    }
    pub fn request<P: Provider>(&self, provider: &P, minimum: u32) -> Result<Status> {
        if minimum == 0 {
            return Err(Failure::unchanged(
                "virtual target capacity must be positive",
            ));
        }
        if self.blocked.load(Ordering::Acquire) {
            return self.inspect(provider);
        }
        self.requested
            .fetch_max(minimum.max(INITIAL_CAPACITY), Ordering::AcqRel);
        self.queued.store(true, Ordering::Release);
        if let Ok(mut state) = self.state.lock() {
            state.failure = None;
        }
        self.resume(provider)
    }
    pub fn resume<P: Provider>(&self, provider: &P) -> Result<Status> {
        let before = provider.observe()?;
        let status = self.status_for(before);
        if status.phase == Phase::Ready {
            self.complete(before);
        }
        if matches!(status.phase, Phase::Unsupported | Phase::RecoveryPending) {
            self.queued.store(false, Ordering::Release);
        }
        if self.requested.load(Ordering::Acquire) == 0
            || !matches!(status.phase, Phase::NeedsDriver | Phase::NeedsCapacity)
        {
            return Ok(status);
        }
        if self
            .running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return self.inspect(provider);
        }
        let mut flight = Flight {
            owner: self,
            finished: false,
        };
        let requested = self.requested.load(Ordering::Acquire);
        let checkpoint = || {
            let fresh = provider.observe()?;
            if fresh.recovery_pending {
                return Err(Failure::pending(
                    "an earlier driver preparation still needs verified recovery",
                ));
            }
            if !fresh.supported || !fresh.authority || fresh.busy() {
                return Err(Failure::admission_changed());
            }
            Ok(())
        };
        let operation = (|| {
            checkpoint()?;
            let package = if before.driver_present {
                None
            } else {
                Some(provider.download_pinned()?)
            };
            // Existing work may change while downloading. Recheck immediately
            // before the provider takes its exact machine-wide mutation locks.
            checkpoint()?;
            if !provider.observe()?.ready(requested) {
                provider.apply(requested, package.as_ref(), &|| provider.host_idle())?;
            }
            let after = provider.observe()?;
            if !after.ready(requested) || after.recovery_pending {
                return Err(Failure::pending(
                    "prepared virtual target capacity could not be verified",
                ));
            }
            self.complete(after);
            Ok(())
        })();
        if let Err(failure) = operation {
            if failure.outcome != FailureOutcome::AdmissionChanged {
                self.queued.store(false, Ordering::Release);
            }
            self.blocked.fetch_or(
                failure.outcome == FailureOutcome::RecoveryPending,
                Ordering::AcqRel,
            );
            self.state
                .lock()
                .map_err(|_| Failure::pending("preparation state is poisoned"))?
                .failure = (failure.outcome != FailureOutcome::AdmissionChanged).then_some(failure);
        }
        flight.finished = true;
        self.running.store(false, Ordering::Release);
        self.inspect(provider)
    }
}
struct Flight<'a> {
    owner: &'a Coordinator,
    finished: bool,
}
impl Drop for Flight<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.owner.blocked.store(true, Ordering::Release);
        }
        self.owner.running.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Fake {
        observed: Mutex<Observation>,
        downloads: AtomicUsize,
        changes: AtomicUsize,
        busy_after_download: AtomicBool,
        download_error: AtomicBool,
        pending_apply: AtomicBool,
    }
    impl Fake {
        fn missing() -> Self {
            Self {
                observed: Mutex::new(Observation {
                    supported: true,
                    authority: true,
                    ..Default::default()
                }),
                downloads: AtomicUsize::new(0),
                changes: AtomicUsize::new(0),
                busy_after_download: AtomicBool::new(false),
                download_error: AtomicBool::new(false),
                pending_apply: AtomicBool::new(false),
            }
        }
    }
    impl Provider for Fake {
        type Package = ();
        fn observe(&self) -> Result<Observation> {
            Ok(*self.observed.lock().unwrap())
        }
        fn download_pinned(&self) -> Result<()> {
            self.downloads.fetch_add(1, Ordering::SeqCst);
            if self.download_error.load(Ordering::SeqCst) {
                return Err(Failure::unchanged("network unavailable"));
            }
            if self.busy_after_download.load(Ordering::SeqCst) {
                self.observed.lock().unwrap().active_streams = 1;
            }
            Ok(())
        }
        fn apply(
            &self,
            requested: u32,
            package: Option<&()>,
            check: &(dyn Fn() -> Result<()> + Sync),
        ) -> Result<()> {
            check()?;
            self.changes.fetch_add(1, Ordering::SeqCst);
            if self.pending_apply.load(Ordering::SeqCst) {
                return Err(Failure::pending("driver outcome is unknown"));
            }
            let mut observed = self.observed.lock().unwrap();
            if !observed.driver_present {
                assert!(package.is_some());
            }
            observed.driver_present = true;
            observed.configured_capacity = Some(requested);
            observed.targets = requested;
            observed.available = requested;
            Ok(())
        }
    }
    #[test]
    fn read_only_status_never_downloads_or_installs() {
        let coordinator = Coordinator::default();
        let fake = Fake::missing();
        assert_eq!(
            coordinator.inspect(&fake).unwrap().phase,
            Phase::NeedsDriver
        );
        assert!(!coordinator.inspect(&fake).unwrap().queued);
        assert_eq!(fake.downloads.load(Ordering::SeqCst), 0);
        assert_eq!(fake.changes.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn first_prepare_installs_once_and_repeated_request_is_idempotent() {
        let coordinator = Coordinator::default();
        let fake = Fake::missing();
        assert_eq!(coordinator.request(&fake, 16).unwrap().phase, Phase::Ready);
        assert_eq!(coordinator.request(&fake, 16).unwrap().phase, Phase::Ready);
        assert_eq!(fake.downloads.load(Ordering::SeqCst), 1);
        assert_eq!(fake.changes.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn active_stream_or_driver_target_or_claim_queues_without_any_system_change() {
        for field in 0..3 {
            let coordinator = Coordinator::default();
            let fake = Fake::missing();
            {
                let mut state = fake.observed.lock().unwrap();
                match field {
                    0 => state.active_streams = 1,
                    1 => state.active_driver_targets = 1,
                    _ => state.claimed_targets = 1,
                }
            }
            let status = coordinator.request(&fake, 16).unwrap();
            assert_eq!(status.phase, Phase::WaitingForIdle);
            assert!(status.queued);
            assert_eq!(fake.downloads.load(Ordering::SeqCst), 0);
            assert_eq!(fake.changes.load(Ordering::SeqCst), 0);
            {
                let mut state = fake.observed.lock().unwrap();
                state.active_streams = 0;
                state.active_driver_targets = 0;
                state.claimed_targets = 0;
            }
            assert_eq!(coordinator.resume(&fake).unwrap().phase, Phase::Ready);
        }
    }
    #[test]
    fn prepared_capacity_is_usable_without_elevation_or_reload_even_while_streaming() {
        let coordinator = Coordinator::default();
        let fake = Fake::missing();
        *fake.observed.lock().unwrap() = Observation {
            supported: true,
            driver_present: true,
            targets: 16,
            available: 15,
            active_streams: 1,
            claimed_targets: 1,
            authority: false,
            ..Default::default()
        };
        assert_eq!(coordinator.request(&fake, 16).unwrap().phase, Phase::Ready);
        assert_eq!(fake.changes.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn unprivileged_foreground_needs_elevation_but_installed_service_can_prepare() {
        let coordinator = Coordinator::default();
        let fake = Fake::missing();
        fake.observed.lock().unwrap().authority = false;
        assert_eq!(
            coordinator.request(&fake, 16).unwrap().phase,
            Phase::NeedsElevation
        );
        assert_eq!(fake.downloads.load(Ordering::SeqCst), 0);
        fake.observed.lock().unwrap().authority = true;
        assert_eq!(coordinator.resume(&fake).unwrap().phase, Phase::Ready);
    }
    #[test]
    fn stream_starting_during_download_prevents_driver_mutation() {
        let coordinator = Coordinator::default();
        let fake = Fake::missing();
        fake.busy_after_download.store(true, Ordering::SeqCst);
        let status = coordinator.request(&fake, 16).unwrap();
        assert_eq!(status.phase, Phase::WaitingForIdle);
        assert!(status.queued);
        assert_eq!(fake.changes.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn network_failure_requires_a_new_request_and_never_reloads_a_driver() {
        let coordinator = Coordinator::default();
        let fake = Fake::missing();
        fake.download_error.store(true, Ordering::SeqCst);
        assert_eq!(coordinator.request(&fake, 16).unwrap().phase, Phase::Failed);
        assert_eq!(coordinator.resume(&fake).unwrap().phase, Phase::Failed);
        assert_eq!(fake.downloads.load(Ordering::SeqCst), 1);
        assert_eq!(fake.changes.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn unknown_driver_outcome_blocks_polling_and_new_requests() {
        let coordinator = Coordinator::default();
        let fake = Fake::missing();
        fake.pending_apply.store(true, Ordering::SeqCst);
        assert_eq!(
            coordinator.request(&fake, 16).unwrap().phase,
            Phase::RecoveryPending
        );
        assert_eq!(
            coordinator.request(&fake, 32).unwrap().phase,
            Phase::RecoveryPending
        );
        assert_eq!(
            coordinator.resume(&fake).unwrap().phase,
            Phase::RecoveryPending
        );
        assert_eq!(fake.changes.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn growth_is_monotonic_and_initial_reservation_is_not_a_session_limit() {
        let coordinator = Coordinator::default();
        let fake = Fake::missing();
        assert_eq!(coordinator.request(&fake, 32).unwrap().targets, 32);
        assert_eq!(
            coordinator.request(&fake, 16).unwrap().requested_capacity,
            32
        );
        assert_eq!(fake.changes.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn concurrent_requests_share_the_same_accepted_preparation() {
        use std::sync::{mpsc, Barrier};
        struct Blocking {
            inner: Fake,
            entered: Mutex<Option<mpsc::Sender<()>>>,
            release: Barrier,
        }
        impl Provider for Blocking {
            type Package = ();
            fn observe(&self) -> Result<Observation> {
                self.inner.observe()
            }
            fn download_pinned(&self) -> Result<()> {
                if let Some(entered) = self.entered.lock().unwrap().take() {
                    entered.send(()).unwrap();
                }
                self.release.wait();
                self.inner.download_pinned()
            }
            fn apply(
                &self,
                capacity: u32,
                package: Option<&()>,
                check: &(dyn Fn() -> Result<()> + Sync),
            ) -> Result<()> {
                self.inner.apply(capacity, package, check)
            }
        }
        let coordinator = Coordinator::default();
        let (entered, observed) = mpsc::channel();
        let provider = Blocking {
            inner: Fake::missing(),
            entered: Mutex::new(Some(entered)),
            release: Barrier::new(2),
        };
        std::thread::scope(|scope| {
            let first = scope.spawn(|| coordinator.request(&provider, 16));
            observed
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap();
            assert_eq!(
                coordinator.request(&provider, 16).unwrap().phase,
                Phase::Preparing
            );
            provider.release.wait();
            assert_eq!(first.join().unwrap().unwrap().phase, Phase::Ready);
        });
        assert_eq!(provider.inner.downloads.load(Ordering::SeqCst), 1);
        assert_eq!(provider.inner.changes.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn panic_after_admission_is_unknown_and_cannot_start_another_mutation() {
        struct Panicking(Fake);
        impl Provider for Panicking {
            type Package = ();
            fn observe(&self) -> Result<Observation> {
                self.0.observe()
            }
            fn download_pinned(&self) -> Result<()> {
                self.0.download_pinned()
            }
            fn apply(
                &self,
                _: u32,
                _: Option<&()>,
                _: &(dyn Fn() -> Result<()> + Sync),
            ) -> Result<()> {
                self.0.changes.fetch_add(1, Ordering::SeqCst);
                panic!("simulated driver worker panic")
            }
        }
        let coordinator = Coordinator::default();
        let provider = Panicking(Fake::missing());
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || coordinator.request(&provider, 16)
        ))
        .is_err());
        assert_eq!(
            coordinator.inspect(&provider).unwrap().phase,
            Phase::RecoveryPending
        );
        assert_eq!(
            coordinator.request(&provider, 32).unwrap().phase,
            Phase::RecoveryPending
        );
        assert_eq!(provider.0.changes.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn explicit_recovery_holds_admission_and_never_repeats_an_unknown_install() {
        let coordinator = Coordinator::default();
        let fake = Fake::missing();
        fake.pending_apply.store(true, Ordering::SeqCst);
        assert_eq!(
            coordinator.request(&fake, 16).unwrap().phase,
            Phase::RecoveryPending
        );
        fake.observed.lock().unwrap().active_streams = 1;
        let refused = coordinator
            .recover(&fake, || panic!("recovery must wait for idle"))
            .unwrap();
        assert_eq!(refused.phase, Phase::RecoveryPending);
        assert_eq!(fake.changes.load(Ordering::SeqCst), 1);
        fake.observed.lock().unwrap().active_streams = 0;
        let recovered = coordinator
            .recover(&fake, || {
                assert!(coordinator.in_flight());
                Ok(())
            })
            .unwrap();
        assert_eq!(recovered.phase, Phase::NeedsDriver);
        assert!(!coordinator.queued());
        assert_eq!(
            fake.changes.load(Ordering::SeqCst),
            1,
            "recovery alone cannot replay installation"
        );
        fake.pending_apply.store(false, Ordering::SeqCst);
        assert_eq!(coordinator.request(&fake, 16).unwrap().phase, Phase::Ready);
    }
}
