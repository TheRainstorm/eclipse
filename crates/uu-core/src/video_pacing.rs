//! Preserve the capture clock through a small overrun without accumulating
//! catch-up work after a long stall. This only schedules new captures.
use std::time::{Duration, Instant};

pub(crate) fn next_deadline(previous: Instant, period: Duration, now: Instant) -> Instant {
    let Some(next) = previous.checked_add(period) else {
        return now;
    };
    // At most one late opportunity may run immediately. Larger debt is retired;
    // never replay stale captures or build an unbounded burst of encoded frames.
    if now.saturating_duration_since(next) >= period {
        now
    } else {
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn small_overruns_keep_phase_and_large_stalls_rebase_once() {
        let base = Instant::now();
        let period = Duration::from_millis(10);
        let next = next_deadline(base, period, base + Duration::from_millis(12));
        assert_eq!(next, base + period);
        assert_eq!(
            next_deadline(next, period, base + Duration::from_millis(19)),
            base + period * 2
        );
        let reset = next_deadline(next, period, base + Duration::from_millis(105));
        assert_eq!(reset, base + Duration::from_millis(105));
        assert!(next_deadline(reset, period, reset) > reset);
        assert_eq!(
            next_deadline(base, Duration::ZERO, base + period),
            base + period
        );
    }
    #[test]
    fn repeated_short_scheduler_overruns_do_not_lower_the_requested_rate() {
        let period = Duration::from_nanos(1_000_000_000 / 165);
        let work = Duration::from_micros(5500);
        let base = Instant::now();
        let mut end = base;
        for keep_phase in [false, true] {
            let mut now = base;
            let mut deadline = base;
            for _ in 0..5000 {
                if now < deadline {
                    now = deadline + Duration::from_micros(800);
                }
                now += work;
                deadline = if keep_phase {
                    next_deadline(deadline, period, now)
                } else {
                    (deadline + period).max(now)
                };
            }
            if keep_phase {
                assert!(now.duration_since(base) < period * 5001);
                assert!(now < end - Duration::from_millis(300));
            } else {
                end = now;
            }
        }
    }
}
