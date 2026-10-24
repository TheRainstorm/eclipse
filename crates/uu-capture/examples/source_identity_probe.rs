//! Explicit, read-only desktop probe. Export metadata, never desktop pixels.
#[cfg(not(windows))]
fn main() {
    eprintln!("This probe requires Windows Desktop Duplication.");
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    anyhow::ensure!(
        std::env::args().any(|arg| arg == "--desktop"),
        "pass --desktop to inspect the current desktop's source identity"
    );
    let mut capture = uu_capture::open(&uu_capture::CaptureSource::Primary)?;
    capture.prefer_gpu(true, None);
    let started = std::time::Instant::now();
    let mut previous = None;
    let mut fresh = 0;
    let mut repeated = 0;
    for index in 0..60 {
        let frame = loop {
            match capture.next_frame() {
                Ok(frame) => break frame,
                Err(uu_capture::CaptureError::Suspended(_))
                    if started.elapsed() < std::time::Duration::from_secs(5) => {}
                Err(error) => return Err(error.into()),
            }
        };
        let info = capture
            .frame_info()
            .ok_or_else(|| anyhow::anyhow!("missing source identity"))?;
        if let Some((generation, id)) = previous {
            if info.generation == generation {
                anyhow::ensure!(
                    if info.duplicate {
                        info.frame_id == id
                    } else {
                        info.frame_id > id
                    },
                    "source identity advanced incorrectly"
                );
            } else {
                anyhow::ensure!(!info.duplicate, "a new desktop repeated a retired image");
            }
        }
        previous = Some((info.generation, info.frame_id));
        if info.duplicate {
            repeated += 1;
        } else {
            fresh += 1;
        }
        if index < 3 || index == 59 {
            println!("sample={index} gpu={} source={info:?}", frame.gpu);
        }
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    println!("source identity: new={fresh} repeated={repeated}");
    Ok(())
}
