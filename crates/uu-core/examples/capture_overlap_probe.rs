//! Real DXGI -> serial/overlap -> AMF -> MF GPU correctness only.
//! Requires an owned changing marker window; samples only its fixed coordinates.
#[path = "capture_overlap_probe/oracle.rs"]
mod oracle;
#[cfg(not(windows))]
fn main() {
    eprintln!("Windows GPU correctness probe required");
}
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    let overlap =
        oracle::mode(&std::env::args().skip(1).collect::<Vec<_>>()).map_err(anyhow::Error::msg)?;
    for generation in 1..=2 {
        run(generation, overlap)?;
    }
    Ok(())
}
#[cfg(windows)]
fn run(generation: u32, overlap: bool) -> anyhow::Result<()> {
    use anyhow::{ensure, Context};
    use std::{collections::BTreeMap, time::Duration};
    use uu_media::{
        d3d11_frame,
        encoder::{self, Preference, RateControl},
    };
    use uu_proto::media::VideoCodec;
    let mode = if overlap { "overlap" } else { "serial" };
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
    println!(
        "{}",
        serde_json::json!({"event":"overlap_gpu_preflight","mode":mode,"generation":generation,"encoder":encoder.backend_name(),"hardware":encoder.is_hardware()})
    );
    ensure!(
        encoder.backend_name().starts_with("amf") && encoder.is_hardware(),
        "AMF required"
    );
    let mut input = uu_core::capture_overlap::Input::new(
        capture,
        overlap,
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
        serde_json::json!({"event":"overlap_gpu_start","mode":mode,"generation":generation,"size":[width,height],"encoder":encoder.backend_name(),"decoder":decoder.backend_name(),"white":white,"scope":"real DXGI; point readback correctness only; not performance"})
    );
    let mut expected = BTreeMap::new();
    let mut converted: Option<d3d11_frame::Converter> = None;
    let mut decoded = 0;
    let mut first_source = None;
    let mut last_source = None;
    let mut weak = Vec::new();
    let mut sequence = None;
    for index in 0..16 {
        let observed = input.next(true)?;
        println!(
            "{}",
            serde_json::json!({"event":"overlap_gpu_observation","mode":mode,"generation":generation,"input":index,"pts":observed.frame.pts.0,"source":observed.source,"current":observed.current(),"gpu":observed.frame.gpu})
        );
        ensure!(
            observed.current() && observed.frame.gpu,
            "active GPU capture required"
        );
        let info = observed.source.context("source identity missing")?;
        first_source.get_or_insert((info.generation, info.frame_id));
        last_source = Some((info.generation, info.frame_id));
        // Weak references on the pool slot itself would block Arc::get_mut.
        // Observe an equivalent per-input lease view of that same GPU texture.
        let leased = encoder::capture_probe_lease(&observed.frame).map_err(anyhow::Error::msg)?;
        let before = samples(&leased, white)?;
        std::thread::sleep(Duration::from_millis(50));
        let after = samples(&leased, white)?;
        println!(
            "{}",
            serde_json::json!({"event":"overlap_gpu_held_pixels","mode":mode,"generation":generation,"input":index,"pts":observed.frame.pts.0,"source":info,"points":points(),"before_rgb":before,"after_rgb":after,"hold_ms":50,"immutable":before==after})
        );
        ensure!(before == after, "in-flight capture texture overwritten");
        weak.push(std::sync::Arc::downgrade(
            leased.d3d11.as_ref().context("capture surface absent")?,
        ));
        let old = expected.insert(observed.frame.pts.0, (before.clone(), info, index));
        println!(
            "{}",
            serde_json::json!({"event":"overlap_gpu_input_identity","mode":mode,"generation":generation,"input":index,"pts":observed.frame.pts.0,"source":info,"expected_rgb":before,"duplicate_pts":old.is_some()})
        );
        ensure!(old.is_none(), "duplicate input PTS");
        if index % 5 == 0 {
            encoder.request_keyframe();
        }
        for au in encoder.encode(&leased)? {
            if let Some(v) = uu_media::av1::sequence(&au.data) {
                println!(
                    "{}",
                    serde_json::json!({"event":"overlap_gpu_sequence","mode":mode,"generation":generation,"pts":au.pts.0,"size":[v.width,v.height],"bit_depth":v.bit_depth,"hdr":v.color.hdr()})
                );
                ensure!(
                    v.width == width && v.height == height && v.bit_depth == 8 && !v.color.hdr(),
                    "AV1 SDR geometry/color mismatch"
                );
                sequence = Some(v);
            }
            for picture in decoder.decode_frames(&au)? {
                let reference = expected.remove(&picture.pts.0);
                println!(
                    "{}",
                    serde_json::json!({"event":"overlap_gpu_decode_identity","mode":mode,"generation":generation,"pts":picture.pts.0,"expected":reference,"gpu":picture.gpu,"format":format!("{:?}",picture.format)})
                );
                let (reference, expected_info, input_index) =
                    reference.context("unknown/duplicate decoded PTS")?;
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
                let bgra = converter.readback().map_err(anyhow::Error::msg)?;
                let actual_bgra = points()
                    .into_iter()
                    .map(|(x, y)| {
                        let at = (y * width as usize + x) * 4;
                        [bgra[at], bgra[at + 1], bgra[at + 2], bgra[at + 3]]
                    })
                    .collect::<Vec<_>>();
                let assessment =
                    oracle::assess(&reference, &actual_bgra).map_err(anyhow::Error::msg)?;
                // Emit complete fixed-point evidence BEFORE marker/color assertions.
                println!(
                    "{}",
                    serde_json::json!({"event":"overlap_gpu_pixel_evidence","mode":mode,"generation":generation,"input":input_index,"pts":picture.pts.0,"source":expected_info,"points":points(),"expected_rgb":reference,"actual_bgra":actual_bgra,"actual_rgb":assessment.actual_rgb,"point_errors":assessment.point_errors,"max_rgb_error":assessment.max_error,"rgb_error_limit":oracle::MAX_RGB_ERROR,"expected_marker_points_rgb":&reference[6..],"actual_marker_points_bgra":&actual_bgra[6..],"expected_marker":assessment.expected_marker,"actual_marker":assessment.actual_marker})
                );
                assessment.check().map_err(anyhow::Error::msg)?;
                decoded += 1;
                println!(
                    "{}",
                    serde_json::json!({"event":"overlap_gpu_frame","mode":mode,"generation":generation,"input":input_index,"source":expected_info,"pts":picture.pts.0,"max_rgb_error":assessment.max_error,"expected_marker":assessment.expected_marker.unwrap(),"actual_marker":assessment.actual_marker.unwrap(),"immutable_held_pixels":true})
                );
            }
        }
    }
    println!(
        "{}",
        serde_json::json!({"event":"overlap_gpu_generation_evidence","mode":mode,"generation":generation,"first_source":first_source,"last_source":last_source,"decoded":decoded,"expected_pending":expected.len(),"held_weak_count":weak.len()})
    );
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
    let released = weak
        .iter()
        .map(|v| v.upgrade().is_none())
        .collect::<Vec<_>>();
    println!(
        "{}",
        serde_json::json!({"event":"overlap_gpu_lease_evidence","mode":mode,"generation":generation,"released":released})
    );
    ensure!(
        released.len() == 16 && released.iter().all(|v| *v),
        "capture lease leaked across owner shutdown"
    );
    println!(
        "{}",
        serde_json::json!({"event":"overlap_gpu_complete","mode":mode,"generation":generation,"decoded":decoded,"lease_released":true,"all_held_leases":released.len()})
    );
    Ok(())
}
#[cfg(windows)]
fn points() -> Vec<(usize, usize)> {
    (0..6)
        .map(|i| (32 + 80 + i * 160, 32 + 270))
        .chain((0..32).map(|bit| (32 + 15 + bit * 30, 32 + 450)))
        .collect()
}
#[cfg(windows)]
fn samples(frame: &uu_media::RawFrame, white: f32) -> anyhow::Result<Vec<[u8; 3]>> {
    uu_media::encoder::capture_rgb_samples(frame, &points(), white).map_err(anyhow::Error::msg)
}
