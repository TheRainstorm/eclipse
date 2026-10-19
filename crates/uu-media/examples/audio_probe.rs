//! End-to-end Opus -> playback device probe; records no microphone input.
use std::{sync::{Arc, atomic::Ordering::Relaxed}, time::{Duration, Instant}};
use uu_media::audio::{self, AudioControl, Packet};
fn main() -> anyhow::Result<()> {
    let control = Arc::new(AudioControl::default());
    let tx = audio::playback(control.clone());
    let mut encoder = opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::LowDelay)?;
    let start = Instant::now();
    for i in 0..150u16 {
        let pcm: Vec<f32> = (0..960).flat_map(|n| {
            let sample = ((i as usize * 960 + n) as f32 * std::f32::consts::TAU * 440.0 / 48_000.0).sin() * 0.2;
            [sample, sample]
        }).collect();
        let mut encoded = vec![0; 4000];
        let len = encoder.encode_float(&pcm, &mut encoded)?;
        encoded.truncate(len);
        control.packets.fetch_add(1, Relaxed);
        tx.send(Packet { repaired:false, sequence: i, timestamp: u32::from(i) * 960, payload: encoded, received: Instant::now() })?;
        std::thread::sleep((start + Duration::from_millis(u64::from(i + 1) * 20)).saturating_duration_since(Instant::now()));
    }
    std::thread::sleep(Duration::from_millis(80));
    let status = control.status.lock().unwrap().clone();
    let packets = control.packets.load(Relaxed);
    println!("status={status}; packets={packets}; dropped={}; underruns={}; queue_ms={}", control.dropped.load(Relaxed), control.underruns.load(Relaxed), control.queue_ms.load(Relaxed));
    control.stop();
    anyhow::ensure!(packets >= 145 && status.starts_with("Opus"), "playback did not establish a device");
    Ok(())
}
