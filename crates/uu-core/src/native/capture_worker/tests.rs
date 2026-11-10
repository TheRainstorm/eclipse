use super::*;
use tokio::time::timeout;
use uu_capture::CaptureSource;
use uu_proto::media::VideoCodec;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_requests_initial_idr_without_adding_a_periodic_refresh() {
    struct Source {
        inner: Box<dyn uu_capture::Capture>,
        count: u32,
    }
    impl uu_capture::Capture for Source {
        fn resolution(&self) -> (u32, u32) {
            self.inner.resolution()
        }
        fn next_frame(
            &mut self,
        ) -> std::result::Result<uu_media::RawFrame, uu_capture::CaptureError> {
            if self.count == 1 {
                std::thread::sleep(Duration::from_millis(2100));
            }
            self.count += 1;
            self.inner.next_frame()
        }
    }
    struct Encoder(bool);
    impl uu_media::VideoEncoder for Encoder {
        fn backend_name(&self) -> &str {
            "owned reference fixture"
        }
        fn codec(&self) -> VideoCodec {
            VideoCodec::H264
        }
        fn request_keyframe(&mut self) {
            self.0 = true;
        }
        fn set_bitrate(&mut self, _: u32) -> std::result::Result<(), uu_media::CodecError> {
            self.0 = true;
            Ok(())
        }
        fn encode(
            &mut self,
            frame: &uu_media::RawFrame,
        ) -> std::result::Result<Vec<uu_media::EncodedFrame>, uu_media::CodecError> {
            Ok(vec![uu_media::EncodedFrame {
                data: vec![0u8].into(),
                pts: frame.pts,
                is_keyframe: std::mem::take(&mut self.0),
            }])
        }
    }
    let settings = uu_media::encoder::Settings {
        hdr: false,
        source_hdr: None,
        tuning: Default::default(),
        codec: VideoCodec::H264,
        rate_control: Default::default(),
        width: 64,
        height: 64,
        fps: 60,
        bitrate_kbps: 1000,
        preference: EncoderPreference::Software,
    };
    let source = Box::new(Source {
        inner: uu_capture::open(&CaptureSource::Synthetic {
            width: 64,
            height: 64,
            fps: 60,
        })
        .unwrap(),
        count: 0,
    });
    let mut producer = Worker::start(
        source,
        settings,
        Arc::default(),
        Box::new(Encoder(false)),
        Arc::default(),
    );
    assert!(
        timeout(Duration::from_secs(2), producer.frames.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .is_keyframe
    );
    assert!(
        !timeout(Duration::from_secs(4), producer.frames.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .is_keyframe,
        "capture added a periodic refresh even though references were intact"
    );
    producer.stop().await.unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suspended_capture_emits_no_old_pixels_resumes_with_idr_and_cancels() {
    struct Source {
        inner: Box<dyn uu_capture::Capture>,
        paused: Arc<AtomicBool>,
        suspended: Arc<AtomicU64>,
    }
    impl uu_capture::Capture for Source {
        fn resolution(&self) -> (u32, u32) {
            self.inner.resolution()
        }
        fn next_frame(
            &mut self,
        ) -> std::result::Result<uu_media::frame::RawFrame, uu_capture::CaptureError> {
            if self.paused.load(Ordering::Acquire) {
                self.suspended.fetch_add(1, Ordering::Release);
                Err(uu_capture::CaptureError::Suspended(
                    "owned desktop transition".into(),
                ))
            } else {
                self.inner.next_frame()
            }
        }
    }
    let settings = uu_media::encoder::Settings {
        hdr: false,
        source_hdr: None,
        tuning: Default::default(),
        codec: VideoCodec::H264,
        rate_control: Default::default(),
        width: 64,
        height: 64,
        fps: 60,
        bitrate_kbps: 1000,
        preference: EncoderPreference::Software,
    };
    let paused = Arc::new(AtomicBool::new(false));
    let suspended = Arc::new(AtomicU64::new(0));
    let capture = Box::new(Source {
        inner: uu_capture::open(&CaptureSource::Synthetic {
            width: 64,
            height: 64,
            fps: 60,
        })
        .unwrap(),
        paused: paused.clone(),
        suspended: suspended.clone(),
    });
    let mut producer = Worker::start(
        capture,
        settings,
        Arc::default(),
        uu_media::encoder::create(settings).unwrap(),
        Arc::default(),
    );
    let _ = timeout(Duration::from_secs(2), producer.frames.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    paused.store(true, Ordering::Release);
    timeout(Duration::from_secs(2), async {
        while suspended.load(Ordering::Acquire) == 0 {
            while producer.frames.try_recv().is_ok() {}
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    while producer.frames.try_recv().is_ok() {}
    assert!(
        timeout(Duration::from_millis(80), producer.frames.recv())
            .await
            .is_err(),
        "no old desktop frame is repeated"
    );
    paused.store(false, Ordering::Release);
    let frame = timeout(Duration::from_secs(2), producer.frames.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(frame.is_keyframe);
    assert!(uu_media::software_decoder()
        .unwrap()
        .decode(&frame)
        .unwrap()
        .is_some());
    paused.store(true, Ordering::Release);
    timeout(Duration::from_millis(500), producer.stop())
        .await
        .unwrap()
        .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovery_immediately_after_an_idr_is_encoded_in_the_next_capture_batch() {
    let settings = uu_media::encoder::Settings {
        hdr: false,
        source_hdr: None,
        tuning: Default::default(),
        codec: VideoCodec::H264,
        rate_control: Default::default(),
        width: 64,
        height: 64,
        fps: 60,
        bitrate_kbps: 1000,
        preference: EncoderPreference::Software,
    };
    let capture = uu_capture::open(&CaptureSource::Synthetic {
        width: 64,
        height: 64,
        fps: 60,
    })
    .unwrap();
    let encoder = uu_media::encoder::create(settings).unwrap();
    let refresh = Arc::new(AtomicBool::new(false));
    let mut producer = Worker::start(capture, settings, refresh.clone(), encoder, Arc::default());
    let first = timeout(Duration::from_secs(2), producer.frames.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(first.is_keyframe);
    refresh.store(true, Ordering::Release);
    // Allow the two already-buffered frames; a new request must reach the
    // next captured frame rather than wait for a 250 ms debounce window.
    let mut recovered = None;
    for _ in 0..3 {
        let frame = timeout(Duration::from_secs(2), producer.frames.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if frame.is_keyframe {
            recovered = Some(frame);
            break;
        }
    }
    producer.stop().await.unwrap();
    let recovered = recovered.expect("recovery feedback was delayed after a recent IDR");
    let mut decoder = uu_media::software_decoder().unwrap();
    assert!(
        decoder.decode(&recovered).unwrap().is_some(),
        "recovery is independently decodable"
    );
}

mod overlap_integration_tests {
    use super::*;
    use std::sync::mpsc as channel;
    enum Event {
        Frame(u8),
        Pause,
        Fence(channel::SyncSender<()>),
    }
    struct Capture {
        rx: channel::Receiver<Event>,
        info: Option<uu_capture::frame_info::FrameInfo>,
        generation: u64,
    }
    impl uu_capture::Capture for Capture {
        fn resolution(&self) -> (u32, u32) {
            (64, 64)
        }
        fn enable_overlap(&mut self) -> std::result::Result<(), uu_capture::CaptureError> {
            Ok(())
        }
        fn frame_info(&self) -> Option<uu_capture::frame_info::FrameInfo> {
            self.info
        }
        fn next_frame(
            &mut self,
        ) -> std::result::Result<uu_media::RawFrame, uu_capture::CaptureError> {
            let info = self
                .info
                .as_mut()
                .ok_or_else(|| uu_capture::CaptureError::Suspended("waiting owned image".into()))?;
            info.duplicate = true;
            let id = info.frame_id as u8;
            Ok(uu_media::RawFrame::new_i420(
                64,
                64,
                vec![id; 64 * 64 * 3 / 2],
                uu_media::Timestamp(u64::from(id) * 10000),
            ))
        }
        fn wait_updated_frame(
            &mut self,
            wait: Duration,
        ) -> std::result::Result<Option<uu_media::RawFrame>, uu_capture::CaptureError> {
            loop {
                match self.rx.recv_timeout(wait) {
                    Ok(Event::Frame(id)) => {
                        self.info = Some(uu_capture::frame_info::FrameInfo {
                            generation: self.generation,
                            frame_id: u64::from(id),
                            duplicate: false,
                            accumulated_frames: 1,
                            present: None,
                            present_age_us: None,
                        });
                        return Ok(Some(uu_media::RawFrame::new_i420(
                            64,
                            64,
                            vec![id; 64 * 64 * 3 / 2],
                            uu_media::Timestamp(u64::from(id) * 10000),
                        )));
                    }
                    Ok(Event::Pause) => {
                        self.generation += 1;
                        self.info = None;
                        return Err(uu_capture::CaptureError::Suspended("owned pause".into()));
                    }
                    Ok(Event::Fence(done)) => {
                        done.send(()).unwrap();
                    }
                    Err(channel::RecvTimeoutError::Timeout) => return Ok(None),
                    Err(_) => {
                        return Err(uu_capture::CaptureError::Backend(
                            "owned source closed".into(),
                        ))
                    }
                }
            }
        }
    }
    struct Encoder {
        entered: channel::SyncSender<u8>,
        release: channel::Receiver<()>,
        first: bool,
        keyframe: bool,
        fail: bool,
    }
    impl uu_media::VideoEncoder for Encoder {
        fn codec(&self) -> VideoCodec {
            VideoCodec::H264
        }
        fn backend_name(&self) -> &str {
            "owned overlap encoder"
        }
        fn set_bitrate(&mut self, _: u32) -> std::result::Result<(), uu_media::CodecError> {
            Ok(())
        }
        fn request_keyframe(&mut self) {
            self.keyframe = true;
        }
        fn encode(
            &mut self,
            frame: &uu_media::RawFrame,
        ) -> std::result::Result<Vec<uu_media::EncodedFrame>, uu_media::CodecError> {
            let id = frame.data[0];
            if std::mem::take(&mut self.first) {
                self.entered.send(id).unwrap();
                self.release.recv_timeout(Duration::from_secs(2)).unwrap();
            }
            assert!(
                frame.data.iter().all(|v| *v == id),
                "inflight pixels changed"
            );
            if self.fail {
                return Err(uu_media::CodecError::Backend(
                    "injected encode failure".into(),
                ));
            }
            Ok(vec![uu_media::EncodedFrame {
                data: vec![id].into(),
                pts: frame.pts,
                is_keyframe: std::mem::take(&mut self.keyframe),
            }])
        }
    }
    fn setup(
        fail: bool,
    ) -> (
        Worker,
        channel::Sender<Event>,
        channel::Receiver<u8>,
        channel::SyncSender<()>,
        Arc<AtomicBool>,
    ) {
        let (tx, rx) = channel::channel();
        let (entered, observed) = channel::sync_channel(1);
        let (release, resume) = channel::sync_channel(1);
        let refresh = Arc::new(AtomicBool::new(false));
        let settings = uu_media::encoder::Settings {
            width: 64,
            height: 64,
            fps: 60,
            bitrate_kbps: 1000,
            codec: VideoCodec::H264,
            preference: EncoderPreference::Software,
            rate_control: Default::default(),
            tuning: Default::default(),
            hdr: false,
            source_hdr: None,
        };
        let worker = Worker::start_mode(
            Box::new(Capture {
                rx,
                info: None,
                generation: 1,
            }),
            settings,
            refresh.clone(),
            Box::new(Encoder {
                entered,
                release: resume,
                first: true,
                keyframe: false,
                fail,
            }),
            Arc::default(),
            Some(true),
        );
        (worker, tx, observed, release, refresh)
    }
    fn fence(tx: &channel::Sender<Event>) {
        let (done, rx) = channel::sync_channel(1);
        tx.send(Event::Fence(done)).unwrap();
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slow_encode_gets_latest_pixels_preserves_idr_and_ignores_cancelled_quality() {
        let (mut worker, tx, entered, release, refresh) = setup(false);
        tx.send(Event::Frame(1)).unwrap();
        assert_eq!(entered.recv_timeout(Duration::from_secs(1)).unwrap(), 1);
        for id in 2..=10 {
            tx.send(Event::Frame(id)).unwrap();
        }
        fence(&tx);
        let cancelled = worker
            .quality
            .request(crate::native_quality::Update {
                fps: 24,
                bitrate_kbps: Some(500),
                rate_control: None,
            })
            .unwrap();
        drop(cancelled);
        refresh.store(true, Ordering::Release);
        release.send(()).unwrap();
        let first = timeout(Duration::from_secs(1), worker.frames.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(first.data[0], 1);
        let latest = timeout(Duration::from_secs(1), worker.frames.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(latest.data[0], 10);
        assert!(latest.is_keyframe, "IDR arriving during encode was lost");
        assert_eq!(worker.quality_state.borrow().fps, 60);
        worker.stop().await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pause_while_encoding_retires_old_pixels_and_resumes_with_new_idr() {
        let (mut worker, tx, entered, release, _) = setup(false);
        tx.send(Event::Frame(1)).unwrap();
        entered.recv_timeout(Duration::from_secs(1)).unwrap();
        tx.send(Event::Pause).unwrap();
        fence(&tx);
        release.send(()).unwrap();
        assert!(timeout(Duration::from_millis(80), worker.frames.recv())
            .await
            .is_err());
        tx.send(Event::Frame(2)).unwrap();
        let resumed = timeout(Duration::from_secs(1), worker.frames.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(resumed.data[0], 2);
        assert!(resumed.is_keyframe);
        assert!(resumed.capture_lease.as_ref().unwrap().valid());
        timeout(Duration::from_millis(500), worker.stop())
            .await
            .unwrap()
            .unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn applied_quality_ack_retires_old_raw_and_new_encoder_starts_a_matching_chain() {
        let (mut worker, tx, entered, release, _) = setup(false);
        tx.send(Event::Frame(1)).unwrap();
        entered.recv_timeout(Duration::from_secs(1)).unwrap();
        tx.send(Event::Frame(2)).unwrap();
        fence(&tx);
        let applied = worker
            .quality
            .request(crate::native_quality::Update {
                fps: 24,
                bitrate_kbps: Some(500),
                rate_control: None,
            })
            .unwrap();
        release.send(()).unwrap();
        let first = timeout(Duration::from_secs(1), worker.frames.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(first.data[0], 1);
        assert_eq!(
            timeout(Duration::from_secs(2), applied)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            500
        );
        assert!(
            !first.capture_lease.as_ref().unwrap().valid(),
            "old raw generation survived quality acknowledgement"
        );
        let next = timeout(Duration::from_secs(2), worker.frames.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(next.is_keyframe);
        assert!(uu_media::software_decoder()
            .unwrap()
            .decode(&next)
            .unwrap()
            .is_some());
        assert_eq!(worker.quality_state.borrow().fps, 24);
        assert_eq!(
            worker.trace.report().unwrap().counts.unmatched_outputs,
            0,
            "new encoder was retired again after capture"
        );
        worker.stop().await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn encoder_failure_and_closed_output_stop_the_owned_source() {
        let (mut worker, tx, entered, release, _) = setup(true);
        tx.send(Event::Frame(1)).unwrap();
        entered.recv_timeout(Duration::from_secs(1)).unwrap();
        release.send(()).unwrap();
        assert!(timeout(Duration::from_secs(1), worker.frames.recv())
            .await
            .unwrap()
            .unwrap()
            .is_err());
        worker.stop().await.unwrap();
        assert!(tx.send(Event::Frame(2)).is_err());
        let (mut idle, _tx, _entered, _release, _) = setup(false);
        timeout(Duration::from_millis(600), idle.stop())
            .await
            .unwrap()
            .unwrap();
    }
}

mod owner_pacing_idr_tests {
    use super::*;
    struct Encoder {
        requests: u32,
    }
    impl uu_media::VideoEncoder for Encoder {
        fn codec(&self) -> VideoCodec {
            VideoCodec::Av1
        }
        fn encode(
            &mut self,
            _frame: &uu_media::RawFrame,
        ) -> Result<Vec<uu_media::EncodedFrame>, uu_media::CodecError> {
            Ok(vec![])
        }
        fn request_keyframe(&mut self) {
            self.requests += 1;
        }
        fn set_bitrate(&mut self, _kbps: u32) -> Result<(), uu_media::CodecError> {
            Ok(())
        }
    }
    #[test]
    fn repeat_opportunity_consumes_idr_once_without_requiring_new_source() {
        let refresh = AtomicBool::new(true);
        let mut encoder = Encoder { requests: 0 };
        apply_refresh_request(&refresh, &mut encoder);
        assert_eq!(encoder.requests, 1);
        apply_refresh_request(&refresh, &mut encoder);
        assert_eq!(encoder.requests, 1);
        refresh.store(true, Ordering::Release);
        apply_refresh_request(&refresh, &mut encoder);
        assert_eq!(encoder.requests, 2);
    }
}
