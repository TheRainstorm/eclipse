//! User actions follow the host preparation state; inspecting never starts work.
use uu_core::virtual_display::preparation::{Phase, Status};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Prepare,
    Recover,
    BackgroundSetup,
}

pub(super) fn action(status: &Status, foreground: bool) -> Option<Action> {
    if matches!(status.phase, Phase::Unsupported | Phase::Preparing)
        || (matches!(status.phase,Phase::WaitingForIdle|Phase::RecoveryPending) && status.queued)
    {
        return None;
    }
    if (foreground && (status.phase != Phase::Ready || status.available == 0))
        || status.phase == Phase::NeedsElevation
    {
        return Some(Action::BackgroundSetup);
    }
    match status.phase {
        Phase::NeedsDriver | Phase::NeedsCapacity | Phase::Failed | Phase::WaitingForIdle => {
            Some(Action::Prepare)
        }
        Phase::RecoveryPending => Some(Action::Recover),
        _ => None,
    }
}
pub(super) fn can_create(status: Option<&Status>, available: usize, foreground: bool) -> bool {
    let Some(status) = status else {
        return true;
    };
    if matches!(
        status.phase,
        Phase::Unsupported | Phase::RecoveryPending | Phase::Preparing
    ) {
        return false;
    }
    // Reusing an existing free target needs no driver reload or new privilege.
    if available > 0 {
        return true;
    }
    !foreground
        && !matches!(status.phase, Phase::Preparing | Phase::NeedsElevation)
        && !(status.phase == Phase::WaitingForIdle && status.queued)
}
pub(super) fn active(status: &Status) -> bool {
    status.phase == Phase::Preparing || (matches!(status.phase,Phase::WaitingForIdle|Phase::RecoveryPending) && status.queued)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn status(phase: Phase) -> Status {
        Status {
            phase,
            configured_capacity: Some(16),
            targets: 16,
            available: 0,
            requested_capacity: 16,
            queued: false,
            error: None,
        }
    }
    #[test]
    fn existing_free_targets_remain_usable_without_new_privilege_or_idle() {
        for phase in [
            Phase::NeedsElevation,
            Phase::WaitingForIdle,
            Phase::NeedsCapacity,
            Phase::Failed,
        ] {
            let mut state = status(phase);
            state.available = 1;
            assert!(can_create(Some(&state), 1, true));
        }
        assert!(!can_create(Some(&status(Phase::RecoveryPending)), 1, true));
        assert!(!can_create(Some(&status(Phase::Unsupported)), 1, false));
        assert!(
            !can_create(Some(&status(Phase::Preparing)), 1, false),
            "an accepted in-flight mutation cannot reuse cached capacity"
        );
    }
    #[test]
    fn foreground_capacity_preparation_routes_to_background_instead_of_elevating_itself() {
        for phase in [
            Phase::NeedsDriver,
            Phase::NeedsCapacity,
            Phase::NeedsElevation,
            Phase::Failed,
            Phase::RecoveryPending,
        ] {
            assert_eq!(action(&status(phase), true), Some(Action::BackgroundSetup));
            assert!(!can_create(Some(&status(phase)), 0, true));
        }
        assert!(can_create(None, 0, true));
    }
    #[test]
    fn queued_work_cannot_repeat_or_be_mistaken_for_a_recovery_request() {
        for phase in [Phase::WaitingForIdle, Phase::Preparing,Phase::RecoveryPending] {
            let mut state = status(phase);
            state.queued = true;
            assert!(active(&state));
            assert_eq!(action(&state, false), None);
            assert!(!can_create(Some(&state), 0, false));
        }
        assert_eq!(
            action(&status(Phase::RecoveryPending), false),
            Some(Action::Recover)
        );
        assert_eq!(
            action(&status(Phase::NeedsCapacity), false),
            Some(Action::Prepare)
        );
    }
    #[test]
    fn merely_observing_a_busy_host_does_not_claim_an_intent_already_exists() {
        let state = status(Phase::WaitingForIdle);
        assert!(!active(&state));
        assert_eq!(action(&state, false), Some(Action::Prepare));
        assert!(can_create(Some(&state), 0, false));
        assert!(!can_create(Some(&state), 0, true));
    }
}
