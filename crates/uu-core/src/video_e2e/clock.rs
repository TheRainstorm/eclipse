//! Conservative host-to-client monotonic clock mapping.
//!
//! A four-timestamp exchange bounds the offset using nonnegative path delays.
//! It does not identify either one-way delay or an exact offset. Relative clock
//! drift expands the bounds; neither a midpoint nor symmetric paths are assumed.

use std::{cmp::Ordering, time::Duration};

const NANOS_PER_SECOND: u128 = 1_000_000_000;
const PARTS_PER_MILLION: u128 = 1_000_000;

/// A monotonic counter value, with an identity for its session and clock domain.
///
/// Epochs must change when a session or counter restarts. A frequency change is
/// also a new domain; comparing raw counters from different domains is invalid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WireClockStamp {
    pub epoch: u64,
    pub ticks: u64,
    pub frequency_hz: u64,
}

impl WireClockStamp {
    /// Expresses this counter in nanoseconds, rounding each endpoint outward.
    ///
    /// This conversion does not synchronize clocks or establish a session. Zero
    /// ticks can denote the counter origin; zero frequency is always invalid.
    pub fn interval_ns(self) -> Option<TimeInterval> {
        (self.frequency_hz != 0).then(|| tick_interval(self.ticks, self.frequency_hz))
    }

    fn same_domain(self, other: Self) -> bool {
        self.epoch == other.epoch
            && self.frequency_hz != 0
            && self.frequency_hz == other.frequency_hz
    }
}

/// The client sends a probe, the host receives and replies, then the client
/// receives that reply. Send/receive timestamps must bracket the actual IO.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FourTimestamp {
    pub client_send: WireClockStamp,
    pub host_receive: WireClockStamp,
    pub host_send: WireClockStamp,
    pub client_receive: WireClockStamp,
}

/// A closed interval in one monotonic clock domain, in nanoseconds since that
/// counter's origin. These bounds are not an exact time or a midpoint estimate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeInterval {
    pub earliest_ns: u128,
    pub latest_ns: u128,
}

/// A single-session map using the latest valid four-timestamp exchange.
///
/// `drift_ppm` bounds the relative rate error of client nanoseconds per host
/// nanosecond, after each clock is scaled by its advertised frequency. For
/// example, if each oscillator can err by 100 ppm, the relative bound must cover
/// both oscillators rather than passing 100 ppm here. The caller must choose a
/// bound justified for the clocks in use; this module cannot measure that bound.
///
/// The first valid sample binds both domains. Later changes require an explicit
/// reset; an old-session message never switches the active domain automatically.
#[derive(Clone, Debug)]
pub struct ClockMap {
    max_sample_age_ns: u128,
    drift_ppm: u32,
    sample: Option<FourTimestamp>,
}

impl ClockMap {
    /// Sets the maximum conservative age of a probe or mapped source event.
    ///
    /// A drift of one million ppm or more cannot provide a positive lower clock
    /// rate, so that configuration disables sampling and mapping.
    pub fn new(max_sample_age: Duration, drift_ppm: u32) -> Self {
        Self {
            max_sample_age_ns: max_sample_age.as_nanos(),
            drift_ppm,
            sample: None,
        }
    }

    /// Clears the mapping and its domain bindings, preserving configuration.
    ///
    /// The owner must also retire outstanding probes and use new session epochs
    /// before accepting messages from a replacement session.
    pub fn reset(&mut self) {
        self.sample = None;
    }

    /// Accepts a complete probe exchange, returning false for invalid samples.
    ///
    /// Rejected samples leave the previous mapping intact. Probe correlation and
    /// recording `client_receive` at actual receipt belong to the caller. This
    /// method rejects changed domains, reversed time, old/duplicate exchanges,
    /// an over-age round trip, and host processing that cannot fit in the client
    /// round trip even at the configured minimum relative clock rate.
    pub fn sample(&mut self, sample: FourTimestamp) -> bool {
        if u128::from(self.drift_ppm) >= PARTS_PER_MILLION
            || !sample.client_send.same_domain(sample.client_receive)
            || !sample.host_receive.same_domain(sample.host_send)
            || sample.client_receive.ticks < sample.client_send.ticks
            || sample.host_send.ticks < sample.host_receive.ticks
        {
            return false;
        }

        let client_elapsed = sample.client_receive.ticks - sample.client_send.ticks;
        let host_elapsed = sample.host_send.ticks - sample.host_receive.ticks;
        if tick_interval(client_elapsed, sample.client_send.frequency_hz).latest_ns
            > self.max_sample_age_ns
        {
            return false;
        }

        // C4-C1 must cover the host processing span at the slowest permitted
        // client rate. Compare exact rational durations: rounding to whole ns
        // could otherwise accept an impossible sub-nanosecond negative RTT.
        // Each numerator/denominator is at most u64::MAX * 1_000_000.
        if compare_ratios(
            u128::from(client_elapsed),
            u128::from(sample.client_send.frequency_hz),
            u128::from(host_elapsed) * (PARTS_PER_MILLION - u128::from(self.drift_ppm)),
            u128::from(sample.host_receive.frequency_hz) * PARTS_PER_MILLION,
        ) == Ordering::Less
        {
            return false;
        }

        if let Some(previous) = self.sample {
            if !previous.client_send.same_domain(sample.client_send)
                || !previous.host_receive.same_domain(sample.host_receive)
                || sample.client_send.ticks < previous.client_send.ticks
                || sample.client_receive.ticks <= previous.client_receive.ticks
                || sample.host_receive.ticks < previous.host_receive.ticks
                || sample.host_send.ticks < previous.host_send.ticks
            {
                return false;
            }
        }

        self.sample = Some(sample);
        true
    }

    /// Maps a host source event already observed by the client into client time.
    ///
    /// `now_client` is an actual counter reading taken after observing the event.
    /// This causal fact permits clipping the upper bound to that reading. An
    /// invalid/zero source counter, a different domain, a reversed client clock,
    /// an expired sample, or an over-age source interval returns None. The maximum
    /// age applies to the entire returned interval, not its midpoint.
    pub fn map_host(
        &self,
        source: WireClockStamp,
        now_client: WireClockStamp,
    ) -> Option<TimeInterval> {
        let sample = self.sample?;
        if source.ticks == 0
            || !source.same_domain(sample.host_receive)
            || !now_client.same_domain(sample.client_receive)
            || now_client.ticks < sample.client_receive.ticks
        {
            return None;
        }

        let sample_age = tick_interval(
            now_client.ticks - sample.client_receive.ticks,
            now_client.frequency_hz,
        );
        if sample_age.latest_ns > self.max_sample_age_ns {
            return None;
        }

        let client_send = sample.client_send.interval_ns()?;
        let client_receive = sample.client_receive.interval_ns()?;
        let (from_receive_lo, _, receive_drift) = self.host_delta(source, sample.host_receive);
        let (_, from_send_hi, send_drift) = self.host_delta(source, sample.host_send);

        // With zero drift these are Hs + [C1-H2, C4-H3]. With drift,
        // C(Hs) >= C1 + (Hs-H2) - drift(Hs-H2), and
        // C(Hs) <= C4 + (Hs-H3) + drift(Hs-H3).
        // Host processing and path asymmetry remain inside the two bounds.
        let earliest = client_send.earliest_ns as i128 + from_receive_lo - receive_drift;
        let latest = client_receive.latest_ns as i128 + from_send_hi + send_drift;
        if latest < 0 {
            return None;
        }

        let now = now_client.interval_ns()?;
        let interval = TimeInterval {
            earliest_ns: earliest.max(0) as u128,
            latest_ns: (latest as u128).min(now.latest_ns),
        };
        if interval.earliest_ns > interval.latest_ns
            || now.latest_ns - interval.earliest_ns > self.max_sample_age_ns
        {
            return None;
        }
        Some(interval)
    }

    fn host_delta(&self, source: WireClockStamp, anchor: WireClockStamp) -> (i128, i128, i128) {
        let magnitude = tick_interval(source.ticks.abs_diff(anchor.ticks), source.frequency_hz);
        let drift = ceil_div(
            magnitude.latest_ns * u128::from(self.drift_ppm),
            PARTS_PER_MILLION,
        ) as i128;
        let lo = magnitude.earliest_ns as i128;
        let hi = magnitude.latest_ns as i128;
        if source.ticks >= anchor.ticks {
            (lo, hi, drift)
        } else {
            (-hi, -lo, drift)
        }
    }
}

fn tick_interval(ticks: u64, frequency_hz: u64) -> TimeInterval {
    // u64 ticks * 1e9 fits u128 (and i128). A relative drift below one million
    // ppm keeps every signed mapping sum far below the i128 bound as well.
    let numerator = u128::from(ticks) * NANOS_PER_SECOND;
    let denominator = u128::from(frequency_hz);
    TimeInterval {
        earliest_ns: numerator / denominator,
        latest_ns: ceil_div(numerator, denominator),
    }
}

fn ceil_div(numerator: u128, denominator: u128) -> u128 {
    numerator / denominator + u128::from(numerator % denominator != 0)
}

/// Compares nonnegative rational values without overflowing a cross product.
fn compare_ratios(mut a: u128, mut b: u128, mut c: u128, mut d: u128) -> Ordering {
    let mut reversed = false;
    loop {
        let order = (a / b).cmp(&(c / d));
        if order != Ordering::Equal {
            return if reversed { order.reverse() } else { order };
        }
        let (a_remainder, c_remainder) = (a % b, c % d);
        if a_remainder == 0 || c_remainder == 0 {
            let order = a_remainder.cmp(&c_remainder);
            return if reversed { order.reverse() } else { order };
        }
        (a, b) = (b, a_remainder);
        (c, d) = (d, c_remainder);
        reversed = !reversed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(ticks: u64) -> WireClockStamp {
        WireClockStamp {
            epoch: 11,
            ticks,
            frequency_hz: 1_000_000_000,
        }
    }

    fn host(ticks: u64) -> WireClockStamp {
        WireClockStamp {
            epoch: 22,
            ..client(ticks)
        }
    }

    fn exchange(c1: u64, h2: u64, h3: u64, c4: u64) -> FourTimestamp {
        FourTimestamp {
            client_send: client(c1),
            host_receive: host(h2),
            host_send: host(h3),
            client_receive: client(c4),
        }
    }

    fn map(drift_ppm: u32) -> ClockMap {
        ClockMap::new(Duration::from_secs(5), drift_ppm)
    }

    #[test]
    fn asymmetric_paths_bound_offset_without_assuming_an_exact_midpoint() {
        // The true client time is host time minus 1000 ns. Forward delay is
        // 5 ns, host processing is 5 ns, and reverse delay is 35 ns.
        let mut mapping = map(0);
        assert!(mapping.sample(exchange(100, 1105, 1110, 145)));
        let interval = mapping.map_host(host(1120), client(200)).unwrap();
        assert_eq!(
            interval,
            TimeInterval {
                earliest_ns: 115,
                latest_ns: 155,
            }
        );
        assert!(interval.earliest_ns <= 120 && 120 <= interval.latest_ns);
        assert_ne!((interval.earliest_ns + interval.latest_ns) / 2, 120);
    }

    #[test]
    fn unequal_frequencies_map_into_client_nanoseconds() {
        let mut probe = exchange(10_000, 1000, 1005, 10_600);
        probe.host_receive.frequency_hz = 10_000_000;
        probe.host_send.frequency_hz = 10_000_000;
        let mut mapping = map(0);
        assert!(mapping.sample(probe));
        assert_eq!(
            mapping.map_host(
                WireClockStamp {
                    ticks: 1010,
                    ..probe.host_receive
                },
                client(20_000),
            ),
            Some(TimeInterval {
                earliest_ns: 11_000,
                latest_ns: 11_100,
            })
        );
    }

    #[test]
    fn conversion_and_signed_deltas_round_outward() {
        let c = WireClockStamp {
            frequency_hz: 3,
            ..client(1)
        };
        let h = WireClockStamp { epoch: 22, ..c };
        let mut mapping = map(0);
        assert!(mapping.sample(FourTimestamp {
            client_send: c,
            host_receive: h,
            host_send: h,
            client_receive: c,
        }));
        assert_eq!(
            WireClockStamp { ticks: 2, ..c }.interval_ns(),
            Some(TimeInterval {
                earliest_ns: 666_666_666,
                latest_ns: 666_666_667,
            })
        );
        assert_eq!(
            mapping.map_host(
                WireClockStamp { ticks: 2, ..h },
                WireClockStamp { ticks: 3, ..c },
            ),
            Some(TimeInterval {
                earliest_ns: 666_666_666,
                latest_ns: 666_666_668,
            })
        );

        // The source is one fractional tick before the synchronization anchor.
        let c = WireClockStamp { ticks: 2, ..c };
        let h = WireClockStamp { ticks: 2, ..h };
        mapping.reset();
        assert!(mapping.sample(FourTimestamp {
            client_send: c,
            host_receive: h,
            host_send: h,
            client_receive: c,
        }));
        assert_eq!(
            mapping.map_host(
                WireClockStamp { ticks: 1, ..h },
                WireClockStamp { ticks: 3, ..c },
            ),
            Some(TimeInterval {
                earliest_ns: 333_333_332,
                latest_ns: 333_333_334,
            })
        );
    }

    #[test]
    fn relative_drift_expands_both_past_and_future_source_bounds() {
        let mut mapping = map(100);
        assert!(mapping.sample(exchange(1_000_000, 1_000_000, 1_000_000, 1_000_000)));
        assert_eq!(
            mapping.map_host(host(2_000_000), client(2_100_000)),
            Some(TimeInterval {
                earliest_ns: 1_999_900,
                latest_ns: 2_000_100,
            })
        );
        assert_eq!(
            mapping.map_host(host(500_000), client(1_100_000)),
            Some(TimeInterval {
                earliest_ns: 499_950,
                latest_ns: 500_050,
            })
        );
    }

    #[test]
    fn processing_must_fit_round_trip_even_with_drift() {
        let probe = exchange(1_000_000_000, 1_000_000_000, 2_000_000_000, 1_999_900_000);
        assert!(map(100).sample(probe));
        assert!(!map(99).sample(probe));
        assert!(!map(0).sample(exchange(100, 200, 220, 119)));

        // Host processing is positive but below one whole nanosecond. Whole-ns
        // outward rounding must not turn this negative RTT into an admissible 0.
        let mut probe = exchange(100, 200, 201, 100);
        probe.host_receive.frequency_hz = u64::MAX;
        probe.host_send.frequency_hz = u64::MAX;
        assert!(!map(0).sample(probe));
    }

    #[test]
    fn rejects_invalid_frequencies_domains_and_order_without_replacing_sample() {
        let baseline = exchange(100, 1100, 1100, 120);
        let mut mapping = map(0);
        assert!(mapping.sample(baseline));
        for invalid in [
            FourTimestamp {
                client_receive: WireClockStamp {
                    frequency_hz: 0,
                    ..baseline.client_receive
                },
                ..baseline
            },
            FourTimestamp {
                host_receive: WireClockStamp {
                    frequency_hz: 0,
                    ..baseline.host_receive
                },
                ..baseline
            },
            FourTimestamp {
                client_receive: WireClockStamp {
                    epoch: 12,
                    ..baseline.client_receive
                },
                ..baseline
            },
            FourTimestamp {
                host_send: WireClockStamp {
                    frequency_hz: 1,
                    ..baseline.host_send
                },
                ..baseline
            },
            exchange(100, 1100, 1099, 120),
            exchange(100, 1100, 1100, 99),
            baseline,
            exchange(90, 1200, 1200, 130),
            exchange(110, 1000, 1000, 130),
        ] {
            assert!(!mapping.sample(invalid), "{invalid:?}");
        }
        assert_eq!(client(1).interval_ns().unwrap().earliest_ns, 1);
        assert!(WireClockStamp {
            frequency_hz: 0,
            ..client(1)
        }
        .interval_ns()
        .is_none());
        assert_eq!(
            mapping.map_host(host(1100), client(130)),
            Some(TimeInterval {
                earliest_ns: 100,
                latest_ns: 120,
            })
        );
    }

    #[test]
    fn expired_sample_or_source_and_reversed_now_return_unknown() {
        let mut mapping = ClockMap::new(Duration::from_nanos(100), 0);
        assert!(mapping.sample(exchange(100, 1100, 1100, 100)));
        assert!(mapping.map_host(host(1100), client(200)).is_some());
        assert!(mapping.map_host(host(1200), client(201)).is_none());
        assert!(mapping.map_host(host(1099), client(200)).is_none());
        assert!(mapping.map_host(host(1100), client(99)).is_none());
        assert!(mapping.map_host(host(0), client(100)).is_none());
        assert!(mapping.map_host(host(1500), client(200)).is_none());
        assert!(!mapping.sample(exchange(1000, 2000, 2000, 1101)));
    }

    #[test]
    fn receipt_causality_clips_uncertain_future_bound() {
        let mut mapping = map(0);
        assert!(mapping.sample(exchange(100, 110, 110, 130)));
        assert_eq!(
            mapping.map_host(host(130), client(140)),
            Some(TimeInterval {
                earliest_ns: 120,
                latest_ns: 140,
            })
        );
        assert!(mapping.map_host(host(151), client(140)).is_none());
    }

    #[test]
    fn epochs_and_frequencies_cannot_cross_sessions_until_reset() {
        let mut mapping = map(0);
        let baseline = exchange(100, 1100, 1100, 120);
        assert!(mapping.sample(baseline));
        let source = WireClockStamp {
            epoch: 23,
            ..host(1100)
        };
        assert!(mapping.map_host(source, client(130)).is_none());
        assert!(mapping
            .map_host(
                host(1100),
                WireClockStamp {
                    epoch: 12,
                    ..client(130)
                }
            )
            .is_none());
        assert!(mapping
            .map_host(
                WireClockStamp {
                    frequency_hz: 10_000_000,
                    ..host(1100)
                },
                client(130)
            )
            .is_none());
        let replacement = FourTimestamp {
            host_receive: source,
            host_send: source,
            ..baseline
        };
        assert!(!mapping.sample(replacement));
        mapping.reset();
        assert!(mapping.map_host(host(1100), client(130)).is_none());
        assert!(mapping.sample(replacement));
        assert!(mapping.map_host(host(1100), client(130)).is_none());
        assert!(mapping.map_host(source, client(130)).is_some());
    }

    #[test]
    fn full_integer_range_does_not_overflow_or_lose_sub_nanosecond_order() {
        let stamp = WireClockStamp {
            ticks: u64::MAX,
            frequency_hz: 1,
            ..client(1)
        };
        let ns = u128::from(u64::MAX) * NANOS_PER_SECOND;
        assert_eq!(
            stamp.interval_ns(),
            Some(TimeInterval {
                earliest_ns: ns,
                latest_ns: ns,
            })
        );

        let anchor = WireClockStamp {
            ticks: u64::MAX - 100,
            ..stamp
        };
        let host_anchor = WireClockStamp {
            epoch: 22,
            ..anchor
        };
        let mut mapping = ClockMap::new(Duration::MAX, 999_999);
        assert!(mapping.sample(FourTimestamp {
            client_send: anchor,
            host_receive: host_anchor,
            host_send: host_anchor,
            client_receive: anchor,
        }));
        let result = mapping
            .map_host(
                WireClockStamp {
                    ticks: 1,
                    ..host_anchor
                },
                stamp,
            )
            .unwrap();
        assert!(result.earliest_ns <= NANOS_PER_SECOND);
        assert!(result.latest_ns >= NANOS_PER_SECOND);

        // Both spans are near one second; host processing exceeds the client
        // round trip by less than one ns, even though u64 counters are maximal.
        let probe = FourTimestamp {
            client_send: WireClockStamp {
                ticks: 0,
                frequency_hz: u64::MAX,
                ..client(1)
            },
            client_receive: WireClockStamp {
                ticks: u64::MAX,
                frequency_hz: u64::MAX,
                ..client(1)
            },
            host_receive: WireClockStamp {
                ticks: 0,
                frequency_hz: u64::MAX - 1,
                ..host(1)
            },
            host_send: WireClockStamp {
                ticks: u64::MAX,
                frequency_hz: u64::MAX - 1,
                ..host(1)
            },
        };
        assert!(!map(0).sample(probe));
        assert!(map(1).sample(probe));
    }

    #[test]
    fn invalid_drift_configuration_stays_unknown() {
        for drift_ppm in [1_000_000, u32::MAX] {
            let mut mapping = map(drift_ppm);
            assert!(!mapping.sample(exchange(100, 100, 100, 100)));
            assert!(mapping.map_host(host(100), client(100)).is_none());
        }
    }

    #[test]
    fn all_asymmetric_delay_combinations_contain_the_known_source_time() {
        for forward in [0, 1, 17, 1000] {
            for reverse in [0, 1, 17, 1000] {
                for processing in [0, 7, 100] {
                    let c1 = 1_000_000;
                    let h2 = c1 + 50_000 + forward;
                    let h3 = h2 + processing;
                    let c4 = h3 - 50_000 + reverse;
                    let source = h3 + 500;
                    let true_client = u128::from(source - 50_000);
                    let mut mapping = map(0);
                    assert!(mapping.sample(exchange(c1, h2, h3, c4)));
                    let interval = mapping
                        .map_host(host(source), client(source - 50_000 + 2000))
                        .unwrap();
                    assert!(interval.earliest_ns <= true_client);
                    assert!(interval.latest_ns >= true_client);
                }
            }
        }
    }
}
