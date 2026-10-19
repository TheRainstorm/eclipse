//! Capture only an explicitly requested local diagnostic; never write images by default.
use anyhow::{bail, Context, Result};
use std::{io::Write, path::PathBuf};
use uu_capture::{Capture, CaptureSource};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (mut display, mut output, mut source, mut list, mut frames) =
        (None, None, CaptureSource::Primary, false, 1usize);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--x11-display" => display = Some(args.next().context("missing X11 display")?),
            "--monitor" => source = CaptureSource::Monitor(args.next().context("missing monitor")?),
            "--window" => source = CaptureSource::Window(args.next().context("missing window ID")?.parse()?),
            "--output" => output = Some(PathBuf::from(args.next().context("missing output path")?)),
            "--frames" => frames = args.next().context("missing frame count")?.parse()?,
            "--list" => list = true,
            _ => bail!("usage: capture_probe [--x11-display DISPLAY] [--monitor NAME|--window ID] --output NEW_FILE.ppm [--frames N]; or --list"),
        }
    }
    if list {
        #[cfg(target_os = "linux")]
        for monitor in uu_capture::x11::monitors(display.as_deref())? {
            println!("{monitor:?}");
        }
        #[cfg(windows)]
        for monitor in uu_capture::windows::monitors()? {
            println!("{monitor:?}");
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        bail!("monitor listing is not available on this platform yet");
        return Ok(());
    }
    let output =
        output.context("--output NEW_FILE.ppm is required; no image is saved implicitly")?;
    anyhow::ensure!((1..=300).contains(&frames), "frame count must be 1..=300");
    let mut capture: Box<dyn Capture> = {
        #[cfg(target_os = "linux")]
        {
            Box::new(uu_capture::x11::X11Capture::open(
                display.as_deref(),
                &source,
            )?)
        }
        #[cfg(not(target_os = "linux"))]
        {
            anyhow::ensure!(display.is_none(), "X11 display is Linux-only");
            uu_capture::open(&source)?
        }
    };
    let mut last = None;
    let started = std::time::Instant::now();
    for _ in 0..frames {
        last = Some(capture.next_frame()?);
    }
    let last = last.context("capture produced no frames")?;
    anyhow::ensure!(last.is_consistent(), "capture returned an incomplete frame");
    let rgba = uu_media::convert::frame_to_rgba(&last);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let capture_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut file = std::io::BufWriter::new(options.open(output)?);
    writeln!(file, "P6\n{} {}\n255", last.width, last.height)?;
    for pixel in rgba.chunks_exact(4) {
        file.write_all(&pixel[..3])?;
    }
    file.flush()?;
    println!(
        "capture frames={} resolution={}x{} capture_ms={:.2}",
        frames, last.width, last.height, capture_ms
    );
    Ok(())
}
