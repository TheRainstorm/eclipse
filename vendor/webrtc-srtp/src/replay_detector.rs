//! Bounded replay bitmap with normalized offsets across counter rollover.
//!
//! RFC 3711 3.3.2: only an authenticated, previously unseen index inside the
//! window may be accepted. Keep tentative checks separate from acceptance.
use util::replay_detector::ReplayDetector;

pub(crate) struct WrappedReplayDetector {
    maximum: u64,
    window: usize,
    highest: Option<u64>,
    pending: Option<u64>,
    seen: Vec<u64>,
    head: usize,
}
impl WrappedReplayDetector {
    pub fn new(window: usize, maximum: u64) -> Self {
        Self {
            maximum,
            window,
            highest: None,
            pending: None,
            seen: vec![0; window.div_ceil(64)],
            head: 0,
        }
    }
    fn bit(&self, age: usize) -> bool {
        let index = (self.head + self.window - age) % self.window;
        self.seen[index / 64] & (1u64 << (index % 64)) != 0
    }
    fn mark(&mut self, age: usize) {
        let index = (self.head + self.window - age) % self.window;
        self.seen[index / 64] |= 1u64 << (index % 64);
    }
    fn advance(&mut self, distance: usize) {
        if distance >= self.window {
            self.seen.fill(0);
            self.head = (self.head + distance % self.window) % self.window;
        } else {
            for _ in 0..distance {
                self.head = (self.head + 1) % self.window;
                self.seen[self.head / 64] &= !(1u64 << (self.head % 64));
            }
        }
    }
    fn distance(&self, sequence: u64, highest: u64) -> i128 {
        let modulus = i128::from(self.maximum) + 1;
        let mut delta = i128::from(sequence) - i128::from(highest);
        if delta > modulus / 2 {
            delta -= modulus;
        } else if delta < -(modulus / 2) {
            delta += modulus;
        }
        delta
    }
}
impl ReplayDetector for WrappedReplayDetector {
    fn check(&mut self, sequence: u64) -> bool {
        self.pending = None;
        if self.window == 0 || sequence > self.maximum {
            return false;
        }
        if let Some(highest) = self.highest {
            let delta = self.distance(sequence, highest);
            if delta <= 0 {
                let age = (-delta) as usize;
                if age >= self.window || self.bit(age) {
                    return false;
                }
            }
        }
        self.pending = Some(sequence);
        true
    }
    fn accept(&mut self) {
        let Some(sequence) = self.pending.take() else {
            return;
        };
        let delta = self.highest.map(|highest| self.distance(sequence, highest));
        match delta {
            None => {
                self.highest = Some(sequence);
                self.mark(0);
            }
            Some(delta) if delta > 0 => {
                self.advance(delta as usize);
                self.highest = Some(sequence);
                self.mark(0);
            }
            Some(delta) => self.mark((-delta) as usize),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ring_matches_full_index_reference_under_deterministic_loss_and_reordering() {
        for window in [1, 63, 64, 65, 2048] {
            let mut detector = WrappedReplayDetector::new(window, 65535);
            let mut seen = std::collections::HashSet::new();
            let mut highest: Option<u64> = None;
            let mut next = 0u64;
            let mut random = 0x12345678u64;
            for iteration in 0..50_000 {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                next += 1 + (random >> 8) % 17;
                let index = if random & 3 == 0 {
                    next.saturating_sub(random % 4096)
                } else {
                    next
                };
                let expected = highest.map_or(true, |head| {
                    index > head || (head - index < window as u64 && !seen.contains(&index))
                });
                assert_eq!(
                    detector.check(index & 65535),
                    expected,
                    "reference mismatch: window={window}, index={index}, highest={highest:?}"
                );
                if expected && iteration % 11 != 0 {
                    detector.accept();
                    seen.insert(index);
                    highest = Some(highest.map_or(index, |head| head.max(index)));
                } // Otherwise authentication fails: do not accept or advance.
            }
        }
    }
    #[test]
    fn ring_boundaries_zero_windows_and_large_jumps_remain_bounded() {
        let mut disabled = WrappedReplayDetector::new(0, 65535);
        assert!(!disabled.check(1));
        disabled.accept();
        for window in [1, 2, 63, 65, 2047] {
            let mut detector = WrappedReplayDetector::new(window, 65535);
            assert!(detector.check(65534));
            detector.accept();
            assert!(detector.check(0));
            detector.accept();
            assert_eq!(detector.check(65535), window > 1);
            detector.accept();
            assert!(!detector.check(65535));
            assert!(detector.check(4096));
            detector.accept();
            assert!(!detector.check(0));
            assert!(!detector.check(4096));
        }
        let mut detector = WrappedReplayDetector::new(64, 65535);
        assert!(detector.check(0));
        detector.accept();
        assert!(detector.check(32768));
        detector.accept();
        assert!(!detector.check(0));
        assert!(detector.check(32769));
        detector.accept();
    }
    #[test]
    fn a_first_late_packet_from_the_previous_cycle_is_recorded_at_its_wrapped_age() {
        for window in [64, 2048] {
            let mut detector = WrappedReplayDetector::new(window, 65535);
            for sequence in (65500..=65535).chain(0..=20) {
                if sequence == 65530 {
                    continue;
                }
                assert!(detector.check(sequence));
                detector.accept();
            }
            assert!(detector.check(65530));
            detector.accept();
            assert!(!detector.check(65530));
            assert!(detector.check(21));
            detector.accept();
            assert!(!detector.check(21));
        }
    }
    #[test]
    fn failed_authentication_checks_do_not_move_the_window_or_mark_an_index() {
        let mut detector = WrappedReplayDetector::new(64, 65535);
        assert!(detector.check(32768)); // No accept: forged first packet.
        assert!(detector.check(1));
        detector.accept();
        assert!(detector.check(30000)); // No accept: forged jump.
        assert!(detector.check(2));
        detector.accept();
        assert!(!detector.check(1));
        assert!(!detector.check(65536));
        detector.accept();
        assert!(detector.check(3));
        detector.accept();
    }
    #[test]
    fn delayed_unseen_packets_and_duplicates_remain_correct_across_multiple_cycles() {
        for window in [64, 2048] {
            let mut detector = WrappedReplayDetector::new(window, 65535);
            let mut waiting = std::collections::VecDeque::new();
            for index in 0..(2u64 * 65536 + 1000) {
                let sequence = index & 65535;
                if index % 97 == 0 {
                    waiting.push_back(index);
                } else {
                    assert!(detector.check(sequence));
                    detector.accept();
                    assert!(!detector.check(sequence));
                }
                if waiting.front().is_some_and(|old| index - old >= 31) {
                    let old = waiting.pop_front().unwrap();
                    assert!(detector.check(old & 65535));
                    detector.accept();
                    assert!(!detector.check(old & 65535));
                }
            }
        }
    }
}
