//! Windows-only synthetic timing probe; no desktop, connection or input access.
//! Measures the production timer guard and its release on async cancellation.
#[cfg(windows)]
#[path = "../src/video_timing.rs"]
mod video_timing;

#[cfg(windows)]
fn main() {
    use std::time::{Duration, Instant};
    use video_timing::VideoTimerResolution;
    fn report(phase: &str, mut late: Vec<f64>, elapsed: Duration) {
        late.sort_by(f64::total_cmp);
        let n = late.len();
        println!(
            "{}",
            serde_json::json!({"phase":phase,"samples":n,
            "elapsed_ms":elapsed.as_secs_f64()*1000.0,
            "late_mean_p50_p95_max_ms":[late.iter().sum::<f64>()/n as f64,
                late[n/2],late[(n*95).div_ceil(100)-1],late[n-1]]})
        );
    }
    async fn measure(phase: &str) {
        let mut ticker = tokio::time::interval(Duration::from_millis(5));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;
        let start = Instant::now();
        let mut late = Vec::with_capacity(400);
        for _ in 0..400 {
            late.push(ticker.tick().await.elapsed().as_secs_f64() * 1000.0);
        }
        report(phase, late, start.elapsed());
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        measure("default").await;
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let session = tokio::spawn(async move {
            let timer = VideoTimerResolution::request();
            ready_tx.send(timer.result()).unwrap();
            std::future::pending::<()>().await;
            drop(timer);
        });
        let result = ready_rx.await.unwrap();
        println!("{}", serde_json::json!({"requested_ms":1,"result":result}));
        assert_eq!(result, 0, "Windows rejected the timer request");
        measure("active_video_guard").await;
        session.abort();
        assert!(session.await.unwrap_err().is_cancelled());
        measure("after_session_cancel").await;
    });
}

#[cfg(not(windows))]
fn main() {
    eprintln!("This synthetic timing probe requires Windows.");
}
