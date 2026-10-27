//! The common bounded capture/encode actor, usable by other session protocols.
use super::*;

#[derive(Clone, Copy, Debug)]
pub struct CaptureOptions {
    pub codec: VideoCodec,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub bounds: Option<[u32; 2]>,
}
struct CancelOpen(Arc<AtomicBool>);
impl Drop for CancelOpen {
    fn drop(&mut self) { self.0.store(true, Ordering::Release); }
}
pub struct CapturedSample(TracedEncoded);
impl CapturedSample {
    pub fn current(&self) -> bool {
        self.0.capture_lease.as_ref().is_none_or(|lease| lease.valid())
    }
}
impl std::ops::Deref for CapturedSample {
    type Target = uu_media::EncodedFrame;
    fn deref(&self) -> &Self::Target { &self.0.frame }
}
/// Caller must complete its protocol's authorization before opening a source.
/// All outputs retain the same GPU/capture leases as native sessions.
pub struct EncodedCapture {
    worker: CaptureWorker,
    refresh: Arc<AtomicBool>,
    rect: Option<uu_capture::DesktopRect>,
    injector: Option<Box<dyn uu_input::InputInjector>>,
    _cancel: CancelOpen,
}
impl EncodedCapture {
    pub async fn open_sdr(config: SharingConfig, options: CaptureOptions) -> Result<Self> {
        config.validate()?;
        ensure!((5..=crate::native_quality::MAX_FPS).contains(&options.fps), "invalid capture frame rate");
        ensure!((64..=crate::native_quality::MAX_BITRATE_KBPS).contains(&options.bitrate_kbps), "invalid capture bitrate");
        ensure!(options.bounds.is_none_or(|[w,h]|(2..=8192).contains(&w)&&(2..=8192).contains(&h)&&w%2==0&&h%2==0), "invalid output bounds");
        if let Some(policy) = &config.video_policy {
            ensure!(policy.codecs.contains(&options.codec), "requested codec is disabled by host policy");
        }
        let cancelled = CancelOpen(Arc::default());
        let capture_options = uu_capture::OpenOptions {
            hdr: false, allow_input: config.allow_input, fps: options.fps, cancelled: cancelled.0.clone(),
        };
        let prepared = tokio::task::spawn_blocking(move || -> Result<_> {
            let mut capture = uu_capture::open_with_options(&config.source, capture_options)?;
            capture.set_frame_rate(options.fps)?;
            let mut capture = uu_capture::scale::bounded(capture, options.bounds);
            let separate = crate::capture_overlap::separate_from_env()?;
            crate::capture_overlap::prepare_device(capture.as_mut(),crate::capture_overlap::requested_from_env()?,separate)?;
            let (width,height) = capture.resolution();
            validate_video(width,height,options.fps)?;
            let settings = uu_media::encoder::Settings {
                hdr: false, source_hdr: capture.hdr_metadata(), tuning: config.encoder_tuning,
                codec: options.codec, rate_control: uu_media::encoder::RateControl::Cbr,
                width,height,fps:options.fps,bitrate_kbps:options.bitrate_kbps,preference:config.encoder,
            };
            let encoder=uu_media::encoder::create_with_device(settings,capture.encoder_input_device())?;
            ensure!(!separate || encoder.backend_name().starts_with("amf"), "separate encode device requires native AMF");
            Ok((capture,encoder,settings))
        }).await.context("capture setup worker failed")??;
        let (capture,encoder,settings)=prepared;
        let rect=capture.desktop_rect();
        let injector=capture.input_injector();
        let refresh=Arc::new(AtomicBool::new(true));
        let worker=CaptureWorker::start(capture,settings,refresh.clone(),encoder,Arc::default());
        Ok(Self { worker,refresh,rect,injector,_cancel:cancelled })
    }
    pub fn settings(&self)->uu_media::encoder::Settings { *self.worker.quality_state.borrow() }
    pub fn backend(&self)->String { self.worker.encoder.borrow().clone() }
    pub fn keyframe_flag(&self)->Arc<AtomicBool> { self.refresh.clone() }
    pub fn request_keyframe(&self) { self.refresh.store(true,Ordering::Release); }
    pub fn update(&self, settings:crate::native_quality::Update)
        ->Result<tokio::sync::oneshot::Receiver<std::result::Result<u32,String>>> {
        self.worker.quality.request(settings)
    }
    pub async fn next(&mut self)->Option<Result<CapturedSample>> {
        self.worker.frames.recv().await.map(|v|v.map(CapturedSample))
    }
    pub(crate) fn take_input(&mut self)->(Option<uu_capture::DesktopRect>,Option<Box<dyn uu_input::InputInjector>>) {
        (self.rect,self.injector.take())
    }
    pub fn report(&self)->HostReport {
        HostReport {
            encoder:self.backend(),captured:self.worker.captured.load(Ordering::Relaxed),
            producer_ms:self.worker.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            capture_us:self.worker.timings.capture_us.load(Ordering::Relaxed),
            encode_us:self.worker.timings.encode_us.load(Ordering::Relaxed),
            backpressure_us:self.worker.timings.backpressure_us.load(Ordering::Relaxed),..Default::default()
        }
    }
    pub async fn stop(&mut self)->Result<()> { self.worker.stop().await }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config()->SharingConfig {
        SharingConfig { encoder_tuning:Default::default(),session:None,video_policy:None,codec:VideoCodec::H264,
            rate_control:uu_media::encoder::RateControl::Cbr,source:CaptureSource::Synthetic {width:320,height:180,fps:60},
            fps:60,bitrate_kbps:2000,encoder:EncoderPreference::Software,session_limit:None,allow_input:false,audio:None,clipboard:None }
    }
    #[tokio::test(flavor="multi_thread",worker_threads=2)]
    async fn shared_actor_encodes_updates_and_joins_while_the_bounded_output_is_full() {
        let mut capture=EncodedCapture::open_sdr(config(),CaptureOptions {codec:VideoCodec::H264,fps:60,bitrate_kbps:2000,bounds:Some([160,90])}).await.unwrap();
        let first=timeout(Duration::from_secs(3),capture.next()).await.unwrap().unwrap().unwrap();
        assert!(first.current());assert!(first.is_keyframe);assert_eq!(capture.settings().width,160);
        let reply=capture.update(crate::native_quality::Update {fps:30,bitrate_kbps:Some(1500),rate_control:None}).unwrap();
        let update=async {tokio::pin!(reply);loop {tokio::select! {
            r=&mut reply=>break r.unwrap().unwrap(),
            sample=capture.next()=>{assert!(sample.unwrap().is_ok());}
        }}};
        assert_eq!(timeout(Duration::from_secs(3),update).await.unwrap(),1500);
        tokio::time::sleep(Duration::from_millis(100)).await;
        timeout(Duration::from_secs(3),capture.stop()).await.unwrap().unwrap();
        assert!(capture.report().captured>0);
        assert!(capture.worker.task.is_none());
    }
    #[tokio::test]
    async fn disabled_codecs_and_invalid_geometry_fail_before_capture() {
        let mut config=config();config.video_policy=Some(crate::native_quality::Policy {codecs:vec![VideoCodec::Av1],preferred_codec:Some(VideoCodec::Av1)});
        assert!(EncodedCapture::open_sdr(config,CaptureOptions {codec:VideoCodec::H264,fps:60,bitrate_kbps:2000,bounds:None}).await.is_err());
        assert!(EncodedCapture::open_sdr(super::tests::config(),CaptureOptions {codec:VideoCodec::H264,fps:60,bitrate_kbps:2000,bounds:Some([1,180])}).await.is_err());
    }
}
