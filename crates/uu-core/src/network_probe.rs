//! CONTROL echo RTT, independent of synchronized host/client clocks.
use std::{collections::VecDeque, time::Instant};

#[derive(Default)]
pub(crate) struct NetworkProbe {
    pending: Option<(u64, Instant)>,
    samples: VecDeque<f64>,
}
impl NetworkProbe {
    pub fn begin(&mut self, seq: u64) { self.pending = Some((seq, Instant::now())); }
    pub fn reply(&mut self, data: &[u8]) -> Option<(u32, u32)> {
        let (seq, _) = self.pending?;
        if !crate::control_wire::echo_matches(data, seq) { return None; }
        self.reply_sequence(seq)
    }
    pub fn reply_sequence(&mut self, seq:u64) -> Option<(u32,u32)> {
        let (expected,at)=self.pending?;
        if seq!=expected {return None;}
        self.pending = None;
        self.sample(at.elapsed().as_secs_f64() * 1_000_000.0)
    }
    fn sample(&mut self, us: f64) -> Option<(u32, u32)> {
        if !us.is_finite() || us < 0.0 { return None; }
        if self.samples.len() == 32 { self.samples.pop_front(); }
        self.samples.push_back(us);
        let mean = self.samples.iter().sum::<f64>() / self.samples.len() as f64;
        let variance = self.samples.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / self.samples.len() as f64;
        Some((mean.round() as u32, variance.sqrt().round() as u32))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unmatched_or_duplicate_echo_does_not_contaminate_rtt() {
        let mut probe = NetworkProbe::default();
        probe.begin(10);
        assert!(probe.reply(&crate::control_wire::echo_response(1, 9)).is_none());
        assert!(probe.reply(&crate::control_wire::echo_response(2, 10)).is_some());
        assert!(probe.reply(&crate::control_wire::echo_response(2, 10)).is_none());
        let mut p = NetworkProbe::default();
        p.sample(1000.0);
        assert_eq!(p.sample(3000.0), Some((2000, 1000)));
    }
}
