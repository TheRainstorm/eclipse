use std::{sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OutputStatus {
    PresentedHdr,
    Unavailable,
    TimedOut,
}

/// Each media offer asks its own viewer about the current output. The caller
/// drives the surface on the GUI thread; the network task only awaits a reply.
pub struct OutputRequest {
    send: mpsc::Sender<oneshot::Sender<bool>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}
pub struct WindowRequests {
    receive: mpsc::Receiver<oneshot::Sender<bool>>,
    pending: Option<oneshot::Sender<bool>>,
}
pub fn channel(wake: impl Fn() + Send + Sync + 'static) -> (OutputRequest, WindowRequests) {
    let (send, receive) = mpsc::channel(1);
    (
        OutputRequest {
            send,
            wake: Arc::new(wake),
        },
        WindowRequests {
            receive,
            pending: None,
        },
    )
}
impl OutputRequest {
    pub(crate) async fn supported(&self) -> OutputStatus {
        self.resolve(Duration::from_millis(600)).await
    }
    async fn resolve(&self, timeout: Duration) -> OutputStatus {
        let (reply, result) = oneshot::channel();
        if self.send.try_send(reply).is_err() {
            return OutputStatus::Unavailable;
        }
        (self.wake)();
        match tokio::time::timeout(timeout, result).await {
            Ok(Ok(true)) => OutputStatus::PresentedHdr,
            Ok(Ok(false) | Err(_)) => OutputStatus::Unavailable,
            Err(_) => OutputStatus::TimedOut,
        }
    }
}
impl WindowRequests {
    pub fn pending(&mut self) -> bool {
        while self.pending.as_ref().is_none_or(oneshot::Sender::is_closed) {
            self.pending = self.receive.try_recv().ok();
            if self.pending.is_none() {
                return false;
            }
        }
        true
    }
    pub fn answer(&mut self, supported: bool) {
        if let Some(reply) = self.pending.take() {
            let _ = reply.send(supported);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn requests_follow_each_offer_and_its_own_window() {
        let (a, mut window_a) = channel(|| {});
        let (b, mut window_b) = channel(|| {});
        let window = async {
            tokio::task::yield_now().await;
            assert!(window_a.pending() && window_b.pending());
            window_b.answer(false);
            window_a.answer(true);
        };
        let (first, other, _) = tokio::join!(a.supported(), b.supported(), window);
        assert_eq!(
            (first, other),
            (OutputStatus::PresentedHdr, OutputStatus::Unavailable)
        );
        let (second, _) = tokio::join!(a.supported(), async {
            tokio::task::yield_now().await;
            assert!(window_a.pending());
            window_a.answer(false);
        });
        assert_eq!(second, OutputStatus::Unavailable);
    }
    #[tokio::test]
    async fn closed_inactive_and_timed_out_requests_remain_sdr() {
        let (request, mut window) = channel(|| {});
        assert_eq!(
            request.resolve(Duration::ZERO).await,
            OutputStatus::TimedOut
        );
        assert!(!window.pending());
        let (answer, _) = tokio::join!(request.supported(), async {
            tokio::task::yield_now().await;
            assert!(window.pending());
            drop(window);
        });
        assert_eq!(answer, OutputStatus::Unavailable);
        assert_eq!(request.supported().await, OutputStatus::Unavailable);
    }
}
