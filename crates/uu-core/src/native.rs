//! Native LAN viewing prototype: private TLS pairing, WebRTC DTLS-SRTP, one source.
//! No vendor signaling or production credential is used. Input needs an
//! explicit host permission and a separate authenticated control request.
use crate::{
    media::{FrameSender, MediaStats},
    native_media::NativePeer,
    native_pairing::{HostCredentials, Invitation},
    native_signaling::{SessionSignaling, TlsSignaling},
};
use anyhow::{bail, ensure, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, watch},
    time::timeout,
};
use webrtc::{
    media::Sample,
    peer_connection::{
        peer_connection_state::RTCPeerConnectionState,
        sdp::session_description::RTCSessionDescription,
    },
};

const MAX_SIGNAL: usize = 256 * 1024;
pub use uu_capture::CaptureSource;
pub use uu_media::audio_capture::Source as AudioSource;
pub use uu_media::encoder::Preference as EncoderPreference;

pub fn hardware_encoding_compiled() -> bool {
    uu_media::encoder::hardware_compiled()
}

pub fn uses_desktop_portal() -> bool { uu_capture::uses_desktop_portal() }

#[derive(Clone, Debug)]
pub struct CaptureMonitor {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

impl CaptureMonitor {
    pub fn software_encodable(&self)->bool {
        uu_media::H264Encoder::validate_dimensions(self.width,self.height).is_ok()
    }
    pub fn encodable(&self, preference:EncoderPreference, fps:u32)->bool {
        let settings=uu_media::encoder::Settings {width:self.width,height:self.height,fps,
            bitrate_kbps:64,preference};
        settings.validate().is_ok() && (preference!=EncoderPreference::Software || self.software_encodable())
    }
}

/// Enumerate the current authorized desktop without reading its pixels.
pub fn monitors() -> Result<Vec<CaptureMonitor>> {
    // Wayland output selection and dimensions are only known after consent.
    // Enumerating Xwayland would describe an unrelated desktop.
    if uses_desktop_portal() { return Ok(Vec::new()); }
    #[cfg(windows)]
    let sources = uu_capture::windows::monitors()?;
    #[cfg(target_os = "linux")]
    let sources = uu_capture::x11::monitors(None)?;
    #[cfg(any(windows, target_os = "linux"))]
    return Ok(sources.into_iter().map(|m| CaptureMonitor {
        name: m.name, x: m.x, y: m.y, width: m.width, height: m.height, primary: m.primary,
    }).collect());
    #[cfg(not(any(windows, target_os = "linux")))]
    bail!("native desktop capture is unavailable on this platform")
}

/// Local host UI state. Capability contents never enter progress messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostStatus {
    Starting,
    Waiting { address: SocketAddr, expires_at: u64 },
    Preparing,
    Streaming { width: u32, height: u32, fps: u32, audio: String, encoder: String },
}
fn progress(status: &Option<watch::Sender<HostStatus>>, value: HostStatus) {
    if let Some(status) = status { status.send_replace(value); }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    version: u32,
    sdp: RTCSessionDescription,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    version: u32,
    sdp: RTCSessionDescription,
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
    view_only: bool,
}

fn validate_video(width: u32, height: u32, fps: u32) -> Result<()> {
    ensure!(
        (2..=8192).contains(&width)
            && (2..=8192).contains(&height)
            && width % 2 == 0
            && height % 2 == 0
            && (1..=60).contains(&fps),
        "invalid native software video dimensions or frame rate"
    );
    let macroblocks = width.div_ceil(16) as u64 * height.div_ceil(16) as u64;
    ensure!(
        macroblocks <= 36_864 && macroblocks * fps as u64 <= 2_073_600,
        "native source exceeds negotiated H264 level 5.2; choose a smaller source or frame rate"
    );
    Ok(())
}

pub(crate) async fn read_message<T: DeserializeOwned>(stream: &mut (impl AsyncRead + Unpin)) -> Result<T> {
    read_message_wait(stream,Duration::from_secs(15)).await
}
pub(crate) async fn read_message_wait<T: DeserializeOwned>(stream: &mut (impl AsyncRead + Unpin),wait:Duration) -> Result<T> {
    timeout(wait, async {
        let len = stream.read_u32().await? as usize;
        ensure!(
            (1..=MAX_SIGNAL).contains(&len),
            "native signaling message exceeds size limit"
        );
        let mut data = vec![0; len];
        stream.read_exact(&mut data).await?;
        serde_json::from_slice(&data)
            .map_err(|_| anyhow::anyhow!("invalid native signaling message"))
    })
    .await
    .context("native signaling timed out")?
}
pub(crate) async fn write_message<T: Serialize>(
    stream: &mut (impl AsyncWrite + Unpin),
    message: &T,
) -> Result<()> {
    let data = serde_json::to_vec(message)?;
    ensure!(
        data.len() <= MAX_SIGNAL,
        "native signaling message exceeds size limit"
    );
    timeout(Duration::from_secs(15), async {
        stream.write_u32(data.len() as u32).await?;
        stream.write_all(&data).await?;
        stream.flush().await
    })
    .await
    .context("native signaling write timed out")??;
    Ok(())
}

pub struct HostConfig {
    /// Concrete local interface, loopback by default. No firewall rules are changed.
    pub bind: SocketAddr,
    pub source: uu_capture::CaptureSource,
    pub invitation_file: PathBuf,
    pub invitation_lifetime: Duration,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// Per-session choice; default callers should select Software.
    pub encoder: EncoderPreference,
    pub session_limit: Option<Duration>,
    pub allow_input: bool,
    pub audio: Option<uu_media::audio_capture::Source>,
    /// Caller-owned, opt-in OS backend; the core only transports bounded text.
    pub clipboard: Option<Arc<crate::clipboard::Clipboard>>,
}

/// Capture and permissions shared by LAN and internet sessions.
#[derive(Clone)]
pub struct SharingConfig {
    pub source: CaptureSource,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub encoder: EncoderPreference,
    pub session_limit: Option<Duration>,
    pub allow_input: bool,
    pub audio: Option<AudioSource>,
    pub clipboard: Option<Arc<crate::clipboard::Clipboard>>,
}
impl From<HostConfig> for SharingConfig {
    fn from(c: HostConfig) -> Self {
        Self { source:c.source, fps:c.fps, bitrate_kbps:c.bitrate_kbps, encoder:c.encoder,
            session_limit:c.session_limit, allow_input:c.allow_input, audio:c.audio, clipboard:c.clipboard }
    }
}
impl SharingConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!((1..=60).contains(&self.fps) && (64..=100_000).contains(&self.bitrate_kbps),
            "native H.264 host needs 1..=60 fps and 64..=100000 kbps");
        let monitor = matches!(self.source,CaptureSource::Primary|CaptureSource::Monitor(_));
        ensure!(!self.allow_input || monitor, "desktop input requires an explicitly shared monitor");
        ensure!(self.audio != Some(AudioSource::System) || monitor, "system audio requires an explicitly shared monitor");
        Ok(())
    }
}

/// The provider must return authenticated, session-bound SDP. No capture is
/// opened until the authenticated offer and direct-only candidates are checked.
pub async fn serve_direct(signaling: &mut dyn SessionSignaling, ice: &crate::direct_ice::InternetIce,
    config: SharingConfig, stop: watch::Receiver<bool>, status: Option<watch::Sender<HostStatus>>) -> Result<HostReport> {
    config.validate()?;
    let mut peer = NativePeer::new_direct(ice, None, true).await?;
    let result = serve_peer(&mut peer, signaling, config, stop, status).await;
    peer.close().await;
    result
}

pub async fn view_direct(signaling: &mut dyn SessionSignaling, ice: &crate::direct_ice::InternetIce,
    frames: FrameSender, stats: Arc<MediaStats>, mut stop: watch::Receiver<bool>,
    input: Option<crate::native_input::NativeInputReceiver>) -> Result<()> {
    stats.capabilities.set(0);
    stats.native_input_available.store(false, Ordering::Release);
    let mut peer = NativePeer::new_direct(ice, Some(stats.clone()), false).await?;
    let result = tokio::select! {
        result=receive_session(&mut peer,signaling,frames,stats.clone(),input)=>result,
        _=cancelled(&mut stop)=>Ok(()),
    };
    peer.close().await;
    stats.control_open.store(0, Ordering::Release);
    stats.audio.stop();
    crate::native_clipboard::stop(&stats.clipboard);
    stats.capabilities.enable(crate::capabilities::Capability::Audio,false);
    result
}
#[derive(Debug, Default)]
pub struct HostReport {
    pub encoder: String,
    pub captured: u64,
    pub sent: u64,
    pub bytes: u64,
    /// Local producer timings, independent of network/decoder latency.
    pub producer_ms: u64,
    pub capture_us: u64,
    pub encode_us: u64,
    pub backpressure_us: u64,
    pub input_events: u64,
    pub input_errors: u64,
    pub audio_packets:u64,
    pub audio_bytes:u64,
    pub audio_errors:u64,
    pub audio_queue_drops:u64,
}

async fn cancelled(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow_and_update() {
            return;
        }
        if stop.changed().await.is_err() {
            return;
        }
    }
}

/// Serve one invited controller, then close. A fresh invocation makes a fresh invitation.
pub async fn serve(config: HostConfig, stop: watch::Receiver<bool>) -> Result<HostReport> {
    serve_with_status(config, stop, None).await
}

pub async fn serve_with_status(config: HostConfig, mut stop: watch::Receiver<bool>, status: Option<watch::Sender<HostStatus>>) -> Result<HostReport> {
    ensure!(
        !config.bind.ip().is_unspecified() && !config.bind.ip().is_multicast(),
        "choose a concrete native host interface"
    );
    ensure!(
        (1..=60).contains(&config.fps) && (64..=100_000).contains(&config.bitrate_kbps),
        "native H.264 host needs 1..=60 fps and 64..=100000 kbps"
    );
    ensure!(!config.allow_input || matches!(config.source,uu_capture::CaptureSource::Primary|uu_capture::CaptureSource::Monitor(_)),
        "desktop input can only be enabled for a real whole-monitor source");
    ensure!(config.audio!=Some(uu_media::audio_capture::Source::System)||matches!(config.source,uu_capture::CaptureSource::Primary|uu_capture::CaptureSource::Monitor(_)),
        "system audio requires an explicitly shared real monitor source");
    let listener = TcpListener::bind(config.bind)
        .await
        .context("cannot bind native signaling listener")?;
    let address = listener.local_addr()?;
    let credentials = HostCredentials::new(address, config.invitation_lifetime)?;
    let _published = credentials.invitation().publish_new(&config.invitation_file)?;
    progress(&status, HostStatus::Waiting { address, expires_at: credentials.invitation().expires_at() });
    tracing::info!(%address,input_allowed=config.allow_input,"native host waiting for an invited controller");
    let socket = tokio::select! {
        _=cancelled(&mut stop)=>return Ok(HostReport::default()),
        result=timeout(config.invitation_lifetime,async{
            loop {
                let (socket,_)=listener.accept().await?;
                match credentials.accept(socket).await {Ok(socket)=>return Ok::<_,anyhow::Error>(socket),Err(_)=>tracing::debug!("native pairing attempt rejected")}
            }
        })=>result.context("native invitation expired before a controller paired")??,
    };
    drop(listener);
    progress(&status, HostStatus::Preparing);
    host_session(socket, config, stop, status).await
}

#[derive(Default)]
struct ProducerTimings {
    capture_us: AtomicU64,
    encode_us: AtomicU64,
    backpressure_us: AtomicU64,
}

fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u64::MAX as u128) as u64
}

struct CaptureWorker {
    started: Instant,
    timings: Arc<ProducerTimings>,
    stopped: Arc<AtomicBool>,
    captured: Arc<AtomicU64>,
    frames: mpsc::Receiver<Result<uu_media::EncodedFrame>>,
    encoder: watch::Receiver<String>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl CaptureWorker {
    fn start(
        capture: Box<dyn uu_capture::Capture>,
        fps: u32,
        refresh: Arc<AtomicBool>,
        encoder: Box<dyn uu_media::VideoEncoder>,
    ) -> Self {
        let stopped = Arc::new(AtomicBool::new(false));
        let captured = Arc::new(AtomicU64::new(0));
        let (tx, frames) = mpsc::channel(2);
        let worker_stop = stopped.clone();
        let counter = captured.clone();
        let timings = Arc::new(ProducerTimings::default());
        let worker_timings = timings.clone();
        let started = Instant::now();
        let (encoder_status,encoder_state)=watch::channel(encoder.backend_name().to_owned());
        let task = tokio::task::spawn_blocking(move || {
            let result = encode_source(capture, fps, encoder, refresh, worker_stop, counter, worker_timings, encoder_status, &tx);
            if let Err(error) = result {
                let _ = tx.blocking_send(Err(error));
            }
        });
        Self {
            started,
            timings,
            stopped,
            captured,
            frames,
            encoder: encoder_state,
            task: Some(task),
        }
    }
    async fn stop(&mut self) -> Result<()> {
        self.stopped.store(true, Ordering::Release);
        self.frames.close();
        if let Some(task) = self.task.take() {
            task.await.context("native capture worker failed")?;
        }
        Ok(())
    }
}
impl Drop for CaptureWorker {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
}

fn encode_source(
    mut capture: Box<dyn uu_capture::Capture>,
    fps: u32,
    mut encoder: Box<dyn uu_media::VideoEncoder>,
    refresh: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    captured: Arc<AtomicU64>,
    timings: Arc<ProducerTimings>,
    encoder_status: watch::Sender<String>,
    output: &mpsc::Sender<Result<uu_media::EncodedFrame>>,
) -> Result<()> {
    let mut backend=encoder.backend_name().to_owned();
    let interval = Duration::from_secs_f64(1.0 / fps as f64);
    let mut next = Instant::now();
    let mut keyframe = Instant::now() - Duration::from_secs(3);
    while !stopped.load(Ordering::Acquire) {
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
        if stopped.load(Ordering::Acquire) {
            break;
        }
        let started = Instant::now();
        let frame = capture.next_frame()?;
        timings.capture_us.fetch_add(elapsed_us(started), Ordering::Relaxed);
        captured.fetch_add(1, Ordering::Relaxed);
        let requested = refresh.load(Ordering::Acquire);
        if keyframe.elapsed() >= Duration::from_secs(2)
            || (requested && keyframe.elapsed() >= Duration::from_millis(250))
        {
            refresh.store(false, Ordering::Release);
            encoder.request_keyframe();
            keyframe = Instant::now();
        }
        let started = Instant::now();
        let encoded_frames = encoder.encode(&frame)?;
        if encoder.backend_name()!=backend {
            backend=encoder.backend_name().to_owned();encoder_status.send_replace(backend.clone());
        }
        timings.encode_us.fetch_add(elapsed_us(started), Ordering::Relaxed);
        let started = Instant::now();
        for encoded in encoded_frames {
            if output.blocking_send(Ok(encoded)).is_err() {
                return Ok(());
            }
        }
        timings.backpressure_us.fetch_add(elapsed_us(started), Ordering::Relaxed);
        next = (next + interval).max(Instant::now());
    }
    Ok(())
}

async fn host_session(
    stream: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    config: HostConfig,
    stop: watch::Receiver<bool>,
    status: Option<watch::Sender<HostStatus>>,
) -> Result<HostReport> {
    let local_ip = stream.get_ref().0.local_addr()?.ip();
    let peer_ip = stream.get_ref().0.peer_addr()?.ip();
    let mut peer = NativePeer::new(local_ip, None, true).await?;
    let mut signaling = TlsSignaling { stream, peer: peer_ip };
    let result = serve_peer(&mut peer, &mut signaling, config.into(), stop, status).await;
    peer.close().await;
    let _ = signaling.stream.shutdown().await;
    result
}

async fn serve_peer(peer: &mut NativePeer, signaling: &mut dyn SessionSignaling, config: SharingConfig,
    mut stop: watch::Receiver<bool>, status: Option<watch::Sender<HostStatus>>) -> Result<HostReport> {
    let message = tokio::select! {result=signaling.receive()=>result?,_=cancelled(&mut stop)=>return Ok(HostReport::default())};
    let offer: Offer = serde_json::from_value(message).map_err(|_| anyhow::anyhow!("invalid native offer"))?;
    ensure!(offer.version == 1, "unsupported native protocol version");
    send_video(peer, signaling, config, stop, offer, status).await
}

async fn send_video(
    peer: &mut NativePeer,
    stream: &mut dyn SessionSignaling,
    config: SharingConfig,
    mut stop: watch::Receiver<bool>,
    offer: Offer,
    status: Option<watch::Sender<HostStatus>>,
) -> Result<HostReport> {
    let refresh = Arc::new(AtomicBool::new(false));
    let track = peer.sender(refresh.clone()).await?;
    let audio_mode=if config.audio.is_some(){crate::native_media::offered_audio(&offer.sdp)?}else{None};
    let mut audio_sender=if let Some(red)=audio_mode {Some(peer.audio_sender(red).await?)}else{None};
    match stream.lan_peer() {
        Some(ip) => peer.remote(offer.sdp, ip).await?,
        None => peer.remote_direct(offer.sdp).await?,
    }
    // Authentication and a valid media offer precede capture access.
    let (fps,kbps,preference)=(config.fps,config.bitrate_kbps,config.encoder);
    struct CancelCaptureOpen(Arc<AtomicBool>);
    impl Drop for CancelCaptureOpen { fn drop(&mut self) { self.0.store(true, Ordering::Release); } }
    let capture_cancel = CancelCaptureOpen(Arc::new(AtomicBool::new(false)));
    let capture_options = uu_capture::OpenOptions { allow_input: config.allow_input, fps, cancelled:capture_cancel.0.clone() };
    let (capture,encoder) = tokio::select! {
        result=tokio::task::spawn_blocking(move || -> Result<_> {
            let capture=uu_capture::open_with_options(&config.source,capture_options)?;
            let (width,height)=capture.resolution();validate_video(width,height,fps)?;
            let encoder=uu_media::encoder::create(uu_media::encoder::Settings {
                width,height,fps,bitrate_kbps:kbps,preference,
            }).context("无法为当前显示源初始化所选编码器；不会更改系统显示设置")?;
            Ok((capture,encoder))
        })=>result??,
        _=cancelled(&mut stop)=>return Ok(HostReport::default()),
        _=stream.disconnected()=>return Ok(HostReport::default()),
    };
    let (width, height) = capture.resolution();
    validate_video(width, height, config.fps)?;
    let controls=tokio::select! {
        controls=crate::native_control::HostControl::with_injector(config.allow_input,capture.desktop_rect(),config.clipboard.clone(),capture.input_injector())=>controls,
        _=cancelled(&mut stop)=>return Ok(HostReport::default()),
    };
    controls.attach(&peer.pc);
    let answer = Answer {
        version: 1,
        sdp: peer.description(false).await?,
        width,
        height,
        fps: config.fps,
        bitrate_kbps: config.bitrate_kbps,
        view_only: true,
    };
    stream.send(serde_json::to_value(&answer)?).await?;
    let connected=tokio::select! {result=peer.connected()=>Some(result),_=cancelled(&mut stop)=>None};
    match connected {
        Some(Ok(()))=>{},
        Some(Err(error))=>{controls.stop().await;return Err(error);},
        None=>{controls.stop().await;return Ok(HostReport::default());},
    }
    if stream.lan_peer().is_none() {
        if let Err(error)=peer.direct_path().await {controls.stop().await;return Err(error);}
    }
    let mut producer = CaptureWorker::start(capture, config.fps, refresh, encoder);
    let mut encoder_label=producer.encoder.borrow().clone();
    let mut encoder_state=Some(producer.encoder.clone());
    let mut audio_label=if config.audio.is_none(){String::new()}else if audio_sender.is_none(){"控制端未接收声音".into()}else{"声音准备中".into()};
    let (mut audio_worker,mut audio_frames,mut audio_state)=if let Some(source)=config.audio.filter(|_|audio_sender.is_some()) {
        match crate::native_audio::Worker::start(source) {
            Ok((worker,frames,state))=>(Some(worker),Some(frames),Some(state)),
            Err(error)=>{audio_label=format!("声音不可用：{error}");(None,None,None)},
        }
    }else{(None,None,None)};
    progress(&status, HostStatus::Streaming { width, height, fps: config.fps, audio:audio_label.clone(),encoder:encoder_label.clone() });
    let mut report = HostReport::default();
    let mut previous = None;
    let deadline = async {
        if let Some(limit) = config.session_limit {
            tokio::time::sleep(limit).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    tokio::pin!(deadline);
    let active = async {
        loop {
            tokio::select! {
                _ = cancelled(&mut stop) => break,
                _ = &mut deadline => break,
                _ = stream.disconnected() => break,
                changed=async {match &mut encoder_state {Some(state)=>state.changed().await,None=>std::future::pending().await}}=>{
                    if changed.is_ok() {
                        encoder_label=encoder_state.as_mut().unwrap().borrow_and_update().clone();
                        progress(&status,HostStatus::Streaming {width,height,fps:config.fps,audio:audio_label.clone(),encoder:encoder_label.clone()});
                    }else{encoder_state=None;}
                },
                changed=async {match &mut audio_state {Some(state)=>state.changed().await,None=>std::future::pending().await}}=>{
                    if changed.is_ok() {
                        let state=audio_state.as_mut().unwrap().borrow_and_update().clone();
                        audio_label=state.label();
                        progress(&status,HostStatus::Streaming {width,height,fps:config.fps,audio:audio_label.clone(),encoder:encoder_label.clone()});
                    }else{audio_state=None;}
                },
                frame=async {match &mut audio_frames {Some(frames)=>frames.recv().await,None=>std::future::pending().await}}=>{
                    if let Some(frame)=frame {
                        if frame.created.elapsed()>Duration::from_millis(100){report.audio_queue_drops+=1;continue;}
                        if let Some(sender)=&mut audio_sender {match sender.send(frame).await {
                            Ok(bytes)=>{report.audio_packets+=1;report.audio_bytes+=bytes as u64;},
                            Err(error)=>{
                                report.audio_errors+=1;audio_frames=None;audio_state=None;
                                if let Some(worker)=&audio_worker{worker.request_stop();}
                                audio_label=format!("声音传输停止：{error}");
                                progress(&status,HostStatus::Streaming {width,height,fps:config.fps,audio:audio_label.clone(),encoder:encoder_label.clone()});
                            },
                        }}
                    }else{audio_frames=None;}
                },
                state = peer.states.changed() => {
                    if state.is_err() || matches!(*peer.states.borrow(), RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed) {
                        bail!("native controller disconnected");
                    }
                },
                encoded = producer.frames.recv() => {
                    let encoded = encoded.context("native capture stopped unexpectedly")??;
                    let duration = previous.map(|pts:u64| Duration::from_micros(encoded.pts.0.saturating_sub(pts)))
                        .unwrap_or_else(|| Duration::from_secs_f64(1.0 / config.fps as f64));
                    previous = Some(encoded.pts.0);
                    let bytes = encoded.data.len() as u64;
                    track.write_sample(&Sample { data: encoded.data, duration, ..Default::default() }).await?;
                    report.sent += 1; report.bytes += bytes;
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    }.await;
    controls.stop().await;
    if let Some(worker)=&mut audio_worker {
        worker.stop().await;report.audio_errors+=worker.errors.load(Ordering::Relaxed);report.audio_queue_drops+=worker.dropped.load(Ordering::Relaxed);
    }
    (report.input_events,report.input_errors)=controls.counters();
    peer.close().await;
    let stopped = producer.stop().await;
    report.captured = producer.captured.load(Ordering::Relaxed);
    report.producer_ms = producer.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    report.capture_us = producer.timings.capture_us.load(Ordering::Relaxed);
    report.encode_us = producer.timings.encode_us.load(Ordering::Relaxed);
    report.backpressure_us = producer.timings.backpressure_us.load(Ordering::Relaxed);
    report.encoder=producer.encoder.borrow().clone();
    active?;
    stopped?;
    Ok(report)
}

/// Receive an authenticated native source into the same bounded render mailbox.
pub async fn view(
    invitation: &Invitation,
    frames: FrameSender,
    stats: Arc<MediaStats>,
    stop: watch::Receiver<bool>,
) -> Result<()> {
    view_session(invitation,frames,stats,stop,None).await
}

pub async fn view_with_input(invitation:&Invitation,frames:FrameSender,stats:Arc<MediaStats>,stop:watch::Receiver<bool>,input:crate::native_input::NativeInputReceiver)->Result<()> {
    view_session(invitation,frames,stats,stop,Some(input)).await
}

async fn view_session(invitation:&Invitation,frames:FrameSender,stats:Arc<MediaStats>,mut stop:watch::Receiver<bool>,input:Option<crate::native_input::NativeInputReceiver>)->Result<()> {
    stats.connection.set_service("IP 直连");
    stats.capabilities.set(0);
    stats.native_input_available.store(false,Ordering::Release);
    let stream = tokio::select! {
        result=crate::native_pairing::connect(invitation)=>result?,
        _=cancelled(&mut stop)=>return Ok(()),
    };
    let local_ip = stream.get_ref().0.local_addr()?.ip();
    let advertised_ip = invitation.observed_client_ip().unwrap_or(local_ip);
    let mut peer = NativePeer::new_with_advertised(local_ip, advertised_ip, Some(stats.clone()), false).await?;
    let peer_ip = stream.get_ref().0.peer_addr()?.ip();
    let mut signaling = TlsSignaling { stream, peer: peer_ip };
    let result = tokio::select! {
        result=receive_session(&mut peer,&mut signaling,frames,stats.clone(),input)=>result,
        _=cancelled(&mut stop)=>Ok(()),
    };
    peer.close().await;
    stats.control_open.store(0, Ordering::Release);
    stats.audio.stop();
    crate::native_clipboard::stop(&stats.clipboard);
    stats.capabilities.enable(crate::capabilities::Capability::Audio,false);
    let _ = signaling.stream.shutdown().await;
    result
}

async fn receive_session(
    peer: &mut NativePeer,
    stream: &mut dyn SessionSignaling,
    frames: FrameSender,
    stats: Arc<MediaStats>,
    input:Option<crate::native_input::NativeInputReceiver>,
) -> Result<()> {
    peer.receiver(frames, stats.clone()).await?;
    let channel = peer
        .pc
        .create_data_channel("eclipse-control-v1", None)
        .await?;
    let clipboard=peer.pc.create_data_channel(crate::native_clipboard::LABEL,None).await?;
    crate::native_clipboard::viewer(clipboard,stats.clone());
    let mut controls=crate::native_control::ViewerControl::new(channel.clone(),stats.clone(),input);
    stream.send(serde_json::to_value(Offer {
        version: 1,
        sdp: peer.description(true).await?,
    })?).await?;
    let answer: Answer = serde_json::from_value(timeout(crate::native_signaling::PREPARATION_WAIT,stream.receive_answer()).await
        .context("等待被控端屏幕共享授权或编码器准备超时")??)
        .map_err(|_| anyhow::anyhow!("invalid native answer"))?;
    ensure!(
        answer.version == 1 && answer.view_only,
        "unsupported native session capabilities"
    );
    validate_video(answer.width, answer.height, answer.fps)?;
    // Complete dimension-dependent driver work before setting remote SDP can
    // start ICE/DTLS and release the first burst of video into the AU queue.
    peer.prepare_video(answer.width, answer.height, &stats).await?;
    stats.configured_fps.store(answer.fps,Ordering::Release);
    match stream.lan_peer() {
        Some(ip) => peer.remote(answer.sdp, ip).await?,
        None => peer.remote_direct(answer.sdp).await?,
    }
    peer.connected().await?;
    if stream.lan_peer().is_none() {stats.diagnostics.record(peer.direct_path().await?);}
    let mut control_active=true;
    let control=controls.run();tokio::pin!(control);
    loop {
        tokio::select! {
            result=&mut control,if control_active=>{
                control_active=false;
                if let Err(error)=result {tracing::warn!(%error,"native control unavailable; video continues");}
                stats.capabilities.enable(crate::capabilities::Capability::Input,false);
                stats.native_input_available.store(false,Ordering::Release);
                let _=timeout(Duration::from_secs(1),channel.close()).await;
            },
            _=stream.disconnected()=>{
                ensure!(stats.decoded_total.load(Ordering::Relaxed)>0,"native host ended before delivering video");
                break;
            },
            state=peer.states.changed()=>{
                if state.is_err()||matches!(*peer.states.borrow(),RTCPeerConnectionState::Failed|RTCPeerConnectionState::Closed){
                    bail!("native host disconnected");
                }
            },
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::frame_channel;
    struct InvitationFile(PathBuf);
    impl Drop for InvitationFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn config() -> (HostConfig, InvitationFile) {
        let file = InvitationFile(std::env::temp_dir().join(format!(
            "uu-video-{}.eclipse-invite.json",
            uuid::Uuid::new_v4()
        )));
        let config = HostConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            source: uu_capture::CaptureSource::Synthetic {
                width: 320,
                height: 180,
                fps: 60,
            },
            invitation_file: file.0.clone(),
            invitation_lifetime: Duration::from_secs(60),
            fps: 60,
            bitrate_kbps: 2000,
            encoder: EncoderPreference::Software,
            session_limit: Some(Duration::from_secs(10)),
            allow_input:false,
            audio:None, clipboard:None,
        };
        (config, file)
    }
    async fn invitation(file: &InvitationFile) -> Invitation {
        timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(invite) = Invitation::load(&file.0) {
                    return invite;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn synthetic_source_cannot_authorize_system_audio() {
        let (mut config,file)=config();config.audio=Some(AudioSource::System);
        let (_stop,rx)=watch::channel(false);
        assert!(serve(config,rx).await.is_err());assert!(!file.0.exists());
    }
    #[tokio::test(flavor="multi_thread",worker_threads=2)]
    async fn encrypted_audio_recovers_single_packet_losses_without_opening_an_output_device() {
        let (mut config,file)=config();config.audio=Some(AudioSource::Synthetic);
        let (host_stop,rx)=watch::channel(false);let (phase,host_phase)=watch::channel(HostStatus::Starting);let host=tokio::spawn(serve_with_status(config,rx,Some(phase)));
        let invitation=invitation(&file).await;
        let stats=Arc::new(MediaStats::default());stats.audio.output_enabled.store(false,Ordering::Release);
        stats.audio.test_loss_every.store(5,Ordering::Release);
        let (frames,mut receiver)=frame_channel();let (stop,rx)=watch::channel(false);
        let client_stats=stats.clone();let viewer=tokio::spawn(async move{view(&invitation,frames,client_stats,rx).await});
        let result=timeout(Duration::from_secs(10),async {
            loop {
                while receiver.try_recv().is_ok(){}
                if stats.decoded_total.load(Ordering::Relaxed)>=45 && stats.audio.recovered.load(Ordering::Relaxed)>=5
                    && stats.audio.decoded_samples.load(Ordering::Relaxed)>=24000 {break;}
                assert!(!viewer.is_finished(),"audio/video viewer ended unexpectedly");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await;
        let phase=host_phase.borrow().clone();
        stop.send_replace(true);let viewer_result=timeout(Duration::from_secs(3),viewer).await;
        host_stop.send_replace(true);let report=timeout(Duration::from_secs(3),host).await.unwrap().unwrap().unwrap();
        assert!(result.is_ok(),"video={} audio_packets={} samples={} recovered={} dropped={} level={} phase={phase:?} report={report:?}",stats.decoded_total.load(Ordering::Relaxed),stats.audio.packets.load(Ordering::Relaxed),stats.audio.decoded_samples.load(Ordering::Relaxed),stats.audio.recovered.load(Ordering::Relaxed),stats.audio.dropped.load(Ordering::Relaxed),stats.audio.level_x10000.load(Ordering::Relaxed));
        viewer_result.unwrap().unwrap().unwrap();
        assert!(stats.audio.level_x10000.load(Ordering::Relaxed)>100);
        assert!(stats.audio.injected_loss.load(Ordering::Relaxed)>=5);
        assert_eq!(stats.errors.load(Ordering::Relaxed),0);
        assert!(report.audio_packets>40);assert_eq!(report.audio_errors,0);
        assert!(!file.0.exists());
    }
    #[tokio::test]
    async fn stopped_waiting_host_removes_its_invitation_and_publishes_progress() {
        let (config, file) = config();
        let (stop, rx) = watch::channel(false);
        let (status, mut state) = watch::channel(HostStatus::Starting);
        let host = tokio::spawn(serve_with_status(config, rx, Some(status)));
        timeout(Duration::from_secs(5), state.changed()).await.unwrap().unwrap();
        assert!(matches!(*state.borrow_and_update(), HostStatus::Waiting { .. }));
        assert!(file.0.exists());
        stop.send_replace(true);
        let report = timeout(Duration::from_secs(2), host).await.unwrap().unwrap().unwrap();
        assert_eq!(report.captured, 0);
        assert!(!file.0.exists());
    }
    #[tokio::test]
    async fn synthetic_sources_cannot_grant_desktop_input_or_publish_an_invitation() {
        let (mut config,file)=config();config.allow_input=true;
        let (_keep_alive,stop)=watch::channel(false);
        assert!(serve(config,stop).await.is_err());
        assert!(!file.0.exists());
    }
    #[test]
    fn software_video_fits_its_advertised_h264_level() {
        assert!(validate_video(4096, 2160, 60).is_ok());
        assert!(validate_video(5120, 1440, 60).is_ok());
        assert!(validate_video(8100, 1164, 60).is_err());
        assert!(validate_video(8192, 8192, 30).is_err());
        assert!(validate_video(1920, 1080, 120).is_err());
        assert!(validate_video(1919, 1080, 60).is_err());
    }

    async fn wait_clipboard(mut condition: impl FnMut() -> bool) -> Result<()> {
        timeout(Duration::from_secs(3), async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("clipboard state did not converge")?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_text_clipboard_requires_both_permissions_and_stops_without_stopping_video() {
        use crate::capabilities::Capability;
        let (mut config, file) = config();
        let source = Arc::new(crate::clipboard::Clipboard::default());
        source.available.store(true, Ordering::Release);
        source.enabled.store(true, Ordering::Release);
        config.clipboard = Some(source.clone());
        let (host_stop, rx) = watch::channel(false);
        let host = tokio::spawn(serve(config, rx));
        let invite = invitation(&file).await;
        let (frames, _images) = frame_channel();
        let stats = Arc::new(MediaStats::default());
        stats.clipboard.available.store(true, Ordering::Release);
        let (view_stop, rx) = watch::channel(false);
        let client_stats = stats.clone();
        let client = tokio::spawn(async move { view(&invite, frames, client_stats, rx).await });
        let validation = async {
            wait_clipboard(|| {
                stats.capabilities.allows(Capability::Clipboard)
                    && stats.decoded_total.load(Ordering::Relaxed) > 10
            })
            .await?;
            ensure!(
                !source.active.load(Ordering::Acquire) && source.received.load(Ordering::Relaxed) == 0,
                "clipboard activated without local consent"
            );
            stats.clipboard.enabled.store(true, Ordering::Release);
            stats.clipboard.active.store(true, Ordering::Release);
            wait_clipboard(|| source.active.load(Ordering::Acquire)).await?;
            wait_clipboard(|| {
                *stats.clipboard.local.lock().unwrap() = Some("generated local 中文🙂".into());
                source.remote.lock().unwrap().as_deref() == Some("generated local 中文🙂")
            })
            .await?;
            wait_clipboard(|| {
                *source.local.lock().unwrap() = Some("generated remote text".into());
                stats.clipboard.remote.lock().unwrap().as_deref() == Some("generated remote text")
            })
            .await?;
            stats.clipboard.active.store(false, Ordering::Release);
            wait_clipboard(|| !source.active.load(Ordering::Acquire)).await?;
            let received = stats.clipboard.received.load(Ordering::Relaxed);
            *source.local.lock().unwrap() = Some("inactive text must stay local".into());
            let decoded = stats.decoded_total.load(Ordering::Relaxed);
            wait_clipboard(|| stats.decoded_total.load(Ordering::Relaxed) > decoded + 15).await?;
            ensure!(
                stats.clipboard.received.load(Ordering::Relaxed) == received,
                "inactive controller received clipboard text"
            );
            ensure!(
                stats.errors.load(Ordering::Relaxed) == 0,
                "clipboard transport disturbed video"
            );
            Ok::<_, anyhow::Error>(())
        }
        .await;
        view_stop.send_replace(true);
        let viewed = timeout(Duration::from_secs(5), client).await;
        host_stop.send_replace(true);
        let hosted = timeout(Duration::from_secs(5), host).await;
        validation.unwrap();
        viewed.unwrap().unwrap().unwrap();
        hosted.unwrap().unwrap().unwrap();
        assert!(
            source.stopped.load(Ordering::Acquire) && stats.clipboard.stopped.load(Ordering::Acquire)
        );
        assert!(
            source.remote.lock().unwrap().is_none() && stats.clipboard.remote.lock().unwrap().is_none()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn encrypted_native_video_decodes_moving_frames_and_closes_both_ends() {
        let (config, file) = config();
        let (host_stop, rx) = watch::channel(false);
        let host = tokio::spawn(serve(config, rx));
        let invite = invitation(&file).await;
        let (frames, mut images) = frame_channel();
        let stats = Arc::new(MediaStats::default());
        let view_stats = stats.clone();
        let (view_stop, rx) = watch::channel(false);
        let client = tokio::spawn(async move { view(&invite, frames, view_stats, rx).await });
        let mut first = None;
        let mut latest = None;
        timeout(Duration::from_secs(12), async {
            loop {
                while let Ok(frame) = images.try_recv() {
                    assert_eq!((frame.1, frame.2), (320, 180));
                    if first.is_none() {
                        first = Some(frame.3.clone());
                    }
                    latest = Some(frame.3);
                }
                if stats.decoded_total.load(Ordering::Relaxed) >= 30
                    && stats.control_open.load(Ordering::Acquire) == 1
                {
                    break;
                }
                ensure!(
                    !client.is_finished(),
                    "native client ended before receiving video"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .unwrap()
        .unwrap();
        assert_ne!(
            first.unwrap(),
            latest.unwrap(),
            "native video must actually move"
        );
        assert_eq!(stats.errors.load(Ordering::Relaxed), 0);
        assert!(stats.packets.load(Ordering::Relaxed) > 0);
        view_stop.send(true).unwrap();
        timeout(Duration::from_secs(5), client)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let report = timeout(Duration::from_secs(5), host)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(report.sent >= 30 && report.captured >= report.sent && report.bytes > 0);
        assert_eq!(stats.control_open.load(Ordering::Acquire), 0);
        drop(host_stop);
    }
    #[tokio::test]
    async fn unauthenticated_connections_do_not_open_the_capture_source() {
        let (mut config, file) = config();
        config.source = uu_capture::CaptureSource::Monitor("nonexistent-owned-test-output".into());
        let (stop, rx) = watch::channel(false);
        let host = tokio::spawn(serve(config, rx));
        let invite = invitation(&file).await;
        let socket = tokio::net::TcpStream::connect(invite.address())
            .await
            .unwrap();
        drop(socket);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !host.is_finished(),
            "capture was opened before authentication"
        );
        stop.send(true).unwrap();
        let report = timeout(Duration::from_secs(2), host)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(report.captured, 0);
    }
    #[tokio::test]
    async fn unscoped_listeners_are_rejected_without_creating_an_invitation() {
        let (mut config, file) = config();
        config.bind = "0.0.0.0:0".parse().unwrap();
        let (_stop, rx) = watch::channel(false);
        assert!(serve(config, rx).await.is_err());
        assert!(!file.0.exists());
    }
    #[tokio::test]
    async fn oversized_signaling_is_rejected_before_allocating_its_body() {
        let (mut writer, mut reader) = tokio::io::duplex(8);
        writer.write_u32(u32::MAX).await.unwrap();
        assert!(read_message::<Offer>(&mut reader).await.is_err());
    }
}
