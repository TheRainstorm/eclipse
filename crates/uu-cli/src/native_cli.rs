use anyhow::{ensure, Context, Result};
use std::{
    path::PathBuf,
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};
use tokio::sync::watch;

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
        "native host ended: captured={} sent={} bytes={} input={} input_errors={}",
        report.captured, report.sent, report.bytes, report.input_events, report.input_errors
    );
    Ok(())
}

pub async fn probe(path: PathBuf, frames: u64, seconds: u64) -> Result<()> {
    ensure!(
        (1..=100_000).contains(&frames) && (1..=3600).contains(&seconds),
        "native probe requires 1..=100000 frames and 1..=3600 seconds"
    );
    let invitation = uu_core::native_pairing::Invitation::load(&path)?;
    let stats = Arc::new(uu_core::media::MediaStats::default());
    let worker_stats = stats.clone();
    let (images, mut receiver) = uu_core::media::frame_channel();
    let (stop, rx) = watch::channel(false);
    let worker =
        tokio::spawn(
            async move { uu_core::native::view(&invitation, images, worker_stats, rx).await },
        );
    let started = Instant::now();
    let mut rendered = 0u64;
    let observation = tokio::time::timeout(Duration::from_secs(seconds), async {
        loop {
            while receiver.try_recv().is_ok() {
                rendered += 1;
            }
            if stats.decoded_total.load(Ordering::Relaxed) >= frames {
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
    let _ = stop.send(true);
    let closed = worker.await.context("native viewing task failed")?;
    closed?;
    observation??;
    let (w, h) = stats.res();
    println!("native probe: decoded={} observed={} resolution={}x{} elapsed_ms={:.0} errors={} backend={}",stats.decoded_total.load(Ordering::Relaxed),rendered,w,h,started.elapsed().as_secs_f64()*1000.0,stats.errors.load(Ordering::Relaxed),stats.backend());
    ensure!(
        stats.errors.load(Ordering::Relaxed) == 0,
        "native decoder reported errors"
    );
    Ok(())
}

pub fn sources() -> Result<()> {
    #[cfg(windows)]
    let monitors = uu_capture::windows::monitors()?;
    #[cfg(target_os = "linux")]
    let monitors = uu_capture::x11::monitors(None)?;
    #[cfg(any(windows, target_os = "linux"))]
    for source in monitors {
        println!(
            "{} {}x{} origin=({},{}) primary={}",
            source.name, source.width, source.height, source.x, source.y, source.primary
        );
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    anyhow::bail!("native capture sources are unavailable on this platform");
    Ok(())
}
