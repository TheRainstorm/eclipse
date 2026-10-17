//! Session state machine.
//!
//! The official product is one binary that can act in either direction; we
//! keep the same duality but make the role explicit and testable.

use crate::error::CoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// This machine is being controlled.
    Host,
    /// This machine is viewing/controlling another.
    Controller,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    Authenticating,
    Negotiating,
    Streaming,
    Closing,
    Closed,
}

impl SessionState {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionState::Idle => "idle",
            SessionState::Authenticating => "authenticating",
            SessionState::Negotiating => "negotiating",
            SessionState::Streaming => "streaming",
            SessionState::Closing => "closing",
            SessionState::Closed => "closed",
        }
    }
}

/// Legal transitions. Keeping this table explicit means an unexpected
/// message from the service surfaces as an error instead of silent drift.
const TRANSITIONS: &[(SessionState, SessionState)] = &[
    (SessionState::Idle, SessionState::Authenticating),
    (SessionState::Authenticating, SessionState::Negotiating),
    (SessionState::Negotiating, SessionState::Streaming),
    (SessionState::Streaming, SessionState::Closing),
    (SessionState::Negotiating, SessionState::Closing),
    (SessionState::Authenticating, SessionState::Closing),
    (SessionState::Closing, SessionState::Closed),
    // Teardown paths are idempotent.
    (SessionState::Idle, SessionState::Closing),
    (SessionState::Closed, SessionState::Closed),
];

#[derive(Debug)]
pub struct Session {
    pub role: Role,
    state: SessionState,
}

impl Session {
    pub fn new(role: Role) -> Self {
        Self { role, state: SessionState::Idle }
    }

    pub fn state(&self) -> SessionState {
        self.state
    }

    pub fn transition(&mut self, to: SessionState) -> Result<(), CoreError> {
        if self.state == to && to == SessionState::Closed {
            return Ok(());
        }
        if TRANSITIONS.iter().any(|(from, target)| *from == self.state && *target == to) {
            tracing::debug!("session {} -> {}", self.state.as_str(), to.as_str());
            self.state = to;
            Ok(())
        } else {
            Err(CoreError::BadTransition { from: self.state.as_str(), to: to.as_str() })
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self.state,
            SessionState::Authenticating | SessionState::Negotiating | SessionState::Streaming
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path() {
        let mut s = Session::new(Role::Controller);
        s.transition(SessionState::Authenticating).unwrap();
        s.transition(SessionState::Negotiating).unwrap();
        s.transition(SessionState::Streaming).unwrap();
        assert!(s.is_active());
        s.transition(SessionState::Closing).unwrap();
        s.transition(SessionState::Closed).unwrap();
        assert!(!s.is_active());
    }

    #[test]
    fn cannot_jump_straight_to_streaming() {
        let mut s = Session::new(Role::Host);
        let err = s.transition(SessionState::Streaming).unwrap_err();
        assert!(matches!(err, CoreError::BadTransition { .. }));
    }

    #[test]
    fn closing_is_idempotent() {
        let mut s = Session::new(Role::Host);
        s.transition(SessionState::Closing).unwrap();
        s.transition(SessionState::Closed).unwrap();
        s.transition(SessionState::Closed).unwrap();
    }
}
