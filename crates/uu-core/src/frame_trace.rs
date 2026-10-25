//! Bounded, same-host frame observations. API return is never GPU/display completion.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use uu_capture::frame_info::FrameInfo;

const CAPACITY: usize = 512;
const PENDING: usize = 128;
const RECENT: usize = 12;
pub const EVENTS: [&str; 9] = [
    "capture_start",
    "capture_ready",
    "encode_call_start",
    "encode_call_return",
    "au_ready",
    "enqueue_return",
    "send_dequeue",
    "first_rtp_write_return",
    "last_rtp_write_return",
];
const CAPABILITY: &str = "a=x-eclipse-frame-trace:2";
const LABEL: &str = "eclipse-frame-trace-v2";
const LIFETIME_MS: u16 = 100;
const MAX_JSON: usize = 64 * 1024;

pub(crate) fn accepts_channel(channel: &webrtc::data_channel::RTCDataChannel) -> bool {
    channel.label() == LABEL
        && !channel.ordered()
        && channel.max_packet_lifetime() == LIFETIME_MS
        && channel.max_retransmits() == 0
}

pub(crate) fn host_channel(
    channel: Arc<webrtc::data_channel::RTCDataChannel>,
    trace: Arc<HostTrace>,
) {
    let weak = Arc::downgrade(&channel);
    channel.on_open(Box::new(move || {
        Box::pin(async move {
            tokio::spawn(async move {
                use webrtc::data_channel::data_channel_state::RTCDataChannelState;
                let mut timer = tokio::time::interval(Duration::from_secs(1));
                timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    timer.tick().await;
                    let Some(channel) = weak.upgrade() else {
                        break;
                    };
                    if channel.ready_state() != RTCDataChannelState::Open {
                        break;
                    }
                    if channel.buffered_amount().await > 0 {
                        continue;
                    }
                    let snapshot = trace.clone();
                    let Ok(Some(report)) =
                        tokio::task::spawn_blocking(move || snapshot.report()).await
                    else {
                        continue;
                    };
                    let Ok(data) = serde_json::to_vec(&report) else {
                        continue;
                    };
                    if data.len() > MAX_JSON {
                        continue;
                    }
                    let _ =
                        tokio::time::timeout(Duration::from_millis(20), channel.send(&data.into()))
                            .await;
                }
            });
        })
    }));
}

pub(crate) async fn viewer_channel(
    pc: &webrtc::peer_connection::RTCPeerConnection,
    stats: Arc<crate::media::MediaStats>,
) -> anyhow::Result<Arc<webrtc::data_channel::RTCDataChannel>> {
    use std::sync::atomic::Ordering;
    let options = webrtc::data_channel::data_channel_init::RTCDataChannelInit {
        ordered: Some(false),
        max_packet_life_time: Some(LIFETIME_MS),
        ..Default::default()
    };
    let channel = pc.create_data_channel(LABEL, Some(options)).await?;
    let epoch = stats.native_media_epoch.load(Ordering::Acquire);
    channel.on_message(Box::new(move |message| {
        if message.data.len() <= MAX_JSON
            && stats.native_media_epoch.load(Ordering::Acquire) == epoch
        {
            if let Ok(report) = serde_json::from_slice::<Report>(&message.data) {
                if report.valid() {
                    receive_report(&stats, epoch, report);
                }
            }
        }
        Box::pin(async {})
    }));
    Ok(channel)
}

fn receive_report(stats: &crate::media::MediaStats, epoch: u64, report: Report) -> bool {
    use std::sync::atomic::Ordering;
    // Build JSON outside the slot lock. A reconnect may retire this callback
    // while it is parsing or serializing, so check again at the write boundary.
    let diagnostic =
        serde_json::json!({"event":"host_frame_trace","client_media_epoch":epoch,"report":report});
    let mut slot = stats.native_frame_trace.lock().unwrap();
    if stats.native_media_epoch.load(Ordering::Acquire) != epoch {
        return false;
    }
    if slot.as_ref().is_some_and(|previous| {
        previous.epoch != report.epoch
            || previous.context.generation > report.context.generation
            || previous.window_end_us >= report.window_end_us
    }) {
        return false;
    }
    *slot = Some(report);
    drop(slot);
    stats.diagnostics.record(diagnostic);
    true
}

pub(crate) fn offered(
    description: &webrtc::peer_connection::sdp::session_description::RTCSessionDescription,
) -> bool {
    let mut attributes = description
        .sdp
        .lines()
        .filter(|line| line.starts_with("a=x-eclipse-frame-trace:"));
    attributes.next() == Some(CAPABILITY) && attributes.next().is_none()
}
pub(crate) fn advertise(
    description: &mut webrtc::peer_connection::sdp::session_description::RTCSessionDescription,
) {
    description.sdp.push_str(CAPABILITY);
    description.sdp.push_str("\r\n");
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Token {
    id: u64,
    generation: u64,
}
#[derive(Clone, Copy)]
pub(crate) enum Event {
    EncodeReturn = 3,
    EnqueueReturn = 5,
    Dequeue = 6,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    /// Configured target, not measured unique source cadence or driver readback.
    pub fps: u32,
    /// Configured target; effective driver properties remain a separate probe.
    pub bitrate_kbps: u32,
    pub codec: uu_proto::media::VideoCodec,
    pub hdr: bool,
    pub backend: String,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub input_id: u64,
    pub output_id: Option<u64>,
    pub generation: u64,
    pub pts_us: u64,
    pub source: Option<FrameInfo>,
    pub gpu_input: bool,
    /// Actual source-host QPC sampled at the last RTP write API return.
    pub last_rtp_qpc: Option<uu_capture::frame_info::SourceTime>,
    pub keyframe: bool,
    pub rtp_timestamp: Option<u32>,
    /// Local monotonic offsets; positions are specified by `EVENTS`, schema v1.
    pub events_us: [Option<u64>; 9],
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Counts {
    pub captured: u64,
    pub new_source: u64,
    pub repeated_source: u64,
    pub unknown_source: u64,
    pub outputs: u64,
    pub sent: u64,
    pub ring_evictions: u64,
    pub pending_evictions: u64,
    pub retired_inputs: u64,
    pub unmatched_outputs: u64,
    pub source_age_retired: u64,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Distribution {
    pub samples: u32,
    pub mean_us: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub p999_us: u64,
    pub max_us: u64,
}
impl Distribution {
    fn from_values(mut values: Vec<u64>) -> Option<Self> {
        if values.is_empty() {
            return None;
        }
        values.sort_unstable();
        let n = values.len();
        let rank = |p: usize, q: usize| values[(n * p).div_ceil(q) - 1];
        Some(Self {
            samples: n as u32,
            mean_us: (values.iter().map(|v|u128::from(*v)).sum::<u128>()/n as u128).min(u128::from(u64::MAX)) as u64,
            p50_us: rank(50, 100),
            p95_us: rank(95, 100),
            p99_us: rank(99, 100),
            p999_us: rank(999, 1000),
            max_us: values[n - 1],
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub version: u8,
    pub epoch: u64,
    pub clock_domain: ClockDomain,
    pub context: Context,
    pub window_start_us: u64,
    pub window_end_us: u64,
    pub window_frames: u32,
    pub counts: Counts,
    /// Only same-clock intervals; no network one-way or actual display latency.
    pub stages: BTreeMap<String, Distribution>,
    pub source_age: SourceAgeWindow,
    /// Latest bounded observations around a slow frame, otherwise latest frames.
    pub frames: Vec<Frame>,
    pub slow_input_id: Option<u64>,
}
/// Bounded, replayable same-host samples from the current trace generation.
/// Each tuple: input id, source generation, source frame id, present QPC,
/// last-write QPC, QPC frequency. No cross-machine clock subtraction.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAgeWindow {
    pub raw_qpc: Vec<[u64;6]>,
    pub unknown_source: u32,
    pub repeated_source: u32,
    pub invalid_clock: u32,
    pub retired_generation:u64,
}
pub(crate) fn source_clock() -> Option<uu_capture::frame_info::SourceTime> {
    #[cfg(windows)] {
        use windows_sys::Win32::System::Performance::{QueryPerformanceCounter,QueryPerformanceFrequency};
        let (mut ticks,mut hz)=(0i64,0i64);
        if unsafe{QueryPerformanceCounter(&mut ticks)}==0 || unsafe{QueryPerformanceFrequency(&mut hz)}==0{return None;}
        Some(uu_capture::frame_info::SourceTime{qpc_ticks:u64::try_from(ticks).ok().filter(|v|*v>0)?,frequency_hz:u64::try_from(hz).ok().filter(|v|*v>0)?})
    }
    #[cfg(not(windows))] {None}
}
fn source_interval(source: uu_capture::frame_info::SourceTime, end: uu_capture::frame_info::SourceTime) -> Option<u64> {
    if source.frequency_hz==0 || source.frequency_hz!=end.frequency_hz || source.qpc_ticks==0 {return None;}
    let ticks=end.qpc_ticks.checked_sub(source.qpc_ticks)?;
    u64::try_from(u128::from(ticks)*1_000_000/u128::from(source.frequency_hz)).ok()
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockDomain {
    HostMonotonic,
}
const INTERVALS: [(&str, usize, usize); 9] = [
    ("capture_call", 0, 1),
    ("capture_slot_wait", 1, 2),
    ("encode_call", 2, 3),
    ("ready_to_au", 1, 4),
    ("au_to_dequeue", 4, 6),
    ("ready_to_dequeue", 1, 6),
    ("dequeue_to_first_rtp", 6, 7),
    ("rtp_write_span", 7, 8),
    ("ready_to_last_rtp", 1, 8),
];
impl Report {
    pub(crate) fn valid(&self) -> bool {
        let max_time = self.window_end_us;
        self.version == 2
            && self.epoch != 0
            && self.context.generation > 0
            && (2..=8192).contains(&self.context.width)
            && (2..=8192).contains(&self.context.height)
            && (1..=240).contains(&self.context.fps)
            && self.context.backend.len() <= 256
            && self.window_start_us <= max_time
            && self.window_frames <= CAPACITY as u32
            && self.frames.len() <= RECENT
            && self.source_age.raw_qpc.len()<=CAPACITY
            && self.source_age.raw_qpc.iter().all(|v|v[0]>0&&v[1]>0&&v[2]>0&&v[3]>0&&v[4]>=v[3]&&v[5]>0)
            && self.stages.len() <= INTERVALS.len()+1
            && self.stages.iter().all(|(name, d)| {
                (name=="source_present_to_last_rtp" || INTERVALS.iter().any(|(known, ..)| name == known))
                    && (1..=CAPACITY as u32).contains(&d.samples)
                    && d.p50_us <= d.p95_us
                    && d.p95_us <= d.p99_us
                    && d.p99_us <= d.p999_us
                    && d.p999_us <= d.max_us
                    && (name=="source_present_to_last_rtp" || d.max_us <= max_time)
            })
            && self.frames.iter().all(|f| {
                f.input_id > 0
                    && f.generation == self.context.generation
                    && f.events_us.iter().flatten().all(|at| *at <= max_time)
                    && f.events_us[0].is_some()
                    && f.events_us[1].is_some()
                    && [
                        (0, 1),
                        (1, 2),
                        (2, 3),
                        (1, 4),
                        (4, 5),
                        (4, 6),
                        (6, 7),
                        (7, 8),
                    ]
                    .iter()
                    .all(|&(a, b)| match (f.events_us[a], f.events_us[b]) {
                        (Some(a), Some(b)) => a <= b,
                        _ => true,
                    })
                    && f.source.is_none_or(|s| {
                        s.frame_id > 0
                            && s.generation > 0
                            && s.present
                                .is_none_or(|p| p.qpc_ticks > 0 && p.frequency_hz > 0)
                    })
            })
    }
}
struct State {
    context: Option<Context>,
    rows: VecDeque<Frame>,
    pending: VecDeque<(u64, Token)>,
    counts: Counts,
    slow: Option<u64>,
    continuous:Option<VecDeque<Frame>>,
    continuous_evicted:u64,
}
pub(crate) struct HostTrace {
    enabled: bool,
    continuous_limit:usize,
    started: Instant,
    epoch: u64,
    state: Mutex<State>,
}
impl Default for HostTrace {
    fn default() -> Self {
        Self::new(std::env::var("ECLIPSE_FRAME_TRACE").as_deref() != Ok("0"))
    }
}
impl HostTrace {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            continuous_limit:crate::pipeline_trace::diagnostic_limit(),
            started: Instant::now(),
            epoch: rand::random::<u64>().max(1),
            state: Mutex::new(State {
                context: None,
                rows: VecDeque::with_capacity(if enabled { CAPACITY } else { 0 }),
                pending: VecDeque::with_capacity(if enabled { PENDING } else { 0 }),
                counts: Counts::default(),
                slow: None,
                continuous:std::env::var("ECLIPSE_CONTINUOUS_TRACE").is_ok_and(|v|v=="1").then(VecDeque::new),
                continuous_evicted:0,
            }),
        }
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    pub fn configure(&self, settings: uu_media::encoder::Settings, backend: &str) {
        if !self.enabled {
            return;
        }
        let mut state = self.state.lock().unwrap();
        let generation = state.context.as_ref().map_or(1, |c| c.generation + 1);
        state.counts.retired_inputs += state.pending.len() as u64;
        state.pending.clear();
        let mut label = backend.chars().take(256).collect::<String>();
        while label.len() > 256 {
            label.pop();
        }
        state.context = Some(Context {
            generation,
            width: settings.width,
            height: settings.height,
            fps: settings.fps,
            bitrate_kbps: settings.bitrate_kbps,
            codec: settings.codec,
            hdr: settings.hdr,
            backend: label,
        });
        state.slow = None;
    }
    fn offset(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.started)
            .as_micros()
            .min(u128::from(u64::MAX)) as u64
    }
    pub fn captured(
        &self,
        pts: u64,
        source: Option<FrameInfo>,
        gpu: bool,
        started: Instant,
        ready: Instant,
        encode_start: Instant,
    ) -> Option<Token> {
        if !self.enabled {
            return None;
        }
        let mut state = self.state.lock().unwrap();
        let generation = state.context.as_ref()?.generation;
        state.counts.captured += 1;
        match source {
            None => state.counts.unknown_source += 1,
            Some(s) if s.duplicate => state.counts.repeated_source += 1,
            Some(_) => state.counts.new_source += 1,
        }
        let token = Token {
            id: state.counts.captured,
            generation,
        };
        if state.rows.len() == CAPACITY {
            state.rows.pop_front();
            state.counts.ring_evictions += 1;
        }
        let mut times = [None; 9];
        times[0] = Some(self.offset(started));
        times[1] = Some(self.offset(ready));
        times[2] = Some(self.offset(encode_start));
        state.rows.push_back(Frame {
            input_id: token.id,
            output_id: None,
            generation,
            pts_us: pts,
            source,
            gpu_input: gpu,
            last_rtp_qpc: None,
            keyframe: false,
            rtp_timestamp: None,
            events_us: times,
        });
        if state.pending.len() == PENDING {
            state.pending.pop_front();
            state.counts.pending_evictions += 1;
        }
        state.pending.push_back((pts, token));
        Some(token)
    }
    fn row(state: &mut State, token: Token) -> Option<&mut Frame> {
        let base = state.rows.front()?.input_id;
        let row = state
            .rows
            .get_mut(usize::try_from(token.id.checked_sub(base)?).ok()?)?;
        (row.input_id == token.id && row.generation == token.generation).then_some(row)
    }
    pub fn mark(&self, token: Option<Token>, event: Event, at: Instant) {
        let Some(token) = token else {
            return;
        };
        let mut state = self.state.lock().unwrap();
        if let Some(row) = Self::row(&mut state, token) {
            row.events_us[event as usize].get_or_insert(self.offset(at));
        }
    }
    pub fn output(&self, pts: u64, keyframe: bool, at: Instant) -> Option<Token> {
        if !self.enabled {
            return None;
        }
        let mut state = self.state.lock().unwrap();
        state.counts.outputs += 1;
        let Some(index) = state
            .pending
            .iter()
            .position(|(pending, _)| *pending == pts)
        else {
            state.counts.unmatched_outputs += 1;
            return None;
        };
        let (_, token) = state.pending.remove(index).unwrap();
        let output_id = state.counts.outputs;
        if let Some(row) = Self::row(&mut state, token) {
            row.events_us[4] = Some(self.offset(at));
            row.output_id = Some(output_id);
            row.keyframe = keyframe;
        }
        Some(token)
    }
    pub fn sent(&self, token: Option<Token>, timestamp: u32, first: Instant, last: Instant, last_qpc: Option<uu_capture::frame_info::SourceTime>) {
        if !self.enabled {
            return;
        }
        let mut state = self.state.lock().unwrap();
        state.counts.sent += 1;
        let Some(token) = token else {
            return;
        };
        if state.context.as_ref().is_none_or(|c|c.generation!=token.generation){state.counts.source_age_retired+=1;return;}
        let threshold = state
            .context
            .as_ref()
            .map_or(20_000, |c| (2_000_000 / u64::from(c.fps)).max(20_000));
        if let Some(row) = Self::row(&mut state, token) {
            row.events_us[7] = Some(self.offset(first));
            row.events_us[8] = Some(self.offset(last));
            row.rtp_timestamp = Some(timestamp);
            row.last_rtp_qpc = last_qpc;
            // Detect a slow processing stage, not a static source's intentional age.
            let slow = [(0, 1), (2, 3), (1, 6)].iter().any(|&(a, b)| {
                row.events_us[a]
                    .zip(row.events_us[b])
                    .is_some_and(|(a, b)| b.saturating_sub(a) > threshold)
            });
            let completed=*row;
            if slow {
                state.slow = Some(token.id);
            }
            if let Some(rows)=&mut state.continuous {
                let full=rows.len()==self.continuous_limit;
                if full{rows.pop_front();}
                rows.push_back(completed);
                if full{state.continuous_evicted+=1;}
            }
        }
    }
    pub fn continuous_report(&self)->Option<serde_json::Value> {
        let state=self.state.lock().unwrap();let rows=state.continuous.as_ref()?.clone();
        let (captured,sent,evicted)=(state.counts.captured,state.counts.sent,state.continuous_evicted);drop(state);
        Some(serde_json::json!({"qpc_anchor":crate::socket_trace::clock_anchor(self.started),"clock":"host_monotonic","epoch":self.epoch,"events":EVENTS,"limit":self.continuous_limit,"captured":captured,"sent":sent,"evicted":evicted,"frames":rows}))
    }
    /// Called off the media hot path. Clone under the lock; sort/serialize after releasing it.
    pub fn report(&self) -> Option<Report> {
        if !self.enabled {
            return None;
        }
        let (context, rows, counts, slow) = {
            let state = self.state.lock().unwrap();
            (
                state.context.clone()?,
                state.rows.iter().copied().collect::<Vec<_>>(),
                state.counts,
                state.slow,
            )
        };
        let end = self.offset(Instant::now());
        let rows: Vec<_> = rows
            .into_iter()
            .filter(|f| f.generation == context.generation)
            .collect();
        let slow = slow.filter(|id| rows.iter().any(|frame| frame.input_id == *id));
        let mut stages = BTreeMap::new();
        for (name, a, b) in INTERVALS {
            let values = rows
                .iter()
                .filter_map(|f| {
                    f.events_us[a]
                        .zip(f.events_us[b])
                        .and_then(|(a, b)| b.checked_sub(a))
                })
                .collect();
            if let Some(d) = Distribution::from_values(values) {
                stages.insert(name.to_owned(), d);
            }
        }
        let mut source_age=SourceAgeWindow{retired_generation:counts.source_age_retired,..Default::default()};
        let mut values=Vec::new();
        for row in rows.iter().filter(|row|row.events_us[8].is_some()) {
            match row.source {
                None=>source_age.unknown_source+=1,
                Some(info) if info.duplicate=>source_age.repeated_source+=1,
                Some(info)=>match info.present.zip(row.last_rtp_qpc).and_then(|(p,e)|source_interval(p,e).map(|us|(p,e,us))) {
                    Some((p,e,us))=>{source_age.raw_qpc.push([row.input_id,info.generation,info.frame_id,p.qpc_ticks,e.qpc_ticks,p.frequency_hz]);values.push(us);},
                    None=>source_age.invalid_clock+=1,
                },
            }
        }
        if let Some(d)=Distribution::from_values(values){stages.insert("source_present_to_last_rtp".into(),d);}
        let mut frames = Vec::new();
        if let Some(index) = slow.and_then(|id| rows.iter().position(|f| f.input_id == id)) {
            // Keep six observations before the trigger and up to five after it.
            let begin = index.saturating_sub(6);
            frames.extend_from_slice(&rows[begin..(begin + RECENT).min(rows.len())]);
        } else {
            frames.extend_from_slice(&rows[rows.len().saturating_sub(RECENT)..]);
        }
        Some(Report {
            version: 2,
            epoch: self.epoch,
            clock_domain: ClockDomain::HostMonotonic,
            context,
            window_start_us: rows.first().and_then(|f| f.events_us[0]).unwrap_or(end),
            window_end_us: end,
            window_frames: rows.len() as u32,
            counts,
            stages,
            source_age,
            frames,
            slow_input_id: slow,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn retired_callbacks_cannot_overwrite_a_new_session_while_waiting_for_the_slot() {
        use std::sync::atomic::Ordering;
        let stats = Arc::new(crate::media::MediaStats::default());
        stats.native_media_epoch.store(1, Ordering::Release);
        let trace = HostTrace::new(true);
        trace.configure(settings(), "fixture");
        let report = trace.report().unwrap();
        let mut slot = stats.native_frame_trace.lock().unwrap();
        let callback = stats.clone();
        let (entered, started) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            entered.send(()).unwrap();
            receive_report(&callback, 1, report)
        });
        started.recv().unwrap();
        stats.native_media_epoch.store(2, Ordering::Release);
        *slot = None;
        drop(slot);
        assert!(!worker.join().unwrap());
        assert!(stats.native_frame_trace.lock().unwrap().is_none());
        let current = HostTrace::new(true);
        current.configure(settings(), "new session");
        assert!(receive_report(&stats, 2, current.report().unwrap()));
        assert_eq!(
            stats
                .native_frame_trace
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .epoch,
            current.epoch
        );
    }
    #[test]
    fn unordered_reports_cannot_rewind_a_bound_epoch_generation_or_window() {
        use std::sync::atomic::Ordering;
        let stats = crate::media::MediaStats::default();
        stats.native_media_epoch.store(7, Ordering::Release);
        let mut trace = HostTrace::new(true);
        trace.started -= Duration::from_secs(1);
        trace.configure(settings(), "fixture");
        trace.configure(settings(), "updated");
        let report = trace.report().unwrap();
        assert!(report.valid());
        assert!(receive_report(&stats, 7, report.clone()));
        assert!(
            !receive_report(&stats, 7, report.clone()),
            "duplicate window"
        );
        let mut older = report.clone();
        older.window_end_us = 0;
        older.window_start_us = 0;
        assert!(older.valid());
        assert!(!receive_report(&stats, 7, older));
        let mut generation = report.clone();
        generation.context.generation = 1;
        generation.window_end_us += 1;
        assert!(generation.valid());
        assert!(!receive_report(&stats, 7, generation));
        let mut another = report.clone();
        another.epoch = another.epoch.wrapping_add(1).max(1);
        another.window_end_us += 1;
        assert!(!receive_report(&stats, 7, another));
        assert!(!receive_report(&stats, 6, report.clone()));
        assert_eq!(
            stats
                .native_frame_trace
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .window_end_us,
            report.window_end_us
        );
    }
    #[test]
    fn capability_requires_one_explicit_supported_version() {
        let description = |sdp: &str| {
            serde_json::from_value(serde_json::json!({"type":"offer","sdp":sdp})).unwrap()
        };
        assert!(!offered(&description("v=0\r\n")));
        assert!(!offered(&description("a=x-eclipse-frame-trace:1\r\n")));
        assert!(!offered(&description(
            "a=x-eclipse-frame-trace:2\r\na=x-eclipse-frame-trace:2\r\n"
        )));
        let mut supported = description("v=0\r\n");
        advertise(&mut supported);
        assert!(offered(&supported));
    }
    fn settings() -> uu_media::encoder::Settings {
        uu_media::encoder::Settings {
            width: 64,
            height: 64,
            fps: 60,
            bitrate_kbps: 1000,
            codec: Default::default(),
            preference: uu_media::encoder::Preference::Software,
            rate_control: Default::default(),
            tuning: Default::default(),
            hdr: false,
            source_hdr: None,
        }
    }
    #[test]
    fn continuous_sent_rows_are_bounded_and_keep_generation_identity() {
        let trace=HostTrace::new(true);
        trace.state.lock().unwrap().continuous=Some(VecDeque::new());
        trace.configure(settings(),"fixture");
        for pts in 0..8194 {
            let at=Instant::now();
            let token=trace.captured(pts,None,true,at,at,at);
            trace.output(pts,false,at);
            trace.sent(token,pts as u32,at,at,None);
        }
        trace.configure(settings(),"rebuilt");
        let at=Instant::now();let token=trace.captured(9000,None,true,at,at,at);
        trace.output(9000,false,at);trace.sent(token,9000,at,at,None);
        let report=trace.continuous_report().unwrap();
        let rows=report["frames"].as_array().unwrap();
        assert_eq!(rows.len(),8192);assert_eq!(report["evicted"],3);
        assert_eq!(rows.first().unwrap()["generation"],1);
        assert_eq!(rows.last().unwrap()["generation"],2);
        assert_eq!(report["captured"],8195);assert_eq!(report["sent"],8195);
    }
    #[test]
    fn delayed_outputs_match_pts_and_queue_delay_is_not_encode_time() {
        let trace = HostTrace::new(true);
        trace.configure(settings(), "fixture");
        let base = trace.started;
        let at = |us| base + Duration::from_micros(us);
        let first = trace
            .captured(10, None, true, at(0), at(100), at(100))
            .unwrap();
        trace.mark(Some(first), Event::EncodeReturn, at(200));
        let second = trace
            .captured(20, None, true, at(300), at(400), at(400))
            .unwrap();
        trace.mark(Some(second), Event::EncodeReturn, at(700));
        let output = trace.output(10, true, at(700));
        trace.mark(output, Event::Dequeue, at(1700));
        trace.mark(output, Event::EnqueueReturn, at(1701));
        trace.sent(output, u32::MAX, at(1800), at(1900),None);
        let report = trace.report().unwrap();
        assert_eq!(report.frames[0].output_id, Some(1));
        assert_eq!(report.frames[1].output_id, None);
        assert_eq!(report.stages["encode_call"].p50_us, 100);
        assert_eq!(report.stages["ready_to_au"].p50_us, 600);
        assert_eq!(report.stages["au_to_dequeue"].p50_us, 1000);
        assert_eq!(report.stages["ready_to_dequeue"].p50_us, 1600);
        assert_eq!(report.stages["rtp_write_span"].p50_us, 100);
        assert!(trace.output(999, false, at(2000)).is_none());
        trace.configure(settings(), "new encoder");
        trace.mark(Some(first), Event::Dequeue, at(2500));
        let next = trace.report().unwrap();
        assert!(next.frames.is_empty());
        assert_eq!(next.counts.retired_inputs, 1);
        assert_eq!(next.counts.unmatched_outputs, 1);
        assert!(next.valid());
    }
    #[test]
    fn bounded_storage_reports_overflow_and_invalid_reports_are_rejected() {
        let trace = HostTrace::new(true);
        trace.configure(settings(), "fixture");
        for index in 0..1024 {
            let now = Instant::now();
            trace.captured(index, None, false, now, now, now);
        }
        let report = trace.report().unwrap();
        assert!(report.valid());
        assert_eq!(report.counts.ring_evictions, 512);
        assert_eq!(report.counts.pending_evictions, 896);
        assert_eq!(report.counts.unknown_source, 1024);
        assert_eq!(report.frames.len(), RECENT);
        assert_eq!(report.window_frames, 512);
        assert!(serde_json::to_vec(&report).unwrap().len() < 12 * 1024);
        let mut invalid = report.clone();
        invalid.frames[0].events_us[1] = Some(u64::MAX);
        assert!(!invalid.valid());
        let mut invalid = report;
        invalid.version = 3;
        assert!(!invalid.valid());
        let off = HostTrace::new(false);
        off.configure(settings(), "fixture");
        assert!(off
            .captured(
                1,
                None,
                false,
                Instant::now(),
                Instant::now(),
                Instant::now()
            )
            .is_none());
        assert!(off.report().is_none());
    }
}

#[cfg(test)]
mod source_age_tests {
    use super::*;
    use uu_capture::frame_info::SourceTime;
    fn clock(ticks:u64,hz:u64)->SourceTime{SourceTime{qpc_ticks:ticks,frequency_hz:hz}}
    #[test]
    fn qpc_interval_checks_frequency_order_and_integer_conversion(){
        assert_eq!(source_interval(clock(100,10_000),clock(119,10_000)),Some(1900));
        assert_eq!(source_interval(clock(100,10_000),clock(99,10_000)),None);
        assert_eq!(source_interval(clock(100,10_000),clock(119,1000)),None);
        assert_eq!(source_interval(clock(1,0),clock(2,0)),None);
        assert_eq!(source_interval(clock(0,10),clock(100,10)),None);
    }
    #[test]
    fn raw_source_age_replays_and_retired_or_duplicate_inputs_do_not_enter_it(){
        let mut trace=HostTrace::new(true);trace.started-=Duration::from_secs(1);
        let settings=uu_media::encoder::Settings{width:64,height:64,fps:60,bitrate_kbps:1000,codec:Default::default(),preference:uu_media::encoder::Preference::Software,rate_control:Default::default(),tuning:Default::default(),hdr:false,source_hdr:None};
        trace.configure(settings,"fixture");let at=trace.started+Duration::from_millis(1);
        let info=FrameInfo{generation:9,frame_id:7,duplicate:false,accumulated_frames:1,present:Some(clock(1000,1_000_000)),present_age_us:Some(1)};
        let a=trace.captured(1,Some(info),true,at,at,at);trace.output(1,true,at);trace.sent(a,1,at,at,Some(clock(2900,1_000_000)));
        let repeated=FrameInfo{duplicate:true,..info};let b=trace.captured(2,Some(repeated),true,at,at,at);trace.output(2,false,at);trace.sent(b,2,at,at,Some(clock(9000,1_000_000)));
        let c=trace.captured(3,Some(info),true,at,at,at);trace.output(3,false,at);trace.sent(c,3,at,at,Some(clock(2900,2000)));
        let report=trace.report().unwrap();assert!(report.valid());assert_eq!(report.source_age.raw_qpc,vec![[1,9,7,1000,2900,1_000_000]]);
        assert_eq!(report.source_age.repeated_source,1);assert_eq!(report.source_age.invalid_clock,1);
        assert_eq!(report.stages["source_present_to_last_rtp"].mean_us,1900);assert_eq!(report.stages["source_present_to_last_rtp"].p99_us,1900);
        trace.configure(settings,"replacement");trace.sent(a,4,at,at,Some(clock(3900,1_000_000)));
        let next=trace.report().unwrap();assert!(next.source_age.raw_qpc.is_empty());assert_eq!(next.source_age.retired_generation,1);assert!(!next.stages.contains_key("source_present_to_last_rtp"));
    }
}
