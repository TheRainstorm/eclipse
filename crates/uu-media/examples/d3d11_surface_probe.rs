//! Offline generated-video validation, no window, desktop, input, or audio.
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use std::{sync::Arc, time::Instant};
    use uu_media::{
        color::YuvColor,
        d3d11_frame::{self, Converter},
        EncodedFrame, Timestamp,
    };
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: d3d11_surface_probe GENERATED_AUD.h264"))?;
    let data = std::fs::read(path)?;
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 < data.len() {
        let n = if data[i..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if data[i..].starts_with(&[0, 0, 1]) {
            3
        } else {
            i += 1;
            continue;
        };
        if data.get(i + n).is_some_and(|h| h & 31 == 9) {
            starts.push(i);
        }
        i += n;
    }
    anyhow::ensure!(!starts.is_empty(), "generated fixture must contain AUDs");
    starts[0] = 0;
    starts.push(data.len());
    let mut cpu = uu_media::hw::create_hardware_decoder()?;
    let mut gpu = uu_media::hw::create_hardware_decoder()?;
    let mut converter: Option<Converter> = None;
    let mut held: Option<(Arc<d3d11_frame::Frame>, Vec<u8>, YuvColor)> = None;
    let mut color = YuvColor::default();
    let mut timings = [0_f64; 3];
    let mut frames = 0;
    let mut checked = 0;
    let mut max_error = 0_u8;
    let mut sum_error = 0_u64;
    let mut pixels = 0_u64;
    for (index, span) in starts.windows(2).enumerate() {
        let bytes = bytes::Bytes::copy_from_slice(&data[span[0]..span[1]]);
        if let Some(c) = uu_media::color::from_h264(&bytes) {
            color = c;
        }
        let encoded = EncodedFrame {
            is_keyframe: EncodedFrame::detect_h264_keyframe(&bytes),
            data: bytes,
            pts: Timestamp(index as u64 * 16667 + 10_000_000),
        };
        d3d11_frame::set_rendering(false);
        let at = Instant::now();
        let a = cpu.decode(&encoded)?;
        let cpu_time = at.elapsed().as_secs_f64();
        d3d11_frame::set_rendering(true);
        let at = Instant::now();
        let b = gpu.decode(&encoded)?;
        let gpu_time = at.elapsed().as_secs_f64();
        let (a, b) = match (a, b) {
            (Some(a), Some(b)) => (a, b),
            (None, None) => continue,
            _ => anyhow::bail!("hardware output ordering differs"),
        };
        anyhow::ensure!(
            b.gpu && b.data.is_empty() && b.is_consistent(),
            "GPU path fell back or contains CPU pixels"
        );
        anyhow::ensure!(
            (a.width, a.height, a.pts) == (b.width, b.height, b.pts),
            "leased frame metadata differs"
        );
        let surface = b.d3d11.unwrap();
        if !converter.as_ref().is_some_and(|c| c.matches(&surface)) {
            converter = Some(Converter::new(&surface).map_err(anyhow::Error::msg)?);
        }
        let c = converter.as_ref().unwrap();
        let at = Instant::now();
        c.convert(&surface, color).map_err(anyhow::Error::msg)?;
        // Readback exists only to compare generated pixels and measure all GPU
        // completion work. Do not present decode submit time as end-to-end time.
        let rgb = c.readback().map_err(anyhow::Error::msg)?;
        let conversion = at.elapsed().as_secs_f64();
        if index >= 10 {
            timings[0] += cpu_time;
            timings[1] += gpu_time;
            timings[2] += conversion;
            frames += 1;
        }
        let expected = uu_media::convert::frame_to_rgba_with_color(&a, color);
        for (actual, expected) in rgb.chunks_exact(4).zip(expected.chunks_exact(4)) {
            for (actual, expected) in [actual[2], actual[1], actual[0]]
                .into_iter()
                .zip(expected[..3].iter().copied())
            {
                let error = actual.abs_diff(expected);
                max_error = max_error.max(error);
                sum_error += error as u64;
                pixels += 1;
            }
        }
        // Retain an old decoder sample across subsequent decode calls. A COM
        // texture reference without the MFT sample lease corrupts this check.
        if let Some((old, reference, old_color)) = &held {
            if c.matches(old) {
                c.convert(old, *old_color).map_err(anyhow::Error::msg)?;
                anyhow::ensure!(
                    c.readback().map_err(anyhow::Error::msg)? == *reference,
                    "held decoder sample was overwritten"
                );
                checked += 1;
            }
        }
        held = Some((surface, rgb, color));
    }
    // Leased frames also survive decoder recreation and MF worker teardown.
    drop(cpu);
    drop(gpu);
    if let (Some(c), Some((old, reference, color))) = (&converter, &held) {
        c.convert(old, *color).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            c.readback().map_err(anyhow::Error::msg)? == *reference,
            "surface lost during decoder teardown"
        );
    }
    anyhow::ensure!(
        frames > 0 && checked > 0,
        "insufficient actual hardware output"
    );
    println!(
        "{}",
        serde_json::json!({"frames":frames,"retained_sample_checks":checked,
        "cpu_decode_readback_ms":timings[0]*1000./frames as f64,"gpu_decode_submit_ms":timings[1]*1000./frames as f64,
        "gpu_convert_and_test_readback_ms":timings[2]*1000./frames as f64,
        "rgb_max_error":max_error,"rgb_mean_error":sum_error as f64/pixels.max(1) as f64,
        "desktop_captured":false,"input_sent":false,"audio_opened":false})
    );
    anyhow::ensure!(
        (sum_error as f64 / pixels.max(1) as f64) < 3.0,
        "GPU color conversion mean error is excessive"
    );
    Ok(())
}
#[cfg(not(windows))]
fn main() {
    eprintln!("D3D11 surface validation requires Windows");
}
