//! Generated colour bars only: never opens a screen, input or audio device.
use anyhow::{ensure, Context};
use std::io::Write;
use uu_proto::media::VideoCodec;
use uu_media::{
    color::YuvColor,
    encoder::{self, Preference, Settings},
    PixelFormat, RawFrame, Timestamp,
};
#[cfg(windows)]
#[path="support/gpu_encoder_input.rs"]
mod gpu_encoder_input;

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
        dma_buf: None,
        d3d11: None,
    }
}
fn input_format(mut frame: RawFrame, format: PixelFormat) -> RawFrame {
    let pixels = frame.width as usize * frame.height as usize;
    match format {
        PixelFormat::P010|PixelFormat::ScRgb16=>unreachable!("HDR input is covered by hdr_gpu_probe"),
        PixelFormat::I420 => frame,
        PixelFormat::Bgra8 => {
            let mut rgba = uu_media::convert::frame_to_rgba(&frame);
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            frame.data = rgba.into();
            frame.format = format;
            frame.stride = frame.width * 4;
            frame
        }
        PixelFormat::Nv12 => {
            let mut nv12 = vec![0; pixels * 3 / 2];
            nv12[..pixels].copy_from_slice(&frame.data[..pixels]);
            for i in 0..pixels / 4 {
                nv12[pixels + i * 2] = frame.data[pixels + i];
                nv12[pixels + i * 2 + 1] = frame.data[pixels + pixels / 4 + i];
            }
            frame.data = nv12.into();
            frame.format = format;
            frame
        }
    }
}
fn main() -> anyhow::Result<()> {
    let mut settings = Settings {
        hdr:false,source_hdr:None,tuning:Default::default(), codec:uu_proto::media::VideoCodec::H264,rate_control:encoder::RateControl::Cbr,
        width: 1280,
        height: 720,
        fps: 60,
        bitrate_kbps: 8000,
        preference: Preference::Software,
    };
    let mut frames = 180u32;
    let mut format = PixelFormat::I420;
    let mut gpu_input=false;
    let mut gpu_generated=false;
    let mut capture_device=true;
    let mut capture_copy=None;
    let mut source_scale=1u32;
    let mut hardware_decoder = false;
    let mut intel_decoder=false;
    let mut encode_only=false;let mut output_path=None;let mut vbr=false;let mut peak=None;
    let mut verify_every=1u32;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().context("missing probe option value")?;
        match arg.as_str() {
            "--encoder" => {
                settings.preference = match value.as_str() {
                    "software" => Preference::Software,
                    "auto" => Preference::Auto,
                    "hardware" => Preference::Hardware,
                    "intel" => Preference::Intel,
                    "nvidia" => Preference::Nvidia,
                    "amd" => Preference::Amd,
                    "vaapi" => Preference::Vaapi,
                    _ => anyhow::bail!("unknown encoder"),
                }
            }
            "--codec"=>settings.codec=match value.as_str(){"h264"=>VideoCodec::H264,"h265"|"hevc"=>VideoCodec::Hevc,"av1"=>VideoCodec::Av1,_=>anyhow::bail!("codec must be h264, hevc or av1")},
            "--rate-control"=>vbr=match value.as_str(){"cbr"=>false,"vbr"=>true,_=>anyhow::bail!("rate-control must be cbr or vbr")},
            "--bitrate-kbps"=>settings.bitrate_kbps=value.parse()?,
            "--peak-kbps"=>peak=Some(value.parse::<u32>()?),
            "--fps"=>settings.fps=value.parse()?,
            "--amd-quality"=>settings.tuning.amd_quality=match value.as_str(){"speed"=>encoder::AmdQuality::Speed,"balanced"=>encoder::AmdQuality::Balanced,"quality"=>encoder::AmdQuality::Quality,_=>anyhow::bail!("AMD quality must be speed, balanced or quality")},
            "--intel-target-usage"=>settings.tuning.intel_target_usage=value.parse()?,
            "--intel-low-power"=>settings.tuning.intel_low_power=match value.as_str(){"auto"=>encoder::LowPower::Auto,"on"=>encoder::LowPower::On,"off"=>encoder::LowPower::Off,_=>anyhow::bail!("Intel low power must be auto, on or off")},
            "--nvidia-quality-vs-speed"=>settings.tuning.nvidia_quality_vs_speed=Some(value.parse()?),
            "--output"=>output_path=Some(std::path::PathBuf::from(value)),
            "--frames" => frames = value.parse()?,
            "--verify-every" => verify_every=value.parse()?,
            "--decoder" => {
                encode_only=value=="none";
                hardware_decoder = match value.as_str() {
                    "none"=>{encode_only=true;false},
                    "sw" => false,
                    "hw" => true,
                    "vpl" => {intel_decoder=true;true},
                    _ => anyhow::bail!("decoder must be sw, hw, vpl or none"),
                };
            }
            "--width" => settings.width = value.parse()?,
            "--source-scale" => source_scale=value.parse()?,
            "--capture-copy" => capture_copy=match value.as_str(){"none"=>None,"fresh"=>Some(false),"pooled"=>Some(true),_=>anyhow::bail!("capture-copy must be none, fresh or pooled")},
            "--capture-device" => capture_device=match value.as_str(){"on"=>true,"off"=>false,_=>anyhow::bail!("capture-device must be on or off")},
            "--height" => settings.height = value.parse()?,
            "--input" => {
                format = match value.as_str() {
                    "i420" => PixelFormat::I420,
                    "bgra" => PixelFormat::Bgra8,
                    "gpu" => {gpu_input=true;PixelFormat::Bgra8},
                    "gpu-generated" => {gpu_input=true;gpu_generated=true;PixelFormat::Bgra8},
                    "nv12" => PixelFormat::Nv12,
                    _ => anyhow::bail!("input must be i420, bgra, nv12, gpu or gpu-generated"),
                }
            }
            _ => anyhow::bail!("unknown probe argument"),
        }
    }
    ensure!(
        (30..=216000).contains(&frames),
        "probe needs 30..=216000 generated frames"
    );
    ensure!((1..=216000).contains(&verify_every),"verify-every must be 1..=216000");
    ensure!(vbr || peak.is_none(),"--peak-kbps requires --rate-control vbr");
    settings.rate_control=if vbr{encoder::RateControl::Vbr{peak_kbps:peak.unwrap_or(settings.bitrate_kbps.saturating_mul(2))}}else{encoder::RateControl::Cbr};
    settings.validate()?;
    ensure!(capture_copy.is_none() || gpu_generated,"capture-copy requires a generated GPU source");
    ensure!((1..=4).contains(&source_scale) && (gpu_input || source_scale==1)
        && settings.width*source_scale<=8192 && settings.height*source_scale<=8192,"source scale requires valid GPU capture geometry");
    #[cfg(windows)]
    let gpu=if gpu_input {Some(gpu_encoder_input::Upload::new()?)}else{None};
    let preparation_started=std::time::Instant::now();
    #[cfg(windows)]
    let input_device=if capture_device {gpu.as_ref().map(|gpu|gpu.input_device()).transpose()?}else{None};
    #[cfg(not(windows))]
    ensure!(!gpu_input,"D3D11 input requires Windows");
    #[cfg(not(windows))]
    let input_device={let _=capture_device;None};
    let capture_device_supplied=input_device.is_some();
    let mut encoder = encoder::create_with_device(settings,input_device)?;
    let encoder_preparation_ms=preparation_started.elapsed().as_secs_f64()*1000.0;
    ensure!(!gpu_input||encoder.gpu_input(),"selected encoder has no native D3D11 input support");
    // In sampled Windows hardware long tests retain the decoder's GPU lease;
    // conversion below maps only the selected verification frames. The normal
    // per-frame validation path keeps its historical CPU output behavior.
    #[cfg(windows)]
    uu_media::d3d11_frame::set_rendering(hardware_decoder && verify_every>1);
    // Explicit hardware validation must fail on unsupported hardware, rather
    // than silently validating the software fallback after driver errors.
    let mut decoder = if encode_only {None}else if intel_decoder {
        #[cfg(feature="intel-vpl")]
        {ensure!(settings.codec==VideoCodec::Av1,"direct VPL decode probe requires AV1");Some(Box::new(uu_media::hw::vpl::VplAv1Decoder::new()?) as Box<dyn uu_media::VideoDecoder>)}
        #[cfg(not(feature="intel-vpl"))]
        {anyhow::bail!("direct VPL decoder is unavailable in this build")}
    }else if hardware_decoder {
        Some(if settings.codec==VideoCodec::H264 {uu_media::hw::create_hardware_decoder()?}else{uu_media::decoder_for_codec(settings.codec)?})
    } else {
        ensure!(settings.codec==VideoCodec::H264,"software decoder validation supports H264; use --decoder hw or none");
        Some(uu_media::software_decoder()?)
    };
    if let Some(decoder)=decoder.as_mut() {decoder.prepare(settings.width,settings.height)?;}
    let mut output_file=output_path.map(|path|std::fs::OpenOptions::new().write(true).create_new(true).open(path)).transpose()?;
    let mut emitted=0u32;
    let mut color = YuvColor::Full601;
    let mut times = Vec::new();
    let mut capture_times = Vec::new();
    let mut capture_encode_times = Vec::new();
    let mut key_times = Vec::new();
    let mut delta_times = Vec::new();
    let mut keyframe_indexes = Vec::new();
    let mut bytes = 0u64;
    let mut decoded = 0u32;
    let mut zero_output_inputs = 0u32;
    let mut max_decode_lag_frames = 0u32;
    let mut gpu_decoded = 0u32;
    let mut verified = 0u32;
    let mut keys = 0u32;
    let mut maximum_mse = 0.0f64;
    let mut requested_keys = 0u32;
    #[cfg(windows)]
    let mut gpu_verifier:Option<uu_media::d3d11_frame::Converter>=None;
    for index in 0..frames {
        if index == frames / 3 || index == frames * 2 / 3 {
            encoder.request_keyframe();
            requested_keys += 1;
        }
        if index == frames / 2 {
            encoder.set_bitrate((settings.bitrate_kbps/2).max(64))?;
            ensure!(
                encoder.set_bitrate(0).is_err(),
                "invalid bitrate must be rejected without replacing encoder"
            );
        }
        if index == frames * 3 / 4 {
            // Verify both directions without replacing the session or losing
            // timestamp/reference ownership after a rate-control reset.
            encoder.set_bitrate(settings.bitrate_kbps)?;
        }
        let capture_started=std::time::Instant::now();
        let input = if gpu_generated {None}else{Some(input_format(pattern(settings, index), format))};
        #[cfg(windows)]
        let input=if let Some(gpu)=&gpu {
            let source=Settings{width:settings.width*source_scale,height:settings.height*source_scale,..settings};
            let pts=Timestamp(u64::from(index)*1_000_000/u64::from(settings.fps));
            let frame=if gpu_generated {gpu.generated_with_copy(source.width,source.height,index*source_scale,pts,capture_copy)?}
                else {let mut pixels=input_format(pattern(source,index*source_scale),PixelFormat::Bgra8);pixels.pts=pts;gpu.frame(&pixels)?};
            RawFrame{width:settings.width,height:settings.height,stride:settings.width*4,..frame}
        }else{input.unwrap()};
        #[cfg(not(windows))]
        let input=input.context("generated GPU input requires Windows")?;
        let capture_ms=capture_started.elapsed().as_secs_f64()*1000.0;
        capture_times.push(capture_ms);
        let started = std::time::Instant::now();
        let output = encoder.encode(&input)?;
        let elapsed=started.elapsed().as_secs_f64() * 1000.0;
        times.push(elapsed);
        capture_encode_times.push(capture_ms+elapsed);
        ensure!(
            !output.is_empty(),
            "low-delay encoder emitted no current frame"
        );
        for coded in output {
            if coded.is_keyframe {key_times.push(elapsed);keyframe_indexes.push(index);}else{delta_times.push(elapsed);}
            bytes += coded.data.len() as u64;
            emitted+=1;
            if let Some(file)=output_file.as_mut(){file.write_all(&coded.data)?;}
            if encode_only {keys+=u32::from(coded.is_keyframe);continue;}
            if coded.is_keyframe {
                keys += 1;
                color = match settings.codec {VideoCodec::Pyrowave=>Some(YuvColor::Full709),VideoCodec::H264=>uu_media::color::from_h264(&coded.data),VideoCodec::Hevc=>uu_media::hevc::sequence(&coded.data).map(|s|s.color),VideoCodec::Av1=>uu_media::av1::sequence(&coded.data).map(|s|s.color)}
                    .context("keyframe needs colour metadata")?;
                let mut fresh=if settings.codec==VideoCodec::H264{uu_media::software_decoder()?}else{uu_media::decoder_for_codec(settings.codec)?};
                fresh.prepare(settings.width,settings.height)?;
                ensure!(
                    fresh.decode(&coded)?.is_some_and(|f|f.width==settings.width && f.height==settings.height),
                    "recovery IDR must decode with no old reference chain"
                );
                if hardware_decoder {
                    let mut fresh=if settings.codec==VideoCodec::H264{uu_media::hw::create_hardware_decoder()?}else{uu_media::decoder_for_codec(settings.codec)?};
                    fresh.prepare(settings.width,settings.height)?;
                    let mut padded = coded.clone();
                    let mut data = coded.data.to_vec();
                    if settings.codec!=VideoCodec::Av1 {data.extend_from_slice(&[0; 7]);}
                    padded.data = data.into();
                    ensure!(
                        fresh.decode(&padded).with_context(|| format!("fresh hardware decoder at frame {index}"))?.is_some_and(|f|f.width==settings.width && f.height==settings.height),
                        "recovery IDR with Annex-B padding must initialize a fresh hardware decoder"
                    );
                }
            }
            let pictures = decoder.as_mut().unwrap()
                .decode_frames(&coded).with_context(|| format!("continuous decoder at frame {index}"))?;
            zero_output_inputs += u32::from(pictures.is_empty());
            for output in pictures {
                let output_index = decoded;
                let expected_pts = Timestamp(u64::from(output_index)*1_000_000/u64::from(settings.fps));
                ensure!(output.pts == expected_pts, "decoded pictures were lost, duplicated or reordered: expected {}, got {}", expected_pts.0, output.pts.0);
                ensure!(
                    (output.width, output.height) == (settings.width, settings.height),
                    "encoder crop dimensions did not roundtrip"
                );
                decoded += 1;
                gpu_decoded += u32::from(output.gpu);
                if output_index % verify_every != 0 {continue;}
                let expected = uu_media::convert::frame_to_rgba(&input_format(pattern(settings, output_index), format));
                #[cfg(windows)]
                let actual=if let Some(surface)=&output.d3d11 {
                    if !gpu_verifier.as_ref().is_some_and(|converter|converter.matches(surface)) {
                        gpu_verifier=Some(uu_media::d3d11_frame::Converter::new(surface).map_err(anyhow::Error::msg)?);
                    }
                    let converter=gpu_verifier.as_ref().unwrap();
                    converter.convert(surface,color).map_err(anyhow::Error::msg)?;
                    let mut rgba=converter.readback().map_err(anyhow::Error::msg)?;
                    for pixel in rgba.chunks_exact_mut(4) {pixel.swap(0,2);}
                    rgba
                }else{uu_media::convert::frame_to_rgba_with_color(&output,color)};
                #[cfg(not(windows))]
                let actual=uu_media::convert::frame_to_rgba_with_color(&output,color);
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
                verified += 1;
            }
            let lag = emitted.saturating_sub(decoded);
            max_decode_lag_frames = max_decode_lag_frames.max(lag);
            ensure!(lag <= 4, "hardware decoder accumulated more than four pending pictures");
        }
    }
    ensure!(
        emitted == frames && (encode_only || decoded == frames) && keys >= requested_keys + 1,
        "keyframe requests or frame completion failed"
    );
    let first_encode=times[0];
    times.sort_by(f64::total_cmp);
    capture_encode_times.sort_by(f64::total_cmp);
    let average = times.iter().sum::<f64>() / times.len() as f64;
    let mut report=serde_json::json!({"encoder":"passed","backend":encoder.backend_name(),"decoder_backend":decoder.as_ref().map(|d|d.backend_name()),"decode_checked":!encode_only,"codec":format!("{:?}",settings.codec),"rate_control":format!("{:?}",settings.rate_control),"frames":frames,"decoded":decoded,
        "keyframes":keys,"keyframe_indexes":keyframe_indexes,"keyframe_requests":requested_keys,"invalid_bitrate_preserved":true,"pixel_verified_frames":verified,"pixel_verify_every":verify_every,"gpu_decoded_frames":gpu_decoded,"zero_output_inputs":zero_output_inputs,"max_decode_lag_frames":max_decode_lag_frames,
        "width":settings.width,"height":settings.height,"input":format!("{format:?}"),"gpu_input":gpu_input,"gpu_generated":gpu_generated,"capture_device_supplied":capture_device_supplied,"source_scale":source_scale,"color":format!("{color:?}"),"maximum_mse":maximum_mse,
        "encode_ms_mean":average,"encode_ms_p50":times[times.len()/2],"encode_ms_p95":times[times.len()*95/100],
        "encode_ms_first":first_encode,"encode_ms_p99":times[times.len()*99/100],"encode_ms_max":times[times.len()-1],
        "encoder_preparation_ms":encoder_preparation_ms,"preparation_plus_first_encode_ms":encoder_preparation_ms+first_encode,
        "keyframe_encode_ms_mean":(!key_times.is_empty()).then(||key_times.iter().sum::<f64>()/key_times.len() as f64),
        "delta_encode_ms_mean":(!delta_times.is_empty()).then(||delta_times.iter().sum::<f64>()/delta_times.len() as f64),
        "bytes":bytes,"screen_used":false,"input_used":false,"audio_used":false});
    report["capture_fixture"]=serde_json::json!({
        "copy":capture_copy.map(|pooled|if pooled{"pooled"}else{"fresh"}).unwrap_or("none"),
        "cpu_ms_mean":capture_times.iter().sum::<f64>()/capture_times.len() as f64,
        "plus_encode_ms_mean":capture_encode_times.iter().sum::<f64>()/capture_encode_times.len() as f64,
        "plus_encode_ms_p50":capture_encode_times[capture_encode_times.len()/2],
        "plus_encode_ms_p95":capture_encode_times[capture_encode_times.len()*95/100]});
    println!("{report}");
    Ok(())
}
