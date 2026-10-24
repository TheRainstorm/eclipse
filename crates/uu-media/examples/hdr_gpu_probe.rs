//! Generated GPU HDR pattern -> real 10-bit encoder -> hardware P010 decoder
//! -> FP16 GPU conversion. Readback is used only to inspect the test result.
#[cfg(not(windows))]
fn main() {
    eprintln!("This probe requires Windows HDR hardware APIs.");
}
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use uu_media::{
        d3d11_frame,
        encoder::{self, Preference, RateControl},
        PixelFormat, Timestamp,
    };
    use uu_proto::media::VideoCodec;
    use windows::{
        core::Interface,
        Win32::{
            Foundation::{HMODULE, RECT},
            Graphics::{Direct3D::*, Direct3D11::*, Dxgi::Common::*},
        },
    };
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let codec = match args.first().map(String::as_str) {
        Some("av1") => VideoCodec::Av1,
        Some("h264") => VideoCodec::H264,
        _ => VideoCodec::Hevc,
    };
    let hdr = codec != VideoCodec::H264;
    let width = args
        .get(1)
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(1280u32);
    let height = args
        .get(2)
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(720u32);
    let count = args
        .get(3)
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(120usize);
    anyhow::ensure!(
        count >= 8
            && width >= 128
            && height >= 128
            && width <= 8192
            && height <= 8192
            && width % 4 == 0
            && height % 4 == 0,
        "invalid probe geometry/count"
    );
    let (mut device, mut context) = (None, None);
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    let device = device.unwrap();
    let context = context.unwrap();
    let v1: ID3D11DeviceContext1 = context.cast()?;
    let input = encoder::InputDevice::from_d3d11(device.clone())?;
    let mut texture = None;
    let mut view = None;
    unsafe {
        device.CreateTexture2D(
            &D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
                ..Default::default()
            },
            None,
            Some(&mut texture),
        )?;
        device.CreateRenderTargetView(texture.as_ref().unwrap(), None, Some(&mut view))?;
    }
    let texture = texture.unwrap();
    let view = view.unwrap();
    let colors = [
        [12.5, 0.05, 0.05, 1.],
        [0.05, 2.5375, 0.05, 1.],
        [0.05, 0.05, 1.25, 1.],
        [0.01, 0.01, 0.01, 1.],
    ];
    for (i, color) in colors.iter().enumerate() {
        let x = (i % 2) as i32 * width as i32 / 2;
        let y = (i / 2) as i32 * height as i32 / 2;
        unsafe {
            v1.ClearView(
                &view,
                color,
                Some(&[RECT {
                    left: x,
                    top: y,
                    right: x + width as i32 / 2,
                    bottom: y + height as i32 / 2,
                }]),
            );
        }
    }
    let mut capture = encoder::CapturePool::new(input.clone(), width, height)?;
    let settings = encoder::Settings {
        hdr,
        source_hdr: Some(uu_media::hdr::Metadata {
            primaries: [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]],
            white: [0.3127, 0.3290],
            min_nits: 0.,
            max_nits: 1000.,
            max_full_frame_nits: 400.,
            sdr_white_nits: 203.,
        }),
        tuning: Default::default(),
        codec,
        rate_control: RateControl::Cbr,
        width,
        height,
        fps: 60,
        bitrate_kbps: 40_000,
        preference: Preference::Auto,
    };
    let mut encoder = encoder::create_with_device(settings, Some(input))?;
    d3d11_frame::set_rendering(true);
    let mut decoder =
        uu_media::decoder_for_codec_with_kind(codec, uu_media::DecoderKind::Hardware)?;
    decoder.prepare(width, height)?;
    let mut converted = None;
    let mut produced = 0;
    let mut checked = 0;
    let mut dump = args.get(4).map(std::fs::File::create).transpose()?;
    let (mut encode_times, mut decode_times) = (vec![], vec![]);
    for i in 0..count {
        let frame = capture
            .capture(&texture, Timestamp(i as u64 * 16667))?
            .ok_or_else(|| anyhow::anyhow!("capture pool saturated"))?;
        let at = std::time::Instant::now();
        let encoded = encoder.encode(&frame)?;
        encode_times.push(at.elapsed().as_secs_f64() * 1000.);
        for sample in encoded {
            if let Some(file) = dump.as_mut() {
                use std::io::Write;
                file.write_all(&sample.data)?;
            }
            let at = std::time::Instant::now();
            let frames = decoder.decode_frames(&sample)?;
            decode_times.push(at.elapsed().as_secs_f64() * 1000.);
            for picture in frames {
                anyhow::ensure!(
                    picture.gpu
                        && picture.format
                            == if hdr {
                                PixelFormat::P010
                            } else {
                                PixelFormat::Nv12
                            },
                    "decoder did not preserve the negotiated GPU storage"
                );
                let surface = picture
                    .d3d11
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("missing decoder sample lease"))?;
                if !converted
                    .as_ref()
                    .is_some_and(|c: &d3d11_frame::Converter| c.matches(surface))
                {
                    converted =
                        Some(d3d11_frame::Converter::new(surface).map_err(anyhow::Error::msg)?);
                }
                let color = match codec {
                    VideoCodec::Hevc => uu_media::hevc::sequence(&sample.data).map(|s| s.color),
                    VideoCodec::Av1 => uu_media::av1::sequence(&sample.data).map(|s| s.color),
                    VideoCodec::H264 => uu_media::color::from_h264(&sample.data),
                }
                .unwrap_or(if hdr {
                    uu_media::color::YuvColor::Limited2020Pq
                } else {
                    uu_media::color::YuvColor::Limited601
                });
                let converter = converted.as_ref().unwrap();
                converter
                    .convert(surface, color)
                    .map_err(anyhow::Error::msg)?;
                if produced % 30 == 0 {
                    if !hdr {
                        let pixels = converter.readback().map_err(anyhow::Error::msg)?;
                        for (index, expected) in colors.iter().enumerate() {
                            let x = width as usize / 4 + (index % 2) * width as usize / 2;
                            let y = height as usize / 4 + (index / 2) * height as usize / 2;
                            let at = (y * width as usize + x) * 4;
                            let reference = uu_media::hdr::sc_rgb_to_sdr(
                                [expected[0], expected[1], expected[2]],
                                203.,
                            );
                            for c in 0..3 {
                                let actual = pixels[at + 2 - c] as f32 / 255.;
                                anyhow::ensure!(
                                    (actual - reference[c]).abs() < 0.055,
                                    "SDR tone-mapped patch {index}/{c}: {actual} expected {}",
                                    reference[c]
                                );
                            }
                        }
                    } else {
                        let words = converter.readback_fp16().map_err(anyhow::Error::msg)?;
                        for (index, expected) in colors.iter().enumerate() {
                            let x = width as usize / 4 + (index % 2) * width as usize / 2;
                            let y = height as usize / 4 + (index / 2) * height as usize / 2;
                            println!(
                                "sample={index} scRGB={:.6},{:.6},{:.6}",
                                half(words[(y * width as usize + x) * 4]),
                                half(words[(y * width as usize + x) * 4 + 1]),
                                half(words[(y * width as usize + x) * 4 + 2])
                            );
                            for c in 0..3 {
                                let actual = half(words[(y * width as usize + x) * 4 + c]); // RGB error is relative to this lossy-coded patch's peak,
                                                                                            // not to a near-zero channel after gamut conversion.
                                let peak = expected[..3].iter().copied().fold(0_f32, f32::max);
                                let tolerance = if peak < 0.02 {
                                    0.02
                                } else {
                                    (peak * 0.04).max(0.12)
                                };
                                anyhow::ensure!(
                                    (actual - expected[c]).abs() <= tolerance,
                                    "HDR color sample {index}/{c}: {actual} expected {}",
                                    expected[c]
                                );
                            }
                        }
                    }
                    checked += 1;
                }
                produced += 1;
            }
        }
    }
    anyhow::ensure!(
        produced >= count.saturating_sub(2) && checked >= 1,
        "too few decoded/checked frames: {produced}/{count}"
    );
    encode_times.sort_by(f64::total_cmp);
    decode_times.sort_by(f64::total_cmp);
    let percentile = |v: &Vec<f64>, p: usize| v[(v.len() - 1) * p / 100];
    println!("codec={codec:?} size={width}x{height} frames={produced} checked={checked} encoder={} encode_p50_ms={:.3} encode_p95_ms={:.3} decode_p50_ms={:.3} decode_p95_ms={:.3}",encoder.backend_name(),percentile(&encode_times,50),percentile(&encode_times,95),percentile(&decode_times,50),percentile(&decode_times,95));
    Ok(())
}
#[cfg(windows)]
fn half(value: u16) -> f32 {
    let sign = if value & 0x8000 != 0 { -1. } else { 1. };
    let e = (value >> 10) & 31;
    let m = value & 1023;
    sign * if e == 0 {
        f32::from(m) * 2f32.powi(-24)
    } else {
        (1. + f32::from(m) / 1024.) * 2f32.powi(i32::from(e) - 15)
    }
}
