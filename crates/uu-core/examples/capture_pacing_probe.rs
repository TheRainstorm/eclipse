//! Bounded scheduler model. No desktop, GPU, input or network access.
#[path = "../src/video_pacing.rs"]
mod video_pacing;
#[cfg(windows)]
#[path = "../src/video_timing.rs"]
mod video_timing;
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let value = |name: &str, default: &str| {
        args.iter()
            .position(|v| v == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_else(|| default.into())
    };
    let fps: u32 = value("--fps", "165").parse()?;
    let seconds: u64 = value("--seconds", "10").parse()?;
    let work: f64 = value("--work-ms", "5.3").parse()?;
    anyhow::ensure!(
        (5..=240).contains(&fps)
            && (1..=20).contains(&seconds)
            && work.is_finite()
            && (0.0..=20.).contains(&work),
        "invalid model bounds"
    );
    #[cfg(windows)]
    let _timer = video_timing::VideoTimerResolution::request();
    let period = Duration::from_secs_f64(1. / f64::from(fps));
    let work = Duration::from_secs_f64(work / 1000.);
    let mut trials = Vec::new();
    for keep_phase in [false, true, true, false] {
        let start = Instant::now();
        let mut next = start;
        let mut previous = None;
        let mut gaps = Vec::new();
        let mut costs = Vec::new();
        let end = start + Duration::from_secs(seconds);
        while Instant::now() < end {
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
            let begin = Instant::now();
            std::thread::sleep(work);
            let done = Instant::now();
            costs.push(done.duration_since(begin).as_secs_f64() * 1000.);
            if let Some(previous) = previous {
                gaps.push(done.duration_since(previous).as_secs_f64() * 1000.);
            }
            previous = Some(done);
            next = if keep_phase {
                video_pacing::next_deadline(next, period, done)
            } else {
                (next + period).max(done)
            };
        }
        let elapsed = start.elapsed();
        let count = costs.len();
        let stats = |mut values: Vec<f64>| {
            values.sort_by(f64::total_cmp);
            let n = values.len();
            [
                values.iter().sum::<f64>() / n as f64,
                values[n / 2],
                values[(n * 95).div_ceil(100) - 1],
                values[n - 1],
            ]
        };
        trials.push(serde_json::json!({"keep_phase":keep_phase,"iterations":count,"elapsed_ms":elapsed.as_secs_f64()*1000.,
            "iterations_per_second":count as f64/elapsed.as_secs_f64(),"gap_mean_p50_p95_max_ms":stats(gaps),"work_mean_p50_p95_max_ms":stats(costs)}));
    }
    println!(
        "{}",
        serde_json::json!({"scope":"synthetic scheduler model, not GPU/video or input-to-display latency","fps":fps,"trials":trials})
    );
    Ok(())
}
