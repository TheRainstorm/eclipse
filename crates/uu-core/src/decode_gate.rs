//! Reference recovery without destroying a healthy decoder on transport loss.
#[derive(Default)]
pub(crate) struct DecodeGate {
    epoch: Option<u64>,
    pub waiting: bool,
}
impl DecodeGate {
    pub fn accepts(&mut self, sample: u64, latest: u64, idr: bool) -> bool {
        if self.epoch != Some(latest) {
            self.epoch = Some(latest);
            self.waiting = true;
        }
        // A complete queued IDR remains independently decodable even when
        // later pictures have suffered loss. Show it, but keep waiting for a
        // refresh that also covers that later loss before accepting P frames.
        idr || (sample == latest && !self.waiting)
    }
    pub fn decoded(&mut self, sample: u64, latest: u64, idr: bool) {
        if idr { self.waiting = sample != latest; }
    }
    pub fn failed(&mut self) { self.waiting = true; }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn later_loss_does_not_starve_a_complete_queued_idr_or_admit_broken_references() {
        let mut gate = DecodeGate::default();
        assert!(!gate.accepts(0, 0, false));
        assert!(gate.accepts(0, 0, true));
        gate.decoded(0, 0, true);
        assert!(gate.accepts(0, 0, false));
        assert!(gate.accepts(0, 2, true));
        gate.decoded(0, 2, true);
        assert!(!gate.accepts(2, 2, false));
        assert!(!gate.accepts(0, 2, false));
        assert!(gate.accepts(2, 2, true));
        gate.decoded(2, 2, true);
        assert!(gate.accepts(2, 2, false));
        gate.failed();
        assert!(!gate.accepts(2, 2, false));
    }
}

use std::sync::atomic::{AtomicU64, Ordering};
/// A late decode completion cannot clear a newer recovery request.
pub(crate) struct RecoverySignal {
    requested: AtomicU64,
    completed: AtomicU64,
}
impl Default for RecoverySignal {
    fn default() -> Self { Self { requested: AtomicU64::new(0), completed: AtomicU64::new(u64::MAX) } }
}
impl RecoverySignal {
    pub fn requested(&self) -> u64 { self.requested.load(Ordering::Acquire) }
    pub fn request(&self) { self.requested.fetch_add(1, Ordering::AcqRel); }
    pub fn complete(&self, request: u64) { self.completed.store(request, Ordering::Release); }
    pub fn is_waiting(&self) -> bool { self.requested() != self.completed.load(Ordering::Acquire) }
}
#[cfg(test)]
mod signal_tests {
    use super::*;
    #[test]
    fn a_loss_between_validation_and_decode_completion_stays_pending() {
        let signal = RecoverySignal::default();
        assert!(signal.is_waiting());
        let idr_request = signal.requested();
        signal.request(); // Loss arrives after the decoder checked its IDR.
        signal.complete(idr_request);
        assert!(signal.is_waiting());
        signal.complete(signal.requested());
        assert!(!signal.is_waiting());
    }
}
