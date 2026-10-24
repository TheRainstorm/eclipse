//! Per-session native stage measurements, independent of synchronized clocks.
//! Input confirmation ends after injection; it is not a screen-response metric.
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{Duration, Instant},
};

const WINDOW: Duration = Duration::from_secs(2);
const FRESH: Duration = Duration::from_secs(5);
const MAX_US: u32 = 10_000_000;
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VideoTiming {
    pub samples: u32,
    pub capture_us: u32,
    pub encode_us: u32,
    pub send_wait_us: u32,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HostInputTiming {
    pub samples: u32,
    pub queue_us: u32,
    pub inject_us: u32,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HostSnapshot {
    pub video: Option<VideoTiming>,
    pub input: Option<HostInputTiming>,
}
impl HostSnapshot {
    fn valid(&self) -> bool {
        self.video.map_or(true, |s| {
            (1..=120).contains(&s.samples)
                && [s.capture_us, s.encode_us, s.send_wait_us]
                    .iter()
                    .all(|v| *v <= MAX_US)
        }) && self.input.map_or(true, |s| {
            (1..=120).contains(&s.samples) && [s.queue_us, s.inject_us].iter().all(|v| *v <= MAX_US)
        })
    }
}
struct Window<const N: usize>(VecDeque<(Instant, [u32; N])>);
impl<const N: usize> Default for Window<N> {
    fn default() -> Self {
        Self(VecDeque::new())
    }
}
impl<const N: usize> Window<N> {
    fn push(&mut self, values: [u32; N]) {
        if values.iter().any(|v| *v > MAX_US) {
            return;
        }
        self.0.push_back((Instant::now(), values));
        while self.0.len() > 120 {
            self.0.pop_front();
        }
    }
    fn average(&self) -> Option<(u32, [u32; N])> {
        let now = Instant::now();
        let mut sum = [0u64; N];
        let mut n = 0u32;
        for (_, sample) in self
            .0
            .iter()
            .filter(|(at, _)| now.duration_since(*at) < WINDOW)
        {
            for i in 0..N {
                sum[i] += sample[i] as u64;
            }
            n += 1;
        }
        (n > 0).then(|| (n, sum.map(|v| (v / u64::from(n)) as u32)))
    }
}
#[derive(Default)]
pub(crate) struct HostTiming {
    pub trace: std::sync::Arc<crate::frame_trace::HostTrace>,
    video: Mutex<Window<3>>,
    input: Mutex<Window<2>>,
}
impl HostTiming {
    pub fn video(&self, capture: u64, encode: u64, wait: u64) {
        self.video
            .lock()
            .unwrap()
            .push([capture, encode, wait].map(|v| v.min(u32::MAX as u64) as u32));
    }
    pub fn input(&self, queue: u32, inject: u32) {
        self.input.lock().unwrap().push([queue, inject]);
    }
    pub fn snapshot(&self) -> HostSnapshot {
        HostSnapshot {
            video: self.video.lock().unwrap().average().map(
                |(samples, [capture_us, encode_us, send_wait_us])| VideoTiming {
                    samples,
                    capture_us,
                    encode_us,
                    send_wait_us,
                },
            ),
            input: self
                .input
                .lock()
                .unwrap()
                .average()
                .map(|(samples, [queue_us, inject_us])| HostInputTiming {
                    samples,
                    queue_us,
                    inject_us,
                }),
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct InputTiming {
    pub samples: u32,
    pub confirmation_rtt_us: u32,
    pub host_queue_us: u32,
    pub host_inject_us: u32,
}
#[derive(Default, Clone, Copy, Debug, Serialize)]
pub struct Snapshot {
    pub host: Option<HostSnapshot>,
    pub input: Option<InputTiming>,
    pub client_input_queue_us: Option<u32>,
}
#[derive(Default)]
struct ClientState {
    host: Option<(Instant, HostSnapshot)>,
    input: Window<3>,
    client_queue: Window<1>,
    pending: VecDeque<(u64, Instant)>,
    last_probe: Option<Instant>,
    sequence: u64,
}
#[derive(Default)]
pub struct ClientTiming(Mutex<ClientState>);
impl ClientTiming {
    pub(crate) fn reset(&self) {
        let mut state = self.0.lock().unwrap();
        let sequence = state.sequence;
        *state = ClientState {
            sequence,
            ..Default::default()
        };
    }
    pub(crate) fn host(&self, snapshot: HostSnapshot) {
        if snapshot.valid() {
            self.0.lock().unwrap().host = Some((Instant::now(), snapshot));
        }
    }
    pub(crate) fn client_queue(&self, queued: Instant) {
        self.0
            .lock()
            .unwrap()
            .client_queue
            .push([queued.elapsed().as_micros().min(u32::MAX as u128) as u32]);
    }
    pub(crate) fn begin_input(&self) -> Option<u64> {
        let mut state = self.0.lock().unwrap();
        let now = Instant::now();
        if !state
            .host
            .is_some_and(|(at, _)| now.duration_since(at) < FRESH)
            || state
                .last_probe
                .is_some_and(|at| now.duration_since(at) < Duration::from_millis(250))
        {
            return None;
        }
        state.sequence = state.sequence.wrapping_add(1);
        let seq = state.sequence;
        state.pending.push_back((seq, now));
        while state.pending.len() > 8 {
            state.pending.pop_front();
        }
        state.last_probe = Some(now);
        Some(seq)
    }
    pub(crate) fn applied(&self, sequence: u64, queue_us: u32, inject_us: u32) {
        let mut state = self.0.lock().unwrap();
        let Some(index) = state.pending.iter().position(|(seq, _)| *seq == sequence) else {
            return;
        };
        let (_, at) = state.pending.remove(index).unwrap();
        let elapsed = at.elapsed();
        if elapsed > Duration::from_secs(2)
            || queue_us > MAX_US
            || inject_us > MAX_US
            || u64::from(queue_us) + u64::from(inject_us) > elapsed.as_micros() as u64 + 1000
        {
            return;
        }
        state.input.push([
            elapsed.as_micros().min(u32::MAX as u128) as u32,
            queue_us,
            inject_us,
        ]);
    }
    pub fn snapshot(&self) -> Snapshot {
        let state = self.0.lock().unwrap();
        Snapshot {
            host: state
                .host
                .filter(|(at, _)| at.elapsed() < FRESH)
                .map(|(_, s)| s),
            input: state.input.average().map(
                |(samples, [confirmation_rtt_us, host_queue_us, host_inject_us])| InputTiming {
                    samples,
                    confirmation_rtt_us,
                    host_queue_us,
                    host_inject_us,
                },
            ),
            client_input_queue_us: state.client_queue.average().map(|(_, v)| v[0]),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metrics_require_fresh_valid_host_support_and_a_matching_actual_input_ack() {
        let stats = ClientTiming::default();
        assert!(stats.begin_input().is_none());
        assert!(stats.snapshot().host.is_none());
        stats.host(HostSnapshot {
            video: Some(VideoTiming {
                samples: 1,
                capture_us: 10,
                encode_us: 20,
                send_wait_us: 30,
            }),
            input: None,
        });
        let seq = stats.begin_input().unwrap();
        stats.applied(seq + 1, 1, 2);
        assert!(stats.snapshot().input.is_none());
        stats.applied(seq, 1, 2);
        let first = stats.snapshot().input.unwrap();
        assert_eq!((first.host_queue_us, first.host_inject_us), (1, 2));
        stats.applied(seq, 100, 200);
        assert_eq!(stats.snapshot().input.unwrap().samples, 1);
        stats.0.lock().unwrap().host.as_mut().unwrap().0 -= Duration::from_secs(6);
        assert!(stats.snapshot().host.is_none());
        assert!(stats.begin_input().is_none());
    }
    #[test]
    fn windows_are_bounded_and_expired_samples_are_unknown_rather_than_zero() {
        let metrics = HostTiming::default();
        assert!(metrics.snapshot().video.is_none());
        for _ in 0..130 {
            metrics.video(10, 20, 30);
        }
        assert_eq!(
            metrics.snapshot().video.unwrap(),
            VideoTiming {
                samples: 120,
                capture_us: 10,
                encode_us: 20,
                send_wait_us: 30
            }
        );
        for (at, _) in metrics.video.lock().unwrap().0.iter_mut() {
            *at -= Duration::from_secs(3);
        }
        assert!(metrics.snapshot().video.is_none());
        let stats = ClientTiming::default();
        stats.host(HostSnapshot {
            video: Some(VideoTiming {
                samples: 0,
                capture_us: 0,
                encode_us: 0,
                send_wait_us: 0,
            }),
            input: None,
        });
        assert!(stats.snapshot().host.is_none());
    }
}
