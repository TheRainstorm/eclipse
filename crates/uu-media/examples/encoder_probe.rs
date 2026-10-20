//! Generated colour bars only: never opens a screen, input or audio device.
use anyhow::{ensure, Context};
use uu_media::{
    color::YuvColor,
    encoder::{self, Preference, Settings},
    PixelFormat, RawFrame, Timestamp, VideoDecoder,
};

fn pattern(settings: Settings, index: u32) -> RawFrame {
    let (w, h) = (settings.width as usize, settings.height as usize);
    let mut data = vec![0; w * h * 3 / 2];
    let colors = [(0, 128, 128), (255, 128, 128), (76, 85, 255), (150, 44, 21)];
    for y in 0..h {
        for x in 0..w {
            let (luma, u, v) = colors[((x + index as usize * 2) % w) * 4 / w];
            data[y * w + x] = luma;
            if y % 2 == 0 && x % 2 == 0 {
                let c = y / 2 * (w / 2) + x / 2;
                data[w * h + c] = u;
                data[w * h + w * h / 4 + c] = v;
            }
        }
    }
    RawFrame {
        width: settings.width,
        height: settings.height,
        stride: settings.width,
        format: PixelFormat::I420,
        data: data.into(),
        pts: Timestamp(u64::from(index) * 1_000_000 / u64::from(settings.fps)),
        gpu: false,
    }
}
fn main() -> anyhow::Result<()> {
    let mut settings = Settings {
        width: 1280,
        height: 720,
        fps: 60,
        bitrate_kbps: 8000,
        preference: Preference::Software,
    };
    let mut frames = 180u32;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().context("missing probe option value")?;
        match arg.as_str() {
            "--encoder" => {
                settings.preference = match value.as_str() {
                    "software" => Preference::Software,
                    "auto" => Preference::Auto,
                    "vaapi" => Preference::Vaapi,
                    _ => anyhow::bail!("unknown encoder"),
                }
            }
            "--frames" => frames = value.parse()?,
            "--width" => settings.width = value.parse()?,
            "--height" => settings.height = value.parse()?,
            _ => anyhow::bail!("unknown probe argument"),
        }
    }
    ensure!(
        (30..=1200).contains(&frames),
        "probe needs 30..=1200 generated frames"
    );
    settings.validate()?;
    let mut encoder = encoder::create(settings)?;
    let mut decoder = uu_media::H264Decoder::new()?;
    let mut color = YuvColor::Full601;
    let mut times = Vec::new();
    let mut bytes = 0u64;
    let mut decoded = 0u32;
    let mut keys = 0u32;
    let mut maximum_mse = 0.0f64;
    let mut requested_keys = 0u32;
    for index in 0..frames {
        if index == frames / 3 || index == frames * 2 / 3 {
            encoder.request_keyframe();
            requested_keys += 1;
        }
        if index == frames / 2 {
            encoder.set_bitrate(4000)?;
            ensure!(
                encoder.set_bitrate(0).is_err(),
                "invalid bitrate must be rejected without replacing encoder"
            );
        }
        let input = pattern(settings, index);
        let started = std::time::Instant::now();
        let output = encoder.encode(&input)?;
        times.push(started.elapsed().as_secs_f64() * 1000.0);
        ensure!(
            !output.is_empty(),
            "low-delay encoder emitted no current frame"
        );
        let expected = uu_media::convert::frame_to_rgba(&input);
        for coded in output {
            bytes += coded.data.len() as u64;
            if coded.is_keyframe {
                keys += 1;
                color = uu_media::color::from_h264(&coded.data)
                    .context("keyframe needs colour metadata")?;
                let mut fresh = uu_media::H264Decoder::new()?;
                ensure!(
                    fresh.decode(&coded)?.is_some(),
                    "recovery IDR must decode with no old reference chain"
                );
            }
            let output = decoder
                .decode(&coded)?
                .context("low-delay output must be immediately decodable")?;
            ensure!(
                (output.width, output.height) == (settings.width, settings.height),
                "encoder crop dimensions did not roundtrip"
            );
            let actual = uu_media::convert::frame_to_rgba_with_color(&output, color);
            let mut squared = 0u64;
            for (actual, expected) in actual.chunks_exact(4).zip(expected.chunks_exact(4)) {
                for channel in 0..3 {
                    let diff = i64::from(actual[channel]) - i64::from(expected[channel]);
                    squared += (diff * diff) as u64;
                }
            }
            let mse =
                squared as f64 / (f64::from(settings.width) * f64::from(settings.height) * 3.0);
            maximum_mse = maximum_mse.max(mse);
            ensure!(
                mse < 100.0,
                "encoded colour bars differ from expected range/matrix: MSE={mse}"
            );
            decoded += 1;
        }
    }
    ensure!(
        decoded == frames && keys >= requested_keys + 1,
        "keyframe requests or frame completion failed"
    );
    times.sort_by(f64::total_cmp);
    let average = times.iter().sum::<f64>() / times.len() as f64;
    println!(
        "{}",
        serde_json::json!({"encoder":"passed","backend":encoder.backend_name(),"frames":frames,"decoded":decoded,
        "keyframes":keys,"keyframe_requests":requested_keys,"invalid_bitrate_preserved":true,
        "width":settings.width,"height":settings.height,"color":format!("{color:?}"),"maximum_mse":maximum_mse,
        "encode_ms_mean":average,"encode_ms_p50":times[times.len()/2],"encode_ms_p95":times[times.len()*95/100],
        "bytes":bytes,"screen_used":false,"input_used":false,"audio_used":false})
    );
    Ok(())
}
