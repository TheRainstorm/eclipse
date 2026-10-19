//! Native LAN viewing prototype: private TLS pairing, WebRTC DTLS-SRTP, one source.
//! No vendor signaling or production credential is used. Input needs an
//! explicit host permission and a separate authenticated control request.
use crate::{
    media::{FrameSender, MediaStats},
    native_media::NativePeer,
    native_pairing::{HostCredentials, Invitation},
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

async fn read_message<T: DeserializeOwned>(stream: &mut (impl AsyncRead + Unpin)) -> Result<T> {
    timeout(Duration::from_secs(15), async {
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
async fn write_message<T: Serialize>(
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
    pub session_limit: Option<Duration>,
    pub allow_input: bool,
}
#[derive(Debug, Default)]
pub struct HostReport {
    pub captured: u64,
    pub sent: u64,
    pub bytes: u64,
    pub input_events: u64,
    pub input_errors: u64,
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
pub async fn serve(config: HostConfig, mut stop: watch::Receiver<bool>) -> Result<HostReport> {
    ensure!(
        !config.bind.ip().is_unspecified() && !config.bind.ip().is_multicast(),
        "choose a concrete native host interface"
    );
    ensure!(
        (1..=60).contains(&config.fps) && (64..=100_000).contains(&config.bitrate_kbps),
        "software native host needs 1..=60 fps and 64..=100000 kbps"
    );
    ensure!(!config.allow_input || matches!(config.source,uu_capture::CaptureSource::Primary|uu_capture::CaptureSource::Monitor(_)),
        "desktop input can only be enabled for a real whole-monitor source");
    let listener = TcpListener::bind(config.bind)
        .await
        .context("cannot bind native signaling listener")?;
    let address = listener.local_addr()?;
    let credentials = HostCredentials::new(address, config.invitation_lifetime)?;
    credentials.invitation().save_new(&config.invitation_file)?;
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
    host_session(socket, config, stop).await
}

struct CaptureWorker {
    stopped: Arc<AtomicBool>,
    captured: Arc<AtomicU64>,
    frames: mpsc::Receiver<Result<uu_media::EncodedFrame>>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl CaptureWorker {
    fn start(
        capture: Box<dyn uu_capture::Capture>,
        fps: u32,
        kbps: u32,
        refresh: Arc<AtomicBool>,
    ) -> Self {
        let stopped = Arc::new(AtomicBool::new(false));
        let captured = Arc::new(AtomicU64::new(0));
        let (tx, frames) = mpsc::channel(2);
        let worker_stop = stopped.clone();
        let counter = captured.clone();
        let task = tokio::task::spawn_blocking(move || {
            let result = encode_source(capture, fps, kbps, refresh, worker_stop, counter, &tx);
            if let Err(error) = result {
                let _ = tx.blocking_send(Err(error));
            }
        });
        Self {
            stopped,
            captured,
            frames,
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
    kbps: u32,
    refresh: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    captured: Arc<AtomicU64>,
    output: &mpsc::Sender<Result<uu_media::EncodedFrame>>,
) -> Result<()> {
    use uu_media::VideoEncoder;
    let mut encoder = uu_media::H264Encoder::with_frame_rate(kbps, fps)?;
    let interval = Duration::from_secs_f64(1.0 / fps as f64);
    let mut next = Instant::now();
    let mut keyframe = Instant::now() - Duration::from_secs(3);
    while !stopped.load(Ordering::Acquire) {
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
        if stopped.load(Ordering::Acquire) {
            break;
        }
        let frame = capture.next_frame()?;
        captured.fetch_add(1, Ordering::Relaxed);
        let requested = refresh.load(Ordering::Acquire);
        if keyframe.elapsed() >= Duration::from_secs(2)
            || (requested && keyframe.elapsed() >= Duration::from_millis(250))
        {
            refresh.store(false, Ordering::Release);
            encoder.request_keyframe();
            keyframe = Instant::now();
        }
        for encoded in encoder.encode(&frame)? {
            if output.blocking_send(Ok(encoded)).is_err() {
                return Ok(());
            }
        }
        next = (next + interval).max(Instant::now());
    }
    Ok(())
}

async fn host_session(
    mut stream: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    config: HostConfig,
    mut stop: watch::Receiver<bool>,
) -> Result<HostReport> {
    let offer: Offer = tokio::select! {result=read_message(&mut stream)=>result?,_=cancelled(&mut stop)=>return Ok(HostReport::default())};
    ensure!(offer.version == 1, "unsupported native protocol version");
    let local_ip = stream.get_ref().0.local_addr()?.ip();
    let mut peer = NativePeer::new(local_ip, None, true).await?;
    let result = send_video(&mut peer, &mut stream, config, stop, offer).await;
    peer.close().await;
    let _ = stream.shutdown().await;
    result
}

async fn send_video(
    peer: &mut NativePeer,
    stream: &mut tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    config: HostConfig,
    mut stop: watch::Receiver<bool>,
    offer: Offer,
) -> Result<HostReport> {
    let refresh = Arc::new(AtomicBool::new(false));
    let track = peer.sender(refresh.clone()).await?;
    peer.remote(offer.sdp, stream.get_ref().0.peer_addr()?.ip())
        .await?;
    // Authentication and a valid media offer precede capture access.
    let capture = tokio::select! {
        result=tokio::task::spawn_blocking(move || uu_capture::open(&config.source))=>result??,
        _=cancelled(&mut stop)=>return Ok(HostReport::default()),
    };
    let (width, height) = capture.resolution();
    validate_video(width, height, config.fps)?;
    let controls=tokio::select! {
        controls=crate::native_control::HostControl::new(config.allow_input,capture.desktop_rect())=>controls,
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
    write_message(stream, &answer).await?;
    let connected=tokio::select! {result=peer.connected()=>Some(result),_=cancelled(&mut stop)=>None};
    match connected {
        Some(Ok(()))=>{},
        Some(Err(error))=>{controls.stop().await;return Err(error);},
        None=>{controls.stop().await;return Ok(HostReport::default());},
    }
    let mut producer = CaptureWorker::start(capture, config.fps, config.bitrate_kbps, refresh);
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
                _ = stream.read_u8() => break,
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
    (report.input_events,report.input_errors)=controls.counters();
    peer.close().await;
    let stopped = producer.stop().await;
    report.captured = producer.captured.load(Ordering::Relaxed);
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
    stats.capabilities.set(0);
    stats.native_input_available.store(false,Ordering::Release);
    let mut stream = tokio::select! {
        result=crate::native_pairing::connect(invitation)=>result?,
        _=cancelled(&mut stop)=>return Ok(()),
    };
    let local_ip = stream.get_ref().0.local_addr()?.ip();
    let mut peer = NativePeer::new(local_ip, Some(stats.clone()), false).await?;
    let result = tokio::select! {
        result=receive_session(&mut peer,&mut stream,frames,stats.clone(),input)=>result,
        _=cancelled(&mut stop)=>Ok(()),
    };
    peer.close().await;
    stats.control_open.store(0, Ordering::Release);
    let _ = stream.shutdown().await;
    result
}

async fn receive_session(
    peer: &mut NativePeer,
    stream: &mut tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    frames: FrameSender,
    stats: Arc<MediaStats>,
    input:Option<crate::native_input::NativeInputReceiver>,
) -> Result<()> {
    peer.receiver(frames, stats.clone()).await?;
    let channel = peer
        .pc
        .create_data_channel("eclipse-control-v1", None)
        .await?;
    let mut controls=crate::native_control::ViewerControl::new(channel.clone(),stats.clone(),input);
    write_message(
        stream,
        &Offer {
            version: 1,
            sdp: peer.description(true).await?,
        },
    )
    .await?;
    let answer: Answer = read_message(stream).await?;
    ensure!(
        answer.version == 1 && answer.view_only,
        "unsupported native session capabilities"
    );
    validate_video(answer.width, answer.height, answer.fps)?;
    // Complete dimension-dependent driver work before setting remote SDP can
    // start ICE/DTLS and release the first burst of video into the AU queue.
    peer.prepare_video(answer.width, answer.height, &stats).await?;
    stats.configured_fps.store(answer.fps,Ordering::Release);
    peer.remote(answer.sdp, stream.get_ref().0.peer_addr()?.ip())
        .await?;
    peer.connected().await?;
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
            _=stream.read_u8()=>{
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
            session_limit: Some(Duration::from_secs(10)),
            allow_input:false,
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
