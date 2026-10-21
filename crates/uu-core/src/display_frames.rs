//! At most one pending decoded image per display. A slow UI must never queue
//! seconds of old RGBA frames or upload every stale texture on its next paint.
use std::collections::BTreeMap;
use std::sync::{mpsc, Arc, Mutex};

pub type DisplayFrame = (u32, u32, u32, Vec<u8>);
pub use uu_media::{RawFrame, PixelFormat, Timestamp};
pub use uu_media::color::YuvColor;
pub use uu_media::dma_buf;
pub use uu_media::d3d11_frame;

pub enum DisplayImage {
    Rgba(DisplayFrame),
    Decoded(u32, RawFrame, YuvColor),
}
impl DisplayImage {
    fn track(&self) -> u32 { match self { Self::Rgba(f) => f.0, Self::Decoded(track, ..) => *track } }
    pub fn into_rgba(self) -> DisplayFrame {
        match self {
            Self::Rgba(frame) => frame,
            Self::Decoded(track, frame, color) => (track, frame.width, frame.height, uu_media::convert::frame_to_rgba_with_color(&frame, color)),
        }
    }
}
type Pending = Arc<Mutex<BTreeMap<u32, DisplayImage>>>;
type Waker = Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>;

#[derive(Clone)]
pub struct FrameSender {
    pending: Pending,
    wake: mpsc::SyncSender<()>,
    notify: Waker,
    planar: Arc<std::sync::atomic::AtomicBool>,
}
pub struct FrameReceiver {
    pending: Pending,
    wake: mpsc::Receiver<()>,
    notify: Waker,
    planar: Arc<std::sync::atomic::AtomicBool>,
}

pub fn channel() -> (FrameSender, FrameReceiver) {
    let pending = Pending::default();
    let (tx, rx) = mpsc::sync_channel(1);
    let notify = Waker::default();
    let planar = Arc::new(std::sync::atomic::AtomicBool::new(false));
    (
        FrameSender {
            pending: pending.clone(),
            wake: tx,
            notify: notify.clone(),
            planar: planar.clone(),
        },
        FrameReceiver { pending, wake: rx, notify, planar },
    )
}
impl FrameSender {
    pub fn send(&self, frame: DisplayFrame) -> Result<(), mpsc::SendError<()>> {
        self.send_image(DisplayImage::Rgba(frame))
    }
    pub fn send_decoded(&self, track: u32, frame: RawFrame, color: YuvColor) -> Result<(), mpsc::SendError<()>> {
        let image = DisplayImage::Decoded(track, frame, color);
        if matches!(&image,DisplayImage::Decoded(_,frame,_) if frame.gpu)
            || self.planar.load(std::sync::atomic::Ordering::Acquire) {
            self.send_image(image)
        } else {
            self.send(image.into_rgba())
        }
    }
    fn send_image(&self, frame: DisplayImage) -> Result<(), mpsc::SendError<()>> {
        self.pending.lock().unwrap().insert(frame.track(), frame);
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
    /// Opt in before starting a GUI receiver. CLI consumers retain RGBA.
    pub fn enable_planar(&self) { self.planar.store(true, std::sync::atomic::Ordering::Release); }
    pub fn try_recv(&mut self) -> Result<DisplayFrame, mpsc::TryRecvError> {
        self.try_recv_image().map(DisplayImage::into_rgba)
    }
    pub fn try_recv_image(&mut self) -> Result<DisplayImage, mpsc::TryRecvError> {
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
                return Some(frame.into_rgba());
            }
            self.wake.recv().ok()?;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn planar_mailbox_preserves_pixels_color_and_latest_only_without_rgba() {
        let (tx,mut rx)=channel();rx.enable_planar();
        let frame=RawFrame::new_i420(2,2,vec![16,16,235,235,128,128],Timestamp(42));
        let address=frame.data.as_ptr();
        tx.send_decoded(0,frame.clone(),YuvColor::Limited709).unwrap();
        tx.send_decoded(0,frame,YuvColor::Limited601).unwrap();
        match rx.try_recv_image().unwrap() {
            DisplayImage::Decoded(track,frame,color)=>{assert_eq!(track,0);assert_eq!(frame.data.as_ptr(),address);assert_eq!(frame.pts,Timestamp(42));assert_eq!(color,YuvColor::Limited601);}
            _=>panic!("planar opt-in converted on the decode thread"),
        }
        assert!(rx.try_recv_image().is_err());
    }
    #[test]
    fn legacy_consumers_still_receive_rgba_after_planar_delivery() {
        for planar in [false,true] {
            let (tx,mut rx)=channel();if planar {rx.enable_planar();}
            tx.send_decoded(2,RawFrame::new_i420(2,2,vec![16,16,235,235,128,128],Timestamp(0)),YuvColor::Limited709).unwrap();
            let (track,w,h,rgba)=rx.try_recv().unwrap();
            assert_eq!((track,w,h),(2,2,2));assert_eq!(&rgba[..4],&[0,0,0,255]);assert_eq!(&rgba[8..12],&[254,254,254,255]);
        }
    }
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
