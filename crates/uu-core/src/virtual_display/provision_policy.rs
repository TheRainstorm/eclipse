//! Pure admission and recovery checks for a disabled-adapter supply operation.
//! Neither check grants authority to rewrite another output's layout.
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Violation<K> {
    ExistingChanged(K),
    ForeignActivated(K),
}

pub(super) fn check_enabled<K: Ord + Clone, V: Eq>(
    baseline: &BTreeMap<K, V>,
    current: &BTreeMap<K, V>,
    candidate: &BTreeSet<K>,
) -> Result<(), Violation<K>> {
    for (key, value) in baseline {
        if current.get(key) != Some(value) {
            return Err(Violation::ExistingChanged(key.clone()));
        }
    }
    for key in current.keys() {
        if !baseline.contains_key(key) && !candidate.contains(key) {
            return Err(Violation::ForeignActivated(key.clone()));
        }
    }
    Ok(())
}

pub(super) fn rollback_matches<K: Ord, V: Eq>(
    baseline: &BTreeMap<K, V>,
    current_preserved: &BTreeMap<K, V>,
    after: &BTreeMap<K, V>,
) -> bool {
    after == baseline || after == current_preserved
}

#[cfg(test)]
mod tests {
    use super::*;
    fn map(items: &[(&str, &[i64])]) -> BTreeMap<String, Vec<i64>> {
        items
            .iter()
            .map(|(k, v)| ((*k).into(), v.to_vec()))
            .collect()
    }
    #[test]
    fn own_new_targets_are_allowed_without_changing_original_fingerprints() {
        let before = map(&[("original", &[1920, 1080, 0, 0])]);
        let after = map(&[
            ("original", &[1920, 1080, 0, 0]),
            ("own-1", &[800, 600]),
            ("own-2", &[800, 600]),
        ]);
        let own = ["own-1".to_owned(), "own-2".to_owned()]
            .into_iter()
            .collect();
        assert_eq!(check_enabled(&before, &after, &own), Ok(()));
    }
    #[test]
    fn foreign_activation_is_rejected_before_deactivation() {
        let before = map(&[("original", &[1, 2])]);
        let after = map(&[("original", &[1, 2]), ("own", &[3]), ("foreign", &[4])]);
        let own = ["own".to_owned()].into_iter().collect();
        let mut deactivations = 0;
        let result = check_enabled(&before, &after, &own).map(|()| deactivations += 1);
        assert_eq!(result, Err(Violation::ForeignActivated("foreign".into())));
        assert_eq!(deactivations, 0);
    }
    #[test]
    fn changed_or_missing_original_output_is_rejected() {
        let before = map(&[("original", &[1920, 1080, 0, 0])]);
        for after in [
            map(&[]),
            map(&[("original", &[1920, 1080, 1, 0])]),
            map(&[("original", &[1280, 720, 0, 0])]),
        ] {
            assert_eq!(
                check_enabled(&before, &after, &BTreeSet::new()),
                Err(Violation::ExistingChanged("original".into()))
            );
        }
    }
    #[test]
    fn natural_baseline_restoration_and_preserved_external_state_both_finish_rollback() {
        let baseline = map(&[("original", &[1, 2])]);
        let preserved = map(&[("original", &[1, 2]), ("foreign", &[3, 4])]);
        assert!(rollback_matches(&baseline, &preserved, &baseline));
        assert!(rollback_matches(&baseline, &preserved, &preserved));
        assert_eq!(preserved["foreign"], [3, 4]);
    }
    #[test]
    fn partial_or_changed_external_layout_cannot_clear_pending_recovery() {
        let baseline = map(&[("original-1", &[1]), ("original-2", &[2])]);
        let preserved = map(&[
            ("original-1", &[1]),
            ("original-2", &[2]),
            ("foreign", &[3]),
        ]);
        for after in [
            map(&[("original-1", &[1])]),
            map(&[("original-1", &[9]), ("original-2", &[2])]),
            map(&[
                ("original-1", &[1]),
                ("original-2", &[2]),
                ("foreign", &[4]),
            ]),
        ] {
            assert!(!rollback_matches(&baseline, &preserved, &after));
        }
    }
    #[test]
    fn original_output_change_blocks_installation_but_can_be_preserved_during_owned_cleanup() {
        let original=map(&[("zako",&[1920,1080,0,0])]);
        let fresh=map(&[("zako",&[1920,1080,0,-1080]),("our-new-target",&[1920,1080,0,0])]);
        let owned=["our-new-target".to_owned()].into_iter().collect();
        assert!(matches!(check_enabled(&original,&fresh,&owned),Err(Violation::ExistingChanged(_))));
        let preserved=super::super::capacity_policy::foreign(&fresh,&owned);
        assert!(rollback_matches(&original,&preserved,&preserved));
        assert!(!rollback_matches(&original,&preserved,&fresh),"an owned output left active is not completed cleanup");
        assert!(!rollback_matches(&original,&preserved,&map(&[])),"cleanup cannot remove the last original output");
    }
}
