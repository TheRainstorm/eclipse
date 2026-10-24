//! Read-only HDR source-format inspection; no desktop pixels leave the GPU.
#[cfg(not(windows))]
fn main() {
    eprintln!("This diagnostic requires a Windows HDR desktop.");
}
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    anyhow::ensure!(
        std::env::args().any(|v| v == "--desktop"),
        "pass --desktop to explicitly inspect the current desktop capture format"
    );
    use uu_capture::{CaptureError, CaptureSource, OpenOptions};
    let mut capture = uu_capture::open_with_options(
        &CaptureSource::Primary,
        OpenOptions {
            hdr: true,
            ..Default::default()
        },
    )?;
    let metadata = capture
        .hdr_metadata()
        .ok_or_else(|| anyhow::anyhow!("the selected output does not report HDR enabled"))?;
    capture.prefer_gpu(true, None);
    let started = std::time::Instant::now();
    let mut count = 0;
    while count < 3 {
        match capture.next_frame() {
            Ok(frame) => {
                anyhow::ensure!(
                    frame.gpu
                        && frame.format == uu_media::PixelFormat::ScRgb16
                        && frame.data.is_empty()
                        && frame.is_consistent(),
                    "HDR desktop did not retain a floating-point GPU surface"
                );
                count += 1;
                println!(
                    "frame={count} size={}x{} format={:?} gpu=true cpu_bytes=0",
                    frame.width, frame.height, frame.format
                );
            }
            Err(CaptureError::Suspended(_))
                if started.elapsed() < std::time::Duration::from_secs(5) => {}
            Err(error) => return Err(error.into()),
        }
    }
    println!("HDR source metadata={metadata:?}");
    Ok(())
}
