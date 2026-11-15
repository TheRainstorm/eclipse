use super::Status;
use anyhow::{ensure, Context, Result};
use std::{
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};

pub(crate) enum Command {
    Lock(u64),
}
pub struct Completion {
    pub disconnect: bool,
    pub error: Option<String>,
}
struct Pending {
    id: u64,
    disconnect: bool,
    since: Instant,
}
#[derive(Default)]
struct State {
    channel: Option<Weak<webrtc::data_channel::RTCDataChannel>>,
    status: Option<Status>,
    sender: Option<mpsc::Sender<Command>>,
    next: u64,
    pending: Option<Pending>,
    result: Option<Completion>,
    ended: Option<oneshot::Sender<()>>,
}
#[derive(Default)]
pub struct Client(Mutex<State>);
impl Client {
    pub fn status(&self) -> Option<Status> {
        self.0.lock().unwrap().status.clone()
    }
    pub fn busy(&self) -> bool {
        self.0.lock().unwrap().pending.is_some()
    }
    pub fn lock(&self, disconnect: bool) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        ensure!(
            state.status.as_ref().is_some_and(|s| s.can_lock),
            "{}",
            uu_i18n::text("当前会话没有锁定远端的权限")
        );
        ensure!(
            state.pending.is_none(),
            "{}",
            uu_i18n::text("正在等待远端确认锁屏")
        );
        state.next += 1;
        let id = state.next;
        state
            .sender
            .as_ref()
            .context("锁屏通路未连接")?
            .try_send(Command::Lock(id))
            .context("锁屏请求未发送")?;
        state.pending = Some(Pending {
            id,
            disconnect,
            since: Instant::now(),
        });
        state.result = None;
        Ok(())
    }
    pub fn take_result(&self) -> Option<Completion> {
        let mut state = self.0.lock().unwrap();
        if state
            .pending
            .as_ref()
            .is_some_and(|p| p.since.elapsed() >= Duration::from_secs(6))
        {
            state.pending = None;
            state.result = Some(Completion {
                disconnect: false,
                error: Some(uu_i18n::text("远端锁屏结果未确认，请检查主机；当前串流已保留").into()),
            });
        }
        state.result.take()
    }
    pub(crate) fn attach(
        &self,
        channel: &Arc<webrtc::data_channel::RTCDataChannel>,
    ) -> mpsc::Receiver<Command> {
        let (sender, receiver) = mpsc::channel(4);
        let mut state = self.0.lock().unwrap();
        state.channel = Some(Arc::downgrade(channel));
        state.status = None;
        state.sender = Some(sender);
        state.pending = None;
        state.result = None;
        state.ended = None;
        receiver
    }
    pub(crate) fn status_received(&self, status: Status) {
        self.0.lock().unwrap().status = Some(status);
    }
    pub(crate) fn locked(&self, id: u64, error: Option<String>) {
        let mut state = self.0.lock().unwrap();
        if !state.pending.as_ref().is_some_and(|p| p.id == id) {
            return;
        }
        let pending = state.pending.take().unwrap();
        state.result = Some(Completion {
            disconnect: pending.disconnect && error.is_none(),
            error,
        });
    }
    pub(crate) fn ended(&self) {
        if let Some(done) = self.0.lock().unwrap().ended.take() {
            let _ = done.send(());
        }
    }
    pub(crate) fn retire(&self) {
        let mut state = self.0.lock().unwrap();
        state.channel = None;
        state.status = None;
        state.sender = None;
        state.ended = None;
    }
    /// Drive this alongside the still-live viewer future. Unsupported or failed
    /// peers use the host's conservative disconnect grace instead.
    pub(crate) async fn ending(&self) {
        let (receiver, channel) = {
            let mut state = self.0.lock().unwrap();
            if state.status.is_none() {
                return;
            }
            let (send, receive) = oneshot::channel();
            let Some(channel) = state.channel.as_ref().and_then(Weak::upgrade) else {
                return;
            };
            state.ended = Some(send);
            (receive, channel)
        };
        let _ = tokio::time::timeout(Duration::from_millis(600), async {
            crate::native_control::send_session_ending(&channel).await?;
            receiver.await.context("会话退出确认丢失")
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_lock::Policy;
    #[test]
    fn client_waits_for_matching_confirmation_and_keeps_connection_on_error() {
        let client = Client::default();
        // The queue test does not create a peer or touch a real workstation.
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        {
            let mut state = client.0.lock().unwrap();
            state.sender = Some(sender);
            state.status = Some(Status {
                policy: Policy::Restore,
                originally_locked: Some(true),
                sessions: 1,
                can_lock: true,
                error: None,
            });
        }
        client.lock(true).unwrap();
        assert!(client.busy());
        let Command::Lock(id) = receiver.try_recv().unwrap();
        client.locked(id + 1, None);
        assert!(client.take_result().is_none());
        client.locked(id, Some("failure".into()));
        let result = client.take_result().unwrap();
        assert!(!result.disconnect);
        assert!(result.error.is_some());
        client.lock(true).unwrap();
        let Command::Lock(id) = receiver.try_recv().unwrap();
        client.locked(id, None);
        assert!(client.take_result().unwrap().disconnect);
    }
    #[test]
    fn unknown_lock_result_does_not_close_the_stream() {
        let client = Client::default();
        client.0.lock().unwrap().pending = Some(Pending {
            id: 1,
            disconnect: true,
            since: Instant::now() - Duration::from_secs(7),
        });
        let result = client.take_result().unwrap();
        assert!(!result.disconnect);
        assert!(result.error.is_some());
        assert!(!client.busy());
    }
}
