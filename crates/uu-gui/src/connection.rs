//! Own one client controller through its complete retirement. A stop request
//! keeps the handle alive; only the core's `stopped` acknowledgement retires it.
use std::sync::atomic::Ordering;
use uu_core::controller::ControllerHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AfterClose {
    Disconnected,
    Reconnect,
}
#[derive(Default)]
enum State {
    #[default]
    Idle,
    Live(ControllerHandle),
    Closing {
        handle: ControllerHandle,
        after: AfterClose,
    },
}
#[derive(Default)]
pub(crate) struct Controller(State);
impl Controller {
    pub fn new(handle: ControllerHandle) -> Self {
        Self(State::Live(handle))
    }
    /// Closing controllers remain available for input release and observation.
    pub fn as_ref(&self) -> Option<&ControllerHandle> {
        match &self.0 {
            State::Idle => None,
            State::Live(handle) | State::Closing { handle, .. } => Some(handle),
        }
    }
    pub fn as_mut(&mut self) -> Option<&mut ControllerHandle> {
        match &mut self.0 {
            State::Idle => None,
            State::Live(handle) | State::Closing { handle, .. } => Some(handle),
        }
    }
    pub fn is_some(&self) -> bool {
        self.as_ref().is_some()
    }
    pub fn is_none(&self) -> bool {
        self.as_ref().is_none()
    }
    pub fn closing(&self) -> bool {
        matches!(self.0, State::Closing { .. })
    }
    pub fn stopped(&self) -> bool {
        self.as_ref()
            .is_some_and(|handle| handle.stopped.load(Ordering::Acquire))
    }
    /// The caller releases GUI input before the first close request. Repeated
    /// requests are idempotent; an explicit disconnect cancels a pending reopen.
    pub fn request_close(&mut self, after: AfterClose) {
        self.0 = match std::mem::take(&mut self.0) {
            State::Idle => State::Idle,
            State::Live(handle) => {
                let _ = handle.close.send(());
                State::Closing { handle, after }
            }
            State::Closing {
                handle,
                after: current,
            } => State::Closing {
                handle,
                after: if after == AfterClose::Disconnected {
                    after
                } else {
                    current
                },
            },
        };
    }
    /// Called after GUI cleanup that still needs the handle. Core completion is
    /// monotonic and follows input, transport and runtime teardown.
    pub fn retire_stopped(&mut self) -> Option<AfterClose> {
        if !self.stopped() {
            return None;
        }
        match std::mem::take(&mut self.0) {
            State::Idle => None,
            State::Live(_) => Some(AfterClose::Disconnected),
            State::Closing { after, .. } => Some(after),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::AtomicBool, Arc};
    fn fixture() -> (
        Controller,
        Arc<AtomicBool>,
        tokio::sync::mpsc::UnboundedReceiver<()>,
    ) {
        let stopped = Arc::new(AtomicBool::new(false));
        let (_, events) = std::sync::mpsc::channel();
        let (_, frames) = uu_core::media::frame_channel();
        let (input, _) = tokio::sync::mpsc::unbounded_channel();
        let (settings, _) = tokio::sync::mpsc::unbounded_channel();
        let (close, requests) = tokio::sync::mpsc::unbounded_channel();
        (
            Controller::new(ControllerHandle {
                stopped: stopped.clone(),
                events,
                frames,
                input,
                settings,
                close,
                native_input: None,
                stats: Default::default(),
            }),
            stopped,
            requests,
        )
    }
    #[test]
    fn close_keeps_ownership_and_reconnect_waits_for_core_retirement() {
        let (mut owner, stopped, mut requests) = fixture();
        owner.request_close(AfterClose::Reconnect);
        owner.request_close(AfterClose::Reconnect);
        assert!(owner.closing());
        assert!(owner.as_ref().is_some());
        assert_eq!(requests.try_recv(), Ok(()));
        assert!(requests.try_recv().is_err(), "close must be sent once");
        assert_eq!(owner.retire_stopped(), None);
        stopped.store(true, Ordering::Release);
        assert_eq!(owner.retire_stopped(), Some(AfterClose::Reconnect));
        assert!(owner.is_none());
        assert!(!owner.closing());
        assert_eq!(owner.retire_stopped(), None);
    }
    #[test]
    fn explicit_disconnect_cannot_be_reversed_by_a_late_reconnect_request() {
        let (mut owner, stopped, mut requests) = fixture();
        owner.request_close(AfterClose::Reconnect);
        owner.request_close(AfterClose::Disconnected);
        owner.request_close(AfterClose::Reconnect);
        assert_eq!(requests.try_recv(), Ok(()));
        assert!(requests.try_recv().is_err());
        stopped.store(true, Ordering::Release);
        assert_eq!(owner.retire_stopped(), Some(AfterClose::Disconnected));
    }
    #[test]
    fn unexpected_completion_retires_without_scheduling_reconnection() {
        let (mut owner, stopped, mut requests) = fixture();
        assert_eq!(owner.retire_stopped(), None);
        stopped.store(true, Ordering::Release);
        assert_eq!(owner.retire_stopped(), Some(AfterClose::Disconnected));
        assert!(owner.is_none());
        assert!(requests.try_recv().is_err());
    }
}
