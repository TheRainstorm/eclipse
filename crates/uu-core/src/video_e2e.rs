//! Same-frame source-image to system-display observations.
//!
//! A missing source, clock mapping or display receipt stays unknown. CPU stage
//! sums, GPU fences and Present API returns are deliberately not endpoints.
pub mod clock;
mod wire;

pub use clock::{FourTimestamp, TimeInterval, WireClockStamp};
use serde::{Deserialize, Serialize};
#[cfg(not(windows))]
use std::time::Instant;
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{sync_channel, Receiver, SyncSender},
        Mutex,
    },
    time::Duration,
};

const LIMIT: usize = 512;
const WINDOW: Duration = Duration::from_secs(3);
const FRESH: Duration = Duration::from_secs(5);
// An explicit engineering drift budget, not a platform-guaranteed accuracy.
// Four-Hz probes keep extrapolation short; the snapshot exposes its full bound.
const RELATIVE_DRIFT_PPM: u32 = 1000;

#[derive(Serialize, Deserialize)]
#[serde(remote = "WireClockStamp", deny_unknown_fields)]
struct Stamp {
    epoch: u64,
    ticks: u64,
    frequency_hz: u64,
}

/// Client-side identity. It must survive all decoded-image and rendering paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameToken {
    pub media_epoch: u64,
    /// Retires PTS associations after a metadata hook is dropped under load.
    pub observation_epoch: u64,
    pub track: u32,
    pub ssrc: u32,
    pub rtp_timestamp: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEndpoint {
    pub generation: u64,
    pub frame_id: u64,
    pub duplicate: bool,
    /// Source desktop-image update time (DXGI LastPresentTime on Windows).
    /// This is independent of output capture polling/encoding timestamps.
    #[serde(with = "Stamp")]
    pub presented: WireClockStamp,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceFrame {
    pub ssrc: u32,
    pub rtp_timestamp: u32,
    pub source: Option<SourceEndpoint>,
}

/// Quality describes the *display endpoint*, independently of clock error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayMethod {
    /// A platform receipt identifies this present and its system display time.
    PlatformDisplayed,
    /// An identified displayed present has a bounded refresh-time estimate.
    RefreshInterval,
    /// Same-chain DXGI PresentCount feedback bounds a submission-to-query
    /// interval empirically. Multiple-monitor/competing-fullscreen statistics
    /// may be unreliable; this is not a platform-guaranteed display-time bound.
    DxgiReceiptInterval,
}

/// A renderer may supply this only after identifying the actually displayed
/// present. Both timestamps use the client's clock, never GPU completion time.
/// DxgiReceiptInterval preserves its API-reported, empirical quality and must
/// not be promoted to PlatformDisplayed or a guaranteed display-time bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayObservation {
    /// Changes when the swap chain/observer is recreated or becomes disjoint.
    pub surface_epoch: u64,
    pub present_id: u64,
    pub earliest: WireClockStamp,
    pub latest: WireClockStamp,
    pub method: DisplayMethod,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReason {
    UnsupportedHost,
    SourceTimestampUnavailable,
    ClockUnsynchronized,
    DisplayTimestampUnavailable,
    NoCurrentSample,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Measurement {
    pub samples: u32,
    pub mean_us: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    /// Outward-rounded bound on the mean of this same batch of intervals.
    /// This is deterministic interval arithmetic, not a confidence interval
    /// or a reduction based on independent samples. DXGI quality assumptions
    /// still apply; this is not a multiple-monitor platform guarantee.
    pub uncertainty_us: u64,
    /// Largest half-width of a contributing single-frame interval, retained
    /// separately from the mean bound. P50/P95 are midpoint quantiles.
    pub max_sample_uncertainty_us: u64,
    pub method: DisplayMethod,
}
impl Measurement {
    fn from_intervals(
        intervals: impl IntoIterator<Item = TimeInterval>,
        method: DisplayMethod,
    ) -> Option<Self> {
        let (mut sum_lower, mut sum_upper) = (0u128, 0u128);
        let mut midpoints = Vec::new();
        let mut max_sample_uncertainty_us = 0;
        for interval in intervals {
            if interval.earliest_ns > interval.latest_ns || midpoints.len() == LIMIT {
                return None;
            }
            sum_lower = sum_lower.checked_add(interval.earliest_ns)?;
            sum_upper = sum_upper.checked_add(interval.latest_ns)?;
            let center_us =
                (interval.earliest_ns + (interval.latest_ns - interval.earliest_ns) / 2) / 1000;
            midpoints.push(u64::try_from(center_us).ok()?);
            max_sample_uncertainty_us = max_sample_uncertainty_us
                .max(u64::try_from((interval.latest_ns - center_us * 1000).div_ceil(1000)).ok()?);
        }
        if midpoints.is_empty() {
            return None;
        }
        let n = midpoints.len();
        let denominator = (n as u128) * 1000;
        // The same batch's actual mean belongs to [sum_lower/n,sum_upper/n].
        // This requires no independent samples or cancellation of clock error.
        // Keep the rational endpoints until the final outward microsecond bound.
        let center_us = (sum_lower + (sum_upper - sum_lower) / 2) / denominator;
        let center_numerator = center_us.checked_mul(denominator)?;
        let uncertainty_us = sum_lower
            .abs_diff(center_numerator)
            .max(sum_upper.abs_diff(center_numerator))
            .div_ceil(denominator);
        midpoints.sort_unstable();
        Some(Self {
            samples: n as u32,
            mean_us: u64::try_from(center_us).ok()?,
            p50_us: midpoints[(n * 50).div_ceil(100) - 1],
            p95_us: midpoints[(n * 95).div_ceil(100) - 1],
            uncertainty_us: u64::try_from(uncertainty_us).ok()?,
            max_sample_uncertainty_us,
            method,
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Snapshot {
    pub generation: u64,
    /// Drift assumption used in the reported uncertainty, not an OS guarantee.
    pub relative_drift_ppm: u32,
    pub measurement: Option<Measurement>,
    pub unknown: Option<UnknownReason>,
    /// Counts refer to bounded observation coverage, not media success/failure.
    pub source_frames: u64,
    pub displayed_frames: u64,
    pub observations_dropped: u64,
    /// Capacity removals from PTS, receive, source-proof and sample histories.
    /// One frame may occupy several histories; this is not a media-frame count.
    pub history_evicted: u64,
}

pub(crate) struct LocalClock {
    epoch: u64,
    #[cfg(not(windows))]
    started: Instant,
}
impl Default for LocalClock {
    fn default() -> Self {
        Self {
            epoch: rand::random::<u64>().max(1),
            #[cfg(not(windows))]
            started: Instant::now(),
        }
    }
}
impl LocalClock {
    pub(crate) fn stamp(&self) -> Option<WireClockStamp> {
        #[cfg(windows)]
        {
            let qpc = crate::frame_trace::source_clock()?;
            Some(WireClockStamp {
                epoch: self.epoch,
                ticks: qpc.qpc_ticks,
                frequency_hz: qpc.frequency_hz,
            })
        }
        #[cfg(not(windows))]
        {
            Some(WireClockStamp {
                epoch: self.epoch,
                ticks: u64::try_from(self.started.elapsed().as_nanos())
                    .ok()?
                    .max(1),
                frequency_hz: 1_000_000_000,
            })
        }
    }
}

/// Independent of the large diagnostic trace. Enabling tracing is unnecessary.
#[derive(Default)]
pub(crate) struct Host {
    enabled: AtomicBool,
    clock: LocalClock,
    generation: AtomicU64,
    dropped: AtomicU64,
    pending: Mutex<VecDeque<(u64, u64, Option<SourceEndpoint>)>>,
    frames: Mutex<VecDeque<SourceFrame>>,
}
impl Host {
    pub(crate) fn enable(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
    }
    pub(crate) fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
    pub(crate) fn captured(&self, pts: u64, info: Option<uu_capture::frame_info::FrameInfo>) {
        if !self.enabled() {
            return;
        }
        let source = info.and_then(|info| {
            let present = info.present?;
            let now = self.clock.stamp()?;
            (present.qpc_ticks > 0 && present.frequency_hz == now.frequency_hz).then_some(
                SourceEndpoint {
                    generation: info.generation,
                    frame_id: info.frame_id,
                    duplicate: info.duplicate,
                    presented: WireClockStamp {
                        epoch: now.epoch,
                        ticks: present.qpc_ticks,
                        frequency_hz: present.frequency_hz,
                    },
                },
            )
        });
        let generation = self.generation.load(Ordering::Acquire);
        let Ok(mut pending) = self.pending.try_lock() else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        pending.retain(|(current, _, _)| *current == generation);
        if pending.len() == 128 {
            pending.pop_front();
        }
        pending.push_back((generation, pts, source));
    }
    pub(crate) fn output(&self, pts: u64) -> Option<SourceEndpoint> {
        if !self.enabled() {
            return None;
        }
        let generation = self.generation.load(Ordering::Acquire);
        let Ok(mut pending) = self.pending.try_lock() else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        let index = pending
            .iter()
            .position(|(current, input, _)| *current == generation && *input == pts)?;
        pending.remove(index)?.2
    }
    pub(crate) fn retire(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        if let Ok(mut pending) = self.pending.try_lock() {
            pending.clear();
        }
    }
    pub(crate) fn sent(&self, source: Option<SourceEndpoint>, ssrc: u32, rtp_timestamp: u32) {
        if !self.enabled() || ssrc == 0 {
            return;
        }
        let Ok(mut frames) = self.frames.try_lock() else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if frames.len() == LIMIT {
            frames.pop_front();
        }
        frames.push_back(SourceFrame {
            ssrc,
            rtp_timestamp,
            source,
        });
    }
}

#[derive(Clone, Copy)]
struct Record {
    token: FrameToken,
    source: Option<SourceEndpoint>,
    display: Option<DisplayObservation>,
    sampled: bool,
    evidence_registered: bool,
    first_display_eligible: bool,
    decoded_sequence: Option<u64>,
}

/// Decoder input/output associations have their own short ownership domain.
/// Source telemetry, clock fitting and HUD aggregation never acquire this lock.
#[derive(Default)]
struct DecodeAssociations {
    media_epoch: u64,
    observation_epoch: u64,
    streams: HashMap<u32, u32>,
    pts: HashMap<(u32, u64), (FrameToken, u64)>,
    order: VecDeque<((u32, u64), u64)>,
}

#[derive(Clone, Copy)]
enum MetadataEvent {
    Decoded {
        token: FrameToken,
        sequence: u64,
    },
    Displayed {
        token: FrameToken,
        display: DisplayObservation,
    },
}

/// try_send never waits for capacity or for the aggregate State lock. The
/// receiver is consumed only by aggregate owners, outside decoder/render hooks.
struct MetadataQueue {
    sender: SyncSender<MetadataEvent>,
    receiver: Mutex<Receiver<MetadataEvent>>,
}
impl Default for MetadataQueue {
    fn default() -> Self {
        let (sender, receiver) = sync_channel(LIMIT);
        Self {
            sender,
            receiver: Mutex::new(receiver),
        }
    }
}
type SourceId = (u64, u64, u64);
#[derive(Clone, Copy, PartialEq, Eq)]
struct FirstDisplay {
    token: FrameToken,
    display: DisplayObservation,
}
#[derive(Clone, Copy)]
struct SourceProof {
    source_id: SourceId,
    original: Option<FirstDisplay>,
    earliest_duplicate_ns: Option<u128>,
}
impl SourceProof {
    fn permits(&self, first: FirstDisplay) -> bool {
        self.original == Some(first)
            && self.earliest_duplicate_ns.is_none_or(|duplicate| {
                first
                    .display
                    .latest
                    .interval_ns()
                    .is_some_and(|latest| latest.latest_ns <= duplicate)
            })
    }
}
#[derive(Clone, Copy)]
struct Sample {
    source_id: SourceId,
    first: FirstDisplay,
    display_ns: u128,
    interval: TimeInterval,
    method: DisplayMethod,
}
struct State {
    media_epoch: u64,
    observation_epoch: u64,
    generation: u64,
    surface_epoch: Option<u64>,
    method: Option<DisplayMethod>,
    retired_surfaces: VecDeque<u64>,
    host_epoch: Option<u64>,
    clock: clock::ClockMap,
    clock_anchor: Option<WireClockStamp>,
    records: VecDeque<Record>,
    streams: Vec<(u32, u32)>,
    // Decoder sequence at the last observer/coverage barrier. A late queued
    // input must not become eligible merely because aggregation happened later.
    decode_cutoff: u64,
    samples: VecDeque<Sample>,
    source_proofs: VecDeque<SourceProof>,
    receipts: VecDeque<(u64, u64, FrameToken)>,
    source_frames: u64,
    displayed_frames: u64,
    dropped: u64,
    history_evicted: u64,
}
impl Default for State {
    fn default() -> Self {
        Self {
            media_epoch: 0,
            observation_epoch: 0,
            generation: 0,
            surface_epoch: None,
            method: None,
            retired_surfaces: VecDeque::new(),
            host_epoch: None,
            clock: clock::ClockMap::new(FRESH, RELATIVE_DRIFT_PPM),
            clock_anchor: None,
            records: VecDeque::new(),
            streams: Vec::new(),
            decode_cutoff: 0,
            samples: VecDeque::new(),
            source_proofs: VecDeque::new(),
            receipts: VecDeque::new(),
            source_frames: 0,
            displayed_frames: 0,
            dropped: 0,
            history_evicted: 0,
        }
    }
}

/// Session-scoped, bounded same-frame observer. It adds no media waits.
#[derive(Default)]
pub struct Client {
    supported: AtomicBool,
    media_epoch: AtomicU64,
    active_surface: AtomicU64,
    active_method: AtomicU64,
    observation_epoch: AtomicU64,
    decode_sequence: AtomicU64,
    qualification_cutoff: AtomicU64,
    generation: AtomicU64,
    dropped: AtomicU64,
    history_evicted: AtomicU64,
    local: LocalClock,
    associations: Mutex<DecodeAssociations>,
    metadata: MetadataQueue,
    state: Mutex<State>,
}
impl Client {
    pub(crate) fn begin(&self, media_epoch: u64, supported: bool) {
        self.supported.store(false, Ordering::Release);
        self.active_surface.store(0, Ordering::Release);
        self.active_method.store(0, Ordering::Release);
        let observation_epoch = self
            .observation_epoch
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        let generation = self
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        let cutoff = self.decode_sequence.load(Ordering::Acquire);
        self.qualification_cutoff.store(cutoff, Ordering::Release);
        let mut state = self.state.lock().unwrap();
        *state = State {
            media_epoch,
            observation_epoch,
            generation,
            decode_cutoff: cutoff,
            ..State::default()
        };
        let receiver = self.metadata.receiver.lock().unwrap();
        while receiver.try_recv().is_ok() {}
        self.media_epoch.store(media_epoch, Ordering::Release);
        self.supported.store(supported, Ordering::Release);
    }
    pub fn supported(&self) -> bool {
        self.supported.load(Ordering::Acquire)
    }
    /// Native display bridges use this epoch for QPC receipts on Windows.
    pub fn clock_stamp(&self) -> Option<WireClockStamp> {
        self.local.stamp()
    }
    fn method_code(method: DisplayMethod) -> u64 {
        match method {
            DisplayMethod::PlatformDisplayed => 1,
            DisplayMethod::RefreshInterval => 2,
            DisplayMethod::DxgiReceiptInterval => 3,
        }
    }
    fn retire_metadata(&self, observation_epoch: u64) {
        self.qualification_cutoff.fetch_max(
            self.decode_sequence.load(Ordering::Acquire),
            Ordering::AcqRel,
        );
        let _ = self.observation_epoch.compare_exchange(
            observation_epoch,
            observation_epoch.wrapping_add(1),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }
    fn enqueue(&self, event: MetadataEvent, token: FrameToken) -> bool {
        if !self.current_token(token) {
            return false;
        }
        if self.metadata.sender.try_send(event).is_err() {
            // Losing first-display coverage must not let a cached redraw stand
            // in for the missing first receipt. This is genuine queue loss,
            // independent of source/clock/HUD aggregate lock contention.
            let owner_current = match event {
                MetadataEvent::Decoded { .. } => true,
                MetadataEvent::Displayed { display, .. } => {
                    self.active_surface.load(Ordering::Acquire) == display.surface_epoch
                        && self.active_method.load(Ordering::Acquire)
                            == Self::method_code(display.method)
                }
            };
            if self.current_token(token) && owner_current {
                self.retire_metadata(token.observation_epoch);
            }
            return false;
        }
        true
    }
    fn sync_coverage(&self, s: &mut State) {
        let observation_epoch = self.observation_epoch.load(Ordering::Acquire);
        if s.observation_epoch != observation_epoch {
            s.observation_epoch = observation_epoch;
            s.decode_cutoff = s
                .decode_cutoff
                .max(self.qualification_cutoff.load(Ordering::Acquire));
            s.streams.clear();
            // Keep old source/receipt evidence for late source reports. It can
            // disqualify a later duplicate, but cannot create current samples.
            for record in &mut s.records {
                record.first_display_eligible = false;
            }
            s.samples.clear();
            s.generation = self
                .generation
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1);
        }
    }
    fn drain_metadata(&self, s: &mut State) {
        self.sync_coverage(s);
        let receiver = self.metadata.receiver.lock().unwrap();
        // The queue is bounded; arrivals while draining cannot extend this
        // operation indefinitely. Only aggregate callers own this receiver.
        for _ in 0..LIMIT {
            let Ok(event) = receiver.try_recv() else {
                break;
            };
            match event {
                MetadataEvent::Decoded { token, sequence } => {
                    if token.media_epoch != s.media_epoch
                        || token.observation_epoch != s.observation_epoch
                    {
                        continue;
                    }
                    if let Some((_, old)) = s
                        .streams
                        .iter_mut()
                        .find(|(track, _)| *track == token.track)
                    {
                        *old = token.ssrc;
                    } else if s.streams.len() < 32 {
                        s.streams.push((token.track, token.ssrc));
                    } else {
                        s.dropped += 1;
                        continue;
                    }
                    let index = s
                        .records
                        .iter()
                        .position(|record| record.token == token)
                        .or_else(|| {
                            s.records.iter().position(|record| {
                                record.token.track == u32::MAX
                                    && record.token.ssrc == token.ssrc
                                    && record.token.rtp_timestamp == token.rtp_timestamp
                            })
                        });
                    if let Some(index) = index {
                        let record = &mut s.records[index];
                        // A repeated decoder input cannot re-qualify an AU
                        // retired by an observer or coverage barrier.
                        if record.decoded_sequence.is_none() {
                            record.token = token;
                            record.decoded_sequence = Some(sequence);
                            record.first_display_eligible = sequence > s.decode_cutoff;
                        }
                    } else {
                        let source = s
                            .records
                            .iter()
                            .rev()
                            .find(|record| {
                                record.token.ssrc == token.ssrc
                                    && record.token.rtp_timestamp == token.rtp_timestamp
                            })
                            .and_then(|record| record.source);
                        if s.records.len() == LIMIT {
                            s.records.pop_front();
                            s.history_evicted += 1;
                        }
                        s.records.push_back(Record {
                            token,
                            source,
                            display: None,
                            sampled: false,
                            evidence_registered: false,
                            first_display_eligible: sequence > s.decode_cutoff,
                            decoded_sequence: Some(sequence),
                        });
                    }
                }
                MetadataEvent::Displayed { token, display } => {
                    if token.media_epoch != s.media_epoch
                        || token.observation_epoch != s.observation_epoch
                        || !s.streams.contains(&(token.track, token.ssrc))
                    {
                        continue;
                    }
                    if s.receipts.iter().any(|(surface, present, _)| {
                        *surface == display.surface_epoch && *present == display.present_id
                    }) {
                        continue;
                    }
                    let Some(record) = s.records.iter_mut().find(|record| record.token == token)
                    else {
                        continue;
                    };
                    if record
                        .display
                        .is_some_and(|old| old.earliest.ticks <= display.earliest.ticks)
                    {
                        continue;
                    }
                    record.display = Some(display);
                    record.sampled = false;
                    record.evidence_registered = false;
                    if s.receipts.len() == LIMIT {
                        s.receipts.pop_front();
                    }
                    s.receipts
                        .push_back((display.surface_epoch, display.present_id, token));
                    s.displayed_frames += 1;
                }
            }
        }
        // A producer can report genuine overflow while this batch is drained.
        // Do not publish the now-retired batch as a current measurement.
        self.sync_coverage(s);
    }
    /// The renderer declares ownership before supplying receipts. A new unique
    /// epoch is required after recreation/disjoint. A receipt never selects its
    /// own observer, so an old asynchronous callback cannot restore a surface.
    pub fn set_display_observer(
        &self,
        media_epoch: u64,
        surface_epoch: u64,
        method: DisplayMethod,
    ) -> bool {
        if surface_epoch == 0 {
            return false;
        }
        self.change_observer(media_epoch, Some((surface_epoch, method)))
    }
    /// Invalidates receipts immediately, including when metadata is contended.
    /// The renderer retries a false return to finish retiring the old metadata.
    pub fn clear_display_observer(&self, media_epoch: u64) -> bool {
        self.change_observer(media_epoch, None)
    }
    /// Retires only this renderer's observer. A late old-window destructor must
    /// not clear a replacement window's receipt. False means cleanup contended;
    /// the active observer is already invalid and the caller retries later.
    pub fn clear_display_observer_if(&self, media_epoch: u64, surface_epoch: u64) -> bool {
        if surface_epoch == 0 {
            return true;
        }
        self.change_owned_observer(media_epoch, None, Some(surface_epoch))
    }
    fn change_observer(&self, media_epoch: u64, observer: Option<(u64, DisplayMethod)>) -> bool {
        self.change_owned_observer(media_epoch, observer, None)
    }
    fn change_owned_observer(
        &self,
        media_epoch: u64,
        observer: Option<(u64, DisplayMethod)>,
        expected: Option<u64>,
    ) -> bool {
        if !self.supported() || self.media_epoch.load(Ordering::Acquire) != media_epoch {
            return expected.is_some();
        }
        let previous_active = if let Some(expected) = expected {
            match self.active_surface.compare_exchange(
                expected,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(previous) => previous,
                Err(0) => 0,
                Err(_) => return true,
            }
        } else {
            self.active_surface.swap(0, Ordering::AcqRel)
        };
        let Ok(mut s) = self.state.try_lock() else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        if s.media_epoch != media_epoch {
            return expected.is_some();
        }
        if expected.is_some_and(|surface| s.surface_epoch != Some(surface)) {
            return true;
        }
        if observer.is_some_and(|(surface, _)| s.retired_surfaces.contains(&surface)) {
            self.active_surface
                .store(previous_active, Ordering::Release);
            return false;
        }
        self.drain_metadata(&mut s);
        let surface = observer.map(|(surface, _)| surface);
        let method = observer.map(|(_, method)| method);
        if s.surface_epoch != surface
            || s.method != method
            || (previous_active == 0 && s.surface_epoch.is_some())
        {
            // A cached AU decoded under an older observer has no first-display
            // coverage in the replacement observer. Only later decoded AUs
            // may become new first-display candidates there.
            if s.surface_epoch.is_some() || !s.retired_surfaces.is_empty() {
                s.decode_cutoff = s
                    .decode_cutoff
                    .max(self.decode_sequence.load(Ordering::Acquire));
                for record in &mut s.records {
                    record.first_display_eligible = false;
                }
            }
            if let Some(previous) = s
                .surface_epoch
                .filter(|previous| Some(*previous) != surface)
            {
                if s.retired_surfaces.len() == 32 {
                    s.retired_surfaces.pop_front();
                }
                s.retired_surfaces.push_back(previous);
            }
            s.surface_epoch = surface;
            s.method = method;
            s.generation = self
                .generation
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1);
            s.samples.clear();
        }
        self.active_method
            .store(method.map_or(0, Self::method_code), Ordering::Release);
        self.active_surface
            .store(surface.unwrap_or(0), Ordering::Release);
        true
    }
    pub(crate) fn clock_sample(&self, epoch: u64, sample: FourTimestamp) -> bool {
        if !self.supported() || sample.client_receive.epoch != self.local.epoch {
            return false;
        }
        let mut s = self.state.lock().unwrap();
        if s.media_epoch != epoch
            || s.host_epoch
                .is_some_and(|host| host != sample.host_receive.epoch)
        {
            return false;
        }
        if !s.clock.sample(sample) {
            return false;
        }
        s.host_epoch = Some(sample.host_receive.epoch);
        s.clock_anchor = Some(sample.host_receive);
        self.drain_metadata(&mut s);
        if let Some(now) = self.local.stamp() {
            Self::join_all(&mut s, now);
        }
        true
    }
    pub(crate) fn source_frames(
        &self,
        epoch: u64,
        host_epoch: u64,
        frames: &[SourceFrame],
    ) -> bool {
        if !self.supported() || host_epoch == 0 {
            return false;
        }
        let mut s = self.state.lock().unwrap();
        if s.media_epoch != epoch || s.host_epoch.is_some_and(|host| host != host_epoch) {
            return false;
        }
        self.drain_metadata(&mut s);
        s.host_epoch = Some(host_epoch);
        for frame in frames {
            if frame.ssrc == 0
                || frame.source.is_some_and(|source| {
                    source.presented.epoch != host_epoch || source.presented.interval_ns().is_none()
                })
            {
                s.dropped += 1;
                continue;
            }
            s.source_frames += 1;
            // Telemetry may arrive before decode and has no local track yet.
            let mut found = false;
            for record in s.records.iter_mut().filter(|record| {
                record.token.ssrc == frame.ssrc && record.token.rtp_timestamp == frame.rtp_timestamp
            }) {
                found = true;
                if record.source.is_none() {
                    record.source = frame.source;
                }
            }
            if !found {
                if s.records.len() == LIMIT {
                    s.records.pop_front();
                    s.history_evicted += 1;
                }
                let observation_epoch = s.observation_epoch;
                s.records.push_back(Record {
                    token: FrameToken {
                        media_epoch: epoch,
                        observation_epoch,
                        track: u32::MAX,
                        ssrc: frame.ssrc,
                        rtp_timestamp: frame.rtp_timestamp,
                    },
                    source: frame.source,
                    display: None,
                    sampled: false,
                    evidence_registered: false,
                    first_display_eligible: false,
                    decoded_sequence: None,
                });
            }
        }
        if let Some(now) = self.local.stamp() {
            Self::join_all(&mut s, now);
        }
        true
    }
    /// Called for each encoded input. Output PTS may refer to a previous input.
    pub(crate) fn decode_input(
        &self,
        epoch: u64,
        track: u32,
        ssrc: u32,
        rtp_timestamp: u32,
        pts: u64,
    ) {
        if !self.supported() || ssrc == 0 || self.media_epoch.load(Ordering::Acquire) != epoch {
            return;
        }
        let observation_epoch = self.observation_epoch.load(Ordering::Acquire);
        let Ok(mut associations) = self.associations.try_lock() else {
            // This lock is private to decoder association/lifetime operations.
            // Actual missed input metadata still retires the old PTS namespace.
            self.retire_metadata(observation_epoch);
            return;
        };
        if !self.supported()
            || self.media_epoch.load(Ordering::Acquire) != epoch
            || self.observation_epoch.load(Ordering::Acquire) != observation_epoch
        {
            return;
        }
        if associations.media_epoch != epoch || associations.observation_epoch != observation_epoch
        {
            *associations = DecodeAssociations {
                media_epoch: epoch,
                observation_epoch,
                ..DecodeAssociations::default()
            };
        }
        if let Some(old_ssrc) = associations.streams.get(&track).copied() {
            if old_ssrc != ssrc {
                associations.pts.retain(|(current, _), _| *current != track);
                associations.streams.insert(track, ssrc);
            }
        } else if associations.streams.len() < 32 {
            associations.streams.insert(track, ssrc);
        } else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let Ok(sequence) = self
            .decode_sequence
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |previous| {
                previous.checked_add(1)
            })
            .map(|previous| previous + 1)
        else {
            self.retire_metadata(observation_epoch);
            return;
        };
        let token = FrameToken {
            media_epoch: epoch,
            observation_epoch,
            track,
            ssrc,
            rtp_timestamp,
        };
        // Publish aggregate registration before making the token visible to
        // decoder outputs. Keeping this bounded send in the short association
        // ownership also preserves input ordering across decoder threads.
        if !self.enqueue(MetadataEvent::Decoded { token, sequence }, token) {
            return;
        }
        // Sequence-tagged FIFO entries permit O(1) replacement and expiry even
        // when the same PTS is reused. Rare SSRC changes clear that track above.
        if associations.order.len() == LIMIT {
            if let Some((key, old_sequence)) = associations.order.pop_front() {
                if associations
                    .pts
                    .get(&key)
                    .is_some_and(|(_, current)| *current == old_sequence)
                {
                    associations.pts.remove(&key);
                    self.history_evicted.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        associations.pts.insert((track, pts), (token, sequence));
        associations.order.push_back(((track, pts), sequence));
    }
    /// The decoder output freezes this token with its actual image, including
    /// CPU RGBA fallback. Source/clock/HUD aggregation cannot block this lookup.
    pub fn frame_for_pts(&self, epoch: u64, track: u32, pts: u64) -> Option<FrameToken> {
        if !self.supported() || self.media_epoch.load(Ordering::Acquire) != epoch {
            return None;
        }
        let Ok(associations) = self.associations.try_lock() else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        if associations.media_epoch != epoch
            || associations.observation_epoch != self.observation_epoch.load(Ordering::Acquire)
        {
            return None;
        }
        associations
            .pts
            .get(&(track, pts))
            .map(|(token, _)| *token)
            .filter(|token| self.current_token(*token))
    }
    /// Lifetime check for an already frozen token. It never looks up a newer
    /// PTS mapping and does not itself prove source-first-display eligibility.
    pub fn current_token(&self, token: FrameToken) -> bool {
        self.supported.load(Ordering::Acquire)
            && self.media_epoch.load(Ordering::Acquire) == token.media_epoch
            && self.observation_epoch.load(Ordering::Acquire) == token.observation_epoch
    }
    /// Queues one immutable receipt without taking the aggregate lock. True
    /// means accepted for asynchronous association, not a new measured sample;
    /// duplicates and unavailable source evidence are resolved by aggregation.
    pub fn displayed(&self, token: FrameToken, display: DisplayObservation) -> bool {
        if !self.current_token(token)
            || display.surface_epoch == 0
            || self.active_surface.load(Ordering::Acquire) != display.surface_epoch
            || self.active_method.load(Ordering::Acquire) != Self::method_code(display.method)
        {
            return false;
        }
        let Some(now) = self.local.stamp() else {
            return false;
        };
        if display.earliest.epoch != now.epoch
            || display.latest.epoch != now.epoch
            || display.earliest.frequency_hz != now.frequency_hz
            || display.latest.frequency_hz != now.frequency_hz
            || display.earliest.ticks > display.latest.ticks
            || display.latest.ticks > now.ticks
            || self.active_surface.load(Ordering::Acquire) != display.surface_epoch
        {
            return false;
        }
        self.enqueue(MetadataEvent::Displayed { token, display }, token)
    }
    fn join_all(s: &mut State, now: WireClockStamp) {
        // First-display evidence outlives HUD/surface windows. Late telemetry
        // still describes the old display; it cannot borrow a newer redraw.
        for record in &mut s.records {
            if record.evidence_registered {
                continue;
            }
            let (Some(source), Some(display)) = (record.source, record.display) else {
                continue;
            };
            let Some(earliest) = display.earliest.interval_ns() else {
                continue;
            };
            let source_id = (source.presented.epoch, source.generation, source.frame_id);
            let index = if let Some(index) = s
                .source_proofs
                .iter()
                .position(|proof| proof.source_id == source_id)
            {
                index
            } else {
                if s.source_proofs.len() == LIMIT {
                    if let Some(old) = s.source_proofs.pop_front() {
                        s.samples.retain(|sample| sample.source_id != old.source_id);
                    }
                    s.history_evicted += 1;
                }
                s.source_proofs.push_back(SourceProof {
                    source_id,
                    original: None,
                    earliest_duplicate_ns: None,
                });
                s.source_proofs.len() - 1
            };
            let proof = &mut s.source_proofs[index];
            if source.duplicate {
                proof.earliest_duplicate_ns = Some(
                    proof
                        .earliest_duplicate_ns
                        .map_or(earliest.earliest_ns, |previous| {
                            previous.min(earliest.earliest_ns)
                        }),
                );
            } else if proof
                .original
                .is_none_or(|previous| display.earliest.ticks < previous.display.earliest.ticks)
            {
                proof.original = Some(FirstDisplay {
                    token: record.token,
                    display,
                });
            }
            let proof = *proof;
            // An earlier/overlapping duplicate receipt disproves the original
            // AU's first-display eligibility; remove even an existing sample.
            s.samples
                .retain(|sample| sample.source_id != source_id || proof.permits(sample.first));
            record.evidence_registered = true;
        }
        for record in &mut s.records {
            if record.sampled {
                continue;
            }
            let (Some(source), Some(display)) = (record.source, record.display) else {
                continue;
            };
            let source_id = (source.presented.epoch, source.generation, source.frame_id);
            let first_display = FirstDisplay {
                token: record.token,
                display,
            };
            // Capture repetition is not evidence of first system display,
            // even if the original AU was really lost. No coverage proof is
            // carried by this protocol, so duplicate AUs never create samples.
            if source.duplicate
                || !record.first_display_eligible
                || !s
                    .source_proofs
                    .iter()
                    .any(|proof| proof.source_id == source_id && proof.permits(first_display))
            {
                record.sampled = true;
                continue;
            }
            if Some(display.surface_epoch) != s.surface_epoch || Some(display.method) != s.method {
                record.sampled = true;
                continue;
            }
            let Some(start) = s.clock.map_host(source.presented, now) else {
                continue;
            };
            let (Some(first), Some(last)) =
                (display.earliest.interval_ns(), display.latest.interval_ns())
            else {
                continue;
            };
            // A delayed clock/source report must not revive an old display as
            // a current sample. The window belongs to the display endpoint.
            let Some(observed_now) = now.interval_ns() else {
                continue;
            };
            if observed_now.latest_ns.saturating_sub(first.earliest_ns) >= WINDOW.as_nanos() {
                record.sampled = true;
                continue;
            }
            if last.latest_ns < start.earliest_ns {
                continue;
            }
            let interval = TimeInterval {
                earliest_ns: first.earliest_ns.saturating_sub(start.latest_ns),
                latest_ns: last.latest_ns - start.earliest_ns,
            };
            if interval.latest_ns > 10_000_000_000 {
                continue;
            }
            record.sampled = true;
            if let Some(old) = s.samples.iter_mut().find(|old| old.source_id == source_id) {
                if first.earliest_ns >= old.display_ns {
                    continue;
                }
                *old = Sample {
                    source_id,
                    first: first_display,
                    display_ns: first.earliest_ns,
                    interval,
                    method: display.method,
                };
            } else {
                if s.samples.len() == LIMIT {
                    s.samples.pop_front();
                    s.history_evicted += 1;
                }
                s.samples.push_back(Sample {
                    source_id,
                    first: first_display,
                    display_ns: first.earliest_ns,
                    interval,
                    method: display.method,
                });
            }
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        let mut s = self.state.lock().unwrap();
        self.drain_metadata(&mut s);
        let stamp = self.local.stamp();
        if let Some(stamp) = stamp {
            Self::join_all(&mut s, stamp);
        }
        let now = stamp
            .and_then(WireClockStamp::interval_ns)
            .map(|now| now.latest_ns);
        let current = |sample: &&Sample| {
            now.is_some_and(|now| {
                now >= sample.display_ns && now - sample.display_ns < WINDOW.as_nanos()
            })
        };
        let method = s
            .samples
            .iter()
            .rev()
            .find(current)
            .map(|sample| sample.method);
        let samples: Vec<_> = s
            .samples
            .iter()
            .filter(|sample| Some(sample.method) == method && current(sample))
            .collect();
        let observer_active = self.supported()
            && s.observation_epoch == self.observation_epoch.load(Ordering::Acquire)
            && self.active_surface.load(Ordering::Acquire) != 0
            && Some(self.active_surface.load(Ordering::Acquire)) == s.surface_epoch;
        let measurement = method
            .filter(|_| observer_active && !samples.is_empty())
            .and_then(|method| {
                Measurement::from_intervals(samples.iter().map(|sample| sample.interval), method)
            });
        let unknown = if measurement.is_some() {
            None
        } else if !self.supported() {
            Some(UnknownReason::UnsupportedHost)
        } else if !s.records.iter().any(|record| record.source.is_some()) {
            Some(UnknownReason::SourceTimestampUnavailable)
        } else if !observer_active || !s.records.iter().any(|record| record.display.is_some()) {
            Some(UnknownReason::DisplayTimestampUnavailable)
        } else if self.local.stamp().is_none_or(|now| {
            s.clock_anchor
                .is_none_or(|anchor| s.clock.map_host(anchor, now).is_none())
        }) {
            Some(UnknownReason::ClockUnsynchronized)
        } else {
            Some(UnknownReason::NoCurrentSample)
        };
        Snapshot {
            generation: s.generation,
            relative_drift_ppm: RELATIVE_DRIFT_PPM,
            measurement,
            unknown,
            source_frames: s.source_frames,
            displayed_frames: s.displayed_frames,
            observations_dropped: s
                .dropped
                .saturating_add(self.dropped.load(Ordering::Relaxed)),
            history_evicted: s
                .history_evicted
                .saturating_add(self.history_evicted.load(Ordering::Relaxed)),
        }
    }
}

pub(crate) use wire::{accepts_channel, advertise, host_channel, offered, viewer_channel};

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_client() -> (Client, WireClockStamp) {
        #[allow(unused_mut)]
        let mut client = Client::default();
        #[cfg(not(windows))]
        {
            client.local.started = Instant::now() - Duration::from_secs(10);
        }
        client.begin(7, true);
        assert!(client.set_display_observer(7, 1, DisplayMethod::PlatformDisplayed));
        let send = client.clock_stamp().unwrap();
        let host = WireClockStamp {
            epoch: 9,
            ticks: send.ticks + 1_000_000,
            frequency_hz: send.frequency_hz,
        };
        let received = client.clock_stamp().unwrap();
        assert!(client.clock_sample(
            7,
            FourTimestamp {
                client_send: send,
                host_receive: host,
                host_send: host,
                client_receive: received
            }
        ));
        (client, host)
    }
    fn descriptor(
        presented: WireClockStamp,
        rtp_timestamp: u32,
        frame_id: u64,
        duplicate: bool,
    ) -> SourceFrame {
        SourceFrame {
            ssrc: 42,
            rtp_timestamp,
            source: Some(SourceEndpoint {
                generation: 1,
                frame_id,
                duplicate,
                presented,
            }),
        }
    }
    fn display(client: &Client, present_id: u64) -> DisplayObservation {
        let at = client.clock_stamp().unwrap();
        DisplayObservation {
            surface_epoch: 1,
            present_id,
            earliest: at,
            latest: at,
            method: DisplayMethod::PlatformDisplayed,
        }
    }
    fn measured(intervals: &[(u128, u128)]) -> Measurement {
        Measurement::from_intervals(
            intervals
                .iter()
                .map(|&(earliest_ns, latest_ns)| TimeInterval {
                    earliest_ns,
                    latest_ns,
                }),
            DisplayMethod::DxgiReceiptInterval,
        )
        .unwrap()
    }
    #[test]
    fn mean_bound_averages_the_same_endpoints_instead_of_the_widest_sample() {
        let m = measured(&[(10_000_000, 10_000_000), (10_000_000, 30_000_000)]);
        assert_eq!((m.mean_us, m.uncertainty_us), (15_000, 5_000));
        assert_eq!(m.max_sample_uncertainty_us, 10_000);
        assert_eq!(
            (m.p50_us, m.p95_us),
            (10_000, 20_000),
            "quantiles still describe interval midpoints"
        );
    }
    #[test]
    fn shared_clock_error_does_not_shrink_with_sample_count() {
        let one = measured(&[(10_000_000, 20_000_000)]);
        let correlated = measured(&[(10_000_000, 20_000_000); 64]);
        assert_eq!(one.mean_us, correlated.mean_us);
        assert_eq!(one.uncertainty_us, correlated.uncertainty_us);
        assert_eq!(correlated.uncertainty_us, 5_000);
    }
    #[test]
    fn mean_microsecond_rounding_contains_both_rational_endpoints() {
        // Every individual point is exact; the mean is half a microsecond.
        let m = measured(&[(0, 0), (1000, 1000)]);
        assert_eq!(
            (m.mean_us, m.uncertainty_us, m.max_sample_uncertainty_us),
            (0, 1, 0)
        );
        for a in 0..6u128 {
            for b in a..7 {
                for c in 0..6u128 {
                    for d in c..7 {
                        let intervals = [(a * 401, b * 401), (c * 599, d * 599)];
                        let m = measured(&intervals);
                        let lo = intervals.iter().map(|i| i.0).sum::<u128>();
                        let hi = intervals.iter().map(|i| i.1).sum::<u128>();
                        assert!((m.mean_us.saturating_sub(m.uncertainty_us) as u128) * 2000 <= lo);
                        assert!(((m.mean_us + m.uncertainty_us) as u128) * 2000 >= hi);
                    }
                }
            }
        }
        let at_limit = measured(&[(9_999_999_999, 10_000_000_000); LIMIT]);
        assert_eq!((at_limit.mean_us, at_limit.uncertainty_us), (9_999_999, 1));
        assert!(Measurement::from_intervals(
            [TimeInterval {
                earliest_ns: 2,
                latest_ns: 1
            }],
            DisplayMethod::PlatformDisplayed
        )
        .is_none());
        assert!(Measurement::from_intervals(
            [TimeInterval {
                earliest_ns: u128::MAX,
                latest_ns: u128::MAX
            }],
            DisplayMethod::PlatformDisplayed
        )
        .is_none());
    }
    #[test]
    fn source_history_capacity_eviction_is_not_an_invalid_observation() {
        let (client, source) = setup_client();
        for n in 1..=LIMIT + 1 {
            assert!(client.source_frames(7, 9, &[descriptor(source, n as u32, n as u64, false)]));
        }
        let snapshot = client.snapshot();
        assert_eq!(snapshot.observations_dropped, 0);
        assert_eq!(snapshot.history_evicted, 1);
        let invalid = WireClockStamp {
            epoch: 10,
            ..source
        };
        assert!(client.source_frames(7, 9, &[descriptor(invalid, 9000, 9000, false)]));
        let snapshot = client.snapshot();
        assert_eq!(snapshot.observations_dropped, 1);
        assert_eq!(snapshot.history_evicted, 1);
    }
    #[test]
    fn no_endpoint_remains_unknown_instead_of_becoming_zero_or_stage_sum() {
        let client = Client::default();
        assert_eq!(
            client.snapshot().unknown,
            Some(UnknownReason::UnsupportedHost)
        );
        client.begin(7, true);
        assert!(client.set_display_observer(7, 1, DisplayMethod::PlatformDisplayed));
        client.decode_input(7, 0, 42, 1, 100);
        assert_eq!(
            client.snapshot().unknown,
            Some(UnknownReason::SourceTimestampUnavailable)
        );
        let token = client.frame_for_pts(7, 0, 100).unwrap();
        assert!(client.displayed(token, display(&client, 1)));
        assert!(client.snapshot().measurement.is_none());
    }
    #[test]
    fn actual_output_pts_survives_reordering_rtp_wrap_and_rejects_retired_sources() {
        let client = Client::default();
        client.begin(7, true);
        client.decode_input(7, 0, 42, u32::MAX, 100);
        client.decode_input(7, 0, 42, 0, 200);
        assert_eq!(
            client.frame_for_pts(7, 0, 100).unwrap().rtp_timestamp,
            u32::MAX
        );
        assert_eq!(client.frame_for_pts(7, 0, 200).unwrap().rtp_timestamp, 0);
        client.decode_input(7, 0, 43, 10, 300);
        assert!(client.frame_for_pts(7, 0, 100).is_none());
        client.begin(8, true);
        client.decode_input(7, 0, 42, 0, 200);
        assert!(client.frame_for_pts(8, 0, 200).is_none());
        assert!(client.frame_for_pts(7, 0, 200).is_none());
    }
    #[test]
    fn telemetry_after_display_joins_the_same_frame_once_and_never_nearby_input() {
        let (client, source) = setup_client();
        client.decode_input(7, 0, 42, 88, 1200);
        let token = client.frame_for_pts(7, 0, 1200).unwrap();
        assert!(client.displayed(token, display(&client, 5)));
        assert!(client.snapshot().measurement.is_none());
        assert!(client.source_frames(7, 9, &[descriptor(source, 89, 1, false)]));
        assert!(client.snapshot().measurement.is_none());
        assert!(client.source_frames(7, 9, &[descriptor(source, 88, 2, false)]));
        let measured = client.snapshot().measurement.unwrap();
        assert_eq!(measured.samples, 1);
        assert_eq!(measured.method, DisplayMethod::PlatformDisplayed);
        assert!(client.displayed(token, display(&client, 5)));
        assert!(client.displayed(token, display(&client, 6)));
        assert!(client.source_frames(7, 9, &[descriptor(source, 88, 2, false)]));
        assert_eq!(client.snapshot().measurement.unwrap().samples, 1);
        client.begin(8, true);
        assert!(!client.source_frames(7, 9, &[descriptor(source, 88, 2, false)]));
        assert!(!client.displayed(token, display(&client, 7)));
        assert!(client.snapshot().measurement.is_none());
    }
    #[test]
    fn display_receipts_cannot_be_shared_between_frames_or_fabricated_in_future() {
        let (client, source) = setup_client();
        for (rtp, pts) in [(1, 100), (2, 200)] {
            client.decode_input(7, 0, 42, rtp, pts);
        }
        client.source_frames(
            7,
            9,
            &[
                descriptor(source, 1, 1, false),
                descriptor(source, 2, 2, false),
            ],
        );
        let receipt = display(&client, 1);
        assert!(client.displayed(client.frame_for_pts(7, 0, 100).unwrap(), receipt));
        assert!(client.displayed(client.frame_for_pts(7, 0, 200).unwrap(), receipt));
        let mut future = display(&client, 2);
        future.earliest.ticks += future.earliest.frequency_hz;
        future.latest = future.earliest;
        assert!(!client.displayed(client.frame_for_pts(7, 0, 200).unwrap(), future));
        assert_eq!(client.snapshot().measurement.unwrap().samples, 1);
    }
    #[test]
    fn repeated_capture_is_not_a_new_image_sample_and_stale_samples_expire() {
        let (client, source) = setup_client();
        for (rtp, pts, duplicate) in [(1, 100, false), (2, 200, true)] {
            client.decode_input(7, 0, 42, rtp, pts);
            client.source_frames(7, 9, &[descriptor(source, rtp, 1, duplicate)]);
            assert!(client.displayed(
                client.frame_for_pts(7, 0, pts).unwrap(),
                display(&client, u64::from(rtp))
            ));
        }
        assert_eq!(client.snapshot().measurement.unwrap().samples, 1);
        let now_ns = client
            .clock_stamp()
            .unwrap()
            .interval_ns()
            .unwrap()
            .latest_ns;
        for sample in &mut client.state.lock().unwrap().samples {
            sample.display_ns = now_ns - WINDOW.as_nanos() - 1_000_000;
        }
        assert!(client.snapshot().measurement.is_none());
    }
    #[test]
    fn duplicate_without_original_first_display_proof_stays_unknown() {
        let (client, source) = setup_client();
        client.decode_input(7, 0, 42, 2, 200);
        client.source_frames(7, 9, &[descriptor(source, 2, 1, true)]);
        assert!(client.displayed(
            client.frame_for_pts(7, 0, 200).unwrap(),
            display(&client, 2)
        ));
        assert!(client.snapshot().measurement.is_none(),"even a genuinely lost original AU does not prove a duplicate is the source's first display");
    }
    #[test]
    fn delayed_source_telemetry_does_not_revive_an_expired_display_sample() {
        let (client, mut source) = setup_client();
        client.decode_input(7, 0, 42, 1, 100);
        let mut receipt = display(&client, 1);
        receipt.earliest.ticks -= receipt.earliest.frequency_hz * 4;
        receipt.latest = receipt.earliest;
        source.ticks -= source.frequency_hz * 4;
        assert!(client.displayed(client.frame_for_pts(7, 0, 100).unwrap(), receipt));
        client.source_frames(7, 9, &[descriptor(source, 1, 1, false)]);
        assert!(client.snapshot().measurement.is_none());
    }
    #[test]
    fn host_hot_hooks_drop_and_client_mapping_survives_aggregate_contention() {
        let (client, _) = setup_client();
        let client = std::sync::Arc::new(client);
        client.decode_input(7, 0, 42, 1, 100);
        let epoch = client.observation_epoch.load(Ordering::Acquire);
        assert!(client.frame_for_pts(7, 0, 100).is_some());
        let host = std::sync::Arc::new(Host::default());
        host.enable(true);
        let at = host.clock.stamp().unwrap();
        host.captured(
            300,
            Some(uu_capture::frame_info::FrameInfo {
                generation: 1,
                frame_id: 1,
                duplicate: false,
                accumulated_frames: 1,
                present: Some(uu_capture::frame_info::SourceTime {
                    qpc_ticks: at.ticks,
                    frequency_hz: at.frequency_hz,
                }),
                present_age_us: None,
            }),
        );
        let pending = host.pending.lock().unwrap();
        let frames = host.frames.lock().unwrap();
        let state = client.state.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_host = host.clone();
        let worker_client = client.clone();
        let worker = std::thread::spawn(move || {
            worker_host.captured(400, None);
            assert!(worker_host.output(400).is_none());
            worker_host.sent(None, 42, 1);
            worker_host.retire();
            worker_client.decode_input(7, 0, 99, 2, 100);
            assert!(!worker_client.clear_display_observer(7));
            tx.send(()).unwrap();
        });
        let completed = rx.recv_timeout(Duration::from_millis(100));
        drop(state);
        drop(frames);
        drop(pending);
        worker.join().unwrap();
        assert!(
            completed.is_ok(),
            "metadata contention must not wait on media threads"
        );
        assert_eq!(client.observation_epoch.load(Ordering::Acquire), epoch);
        assert_eq!(client.frame_for_pts(7, 0, 100).unwrap().ssrc, 99);
        assert_eq!(client.active_surface.load(Ordering::Acquire), 0);
        client.decode_input(7, 0, 99, 3, 200);
        assert_eq!(client.frame_for_pts(7, 0, 100).unwrap().rtp_timestamp, 2);
        assert_eq!(client.frame_for_pts(7, 0, 200).unwrap().ssrc, 99);
        assert!(client.snapshot().observations_dropped > 0);
        assert!(host.output(300).is_none());
    }
    #[test]
    fn source_clock_or_hud_lock_cannot_retire_decoder_identity_or_delay_receipts() {
        let (client, source) = setup_client();
        let client = std::sync::Arc::new(client);
        let epoch = client.observation_epoch.load(Ordering::Acquire);
        // Exactly the aggregate ownership held by source batches, clock fitting
        // and HUD snapshots. The media/render worker must finish while held.
        let aggregate = client.state.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_client = client.clone();
        let worker = std::thread::spawn(move || {
            for n in 1..=64 {
                worker_client.decode_input(7, 0, 42, n, u64::from(n));
                let token = worker_client.frame_for_pts(7, 0, u64::from(n)).unwrap();
                assert_eq!(token.rtp_timestamp, n);
                assert_eq!(token.observation_epoch, epoch);
                assert!(worker_client.displayed(token, display(&worker_client, u64::from(n))));
            }
            tx.send(()).unwrap();
        });
        let finished = rx.recv_timeout(Duration::from_secs(1));
        drop(aggregate);
        worker.join().unwrap();
        assert!(
            finished.is_ok(),
            "aggregate work must never be media/render ownership"
        );
        assert_eq!(client.observation_epoch.load(Ordering::Acquire), epoch);
        client.source_frames(
            7,
            9,
            &(1..=64)
                .map(|n| descriptor(source, n, u64::from(n), false))
                .collect::<Vec<_>>(),
        );
        let snapshot = client.snapshot();
        assert_eq!(snapshot.displayed_frames, 64);
        assert_eq!(snapshot.measurement.unwrap().samples, 64);
        assert_eq!(snapshot.observations_dropped, 0);
    }
    #[test]
    fn genuine_decoder_association_loss_still_retires_old_equal_pts() {
        let (client, _) = setup_client();
        let client = std::sync::Arc::new(client);
        client.decode_input(7, 0, 42, 1, 100);
        let old = client.frame_for_pts(7, 0, 100).unwrap();
        let association = client.associations.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_client = client.clone();
        let worker = std::thread::spawn(move || {
            worker_client.decode_input(7, 0, 99, 2, 100);
            tx.send(()).unwrap();
        });
        let finished = rx.recv_timeout(Duration::from_secs(1));
        drop(association);
        worker.join().unwrap();
        assert!(finished.is_ok());
        assert!(!client.current_token(old));
        assert!(client.frame_for_pts(7, 0, 100).is_none());
        client.decode_input(7, 0, 99, 3, 200);
        assert!(client.frame_for_pts(7, 0, 100).is_none());
        assert_eq!(client.frame_for_pts(7, 0, 200).unwrap().ssrc, 99);
    }
    #[test]
    fn repeated_aggregate_pressure_preserves_one_epoch_and_all_queued_receipts() {
        let (client, source) = setup_client();
        let client = std::sync::Arc::new(client);
        let epoch = client.observation_epoch.load(Ordering::Acquire);
        let (go, commands) = std::sync::mpsc::channel();
        let (done, completed) = std::sync::mpsc::channel();
        let worker_client = client.clone();
        let worker = std::thread::spawn(move || {
            for batch in 0..16u32 {
                commands.recv().unwrap();
                for n in batch * 64 + 1..=batch * 64 + 64 {
                    worker_client.decode_input(7, 0, 42, n, u64::from(n));
                    let token = worker_client.frame_for_pts(7, 0, u64::from(n)).unwrap();
                    assert_eq!(token.observation_epoch, epoch);
                    assert_eq!(token.rtp_timestamp, n);
                    assert!(worker_client.displayed(token, display(&worker_client, u64::from(n))));
                }
                done.send(()).unwrap();
            }
        });
        for batch in 0..16u32 {
            let aggregate = client.state.lock().unwrap();
            go.send(()).unwrap();
            let finished = completed.recv_timeout(Duration::from_secs(1));
            drop(aggregate);
            assert!(
                finished.is_ok(),
                "media must finish a whole batch under aggregate contention"
            );
            client.source_frames(
                7,
                9,
                &(batch * 64 + 1..=batch * 64 + 64)
                    .map(|n| descriptor(source, n, u64::from(n), false))
                    .collect::<Vec<_>>(),
            );
        }
        worker.join().unwrap();
        assert_eq!(client.observation_epoch.load(Ordering::Acquire), epoch);
        let snapshot = client.snapshot();
        assert_eq!(snapshot.displayed_frames, 1024);
        assert_eq!(snapshot.observations_dropped, 0);
        assert_eq!(
            snapshot.history_evicted,
            ((1024 - LIMIT) * 3) as u64,
            "PTS, receive and source-proof histories evict independently"
        );
        assert!(snapshot.measurement.is_some());
        assert_eq!(
            client.frame_for_pts(7, 0, 1024).unwrap().rtp_timestamp,
            1024
        );
        assert!(
            client.frame_for_pts(7, 0, 1).is_none(),
            "bounded PTS retention still applies"
        );
    }
    #[test]
    fn queued_inputs_before_observer_barrier_cannot_gain_first_display_eligibility() {
        let (client, source) = setup_client();
        client.decode_input(7, 0, 42, 1, 100);
        let token = client.frame_for_pts(7, 0, 100).unwrap();
        // Do not aggregate the decode event first. A late consumer must apply
        // the capture-time sequence, not treat consumption as a new decode.
        assert!(client.set_display_observer(7, 2, DisplayMethod::PlatformDisplayed));
        let mut redraw = display(&client, 1);
        redraw.surface_epoch = 2;
        assert!(client.displayed(token, redraw));
        client.source_frames(7, 9, &[descriptor(source, 1, 1, false)]);
        assert!(client.snapshot().measurement.is_none());
        // Repeating the original encoded input must not re-qualify it either.
        client.decode_input(7, 0, 42, 1, 100);
        redraw.present_id += 1;
        assert!(client.displayed(token, redraw));
        assert!(client.snapshot().measurement.is_none());
    }
    #[test]
    fn replacing_display_observer_retires_old_generation_and_receipts() {
        let (client, source) = setup_client();
        client.decode_input(7, 0, 42, 1, 100);
        client.source_frames(7, 9, &[descriptor(source, 1, 1, false)]);
        let token = client.frame_for_pts(7, 0, 100).unwrap();
        assert!(client.displayed(token, display(&client, 1)));
        let old = client.snapshot().generation;
        let mut next = display(&client, 1);
        next.surface_epoch = 2;
        assert!(client.set_display_observer(7, 2, DisplayMethod::PlatformDisplayed));
        assert!(client.displayed(token, next));
        assert_ne!(old, client.snapshot().generation);
        assert!(client.snapshot().measurement.is_none());
        client.decode_input(7, 0, 42, 2, 200);
        client.source_frames(7, 9, &[descriptor(source, 2, 2, false)]);
        let mut fresh = display(&client, 3);
        fresh.surface_epoch = 2;
        assert!(client.displayed(client.frame_for_pts(7, 0, 200).unwrap(), fresh));
        assert_eq!(client.snapshot().measurement.unwrap().samples, 1);
        assert!(!client.displayed(token, display(&client, 2)));
    }
    #[test]
    fn late_old_window_cleanup_does_not_clear_the_replacement_observer() {
        let (client, _) = setup_client();
        assert!(client.set_display_observer(7, 2, DisplayMethod::DxgiReceiptInterval));
        let generation = client.generation.load(Ordering::Acquire);
        let state = client.state.lock().unwrap();
        assert!(
            client.clear_display_observer_if(7, 1),
            "stale cleanup must not need the state lock"
        );
        assert_eq!(client.active_surface.load(Ordering::Acquire), 2);
        assert_eq!(client.generation.load(Ordering::Acquire), generation);
        drop(state);
        assert!(client.clear_display_observer_if(7, 2));
        assert_eq!(client.active_surface.load(Ordering::Acquire), 0);
        assert!(client.state.lock().unwrap().surface_epoch.is_none());
        assert!(!client.set_display_observer(7, 2, DisplayMethod::DxgiReceiptInterval));
    }
    #[test]
    fn owned_cleanup_invalidates_without_waiting_and_retries_only_its_owner() {
        let (client, _) = setup_client();
        let state = client.state.lock().unwrap();
        assert!(!client.clear_display_observer_if(7, 1));
        assert_eq!(client.active_surface.load(Ordering::Acquire), 0);
        drop(state);
        assert!(client.set_display_observer(7, 2, DisplayMethod::DxgiReceiptInterval));
        assert!(client.clear_display_observer_if(7, 1));
        assert_eq!(client.active_surface.load(Ordering::Acquire), 2);
        assert!(
            client.clear_display_observer_if(8, 2),
            "old media cleanup is already retired"
        );
        assert_eq!(client.active_surface.load(Ordering::Acquire), 2);
    }
    #[test]
    fn late_old_surface_receipt_cannot_retire_new_owner_during_contention() {
        let (client, _) = setup_client();
        assert!(client.set_display_observer(7, 2, DisplayMethod::DxgiReceiptInterval));
        client.decode_input(7, 0, 42, 2, 200);
        let token = client.frame_for_pts(7, 0, 200).unwrap();
        let receipt = display(&client, 1); // old surface 1
        let epoch = client.observation_epoch.load(Ordering::Acquire);
        let state = client.state.lock().unwrap();
        assert!(!client.displayed(token, receipt));
        assert_eq!(client.observation_epoch.load(Ordering::Acquire), epoch);
        assert!(client.current_token(token));
        assert_eq!(client.active_surface.load(Ordering::Acquire), 2);
        drop(state);
    }
    #[test]
    fn replaced_surface_cannot_retire_new_owner_when_its_enqueue_loses_the_race() {
        let (client, _) = setup_client();
        client.decode_input(7, 0, 42, 1, 100);
        let old = client.frame_for_pts(7, 0, 100).unwrap();
        let old_receipt = display(&client, 1);
        assert!(client.set_display_observer(7, 2, DisplayMethod::DxgiReceiptInterval));
        client.decode_input(7, 0, 42, 2, 200);
        let current = client.frame_for_pts(7, 0, 200).unwrap();
        client.source_frames(7, 9, &[]);
        let epoch = client.observation_epoch.load(Ordering::Acquire);
        for n in 0..LIMIT {
            let mut receipt = display(&client, n as u64);
            receipt.surface_epoch = 2;
            receipt.method = DisplayMethod::DxgiReceiptInterval;
            assert!(client.displayed(current, receipt));
        }
        // Model a callback that passed its initial guard before replacement,
        // but reaches the now-full queue only after the new owner is active.
        assert!(!client.enqueue(
            MetadataEvent::Displayed {
                token: old,
                display: old_receipt
            },
            old
        ));
        assert_eq!(client.observation_epoch.load(Ordering::Acquire), epoch);
        assert!(client.current_token(current));
        assert_eq!(client.active_surface.load(Ordering::Acquire), 2);
    }
    #[test]
    fn missing_original_observation_does_not_turn_static_frame_age_into_latency() {
        let (client, source) = setup_client();
        client.decode_input(7, 0, 42, 1, 100);
        let original = client.frame_for_pts(7, 0, 100).unwrap();
        assert!(client.displayed(original, display(&client, 1)));
        // The original was really displayed, but its source report is absent
        // and the bounded receive record is later lost before joining it.
        for n in 0..LIMIT {
            client.decode_input(7, 0, 42, 10 + n as u32, 1000 + n as u64);
            client.snapshot();
        }
        assert!(!client
            .state
            .lock()
            .unwrap()
            .records
            .iter()
            .any(|record| record.token == original));
        client.decode_input(7, 0, 42, 9000, 9000);
        client.source_frames(7, 9, &[descriptor(source, 9000, 1, true)]);
        assert!(client.displayed(
            client.frame_for_pts(7, 0, 9000).unwrap(),
            display(&client, 2)
        ));
        assert!(client.snapshot().measurement.is_none());
    }
    #[test]
    fn late_original_source_report_uses_early_display_not_a_later_duplicate() {
        let (client, source) = setup_client();
        client.decode_input(7, 0, 42, 1, 100);
        let early = display(&client, 1);
        assert!(client.displayed(client.frame_for_pts(7, 0, 100).unwrap(), early));
        client.decode_input(7, 0, 42, 2, 200);
        assert!(client.displayed(
            client.frame_for_pts(7, 0, 200).unwrap(),
            display(&client, 2)
        ));
        client.source_frames(7, 9, &[descriptor(source, 2, 1, true)]);
        assert!(client.snapshot().measurement.is_none());
        client.source_frames(7, 9, &[descriptor(source, 1, 1, false)]);
        assert_eq!(client.snapshot().measurement.unwrap().samples, 1);
        assert_eq!(
            client.state.lock().unwrap().samples[0].display_ns,
            early.earliest.interval_ns().unwrap().earliest_ns
        );
    }
    #[test]
    fn an_earlier_duplicate_receipt_with_late_metadata_revokes_false_first_display() {
        let (client, mut source) = setup_client();
        source.ticks -= source.frequency_hz / 100;
        for (rtp, pts) in [(1, 100), (2, 200)] {
            client.decode_input(7, 0, 42, rtp, pts);
        }
        client.source_frames(7, 9, &[descriptor(source, 1, 1, false)]);
        let original = display(&client, 1);
        assert!(client.displayed(client.frame_for_pts(7, 0, 100).unwrap(), original));
        assert_eq!(client.snapshot().measurement.unwrap().samples, 1);
        let mut earlier = display(&client, 2);
        earlier.earliest.ticks = original.earliest.ticks - earlier.earliest.frequency_hz / 1000;
        earlier.latest = earlier.earliest;
        assert!(client.displayed(client.frame_for_pts(7, 0, 200).unwrap(), earlier));
        client.source_frames(7, 9, &[descriptor(source, 2, 1, true)]);
        assert!(client.snapshot().measurement.is_none());
    }
    #[test]
    fn late_source_metadata_across_surfaces_cannot_borrow_a_cached_redraw() {
        let (client, source) = setup_client();
        client.decode_input(7, 0, 42, 1, 100);
        let token = client.frame_for_pts(7, 0, 100).unwrap();
        let early = display(&client, 1);
        assert!(client.displayed(token, early));
        assert!(client.set_display_observer(7, 2, DisplayMethod::PlatformDisplayed));
        let mut redraw = display(&client, 2);
        redraw.surface_epoch = 2;
        assert!(client.displayed(token, redraw));
        client.source_frames(7, 9, &[descriptor(source, 1, 1, false)]);
        assert!(client.snapshot().measurement.is_none());
        let state = client.state.lock().unwrap();
        assert_eq!(state.source_proofs[0].original.unwrap().display, early);
    }
    #[test]
    fn cached_au_without_old_receipt_is_unproven_after_observer_replacement() {
        let (client, source) = setup_client();
        client.decode_input(7, 0, 42, 1, 100);
        client.source_frames(7, 9, &[descriptor(source, 1, 1, false)]);
        let token = client.frame_for_pts(7, 0, 100).unwrap();
        assert!(client.set_display_observer(7, 2, DisplayMethod::PlatformDisplayed));
        let mut redraw = display(&client, 1);
        redraw.surface_epoch = 2;
        assert!(client.displayed(token, redraw));
        assert!(client.snapshot().measurement.is_none());
    }
    #[test]
    fn full_receipt_queue_retires_cached_token_without_querying_newer_pts() {
        let (client, source) = setup_client();
        client.decode_input(7, 0, 42, 1, 100);
        client.source_frames(7, 9, &[descriptor(source, 1, 1, false)]);
        let token = client.frame_for_pts(7, 0, 100).unwrap();
        assert!(client.current_token(token));
        let held = client.state.lock().unwrap();
        for n in 0..LIMIT {
            assert!(client.displayed(token, display(&client, n as u64)));
        }
        assert!(!client.displayed(token, display(&client, LIMIT as u64)));
        drop(held);
        assert!(!client.current_token(token));
        assert!(!client.displayed(token, display(&client, 2)));
        assert!(client.snapshot().measurement.is_none());
        client.begin(8, true);
        assert!(!client.current_token(token));
    }
    #[test]
    fn detached_or_undeclared_surface_receipts_cannot_reactivate_an_observer() {
        let (client, source) = setup_client();
        client.decode_input(7, 0, 42, 1, 100);
        client.source_frames(7, 9, &[descriptor(source, 1, 1, false)]);
        let token = client.frame_for_pts(7, 0, 100).unwrap();
        let old = display(&client, 1);
        let mut undeclared = old;
        undeclared.surface_epoch = 2;
        let generation = client.snapshot().generation;
        assert!(!client.displayed(token, undeclared));
        assert_eq!(generation, client.snapshot().generation);
        assert!(client.displayed(token, old));
        assert!(client.clear_display_observer(7));
        assert!(client.snapshot().measurement.is_none());
        assert!(!client.displayed(token, display(&client, 2)));
        assert!(!client.set_display_observer(7, 1, DisplayMethod::PlatformDisplayed));
        assert!(client.set_display_observer(7, 2, DisplayMethod::PlatformDisplayed));
        assert!(!client.displayed(token, display(&client, 3)));
    }
    #[test]
    fn source_and_clock_without_display_still_remain_unknown() {
        let (client, source) = setup_client();
        client.decode_input(7, 0, 42, 10, 100);
        client.source_frames(7, 9, &[descriptor(source, 10, 1, false)]);
        assert_eq!(
            client.snapshot().unknown,
            Some(UnknownReason::DisplayTimestampUnavailable)
        );
        assert!(client.snapshot().measurement.is_none());
    }
    #[test]
    fn host_tracks_source_by_output_pts_independently_of_diagnostic_trace() {
        let host = Host::default();
        host.enable(true);
        let now = host.clock.stamp().unwrap();
        let info = |frame_id| uu_capture::frame_info::FrameInfo {
            generation: 1,
            frame_id,
            duplicate: false,
            accumulated_frames: 1,
            present: Some(uu_capture::frame_info::SourceTime {
                qpc_ticks: now.ticks,
                frequency_hz: now.frequency_hz,
            }),
            present_age_us: None,
        };
        host.captured(100, Some(info(1)));
        host.captured(200, Some(info(2)));
        assert_eq!(host.output(100).unwrap().frame_id, 1);
        assert_eq!(host.output(200).unwrap().frame_id, 2);
        host.captured(300, Some(info(3)));
        host.retire();
        assert!(host.output(300).is_none());
    }
}
