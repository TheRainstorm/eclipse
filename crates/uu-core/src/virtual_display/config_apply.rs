//! Ownership follows submission, including a driver error after APPLY starts.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    Validate,
    Apply,
}

pub(super) fn run<E>(
    apply_submitted: &mut bool,
    mut invoke: impl FnMut(Phase) -> Result<(), E>,
) -> Result<(), E> {
    invoke(Phase::Validate)?;
    // An error from APPLY does not prove that no display state was changed.
    // Its caller must keep ownership until exact cleanup has been verified.
    *apply_submitted = true;
    invoke(Phase::Apply)
}

/// Absence is proven only when every active target identity can be read.
/// In particular, a transient monitor-interface error is not a removed target.
pub(super) fn exact_index<K: Eq, E>(
    identities: impl IntoIterator<Item = Result<K, E>>,
    expected: &K,
) -> Result<Option<usize>, E> {
    identities
        .into_iter()
        .enumerate()
        .try_fold(None, |selected, (index, identity)| {
            let identity = identity?;
            Ok(selected.or_else(|| (identity == *expected).then_some(index)))
        })
}

/// Build one removal proposal for the complete owned set. Keeping siblings in
/// an intermediate proposal can make Windows reposition them before the next
/// removal. Every identity must be readable before a proposal is returned.
pub(super) fn retained_indices<K: Ord, E>(
    identities: impl IntoIterator<Item = Result<K, E>>,
    removing: &std::collections::BTreeSet<K>,
) -> Result<Vec<usize>, E> {
    identities.into_iter().enumerate().try_fold(Vec::new(), |mut kept, (index, identity)| {
        if !removing.contains(&identity?) { kept.push(index); }
        Ok(kept)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn retiring_a_pool_does_not_submit_an_intermediate_sibling_layout() {
        let current=BTreeMap::from([
            ("physical",vec![3072,1920,0]),
            ("pool-256",vec![800,600,3072]),
            ("pool-257",vec![800,600,3872]),
        ]);
        let removed=BTreeSet::from(["pool-256","pool-257"]);
        // This is the reported Windows readback after sequential removal:
        // the still-active sibling fills the gap and x becomes 3072.
        let mut sequential=current.clone();
        sequential.remove("pool-256");sequential.get_mut("pool-257").unwrap()[2]=3072;
        let only_first=BTreeSet::from(["pool-256"]);
        assert_ne!(super::super::capacity_policy::foreign(&current,&only_first),sequential);

        let paths:Vec<_>=current.keys().copied().collect();
        let kept=retained_indices(paths.iter().map(|key|Ok::<_,()>(*key)),&removed).unwrap();
        let proposal:BTreeMap<_,_>=kept.iter().map(|index|{
            let key=paths[*index];(key,current[key].clone())
        }).collect();
        let mut applied=0;let mut submitted=false;
        run(&mut submitted,|phase|{
            assert_eq!(proposal.keys().copied().collect::<Vec<_>>(),["physical"]);
            if phase==Phase::Apply {applied+=1;}
            Ok::<_,()>(())
        }).unwrap();
        assert!(submitted);assert_eq!(applied,1);
        assert_eq!(proposal,super::super::capacity_policy::foreign(&current,&removed));
    }

    #[test]
    fn removal_membership_uses_the_full_identity_and_retains_every_other_output() {
        let keys=[("adapter-a",256,"monitor-old"),("adapter-a",257,"pool-sibling"),
            ("adapter-a",256,"monitor-replacement"),("adapter-b",256,"monitor-old")];
        let removed=BTreeSet::from([keys[0],keys[1]]);
        assert_eq!(retained_indices(keys.map(Ok::<_,()>),&removed),Ok(vec![2,3]));
        let one=BTreeSet::from([keys[0]]);
        assert_eq!(retained_indices(keys.map(Ok::<_,()>),&one),Ok(vec![1,2,3]),"single-target cleanup must preserve a sibling still in use");
        assert_eq!(retained_indices([Ok(keys[0]),Err("identity unavailable")],&removed),Err("identity unavailable"));
    }

    #[test]
    fn validation_failure_never_submits_apply() {
        let mut submitted = false;
        let mut phases = Vec::new();
        let result = run(&mut submitted, |phase| {
            phases.push(phase);
            Err("validation rejected")
        });
        assert_eq!(result, Err("validation rejected"));
        assert_eq!(phases, [Phase::Validate]);
        assert!(!submitted);
    }

    #[test]
    fn apply_error_retains_submission_state() {
        let mut submitted = false;
        let mut phases = Vec::new();
        let result = run(&mut submitted, |phase| {
            phases.push(phase);
            match phase {
                Phase::Validate => Ok(()),
                Phase::Apply => Err("driver failed after submission"),
            }
        });
        assert_eq!(result, Err("driver failed after submission"));
        assert_eq!(phases, [Phase::Validate, Phase::Apply]);
        assert!(submitted);
    }

    #[test]
    fn successful_apply_retains_submission_state() {
        let mut submitted = false;
        let mut phases = Vec::new();
        let result: Result<(), ()> = run(&mut submitted, |phase| {
            phases.push(phase);
            Ok(())
        });
        assert_eq!(result, Ok(()));
        assert_eq!(phases, [Phase::Validate, Phase::Apply]);
        assert!(submitted);
    }

    #[test]
    fn later_validation_error_cannot_erase_an_earlier_submission() {
        let mut submitted = true;
        assert_eq!(run(&mut submitted, |_| Err("rejected")), Err("rejected"));
        assert!(submitted);
    }

    #[test]
    fn unreadable_active_identity_cannot_prove_absence() {
        assert_eq!(
            exact_index(
                [Ok("other"), Err("monitor interface unavailable")],
                &"owned"
            ),
            Err("monitor interface unavailable")
        );
    }

    #[test]
    fn matching_target_does_not_hide_a_later_unreadable_identity() {
        assert_eq!(
            exact_index(
                [Ok("owned"), Err("monitor interface unavailable")],
                &"owned"
            ),
            Err("monitor interface unavailable")
        );
    }

    #[test]
    fn fully_readable_identities_distinguish_exact_match_and_absence() {
        let identities: [Result<&str, ()>; 2] = [Ok("other"), Ok("owned")];
        assert_eq!(exact_index(identities, &"owned"), Ok(Some(1)));
        assert_eq!(exact_index(identities, &"replaced-monitor"), Ok(None));
    }
}
