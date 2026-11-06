//! Platform-independent ownership rules for a capacity transaction.
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

/// PnP/CCD publishes in stages. Before the next mutation, require consecutive
/// complete, identical observations. An unreadable sample resets the window;
/// it is never interpreted as an absent monitor or a new preservation baseline.
pub(super) struct Settling<T> {
    last: Option<T>,
    since: Duration,
    samples: u32,
}
impl<T> Default for Settling<T> {
    fn default() -> Self {
        Self { last: None, since: Duration::ZERO, samples: 0 }
    }
}
impl<T: Clone + Eq> Settling<T> {
    pub fn observe(&mut self, now: Duration, value: Option<&T>) -> bool {
        if value.is_none() || self.last.as_ref() != value {
            self.last = value.cloned();
            self.since = now;
            self.samples = 0;
        }
        if value.is_none() { return false; }
        self.samples += 1;
        now.saturating_sub(self.since) >= Duration::from_millis(500) && self.samples >= 6
    }
}

#[derive(Default)]
pub(super) struct StableReadback {
    samples: u32,
    failed: bool,
}
impl StableReadback {
    pub fn observe(&mut self, elapsed: Duration, exact: bool) -> Result<bool, ()> {
        self.failed |= !exact;
        if self.failed {
            return Err(());
        }
        self.samples += 1;
        Ok(elapsed >= Duration::from_secs(1) && self.samples >= 11)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(super) enum Progress {
    #[default]
    Unchanged,
    AdapterSubmitted,
    SaveSubmitted,
    FinalReadback,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Recovery {
    ReleaseUnchanged,
    CompensateOwnedAdapter,
    VerifySavedReadback,
    RetainOwnership,
}
impl Progress {
    pub fn recovery(self, host_idle: bool, exact_resource: bool) -> Recovery {
        match self {
            Self::Unchanged => Recovery::ReleaseUnchanged,
            _ if !host_idle || !exact_resource => Recovery::RetainOwnership,
            Self::AdapterSubmitted => Recovery::CompensateOwnedAdapter,
            Self::SaveSubmitted | Self::FinalReadback => Recovery::VerifySavedReadback,
        }
    }
}
pub(super) fn foreign<K: Ord + Clone, V: Clone>(
    value: &BTreeMap<K, V>,
    owned: &BTreeSet<K>,
) -> BTreeMap<K, V> {
    value
        .iter()
        .filter(|(key, _)| !owned.contains(*key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}
pub(super) fn unchanged_foreign<K: Ord + Clone, V: Clone + Eq>(
    before: &BTreeMap<K, V>,
    after: &BTreeMap<K, V>,
    owned: &BTreeSet<K>,
) -> bool {
    foreign(before, owned) == foreign(after, owned)
}
/// A saved scaling policy is future user intent. Even when its resolved live
/// image happens to look identical, ASPECTRATIOCENTEREDMAX (4), IDENTITY (1)
/// and PREFERRED (128) are distinct policies and are not silently normalized.
pub(super) fn saved_mode_matches_current(saved: &[i64], current: &[i64]) -> bool {
    saved == current
}
/// The SAVE proposal uses PREFERRED=128, which asks Windows to retain the last
/// saved scaling for this path. A different resolved transform is acceptable
/// only at native size, where the documented identity/center/stretch/aspect
/// transforms have the same effect. No private CUSTOM state is reproduced.
pub(super) fn preserve_saved_scaling(saved: &[i64], current: &[i64]) -> bool {
    if saved.len() != 23
        || current.len() != 23
        || !(1..=4).contains(&saved[7])
        || !(1..=4).contains(&current[7])
    {
        return false;
    }
    if saved
        .iter()
        .zip(current)
        .enumerate()
        .any(|(i, (a, b))| i != 7 && a != b)
    {
        return false;
    }
    saved_mode_matches_current(saved, current)
        || (current[6] == 1 && current[0] == current[19] && current[1] == current[20])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_waits_for_complete_identity_and_consecutive_original_state() {
        let original = BTreeMap::from([("original", vec![3072, 1920, 0, 0])]);
        let mut settling = Settling::default();
        // Incomplete identity and a transient original mode are read-only
        // waiting states. Neither grants permission to remove another output.
        assert!(!settling.observe(Duration::ZERO, None));
        assert!(!settling.observe(Duration::from_millis(100), Some(&original)));
        assert!(!settling.observe(Duration::from_millis(400), None));
        for step in 0..6 {
            assert_eq!(settling.observe(Duration::from_millis(500 + step * 100), Some(&original)), step == 5);
        }
    }
    #[test]
    fn target_replacement_restarts_the_readiness_window() {
        let mut settling = Settling::default();
        for step in 0..5 {
            assert!(!settling.observe(Duration::from_millis(step * 100), Some(&"first")));
        }
        assert!(!settling.observe(Duration::from_millis(500), Some(&"replacement")));
        for step in 1..6 {
            assert_eq!(settling.observe(Duration::from_millis(500 + step * 100), Some(&"replacement")), step == 5);
        }
    }
    #[test]
    fn stable_foreign_drift_never_becomes_an_accepted_original_state() {
        let original = BTreeMap::from([("original", vec![1920, 1080, 0, 0])]);
        let changed = BTreeMap::from([("original", vec![1920, 1080, 100, 0])]);
        let owned = BTreeSet::from(["owned"]);
        let mut settling = Settling::default();
        for step in 0..20 {
            let value = super::super::provision_policy::check_enabled(&original, &changed, &owned)
                .ok().map(|()| &changed);
            assert!(!settling.observe(Duration::from_millis(step * 100), value));
        }
        for step in 0..6 {
            let value = super::super::provision_policy::check_enabled(&original, &original, &owned)
                .ok().map(|()| &original);
            assert_eq!(settling.observe(Duration::from_millis(2000 + step * 100), value), step == 5);
        }
    }
    #[test]
    fn failure_before_first_submission_does_not_restart_a_driver() {
        assert_eq!(
            Progress::Unchanged.recovery(false, false),
            Recovery::ReleaseUnchanged
        );
    }
    #[test]
    fn delayed_profile_drift_latches_failure_and_never_becomes_a_new_baseline() {
        let mut stable = StableReadback::default();
        for step in 0..11 {
            assert_eq!(
                stable.observe(Duration::from_millis(step * 100), true),
                Ok(step == 10)
            );
        }
        let mut drift = StableReadback::default();
        assert_eq!(drift.observe(Duration::ZERO, true), Ok(false));
        assert_eq!(drift.observe(Duration::from_millis(200), false), Err(()));
        assert_eq!(
            drift.observe(Duration::from_secs(2), true),
            Err(()),
            "a reverted late profile cannot silently release ownership"
        );
    }
    #[test]
    fn new_stream_or_replaced_resource_retains_ownership_after_submission() {
        for progress in [
            Progress::AdapterSubmitted,
            Progress::SaveSubmitted,
            Progress::FinalReadback,
        ] {
            assert_eq!(progress.recovery(false, true), Recovery::RetainOwnership);
            assert_eq!(progress.recovery(true, false), Recovery::RetainOwnership);
        }
        assert_eq!(
            Progress::AdapterSubmitted.recovery(true, true),
            Recovery::CompensateOwnedAdapter
        );
        assert_eq!(
            Progress::SaveSubmitted.recovery(true, true),
            Recovery::VerifySavedReadback
        );
    }
    #[test]
    fn own_disconnect_or_new_pool_target_cannot_mask_foreign_database_edits() {
        let before = BTreeMap::from([("original", vec![1920, 1080]), ("own-old", vec![800, 600])]);
        let after = BTreeMap::from([("original", vec![1920, 1080]), ("own-new", vec![800, 600])]);
        let own = BTreeSet::from(["own-old", "own-new"]);
        assert!(unchanged_foreign(&before, &after, &own));
        let mut changed = after;
        changed.insert("original", vec![3840, 2160]);
        assert!(!unchanged_foreign(&before, &changed, &own));
        changed.insert("original", vec![1920, 1080]);
        changed.insert("unknown-physical", vec![800, 600]);
        assert!(!unchanged_foreign(&before, &changed, &own));
    }
    #[test]
    fn saved_scaling_policy_is_not_guessed_from_the_resolved_active_mode() {
        let active = vec![3840, 2160, 4, 0, 0, 0, 1, 1, 165, 1];
        let mut saved = active.clone();
        saved[7] = 4;
        assert!(!saved_mode_matches_current(&saved, &active));
        saved[0] = 1920;
        assert!(!saved_mode_matches_current(&saved, &active));
        saved = active.clone();
        saved[7] = 128;
        assert!(!saved_mode_matches_current(&saved, &active));
        assert!(saved_mode_matches_current(&active, &active));
    }
    #[test]
    fn preferred_save_requires_exact_modes_and_proven_equal_effect_of_scaling() {
        let current = vec![
            3840, 2160, 4, 0, 0, 0, 1, 1, 165, 1, 1, 7, 0, 5, 1368576000, 356400, 1, 165, 1, 3840,
            2160, 65791, 1,
        ];
        let mut saved = current.clone();
        saved[7] = 4;
        assert!(preserve_saved_scaling(&saved, &current));
        for index in [0, 3, 5, 6, 8, 12, 14, 19, 20] {
            let mut changed = current.clone();
            changed[index] += 1;
            assert!(!preserve_saved_scaling(&saved, &changed));
        }
        for scaling in [0, 5, 128, -1] {
            let mut unsupported = saved.clone();
            unsupported[7] = scaling;
            assert!(!preserve_saved_scaling(&unsupported, &current));
        }
        let mut scaled = current.clone();
        scaled[19] = 7680;
        let mut scaled_saved = scaled.clone();
        scaled_saved[7] = 4;
        assert!(!preserve_saved_scaling(&scaled_saved, &scaled));
        assert!(preserve_saved_scaling(&scaled, &scaled));
    }
    #[test]
    fn fake_failed_compensation_keeps_claims_until_exact_readback() {
        struct Fake {
            progress: Progress,
            claims: usize,
            restarts: usize,
            saves: usize,
        }
        let mut fake = Fake {
            progress: Progress::AdapterSubmitted,
            claims: 17,
            restarts: 1,
            saves: 0,
        };
        for (idle, owned) in [(false, true), (true, false)] {
            match fake.progress.recovery(idle, owned) {
                Recovery::RetainOwnership => {}
                _ => panic!("ownership was released before exact recovery"),
            }
            assert_eq!((fake.claims, fake.restarts, fake.saves), (17, 1, 0));
        }
        assert_eq!(
            fake.progress.recovery(true, true),
            Recovery::CompensateOwnedAdapter
        );
        fake.restarts += 1;
        fake.progress = Progress::SaveSubmitted;
        fake.saves = 1;
        assert_eq!(
            fake.progress.recovery(true, true),
            Recovery::VerifySavedReadback
        );
        // Verifying a submitted SAVE must not manufacture a second write or
        // restore the old global database to make a failed readback pass.
        assert_eq!((fake.restarts, fake.saves), (2, 1));
        fake.claims = 0;
        assert_eq!(fake.claims, 0);
    }
}
