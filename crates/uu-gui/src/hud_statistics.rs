//! Rolling counter deltas. The observed coverage is never extrapolated.
use super::Counters;
use std::{collections::VecDeque, time::Instant};
pub const WINDOWS: [u64; 3] = [180, 600, 3600];
#[derive(Default)]
pub struct Statistics {
    samples: VecDeque<(Instant, Counters)>,
    generation: Option<[u64; 2]>,
}
impl Counters {
    fn delta(&self, old: &Self) -> Option<Self> {
        macro_rules! delta { ($($name:ident),*) => { Some(Self { $($name:self.$name.checked_sub(old.$name)?,)* recovery:[
            self.recovery[0].checked_sub(old.recovery[0])?,self.recovery[1].checked_sub(old.recovery[1])?,
            self.recovery[2].checked_sub(old.recovery[2])?,self.recovery[3].checked_sub(old.recovery[3])?] }) }; }
        delta!(
            lost,
            nack,
            rtx,
            reordered,
            late,
            damaged,
            errors,
            queue_drops,
            keyframes,
            fec_packets,
            fec_recovered,
            fec_bytes,
            fec_drops,
            original_received,
            original_lost,
            duplicates,
            resyncs
        )
    }
}
impl Statistics {
    pub fn record(
        &mut self,
        now: Instant,
        generation: [u64; 2],
        counters: &Counters,
    ) -> ([Counters; 3], [f64; 3]) {
        if self.generation != Some(generation)
            || self
                .samples
                .back()
                .is_some_and(|(at, old)| now < *at || counters.delta(old).is_none())
        {
            self.samples.clear();
            self.generation = Some(generation);
        }
        // Retain one predecessor for the longest window, with bounded 500ms samples.
        while self.samples.len() > 1
            && now
                .saturating_duration_since(self.samples[1].0)
                .as_secs_f64()
                > 3600.5
        {
            self.samples.pop_front();
        }
        if self
            .samples
            .back()
            .is_none_or(|(at, _)| now.saturating_duration_since(*at).as_millis() >= 499)
        {
            self.samples.push_back((now, counters.clone()));
        }
        let mut coverage = [0.; 3];
        let deltas = std::array::from_fn(|i| {
            let Some((at, baseline)) = self.samples.iter().find(|(at, _)| {
                now.saturating_duration_since(*at).as_secs_f64() <= WINDOWS[i] as f64
            }) else {
                return Counters::default();
            };
            coverage[i] = now.saturating_duration_since(*at).as_secs_f64();
            counters.delta(baseline).unwrap_or_default()
        });
        (deltas, coverage)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn windows_switch_without_losing_history_and_reset_on_owner_epoch_or_counter_rollback() {
        let mut stats = Statistics::default();
        let now = Instant::now();
        for second in 0..=4000 {
            let (c, coverage) = stats.record(
                now + Duration::from_secs(second),
                [1, 0],
                &Counters {
                    nack: second,
                    ..Default::default()
                },
            );
            if second == 4000 {
                assert_eq!(c.map(|c| c.nack), [180, 600, 3600]);
                assert_eq!(coverage, [180., 600., 3600.]);
            }
        }
        let (c, coverage) = stats.record(
            now + Duration::from_secs(4001),
            [1, 0],
            &Counters {
                nack: 1,
                ..Default::default()
            },
        );
        assert_eq!(c.map(|c| c.nack), [0; 3]);
        assert_eq!(coverage, [0.; 3]);
        stats.record(
            now + Duration::from_secs(4002),
            [1, 0],
            &Counters {
                nack: 2,
                ..Default::default()
            },
        );
        let (_, coverage) = stats.record(
            now + Duration::from_secs(4003),
            [2, 0],
            &Counters {
                nack: 3,
                ..Default::default()
            },
        );
        assert_eq!(coverage, [0.; 3]);
    }
}
