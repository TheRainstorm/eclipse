//! Eligibility for the protected host's current physical console. An empty
//! sign-in screen keeps the same installed identity and remote authorization.
#[derive(Clone, Copy, Debug)]
pub(super) enum Identity<'a> {
    User(&'a str),
    NoToken { empty_username: bool },
    Unavailable,
}

pub(super) fn permitted(session: u32, owner: &str, identity: Identity<'_>) -> bool {
    session != 0 && session != u32::MAX && match identity {
        Identity::User(user) => !owner.is_empty() && user == owner,
        Identity::NoToken { empty_username } => !owner.is_empty() && empty_username,
        Identity::Unavailable => false,
    }
}
pub(super) fn must_retire(child_session: u32, eligible_session: Option<u32>) -> bool {
    Some(child_session) != eligible_session
}

#[cfg(test)]
mod tests {
    use super::*;
    const OWNER: &str = "S-1-5-21-owner";
    #[test]
    fn sign_in_requires_valid_console_explicit_no_token_and_verified_empty_name() {
        for session in [0, u32::MAX] {
            assert!(!permitted(session, OWNER, Identity::NoToken { empty_username: true }));
            assert!(!permitted(session, OWNER, Identity::User(OWNER)));
        }
        assert!(permitted(1, OWNER, Identity::NoToken { empty_username: true }));
        assert!(!permitted(1, OWNER, Identity::NoToken { empty_username: false }));
        assert!(!permitted(1, OWNER, Identity::Unavailable));
        assert!(!permitted(1, "", Identity::NoToken { empty_username: true }));
    }
    #[test]
    fn owner_signout_signin_keeps_eligibility_but_foreign_login_revokes_it() {
        let states = [Identity::User(OWNER), Identity::NoToken { empty_username: true },
            Identity::User(OWNER), Identity::User("S-1-5-21-foreign"), Identity::Unavailable];
        let eligible: Vec<_> = states.into_iter().map(|s| permitted(1, OWNER, s)).collect();
        assert_eq!(eligible, [true, true, true, false, false]);
    }
    #[test]
    fn no_token_with_nonempty_name_is_a_transition_not_a_sign_in_screen() {
        assert!(!permitted(3, OWNER, Identity::NoToken { empty_username: false }));
        assert!(permitted(3, OWNER, Identity::User(OWNER)));
        assert!(!permitted(3, OWNER, Identity::User("S-1-5-18")));
    }
    #[test]
    fn child_lifetime_follows_console_identity_across_logon_and_session_changes() {
        let states = [(1, Identity::User(OWNER)),
            (1, Identity::NoToken { empty_username: true }), (1, Identity::User(OWNER)),
            (1, Identity::User("S-1-5-21-foreign")), (1, Identity::Unavailable),
            (0, Identity::NoToken { empty_username: true }),
            (u32::MAX, Identity::NoToken { empty_username: true }), (2, Identity::User(OWNER))];
        let retired: Vec<_> = states.into_iter().map(|(session, identity)| {
            let next = permitted(session, OWNER, identity).then_some(session);
            must_retire(1, next)
        }).collect();
        assert_eq!(retired, [false, false, false, true, true, true, true, true]);
    }
}
