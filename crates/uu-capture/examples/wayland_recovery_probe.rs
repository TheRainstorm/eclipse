//! Use only with tools/native/wayland_capture_recovery.py and its owned compositor.
use std::time::{Duration, Instant};
use uu_capture::{Capture, CaptureError, CaptureSource, OpenOptions};

fn stream(index: usize) -> anyhow::Result<()> {
    let mut capture = uu_capture::wayland::WaylandCapture::open(
        &CaptureSource::Monitor("HEADLESS-1".into()), OpenOptions { allow_input: true, ..OpenOptions::default() })?;
    let mapping = capture.desktop_mapping().unwrap();
    let expected = mapping.get().unwrap();
    let revision = mapping.live_revision();
    let mut injector = capture.input_injector().ok_or_else(|| anyhow::anyhow!("owned output has no input backend"))?;
    let started = Instant::now();
    let mut frames = 0usize;
    let mut pauses = 0usize;
    let mut resumed = false;
    let mut last_pts = 0;
    println!("{{\"ready\":true,\"stream\":{index}}}");
    while started.elapsed() < Duration::from_secs(12) {
        match capture.next_frame() {
            Ok(frame) => {
                anyhow::ensure!(frame.pts.0 > last_pts, "capture PTS went backwards after resume");
                last_pts = frame.pts.0;
                frames += 1;
                anyhow::ensure!(mapping.get() == Some(expected), "capture input mapping was not restored");
                resumed |= pauses > 0;
            }
            Err(CaptureError::Suspended(_)) => {
                anyhow::ensure!(mapping.get() == Some(expected) && mapping.live_revision() == revision,
                    "waiting for an unchanged output unnecessarily retired input");
                let input_started = Instant::now();
                injector.inject(uu_input::InputEvent::MouseMoveRelative { dx: 1, dy: 0 })?;
                anyhow::ensure!(input_started.elapsed() < Duration::from_millis(250),
                    "pending screen copy delayed the independent input worker");
                pauses += 1;
            }
            Err(error) => return Err(error.into()),
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    drop(capture);
    anyhow::ensure!(mapping.get().is_none(), "stopped source left its input mapping live");
    anyhow::ensure!(pauses > 0 && resumed && frames > 20, "no pause/resume transition was observed");
    println!("{{\"ok\":true,\"stream\":{index},\"frames\":{frames},\"pauses\":{pauses},\"resumed\":{resumed}}}");
    Ok(())
}
fn main() -> anyhow::Result<()> {
    anyhow::ensure!(std::env::var("UU_OWNED_WAYLAND_TEST").as_deref() == Ok("1"), "requires the owned compositor harness");
    let a = std::thread::spawn(|| stream(1));
    let b = std::thread::spawn(|| stream(2));
    let first = a.join().unwrap();
    let second = b.join().unwrap();
    first?; second?;
    Ok(())
}
