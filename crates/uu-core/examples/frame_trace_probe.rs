//! Owned, generated loopback. No desktop capture, account, input or display changes.
use anyhow::{ensure, Context, Result};
use std::{
    sync::{atomic::Ordering, Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::watch;
use uu_core::{
    media::{frame_channel, MediaStats},
    native,
};
struct DeliveryGaps {
    published: u64,
    previous: Option<Instant>,
    samples: Vec<f64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 3,
        "usage: frame_trace_probe OUTPUT.json FPS FRAMES"
    );
    let output = std::path::PathBuf::from(&args[0]);
    let fps: u32 = args[1].parse()?;
    let target: u64 = args[2].parse()?;
    ensure!(
        (30..=240).contains(&fps) && (120..=2400).contains(&target),
        "requires 30..240 FPS and 120..2400 frames"
    );
    ensure!(
        target >= u64::from(fps) * 2,
        "record at least two seconds so negotiated telemetry can arrive"
    );
    ensure!(!output.exists(), "output must be a new private file");
    let root =
        std::env::temp_dir().join(format!("eclipse-generated-trace-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    }
    let invitation = root.join("owned.eclipse-invite.json");
    let config = native::HostConfig {
        bind: "127.0.0.1:0".parse()?,
        source: native::CaptureSource::Synthetic {
            width: 320,
            height: 180,
            fps,
        },
        invitation_file: invitation.clone(),
        invitation_lifetime: Duration::from_secs(30),
        fps,
        bitrate_kbps: 2000,
        encoder: native::EncoderPreference::Software,
        session_limit: Some(Duration::from_secs(100)),
        allow_input: false,
        audio: None,
        clipboard: None,
        encoder_tuning: Default::default(),
        session: None,
        codec: Default::default(),
        rate_control: Default::default(),
        video_policy: None,
    };
    let (host_stop, stop) = watch::channel(false);
    let host = tokio::spawn(native::serve(config, stop));
    let invite = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(value) = uu_core::native_pairing::Invitation::load(&invitation) {
                break value;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    let stats = Arc::new(MediaStats::default());
    let _ = stats
        .decoder_preference
        .set(uu_media::DecoderKind::Software);
    let (frames, mut images) = frame_channel();
    let gaps = Arc::new(Mutex::new(DeliveryGaps {
        published: 0,
        previous: None,
        samples: Vec::with_capacity(target as usize),
    }));
    let delivery = gaps.clone();
    images.set_waker(move || {
        let now = Instant::now();
        let mut gaps = delivery.lock().unwrap();
        gaps.published += 1;
        if gaps.published > u64::from(fps) && gaps.samples.len() < target as usize {
            if let Some(previous) = gaps.previous {
                gaps.samples
                    .push(now.duration_since(previous).as_secs_f64() * 1000.);
            }
        }
        gaps.previous = Some(now);
    });
    let (view_stop, stop) = watch::channel(false);
    let view_stats = stats.clone();
    let viewer = tokio::spawn(async move { native::view(&invite, frames, view_stats, stop).await });
    let started = Instant::now();
    let mut observed = 0u64;
    let mut steady = None;
    let result = tokio::time::timeout(Duration::from_secs(95), async {
        while stats.decoded_total.load(Ordering::Acquire) < target {
            let decoded = stats.decoded_total.load(Ordering::Acquire);
            if decoded >= u64::from(fps) && steady.is_none() {
                steady = Some((Instant::now(), decoded));
            }
            while images.try_recv_image().is_ok() {
                observed += 1;
            }
            ensure!(!viewer.is_finished(), "viewer ended early");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;
    let elapsed = started.elapsed();
    let steady_fps = steady.map(|(started, baseline)| {
        stats
            .decoded_total
            .load(Ordering::Acquire)
            .saturating_sub(baseline) as f64
            / started.elapsed().as_secs_f64()
    });
    view_stop.send_replace(true);
    host_stop.send_replace(true);
    let viewed = tokio::time::timeout(Duration::from_secs(5), viewer).await;
    let hosted = tokio::time::timeout(Duration::from_secs(5), host).await;
    let _ = std::fs::remove_dir_all(&root);
    result.context("generated loopback timed out")??;
    viewed???;
    let host = hosted???;
    let mut gaps = gaps.lock().unwrap();
    gaps.samples.sort_by(f64::total_cmp);
    let gap_quantiles = (!gaps.samples.is_empty()).then(|| {
        let n = gaps.samples.len();
        [
            gaps.samples[(n * 50).div_ceil(100) - 1],
            gaps.samples[(n * 95).div_ceil(100) - 1],
            gaps.samples[(n * 99).div_ceil(100) - 1],
            gaps.samples[n - 1],
        ]
    });
    ensure!(stats.errors.load(Ordering::Acquire) == 0, "decoder errors");
    let trace_on = std::env::var("ECLIPSE_FRAME_TRACE").as_deref() != Ok("0");
    ensure!(
        stats.native_frame_trace_supported.load(Ordering::Acquire) == trace_on,
        "trace negotiation mismatch"
    );
    let remote = stats.native_frame_trace.lock().unwrap().clone();
    if trace_on {
        let report = host.frame_trace.as_ref().context("host trace missing")?;
        ensure!(
            remote.is_some(),
            "negotiated trace did not reach the viewer"
        );
        ensure!(
            report.counts.new_source == host.captured
                && report.counts.unknown_source == 0
                && report.counts.repeated_source == 0,
            "generated source identity mismatch"
        );
        ensure!(
            report.counts.unmatched_outputs == 0 && report.counts.sent == host.sent,
            "AU-to-input matching failed"
        );
    } else {
        ensure!(
            host.frame_trace.is_none() && remote.is_none(),
            "disabled trace still collected data"
        );
    }
    let value = serde_json::json!({"generated_only":true,"trace_enabled":trace_on,"fps":fps,
        "decoded":stats.decoded_total.load(Ordering::Acquire),"observed":observed,"elapsed_ms":elapsed.as_secs_f64()*1000.,
        "captured":host.captured,"sent":host.sent,"steady_fps":steady_fps,
        "decoded_delivery_gap_ms_p50_p95_p99_max":gap_quantiles,"published":gaps.published,
        "decoded_display_skips":stats.decoded_display_skips.load(Ordering::Acquire),
        "mean_capture_us":host.capture_us as f64/host.captured.max(1) as f64,
        "mean_encode_us":host.encode_us as f64/host.captured.max(1) as f64,
        "mean_backpressure_us":host.backpressure_us as f64/host.captured.max(1) as f64,
        "decoder_backend":stats.backend(),
        "trace":host.frame_trace,"final":{"host_frame_trace":remote}});
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    let mut file = options.open(&output)?;
    file.write_all(serde_json::to_string_pretty(&value)?.as_bytes())?;
    println!(
        "generated loopback: trace={trace_on}, decoded={}, observed={observed}, elapsed_ms={:.1}",
        stats.decoded_total.load(Ordering::Acquire),
        elapsed.as_secs_f64() * 1000.
    );
    Ok(())
}
