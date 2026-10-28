//! At most one pending decoded image per display. A slow UI must never queue
//! seconds of old RGBA frames or upload every stale texture on its next paint.
use std::collections::BTreeMap;
use std::sync::{mpsc, Arc, Mutex};

pub type DisplayFrame = (u32, u32, u32, Vec<u8>);
pub use uu_media::{RawFrame, PixelFormat, Timestamp};
pub use uu_media::color::YuvColor;
pub use uu_media::dma_buf;
pub use uu_media::d3d11_frame;
pub use uu_media::hdr;

pub enum DisplayImage {
    Rgba(DisplayFrame),
    Decoded(u32, RawFrame, YuvColor),
}
/// Frozen at actual decoder output and replaced atomically with its image.
/// A legacy RGBA send has no identity; conversion of an observed decoded image
/// preserves this same sidecar even when the resulting pixels become RGBA.
#[derive(Clone, Copy, Debug)]
pub struct DecodedObservation {
    pub media_epoch:u64,
    pub track:u32,
    pub pts_us:u64,
    pub token:Option<crate::video_e2e::FrameToken>,
    pub receive:Option<crate::pipeline_trace::ImageIdentity>,
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
#[derive(Default)]
struct Mailbox {
    generation: u64,
    images: BTreeMap<u32, (DisplayImage, std::time::Instant, Option<DecodedObservation>)>,
    waits: BTreeMap<u32, std::collections::VecDeque<(std::time::Instant, f32)>>,
    // Per-track lifetime totals: published, replaced before delivery, delivered.
    counts: BTreeMap<u32, [u64; 3]>,
}
impl Mailbox {
    fn pop_first(&mut self) -> Option<(DisplayImage, std::time::Instant, Option<DecodedObservation>)> {
        let (track, (frame, published, observation)) = self.images.pop_first()?;
        let now = std::time::Instant::now();
        let waits = self.waits.entry(track).or_default();
        waits.push_back((now, now.duration_since(published).as_secs_f32() * 1000.0));
        while waits.len() > 120 { waits.pop_front(); }
        self.counts.entry(track).or_default()[2] += 1;
        Some((frame, published, observation))
    }
}
type Pending = Arc<Mutex<Mailbox>>;
type Waker = Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>;

#[derive(Clone)]
pub struct FrameSender {
    generation: u64,
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
            generation: 0,
            pending: pending.clone(),
            wake: tx,
            notify: notify.clone(),
            planar: planar.clone(),
        },
        FrameReceiver { pending, wake: rx, notify, planar },
    )
}
impl FrameSender {
    /// Start a new media source. Retired decoder tasks cannot publish into it,
    /// including a late image with exactly the same size and video track.
    pub fn renew(&self) -> Self {
        let mut mailbox = self.pending.lock().unwrap();
        mailbox.generation = mailbox.generation.wrapping_add(1);
        for track in mailbox.images.keys().copied().collect::<Vec<_>>() {
            mailbox.counts.entry(track).or_default()[1] += 1;
        }
        mailbox.images.clear();
        mailbox.waits.clear();
        Self { generation: mailbox.generation, ..self.clone() }
    }
    pub fn generation(&self) -> u64 { self.generation }

    pub fn send(&self, frame: DisplayFrame) -> Result<(), mpsc::SendError<()>> {
        self.send_image(DisplayImage::Rgba(frame))
    }
    pub fn send_decoded(&self, track: u32, frame: RawFrame, color: YuvColor) -> Result<(), mpsc::SendError<()>> {
        self.send_observed_decoded(track,frame,color,None)
    }
    pub(crate) fn send_observed_decoded(&self, track:u32, frame:RawFrame, color:YuvColor, observation:Option<DecodedObservation>)->Result<(),mpsc::SendError<()>> {
        let observation=observation.filter(|value| value.media_epoch==self.generation&&value.track==track&&value.pts_us==frame.pts.0);
        let image = DisplayImage::Decoded(track, frame, color);
        if matches!(&image,DisplayImage::Decoded(_,frame,_) if frame.gpu)
            || self.planar.load(std::sync::atomic::Ordering::Acquire) {
            self.send_delivery(image,observation)
        } else {
            self.send_delivery(DisplayImage::Rgba(image.into_rgba()),observation)
        }
    }
    fn send_image(&self, frame: DisplayImage) -> Result<(), mpsc::SendError<()>> {
        self.send_delivery(frame,None)
    }
    fn send_delivery(&self, frame:DisplayImage, observation:Option<DecodedObservation>)->Result<(),mpsc::SendError<()>> {
        {
            let mut mailbox = self.pending.lock().unwrap();
            if mailbox.generation != self.generation { return Ok(()); }
            let track = frame.track();
            let replaced = mailbox.images.insert(track, (frame, std::time::Instant::now(), observation)).is_some();
            let counts = mailbox.counts.entry(track).or_default();
            counts[0] += 1;
            counts[1] += u64::from(replaced);
        }
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
    pub fn generation(&self) -> u64 { self.pending.lock().unwrap().generation }
    /// Published, overwritten before delivery, and delivered images for a track.
    /// Overwrites discard stale decoded images, never compressed reference frames.
    pub fn delivery_counts(&self, track: u32) -> [u64; 3] {
        self.pending.lock().unwrap().counts.get(&track).copied().unwrap_or_default()
    }
    /// Mean decoded-image mailbox residence of delivered images in the last 2s.
    /// Replaced images are excluded; no recent image is unknown, not zero.
    pub fn delivery_wait_ms(&self, track: u32) -> Option<f32> {
        let mailbox = self.pending.lock().unwrap();
        let samples = mailbox.waits.get(&track)?;
        let now = std::time::Instant::now();
        let (sum, count) = samples.iter()
            .filter(|(at, _)| now.duration_since(*at) < std::time::Duration::from_secs(2))
            .fold((0.0, 0usize), |(sum, n), (_, ms)| (sum + ms, n + 1));
        (count > 0).then(|| sum / count as f32)
    }
    pub fn set_waker(&self, notify: impl Fn() + Send + Sync + 'static) {
        *self.notify.lock().unwrap() = Some(Arc::new(notify));
    }
    /// Opt in before starting a GUI receiver. CLI consumers retain RGBA.
    pub fn enable_planar(&self) { self.planar.store(true, std::sync::atomic::Ordering::Release); }
    pub fn try_recv(&mut self) -> Result<DisplayFrame, mpsc::TryRecvError> {
        self.try_recv_image().map(DisplayImage::into_rgba)
    }
    pub fn try_recv_image(&mut self) -> Result<DisplayImage, mpsc::TryRecvError> {
        self.try_recv_timed_image().map(|(image, _)| image)
    }
    /// Publication time of this exact mailbox image, on the local monotonic
    /// clock. It is neither the host capture time nor GPU/display completion.
    pub fn try_recv_timed_image(&mut self) -> Result<(DisplayImage, std::time::Instant), mpsc::TryRecvError> {
        self.try_recv_delivery().map(|(image,published,_)|(image,published))
    }
    pub fn try_recv_observed_image(&mut self)->Result<(DisplayImage,Option<DecodedObservation>),mpsc::TryRecvError> {
        self.try_recv_delivery().map(|(image,_,observation)|(image,observation))
    }
    fn try_recv_delivery(&mut self)->Result<(DisplayImage,std::time::Instant,Option<DecodedObservation>),mpsc::TryRecvError> {
        loop {
            if let Some(frame) = self.pending.lock().unwrap().pop_first() {
                return Ok(frame);
            }
            self.wake.try_recv()?;
        }
    }
    pub fn blocking_recv(&mut self) -> Option<DisplayFrame> {
        loop {
            if let Some((frame, _, _)) = self.pending.lock().unwrap().pop_first() {
                return Some(frame.into_rgba());
            }
            self.wake.recv().ok()?;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn observed(client:&crate::video_e2e::Client,epoch:u64,pts:u64)->DecodedObservation {
        DecodedObservation{media_epoch:epoch,track:0,pts_us:pts,token:client.frame_for_pts(epoch,0,pts),receive:None}
    }
    fn decoded(pts:u64,value:u8)->RawFrame {RawFrame::new_i420(2,2,vec![value;6],Timestamp(pts))}
    #[test]
    fn mailbox_freezes_old_image_identity_while_ssrc_reuses_its_pts() {
        let (tx,mut rx)=channel();rx.enable_planar();
        let client=crate::video_e2e::Client::default();client.begin(0,true);
        client.decode_input(0,0,9,100,0);
        tx.send_observed_decoded(0,decoded(0,16),YuvColor::Limited709,Some(observed(&client,0,0))).unwrap();
        // The old image is still in the mailbox when a new source reuses PTS.
        client.decode_input(0,0,10,200,0);
        assert_eq!(client.frame_for_pts(0,0,0).unwrap().ssrc,10);
        let (image,frozen)=rx.try_recv_observed_image().unwrap();
        assert!(matches!(image,DisplayImage::Decoded(_,frame,_) if frame.pts.0==0&&frame.data[0]==16));
        assert_eq!(frozen.unwrap().token.unwrap().ssrc,9);
    }
    #[test]
    fn image_and_sidecar_are_replaced_and_retired_as_one_delivery() {
        let (tx,mut rx)=channel();rx.enable_planar();
        let client=crate::video_e2e::Client::default();client.begin(0,true);
        for (rtp,pts,value) in [(100,0,16),(200,1,32)] {
            client.decode_input(0,0,9,rtp,pts);
            tx.send_observed_decoded(0,decoded(pts,value),YuvColor::Limited709,Some(observed(&client,0,pts))).unwrap();
        }
        let (image,frozen)=rx.try_recv_observed_image().unwrap();
        assert!(matches!(image,DisplayImage::Decoded(_,frame,_) if frame.pts.0==1&&frame.data[0]==32));
        assert_eq!(frozen.unwrap().token.unwrap().rtp_timestamp,200);
        tx.send_observed_decoded(0,decoded(1,32),YuvColor::Limited709,Some(observed(&client,0,1))).unwrap();
        let current=tx.renew();
        tx.send_observed_decoded(0,decoded(1,32),YuvColor::Limited709,Some(observed(&client,0,1))).unwrap();
        assert!(rx.try_recv_observed_image().is_err());
        current.send_observed_decoded(0,decoded(1,48),YuvColor::Limited709,Some(observed(&client,0,1))).unwrap();
        let (_,metadata)=rx.try_recv_observed_image().unwrap();
        assert!(metadata.is_none(),"retired epoch metadata must not be relabelled as the new source");
    }
    #[test]
    fn decoded_rgba_keeps_frozen_identity_but_legacy_rgba_and_mismatched_pts_do_not() {
        let (tx,mut rx)=channel();
        let client=crate::video_e2e::Client::default();client.begin(0,true);client.decode_input(0,0,9,100,0);
        tx.send_observed_decoded(0,decoded(0,16),YuvColor::Limited709,Some(observed(&client,0,0))).unwrap();
        let (image,frozen)=rx.try_recv_observed_image().unwrap();
        assert!(matches!(image,DisplayImage::Rgba(_)));
        assert_eq!(frozen.unwrap().token.unwrap().rtp_timestamp,100);
        tx.send((0,2,2,vec![0;16])).unwrap();
        assert!(rx.try_recv_observed_image().unwrap().1.is_none());
        rx.enable_planar();
        tx.send_observed_decoded(0,decoded(2,16),YuvColor::Limited709,Some(observed(&client,0,0))).unwrap();
        assert!(rx.try_recv_observed_image().unwrap().1.is_none());
    }
    #[test]
    fn renewed_source_drops_pending_and_late_old_images_even_at_the_same_size() {
        let (old, mut rx) = channel();
        old.send((0,2,2,vec![1])).unwrap();
        let fresh = old.renew();
        assert_eq!(rx.generation(), fresh.generation());
        old.send((0,2,2,vec![2])).unwrap();
        assert!(rx.try_recv().is_err());
        fresh.send((0,2,2,vec![3])).unwrap();
        assert_eq!(rx.try_recv().unwrap().3, vec![3]);
        assert_eq!(rx.delivery_counts(0), [2,1,1]);
    }
    #[test]
    fn delivery_wait_tracks_delivered_image_and_expires() {
        let (tx, mut rx) = channel();
        assert_eq!(rx.delivery_wait_ms(0), None);
        tx.send((0, 1, 1, vec![])).unwrap();
        tx.pending.lock().unwrap().images.get_mut(&0).unwrap().1 -= std::time::Duration::from_secs(1);
        let replaced_at = std::time::Instant::now();
        tx.send((0, 2, 1, vec![])).unwrap();
        assert!(tx.pending.lock().unwrap().images.get(&0).unwrap().1 >= replaced_at);
        // Timing must accompany the delivered replacement, not an earlier wake.
        let (image, published) = rx.try_recv_timed_image().unwrap();
        assert!(published >= replaced_at);
        assert!(matches!(image, DisplayImage::Rgba((0, 2, 1, _))));
        let mut pending = rx.pending.lock().unwrap();
        let sample = pending.waits.get_mut(&0).unwrap().back_mut().unwrap();
        sample.1 = 12.5;
        drop(pending);
        assert_eq!(rx.delivery_wait_ms(0), Some(12.5));
        rx.pending.lock().unwrap().waits.get_mut(&0).unwrap().back_mut().unwrap().0 -= std::time::Duration::from_secs(3);
        assert_eq!(rx.delivery_wait_ms(0), None);
    }
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
        assert_eq!(rx.delivery_counts(0), [100, 99, 1]);
        assert_eq!(rx.delivery_counts(1), [1, 0, 1]);
        assert_eq!(rx.delivery_counts(2), [0, 0, 0]);
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
