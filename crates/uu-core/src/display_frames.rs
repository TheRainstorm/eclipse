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
/// The actual decoder owner returning an image, independent of diagnostics.
/// A buffered output still belongs to that owner; PTS is never used to infer it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct DecoderSource {
    pub ssrc: u32,
    pub instance: u64,
}
impl DecoderSource {
    pub(crate) fn new(ssrc: u32) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            ssrc,
            instance: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        }
    }
}
/// Frozen at actual decoder output and replaced atomically with its image.
/// A legacy RGBA send has no identity; conversion of an observed decoded image
/// preserves this same sidecar even when the resulting pixels become RGBA.
#[derive(Clone, Copy, Debug)]
pub struct DecodedObservation {
    pub media_epoch:u64,
    pub track:u32,
    pub pts_us:u64,
    pub source:Option<DecoderSource>,
    pub token:Option<crate::video_e2e::FrameToken>,
    pub receive:Option<crate::pipeline_trace::ImageIdentity>,
}
impl DisplayImage {
    fn track(&self) -> u32 { match self { Self::Rgba(f) => f.0, Self::Decoded(track, ..) => *track } }
    pub fn into_rgba(self) -> DisplayFrame {
        self.into_rgba_with_white(hdr::DEFAULT_SDR_WHITE_NITS)
    }
    pub fn into_rgba_with_white(self, source_white_nits: f32) -> DisplayFrame {
        match self {
            Self::Rgba(frame) => frame,
            Self::Decoded(track, frame, color) => {
                let pixels = uu_media::convert::frame_to_rgba_with_white(&frame, color, source_white_nits);
                (track, frame.width, frame.height, pixels)
            }
        }
    }
}
/// Color metadata and observation travel with the exact image selected from the mailbox.
pub struct FrameDelivery {
    pub image: DisplayImage,
    pub published: std::time::Instant,
    pub observation: Option<DecodedObservation>,
    pub source_white_nits: f32,
}
impl FrameDelivery {
    pub fn into_rgba(self) -> DisplayFrame {
        self.image.into_rgba_with_white(self.source_white_nits)
    }
}
#[derive(Default)]
struct Mailbox {
    generation: u64,
    images: BTreeMap<u32, FrameDelivery>,
    waits: BTreeMap<u32, std::collections::VecDeque<(std::time::Instant, f32)>>,
    // Per-track lifetime totals: published, replaced before delivery, delivered.
    counts: BTreeMap<u32, [u64; 3]>,
}
impl Mailbox {
    fn pop_first(&mut self) -> Option<FrameDelivery> {
        let track = *self.images.keys().next()?;
        self.pop_track(track)
    }
    fn pop_track(&mut self, track: u32) -> Option<FrameDelivery> {
        let delivery = self.images.remove(&track)?;
        let now = std::time::Instant::now();
        let waits = self.waits.entry(track).or_default();
        waits.push_back((now, now.duration_since(delivery.published).as_secs_f32() * 1000.0));
        while waits.len() > 120 { waits.pop_front(); }
        self.counts.entry(track).or_default()[2] += 1;
        Some(delivery)
    }
}
type Pending = Arc<Mutex<Mailbox>>;
type Waker = Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>;

#[derive(Clone)]
pub struct FrameSender {
    source_white_nits: f32,
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

/// A render callback may consume only its current track/generation. It never
/// owns or drains the receiver's wake token, so other tracks remain deliverable.
#[derive(Clone)]
pub struct FrameLatch {
    pending: Pending,
    generation: u64,
}
pub struct LatchedFrame {
    pub image: DisplayImage,
    pub observation: DecodedObservation,
    /// Captured while the chosen image is removed, only for opted-in tracing.
    pub taken_at: Option<std::time::Instant>,
}
impl FrameLatch {
    pub fn try_take_newer(
        &self,
        current: &RawFrame,
        color: YuvColor,
        owner: DecodedObservation,
        compatible: impl FnOnce(&RawFrame) -> bool,
    ) -> Option<LatchedFrame> {
        let mut mailbox = self.pending.try_lock().ok()?;
        if mailbox.generation != self.generation
            || owner.media_epoch != self.generation
            || owner.pts_us != current.pts.0
            || owner.source.is_none()
        {
            return None;
        }
        let FrameDelivery { image: DisplayImage::Decoded(track, next, next_color), observation: Some(next_owner), .. } =
            mailbox.images.get(&owner.track)?
        else {
            return None;
        };
        if *track != owner.track
            || next_owner.track != owner.track
            || next_owner.media_epoch != owner.media_epoch
            || next_owner.source != owner.source
            || next_owner.pts_us != next.pts.0
            || next.pts <= current.pts
            || [next.width, next.height, next.stride]
                != [current.width, current.height, current.stride]
            || next.format != current.format
            || next.gpu != current.gpu
            || *next_color != color
        {
            return None;
        }
        // Diagnostic sampling can be absent or contended. It must never
        // change eligibility, which uses the actual decoder owner above.
        if !compatible(next) {
            return None;
        }
        let delivery = mailbox.pop_track(owner.track)?;
        let image = delivery.image;
        let observation = delivery.observation?;
        let taken_at = observation.receive.map(|_| std::time::Instant::now());
        Some(LatchedFrame {
            image,
            observation,
            taken_at,
        })
    }
}

pub fn channel() -> (FrameSender, FrameReceiver) {
    let pending = Pending::default();
    let (tx, rx) = mpsc::sync_channel(1);
    let notify = Waker::default();
    let planar = Arc::new(std::sync::atomic::AtomicBool::new(false));
    (
        FrameSender {
            source_white_nits: hdr::DEFAULT_SDR_WHITE_NITS,
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
        Self { generation: mailbox.generation, source_white_nits: hdr::DEFAULT_SDR_WHITE_NITS, ..self.clone() }
    }
    pub fn generation(&self) -> u64 { self.generation }
    /// The authenticated answer belongs to this media source alone.
    pub(crate) fn with_source_white(mut self, white_nits: f32) -> Self {
        self.source_white_nits = white_nits;
        self
    }

    pub fn send(&self, frame: DisplayFrame) -> Result<(), mpsc::SendError<()>> {
        self.send_image(DisplayImage::Rgba(frame))
    }
    pub fn send_decoded(&self, track: u32, frame: RawFrame, color: YuvColor) -> Result<(), mpsc::SendError<()>> {
        self.send_observed_decoded(track,frame,color,None)
    }
    /// These consumers accept a leased image without synchronous RGBA work.
    /// Their mailbox already replaces stale decoded pictures during a burst.
    pub(crate) fn accepts_decoded(&self, frame: &RawFrame) -> bool {
        frame.gpu || self.planar.load(std::sync::atomic::Ordering::Acquire)
    }
    pub(crate) fn send_observed_decoded(&self, track:u32, frame:RawFrame, color:YuvColor, observation:Option<DecodedObservation>)->Result<(),mpsc::SendError<()>> {
        self.send_observed_decoded_timed(track, frame, color, observation).map(|_| ())
    }
    pub(crate) fn send_observed_decoded_timed(&self, track:u32, frame:RawFrame, color:YuvColor, observation:Option<DecodedObservation>)->Result<Option<std::time::Instant>,mpsc::SendError<()>> {
        let observation=observation.filter(|value| value.media_epoch==self.generation&&value.track==track&&value.pts_us==frame.pts.0);
        let decoded = self.accepts_decoded(&frame);
        let image = DisplayImage::Decoded(track, frame, color);
        if decoded {
            self.send_delivery_timed(image,observation)
        } else {
            self.send_delivery_timed(DisplayImage::Rgba(image.into_rgba_with_white(self.source_white_nits)),observation)
        }
    }
    fn send_image(&self, frame: DisplayImage) -> Result<(), mpsc::SendError<()>> {
        self.send_delivery(frame,None)
    }
    fn send_delivery(&self, frame:DisplayImage, observation:Option<DecodedObservation>)->Result<(),mpsc::SendError<()>> {
        self.send_delivery_timed(frame, observation).map(|_| ())
    }
    fn send_delivery_timed(&self, frame:DisplayImage, observation:Option<DecodedObservation>)->Result<Option<std::time::Instant>,mpsc::SendError<()>> {
        let (replaced, published) = {
            let mut mailbox = self.pending.lock().unwrap();
            if mailbox.generation != self.generation { return Ok(None); }
            let track = frame.track();
            let replaced = mailbox.images.insert(track, FrameDelivery { image: frame, published: std::time::Instant::now(), observation, source_white_nits: self.source_white_nits });
            let counts = mailbox.counts.entry(track).or_default();
            counts[0] += 1;
            counts[1] += u64::from(replaced.is_some());
            (replaced, observation.filter(|o| o.receive.is_some()).map(|_| std::time::Instant::now()))
        };
        // Invoke outside the lock: a UI integration may immediately drain.
        let notify = self.notify.lock().unwrap().clone();
        if let Some(notify) = notify { notify(); }
        let result = match self.wake.try_send(()) {
            Ok(()) | Err(mpsc::TrySendError::Full(())) => Ok(published),
            Err(mpsc::TrySendError::Disconnected(())) => Err(mpsc::SendError(())),
        };
        // A decoder sample's last release may enter a driver. Never hold the
        // mailbox lock or delay the GUI wake while releasing replaced images.
        drop(replaced);
        result
    }
}
impl FrameReceiver {
    pub fn generation(&self) -> u64 { self.pending.lock().unwrap().generation }
    pub fn latch(&self) -> FrameLatch {
        FrameLatch { pending: self.pending.clone(), generation: self.generation() }
    }
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
        self.try_recv_delivery().map(FrameDelivery::into_rgba)
    }
    pub fn try_recv_image(&mut self) -> Result<DisplayImage, mpsc::TryRecvError> {
        self.try_recv_timed_image().map(|(image, _)| image)
    }
    /// Publication time of this exact mailbox image, on the local monotonic
    /// clock. It is neither the host capture time nor GPU/display completion.
    pub fn try_recv_timed_image(&mut self) -> Result<(DisplayImage, std::time::Instant), mpsc::TryRecvError> {
        self.try_recv_delivery().map(|d| (d.image, d.published))
    }
    pub fn try_recv_observed_image(&mut self)->Result<(DisplayImage,Option<DecodedObservation>),mpsc::TryRecvError> {
        self.try_recv_delivery().map(|d| (d.image, d.observation))
    }
    pub fn try_recv_delivery(&mut self) -> Result<FrameDelivery, mpsc::TryRecvError> {
        loop {
            if let Some(frame) = self.pending.lock().unwrap().pop_first() {
                return Ok(frame);
            }
            self.wake.try_recv()?;
        }
    }
    pub fn blocking_recv(&mut self) -> Option<DisplayFrame> {
        loop {
            let delivery = self.pending.lock().unwrap().pop_first();
            if let Some(delivery) = delivery {
                return Some(delivery.into_rgba());
            }
            self.wake.recv().ok()?;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn observed(client:&crate::video_e2e::Client,epoch:u64,pts:u64)->DecodedObservation {
        DecodedObservation{media_epoch:epoch,track:0,pts_us:pts,source:None,token:client.frame_for_pts(epoch,0,pts),receive:None}
    }
    #[test]
    fn hdr_white_is_owned_by_the_delivered_image_across_cpu_planar_and_renewal() {
        let p010 = || RawFrame {
            width: 2, height: 2, stride: 4, pts: Timestamp(0), format: PixelFormat::P010,
            data: [545u16, 545, 545, 545, 512, 512].into_iter()
                .flat_map(|sample| (sample << 6).to_le_bytes()).collect::<Vec<_>>().into(),
            gpu: false, d3d11: None, dma_buf: None,
        };
        for planar in [false, true] {
            let (base, mut rx) = channel();
            if planar { rx.enable_planar(); }
            let tx = base.clone().with_source_white(240.0);
            tx.send_decoded(0, p010(), YuvColor::Limited2020Pq).unwrap();
            let held = rx.try_recv_delivery().unwrap();
            assert_eq!(held.source_white_nits, 240.0);
            assert_eq!(matches!(held.image, DisplayImage::Decoded(..)), planar);
            let renewed = tx.renew();
            tx.send_decoded(0, p010(), YuvColor::Limited2020Pq).unwrap();
            assert!(rx.try_recv().is_err());
            renewed.send_decoded(0, p010(), YuvColor::Limited2020Pq).unwrap();
            assert_eq!(rx.try_recv().unwrap().3, [223, 223, 223, 255].repeat(4));
            assert_eq!(held.into_rgba().3, [207, 207, 207, 255].repeat(4));
            let current = renewed.with_source_white(240.0);
            current.send_decoded(0, p010(), YuvColor::Limited2020Pq).unwrap();
            assert_eq!(rx.blocking_recv().unwrap().3, [207, 207, 207, 255].repeat(4));
        }
    }
    fn decoded(pts:u64,value:u8)->RawFrame {RawFrame::new_i420(2,2,vec![value;6],Timestamp(pts))}
    fn owned(pts: u64, source: DecoderSource) -> DecodedObservation {
        DecodedObservation {
            media_epoch: 0,
            track: 0,
            pts_us: pts,
            source: Some(source),
            token: None,
            receive: None,
        }
    }
    #[test]
    fn late_take_is_latest_same_decoder_without_trace_and_preserves_other_track_wakes() {
        let (tx, mut rx) = channel();
        rx.enable_planar();
        let source = DecoderSource::new(7);
        let first = decoded(1, 16);
        let owner = owned(1, source);
        tx.send_observed_decoded(0, first.clone(), YuvColor::Limited709, Some(owner))
            .unwrap();
        rx.try_recv_observed_image().unwrap();
        let latch = rx.latch();
        for pts in [2, 3] {
            tx.send_observed_decoded(
                0,
                decoded(pts, 32),
                YuvColor::Limited709,
                Some(owned(pts, source)),
            )
            .unwrap();
        }
        tx.send((1, 2, 2, vec![9])).unwrap();
        let delivery = latch
            .try_take_newer(&first, YuvColor::Limited709, owner, |_| true)
            .unwrap();
        assert!(matches!(delivery.image, DisplayImage::Decoded(0,f,_) if f.pts.0==3));
        assert_eq!(delivery.observation.source, Some(source));
        assert_eq!(rx.try_recv().unwrap().0, 1);
        assert!(
            rx.try_recv().is_err(),
            "old wake tokens are harmless and cannot produce an old image"
        );
        tx.send((1, 2, 2, vec![10])).unwrap();
        assert_eq!(rx.try_recv().unwrap().3, vec![10]);
        assert_eq!(rx.delivery_counts(0), [3, 1, 2]);
    }
    #[test]
    fn late_take_does_not_consume_incompatible_or_retired_images() {
        let (tx, mut rx) = channel();
        rx.enable_planar();
        let source = DecoderSource::new(7);
        let current = decoded(1, 16);
        let owner = owned(1, source);
        let latch = rx.latch();
        let other = DecoderSource::new(7);
        tx.send_observed_decoded(
            0,
            decoded(2, 32),
            YuvColor::Limited709,
            Some(owned(2, other)),
        )
        .unwrap();
        assert!(latch
            .try_take_newer(&current, YuvColor::Limited709, owner, |_| true)
            .is_none());
        assert!(rx.try_recv_observed_image().is_ok());
        tx.send_observed_decoded(0, decoded(2, 32), YuvColor::Full709, Some(owned(2, source)))
            .unwrap();
        assert!(latch
            .try_take_newer(&current, YuvColor::Limited709, owner, |_| true)
            .is_none());
        assert!(rx.try_recv_observed_image().is_ok());
        tx.send_observed_decoded(
            0,
            decoded(3, 32),
            YuvColor::Limited709,
            Some(owned(3, source)),
        )
        .unwrap();
        assert!(latch
            .try_take_newer(&current, YuvColor::Limited709, owner, |_| false)
            .is_none());
        let locked = rx.pending.lock().unwrap();
        assert!(latch
            .try_take_newer(&current, YuvColor::Limited709, owner, |_| true)
            .is_none());
        drop(locked);
        assert!(rx.try_recv_observed_image().is_ok());
        let fresh = tx.renew();
        fresh
            .send_observed_decoded(0, decoded(4, 48), YuvColor::Limited709, None)
            .unwrap();
        assert!(latch
            .try_take_newer(&current, YuvColor::Limited709, owner, |_| true)
            .is_none());
        assert!(
            rx.try_recv_observed_image().is_ok(),
            "an old render callback cannot steal a new media generation"
        );
    }
    #[test]
    fn missing_trace_sidecar_does_not_change_late_selection() {
        for traced in [false, true] {
            let (tx, mut rx) = channel();
            rx.enable_planar();
            let source = DecoderSource::new(7);
            let current = decoded(1, 16);
            let mut owner = owned(1, source);
            let mut next = owned(2, source);
            if traced {
                owner.receive = Some(crate::pipeline_trace::ImageIdentity {
                    row_id: 1,
                    media_epoch: 0,
                    track: 0,
                    ssrc: 7,
                    rtp_timestamp: 100,
                    decode_generation: 0,
                    pts_us: 1,
                });
            }
            // The second trace write may be contended. Runtime choice stays the
            // same; only a replacement's diagnostic provenance becomes unknown.
            next.receive = None;
            tx.send_observed_decoded(0, decoded(2, 32), YuvColor::Limited709, Some(next))
                .unwrap();
            assert!(rx
                .latch()
                .try_take_newer(&current, YuvColor::Limited709, owner, |_| true)
                .is_some());
        }
    }
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
        tx.pending.lock().unwrap().images.get_mut(&0).unwrap().published -= std::time::Duration::from_secs(1);
        let replaced_at = std::time::Instant::now();
        tx.send((0, 2, 1, vec![])).unwrap();
        assert!(tx.pending.lock().unwrap().images.get(&0).unwrap().published >= replaced_at);
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
