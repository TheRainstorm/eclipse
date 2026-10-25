//! Actual DXGI -> bounded overlap owner -> AMF -> MF GPU correctness only.
//! Run with an owned changing desktop stimulus. Readbacks are not performance data.
#[cfg(not(windows))]
fn main() {
    eprintln!("Windows GPU correctness probe required");
}
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    for generation in 1..=2 {
        run(generation)?;
    }
    Ok(())
}
#[cfg(windows)]
fn run(generation: u32) -> anyhow::Result<()> {
    use anyhow::{ensure, Context};
    use std::{collections::BTreeMap, time::Duration};
    use uu_media::{
        d3d11_frame,
        encoder::{self, Preference, RateControl},
    };
    use uu_proto::media::VideoCodec;
    let capture = uu_capture::open_with_options(
        &uu_capture::CaptureSource::Primary,
        uu_capture::OpenOptions {
            hdr: true,
            ..Default::default()
        },
    )?;
    let (width, height) = capture.resolution();
    let white = capture.hdr_metadata().map_or(203., |v| v.sdr_white_nits);
    let settings = encoder::Settings {
        width,
        height,
        fps: 60,
        bitrate_kbps: 40000,
        codec: VideoCodec::Av1,
        preference: Preference::Amd,
        rate_control: RateControl::Cbr,
        tuning: Default::default(),
        hdr: false,
        source_hdr: capture.hdr_metadata(),
    };
    let mut encoder = encoder::create_with_device(settings, capture.encoder_input_device())?;
    ensure!(
        encoder.backend_name().starts_with("amf") && encoder.is_hardware(),
        "AMF required"
    );
    let mut input = uu_core::capture_overlap::Input::new(
        capture,
        true,
        60,
        encoder.gpu_input(),
        encoder.gpu_vendor(),
    )?;
    d3d11_frame::set_rendering(true);
    let mut decoder =
        uu_media::decoder_for_codec_with_kind(VideoCodec::Av1, uu_media::DecoderKind::Hardware)?;
    decoder.prepare(width, height)?;
    println!(
        "{}",
        serde_json::json!({"event":"overlap_gpu_start","generation":generation,"size":[width,height],"encoder":encoder.backend_name(),"decoder":decoder.backend_name(),"white":white,"scope":"real DXGI overlap; GPU readback correctness, not performance"})
    );
    let mut expected = BTreeMap::new();
    let mut converted: Option<d3d11_frame::Converter> = None;
    let mut decoded = 0;
    let mut first_source = None;
    let mut last_source = None;
    let mut weak = None;
    let mut sequence = None;
    for index in 0..16 {
        let observed = input.next(true)?;
        ensure!(
            observed.current() && observed.frame.gpu,
            "active GPU capture required"
        );
        let info = observed.source.context("source identity missing")?;
        first_source.get_or_insert((info.generation, info.frame_id));
        last_source = Some((info.generation, info.frame_id));
        let before = samples(&observed.frame, white)?;
        // Hold the exact source lease while the producer captures/replaces
        // newer images, then prove these pixels have not been overwritten.
        std::thread::sleep(Duration::from_millis(50));
        let after = samples(&observed.frame, white)?;
        ensure!(before == after, "in-flight capture texture overwritten");
        weak = Some(std::sync::Arc::downgrade(
            observed.frame.d3d11.as_ref().unwrap(),
        ));
        ensure!(
            expected
                .insert(observed.frame.pts.0, (before.clone(), info))
                .is_none(),
            "duplicate input PTS"
        );
        if index % 5 == 0 {
            encoder.request_keyframe();
        }
        for au in encoder.encode(&observed.frame)? {
            if let Some(v) = uu_media::av1::sequence(&au.data) {
                ensure!(
                    v.width == width && v.height == height && v.bit_depth == 8 && !v.color.hdr(),
                    "AV1 SDR geometry/color mismatch"
                );
                sequence = Some(v);
            }
            for picture in decoder.decode_frames(&au)? {
                let (reference, expected_info) = expected
                    .remove(&picture.pts.0)
                    .context("unknown/duplicate decoded PTS")?;
                ensure!(
                    picture.gpu && picture.format == uu_media::PixelFormat::Nv12,
                    "MF GPU NV12 required"
                );
                let surface = picture.d3d11.as_ref().context("decoder surface absent")?;
                if !converted.as_ref().is_some_and(|v| v.matches(surface)) {
                    converted =
                        Some(d3d11_frame::Converter::new(surface).map_err(anyhow::Error::msg)?);
                }
                let converter = converted.as_ref().unwrap();
                converter
                    .convert(surface, sequence.context("AV1 sequence missing")?.color)
                    .map_err(anyhow::Error::msg)?;
                let rgba = converter.readback().map_err(anyhow::Error::msg)?;
                let mut max_error = 0u8;
                for (n, (x, y)) in points().into_iter().enumerate() {
                    let at = (y * width as usize + x) * 4;
                    for channel in 0..3 {
                        max_error =
                            max_error.max(rgba[at + channel].abs_diff(reference[n][channel]));
                    }
                }
                ensure!(
                    max_error <= 18,
                    "decoded pixel identity mismatch: max RGB error {max_error}"
                );
                decoded += 1;
                println!(
                    "{}",
                    serde_json::json!({"event":"overlap_gpu_frame","generation":generation,"input":index,"source":expected_info,"pts":picture.pts.0,"max_rgb_error":max_error,"immutable_held_pixels":true})
                );
            }
        }
    }
    ensure!(
        first_source != last_source,
        "owned desktop source did not change"
    );
    ensure!(
        decoded == 16 && expected.is_empty(),
        "missing decoded frames: {decoded}/16"
    );
    drop(converted);
    drop(decoder);
    drop(encoder);
    input.finish()?;
    drop(input);
    ensure!(
        weak.as_ref().is_some_and(|v| v.upgrade().is_none()),
        "capture lease leaked across owner shutdown"
    );
    println!(
        "{}",
        serde_json::json!({"event":"overlap_gpu_complete","generation":generation,"decoded":decoded,"lease_released":true})
    );
    Ok(())
}
#[cfg(windows)]
fn points() -> [(usize, usize); 6] {
    std::array::from_fn(|i| (32 + 80 + i * 160, 32 + 270))
}
#[cfg(windows)]
fn samples(frame: &uu_media::RawFrame, white: f32) -> anyhow::Result<Vec<[u8; 3]>> {
    uu_media::encoder::capture_rgb_samples(frame, &points(), white).map_err(anyhow::Error::msg)
}
