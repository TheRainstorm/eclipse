//! Decode captured packets to metrics only; never opens a playback device.
use anyhow::{ensure, Context, Result};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use uu_media::{
    audio::{AudioControl, OpusReceiver},
    audio_capture::{CaptureSession, Source},
};
fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let source = match args.next().as_deref() {
        Some("--synthetic") => Source::Synthetic,
        Some("--system-output") => Source::System,
        _ => anyhow::bail!("choose --synthetic or explicit --system-output"),
    };
    let mut packets = 200u64;
    let mut expect_signal = false;
    let mut drop_every = 0;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--packets" => packets = args.next().context("missing packet count")?.parse()?,
            "--expect-signal" => expect_signal = true,
            "--drop-every" => drop_every = args.next().context("missing loss period")?.parse()?,
            _ => anyhow::bail!("unknown audio probe option"),
        }
    }
    ensure!(
        (1..=10_000).contains(&packets) && (drop_every == 0 || drop_every >= 2),
        "invalid audio probe bounds"
    );
    let stop = AtomicBool::new(false);
    let mut capture = CaptureSession::new(source, &stop)?;
    let control = AudioControl::default();
    let mut decoder = OpusReceiver::new()?;
    use std::io::Write;
    println!("{{\"ready\":true}}");
    std::io::stdout().flush()?;
    let deadline = Instant::now() + Duration::from_millis(packets * 10 + 10_000);
    let (mut sent, mut decoded, mut samples, mut sum, mut peak) = (0, 0, 0u64, 0.0f64, 0.0f32);
    let mut first_signal_packet = None;
    let mut signal_packets = 0;
    while sent < packets {
        ensure!(Instant::now() < deadline, "audio probe timed out");
        if let Some(packet) = capture.next_packet()? {
            sent += 1;
            if drop_every > 0 && sent % drop_every == 0 {
                continue;
            }
            let pcm = decoder.decode(packet.sample_index as u32, &packet.data, &control)?;
            decoded += 1;
            if pcm.iter().any(|v| v.abs() > 0.001) {
                first_signal_packet.get_or_insert(sent);
                signal_packets += 1;
            }
            for sample in pcm {
                ensure!(sample.is_finite(), "invalid captured audio waveform");
                samples += 1;
                sum += (sample as f64).powi(2);
                peak = peak.max(sample.abs());
            }
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    stop.store(true, Ordering::Release);
    let rms = (sum / samples.max(1) as f64).sqrt();
    ensure!(
        !expect_signal || rms > 0.003,
        "system output did not contain the expected owned tone"
    );
    ensure!(
        drop_every == 0
            || packets < drop_every * 2
            || control.concealed.load(Ordering::Relaxed) > 0,
        "lost packets did not trigger concealment"
    );
    println!(
        "{}",
        serde_json::json!({"audio_capture":"passed","backend":capture.backend(),"encoded":sent,"decoded":decoded,"rms":rms,"peak":peak,"first_signal_packet":first_signal_packet,"signal_packets":signal_packets,"concealment_attempts":control.concealed.load(Ordering::Relaxed)})
    );
    Ok(())
}
