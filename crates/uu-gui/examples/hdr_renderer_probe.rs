//! Inspect only this probe's FP16 framebuffer on an HDR-enabled Windows output.
//! Supply a generated HEVC/AV1 HDR10 stream; no existing user window is read.
//! --offscreen validates generated FP16 pixels, not desktop HDR presentation.
#[cfg(windows)]
#[path="support/hdr_offscreen_target.rs"]
mod offscreen_target;
#[cfg(windows)]
#[path = "../src/video_hdr.rs"]
mod video_hdr;
#[cfg(windows)]
#[path = "../src/video_texture.rs"]
mod video_texture;
#[cfg(windows)]
#[path = "../src/wgl_interop.rs"]
mod wgl_interop;
#[cfg(not(windows))]
fn main() {
    eprintln!("This probe requires an HDR-enabled Windows output.");
}
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use eframe::{egui, glow::HasContext};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };
    use uu_core::display_frames::{DisplayImage, YuvColor};
    use uu_media::{EncodedFrame, PixelFormat, Timestamp, VideoDecoder};
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("provide a generated HDR10 .h265 or .obu stream"))?;
    let codec = if path.ends_with(".obu") {
        uu_proto::media::VideoCodec::Av1
    } else {
        uu_proto::media::VideoCodec::Hevc
    };
    let offscreen=std::env::args().any(|v|v=="--offscreen");
    let bytes = std::fs::read(path)?;
    anyhow::ensure!(bytes.len() < 16 * 1024 * 1024, "probe input too large");
    let sequence = match codec {
        uu_proto::media::VideoCodec::Hevc => {
            uu_media::hevc::sequence(&bytes).map(|s| (s.width, s.height, s.color, s.bit_depth))
        }
        _ => uu_media::av1::sequence(&bytes).map(|s| (s.width, s.height, s.color, s.bit_depth)),
    }
    .ok_or_else(|| anyhow::anyhow!("missing sequence metadata"))?;
    anyhow::ensure!(
        sequence.2.hdr() && sequence.3 == 10,
        "probe input must be 10-bit PQ/BT.2020"
    );
    uu_media::d3d11_frame::set_rendering(true);
    let mut decoder =
        uu_media::decoder_for_codec_with_kind(codec, uu_media::DecoderKind::Hardware)?;
    decoder.prepare(sequence.0, sequence.1)?;
    let first = if codec == uu_proto::media::VideoCodec::Hevc {
        bytes
            .windows(5)
            .enumerate()
            .skip(1)
            .find(|(_, w)| *w == [0, 0, 0, 1, 70])
            .map_or(bytes.len(), |(i, _)| i)
    } else {
        uu_media::av1::obus(&bytes)
            .map_err(anyhow::Error::msg)?
            .iter()
            .skip(1)
            .find(|obu| (obu[0] >> 3) & 15 == 2)
            .map_or(bytes.len(), |obu| {
                obu.as_ptr() as usize - bytes.as_ptr() as usize
            })
    };
    let sample = EncodedFrame {
        data: bytes[..first].to_vec().into(),
        pts: Timestamp(0),
        is_keyframe: true,
    };
    let mut output = decoder.decode_frames(&sample)?;
    if output.is_empty() {
        output = decoder.decode_frames(&EncodedFrame {
            pts: Timestamp(16667),
            ..sample
        })?;
    }
    eprintln!("HDR renderer: decoded output count={}", output.len());
    let frame = output
        .pop()
        .ok_or_else(|| anyhow::anyhow!("HDR decoder produced no frame"))?;
    anyhow::ensure!(
        frame.gpu && frame.format == PixelFormat::P010,
        "missing P010 GPU output"
    );
    let source = frame.d3d11.as_ref().unwrap();
    let converter = uu_media::d3d11_frame::Converter::new(source).map_err(anyhow::Error::msg)?;
    converter
        .convert(source, sequence.2)
        .map_err(anyhow::Error::msg)?;
    eprintln!("HDR renderer: reading generated reference");
    let words = converter.readback_fp16().map_err(anyhow::Error::msg)?;
    let references: Vec<[f32; 3]> = [
        (frame.width / 4, frame.height / 4),
        (frame.width * 3 / 4, frame.height / 4),
        (frame.width / 4, frame.height * 3 / 4),
        (frame.width * 3 / 4, frame.height * 3 / 4),
    ]
    .into_iter()
    .map(|(x, y)| {
        let at = (y as usize * frame.width as usize + x as usize) * 4;
        [half(words[at]), half(words[at + 1]), half(words[at + 2])]
    })
    .collect();
    struct Probe {
        gl: Arc<eframe::glow::Context>,
        texture: Option<video_texture::VideoTexture>,
        ctx: egui::Context,
        references: Vec<[f32; 3]>,
        count: Arc<AtomicUsize>,
        error: Arc<Mutex<Option<String>>>,
        enabled: bool,
        offscreen:bool,
    }
    impl eframe::App for Probe {
        fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
            if self.count.load(Ordering::Acquire) >= 9 || self.error.lock().unwrap().is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            if !self.enabled {
                eprintln!("HDR renderer: enabling verified surface");
                match if self.offscreen{offscreen_target::enable(ctx,self.gl.clone())}else{video_hdr::enable(ctx,self.gl.clone(),false)} {
                    Ok(true) => self.enabled = true,
                    Ok(false) => {
                        *self.error.lock().unwrap() = Some("probe output is not HDR".into());
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        ctx.request_repaint();
                        return;
                    }
                    Err(e) => {
                        eprintln!("HDR renderer setup failed: {e}");
                        *self.error.lock().unwrap() = Some(e);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        ctx.request_repaint();
                        return;
                    }
                }
            }
            let phase = self.count.load(Ordering::Acquire) / 3;
            eframe::glow_presentation::set_active(ctx, ctx.viewport_id(), phase != 1, phase == 2);
            if self.count.load(Ordering::Acquire) == 6 {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(384., 256.)));
            }
            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(egui::Color32::BLACK))
                .show(ctx, |ui| {
                    let draw =
                        egui::Rect::from_min_size(egui::pos2(16., 48.), egui::vec2(256., 144.));
                    self.texture.as_ref().unwrap().paint(ui, draw);
                    ui.painter().rect_filled(
                        egui::Rect::from_min_max(egui::pos2(8., 8.), egui::pos2(80., 32.)),
                        0.,
                        egui::Color32::WHITE,
                    );
                    let count = self.count.clone();
                    let error = self.error.clone();
                    let refs = self.references.clone();
                    let ctx = ctx.clone();
                    let viewport = ctx.viewport_id();
                    ui.painter().add(egui::PaintCallback {
                        rect: ui.max_rect(),
                        callback: Arc::new(eframe::egui_glow::CallbackFn::new(
                            move |info, painter| unsafe {
                                let gl = painter.gl();
                                let color =
                                    eframe::glow_presentation::color_target_for(&ctx, viewport);
                                let hdr = phase != 1;
                                if hdr && !color.is_some_and(|c| c.hdr) {
                                    *error.lock().unwrap() = Some("HDR target lost".into());
                                    return;
                                }
                                if !hdr && color.is_some() {
                                    *error.lock().unwrap() =
                                        Some("inactive HDR target still owns SDR".into());
                                    return;
                                }
                                let component = if hdr {
                                    gl.get_framebuffer_attachment_parameter_i32(
                                        eframe::glow::FRAMEBUFFER,
                                        eframe::glow::COLOR_ATTACHMENT0,
                                        eframe::glow::FRAMEBUFFER_ATTACHMENT_COMPONENT_TYPE,
                                    )
                                } else {
                                    eframe::glow::FLOAT as i32
                                };
                                if component != eframe::glow::FLOAT as i32 {
                                    *error.lock().unwrap() = Some(
                                        "egui rendered into a non-floating-point buffer".into(),
                                    );
                                    return;
                                }
                                let mut read = |x: f32, y: f32| -> [f32; 4] {
                                    let mut value = [0f32; 4];
                                    let raw = std::slice::from_raw_parts_mut(
                                        value.as_mut_ptr().cast::<u8>(),
                                        16,
                                    );
                                    gl.read_pixels(
                                        (x * info.pixels_per_point) as i32,
                                        info.screen_size_px[1] as i32
                                            - (y * info.pixels_per_point) as i32
                                            - 1,
                                        1,
                                        1,
                                        eframe::glow::RGBA,
                                        eframe::glow::FLOAT,
                                        eframe::glow::PixelPackData::Slice(raw),
                                    );
                                    value
                                };
                                for (i, reference) in refs.iter().enumerate() {
                                    let actual = read(
                                        80. + (i % 2) as f32 * 128.,
                                        84. + (i / 2) as f32 * 72.,
                                    );
                                    for c in 0..3 {
                                        let v = reference[c] * 80.
                                            / color.map_or(80., |c| c.sdr_white_nits);
                                        let expected = if hdr {
                                            if v.abs() <= 0.0031308 {
                                                v * 12.92
                                            } else {
                                                v.signum()
                                                    * (1.055 * v.abs().powf(1. / 2.4) - 0.055)
                                            }
                                        } else {
                                            uu_media::hdr::sc_rgb_to_sdr(*reference, 203.)[c]
                                        };
                                        if (actual[c] - expected).abs() > 0.035 {
                                            *error.lock().unwrap() = Some(format!(
                                                "HDR framebuffer {i}/{c}: {} expected {expected}",
                                                actual[c]
                                            ));
                                            return;
                                        }
                                    }
                                }
                                let overlay = read(32., 16.);
                                if overlay[..3].iter().any(|v| (*v - 1.).abs() > 0.01) {
                                    *error.lock().unwrap() =
                                        Some(format!("SDR overlay missing: {overlay:?}"));
                                    return;
                                }
                                if gl.get_error() != eframe::glow::NO_ERROR {
                                    *error.lock().unwrap() = Some("HDR GL error".into());
                                    return;
                                }
                                count.fetch_add(1, Ordering::Release);
                            },
                        )),
                    });
                });
            ctx.request_repaint();
        }
        fn on_exit(&mut self, _: Option<&eframe::glow::Context>) {
            self.texture.take();
            eframe::glow_presentation::remove(&self.ctx, egui::ViewportId::ROOT);
        }
    }
    let count = Arc::new(AtomicUsize::new(0));
    let error = Arc::new(Mutex::new(None));
    let result_count = count.clone();
    let result_error = error.clone();
    eprintln!("HDR renderer: starting owned UI");
    eframe::run_native(
        "Eclipse HDR renderer diagnostic",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([304., 216.])
                .with_position([200., 200.])
                .with_active(false),
            vsync: false,
            ..Default::default()
        },
        Box::new(move |cc| {
            let gl = cc.gl.clone().ok_or("OpenGL unavailable")?;
            video_texture::enable_gpu_video(&gl);
            let texture = video_texture::VideoTexture::new(
                &cc.egui_ctx,
                Some(&gl),
                DisplayImage::Decoded(0, frame, sequence.2),
            );
            Ok(Box::new(Probe {
                gl,
                texture: Some(texture),
                ctx: cc.egui_ctx.clone(),
                references,
                count,
                error,
                enabled: false,offscreen,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    if let Some(error) = result_error.lock().unwrap().as_ref() {
        anyhow::bail!("{error}");
    }
    anyhow::ensure!(
        result_count.load(Ordering::Acquire) >= 9,
        "too few rendered HDR frames"
    );
    println!("{}",serde_json::json!({"passed_frames":9,"floating_point_pixels":true,"resize":true,"sdr_overlay":true,"offscreen":offscreen,"desktop_hdr_presentation":!offscreen}));
    Ok(())
}
#[cfg(windows)]
fn half(v: u16) -> f32 {
    let sign = if v & 0x8000 != 0 { -1. } else { 1. };
    let e = (v >> 10) & 31;
    sign * if e == 0 {
        f32::from(v & 1023) * 2f32.powi(-24)
    } else {
        (1. + f32::from(v & 1023) / 1024.) * 2f32.powi(i32::from(e) - 15)
    }
}
