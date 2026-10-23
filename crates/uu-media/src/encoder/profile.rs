//! Opt-in stage measurements for generated probes and host diagnostics.
use std::time::{Duration, Instant};

pub(super) struct Profile {
    backend: &'static str,
    totals: Option<[u128; 3]>,
}
impl Profile {
    pub fn new(backend: &'static str) -> Self {
        Self {
            backend,
            totals: std::env::var_os("UU_ENCODE_PROFILE").map(|_| [0; 3]),
        }
    }
    pub fn start(&self) -> Option<Instant> {
        self.totals.map(|_| Instant::now())
    }
    pub fn record(&mut self, start: Option<Instant>, uploaded: Option<Duration>) {
        if let (Some(totals), Some(start), Some(uploaded)) = (&mut self.totals, start, uploaded) {
            totals[0] += 1;
            totals[1] += uploaded.as_nanos();
            totals[2] += start.elapsed().saturating_sub(uploaded).as_nanos();
        }
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        if let Some([frames, upload, encode]) = self.totals.filter(|v| v[0] > 0) {
            let divisor = frames as f64 * 1_000_000.0;
            eprintln!(
                "uu-encoder-profile backend={} frames={} upload_ms={:.3} encode_ms={:.3}",
                self.backend,
                frames,
                upload as f64 / divisor,
                encode as f64 / divisor
            );
        }
    }
}
