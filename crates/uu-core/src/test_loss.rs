//! Opt-in, receive-only fault injection for the streaming diagnostic.
//! Never changes the network or the host; disabled unless explicitly configured.
use std::{collections::HashSet, time::Instant};
pub(crate) struct TestLoss {
    start: Instant,
    duration_ms: u64,
    dropped: HashSet<u16>,
}
impl TestLoss {
    pub fn from_env() -> Self {
        Self { start: Instant::now(), duration_ms: std::env::var("UU_TEST_VIDEO_LOSS_MS").ok()
            .and_then(|s| s.parse().ok()).unwrap_or(0).min(500), dropped: HashSet::new() }
    }
    pub fn drop_packet(&mut self, seq: u16, now: Instant) -> bool {
        if self.duration_ms == 0 { return false; }
        let elapsed = now.duration_since(self.start).as_millis() as u64;
        let phase = elapsed % 10_000;
        if elapsed >= 10_000 && phase < self.duration_ms {
            self.dropped.insert(seq);
            true
        } else if phase < 1000 {
            self.dropped.contains(&seq) // Lose matching RTX too, forcing IDR recovery.
        } else {
            self.dropped.clear();
            false
        }
    }
}
