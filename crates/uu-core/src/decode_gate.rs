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
