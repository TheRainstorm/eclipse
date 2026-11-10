//! Bandwidth decisions over session snapshots. No windows, controllers or IO.
//! The workspace gathers candidates only when due and applies these decisions.
use crate::sessions::ConnectionTarget;
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

#[derive(Clone)]
pub(super) struct Candidate {
    pub session: u64,
    pub weight: u64,
    pub fixed: Option<u32>,
    pub budget_kbps: u32,
    pub loss_ppm: Option<u32>,
    pub rtt_ms: Option<u32>,
    pub deferred: bool,
}

pub(super) struct Update {
    pub session: u64,
    pub bitrate_kbps: Option<u32>,
    pub deferred_device: Option<ConnectionTarget>,
}

#[derive(Default)]
pub(super) struct Controller {
    streams: HashMap<u64, Congestion>,
    refreshed: Option<Instant>,
}
impl Controller {
    /// Force recomputes geometry/group changes immediately; each stream still
    /// observes congestion at most once per second.
    pub(super) fn rebalance(
        &mut self,
        now: Instant,
        force: bool,
        gather: impl FnOnce() -> HashMap<ConnectionTarget, Vec<Candidate>>,
    ) -> Option<Vec<Update>> {
        if !force
            && self
                .refreshed
                .is_some_and(|last| now.duration_since(last) < Duration::from_secs(1))
        {
            return None;
        }
        self.refreshed = Some(now);
        let mut active = HashSet::new();
        let mut updates = Vec::new();
        for (device, candidates) in gather() {
            let total = candidates
                .iter()
                .map(|candidate| candidate.budget_kbps)
                .min()
                .unwrap_or(0);
            let items = candidates
                .iter()
                .map(|candidate| (candidate.weight, candidate.fixed))
                .collect::<Vec<_>>();
            let allocations = uu_core::client_settings::allocate_bitrate_budget(total, &items);
            for (candidate, allocation) in candidates.into_iter().zip(allocations) {
                active.insert(candidate.session);
                let state = self.streams.entry(candidate.session).or_default();
                state.observe(candidate.loss_ppm, candidate.rtt_ms, now);
                updates.push(Update {
                    session: candidate.session,
                    bitrate_kbps: candidate
                        .fixed
                        .is_none()
                        .then(|| (allocation as f32 * state.factor).round() as u32),
                    deferred_device: candidate.deferred.then(|| device.clone()),
                });
            }
        }
        self.streams.retain(|id, _| active.contains(id));
        Some(updates)
    }
}

struct Congestion {
    factor: f32,
    last_eval: Option<Instant>,
    last_decrease: Option<Instant>,
    last_increase: Option<Instant>,
}
impl Default for Congestion {
    fn default() -> Self {
        Self {
            factor: 1.,
            last_eval: None,
            last_decrease: None,
            last_increase: None,
        }
    }
}
impl Congestion {
    fn observe(&mut self, loss_ppm: Option<u32>, rtt_ms: Option<u32>, now: Instant) {
        if self
            .last_eval
            .is_some_and(|last| now.duration_since(last) < Duration::from_secs(1))
        {
            return;
        }
        let severe =
            loss_ppm.is_some_and(|loss| loss >= 20_000) || rtt_ms.is_some_and(|rtt| rtt >= 250);
        let good =
            loss_ppm.is_some_and(|loss| loss <= 5_000) && rtt_ms.is_none_or(|rtt| rtt <= 150);
        if severe {
            self.factor = (self.factor * 0.8).max(0.15);
            self.last_decrease = Some(now);
        } else if good
            && self
                .last_decrease
                .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(5))
            && self
                .last_increase
                .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(2))
        {
            self.factor = (self.factor + 0.05).min(1.);
            self.last_increase = Some(now);
        }
        self.last_eval = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_congestion_controller_uses_loss_and_rtt_with_hysteresis() {
        let start = Instant::now();
        let mut state = Congestion::default();
        state.observe(Some(25_000), None, start);
        assert!(state.factor < 1.);
        let reduced = state.factor;
        state.observe(Some(0), Some(20), start + Duration::from_secs(1));
        assert_eq!(
            state.factor, reduced,
            "a healthy sample cannot immediately undo backoff"
        );
        state.observe(None, None, start + Duration::from_secs(6));
        assert_eq!(
            state.factor, reduced,
            "missing loss evidence cannot increase the factor"
        );
        state.observe(Some(0), Some(20), start + Duration::from_secs(7));
        assert!(state.factor > reduced);
        state.observe(Some(0), Some(300), start + Duration::from_secs(8));
        assert!(state.factor < reduced + 0.05);
    }

    fn candidate(session: u64, weight: u64) -> Candidate {
        Candidate {
            session,
            weight,
            fixed: None,
            budget_kbps: 100_000,
            loss_ppm: None,
            rtt_ms: None,
            deferred: false,
        }
    }
    fn device(id: &str) -> ConnectionTarget {
        ConnectionTarget::NativeDevice(id.into())
    }

    #[test]
    fn groups_reserve_manual_streams_and_keep_other_devices_independent() {
        let now = Instant::now();
        let mut controller = Controller::default();
        let mut fixed = candidate(1, 1);
        fixed.fixed = Some(20_000);
        let mut automatic = candidate(2, 1);
        automatic.budget_kbps = 80_000;
        automatic.deferred = true;
        let mut other = candidate(3, 1);
        other.budget_kbps = 50_000;
        let updates = controller
            .rebalance(now, false, || {
                HashMap::from([
                    (device("A"), vec![fixed, automatic]),
                    (device("B"), vec![other]),
                ])
            })
            .unwrap();
        let by_id = |id| updates.iter().find(|update| update.session == id).unwrap();
        assert_eq!(
            by_id(1).bitrate_kbps,
            None,
            "manual settings are not overwritten"
        );
        assert_eq!(by_id(2).bitrate_kbps, Some(60_000));
        assert_eq!(by_id(2).deferred_device, Some(device("A")));
        assert_eq!(by_id(3).bitrate_kbps, Some(50_000));
        assert_eq!(by_id(3).deferred_device, None);
    }

    #[test]
    fn forced_rebalance_preserves_hysteresis_and_closed_sessions_retire_their_history() {
        let now = Instant::now();
        let mut controller = Controller::default();
        let mut congested = candidate(1, 1);
        congested.rtt_ms = Some(300);
        let gather = || HashMap::from([(device("A"), vec![congested.clone()])]);
        let first = controller.rebalance(now, false, gather).unwrap();
        assert_eq!(first[0].bitrate_kbps, Some(80_000));
        assert!(controller
            .rebalance(now + Duration::from_millis(50), false, || panic!(
                "early polls must not resample GUI stats"
            ))
            .is_none());
        let forced = controller
            .rebalance(now + Duration::from_millis(50), true, gather)
            .unwrap();
        assert_eq!(forced[0].bitrate_kbps, first[0].bitrate_kbps);
        controller
            .rebalance(now + Duration::from_secs(1), true, HashMap::new)
            .unwrap();
        let fresh = controller
            .rebalance(now + Duration::from_secs(2), true, || {
                HashMap::from([(device("A"), vec![candidate(1, 1)])])
            })
            .unwrap();
        assert_eq!(
            fresh[0].bitrate_kbps,
            Some(100_000),
            "closed history cannot penalize a new session"
        );
    }
}
