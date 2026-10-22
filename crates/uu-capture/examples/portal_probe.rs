//! Explicit portal diagnostic: metadata only; no frame or key contents are saved.
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use uu_capture::{CaptureSource, OpenOptions};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let (mut allow_input, mut cancel_ms, mut count, mut exercise) = (false, None, 1usize, false);
    let mut sessions = 1u32;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--allow-input" => allow_input = true,
            "--sessions" => {
                sessions = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("missing session count"))?
                    .parse()?
            }
            "--cancel-after-ms" => {
                cancel_ms = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("missing duration"))?
                        .parse::<u64>()?,
                )
            }
            "--frames" => {
                count = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("missing frame count"))?
                    .parse()?
            }
            "--exercise-fixture-input" => {
                anyhow::ensure!(
                    std::env::var("ECLIPSE_PORTAL_FIXTURE").as_deref() == Ok("1"),
                    "input exercise requires the isolated generated fixture"
                );
                exercise = true;
            }
            _ => anyhow::bail!(
                "usage: portal_probe [--allow-input] [--cancel-after-ms N] [--frames N]"
            ),
        }
    }
    anyhow::ensure!((1..=300).contains(&count), "frame count must be 1..=300");
    anyhow::ensure!((1..=4).contains(&sessions), "session count must be 1..=4");
    for _ in 0..sessions {
        let cancelled = Arc::new(AtomicBool::new(false));
        if let Some(ms) = cancel_ms {
            let stop = cancelled.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(ms));
                stop.store(true, Ordering::Release);
            });
        }
        let started = Instant::now();
        let result = uu_capture::open_with_options(
            &CaptureSource::Primary,
            OpenOptions {
                allow_input,
                cancelled,
                ..Default::default()
            },
        );
        match result {
            Ok(mut capture) => {
                let resolution = capture.resolution();
                let mut distinct = std::collections::HashSet::new();
                for _ in 0..count {
                    let frame = capture.next_frame()?;
                    anyhow::ensure!(frame.is_consistent(), "incomplete captured frame");
                    use std::hash::{Hash, Hasher};
                    let mut hash = std::collections::hash_map::DefaultHasher::new();
                    frame.data.hash(&mut hash);
                    distinct.insert(hash.finish());
                    std::thread::sleep(Duration::from_millis(5));
                }
                let mut input = capture.input_injector();
                if exercise {
                    let input = input
                        .as_mut()
                        .ok_or_else(|| anyhow::anyhow!("fixture input unavailable"))?;
                    input.inject(uu_input::InputEvent::MouseMoveAbsolute {
                        x: resolution.0 as i32 / 2,
                        y: resolution.1 as i32 / 2,
                    })?;
                    input.inject(uu_input::InputEvent::Key {
                        key: uu_input::KeyCode(4),
                        pressed: true,
                    })?;
                    input.release_all()?;
                }
                let input_available = input.is_some();
                drop(input);
                drop(capture);
                std::thread::sleep(Duration::from_millis(150));
                println!("{{\"ok\":true,\"width\":{},\"height\":{},\"frames\":{},\"distinct_frames\":{},\"input_available\":{},\"elapsed_ms\":{}}}",resolution.0,resolution.1,count,distinct.len(),input_available,started.elapsed().as_millis());
            }
            Err(e) => {
                // Let asynchronous Session.Close finish while this D-Bus client is
                // still alive, so the fixture can distinguish explicit teardown.
                std::thread::sleep(Duration::from_millis(150));
                eprintln!("capture error: {e}");
                println!(
                    "{{\"ok\":false,\"elapsed_ms\":{}}}",
                    started.elapsed().as_millis()
                );
                std::process::exit(1)
            }
        }
    }
    Ok(())
}
