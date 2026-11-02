//! One owned, read-only local stream through production TLS and UDP media.
//! The short-lived invitation stays in memory; device trust and installed hosts
//! are unrelated to this session. Only enumeration is adapted for old ICE.
use crate::{
    media::{FrameSender, MediaStats},
    native::{self, SharingConfig},
    native_input::NativeInputReceiver,
    native_pairing::HostCredentials,
};
use anyhow::{Context, Result};
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;

fn read_only(mut config: SharingConfig) -> SharingConfig {
    // A self test must not reinject its own input, audio or clipboard. Keep the
    // input receiver for quality/display commands, whose permissions are separate.
    config.allow_input = false;
    config.audio = None;
    config.clipboard = None;
    config.session = None;
    config
}

async fn cancelled(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow_and_update() { return; }
        if stop.changed().await.is_err() { return; }
    }
}

/// Stream the explicitly chosen local source, using genuine encode/decode and
/// presentation consumers. Both signaling and every media candidate use loopback.
/// The future owns both peers and joins their shutdown before returning.
pub async fn view(
    config: SharingConfig,
    frames: FrameSender,
    stats: Arc<MediaStats>,
    mut stop: watch::Receiver<bool>,
    input: NativeInputReceiver,
) -> Result<()> {
    if *stop.borrow() { return Ok(()); }
    let config = read_only(config);
    config.validate()?;
    let source_kind = match &config.source {
        native::CaptureSource::Primary | native::CaptureSource::Monitor(_) | native::CaptureSource::Desktop { .. } => "desktop",
        native::CaptureSource::Window(_) => "window",
        native::CaptureSource::Synthetic { .. } => "synthetic",
    };
    stats.diagnostics.record(serde_json::json!({"event":"local_loopback_started",
        "pid":std::process::id(),"source_kind":source_kind,"input":false,"audio":false,"clipboard":false}));
    let listener = crate::listen_socket::tcp(([127, 0, 0, 1], 0).into())
        .context("cannot bind local streaming test")?;
    let credentials = HostCredentials::new(listener.local_addr()?, Duration::from_secs(30))?;
    let invitation = credentials.invitation().clone();
    let (owned_stop, owned_rx) = watch::channel(false);
    let mut host_stop = owned_rx.clone();
    let host_stats = stats.clone();
    let host = async move {
      let result = async {
        let stream = tokio::select! {
            _ = cancelled(&mut host_stop) => return Ok(native::HostReport::default()),
            result = async {
                loop {
                    let (socket, peer) = listener.accept().await?;
                    anyhow::ensure!(peer.ip().is_loopback(), "local test accepted a non-loopback peer");
                    match credentials.accept(socket).await {
                        Ok(stream) => break Ok::<_, anyhow::Error>(stream),
                        Err(_) => continue,
                    }
                }
            } => result?,
        };
        drop(listener);
        native::host_session(stream, config, host_stop, None, Arc::default()).await
      }.await;
      match &result {
        Ok(report) => host_stats.diagnostics.record(serde_json::json!({"event":"local_loopback_host_stopped",
            "success":true,"captured":report.captured,"sent":report.sent,"input_events":report.input_events,
            "input_errors":report.input_errors,"audio_packets":report.audio_packets,"encoder":report.encoder})),
        Err(_) => host_stats.diagnostics.record(serde_json::json!({"event":"local_loopback_host_stopped","success":false})),
      }
      result
    };
    let viewer = native::view_with_input(&invitation, frames, stats, owned_rx, input);
    tokio::pin!(host, viewer);
    enum First { Host(Result<native::HostReport>), Viewer(Result<()>), Cancelled }
    let first = tokio::select! {
        biased;
        _ = cancelled(&mut stop) => First::Cancelled,
        result = &mut host => First::Host(result),
        result = &mut viewer => First::Viewer(result),
    };
    owned_stop.send_replace(true);
    // No detached host task: closing the client also retires its capture worker,
    // sockets and encoder. Prefer the host's original preparation error over the
    // viewer's secondary EOF, while preserving independent viewer failures.
    match first {
        First::Host(host_result) => {
            let viewer_result = viewer.await;
            host_result.context("local test host failed")?;
            viewer_result.context("local test receiver failed")
        }
        First::Viewer(viewer_result) => {
            let host_result = host.await;
            host_result.context("local test host failed")?;
            viewer_result.context("local test receiver failed")
        }
        First::Cancelled => {
            let (host_result, viewer_result) = tokio::join!(host, viewer);
            host_result.context("local test host shutdown failed")?;
            viewer_result.context("local test receiver shutdown failed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{capabilities::Capability, native::{AudioSource, CaptureSource, EncoderPreference, VideoCodec}};
    use std::sync::atomic::Ordering;

    fn config() -> SharingConfig {
        SharingConfig {
            encoder_tuning: Default::default(), session: None, video_policy: None,
            codec: VideoCodec::H264, rate_control: Default::default(),
            source: CaptureSource::Synthetic { width: 320, height: 180, fps: 60 },
            fps: 60, bitrate_kbps: 1000, encoder: EncoderPreference::Software,
            session_limit: None, allow_input: true, audio: Some(AudioSource::Synthetic),
            clipboard: Some(Arc::default()),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn generated_local_stream_is_read_only_and_releases_both_loopback_sockets() {
        let stats = Arc::new(MediaStats::default());
        let diagnostics = stats.diagnostics.capture().unwrap();
        let _ = stats.decoder_preference.set(crate::media::DecoderKind::Software);
        stats.audio.output_enabled.store(false, Ordering::Release);
        let (frames, mut images) = crate::media::frame_channel();
        let (input, input_rx) = crate::native_input::channel(true);
        let (stop, stop_rx) = watch::channel(false);
        let view_stats = stats.clone();
        let viewing = tokio::spawn(view(config(), frames, view_stats, stop_rx, input_rx));
        let path = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                while images.try_recv().is_ok() {}
                assert!(!viewing.is_finished(), "local test exited before decoding");
                if stats.decoded_total.load(Ordering::Acquire) >= 12 {
                    if let Some(path) = stats.connection.request().selected { break path; }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        assert!(path.local.address.ip().is_loopback() && path.remote.address.ip().is_loopback());
        assert!(!path.relayed());
        assert_eq!(stats.errors.load(Ordering::Acquire), 0);
        for capability in [Capability::Input, Capability::Audio, Capability::Clipboard] {
            assert!(!stats.capabilities.allows(capability));
        }
        assert!(!stats.native_input_available.load(Ordering::Acquire));
        #[cfg(target_os="linux")]
        let socket_owners = [path.local.address, path.remote.address].map(|address| {
            let observer = crate::native_media::socket_lifetime_test::observe(address)
                .expect("live selected native socket has no identity observer");
            observer
        });
        // The caller requested input and supplied shared clipboard state and
        // audio source above; transport capabilities still cannot enable them.
        drop(input);
        stop.send_replace(true);
        tokio::time::timeout(Duration::from_secs(8), viewing).await.unwrap().unwrap().unwrap();
        assert!(stats.native_media_retired.load(Ordering::Acquire));
        let events: Vec<_> = diagnostics.try_iter().collect();
        let started = events.iter().find(|event| event["event"] == "local_loopback_started").unwrap();
        assert_eq!(started["source_kind"], "synthetic");
        assert_eq!(started["input"], false);
        let stopped = events.iter().find(|event| event["event"] == "local_loopback_host_stopped").unwrap();
        assert_eq!(stopped["success"], true);
        assert_eq!(stopped["input_events"], 0);
        assert_eq!(stopped["audio_packets"], 0);
        assert!(stopped["sent"].as_u64().unwrap() >= 12);
        // Verify this original resource, not whether its released ephemeral
        // address has subsequently been claimed by a different parallel test.
        #[cfg(target_os="linux")]
        for observer in &socket_owners {
            tokio::time::timeout(Duration::from_secs(2), async {
                while !observer.retired() { tokio::time::sleep(Duration::from_millis(10)).await; }
            }).await.unwrap_or_else(|_| panic!("original loopback media socket did not retire: {}",observer.failure()));
        }
        #[cfg(not(target_os="linux"))]
        for address in [path.local.address, path.remote.address] {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            loop {
                match std::net::UdpSocket::bind(address) {
                    Ok(socket) => { drop(socket); break; }
                    Err(_) if tokio::time::Instant::now() < deadline => tokio::time::sleep(Duration::from_millis(10)).await,
                    Err(error) => panic!("loopback media socket did not retire: {error}"),
                }
            }
        }
    }

    #[tokio::test]
    async fn already_cancelled_local_test_never_opens_capture_or_media() {
        let stats = Arc::new(MediaStats::default());
        let (frames, _) = crate::media::frame_channel();
        let (_input, input_rx) = crate::native_input::channel(true);
        let (_stop, stop_rx) = watch::channel(true);
        view(config(), frames, stats.clone(), stop_rx, input_rx).await.unwrap();
        assert_eq!(stats.decoded_total.load(Ordering::Acquire), 0);
        assert!(stats.connection.snapshot().selected.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_test_propagates_source_failure_and_releases_its_receiver() {
        let stats = Arc::new(MediaStats::default());
        let _ = stats.decoder_preference.set(crate::media::DecoderKind::Software);
        let (frames, _) = crate::media::frame_channel();
        let (_input, input_rx) = crate::native_input::channel(false);
        let (_stop, stop_rx) = watch::channel(false);
        let mut config = config();
        config.source = CaptureSource::Synthetic { width: 1, height: 1, fps: 60 };
        let result = tokio::time::timeout(Duration::from_secs(12), view(config, frames, stats.clone(), stop_rx, input_rx)).await.unwrap();
        assert!(result.is_err());
        assert!(stats.native_media_retired.load(Ordering::Acquire));
    }
}
