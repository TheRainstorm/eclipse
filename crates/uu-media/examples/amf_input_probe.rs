//! Dynamic BGRA8/scRGB16 -> AMD AMF -> hardware decode correctness probe.
//! Usage: amf_input_probe [sdr|hdr] [width] [height] [frames] [generations] [scrgb|bgra].
//! Readback and per-frame records are correctness work, never a latency benchmark.
#[cfg(not(windows))]
fn main() {
    eprintln!("This probe requires Windows HDR hardware APIs.");
}
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    let generations = std::env::args()
        .nth(5)
        .map(|s| s.parse::<u32>())
        .transpose()?
        .unwrap_or(1);
    anyhow::ensure!((1..=16).contains(&generations), "invalid generation count");
    for generation in 1..=generations {
        run(generation)?;
    }
    Ok(())
}
#[cfg(windows)]
fn run(generation: u32) -> anyhow::Result<()> {
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
    let codec = VideoCodec::Av1;
    let bgra = match args.get(5).map(String::as_str).unwrap_or("scrgb") {
        "scrgb"=>false, "bgra"=>true, _=>anyhow::bail!("source must be scrgb or bgra"),
    };
    let hdr = match args.first().map(String::as_str).unwrap_or("sdr") {
        "sdr" => false,
        "hdr" => true,
        _ => anyhow::bail!("mode must be sdr or hdr"),
    };
    anyhow::ensure!(!bgra || !hdr,"BGRA probe requires SDR output");
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
        (8..=4096).contains(&count)
            && width >= 128
            && height >= 128
            && width <= 8192
            && height <= 8192
            && width % 32 == 0
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
                Format: if bgra {DXGI_FORMAT_B8G8R8A8_UNORM}else{DXGI_FORMAT_R16G16B16A16_FLOAT},
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
    let colors = if bgra {[[0.75,0.15,0.25,1.],[0.15,0.70,0.25,1.],[0.15,0.25,0.65,1.],[0.08,0.08,0.08,1.]]}else{[
        [12.5, 0.05, 0.05, 1.],
        [0.05, 2.5375, 0.05, 1.],
        [0.05, 0.05, 1.25, 1.],
        [0.01, 0.01, 0.01, 1.],
    ]};
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
    // Large 16-bit ID plus its complement, changing for every submitted PTS.
    // Centers are far from lossy/chroma boundaries; all 32 bits are checked.
    let marker_y = height as i32 * 7 / 8;
    let mut expected_ids = std::collections::BTreeMap::new();
    let mut expected_colors = std::collections::BTreeMap::new();
    let mut held:Option<(uu_media::RawFrame,Vec<u8>)>=None;
    let mut last_pts = None;
    let mut sequence = None;
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
        preference: Preference::Amd,
    };
    let mut encoder = encoder::create_with_device(settings, Some(input))?;
    d3d11_frame::set_rendering(true);
    let mut decoder =
        uu_media::decoder_for_codec_with_kind(codec, uu_media::DecoderKind::Hardware)?;
    decoder.prepare(width, height)?;
    let mut converted = None;
    let mut produced = 0;
    let mut checked = 0;
    anyhow::ensure!(
        encoder.is_hardware() && encoder.backend_name().starts_with("amf"),
        "AMD AMF required"
    );
    let dxgi: windows::Win32::Graphics::Dxgi::IDXGIDevice = device.cast()?;
    let desc = unsafe { dxgi.GetAdapter()?.GetDesc()? };
    anyhow::ensure!(desc.VendorId == 0x1002, "AMD adapter required");
    println!(
        "{}",
        serde_json::json!({"event":"generation_start", "generation":generation,
        "codec":"av1", "source":if bgra {"B8G8R8A8_UNORM/Bgra8"}else{"R16G16B16A16_FLOAT/scRGB"}, "expected_path":if bgra {"video-processor"}else{"color-converter"}, "width":width,"height":height,
        "bit_depth":if hdr {10} else {8}, "storage":if hdr {"P010"} else {"NV12"},
        "vendor_id":desc.VendorId,"device_id":desc.DeviceId,"luid":[desc.AdapterLuid.HighPart,desc.AdapterLuid.LowPart],
        "encoder":encoder.backend_name(),"decoder":decoder.backend_name(),"pool":std::env::var("UU_AMF_INPUT_POOL").ok()})
    );
    let (mut encode_times, mut decode_times) = (vec![], vec![]);
    for i in 0..count {
        let frame_colors=std::array::from_fn::<_,4,_>(|patch|colors[if bgra{(patch+i)%4}else{patch}]);
        if bgra {
            for (index,color) in frame_colors.iter().enumerate() {
                let x=(index%2) as i32*width as i32/2;let y=(index/2) as i32*height as i32/2;
                unsafe{v1.ClearView(&view,color,Some(&[RECT{left:x,top:y,right:x+width as i32/2,bottom:y+height as i32/2}]));}
            }
        }
        let id = ((generation - 1) * count as u32 + i as u32) as u16;
        let code = u32::from(id) | (u32::from(!id) << 16);
        for bit in 0..32 {
            let level = if code & (1 << bit) != 0 { if bgra {0.9}else{1.5} } else { if bgra {0.05}else{0.01} };
            unsafe {
                v1.ClearView(
                    &view,
                    &[level, level, level, 1.],
                    Some(&[RECT {
                        left: bit * width as i32 / 32,
                        right: (bit + 1) * width as i32 / 32,
                        top: marker_y,
                        bottom: height as i32,
                    }]),
                );
            }
        }
        let pts = (generation as u64 * 1_000_000_000) + i as u64 * 16667;
        anyhow::ensure!(
            expected_ids.insert(pts, id).is_none(),
            "duplicate input PTS"
        );
        expected_colors.insert(pts,frame_colors);
        if i % 137 == 0 {
            encoder.request_keyframe();
        }
        let frame = capture
            .capture(&texture, Timestamp(pts))?
            .ok_or_else(|| anyhow::anyhow!("capture pool saturated"))?;
        anyhow::ensure!(frame.gpu && frame.format==if bgra {PixelFormat::Bgra8}else{PixelFormat::ScRgb16},"unexpected actual capture source format");
        let at = std::time::Instant::now();
        let encoded = encoder.encode(&frame)?;
        encode_times.push(at.elapsed().as_secs_f64() * 1000.);
        for sample in encoded {
            if let Some(actual) = uu_media::av1::sequence(&sample.data) {
                anyhow::ensure!(
                    (actual.width, actual.height) == (width, height),
                    "encoded geometry mismatch"
                );
                anyhow::ensure!(
                    actual.bit_depth == if hdr { 10 } else { 8 },
                    "encoded bit depth mismatch"
                );
                anyhow::ensure!(
                    actual.color
                        == if hdr {
                            uu_media::color::YuvColor::Limited2020Pq
                        } else {
                            uu_media::color::YuvColor::Limited601
                        },
                    "encoded color mismatch: {:?}",
                    actual.color
                );
                sequence = Some(actual);
            }
            let sequence =
                sequence.ok_or_else(|| anyhow::anyhow!("missing AV1 sequence metadata"))?;
            let at = std::time::Instant::now();
            let frames = decoder.decode_frames(&sample)?;
            decode_times.push(at.elapsed().as_secs_f64() * 1000.);
            for picture in frames {
                let pts = picture.pts.0;
                let expected = expected_ids
                    .remove(&pts)
                    .ok_or_else(|| anyhow::anyhow!("unknown/duplicate decoded PTS {pts}"))?;
                anyhow::ensure!(
                    last_pts.is_none_or(|last| pts > last),
                    "out-of-order decoded PTS {pts}"
                );
                last_pts = Some(pts);
                let frame_colors=expected_colors.remove(&pts).ok_or_else(||anyhow::anyhow!("unknown color PTS"))?;
                anyhow::ensure!(
                    (picture.width, picture.height) == (width, height),
                    "decoded geometry mismatch"
                );
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
                let color = sequence.color;
                anyhow::ensure!(
                    surface.valid([width, height]),
                    "invalid decoder surface layout"
                );
                if checked == 0 {
                    println!(
                        "{}",
                        serde_json::json!({"event":"decoded_storage","generation":generation,"decoder":decoder.backend_name(),"format":format!("{:?}",picture.format),"bit_depth":sequence.bit_depth,"color":format!("{:?}",sequence.color),"surface_device":format!("{:p}",surface.device_ptr()),"conversion_device":format!("{:p}",converted.as_ref().unwrap().device_ptr())})
                    );
                }
                let converter = converted.as_ref().unwrap();
                converter
                    .convert(surface, color)
                    .map_err(anyhow::Error::msg)?;
                {
                    if !hdr {
                        let pixels = converter.readback().map_err(anyhow::Error::msg)?;
                        verify_marker(width, height, expected, |x, y| {
                            let at = (y * width as usize + x) * 4;
                            f32::from(pixels[at]) / 255.
                        })?;
                        if bgra {
                            let mut points=Vec::new();let mut expected_rgb=Vec::new();
                            for (index,expected) in frame_colors.iter().enumerate() {
                                points.push([width as usize/4+(index%2)*width as usize/2,height as usize/4+(index/2)*height as usize/2]);
                                expected_rgb.push([0,1,2].map(|c|(expected[c]*255.).round() as u8));
                            }
                            let code=u32::from(expected)|(u32::from(!expected)<<16);
                            for bit in 0..32 {
                                points.push([(2*bit+1)*width as usize/64,height as usize*15/16]);
                                expected_rgb.push([if code&(1<<bit)!=0{230}else{13};3]);
                            }
                            let actual_rgb:Vec<[u8;3]>=points.iter().map(|[x,y]|{let at=(y*width as usize+x)*4;[pixels[at+2],pixels[at+1],pixels[at]]}).collect();
                            let errors:Vec<[u8;3]>=actual_rgb.iter().zip(&expected_rgb).map(|(a,b)|[0,1,2].map(|c|a[c].abs_diff(b[c]))).collect();
                            let max_error=*errors.iter().flatten().max().unwrap();
                            println!("{}",serde_json::json!({"event":"bgra_pixel_evidence","generation":generation,"pts":pts,"id":expected,"points":points,"expected_rgb":expected_rgb,"actual_rgb":actual_rgb,"errors":errors,"rgb_error_limit":14,"max_rgb_error":max_error,"actual_source":"Bgra8","path":"video-processor"}));
                            anyhow::ensure!(max_error<=14,"BGRA color oracle failed: max {max_error}>14");
                            if held.is_none(){held=Some((picture.clone(),pixels.clone()));}
                        } else {
                            for (index, expected) in frame_colors.iter().enumerate() {
                                let x = width as usize / 4 + (index % 2) * width as usize / 2;
                                let y = height as usize / 4 + (index / 2) * height as usize / 2;
                                let at = (y * width as usize + x) * 4;
                                let reference = uu_media::hdr::sc_rgb_to_sdr([expected[0],expected[1],expected[2]],203.);
                                for c in 0..3 {
                                    let actual = pixels[at + 2 - c] as f32 / 255.;
                                    anyhow::ensure!((actual-reference[c]).abs()<0.055,"SDR patch {index}/{c}: {actual} expected {}",reference[c]);
                                }
                            }
                        }
                    } else {
                        let words = converter.readback_fp16().map_err(anyhow::Error::msg)?;
                        verify_marker(width, height, expected, |x, y| {
                            half(words[(y * width as usize + x) * 4])
                        })?;
                        for (index, expected) in frame_colors.iter().enumerate() {
                            let x = width as usize / 4 + (index % 2) * width as usize / 2;
                            let y = height as usize / 4 + (index / 2) * height as usize / 2;
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
                    println!(
                        "{}",
                        serde_json::json!({"event":"frame_checked","generation":generation,"pts":pts,"id":expected,"marker_errors":0,"color_errors":0})
                    );
                }
                produced += 1;
            }
        }
    }
    println!("{}",serde_json::json!({"event":"generation_output_summary","generation":generation,"inputs":count,"decoded":produced,"checked":checked,"unconsumed_pts":expected_ids.keys().collect::<Vec<_>>()}));
    if let Some((picture,before))=held.take() {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let surface=picture.d3d11.as_ref().unwrap();let converter=converted.as_ref().unwrap();
        converter.convert(surface,sequence.unwrap().color).map_err(anyhow::Error::msg)?;
        let after=converter.readback().map_err(anyhow::Error::msg)?;
        let same=before==after;
        println!("{}",serde_json::json!({"event":"bgra_held_frame","generation":generation,"held_pts":picture.pts.0,"held_across_inputs":count,"hold_ms":50,"compared_bytes":before.len(),"immutable":same}));
        anyhow::ensure!(same,"held decoded frame changed across later submissions");
    }
    anyhow::ensure!(
        produced == count && checked == count && expected_ids.is_empty(),
        "too few decoded/checked frames: {produced}/{count}"
    );
    println!(
        "{}",
        serde_json::json!({"event":"generation_checked","generation":generation,"frames":produced,"checked":checked,"missing_pts":expected_ids.len(),"marker_errors":0,"pts_errors":0,"decoder":decoder.backend_name()})
    );
    // Worker joins its owning thread; native termination and lease audits precede this record.
    drop(decoder);
    drop(encoder);
    drop(capture);
    println!(
        "{}",
        serde_json::json!({"event":"generation_closed","generation":generation,"normal":true})
    );
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

#[cfg(windows)]
fn verify_marker(
    width: u32,
    height: u32,
    expected: u16,
    pixel: impl Fn(usize, usize) -> f32,
) -> anyhow::Result<()> {
    let mut code = 0u32;
    for bit in 0..32 {
        let x = (2 * bit + 1) * width as usize / 64;
        let y = height as usize * 15 / 16;
        let mut value = 0.;
        for dy in -2isize..=2 {
            for dx in -2isize..=2 {
                value += pixel(x.wrapping_add_signed(dx), y.wrapping_add_signed(dy));
            }
        }
        value /= 25.;
        anyhow::ensure!(
            value < 0.25 || value > 0.65,
            "ambiguous marker bit {bit}: {value}"
        );
        if value > 0.65 {
            code |= 1 << bit;
        }
    }
    let id = code as u16;
    anyhow::ensure!(
        (code >> 16) as u16 == !id,
        "marker complement mismatch: {code:08x}"
    );
    anyhow::ensure!(id == expected, "marker ID {id} expected {expected}");
    Ok(())
}
