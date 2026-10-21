//! Clock-offset-independent timing: source RTP spacing vs local arrival spacing.
use std::time::Instant;

#[derive(Clone, Default, serde::Serialize)]
pub struct Snapshot {
    /// Mean / P95 / max interval, ms; source RTP clock does not measure one-way latency.
    pub source_spacing_ms: Option<[f64; 3]>,
    pub arrival_spacing_ms: Option<[f64; 3]>,
    pub decode_queue_ms: Option<[f64; 3]>,
    pub receiver_tick_late_ms: Option<[f64; 3]>,
}
#[derive(Default)]
pub(crate) struct Samples(Vec<f64>);
impl Samples {
    pub fn push(&mut self, ms: f64) {
        // Receive/decode windows normally publish every 500ms; bound even if
        // the consumer is stalled. Old measurements are less useful than new.
        if self.0.len() == 512 {
            self.0.drain(..256);
        }
        self.0.push(ms);
    }
    pub fn take(&mut self) -> Option<[f64; 3]> {
        if self.0.is_empty() {
            return None;
        }
        self.0.sort_by(f64::total_cmp);
        let n = self.0.len();
        let result = [
            self.0.iter().sum::<f64>() / n as f64,
            self.0[(n * 95).div_ceil(100) - 1],
            self.0[n - 1],
        ];
        self.0.clear();
        Some(result)
    }
}
#[derive(Default)]
pub(crate) struct Cadence {
    previous: Option<(u32, Instant)>,
    source: Samples,
    arrival: Samples,
}
impl Cadence {
    pub fn observe(&mut self, timestamp: u32, now: Instant) {
        if let Some((previous, at)) = self.previous {
            let delta = timestamp.wrapping_sub(previous) as i32;
            if delta <= 0 {
                return;
            } // duplicates/RTX/reordering are not new pictures
            self.source.push(delta as f64 / 90.0);
            self.arrival
                .push(now.duration_since(at).as_secs_f64() * 1000.0);
        }
        self.previous = Some((timestamp, now));
    }
    pub fn publish(&mut self, target: &mut Snapshot) {
        target.source_spacing_ms = self.source.take();
        target.arrival_spacing_ms = self.arrival.take();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn constant_source_bursty_delivery_and_timestamp_wrap_are_distinct() {
        let mut c = Cadence::default();
        let now = Instant::now();
        let start = u32::MAX - 750;
        c.observe(start, now);
        c.observe(start.wrapping_add(1500), now + Duration::from_millis(1));
        c.observe(start.wrapping_add(1500), now + Duration::from_millis(2));
        c.observe(start, now + Duration::from_millis(3));
        c.observe(start.wrapping_add(3000), now + Duration::from_millis(34));
        let mut s = Snapshot::default();
        c.publish(&mut s);
        assert!((s.source_spacing_ms.unwrap()[0] - 1000.0 / 60.0).abs() < 0.001);
        assert_eq!(s.arrival_spacing_ms, Some([17.0, 33.0, 33.0]));
        c.publish(&mut s);
        assert_eq!(s.source_spacing_ms, None);
    }
    #[test]
    fn slow_source_is_visible_without_network_bunching() {
        let mut c = Cadence::default();
        let now = Instant::now();
        for i in 0..25 {
            c.observe(i * 1800, now + Duration::from_millis(i as u64 * 20));
        }
        let mut s = Snapshot::default();
        c.publish(&mut s);
        assert_eq!(s.source_spacing_ms, Some([20.0; 3]));
        assert_eq!(s.arrival_spacing_ms, Some([20.0; 3]));
    }
}
