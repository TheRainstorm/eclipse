//! At most one pending decoded image per display. A slow UI must never queue
//! seconds of old RGBA frames or upload every stale texture on its next paint.
use std::collections::BTreeMap;
use std::sync::{mpsc, Arc, Mutex};

pub type DisplayFrame = (u32, u32, u32, Vec<u8>);
type Pending = Arc<Mutex<BTreeMap<u32, DisplayFrame>>>;
type Waker = Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>;

#[derive(Clone)]
pub struct FrameSender {
    pending: Pending,
    wake: mpsc::SyncSender<()>,
    notify: Waker,
}
pub struct FrameReceiver {
    pending: Pending,
    wake: mpsc::Receiver<()>,
    notify: Waker,
}

pub fn channel() -> (FrameSender, FrameReceiver) {
    let pending = Pending::default();
    let (tx, rx) = mpsc::sync_channel(1);
    let notify = Waker::default();
    (
        FrameSender {
            pending: pending.clone(),
            wake: tx,
            notify: notify.clone(),
        },
        FrameReceiver { pending, wake: rx, notify },
    )
}
impl FrameSender {
    pub fn send(&self, frame: DisplayFrame) -> Result<(), mpsc::SendError<()>> {
        self.pending.lock().unwrap().insert(frame.0, frame);
        // Invoke outside the lock: a UI integration may immediately drain.
        let notify = self.notify.lock().unwrap().clone();
        if let Some(notify) = notify { notify(); }
        match self.wake.try_send(()) {
            Ok(()) | Err(mpsc::TrySendError::Full(())) => Ok(()),
            Err(mpsc::TrySendError::Disconnected(())) => Err(mpsc::SendError(())),
        }
    }
}
impl FrameReceiver {
    pub fn set_waker(&self, notify: impl Fn() + Send + Sync + 'static) {
        *self.notify.lock().unwrap() = Some(Arc::new(notify));
    }
    pub fn try_recv(&mut self) -> Result<DisplayFrame, mpsc::TryRecvError> {
        loop {
            if let Some((_, frame)) = self.pending.lock().unwrap().pop_first() {
                return Ok(frame);
            }
            self.wake.try_recv()?;
        }
    }
    pub fn blocking_recv(&mut self) -> Option<DisplayFrame> {
        loop {
            if let Some((_, frame)) = self.pending.lock().unwrap().pop_first() {
                return Some(frame);
            }
            self.wake.recv().ok()?;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slow_ui_gets_only_latest_image_per_display_and_closes_cleanly() {
        let (tx, mut rx) = channel();
        for i in 0..100 {
            tx.send((0, i, 1, vec![i as u8])).unwrap();
        }
        tx.send((1, 40, 1, vec![])).unwrap();
        drop(tx);
        assert_eq!(rx.blocking_recv().unwrap().1, 99);
        assert_eq!(rx.blocking_recv().unwrap().0, 1);
        assert!(rx.blocking_recv().is_none());
    }
    #[test]
    fn new_images_wake_the_ui_even_when_the_mailbox_is_already_full() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (tx, mut rx) = channel();
        let wakes = Arc::new(AtomicUsize::new(0));
        let count = wakes.clone();
        rx.set_waker(move || { count.fetch_add(1, Ordering::Relaxed); });
        for i in 0..3 { tx.send((0, i, 1, vec![])).unwrap(); }
        assert_eq!(wakes.load(Ordering::Relaxed), 3);
        assert_eq!(rx.try_recv().unwrap().1, 2);
    }
}
