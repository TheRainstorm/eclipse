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
    peer_connection::{
        peer_connection_state::RTCPeerConnectionState,
        sdp::session_description::RTCSessionDescription,
    },
};

const MAX_SIGNAL: usize = 256 * 1024;
pub use uu_capture::CaptureSource;
pub use uu_media::audio_capture::Source as AudioSource;
pub use uu_media::encoder::Preference as EncoderPreference;
pub use uu_proto::media::VideoCodec;

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
        let settings=uu_media::encoder::Settings {tuning:Default::default(),codec:uu_proto::media::VideoCodec::H264,rate_control:uu_media::encoder::RateControl::Cbr,width:self.width,height:self.height,fps,
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
    #[serde(default,skip_serializing_if="is_h264")]
    codec:VideoCodec,
    version: u32,
    sdp: RTCSessionDescription,
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
    view_only: bool,
}

fn is_h264(codec:&VideoCodec)->bool{*codec==VideoCodec::H264}

fn validate_video(width: u32, height: u32, fps: u32) -> Result<()> {
    ensure!(
        (2..=8192).contains(&width)
            && (2..=8192).contains(&height)
            && width % 2 == 0
            && height % 2 == 0
            && (1..=crate::native_quality::MAX_FPS).contains(&fps),
        "invalid native video dimensions or frame rate"
    );
    ensure!(
        u64::from(width)*u64::from(height)*3/2<=64*1024*1024,
        "native frame exceeds the capture and decoder buffer budget"
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
    pub encoder_tuning:uu_media::encoder::Tuning,
    pub session:Option<crate::shared_host::SessionContext>,
    pub video_policy:Option<crate::native_quality::Policy>,
    pub codec:VideoCodec,
    pub rate_control:uu_media::encoder::RateControl,
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
    pub encoder_tuning:uu_media::encoder::Tuning,
    pub session:Option<crate::shared_host::SessionContext>,
    pub video_policy:Option<crate::native_quality::Policy>,
    pub codec:VideoCodec,
    pub rate_control:uu_media::encoder::RateControl,
    pub source: CaptureSource,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub encoder: EncoderPreference,
    pub session_limit: Option<Duration>,
    pub allow_input: bool,
    pub audio: Option<AudioSource>,
    pub clipboard: Option<Arc<crate::clipboard::Clipboard>>,
}
impl From<&HostConfig> for SharingConfig {
    fn from(c:&HostConfig)->Self {Self{encoder_tuning:c.encoder_tuning,session:c.session.clone(),video_policy:c.video_policy.clone(),codec:c.codec,rate_control:c.rate_control,source:c.source.clone(),fps:c.fps,bitrate_kbps:c.bitrate_kbps,encoder:c.encoder,
        session_limit:c.session_limit,allow_input:c.allow_input,audio:c.audio,clipboard:c.clipboard.clone()}}
}
impl From<HostConfig> for SharingConfig {fn from(c:HostConfig)->Self{Self::from(&c)}}
impl SharingConfig {
    pub fn validate(&self) -> Result<()> {
        self.encoder_tuning.validate()?;
        if let Some(policy)=&self.video_policy {policy.validate()?;}
        ensure!((1..=crate::native_quality::MAX_FPS).contains(&self.fps) && (64..=crate::native_quality::MAX_BITRATE_KBPS).contains(&self.bitrate_kbps),
            "native frame rate or bitrate exceeds the encoder interface range");
        let peak=self.rate_control.peak(self.bitrate_kbps);
        ensure!(peak>=self.bitrate_kbps && peak<=2_000_000,"VBR 峰值超出编码接口范围");
        ensure!(self.encoder!=EncoderPreference::Software || (self.codec==VideoCodec::H264 && self.rate_control==uu_media::encoder::RateControl::Cbr),"所选编码格式或 VBR 需要硬件编码");
        let monitor = matches!(self.source,CaptureSource::Primary|CaptureSource::Desktop|CaptureSource::Monitor(_));
        ensure!(!self.allow_input || monitor, "desktop input requires an explicitly shared monitor");
        ensure!(self.audio != Some(AudioSource::System) || monitor, "system audio requires an explicitly shared monitor");
        Ok(())
    }
}

/// The provider must return authenticated, session-bound SDP. No capture is
/// opened until the authenticated offer and direct-only candidates are checked.
pub async fn serve_direct(signaling: &mut dyn SessionSignaling, ice: &crate::direct_ice::InternetIce,
    config: SharingConfig, stop: watch::Receiver<bool>, status: Option<watch::Sender<HostStatus>>) -> Result<HostReport> {
    serve_direct_with_authority(signaling, ice, config, stop, status, Arc::default()).await
}
pub(crate) async fn serve_direct_with_authority(signaling: &mut dyn SessionSignaling, ice: &crate::direct_ice::InternetIce,
    config: SharingConfig, stop: watch::Receiver<bool>, status: Option<watch::Sender<HostStatus>>, authority: Arc<crate::input_ownership::Authority>) -> Result<HostReport> {
    let _clipboard=ClipboardLifetime(config.clipboard.clone());
    config.validate()?;
    let displays=matches!(config.source,CaptureSource::Desktop).then(||Arc::new(crate::native_displays::Host::default()));
    let mut report=HostReport::default();
    let started=Instant::now();
    loop {
        let mut current=config.clone();
        if let Some(limit)=config.session_limit {if started.elapsed()>=limit{return Ok(report);}current.session_limit=Some(limit.saturating_sub(started.elapsed()));}
        let mut peer=NativePeer::new_direct(ice,None,true).await?;
        let result=serve_peer(&mut peer,signaling,current,stop.clone(),status.clone(),displays.clone(),authority.clone()).await;
        peer.close().await;
        report.merge(result?);
        if *stop.borrow() || !displays.as_ref().is_some_and(|d|d.switching()){return Ok(report);}
    }
}

pub async fn view_direct(signaling: &mut dyn SessionSignaling, ice: &crate::direct_ice::InternetIce,
    frames: FrameSender, stats: Arc<MediaStats>, mut stop: watch::Receiver<bool>,
    mut input: Option<crate::native_input::NativeInputReceiver>) -> Result<()> {
    let _lifetime=ViewerLifetime(stats.clone());
    stats.capabilities.set(0);
    stats.native_input_available.store(false, Ordering::Release);
    tokio::select! {_=crate::native_media::receive_codecs(&stats)=>{},_=cancelled(&mut stop)=>return Ok(())}
    let result=loop {
        let frames=frames.renew();
        stats.native_media_epoch.store(frames.generation(),Ordering::Release);
        let mut peer = NativePeer::new_direct(ice, Some(stats.clone()), false).await?;
        let result = tokio::select! {
            result=receive_session(&mut peer,signaling,frames,stats.clone(),input)=>result,
            _=cancelled(&mut stop)=>Ok(ViewOutcome::Ended),
        };
        peer.close().await;
        match result {Ok(ViewOutcome::Switch(receiver))=>input=receiver,other=>break other.map(|_|())}
    };
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

impl HostReport {
    fn merge(&mut self,other:Self){
        self.encoder=other.encoder;
        self.captured+=other.captured;self.sent+=other.sent;self.bytes+=other.bytes;
        self.producer_ms+=other.producer_ms;self.capture_us+=other.capture_us;self.encode_us+=other.encode_us;self.backpressure_us+=other.backpressure_us;
        self.input_events+=other.input_events;self.input_errors+=other.input_errors;
        self.audio_packets+=other.audio_packets;self.audio_bytes+=other.audio_bytes;self.audio_errors+=other.audio_errors;self.audio_queue_drops+=other.audio_queue_drops;
    }
}

struct ViewerLifetime(Arc<MediaStats>);
impl Drop for ViewerLifetime {
    fn drop(&mut self){
        self.0.control_open.store(0,Ordering::Release);
        self.0.host_admin.disconnect();
        self.0.capabilities.enable(crate::capabilities::Capability::Input,false);
        self.0.native_input_available.store(false,Ordering::Release);
        self.0.audio.stop();crate::native_clipboard::stop(&self.0.clipboard);
        self.0.capabilities.enable(crate::capabilities::Capability::Audio,false);
        self.0.capabilities.enable(crate::capabilities::Capability::Quality,false);
    }
}

struct ClipboardLifetime(Option<Arc<crate::clipboard::Clipboard>>);
impl Drop for ClipboardLifetime {fn drop(&mut self){if let Some(c)=&self.0{crate::native_clipboard::stop(c);}}}

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

pub async fn serve_with_status(config: HostConfig, stop: watch::Receiver<bool>, status: Option<watch::Sender<HostStatus>>) -> Result<HostReport> {
    serve_with_authority(config, stop, status, Arc::default()).await
}
pub(crate) async fn serve_with_authority(config: HostConfig, mut stop: watch::Receiver<bool>, status: Option<watch::Sender<HostStatus>>, authority: Arc<crate::input_ownership::Authority>) -> Result<HostReport> {
    let _clipboard=ClipboardLifetime(config.clipboard.clone());
    ensure!(
        !config.bind.ip().is_unspecified() && !config.bind.ip().is_multicast(),
        "choose a concrete native host interface"
    );
    SharingConfig::from(&config).validate()?;
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
    host_session(socket, config, stop, status, authority).await
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
    quality:crate::native_quality::Runtime,
    quality_state:watch::Receiver<uu_media::encoder::Settings>,
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
        settings: uu_media::encoder::Settings,
        refresh: Arc<AtomicBool>,
        encoder: Box<dyn uu_media::VideoEncoder>,
        live_timing:Arc<crate::native_timing::HostTiming>,
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
        let (quality,changes)=crate::native_quality::Runtime::channel();
        let (quality_status,quality_state)=watch::channel(settings);
        let task = tokio::task::spawn_blocking(move || {
            let result = encode_source(capture, settings, encoder, refresh, worker_stop, counter, worker_timings, live_timing,encoder_status,changes,quality_status, &tx);
            if let Err(error) = result {
                let _ = tx.blocking_send(Err(error));
            }
        });
        Self {
            quality,quality_state,
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
    mut settings: uu_media::encoder::Settings,
    mut encoder: Box<dyn uu_media::VideoEncoder>,
    refresh: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    captured: Arc<AtomicU64>,
    timings: Arc<ProducerTimings>,
    live_timing:Arc<crate::native_timing::HostTiming>,
    encoder_status: watch::Sender<String>,
    changes:std::sync::mpsc::Receiver<crate::native_quality::Change>,
    quality_status:watch::Sender<uu_media::encoder::Settings>,
    output: &mpsc::Sender<Result<uu_media::EncodedFrame>>,
) -> Result<()> {
    let mut backend=encoder.backend_name().to_owned();
    let mut interval = Duration::from_secs_f64(1.0 / settings.fps as f64);
    let mut next = Instant::now();
    encoder.request_keyframe();
    while !stopped.load(Ordering::Acquire) {
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
        if stopped.load(Ordering::Acquire) {
            break;
        }
        if let Ok(change)=changes.try_recv() {
            let result=(||->Result<u32> {
                ensure!(!change.reply.is_closed()&&!stopped.load(Ordering::Acquire),"画质请求已取消");
                change.settings.validate()?;
                let bitrate_kbps=change.settings.bitrate(settings.width,settings.height);
                let rate_control=match settings.rate_control {uu_media::encoder::RateControl::Cbr=>uu_media::encoder::RateControl::Cbr,_=>uu_media::encoder::RateControl::Vbr{peak_kbps:bitrate_kbps.saturating_mul(2)}};
                let updated=uu_media::encoder::Settings{fps:change.settings.fps,bitrate_kbps,rate_control,..settings};
                updated.validate()?;
                if updated.fps!=settings.fps || updated.rate_control!=settings.rate_control {
                    // Driver contexts stay on their own threads. Build first,
                    // then start a fresh reference chain on the same transport.
                    let reopen=if encoder.is_hardware()&&updated.preference==EncoderPreference::Auto {uu_media::encoder::Settings{preference:EncoderPreference::Hardware,..updated}}else{updated};
                    let replacement=uu_media::encoder::create_with_device(reopen,capture.encoder_input_device())?;
                    ensure!(!change.reply.is_closed()&&!stopped.load(Ordering::Acquire),"画质请求已取消");
                    capture.set_frame_rate(updated.fps)?;
                    encoder=replacement;
                    encoder.request_keyframe();
                    interval=Duration::from_secs_f64(1.0/updated.fps as f64);
                    next=Instant::now();
                }else{encoder.set_bitrate(bitrate_kbps)?;}
                // The encoder owns whether a bitrate update invalidated its
                // references; replacement encoders already start a new chain.
                settings=updated;
                quality_status.send_replace(settings);
                Ok(bitrate_kbps)
            })().map_err(|error|error.to_string());
            let _=change.reply.send(result);
        }
        let started = Instant::now();
        capture.prefer_gpu(encoder.gpu_input(),encoder.gpu_vendor());
        let frame = match capture.next_frame() {
            Ok(frame) => frame,
            Err(uu_capture::CaptureError::Suspended(_)) => {
                // Desktop transitions must not terminate an authenticated
                // transport or transmit a cached image of the old desktop.
                refresh.store(true, Ordering::Release);
                next = Instant::now() + Duration::from_millis(25);
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let capture_us=elapsed_us(started);
        timings.capture_us.fetch_add(capture_us, Ordering::Relaxed);
        captured.fetch_add(1, Ordering::Relaxed);
        // RTCP already coalesces and limits recovery requests. Once requested,
        // renew references on the next frame, even just after a previous IDR.
        // Swap also preserves a request arriving while this frame is encoded.
        let requested = refresh.swap(false, Ordering::AcqRel);
        if requested {
            encoder.request_keyframe();
        }
        let started = Instant::now();
        let encoded_frames = encoder.encode(&frame)?;
        if encoder.backend_name()!=backend {
            backend=encoder.backend_name().to_owned();encoder_status.send_replace(backend.clone());
        }
        let encode_us=elapsed_us(started);
        timings.encode_us.fetch_add(encode_us, Ordering::Relaxed);
        let started = Instant::now();
        for encoded in encoded_frames {
            if output.blocking_send(Ok(encoded)).is_err() {
                return Ok(());
            }
        }
        let wait_us=elapsed_us(started);
        timings.backpressure_us.fetch_add(wait_us, Ordering::Relaxed);
        live_timing.video(capture_us,encode_us,wait_us);
        next = (next + interval).max(Instant::now());
    }
    Ok(())
}

async fn host_session(
    stream: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    config: HostConfig,
    stop: watch::Receiver<bool>,
    status: Option<watch::Sender<HostStatus>>,
    authority: Arc<crate::input_ownership::Authority>,
) -> Result<HostReport> {
    let local_ip = stream.get_ref().0.local_addr()?.ip();
    let peer_ip = stream.get_ref().0.peer_addr()?.ip();
    let displays=matches!(config.source,CaptureSource::Desktop).then(||Arc::new(crate::native_displays::Host::default()));
    let config:SharingConfig=config.into();
    let mut signaling = TlsSignaling { stream, peer: peer_ip };
    let mut report=HostReport::default();
    let started=Instant::now();
    let result=loop {
        let mut current=config.clone();
        if let Some(limit)=config.session_limit {if started.elapsed()>=limit{break Ok(report);}current.session_limit=Some(limit.saturating_sub(started.elapsed()));}
        let mut peer=NativePeer::new(local_ip,None,true).await?;
        let result=serve_peer(&mut peer,&mut signaling,current,stop.clone(),status.clone(),displays.clone(),authority.clone()).await;
        peer.close().await;
        match result {Ok(current)=>report.merge(current),Err(error)=>break Err(error)}
        if *stop.borrow() || !displays.as_ref().is_some_and(|d|d.switching()){break Ok(report);}
    };
    let _ = signaling.stream.shutdown().await;
    result
}

async fn serve_peer(peer: &mut NativePeer, signaling: &mut dyn SessionSignaling, mut config: SharingConfig,
    mut stop: watch::Receiver<bool>, status: Option<watch::Sender<HostStatus>>,displays:Option<Arc<crate::native_displays::Host>>,authority:Arc<crate::input_ownership::Authority>) -> Result<HostReport> {
    let message = tokio::select! {result=signaling.receive()=>result?,_=cancelled(&mut stop)=>return Ok(HostReport::default())};
    let offer: Offer = serde_json::from_value(message).map_err(|_| anyhow::anyhow!("invalid native offer"))?;
    ensure!(offer.version == 1, "unsupported native protocol version");
    if let Some(displays)=&displays {
        let displays=displays.clone();
        config.source=tokio::task::spawn_blocking(move||displays.prepare()).await??;
    }
    progress(&status,HostStatus::Preparing);
    let mut answered=false;
    let result=send_video(peer,signaling,config,stop,offer,status,displays,&mut answered,authority).await;
    if let Err(error)=&result {
        tracing::warn!(error=%format!("{error:#}"),answered,"native media session failed");
        if !answered {
            let reason=format!("{error:#}").chars().map(|c|if c.is_control(){' '}else{c}).take(512).collect::<String>();
            let _=timeout(Duration::from_secs(1),signaling.send(serde_json::json!({"version":1,"preparation_error":reason}))).await;
        }
    }
    result
}

fn codec_rank(codec: VideoCodec) -> u8 {
    match codec {
        VideoCodec::Av1 => 0,
        VideoCodec::Hevc => 1,
        VideoCodec::H264 => 2,
    }
}

fn ordered_codecs(codecs: &[VideoCodec]) -> Vec<VideoCodec> {
    let mut values = codecs.to_vec();
    values.sort_by_key(|codec| codec_rank(*codec));
    values.dedup();
    values
}

fn codec_choices(allowed:&[VideoCodec],host:Option<VideoCodec>,client:Option<VideoCodec>)->Result<Vec<VideoCodec>> {
    if let Some(codec)=host.or(client) {
        ensure!(allowed.contains(&codec),"主机未允许 {:?} 编码，请选择允许的格式",codec);
        // An explicit format is a requirement. Report backend failures instead
        // of silently substituting H.264 while the UI still shows HEVC/AV1.
        Ok(vec![codec])
    } else {
        Ok(ordered_codecs(allowed))
    }
}

async fn send_video(
    peer: &mut NativePeer,
    stream: &mut dyn SessionSignaling,
    mut config: SharingConfig,
    mut stop: watch::Receiver<bool>,
    offer: Offer,
    status: Option<watch::Sender<HostStatus>>,
    displays:Option<Arc<crate::native_displays::Host>>,
    answered:&mut bool,
    authority: Arc<crate::input_ownership::Authority>,
) -> Result<HostReport> {
    let refresh = Arc::new(AtomicBool::new(false));
    let preparation_started=Instant::now();
    let dynamic=crate::native_quality::runtime_requested(&offer.sdp)?;
    let request=crate::native_quality::Request::read(&offer.sdp)?;
    let policy=config.video_policy.clone();
    let request=if policy.is_some(){request}else{None};
    if let Some(request)=&request {
        config.fps=if dynamic {request.fps}else{request.fps.min(60)};
        config.bitrate_kbps=request.bitrate_kbps.unwrap_or(config.bitrate_kbps);
        if let uu_media::encoder::RateControl::Vbr{peak_kbps}=config.rate_control {
            config.rate_control=uu_media::encoder::RateControl::Vbr{peak_kbps:peak_kbps.max(config.bitrate_kbps)};
        }
    }
    let allowed=policy.as_ref().map(|p|p.codecs.clone()).unwrap_or_else(||vec![config.codec]);
    let ordered=ordered_codecs(&allowed);
    let host_codec=policy.as_ref().map_or(Some(config.codec),|p|p.preferred_codec);
    let mut choices=codec_choices(&allowed,host_codec,request.as_ref().and_then(|r|r.codec))?;
    choices.retain(|codec|crate::native_media::offers_codec(&offer.sdp,*codec));
    ensure!(!choices.is_empty(),"两端没有共同可用的视频编码，请检查主机编码设置和客户端解码支持");
    let advertised:Vec<_>=ordered.into_iter().filter(|c|crate::native_media::offers_codec(&offer.sdp,*c)).collect();
    let audio_mode=if config.audio.is_some(){crate::native_media::offered_audio(&offer.sdp)?}else{None};
    let clipboard_images=crate::native_clipboard::images_offered(&offer.sdp);
    let typed_gamepads=crate::native_gamepad::typed_offered(&offer.sdp);
    match stream.lan_peer() {
        Some(ip) => peer.remote(offer.sdp, ip).await?,
        None => peer.remote_direct(offer.sdp).await?,
    }
    // Authentication and a valid media offer precede capture access.
    let (fps,kbps,preference,rate_control)=(config.fps,config.bitrate_kbps,config.encoder,config.rate_control);
    let bounds=request.as_ref().and_then(|r|r.bounds);
    let automatic=request.as_ref().is_some_and(|r|r.bitrate_kbps.is_none());
    struct CancelCaptureOpen(Arc<AtomicBool>);
    impl Drop for CancelCaptureOpen { fn drop(&mut self) { self.0.store(true, Ordering::Release); } }
    let capture_cancel = CancelCaptureOpen(Arc::new(AtomicBool::new(false)));
    let capture_options = uu_capture::OpenOptions { allow_input: config.allow_input, fps, cancelled:capture_cancel.0.clone() };
    let (capture,encoder,codec,available,encoder_settings,mut startup) = tokio::select! {
        result=tokio::task::spawn_blocking(move || -> Result<_> {
            let started=Instant::now();
            let mut capture=uu_capture::open_with_options(&config.source,capture_options)?;
            capture.set_frame_rate(fps)?;
            let capture=uu_capture::scale::bounded(capture,bounds);
            let (width,height)=capture.resolution();validate_video(width,height,fps)?;
            let kbps=if automatic {crate::native_quality::Update{fps,bitrate_kbps:None}.bitrate(width,height)}else{kbps};
            let rate_control=match rate_control {uu_media::encoder::RateControl::Cbr=>rate_control,_=>uu_media::encoder::RateControl::Vbr{peak_kbps:kbps.saturating_mul(2)}};
            let settings=|codec|uu_media::encoder::Settings {tuning:config.encoder_tuning,codec,rate_control,width,height,fps,bitrate_kbps:kbps,preference};
            let capture_us=elapsed_us(started);let started=Instant::now();
            let mut selected=None;let mut last_error=None;let mut attempted=Vec::new();
            for codec in choices {
                attempted.push(codec);
                match uu_media::encoder::create_with_device(settings(codec),capture.encoder_input_device()) {
                    Ok(encoder)=>{selected=Some((codec,encoder));break;},
                    Err(error)=>{tracing::warn!(?codec,?preference,%error,"native codec initialization failed");last_error=Some(anyhow::anyhow!("{codec:?}: {error}"));},
                }
            }
            let (codec,encoder)=selected.ok_or_else(||anyhow::anyhow!("主机无法初始化请求的编码器：{}",last_error.map(|e|e.to_string()).unwrap_or_default()))?;
            let encoder_us=elapsed_us(started);let started=Instant::now();
            let mut available=vec![codec];
            if policy.is_some(){
                // Selection already tested the failed higher-ranked codecs at
                // this geometry. Retrying them delays the answer and can report
                // a different capability for the same preparation attempt.
                for candidate in advertised.into_iter().filter(|c|!attempted.contains(c)) {
                    if uu_media::encoder::create_with_device(settings(candidate),capture.encoder_input_device()).is_ok(){available.push(candidate);}
                }
            }
            let startup=crate::native_quality::Startup{capture_us,encoder_us,other_encoders_us:elapsed_us(started),..Default::default()};
            Ok((capture,encoder,codec,available,settings(codec),startup))
        })=>result??,
        _=cancelled(&mut stop)=>return Ok(HostReport::default()),
        _=stream.disconnected()=>return Ok(HostReport::default()),
    };
    config.codec=codec;
    config.bitrate_kbps=encoder_settings.bitrate_kbps;
    let permissions=config.session.as_ref().map_or(crate::device_trust::Permissions::CONTROL,|s|s.permissions);
    if !permissions.details().clipboard {config.clipboard=None;}
    let mut track=peer.sender_for_codec(refresh.clone(),codec).await?;
    let mut audio_sender=if let Some(red)=audio_mode {Some(peer.audio_sender(red).await?)}else{None};
    let (width,height)=capture.resolution();
    validate_video(width,height,config.fps)?;
    let controls=tokio::select! {
        controls=crate::native_control::HostControl::with_injector(config.allow_input,capture.desktop_rect(),config.clipboard.clone(),capture.input_injector(),authority)=>controls,
        _=cancelled(&mut stop)=>return Ok(HostReport::default()),
    };
    controls.set_permissions(permissions);
    controls.clipboard_images(clipboard_images);
    controls.gamepad_types(typed_gamepads);
    controls.set_session(config.session.clone());
    if let Some(displays)=&displays {displays.dimensions(width,height);}
    controls.set_displays(displays.clone());
    controls.attach(&peer.pc);
    let ice_started=Instant::now();
    let mut description=peer.description(false).await?;
    startup.ice_us=elapsed_us(ice_started);startup.total_us=elapsed_us(preparation_started);
    startup.add(&mut description)?;
    if clipboard_images {crate::native_clipboard::advertise(&mut description);}
    if typed_gamepads {crate::native_gamepad::advertise(&mut description);}
    if config.video_policy.is_some() {
        crate::native_quality::Response{dynamic,codecs:available,max_fps:if dynamic{crate::native_quality::MAX_FPS}else{60},max_bitrate_kbps:if dynamic{crate::native_quality::MAX_BITRATE_KBPS}else{100_000}}.add(&mut description)?;
    }
    let answer = Answer {
        codec:config.codec,
        version: 1,
        sdp: description,
        width,
        height,
        fps: config.fps,
        bitrate_kbps: config.bitrate_kbps,
        view_only: true,
    };
    stream.send(serde_json::to_value(&answer)?).await?;
    *answered=true;
    let connected=tokio::select! {result=peer.connected()=>Some(result),_=cancelled(&mut stop)=>None};
    match connected {
        Some(Ok(()))=>{},
        Some(Err(error))=>{controls.stop().await;return Err(error);},
        None=>{controls.stop().await;return Ok(HostReport::default());},
    }
    if stream.lan_peer().is_none() {
        if let Err(error)=peer.direct_path().await {controls.stop().await;return Err(error);}
    }
    let mut producer = CaptureWorker::start(capture, encoder_settings, refresh, encoder,controls.timing());
    if dynamic {controls.set_quality(producer.quality.clone());}
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
                changed=producer.quality_state.changed()=>{
                    if changed.is_ok() {
                        let settings=*producer.quality_state.borrow_and_update();
                        config.fps=settings.fps;config.bitrate_kbps=settings.bitrate_kbps;
                        progress(&status,HostStatus::Streaming{width,height,fps:config.fps,audio:audio_label.clone(),encoder:encoder_label.clone()});
                    }
                },
                _ = cancelled(&mut stop) => break,
                _ = &mut deadline => break,
                // A source renewal sends its next Offer on this same signaling
                // stream. Never race a disconnect read against that Offer.
                _ = stream.disconnected(), if displays.is_none() => break,
                _ = controls.closed.notified(), if displays.is_some() => break,
                _ = async {match &displays {Some(d)=>d.changed.notified().await,None=>std::future::pending().await}} => break,
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
                    let bytes = encoded.data.len() as u64;
                    track.frame(&encoded).await?;
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

async fn view_session(invitation:&Invitation,frames:FrameSender,stats:Arc<MediaStats>,mut stop:watch::Receiver<bool>,mut input:Option<crate::native_input::NativeInputReceiver>)->Result<()> {
    let _lifetime=ViewerLifetime(stats.clone());
    tokio::select! {_=crate::native_media::receive_codecs(&stats)=>{},_=cancelled(&mut stop)=>return Ok(())}
    stats.connection.set_service("IP 直连");
    stats.capabilities.set(0);
    stats.native_input_available.store(false,Ordering::Release);
    let stream = tokio::select! {
        result=crate::native_pairing::connect(invitation)=>result?,
        _=cancelled(&mut stop)=>return Ok(()),
    };
    let local_ip = stream.get_ref().0.local_addr()?.ip();
    let advertised_ip = invitation.observed_client_ip().unwrap_or(local_ip);
    let peer_ip = stream.get_ref().0.peer_addr()?.ip();
    let mut signaling = TlsSignaling { stream, peer: peer_ip };
    let result=loop {
        let frames=frames.renew();
        stats.native_media_epoch.store(frames.generation(),Ordering::Release);
        let mut peer=NativePeer::new_with_advertised(local_ip,advertised_ip,Some(stats.clone()),false).await?;
        let result=tokio::select! {
            result=receive_session(&mut peer,&mut signaling,frames,stats.clone(),input)=>result,
            _=cancelled(&mut stop)=>Ok(ViewOutcome::Ended),
        };
        peer.close().await;
        match result {Ok(ViewOutcome::Switch(receiver))=>input=receiver,other=>break other.map(|_|())}
    };
    let _ = signaling.stream.shutdown().await;
    result
}

enum ViewOutcome {Ended,Switch(Option<crate::native_input::NativeInputReceiver>)}

async fn receive_session(
    peer: &mut NativePeer,
    stream: &mut dyn SessionSignaling,
    frames: FrameSender,
    stats: Arc<MediaStats>,
    input:Option<crate::native_input::NativeInputReceiver>,
) -> Result<ViewOutcome> {
    let first_frame_baseline=stats.decoded_total.load(Ordering::Acquire);
    peer.receiver(frames, stats.clone()).await?;
    let channel = peer
        .pc
        .create_data_channel("eclipse-control-v1", None)
        .await?;
    let gamepad=input.as_ref().map(|input|(input.gamepads.clone(),input.requested.clone(),input.epoch.clone()));
    let mut controls=crate::native_control::ViewerControl::new(channel.clone(),stats.clone(),input);
    let ice_started=Instant::now();
    let mut description=peer.description(true).await?;
    stats.diagnostics.record(serde_json::json!({"event":"native_local_ice_gather","elapsed_us":elapsed_us(ice_started)}));
    crate::native_clipboard::advertise(&mut description);
    crate::native_gamepad::advertise(&mut description);
    if let Some(request)=stats.native_quality_request.get(){request.add(&mut description)?;}
    stream.send(serde_json::to_value(Offer {
        version: 1,
        sdp: description,
    })?).await?;
    let answer_started=Instant::now();
    let message=timeout(crate::native_signaling::PREPARATION_WAIT,stream.receive_answer()).await
        .context("等待被控端屏幕共享授权或编码器准备超时")??;
    stats.diagnostics.record(serde_json::json!({"event":"native_answer_wait","elapsed_us":elapsed_us(answer_started)}));
    if let Some(reason)=message.get("preparation_error").and_then(|v|v.as_str()) {
        bail!("被控端准备失败：{}",reason.chars().take(512).collect::<String>());
    }
    let answer:Answer=serde_json::from_value(message).map_err(|_|anyhow::anyhow!("invalid native answer"))?;
    ensure!(
        answer.version == 1 && answer.view_only,
        "unsupported native session capabilities"
    );
    validate_video(answer.width, answer.height, answer.fps)?;
    match crate::native_quality::Startup::read(&answer.sdp) {
        Ok(Some(startup))=>stats.diagnostics.record(serde_json::json!({"event":"native_host_preparation","timing":startup})),
        Err(_)=>stats.diagnostics.record(serde_json::json!({"event":"native_host_preparation_invalid"})),
        Ok(None)=>{},
    }
    let quality=crate::native_quality::Response::read(&answer.sdp)?;
    let clipboard_images=crate::native_clipboard::images_offered(&answer.sdp);
    let typed_gamepads=crate::native_gamepad::typed_offered(&answer.sdp);
    if let Some(response)=&quality {ensure!(response.codecs.contains(&answer.codec),"主机实际编码与能力回报不一致");}
    stats.capabilities.enable(crate::capabilities::Capability::Quality,quality.is_some());
    stats.diagnostics.record(serde_json::json!({"event":"native_video_negotiated","width":answer.width,"height":answer.height,"fps":answer.fps,"bitrate_kbps":answer.bitrate_kbps,"codec":answer.codec,"client_request":stats.native_quality_request.get(),"host_capabilities":quality}));
    *stats.native_quality_response.lock().unwrap()=quality;
    // Complete dimension-dependent driver work before setting remote SDP can
    // start ICE/DTLS and release the first burst of video into the AU queue.
    peer.prepare_video(answer.width, answer.height, answer.codec,&stats).await?;
    stats.configured_fps.store(answer.fps,Ordering::Release);
    *stats.native_quality_current.lock().unwrap()=stats.native_quality_request.get().map(|request|crate::native_quality::Update{fps:answer.fps,bitrate_kbps:request.bitrate_kbps});
    match stream.lan_peer() {
        Some(ip) => peer.remote(answer.sdp, ip).await?,
        None => peer.remote_direct(answer.sdp).await?,
    }
    if let Some((samples,requested,epoch))=gamepad {
        let label=if typed_gamepads{crate::native_gamepad::LABEL_V2}else{crate::native_gamepad::LABEL};
        let options=webrtc::data_channel::data_channel_init::RTCDataChannelInit{ordered:Some(false),max_packet_life_time:Some(crate::native_gamepad::LIFETIME_MS),..Default::default()};
        let channel=peer.pc.create_data_channel(label,Some(options)).await?;
        controls.gamepads(Some(crate::native_gamepad::Viewer::start(channel,stats.clone(),samples,requested,epoch)));
    }
    // CONTROL already negotiated the SCTP association. Select one clipboard
    // version only after the authenticated answer, so old peers keep v1 and
    // text/images share a single ordered content stream on new peers.
    let label=if clipboard_images{crate::native_clipboard::LABEL_V2}else{crate::native_clipboard::LABEL};
    let clipboard=peer.pc.create_data_channel(label,None).await?;
    crate::native_clipboard::viewer(clipboard,stats.clone());
    peer.connected().await?;
    if stream.lan_peer().is_none() {stats.diagnostics.record(peer.direct_path().await?);}
    let first_frame=wait_first_frame(&stats,first_frame_baseline,Duration::from_secs(20));
    tokio::pin!(first_frame);
    let mut awaiting_first_frame=true;
    let switching={
    let mut control_active=true;
    let control=controls.run();tokio::pin!(control);
    loop {
        tokio::select! {
            result=&mut first_frame,if awaiting_first_frame=>{awaiting_first_frame=false;result?;},
            _=stats.native_displays.changed.notified()=>{if let Some(request)=stats.native_displays.take_switch(){break Some(request);}},
            result=&mut control,if control_active=>{
                control_active=false;
                if let Err(error)=result {tracing::warn!(%error,"native control unavailable; video continues");}
                stats.capabilities.enable(crate::capabilities::Capability::Input,false);
                stats.native_input_available.store(false,Ordering::Release);
                let _=timeout(Duration::from_secs(1),channel.close()).await;
            },
            _=stream.disconnected()=>{
                ensure!(stats.decoded_total.load(Ordering::Relaxed)>0,"native host ended before delivering video");
                break None;
            },
            state=peer.states.changed()=>{
                if state.is_err()||matches!(*peer.states.borrow(),RTCPeerConnectionState::Failed|RTCPeerConnectionState::Closed){
                    bail!("native host disconnected");
                }
            },
        }
    }
    };
    if let Some(request)=switching {controls.apply_display(request).await?;Ok(ViewOutcome::Switch(controls.take_input()))}else{Ok(ViewOutcome::Ended)}
}

/// Once transport is established a decoder failure must not leave an endless
/// connecting screen. A switch uses its own baseline, not earlier session frames.
async fn wait_first_frame(stats:&MediaStats,baseline:u64,limit:Duration)->Result<()> {
    timeout(limit,async {
        while stats.decoded_total.load(Ordering::Acquire)<=baseline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.context("连接已建立，但 20 秒内未解码出首帧；请检查两端编码支持或改用其它编码，诊断日志已记录协商编码和解码错误")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::frame_channel;
    #[tokio::test(flavor="multi_thread",worker_threads=2)]
    async fn capture_requests_initial_idr_without_adding_a_periodic_refresh() {
        struct Source {inner:Box<dyn uu_capture::Capture>,count:u32}
        impl uu_capture::Capture for Source {
            fn resolution(&self)->(u32,u32){self.inner.resolution()}
            fn next_frame(&mut self)->std::result::Result<uu_media::RawFrame,uu_capture::CaptureError> {
                if self.count==1 {std::thread::sleep(Duration::from_millis(2100));}
                self.count+=1;self.inner.next_frame()
            }
        }
        struct Encoder(bool);
        impl uu_media::VideoEncoder for Encoder {
            fn backend_name(&self)->&str{"owned reference fixture"}
            fn codec(&self)->VideoCodec{VideoCodec::H264}
            fn request_keyframe(&mut self){self.0=true;}
            fn set_bitrate(&mut self,_:u32)->std::result::Result<(),uu_media::CodecError>{self.0=true;Ok(())}
            fn encode(&mut self,frame:&uu_media::RawFrame)->std::result::Result<Vec<uu_media::EncodedFrame>,uu_media::CodecError> {
                Ok(vec![uu_media::EncodedFrame{data:vec![0u8].into(),pts:frame.pts,is_keyframe:std::mem::take(&mut self.0)}])
            }
        }
        let settings=uu_media::encoder::Settings{tuning:Default::default(),codec:VideoCodec::H264,
            rate_control:Default::default(),width:64,height:64,fps:60,bitrate_kbps:1000,preference:EncoderPreference::Software};
        let source=Box::new(Source{inner:uu_capture::open(&CaptureSource::Synthetic{width:64,height:64,fps:60}).unwrap(),count:0});
        let mut producer=CaptureWorker::start(source,settings,Arc::default(),Box::new(Encoder(false)),Arc::default());
        assert!(timeout(Duration::from_secs(2),producer.frames.recv()).await.unwrap().unwrap().unwrap().is_keyframe);
        assert!(!timeout(Duration::from_secs(4),producer.frames.recv()).await.unwrap().unwrap().unwrap().is_keyframe,
            "capture added a periodic refresh even though references were intact");
        producer.stop().await.unwrap();
    }
    #[tokio::test(flavor="multi_thread",worker_threads=2)]
    async fn suspended_capture_emits_no_old_pixels_resumes_with_idr_and_cancels() {
        struct Source {
            inner: Box<dyn uu_capture::Capture>,
            paused: Arc<AtomicBool>,
            suspended: Arc<AtomicU64>,
        }
        impl uu_capture::Capture for Source {
            fn resolution(&self)->(u32,u32){self.inner.resolution()}
            fn next_frame(&mut self)->std::result::Result<uu_media::frame::RawFrame,uu_capture::CaptureError> {
                if self.paused.load(Ordering::Acquire) {
                    self.suspended.fetch_add(1,Ordering::Release);
                    Err(uu_capture::CaptureError::Suspended("owned desktop transition".into()))
                } else {self.inner.next_frame()}
            }
        }
        let settings=uu_media::encoder::Settings{tuning:Default::default(),codec:VideoCodec::H264,
            rate_control:Default::default(),width:64,height:64,fps:60,bitrate_kbps:1000,preference:EncoderPreference::Software};
        let paused=Arc::new(AtomicBool::new(false));let suspended=Arc::new(AtomicU64::new(0));
        let capture=Box::new(Source {inner:uu_capture::open(&CaptureSource::Synthetic{width:64,height:64,fps:60}).unwrap(),paused:paused.clone(),suspended:suspended.clone()});
        let mut producer=CaptureWorker::start(capture,settings,Arc::default(),uu_media::encoder::create(settings).unwrap(),Arc::default());
        let _=timeout(Duration::from_secs(2),producer.frames.recv()).await.unwrap().unwrap().unwrap();
        paused.store(true,Ordering::Release);
        timeout(Duration::from_secs(2),async {
            while suspended.load(Ordering::Acquire)==0 {
                while producer.frames.try_recv().is_ok() {}
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.unwrap();
        while producer.frames.try_recv().is_ok() {}
        assert!(timeout(Duration::from_millis(80),producer.frames.recv()).await.is_err(),"no old desktop frame is repeated");
        paused.store(false,Ordering::Release);
        let frame=timeout(Duration::from_secs(2),producer.frames.recv()).await.unwrap().unwrap().unwrap();
        assert!(frame.is_keyframe);
        assert!(uu_media::software_decoder().unwrap().decode(&frame).unwrap().is_some());
        paused.store(true,Ordering::Release);
        timeout(Duration::from_millis(500),producer.stop()).await.unwrap().unwrap();
    }
    #[tokio::test(flavor="multi_thread",worker_threads=2)]
    async fn recovery_immediately_after_an_idr_is_encoded_in_the_next_capture_batch() {
        let settings=uu_media::encoder::Settings{tuning:Default::default(),codec:VideoCodec::H264,
            rate_control:Default::default(),width:64,height:64,fps:60,bitrate_kbps:1000,preference:EncoderPreference::Software};
        let capture=uu_capture::open(&CaptureSource::Synthetic{width:64,height:64,fps:60}).unwrap();
        let encoder=uu_media::encoder::create(settings).unwrap();
        let refresh=Arc::new(AtomicBool::new(false));
        let mut producer=CaptureWorker::start(capture,settings,refresh.clone(),encoder,Arc::default());
        let first=timeout(Duration::from_secs(2),producer.frames.recv()).await.unwrap().unwrap().unwrap();
        assert!(first.is_keyframe);
        refresh.store(true,Ordering::Release);
        // Allow the two already-buffered frames; a new request must reach the
        // next captured frame rather than wait for a 250 ms debounce window.
        let mut recovered=None;
        for _ in 0..3 {
            let frame=timeout(Duration::from_secs(2),producer.frames.recv()).await.unwrap().unwrap().unwrap();
            if frame.is_keyframe {recovered=Some(frame);break;}
        }
        producer.stop().await.unwrap();
        let recovered=recovered.expect("recovery feedback was delayed after a recent IDR");
        let mut decoder=uu_media::software_decoder().unwrap();
        assert!(decoder.decode(&recovered).unwrap().is_some(),"recovery is independently decodable");
    }
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
        let config = HostConfig {encoder_tuning:Default::default(),session:None,video_policy:None,codec:Default::default(),rate_control:Default::default(),
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
    async fn first_frame_deadline_uses_new_frames_and_can_be_cancelled() {
        let stats=MediaStats::default();
        stats.decoded_total.store(3,Ordering::Release);
        assert!(wait_first_frame(&stats,3,Duration::from_millis(5)).await.is_err());
        assert!(wait_first_frame(&stats,2,Duration::from_secs(1)).await.is_ok());
        let pending=wait_first_frame(&stats,3,Duration::from_secs(20));
        tokio::select! { _=pending=>panic!("old frames cannot satisfy a new connection"), _=tokio::task::yield_now()=>{} }
    }
    #[test]
    fn automatic_codec_order_prefers_av1_then_hevc_then_h264() {
        assert_eq!(
            ordered_codecs(&[VideoCodec::H264, VideoCodec::Av1, VideoCodec::Hevc]),
            vec![VideoCodec::Av1, VideoCodec::Hevc, VideoCodec::H264]
        );
    }
    #[test]
    fn codec_policy_distinguishes_host_auto_client_choice_and_host_override() {
        use VideoCodec::*;
        let all=[H264,Hevc,Av1];
        assert_eq!(codec_choices(&all,None,None).unwrap(),vec![Av1,Hevc,H264]);
        for codec in all {
            assert_eq!(codec_choices(&all,None,Some(codec)).unwrap(),vec![codec]);
            assert_eq!(codec_choices(&all,Some(codec),None).unwrap(),vec![codec]);
            for client in all {
                assert_eq!(codec_choices(&all,Some(codec),Some(client)).unwrap(),vec![codec]);
            }
        }
        assert!(codec_choices(&[H264],None,Some(Hevc)).is_err());
        assert!(codec_choices(&[H264],Some(Av1),None).is_err());
    }

    #[test]
    fn default_answer_preserves_the_legacy_strict_schema(){
        let legacy=serde_json::json!({"version":1,"sdp":{"type":"answer","sdp":"v=0\r\n"},"width":128,"height":128,"fps":60,"bitrate_kbps":2000,"view_only":true});
        let answer:Answer=serde_json::from_value(legacy.clone()).unwrap();assert_eq!(answer.codec,VideoCodec::H264);
        assert_eq!(serde_json::to_value(&answer).unwrap(),legacy);
        let mut extended=legacy;extended["codec"]=serde_json::json!("av1");let answer:Answer=serde_json::from_value(extended.clone()).unwrap();
        assert_eq!(answer.codec,VideoCodec::Av1);assert_eq!(serde_json::to_value(answer).unwrap(),extended);
    }
    #[tokio::test(flavor="multi_thread",worker_threads=2)]
    async fn client_quality_request_scales_encrypted_video_and_reports_real_host_limits() {
        let (mut config,file)=config();
        config.video_policy=Some(crate::native_quality::Policy{codecs:vec![VideoCodec::H264],preferred_codec:None});
        let (host_stop,rx)=watch::channel(false);let (phase,host_phase)=watch::channel(HostStatus::Starting);
        let host=tokio::spawn(serve_with_status(config,rx,Some(phase)));
        let invitation=invitation(&file).await;
        let stats=Arc::new(MediaStats::default());
        stats.native_codec_capabilities.set(vec![VideoCodec::H264]).unwrap();
        stats.native_quality_request.set(crate::native_quality::Request{fps:30,bounds:Some([160,90]),bitrate_kbps:Some(8000),codec:Some(VideoCodec::H264)}).unwrap();
        let observed=stats.clone();let (frames,mut receiver)=frame_channel();let (stop,rx)=watch::channel(false);
        let viewer=tokio::spawn(async move{view(&invitation,frames,observed,rx).await});
        let result=timeout(Duration::from_secs(8),async {
            while stats.decoded_total.load(Ordering::Acquire)<12 {
                while receiver.try_recv().is_ok(){}
                ensure!(!viewer.is_finished(),"negotiated viewer ended before frames");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            ensure!(stats.configured_fps.load(Ordering::Acquire)==30,"target FPS not negotiated");
            ensure!(stats.native_quality_response.lock().unwrap().as_ref().is_some_and(|r|r.codecs==vec![VideoCodec::H264]&&r.dynamic&&r.max_bitrate_kbps==crate::native_quality::MAX_BITRATE_KBPS&&r.max_fps==crate::native_quality::MAX_FPS),"missing real host capability response");
            ensure!(stats.capabilities.allows(crate::capabilities::Capability::Quality),"negotiated quality controls remain hidden");
            ensure!(matches!(*host_phase.borrow(),HostStatus::Streaming{width:160,height:90,fps:30,..}),"capture was not scaled");
            Ok::<_,anyhow::Error>(())
        }).await;
        stop.send_replace(true);host_stop.send_replace(true);
        let viewed=viewer.await.unwrap();let hosted=host.await.unwrap();
        result.unwrap_or_else(|e|panic!("{e}; viewer={viewed:?}; host={hosted:?}; phase={:?}; packets={}; decoded={}; errors={}",*host_phase.borrow(),stats.packets.load(Ordering::Relaxed),stats.decoded_total.load(Ordering::Relaxed),stats.errors.load(Ordering::Relaxed))).unwrap_or_else(|e|panic!("{e}; viewer={viewed:?}; host={hosted:?}"));viewed.unwrap();let report=hosted.unwrap();assert!(report.sent>=12);
        assert!(!stats.capabilities.allows(crate::capabilities::Capability::Quality));
    }
    #[tokio::test]
    async fn encrypted_control_reads_saves_and_acknowledges_host_restart() {
        use crate::{device_auth::PairingControl,device_trust::{Permissions,TrustStore},host_admin::{Request,Response,Service},shared_host::{Access,SessionContext,SessionGate}};
        let root=std::env::temp_dir().join(format!("eclipse-admin-stream-{}",uuid::Uuid::new_v4()));
        let trust=TrustStore::open(root.join("host")).unwrap();
        let client=TrustStore::open(root.join("client")).unwrap().card("Generated client");
        let mut details=Permissions::CONTROL.details();details.manage_server=true;
        let permissions=Permissions::CONTROL.with_details(details);
        trust.authorize(client.clone(),permissions,None).unwrap();
        let store=crate::device_book::Store::open(root.join("host")).unwrap();
        let (sender,_)=mpsc::unbounded_channel();
        let access=Arc::new(Access{store:trust,name:"Generated host".into(),pairing:Arc::new(PairingControl::new(sender)),sessions:Arc::new(SessionGate::default())});
        let restarted=Arc::new(AtomicBool::new(false));let invoked=restarted.clone();
        *access.sessions.administration.lock().unwrap()=Some(Arc::new(Service::new(store.clone(),Default::default(),move||{invoked.store(true,Ordering::Release);Ok(())})));
        let _lease=access.claim(&client.id()).unwrap();
        let (mut config,file)=config();
        config.session=Some(SessionContext{access,peer:client.id(),permissions});
        let (host_stop,rx)=watch::channel(false);let host=tokio::spawn(serve(config,rx));
        let invite=invitation(&file).await;
        let stats=Arc::new(MediaStats::default());let observed=stats.clone();
        let (frames,_images)=frame_channel();let (stop,rx)=watch::channel(false);
        let viewer=tokio::spawn(async move{view(&invite,frames,observed,rx).await});
        let result=timeout(Duration::from_secs(12),async {
            while !stats.host_admin.available.load(Ordering::Acquire)||stats.decoded_total.load(Ordering::Acquire)<3 {tokio::time::sleep(Duration::from_millis(10)).await;}
            let id=stats.host_admin.request(Request::Inspect)?;
            while stats.host_admin.response.lock().unwrap().as_ref().is_none_or(|(received,_)|*received!=id) {tokio::time::sleep(Duration::from_millis(10)).await;}
            let snapshot=stats.host_admin.snapshot.lock().unwrap().clone().context("missing host snapshot")?;
            ensure!(snapshot.connections.len()==1,"missing active connection");
            let server=snapshot.server.context("missing manager settings")?;
            let mut settings=server.saved;settings.audio=false;
            let id=stats.host_admin.request(Request::Save{revision:server.revision,settings,restart:true})?;
            while stats.host_admin.response.lock().unwrap().as_ref().is_none_or(|(received,_)|*received!=id) {tokio::time::sleep(Duration::from_millis(10)).await;}
            ensure!(matches!(stats.host_admin.response.lock().unwrap().as_ref(),Some((_,Response::Saved{restart:true}))),"save failed");
            while !restarted.load(Ordering::Acquire){tokio::time::sleep(Duration::from_millis(10)).await;}
            ensure!(!store.read()?.sharing.audio,"settings were not saved");
            Ok::<_,anyhow::Error>(())
        }).await;
        stop.send_replace(true);host_stop.send_replace(true);
        let viewed=viewer.await.unwrap();let hosted=host.await.unwrap();
        result.unwrap().unwrap();viewed.unwrap();hosted.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn client_changes_frame_rate_and_bitrate_on_one_encrypted_media_session() {
        let (mut config,file)=config();
        config.fps=10;config.bitrate_kbps=64;config.session_limit=Some(Duration::from_secs(20));
        config.video_policy=Some(crate::native_quality::Policy{codecs:vec![VideoCodec::H264],preferred_codec:None});
        let (host_stop,rx)=watch::channel(false);let (phase,host_phase)=watch::channel(HostStatus::Starting);
        let host=tokio::spawn(serve_with_status(config,rx,Some(phase)));
        let invite=invitation(&file).await;
        let stats=Arc::new(MediaStats::default());let observed=stats.clone();
        stats.native_codec_capabilities.set(vec![VideoCodec::H264]).unwrap();
        stats.native_quality_request.set(crate::native_quality::Request{fps:120,bounds:None,bitrate_kbps:Some(8000),codec:None}).unwrap();
        let (frames,_images)=frame_channel();let (stop,rx)=watch::channel(false);
        let (input,receive)=crate::native_input::channel(false);
        let viewer=tokio::spawn(async move{view_with_input(&invite,frames,observed,rx,receive).await});
        let result=timeout(Duration::from_secs(16),async {
            while stats.decoded_total.load(Ordering::Acquire)<8 {tokio::time::sleep(Duration::from_millis(5)).await;}
            ensure!(stats.configured_fps.load(Ordering::Acquire)==120,"old host FPS still caps the client");
            let epoch=stats.native_media_epoch.load(Ordering::Acquire);
            for (index,settings) in [crate::native_quality::Update{fps:24,bitrate_kbps:Some(500)},crate::native_quality::Update{fps:120,bitrate_kbps:Some(150_000)},crate::native_quality::Update{fps:240,bitrate_kbps:None}].into_iter().enumerate() {
                let request=index as u64+1;
                input.set_quality(request,settings)?;
                loop {
                    let reply=stats.native_quality_applied.lock().unwrap().clone();
                    if let Some(applied)=reply.filter(|a|a.request==request) {
                        ensure!(applied.error.is_none(),"runtime update failed: {:?}",applied.error);
                        ensure!(applied.bitrate_kbps==settings.bitrate(320,180),"wrong applied bitrate");break;
                    }
                    ensure!(!viewer.is_finished(),"viewer stopped during update");
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                let baseline=stats.decoded_total.load(Ordering::Acquire);
                while stats.decoded_total.load(Ordering::Acquire)<baseline+8 {tokio::time::sleep(Duration::from_millis(5)).await;}
                ensure!(stats.native_media_epoch.load(Ordering::Acquire)==epoch,"quality change reconnected media");
                ensure!(stats.configured_fps.load(Ordering::Acquire)==settings.fps,"target FPS not applied");
                ensure!(matches!(*host_phase.borrow(),HostStatus::Streaming{fps,..} if fps==settings.fps),"host status retains old FPS");
            }
            ensure!(stats.errors.load(Ordering::Acquire)==0,"decoder errors after reconfiguration");
            ensure!(input.set_quality(99,crate::native_quality::Update{fps:0,bitrate_kbps:None}).is_err(),"invalid update entered control queue");
            Ok::<_,anyhow::Error>(())
        }).await;
        stop.send_replace(true);host_stop.send_replace(true);
        let viewed=viewer.await.unwrap();let hosted=host.await.unwrap();
        result.unwrap().unwrap();viewed.unwrap();let report=hosted.unwrap();assert!(report.sent>=32);
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
        assert!(validate_video(8100, 1164, 60).is_ok());
        assert!(validate_video(8192, 8192, 30).is_err());
        assert!(validate_video(1920, 1080, 120).is_ok());
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
                source.remote.lock().unwrap().as_ref().and_then(crate::clipboard::Content::as_text) == Some("generated local 中文🙂")
            })
            .await?;
            wait_clipboard(|| {
                *source.local.lock().unwrap() = Some("generated remote text".into());
                stats.clipboard.remote.lock().unwrap().as_ref().and_then(crate::clipboard::Content::as_text) == Some("generated remote text")
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
                    && stats.native_timing.snapshot().host.is_some_and(|host|host.video.is_some())
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
        assert!(stats.rtt_updated_ms.load(Ordering::Relaxed)>0,"native RTT must stay visible in the HUD");
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
