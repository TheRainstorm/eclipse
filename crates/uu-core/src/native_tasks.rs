//! Background work owned by one native media phase. Registration and closing
//! share one state lock, so a late data-channel callback cannot detach new work
//! after shutdown has already drained the existing tasks.
use std::{future::Future, sync::Mutex, time::Duration};
use tokio::{sync::watch, task::JoinHandle};

struct Task {
    name: &'static str,
    handle: JoinHandle<()>,
}
#[derive(Default)]
struct State {
    closing: bool,
    tasks: Vec<Task>,
}
pub(crate) struct Tasks {
    state: Mutex<State>,
    stopping: watch::Sender<bool>,
    joining: tokio::sync::Mutex<()>,
}
impl Default for Tasks {
    fn default() -> Self {
        Self {
            state: Mutex::new(State::default()),
            stopping: watch::channel(false).0,
            joining: tokio::sync::Mutex::new(()),
        }
    }
}

#[derive(Default, Debug, PartialEq, Eq)]
pub(crate) struct Summary {
    pub normal: usize,
    pub aborted: usize,
    pub failed: usize,
}
impl Tasks {
    /// The factory's synchronous setup runs only while registration is open.
    /// In particular, rejected late callbacks cannot change clipboard epochs.
    pub fn spawn<F, Make>(&self, name: &'static str, make: Make) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
        Make: FnOnce(watch::Receiver<bool>) -> F,
    {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::error!(task = name, "native auxiliary task has no runtime");
            return false;
        };
        let Ok(mut state) = self.state.lock() else {
            tracing::error!(task = name, "native auxiliary ownership state is poisoned");
            return false;
        };
        if state.closing {
            return false;
        }
        let future = make(self.stopping.subscribe());
        state.tasks.push(Task {
            name,
            handle: runtime.spawn(future),
        });
        true
    }

    pub fn begin_close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.closing = true;
        self.stopping.send_replace(true);
    }

    /// Called after closing the peer's channels. Cooperative cleanup can then
    /// await channel.close without holding another transport's close operation.
    pub async fn finish(&self) -> Summary {
        self.begin_close();
        let _join = self.joining.lock().await;
        let tasks = std::mem::take(
            &mut self
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .tasks,
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let mut summary = Summary::default();
        for Task { name, mut handle } in tasks {
            match tokio::time::timeout_at(deadline, &mut handle).await {
                Ok(Ok(())) => summary.normal += 1,
                Ok(Err(error)) => {
                    summary.failed += 1;
                    tracing::error!(task=name, %error, "native auxiliary task failed during shutdown");
                }
                Err(_) => {
                    summary.aborted += 1;
                    handle.abort();
                    // Dropping a handle would detach its future, still retaining
                    // its RTCDataChannel/SettingEngine and original UDP socket.
                    let _ = handle.await;
                    tracing::error!(
                        task = name,
                        "native auxiliary task exceeded two-second shutdown budget (aborted)"
                    );
                }
            }
        }
        if summary != Summary::default() {
            eprintln!(
                "{}",
                serde_json::json!({"event":"native_auxiliary_task_shutdown",
                "normal":summary.normal,"aborted":summary.aborted,"failed":summary.failed})
            );
        }
        summary
    }
}
impl Drop for Tasks {
    fn drop(&mut self) {
        self.stopping.send_replace(true);
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        for task in &state.tasks {
            task.handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn closing_retires_owned_resources_and_rejects_late_setup() {
        let tasks = Tasks::default();
        let resource = Arc::new(());
        let weak = Arc::downgrade(&resource);
        let (started, entered) = tokio::sync::oneshot::channel();
        assert!(
            tasks.spawn("generated cooperative task", move |mut stop| async move {
                let _resource = resource;
                let _ = started.send(());
                if !*stop.borrow_and_update() {
                    let _ = stop.changed().await;
                }
            })
        );
        entered.await.unwrap();
        tasks.begin_close();
        let setup = Arc::new(AtomicBool::new(false));
        let setup_copy = setup.clone();
        assert!(!tasks.spawn("late task", move |_| {
            setup_copy.store(true, Ordering::Release);
            async {}
        }));
        assert!(!setup.load(Ordering::Acquire));
        assert_eq!(
            tasks.finish().await,
            Summary {
                normal: 1,
                aborted: 0,
                failed: 0
            }
        );
        assert_eq!(weak.strong_count(), 0);
        assert_eq!(tasks.finish().await, Summary::default());
    }

    #[tokio::test]
    async fn uncooperative_task_is_reported_and_joined_after_abort() {
        let tasks = Tasks::default();
        let resource = Arc::new(());
        let weak = Arc::downgrade(&resource);
        let (started, entered) = tokio::sync::oneshot::channel();
        tasks.spawn("generated uncooperative task", move |_| async move {
            let _resource = resource;
            let _ = started.send(());
            std::future::pending::<()>().await;
        });
        entered.await.unwrap();
        assert_eq!(
            tasks.finish().await,
            Summary {
                normal: 0,
                aborted: 1,
                failed: 0
            }
        );
        assert_eq!(
            weak.strong_count(),
            0,
            "abort must be awaited before reporting retirement"
        );
    }
}
