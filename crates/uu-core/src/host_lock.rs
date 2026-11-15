//! Host-owned workstation privacy. SessionGate serializes admission, expiry and
//! lock requests; a failed or renewed media connection never owns this policy.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    Never,
    #[default]
    Restore,
    Always,
}
impl Policy {
    pub const ALL: [Self; 3] = [Self::Never, Self::Restore, Self::Always];
    pub fn label(self) -> &'static str {
        uu_i18n::text(match self {
            Self::Never => "不锁屏",
            Self::Restore => "恢复连接前的锁屏状态",
            Self::Always => "始终锁屏",
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub policy: Policy,
    pub originally_locked: Option<bool>,
    pub sessions: usize,
    pub can_lock: bool,
    pub error: Option<String>,
}
impl Status {
    pub fn locks_on_exit(&self) -> bool {
        self.policy == Policy::Always
            || (self.policy == Policy::Restore && self.originally_locked == Some(true))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Desktop {
    pub session: u32,
    pub logon: i64,
    pub locked: bool,
}
pub(crate) trait Backend: Send + Sync {
    fn desktop(&self) -> Result<Desktop>;
    /// Must confirm the OS lock, not merely successful asynchronous submission.
    fn lock(&self, expected: Desktop) -> Result<()>;
}
struct Cycle {
    original: Option<Desktop>,
    streamed: bool,
}
#[derive(Default)]
struct Inner {
    backend: Option<Arc<dyn Backend>>,
    policy: Policy,
    cycle: Option<Cycle>,
    due: Option<Instant>,
    error: Option<String>,
}
#[derive(Default)]
pub(crate) struct State(Mutex<Inner>);
impl State {
    pub fn enable(&self, backend: Arc<dyn Backend>, policy: Policy) {
        let mut state = self.0.lock().unwrap();
        state.backend = Some(backend);
        state.policy = policy;
    }
    pub fn configure(&self, policy: Policy) {
        self.0.lock().unwrap().policy = policy;
    }
    pub fn begin(&self) {
        let mut state = self.0.lock().unwrap();
        // Reconnection during grace retains the first admission's snapshot.
        if state.cycle.is_some() {
            return;
        }
        state.error = None;
        let backend = state.backend.clone();
        let original = backend.and_then(|backend| match backend.desktop() {
            Ok(value) => Some(value),
            Err(error) => {
                state.error = Some(error.to_string());
                None
            }
        });
        state.cycle = Some(Cycle {
            original,
            streamed: false,
        });
    }
    pub fn connected(&self) {
        let mut state = self.0.lock().unwrap();
        state.due = None;
        if let Some(cycle) = &mut state.cycle {
            cycle.streamed = true;
        }
    }
    pub fn end(&self, explicit: bool, now: Instant) {
        let mut state = self.0.lock().unwrap();
        let armed = state.cycle.as_ref().is_some_and(|c| {
            c.streamed
                && c.original.is_some()
                && (state.policy == Policy::Always
                    || (state.policy == Policy::Restore && c.original.is_some_and(|d| d.locked)))
        });
        if !armed {
            state.cycle = None;
            state.due = None;
            return;
        }
        let due = now
            + if explicit {
                Duration::ZERO
            } else {
                Duration::from_secs(10)
            };
        state.due = Some(state.due.map_or(due, |old| old.min(due)));
    }
    pub fn status(&self, sessions: usize, allowed: bool) -> Option<Status> {
        let state = self.0.lock().unwrap();
        state.backend.as_ref()?;
        Some(Status {
            policy: state.policy,
            originally_locked: state
                .cycle
                .as_ref()
                .and_then(|c| c.original.map(|d| d.locked)),
            sessions,
            can_lock: allowed,
            error: state.error.clone(),
        })
    }
    pub fn lock_now(&self) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        let result = (|| {
            let backend = state
                .backend
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("{}", uu_i18n::text("当前主机不支持锁屏")))?;
            backend.lock(backend.desktop()?)
        })();
        state.error = result.as_ref().err().map(ToString::to_string);
        result
    }
    /// Caller holds SessionGate.active and has proved that no session remains.
    pub fn expire(&self, now: Instant, shutdown: bool) {
        let mut state = self.0.lock().unwrap();
        if !state.due.is_some_and(|due| shutdown || now >= due) {
            return;
        }
        let cycle = state.cycle.take();
        state.due = None;
        let Some(original) = cycle.and_then(|c| c.original) else {
            return;
        };
        if state.policy == Policy::Never || (state.policy == Policy::Restore && !original.locked) {
            return;
        }
        let Some(backend) = &state.backend else {
            return;
        };
        state.error = backend.lock(original).err().map(|error| {
            tracing::warn!(%error, "workstation lock after streaming failed");
            error.to_string()
        });
    }
}

pub(crate) struct Monitor {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Monitor {
    pub fn start(
        gate: &Arc<crate::shared_host::SessionGate>,
        policy: Policy,
    ) -> Result<Option<Self>> {
        #[cfg(all(windows, not(test)))]
        {
            gate.desktop.enable(Arc::new(windows::System), policy);
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stopping = stop.clone();
            let weak = Arc::downgrade(gate);
            let thread = std::thread::Builder::new()
                .name("host-workstation-lock".into())
                .spawn(move || {
                    while !stopping.load(std::sync::atomic::Ordering::Acquire) {
                        if let Some(gate) = weak.upgrade() {
                            gate.expire_desktop_lock(false);
                        } else {
                            break;
                        }
                        std::thread::park_timeout(Duration::from_millis(250));
                    }
                    if let Some(gate) = weak.upgrade() {
                        gate.expire_desktop_lock(true);
                    }
                })?;
            Ok(Some(Self {
                stop,
                thread: Some(thread),
            }))
        }
        #[cfg(any(not(windows), test))]
        {
            let _ = (gate, policy);
            Ok(None)
        }
    }
}
impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}
mod client;
#[cfg(windows)]
mod windows;
pub(crate) use client::Command;
pub use client::{Client, Completion};
#[cfg(test)]
mod tests;
