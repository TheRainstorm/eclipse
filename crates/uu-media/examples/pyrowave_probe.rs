//! Finite generated-image probe. No desktop, input, service or display changes.
#[cfg(windows)]
#[path = "support/gpu_encoder_input.rs"]
mod gpu_encoder_input;
use std::{path::PathBuf, time::Instant};
use uu_media::{
    encoder::{Preference, RateControl, Settings},
    PixelFormat, RawFrame, Timestamp, VideoDecoder, VideoEncoder,
};
fn main() -> anyhow::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    anyhow::ensure!(
        args.len() >= 1,
        "usage: pyrowave_probe RUNTIME [WIDTH HEIGHT FRAMES KBPS] [--gpu] [--tuning-json FILE]"
    );
    let runtime = PathBuf::from(&args[0]).canonicalize()?;
    let value = |i: usize, default: u32| {
        args.get(i)
            .filter(|v| !v.starts_with("--"))
            .map(|v| v.parse())
            .transpose()
            .map(|v| v.unwrap_or(default))
    };
    let (width, height, frames, kbps) = (
        value(1, 1920)?,
        value(2, 1080)?,
        value(3, 30)?,
        value(4, 200_000)?,
    );
    anyhow::ensure!(
        (2..=8192).contains(&width)
            && (2..=8192).contains(&height)
            && width % 2 == 0
            && height % 2 == 0
            && (1..=240).contains(&frames),
        "invalid probe bounds"
    );
    let gpu = args.iter().any(|v| v == "--gpu");
    let availability = args.iter().any(|v| v == "--availability");
    if availability {
        uu_media::d3d11_frame::set_rendering(gpu);
        uu_media::pyrowave::probe_decoder()?;
    }
    let tuning = if let Some(at) = args.iter().position(|v| v == "--tuning-json") {
        let path = args.get(at + 1).ok_or_else(|| anyhow::anyhow!("missing tuning file"))?;
        serde_json::from_slice(&std::fs::read(path)?)?
    } else {
        Default::default()
    };
    let settings = Settings {
        codec: uu_proto::media::VideoCodec::Pyrowave,
        width,
        height,
        fps: 60,
        bitrate_kbps: kbps,
        preference: Preference::Auto,
        rate_control: RateControl::Cbr,
        tuning,
        hdr: false,
        source_hdr: None,
    };
    let mut source = vec![0u8; width as usize * height as usize * 4];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let p = &mut source[(y * width as usize + x) * 4..][..4];
            p.copy_from_slice(&[
                if x % 96 < 48 { 48 } else { 224 },
                (y * 255 / height as usize) as u8,
                (x * 255 / width as usize) as u8,
                255,
            ]);
        }
    }
    let raw = RawFrame {
        width,
        height,
        format: PixelFormat::Bgra8,
        stride: width * 4,
        data: source.into(),
        pts: Timestamp(1),
        gpu: false,
        dma_buf: None,
        d3d11: None,
    };
    let expected = uu_media::convert::frame_to_rgba(&raw);
    let (raw, input) = if gpu {
        #[cfg(windows)]
        {
            let upload = gpu_encoder_input::Upload::new()?;
            (upload.frame(&raw)?, Some(upload.input_device()?))
        }
        #[cfg(not(windows))]
        {
            anyhow::bail!("this probe supports external GPU capture only on Windows");
        }
    } else {
        (raw, None)
    };
    let mut encoder = uu_media::pyrowave::Encoder::with_runtime(settings, input, &runtime)?;
    let mut decoder = uu_media::pyrowave::Decoder::with_runtime(runtime, gpu);
    decoder.prepare(width, height)?;
    let (mut encode, mut decode, mut bytes) = (vec![], vec![], 0usize);
    let mut last = None;
    for index in 0..frames + 3 {
        let mut frame = raw.clone();
        frame.pts = Timestamp(1 + u64::from(index) * 16667);
        let started = Instant::now();
        let encoded = encoder.encode(&frame)?.pop().unwrap();
        let enc = started.elapsed().as_secs_f64() * 1e3;
        let started = Instant::now();
        let decoded = decoder
            .decode(&encoded)?
            .ok_or_else(|| anyhow::anyhow!("missing decoded image"))?;
        let dec = started.elapsed().as_secs_f64() * 1e3;
        anyhow::ensure!(
            decoded.is_consistent() && decoded.pts == frame.pts,
            "invalid image or timestamp"
        );
        anyhow::ensure!(decoded.gpu == gpu, "unexpected GPU/CPU output path");
        if index >= 3 {
            encode.push(enc);
            decode.push(dec);
            bytes += encoded.data.len();
        }
        last = Some(decoded);
    }
    let last = last.unwrap();
    let decoded = if last.gpu {
        #[cfg(windows)]
        {
            let surface = last.d3d11.unwrap();
            let converter =
                uu_media::d3d11_frame::Converter::new(&surface).map_err(anyhow::Error::msg)?;
            converter
                .convert(&surface, uu_media::color::YuvColor::Full709)
                .map_err(anyhow::Error::msg)?;
            converter.synchronize().map_err(anyhow::Error::msg)?;
            let mut bgra = converter.readback().map_err(anyhow::Error::msg)?;
            for p in bgra.chunks_exact_mut(4) {
                p.swap(0, 2);
            }
            bgra
        }
        #[cfg(not(windows))]
        {
            unreachable!()
        }
    } else {
        uu_media::convert::frame_to_rgba_with_color(&last, uu_media::color::YuvColor::Full709)
    };
    let mut error = 0u64;
    for (a, b) in expected.chunks_exact(4).zip(decoded.chunks_exact(4)) {
        for i in 0..3 {
            error += u64::from(a[i].abs_diff(b[i]));
        }
    }
    let mae = error as f64 / (f64::from(width) * f64::from(height) * 3.);
    anyhow::ensure!(
        mae < 12.,
        "decoded colours/quality incorrect: RGB MAE={mae}"
    );
    let summary = |mut times: Vec<f64>| {
        times.sort_by(f64::total_cmp);
        serde_json::json!({"p50_ms":times[times.len()/2],"p95_ms":times[(times.len()*95/100).min(times.len()-1)],"mean_ms":times.iter().sum::<f64>()/times.len() as f64})
    };
    let leases: Option<serde_json::Value> = if args.iter().any(|v| v == "--lease-check") {
        #[cfg(windows)]
        {
            Some(lease_check(&PathBuf::from(&args[0]).canonicalize()?)?)
        }
        #[cfg(not(windows))]
        {
            anyhow::bail!("lease check requires Windows GPU interop");
        }
    } else {
        None
    };
    println!(
        "{}",
        serde_json::json!({"width":width,"height":height,"frames":frames,"target_kbps":kbps,"actual_kbps":bytes as f64*8.*60./f64::from(frames)/1000.,
        "encoder":encoder.backend_name(),"decoder":decoder.backend_name(),"encode_cpu_call":summary(encode),"decode_cpu_call":summary(decode),"rgb_mae":mae,
        "encode_gpu_timestamps":encoder.performance_stats()?,"decode_gpu_timestamps":decoder.performance_stats()?,
        "decode_submission_includes_gpu_completion":!gpu,"capability_probe_passed":availability,"gpu_output_leases":leases,"real_desktop":false,"end_to_end_latency_measured":false})
    );
    Ok(())
}

#[cfg(windows)]
fn lease_check(runtime: &std::path::Path) -> anyhow::Result<serde_json::Value> {
    use anyhow::ensure;
    use uu_media::{color::YuvColor, d3d11_frame::Converter};
    let upload = gpu_encoder_input::Upload::new()?;
    let settings = Settings {
        width: 64,
        height: 64,
        fps: 60,
        bitrate_kbps: 10_000,
        codec: uu_proto::media::VideoCodec::Pyrowave,
        preference: Preference::Auto,
        rate_control: RateControl::Cbr,
        tuning: Default::default(),
        hdr: false,
        source_hdr: None,
    };
    let mut encoder =
        uu_media::pyrowave::Encoder::with_runtime(settings, Some(upload.input_device()?), runtime)?;
    let mut decoder = uu_media::pyrowave::Decoder::with_runtime(runtime.to_owned(), true);
    decoder.prepare(64, 64)?;
    let encode = |encoder: &mut uu_media::pyrowave::Encoder,
                  index: u8|
     -> anyhow::Result<(uu_media::EncodedFrame, [u8; 4])> {
        let color = [32 + index * 20, 160 - index * 10, 48 + index * 16, 255];
        let raw = RawFrame {
            width: 64,
            height: 64,
            stride: 256,
            format: PixelFormat::Bgra8,
            data: color.repeat(64 * 64).into(),
            pts: Timestamp(u64::from(index) + 1),
            gpu: false,
            d3d11: None,
            dma_buf: None,
        };
        let raw = upload.frame(&raw)?;
        Ok((encoder.encode(&raw)?.pop().unwrap(), color))
    };
    let mut held = vec![];
    for index in 0..4 {
        let (frame, color) = encode(&mut encoder, index)?;
        held.push((
            decoder
                .decode(&frame)?
                .ok_or_else(|| anyhow::anyhow!("output pool filled early"))?,
            color,
        ));
    }
    let (frame, _) = encode(&mut encoder, 4)?;
    ensure!(
        decoder.decode(&frame)?.is_none(),
        "leased output was overwritten instead of dropping at the pool bound"
    );
    drop(held.remove(0));
    let (frame, color) = encode(&mut encoder, 5)?;
    held.push((
        decoder
            .decode(&frame)?
            .ok_or_else(|| anyhow::anyhow!("released output slot was not reusable"))?,
        color,
    ));
    drop(decoder); // output leases must retain the native session and its device
    let mut checks = 0;
    let mut max_error = 0u8;
    for (frame, color) in held {
        let surface = frame.d3d11.unwrap();
        let converter = Converter::new(&surface).map_err(anyhow::Error::msg)?;
        converter
            .convert(&surface, YuvColor::Full709)
            .map_err(anyhow::Error::msg)?;
        converter.synchronize().map_err(anyhow::Error::msg)?;
        let pixels = converter.readback().map_err(anyhow::Error::msg)?;
        for index in [0, 32 * 64 + 32, 64 * 64 - 1] {
            for component in 0..3 {
                let delta = pixels[index * 4 + component].abs_diff(color[component]);
                max_error = max_error.max(delta);
                ensure!(
                    delta <= 5,
                    "a held decoder frame changed after pool reuse or decoder drop"
                );
                checks += 1;
            }
        }
    }
    Ok(
        serde_json::json!({"slots":4,"saturation_returns_none":true,"released_slot_reused":true,"frames_valid_after_decoder_drop":true,"colour_checks":checks,"max_channel_error":max_error}),
    )
}
