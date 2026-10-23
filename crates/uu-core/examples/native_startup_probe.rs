//! Measure first-frame preparation on the owned generated portal fixture.
use anyhow::{ensure, Context, Result};
use std::{
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};
use tokio::sync::watch;
use uu_core::{
    media::{frame_channel, MediaStats},
    native, native_quality,
};

#[tokio::main]
async fn main() -> Result<()> {
    let runtime = std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR")?);
    let owned_compositor=std::env::var("UU_OWNED_WAYLAND_TEST").as_deref()==Ok("1")
        &&runtime.file_name().is_some_and(|name|name.to_string_lossy().starts_with("uu-wayland-input-"));
    ensure!(
        std::env::var("ECLIPSE_PORTAL_FIXTURE").as_deref() == Ok("1")
            && (std::env::var("WAYLAND_DISPLAY").as_deref() == Ok("owned-fixture")||owned_compositor),
        "requires tools/native/portal_smoke.py --case stream-startup"
    );
    let expected=std::env::var("ECLIPSE_STARTUP_EXPECTED_SIZE").unwrap_or_else(|_|"320x240".into());
    let (width,height)=expected.split_once('x').context("expected size must be WxH")?;
    let expected=(width.parse::<u32>()?,height.parse::<u32>()?);
    ensure!((2..=8192).contains(&expected.0)&&(2..=8192).contains(&expected.1),"invalid fixture size");
    for round in 0..2 {
        let started = Instant::now();
        let invitation_path = runtime.join(format!("startup-{round}.eclipse-invite.json"));
        let config = native::HostConfig {
            bind: "127.0.0.1:0".parse()?,
            source: native::CaptureSource::Primary,
            invitation_file: invitation_path.clone(),
            invitation_lifetime: Duration::from_secs(60),
            fps: 30,
            bitrate_kbps: 2000,
            encoder: native::EncoderPreference::Auto,
            session_limit: Some(Duration::from_secs(40)),
            allow_input: true,
            audio: None,
            clipboard: None,
            encoder_tuning: Default::default(),
            session: None,
            codec: Default::default(),
            rate_control: Default::default(),
            video_policy: Some(native_quality::Policy {
                codecs: vec![
                    native::VideoCodec::H264,
                    native::VideoCodec::Hevc,
                    native::VideoCodec::Av1,
                ],
                preferred_codec: None,
            }),
        };
        let (host_stop, stop) = watch::channel(false);
        let host = tokio::spawn(native::serve(config, stop));
        let invitation = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(value) = uu_core::native_pairing::Invitation::load(&invitation_path) {
                    break value;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        let stats = Arc::new(MediaStats::default());
        stats.audio.output_enabled.store(false, Ordering::Release);
        let diagnostics = runtime.join(format!("startup-diagnostics-{round}"));
        stats.diagnostics.start([diagnostics.clone()]);
        stats
            .native_quality_request
            .set(native_quality::Request {
                fps: 30,
                bounds: None,
                bitrate_kbps: Some(2000),
                codec: None,
            })
            .unwrap();
        let (frames, mut images) = frame_channel();
        let (viewer_stop, stop) = watch::channel(false);
        let view_stats = stats.clone();
        let viewer =
            tokio::spawn(async move { native::view(&invitation, frames, view_stats, stop).await });
        let mut first_frame_ms = None;
        let received = tokio::time::timeout(Duration::from_secs(35), async {
            let mut count = 0;
            while count < 60 {
                while let Ok((_, width, height, _)) = images.try_recv() {
                    ensure!(
                        (width, height) == expected,
                        "unexpected generated frame geometry"
                    );
                    first_frame_ms.get_or_insert_with(|| started.elapsed().as_millis());
                    count += 1;
                }
                ensure!(
                    !viewer.is_finished(),
                    "viewer ended before generated frames"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Ok::<_, anyhow::Error>(count)
        })
        .await
        .context("generated first-frame probe timed out");
        viewer_stop.send_replace(true);
        host_stop.send_replace(true);
        let viewed = tokio::time::timeout(Duration::from_secs(4), viewer).await??;
        let hosted = tokio::time::timeout(Duration::from_secs(4), host).await??;
        // Preserve the underlying portal/negotiation error if the viewer ends
        // before a frame. The frame-count guard alone hides the useful cause.
        viewed.context("owned portal viewer failed")?;
        hosted.context("owned portal host failed")?;
        let frames = received??;
        ensure!(stats.errors.load(Ordering::Relaxed) == 0, "decode errors");
        // The diagnostic writer is bounded and asynchronous. Wait for its final
        // negotiation metadata; only numeric phase events enter this report.
        let phases = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let mut values = Vec::new();
                for entry in std::fs::read_dir(&diagnostics)?.flatten() {
                    let text = std::fs::read_to_string(entry.path())?;
                    for line in text.lines() {
                        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                            continue;
                        };
                        if matches!(
                            value["event"].as_str(),
                            Some(
                                "native_local_ice_gather"
                                    | "native_answer_wait"
                                    | "native_host_preparation"
                            )
                        ) {
                            values.push(value);
                        }
                    }
                }
                if values
                    .iter()
                    .any(|v| v["event"] == "native_host_preparation")
                {
                    break Ok::<_, anyhow::Error>(values);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        println!(
            "{}",
            serde_json::json!({"ok":true,"round":round,"frames":frames,"errors":0,
            "first_frame_ms":first_frame_ms,"phases":phases,"generated_video":true,"no_user_desktop":true})
        );
    }
    Ok(())
}
