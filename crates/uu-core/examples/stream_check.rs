//! Aggregate-only live diagnostic, restricted to the authorized test host.
use std::{
    sync::atomic::Ordering::Relaxed,
    time::{Duration, Instant},
};
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let device = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: stream_check DEVICE [SECONDS]"))?;
    anyhow::ensure!(
        device == "aeawvmepembcpca5",
        "diagnostic restricted to KVM-WIN11"
    );
    let seconds = args
        .next()
        .map(|v| v.parse::<u64>())
        .transpose()?
        .unwrap_or(35)
        .clamp(10, 120);
    std::env::set_var("UU_FORCE_JOIN", "0");
    let handle = uu_core::controller::spawn(
        uu_core::Config::from_env()?,
        device,
        "36.159.243.209".into(),
    )?;
    let mut frames = handle.frames;
    std::thread::spawn(move || while frames.blocking_recv().is_some() {});
    let started = Instant::now();
    let mut next = started;
    while started.elapsed() < Duration::from_secs(seconds) {
        while let Ok(event) = handle.events.try_recv() {
            if let uu_core::controller::ControllerEvent::Error(_) = event {
                let _ = handle.close.send(());
                anyhow::bail!("controller failed; inspect private diagnostic log");
            }
        }
        if Instant::now() >= next {
            let s = &handle.stats;
            let (w, h) = s.res();
            println!(
                "{}",
                serde_json::json!({"elapsed_s":started.elapsed().as_secs(),"width":w,"height":h,"kbps":s.bitrate_kbps.load(Relaxed),"source_fps":s.src_fps(),"decode_fps":s.dec_fps(),"lost":s.lost.load(Relaxed),"damaged":s.damaged_frames.load(Relaxed),"nack":s.nack_requests.load(Relaxed),"rtx":s.rtx_packets.load(Relaxed),"repair_drops":s.rtx_drops.load(Relaxed),"decoder_errors":s.errors.load(Relaxed),"queue_drops":s.queue_drops.load(Relaxed),"refresh":s.keyframe_requests.load(Relaxed),"waiting_idr":s.waiting_for_keyframe.load(Relaxed),"decoded":s.decoded_total.load(Relaxed)})
            );
            next = Instant::now() + Duration::from_secs(2);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = handle.close.send(());
    std::thread::sleep(Duration::from_secs(2));
    Ok(())
}
