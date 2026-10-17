//! Decode an Annex-B H.264 file through the out-of-process ffmpeg backend.
//!
//! Usage: cargo run -p uu-media --example dec_ffmpeg -- <file.h264> [hwaccel]
//! `hwaccel` is passed to ffmpeg's `-hwaccel` (default `none`; try `auto`,
//! `vaapi`, `cuda`, `d3d11va`, `qsv`). A failing hardware accelerator falls
//! back to software automatically on the next IDR.

use uu_media::FfmpegDecoder;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: dec_ffmpeg <file.h264> [hwaccel]");
    let hw = std::env::args().nth(2).unwrap_or_else(|| "none".to_string());
    let data = std::fs::read(&path).expect("read file");
    let units = split_access_units(&data);
    let mut dec = FfmpegDecoder::new(hw.clone());
    // Offline: feed everything, then close the input and drain. The streaming
    // `decode()` path keeps only the newest frame per call (to stay live), so
    // counting uses `push` here.
    for u in &units {
        if let Err(e) = dec.push(u) {
            eprintln!("push error (hwaccel={hw}): {e}");
            break;
        }
    }
    dec.finish();
    let mut frames = 0u32;
    let mut dims = dec.dimensions();
    while let Some(f) = dec.recv_frame() {
        if frames == 0 {
            dims = Some((f.width, f.height));
        }
        frames += 1;
    }
    println!(
        "requested={hw} effective={} units={} frames={frames} dims={dims:?}",
        dec.hwaccel(),
        units.len()
    );
}

fn split_access_units(data: &[u8]) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 < data.len() {
        if data[i..].starts_with(&[0, 0, 0, 1]) || data[i..].starts_with(&[0, 0, 1]) {
            let sc = if data[i..].starts_with(&[0, 0, 0, 1]) { 4 } else { 3 };
            let h = i + sc;
            if h < data.len() && data[h] & 0x1f == 9 {
                starts.push(i);
            }
            i = h + 1;
        } else {
            i += 1;
        }
    }
    starts.push(data.len());
    let mut units = Vec::new();
    for w in starts.windows(2) {
        if w[1] > w[0] {
            units.push(data[w[0]..w[1]].to_vec());
        }
    }
    units
}
