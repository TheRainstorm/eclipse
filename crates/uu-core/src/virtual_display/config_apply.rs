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

#[cfg(test)]
mod tests {
    use super::*;

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
