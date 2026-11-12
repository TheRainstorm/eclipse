//! Host-owned admission preferences and temporary keyboard/mouse priority.
//! Gamepads are not part of the physical keyboard/mouse pause.
use serde::{Deserialize, Serialize};
use std::{sync::Mutex, time::{Duration, Instant}};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub game_room: bool,
    pub local_priority: bool,
    pub automatic_resume: bool,
}
impl Default for Policy {
    fn default() -> Self { Self { game_room: false, local_priority: true, automatic_resume: true } }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub policy: Policy,
    pub keyboard_mouse_paused: bool,
    pub manually_paused: bool,
    pub local_activity_supported: bool,
}

pub(crate) struct Gate {
    pub paused: bool,
    pub accept_after: Instant,
}
struct Runtime {
    policy: Policy,
    manual: bool,
    resumed_revision: u64,
    changed: Instant,
}
pub struct State(Mutex<Runtime>);
impl Default for State {
    fn default() -> Self {
        Self(Mutex::new(Runtime { policy: Policy::default(), manual: false, resumed_revision: 0, changed: Instant::now() }))
    }
}
impl State {
    pub fn policy(&self) -> Policy { self.0.lock().unwrap().policy }
    pub(crate) fn configure(&self, policy: Policy) {
        let mut state = self.0.lock().unwrap();
        if state.policy != policy { state.policy = policy; state.changed = Instant::now(); }
    }
    pub(crate) fn pause(&self, paused: bool) {
        let mut state = self.0.lock().unwrap();
        state.manual = paused;
        state.changed = Instant::now();
        if !paused { state.resumed_revision = uu_input::local_activity::snapshot().revision; }
    }
    fn sample(&self, now: Instant, activity: uu_input::local_activity::Snapshot) -> Gate {
        let state = self.0.lock().unwrap();
        let mut gate = Gate { paused: state.manual, accept_after: state.changed };
        if state.policy.local_priority && activity.available && activity.held { gate.paused = true; }
        if state.policy.local_priority && activity.available && activity.revision > state.resumed_revision {
            if let Some(last) = activity.last {
                let deadline = last + Duration::from_secs(3);
                gate.paused |= activity.held || !state.policy.automatic_resume || now < deadline;
                gate.accept_after = gate.accept_after.max(deadline);
            }
        }
        gate
    }
    pub(crate) fn keyboard_mouse(&self) -> Gate {
        self.sample(Instant::now(), uu_input::local_activity::snapshot())
    }
    pub fn snapshot(&self) -> Snapshot {
        let activity = uu_input::local_activity::snapshot();
        let gate = self.sample(Instant::now(), activity);
        let state = self.0.lock().unwrap();
        Snapshot { policy: state.policy, keyboard_mouse_paused: gate.paused, manually_paused: state.manual,
            local_activity_supported: activity.available }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_activity_pauses_until_idle_and_never_replays_queued_input() {
        let state = State::default();
        let now = Instant::now();
        let activity = uu_input::local_activity::Snapshot { available: true, last: Some(now), held: false, revision: 1 };
        let paused = state.sample(now + Duration::from_secs(1), activity);
        assert!(paused.paused);
        assert!(paused.accept_after >= now + Duration::from_secs(3));
        assert!(!state.sample(now + Duration::from_secs(4), activity).paused);
        assert!(state.sample(now + Duration::from_secs(4), uu_input::local_activity::Snapshot { held: true, ..activity }).paused);
        state.configure(Policy { automatic_resume: false, ..Default::default() });
        assert!(state.sample(now + Duration::from_secs(30), activity).paused);
        state.0.lock().unwrap().resumed_revision = 1;
        assert!(!state.sample(now + Duration::from_secs(30), activity).paused);
        assert!(state.sample(now + Duration::from_secs(30), uu_input::local_activity::Snapshot { revision: 2, ..activity }).paused);
        state.configure(Policy { local_priority: false, ..Default::default() });
        assert!(!state.sample(now, activity).paused);
        state.pause(true);
        assert!(state.sample(now, activity).paused);
    }
}
