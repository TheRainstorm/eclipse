use anyhow::{ensure, Context, Result};
use std::{
    path::PathBuf,
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};
use tokio::sync::watch;
use uu_core::media::MediaStats;

pub async fn host(config: uu_core::native::HostConfig) -> Result<()> {
    let (stop, rx) = watch::channel(false);
    let interrupt_stop = stop.clone();
    let interrupt = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = interrupt_stop.send(true);
        }
    });
    let result = uu_core::native::serve(config, rx).await;
    interrupt.abort();
    drop(stop);
    let report = result?;
    println!(
        "native host ended: captured={} sent={} bytes={} input={} input_errors={} audio={} audio_errors={} audio_drops={} producer_ms={} capture_us={} encode_us={} backpressure_us={} encoder={:?}",
        report.captured, report.sent, report.bytes, report.input_events, report.input_errors,report.audio_packets,report.audio_errors,report.audio_queue_drops,report.producer_ms,report.capture_us,report.encode_us,report.backpressure_us,report.encoder
    );
    if let Some(report)=report.frame_trace {
        println!("{}",serde_json::json!({"event":"host_frame_trace","report":report}));
    }
    Ok(())
}

pub async fn probe(
    path: PathBuf,
    frames: u64,
    seconds: u64,
    report: Option<PathBuf>,
    audio_loss_every: u32,
    progress_seconds: Option<u64>,
    max_frame_gap_seconds: Option<u64>,
    codec:Option<uu_core::native::VideoCodec>,
) -> Result<()> {
    let invitation = uu_core::native_pairing::Invitation::load(&path)?;
    probe_selected(invitation, frames, seconds, report, audio_loss_every, progress_seconds, max_frame_gap_seconds,codec).await
}

pub async fn probe_invitation(
    invitation: uu_core::native_pairing::Invitation,
    frames:u64, seconds:u64, report:Option<PathBuf>, audio_loss_every:u32,
    progress_seconds:Option<u64>, max_frame_gap_seconds:Option<u64>,
)->Result<()> {
    probe_selected(invitation,frames,seconds,report,audio_loss_every,progress_seconds,max_frame_gap_seconds,None).await
}
async fn probe_selected(
    invitation:uu_core::native_pairing::Invitation,
    frames:u64,seconds:u64,report:Option<PathBuf>,audio_loss_every:u32,
    progress_seconds:Option<u64>,max_frame_gap_seconds:Option<u64>,codec:Option<uu_core::native::VideoCodec>,
)->Result<()> {
    ensure!(
        (1..=100_000).contains(&frames) && (1..=3600).contains(&seconds),
        "native probe requires 1..=100000 frames and 1..=3600 seconds"
    );
    let mut report = report
        .map(|path| {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options
                .open(path)
                .context("probe report must be a new private file")
        })
        .transpose()?;
    let stats = Arc::new(uu_core::media::MediaStats::default());
    if let Some(codec)=codec {let _=stats.native_quality_request.set(uu_core::native_quality::Request{fps:60,bounds:None,bitrate_kbps:None,codec:Some(codec)});}
    let diagnostics = report.as_ref().and_then(|_|stats.diagnostics.capture());
    stats.audio.output_enabled.store(false, Ordering::Release);
    stats
        .audio
        .test_loss_every
        .store(audio_loss_every, Ordering::Release);
    let worker_stats = stats.clone();
    let (images, mut receiver) = uu_core::media::frame_channel();
    let (stop, rx) = watch::channel(false);
    let worker =
        tokio::spawn(
            async move { uu_core::native::view(&invitation, images, worker_stats, rx).await },
        );
    let started = Instant::now();
    let mut rendered = 0u64;
    let mut first_frame_ms = None;
    let mut last_frame = None;
    let mut maximum_gap = Duration::ZERO;
    let mut next_sample = Duration::ZERO;
    let mut next_progress=Duration::ZERO;
    let mut samples = Vec::new();
    let observation = tokio::time::timeout(Duration::from_secs(seconds), async {
        loop {
            let mut observed = false;
            while receiver.try_recv().is_ok() {
                rendered += 1;
                observed = true;
            }
            let now = Instant::now();
            if observed {
                first_frame_ms.get_or_insert(started.elapsed().as_millis() as u64);
                if let Some(previous) = last_frame {
                    maximum_gap = maximum_gap.max(now.duration_since(previous));
                }
                last_frame = Some(now);
            }
            if report.is_some() && started.elapsed() >= next_sample && samples.len() < 3601 {
                samples.push(snapshot(&stats, started.elapsed()));
                next_sample = started.elapsed() + Duration::from_secs(1);
            }
            if let Some(period)=progress_seconds {
                if started.elapsed()>=next_progress {
                    println!("native progress: {}",snapshot(&stats,started.elapsed()));
                    next_progress=started.elapsed()+Duration::from_secs(period);
                }
            }
            if let Some(limit)=max_frame_gap_seconds {
                ensure!(!last_frame.is_some_and(|last|now.duration_since(last)>=Duration::from_secs(limit)),
                    "native video stopped producing visible frames for {limit} seconds");
            }
            if stats.decoded_total.load(Ordering::Relaxed) >= frames && rendered > 0 {
                break;
            }
            ensure!(
                !worker.is_finished(),
                "native connection ended before the requested frames arrived"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("native video probe timed out");
    let elapsed = started.elapsed();
    if let Some(previous) = last_frame {
        maximum_gap = maximum_gap.max(previous.elapsed());
    }
    let _ = stop.send(true);
    let closed = worker
        .await
        .context("native viewing task failed")
        .and_then(|result| result);
    let completed = closed.is_ok()
        && matches!(&observation, Ok(Ok(())))
        && stats.errors.load(Ordering::Relaxed) == 0;
    let (w, h) = stats.res();
    if let Some(file) = &mut report {
        samples.push(snapshot(&stats, elapsed));
        serde_json::to_writer_pretty(
            &mut *file,
            &serde_json::json!({
                "version":1,"completed":completed,"elapsed_ms":elapsed.as_millis() as u64,
                "observed":rendered,"first_frame_ms":first_frame_ms,
                "maximum_frame_gap_ms":maximum_gap.as_secs_f64()*1000.0,
                "final":snapshot(&stats,elapsed),"samples":samples,
                "diagnostics":diagnostics.as_ref().map(|events|events.try_iter().collect::<Vec<_>>()),
            }),
        )?;
        file.sync_all()?;
    }
    println!("native probe: decoded={} observed={} resolution={}x{} elapsed_ms={:.0} errors={} backend={}",stats.decoded_total.load(Ordering::Relaxed),rendered,w,h,elapsed.as_secs_f64()*1000.0,stats.errors.load(Ordering::Relaxed),stats.backend());
    closed?;
    observation??;
    ensure!(
        stats.errors.load(Ordering::Relaxed) == 0,
        "native decoder reported errors"
    );
    Ok(())
}

fn snapshot(stats: &MediaStats, elapsed: Duration) -> serde_json::Value {
    let (width, height) = stats.res();
    let mut value=serde_json::json!({
        "audio_packets":stats.audio.packets.load(Ordering::Relaxed),"audio_samples":stats.audio.decoded_samples.load(Ordering::Relaxed),
        "audio_recovered":stats.audio.recovered.load(Ordering::Relaxed),"audio_concealed":stats.audio.concealed.load(Ordering::Relaxed),
        "audio_injected_loss":stats.audio.injected_loss.load(Ordering::Relaxed),"audio_level":stats.audio.level_x10000.load(Ordering::Relaxed) as f64/10000.0,
        "at_ms":elapsed.as_millis() as u64,"width":width,"height":height,
        "native_timing":stats.native_timing.snapshot(),
        "host_frame_trace":stats.native_frame_trace.lock().unwrap().clone(),
        "backend":stats.backend(),"codec":stats.codec.lock().unwrap().clone(),"decoded":stats.decoded_total.load(Ordering::Relaxed),
        "rx_fps":stats.src_fps(),"decode_fps":stats.dec_fps(),"decode_ms":stats.decode_ms(),
        "bitrate_kbps":stats.bitrate_kbps.load(Ordering::Relaxed),
        "rtt_us":stats.rtt_us.load(Ordering::Relaxed),"rtt_deviation_us":stats.rtt_deviation_us.load(Ordering::Relaxed),
        "loss_ppm":stats.loss_ppm.load(Ordering::Relaxed),"errors":stats.errors.load(Ordering::Relaxed),
        "packets":stats.packets.load(Ordering::Relaxed),"lost":stats.lost.load(Ordering::Relaxed),
        "nack_requests":stats.nack_requests.load(Ordering::Relaxed),"rtx_packets":stats.rtx_packets.load(Ordering::Relaxed),
        "fec":{"enabled":stats.fec_enabled.load(Ordering::Relaxed),"packets":stats.fec_packets.load(Ordering::Relaxed),"bytes":stats.fec_bytes.load(Ordering::Relaxed),"recovered":stats.fec_recovered.load(Ordering::Relaxed),"drops":stats.fec_drops.load(Ordering::Relaxed)},
        "damaged_frames":stats.damaged_frames.load(Ordering::Relaxed),"queue_drops":stats.queue_drops.load(Ordering::Relaxed),
        "injected_loss":stats.injected_loss.load(Ordering::Relaxed),"keyframe_requests":stats.keyframe_requests.load(Ordering::Relaxed),
        "waiting_for_keyframe":stats.waiting_for_keyframe.load(Ordering::Relaxed),
    });
    value["recovery_causes"]=serde_json::json!({"receive_damage":stats.recovery_receive_damage.load(Ordering::Relaxed),"decode_queue":stats.recovery_decode_queue.load(Ordering::Relaxed),"decoder_error":stats.recovery_decoder_error.load(Ordering::Relaxed),"format_change":stats.recovery_format_change.load(Ordering::Relaxed)});
    value["frame_cadence"]=serde_json::to_value(stats.frame_cadence.lock().unwrap().clone()).unwrap_or_default();
    value
}

pub fn sources(json: bool) -> Result<()> {
    if json {
        #[cfg(windows)]
        let monitors = uu_capture::windows::diagnostic_monitors()?;
        #[cfg(not(windows))]
        let monitors = uu_core::native::monitors()?.into_iter().map(|m|serde_json::json!({"name":m.name,"physical_bounds":[m.x,m.y,m.width as i32,m.height as i32],"primary":m.primary})).collect::<Vec<_>>();
        println!("{}",serde_json::json!({"version":1,"coordinate_units":"physical_pixels","monitors":monitors}));
        return Ok(());
    }
    for source in uu_core::native::monitors()? {
        println!(
            "{} {}x{} origin=({},{}) primary={}",
            source.name, source.width, source.height, source.x, source.y, source.primary
        );
    }
    Ok(())
}
