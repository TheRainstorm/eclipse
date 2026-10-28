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
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
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
}

/// A renderer may supply this only after identifying the actually displayed
/// present. Both timestamps use the client's clock, never GPU completion time.
#[derive(Clone, Copy, Debug)]
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
    /// Largest half-width of any contributing same-frame latency interval.
    pub uncertainty_us: u64,
    pub method: DisplayMethod,
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
}
#[derive(Clone, Copy)]
struct Sample {
    source_id: (u64, u64, u64),
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
    pts: VecDeque<(u32, u64, FrameToken)>,
    streams: Vec<(u32, u32)>,
    samples: VecDeque<Sample>,
    receipts: VecDeque<(u64, u64, FrameToken)>,
    source_frames: u64,
    displayed_frames: u64,
    dropped: u64,
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
            pts: VecDeque::new(),
            streams: Vec::new(),
            samples: VecDeque::new(),
            receipts: VecDeque::new(),
            source_frames: 0,
            displayed_frames: 0,
            dropped: 0,
        }
    }
}

/// Session-scoped, bounded same-frame observer. It adds no media waits.
#[derive(Default)]
pub struct Client {
    supported: AtomicBool,
    media_epoch: AtomicU64,
    active_surface: AtomicU64,
    observation_epoch: AtomicU64,
    generation: AtomicU64,
    dropped: AtomicU64,
    local: LocalClock,
    state: Mutex<State>,
}
impl Client {
    pub(crate) fn begin(&self, media_epoch: u64, supported: bool) {
        self.supported.store(false, Ordering::Release);
        self.active_surface.store(0, Ordering::Release);
        let observation_epoch = self
            .observation_epoch
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        let generation = self
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        *self.state.lock().unwrap() = State {
            media_epoch,
            observation_epoch,
            generation,
            ..State::default()
        };
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
    fn change_observer(&self, media_epoch: u64, observer: Option<(u64, DisplayMethod)>) -> bool {
        if !self.supported() || self.media_epoch.load(Ordering::Acquire) != media_epoch {
            return false;
        }
        let previous_active = self.active_surface.swap(0, Ordering::AcqRel);
        let Ok(mut s) = self.state.try_lock() else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        if s.media_epoch != media_epoch {
            return false;
        }
        if observer.is_some_and(|(surface, _)| s.retired_surfaces.contains(&surface)) {
            self.active_surface
                .store(previous_active, Ordering::Release);
            return false;
        }
        let surface = observer.map(|(surface, _)| surface);
        let method = observer.map(|(_, method)| method);
        if s.surface_epoch != surface
            || s.method != method
            || (previous_active == 0 && s.surface_epoch.is_some())
        {
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
        Self::join_all(&mut s, sample.client_receive);
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
        let Some(now) = self.local.stamp() else {
            return false;
        };
        let mut s = self.state.lock().unwrap();
        if s.media_epoch != epoch || s.host_epoch.is_some_and(|host| host != host_epoch) {
            return false;
        }
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
            if let Some(record) = s.records.iter_mut().find(|record| {
                record.token.ssrc == frame.ssrc && record.token.rtp_timestamp == frame.rtp_timestamp
            }) {
                if record.source.is_none() {
                    record.source = frame.source;
                }
            } else {
                if s.records.len() == LIMIT {
                    s.records.pop_front();
                    s.dropped += 1;
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
                });
            }
        }
        Self::join_all(&mut s, now);
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
        let Ok(mut s) = self.state.try_lock() else {
            // No subsequent output may reuse an old association for equal PTS
            // after a missed SSRC/source transition.
            self.observation_epoch.fetch_add(1, Ordering::AcqRel);
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if s.media_epoch != epoch {
            return;
        }
        if s.observation_epoch != observation_epoch {
            s.observation_epoch = observation_epoch;
            s.pts.clear();
            s.streams.clear();
        }
        if let Some((_, old_ssrc)) = s.streams.iter_mut().find(|(current, _)| *current == track) {
            if *old_ssrc != ssrc {
                *old_ssrc = ssrc;
                s.pts.retain(|(current, _, _)| *current != track);
            }
        } else {
            if s.streams.len() == 32 {
                s.dropped += 1;
                return;
            }
            s.streams.push((track, ssrc));
        }
        let token = FrameToken {
            media_epoch: epoch,
            observation_epoch,
            track,
            ssrc,
            rtp_timestamp,
        };
        if s.pts.len() == LIMIT {
            s.pts.pop_front();
            s.dropped += 1;
        }
        s.pts
            .retain(|(current, previous, _)| *current != track || *previous != pts);
        s.pts.push_back((track, pts, token));
        if let Some(record) = s
            .records
            .iter_mut()
            .find(|record| record.token.ssrc == ssrc && record.token.rtp_timestamp == rtp_timestamp)
        {
            record.token = token;
        } else {
            if s.records.len() == LIMIT {
                s.records.pop_front();
                s.dropped += 1;
            }
            s.records.push_back(Record {
                token,
                source: None,
                display: None,
                sampled: false,
            });
        }
    }
    /// The renderer carries this token with the actual decoded image, including
    /// CPU RGBA fallback. Looking up a newer input instead is incorrect.
    pub fn frame_for_pts(&self, epoch: u64, track: u32, pts: u64) -> Option<FrameToken> {
        if !self.supported() || self.media_epoch.load(Ordering::Acquire) != epoch {
            return None;
        }
        let Ok(s) = self.state.try_lock() else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        if s.media_epoch != epoch
            || s.observation_epoch != self.observation_epoch.load(Ordering::Acquire)
        {
            return None;
        }
        s.pts
            .iter()
            .rev()
            .find(|(current, candidate, _)| *current == track && *candidate == pts)
            .map(|(_, _, token)| *token)
    }
    pub fn displayed(&self, token: FrameToken, display: DisplayObservation) -> bool {
        if !self.supported() {
            return false;
        }
        let Some(now) = self.local.stamp() else {
            return false;
        };
        if display.surface_epoch == 0
            || display.earliest.epoch != now.epoch
            || display.latest.epoch != now.epoch
            || display.earliest.frequency_hz != now.frequency_hz
            || display.latest.frequency_hz != now.frequency_hz
            || display.earliest.ticks > display.latest.ticks
            || display.latest.ticks > now.ticks
        {
            return false;
        }
        let Ok(mut s) = self.state.try_lock() else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        if s.media_epoch != token.media_epoch
            || token.observation_epoch != self.observation_epoch.load(Ordering::Acquire)
            || self.active_surface.load(Ordering::Acquire) != display.surface_epoch
            || s.surface_epoch != Some(display.surface_epoch)
            || s.method != Some(display.method)
            || !s.streams.contains(&(token.track, token.ssrc))
        {
            return false;
        }
        if s.receipts.iter().any(|(surface, present, _)| {
            *surface == display.surface_epoch && *present == display.present_id
        }) {
            return false;
        }
        let Some(index) = s.records.iter().position(|record| record.token == token) else {
            return false;
        };
        if s.records[index].display.is_some_and(|previous| {
            previous.surface_epoch == display.surface_epoch
                && previous.method == display.method
                && previous.earliest.ticks <= display.earliest.ticks
        }) {
            return false;
        }
        let record = &mut s.records[index];
        record.display = Some(display);
        record.sampled = false;
        if s.receipts.len() == LIMIT {
            s.receipts.pop_front();
        }
        s.receipts
            .push_back((display.surface_epoch, display.present_id, token));
        s.displayed_frames += 1;
        Self::join_all(&mut s, now);
        true
    }
    fn join_all(s: &mut State, now: WireClockStamp) {
        for record in &mut s.records {
            if record.sampled {
                continue;
            }
            let (Some(source), Some(display)) = (record.source, record.display) else {
                continue;
            };
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
            let source_id = (source.presented.epoch, source.generation, source.frame_id);
            if let Some(old) = s.samples.iter_mut().find(|old| old.source_id == source_id) {
                if first.earliest_ns >= old.display_ns {
                    continue;
                }
                *old = Sample {
                    source_id,
                    display_ns: first.earliest_ns,
                    interval,
                    method: display.method,
                };
            } else {
                if s.samples.len() == LIMIT {
                    s.samples.pop_front();
                    s.dropped += 1;
                }
                s.samples.push_back(Sample {
                    source_id,
                    display_ns: first.earliest_ns,
                    interval,
                    method: display.method,
                });
            }
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        let s = self.state.lock().unwrap();
        let now = self
            .local
            .stamp()
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
            && self.active_surface.load(Ordering::Acquire) != 0
            && Some(self.active_surface.load(Ordering::Acquire)) == s.surface_epoch;
        let measurement = method
            .filter(|_| observer_active && !samples.is_empty())
            .map(|method| {
                let mut values: Vec<_> = samples
                    .iter()
                    .map(|sample| {
                        ((sample.interval.earliest_ns + sample.interval.latest_ns) / 2 / 1000)
                            as u64
                    })
                    .collect();
                values.sort_unstable();
                Measurement {
                    samples: values.len() as u32,
                    mean_us: values.iter().sum::<u64>() / values.len() as u64,
                    p50_us: values[(values.len() * 50).div_ceil(100) - 1],
                    p95_us: values[(values.len() * 95).div_ceil(100) - 1],
                    uncertainty_us: samples
                        .iter()
                        .map(|sample| {
                            let center_us =
                                (sample.interval.earliest_ns + sample.interval.latest_ns) / 2000;
                            (sample.interval.latest_ns - center_us * 1000).div_ceil(1000) as u64
                        })
                        .max()
                        .unwrap_or(0)
                        .saturating_add(1),
                    method,
                }
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
        assert!(!client.displayed(token, display(&client, 5)));
        assert!(!client.displayed(token, display(&client, 6)));
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
        assert!(!client.displayed(client.frame_for_pts(7, 0, 200).unwrap(), receipt));
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
    fn repeated_source_first_display_is_measured_when_original_au_was_dropped() {
        let (client, source) = setup_client();
        client.decode_input(7, 0, 42, 2, 200);
        client.source_frames(7, 9, &[descriptor(source, 2, 1, true)]);
        assert!(client.displayed(
            client.frame_for_pts(7, 0, 200).unwrap(),
            display(&client, 2)
        ));
        assert_eq!(client.snapshot().measurement.unwrap().samples, 1);
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
    fn hot_hooks_drop_without_waiting_and_retire_pts_after_contention() {
        let (client, _) = setup_client();
        let client = std::sync::Arc::new(client);
        client.decode_input(7, 0, 42, 1, 100);
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
        assert!(client.frame_for_pts(7, 0, 100).is_none());
        assert_eq!(client.active_surface.load(Ordering::Acquire), 0);
        client.decode_input(7, 0, 99, 3, 200);
        assert!(client.frame_for_pts(7, 0, 100).is_none());
        assert_eq!(client.frame_for_pts(7, 0, 200).unwrap().ssrc, 99);
        assert!(client.snapshot().observations_dropped > 0);
        assert!(host.output(300).is_none());
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
        assert_eq!(client.snapshot().measurement.unwrap().samples, 1);
        assert!(!client.displayed(token, display(&client, 2)));
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
