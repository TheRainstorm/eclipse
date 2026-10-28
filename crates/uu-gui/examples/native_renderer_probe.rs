//! Generated HDR pixels through native D3D video + GL UI, without changing OS HDR.
//! Optional --fullscreen-cycle [--position x y] also exercises borderless
//! fullscreen on a test output no larger than the 2048x2048 inspection limit.
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
    eprintln!("native composition probe requires Windows");
}
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use eframe::egui;
    use std::sync::{Arc, Mutex};
    use uu_core::display_frames::{DisplayImage, RawFrame};
    use uu_media::{EncodedFrame, PixelFormat, Timestamp, VideoDecoder};
    let args: Vec<String> = std::env::args().collect();
    let fullscreen_cycle = args.iter().any(|s| s == "--fullscreen-cycle");
    let position = args
        .iter()
        .position(|s| s == "--position")
        .map(|at| -> anyhow::Result<[f32; 2]> {
            Ok([
                args.get(at + 1)
                    .ok_or_else(|| anyhow::anyhow!("missing position x"))?
                    .parse()?,
                args.get(at + 2)
                    .ok_or_else(|| anyhow::anyhow!("missing position y"))?
                    .parse()?,
            ])
        })
        .transpose()?
        .unwrap_or([200., 200.]);
    let path = args
        .get(1)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("provide a generated HDR10 .h265 or .obu stream"))?;
    let codec = if path.ends_with(".obu") {
        uu_proto::media::VideoCodec::Av1
    } else {
        uu_proto::media::VideoCodec::Hevc
    };
    anyhow::ensure!(
        position.iter().all(|p| p.is_finite()),
        "position must be finite"
    );
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
        frame: RawFrame,
        color: uu_core::display_frames::YuvColor,
        references: Vec<[f32; 3]>,
        result: Arc<Mutex<Option<video_hdr::native::InspectedFrame>>>,
        checked: Arc<Mutex<Vec<serde_json::Value>>>,
        error: Arc<Mutex<Option<String>>>,
        count: usize,
        ctx: egui::Context,
        last: u64,
        hidden: bool,
        snapshot: bool,
        fullscreen_cycle: bool,
    }
    impl eframe::App for Probe {
        fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
            if self.count >= 12 || self.error.lock().unwrap().is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            let inspected = self.result.lock().unwrap().take();
            if let Some(data) = inspected.filter(|d| d.serial > self.last) {
                self.last = data.serial;
                let ppp = f32::from_bits((data.tag >> 32) as u32);
                let tag = data.tag & u32::MAX as u64;
                let read = |x: f32, y: f32| -> [f32; 3] {
                    let x = (x * ppp) as usize;
                    let y = (y * ppp) as usize;
                    let at = (y * data.size[0] as usize + x) * 4;
                    [
                        half(data.pixels[at]),
                        half(data.pixels[at + 1]),
                        half(data.pixels[at + 2]),
                    ]
                };
                let (hdr, white) = data.color.unwrap();
                let phase = (tag >> 8) & 255;
                let fullscreen = tag & (1 << 24) != 0;
                let ui_code = tag & 255;
                let mut worst = 0_f32;
                for (i, reference) in self.references.iter().enumerate() {
                    let actual = read(80. + (i % 2) as f32 * 128., 84. + (i / 2) as f32 * 72.);
                    let mut expected = if hdr {
                        *reference
                    } else {
                        uu_media::hdr::sc_rgb_to_sdr(*reference, 203.).map(|v| {
                            if v <= 0.04045 {
                                v / 12.92
                            } else {
                                ((v + 0.055) / 1.055).powf(2.4)
                            }
                        })
                    };
                    if i == 0 && phase == 2 {
                        expected = [0., 0., white / 80.];
                    }
                    if i == 0 && phase == 3 {
                        expected = uu_media::hdr::sc_rgb_to_sdr(*reference, 203.).map(|v| {
                            let x = (255. - (v.clamp(0., 1.) * 255.).round()) / 255.;
                            if x <= 0.04045 {
                                x / 12.92
                            } else {
                                ((x + 0.055) / 1.055).powf(2.4)
                            }
                        });
                    }
                    if i == 1 {
                        let ui = egui::Color32::from_rgba_unmultiplied(0, 0, 255, 128)
                            .to_array()
                            .map(|v| v as f32 / 255.);
                        expected = std::array::from_fn(|c| {
                            let v = expected[c] / (white / 80.);
                            let gamma = if v <= 0.0031308 {
                                v * 12.92
                            } else {
                                1.055 * v.powf(1. / 2.4) - 0.055
                            };
                            let gamma = gamma * (1. - ui[3]) + ui[c];
                            let linear = if gamma <= 0.04045 {
                                gamma / 12.92
                            } else {
                                ((gamma + 0.055) / 1.055).powf(2.4)
                            };
                            linear * white / 80.
                        });
                    }
                    for c in 0..3 {
                        worst = worst.max((actual[c] - expected[c]).abs());
                    }
                    if actual.iter().any(|v| !v.is_finite()) || worst > 0.035 {
                        *self.error.lock().unwrap() = Some(format!(
                            "native quad {i} phase{}: {actual:?} expected{expected:?}",
                            data.tag
                        ));
                        break;
                    }
                }
                let ui = read(32., 16.);
                let expected_ui = match (ui_code / 2) % 3 {
                    0 => [white / 80., 0., 0.],
                    1 => [0., white / 80., 0.],
                    _ => [0., 0., white / 80.],
                };
                if ui
                    .iter()
                    .zip(expected_ui)
                    .any(|(v, e)| !v.is_finite() || (*v - e).abs() > 0.015)
                {
                    *self.error.lock().unwrap() =
                        Some(format!("native UI white {ui:?} expected{}", white / 80.));
                }
                eprintln!(
                    "native check tag={} ppp={} ui={:?} expected={:?} size={:?} sample_bottom={:?}",
                    data.tag,
                    ppp,
                    ui,
                    expected_ui,
                    data.size,
                    read(32., data.size[1] as f32 / ppp - 16.)
                );
                self.checked.lock().unwrap().push(serde_json::json!({"phase":phase,"ui_code":ui_code,"pixels_per_point":ppp,"fullscreen":fullscreen,"ui_reused":data.ui_reused,"logical_hdr":hdr,"actual_desktop_hdr":data.actual_hdr,"white_nits":white,"max_quad_error":worst,"ui":ui,"size":data.size}));
                self.count += 1;
            }
            if self.count >= 12 || self.error.lock().unwrap().is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            if self.count == 4 && !self.hidden {
                self.hidden = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                let resume = ctx.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    resume.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    resume.request_repaint();
                });
            }
            if self.count == 8 && !self.snapshot {
                self.snapshot = true;
                match self.texture.as_ref().unwrap().snapshot() {
                    Ok(image) => {
                        for (i, reference) in self.references.iter().enumerate().skip(1) {
                            let at = (image.size[1] * (1 + 2 * (i / 2)) / 4) * image.size[0]
                                + image.size[0] * (1 + 2 * (i % 2)) / 4;
                            let actual = image.pixels[at].to_array();
                            let expected = uu_media::hdr::sc_rgb_to_sdr(*reference, 203.);
                            if (0..3)
                                .any(|c| (actual[c] as f32 / 255. - expected[c]).abs() > 3. / 255.)
                            {
                                *self.error.lock().unwrap() = Some(format!(
                                    "snapshot quad {i}: {actual:?} expected{expected:?}"
                                ));
                            }
                        }
                    }
                    Err(e) => *self.error.lock().unwrap() = Some(format!("snapshot failed: {e}")),
                }
            }
            let phase = (self.count / 3) as u64;
            if self.count == 6 {
                ctx.send_viewport_cmd(if self.fullscreen_cycle {
                    egui::ViewportCommand::Fullscreen(true)
                } else {
                    egui::ViewportCommand::InnerSize(egui::vec2(384., 256.))
                });
            }
            if self.count == 9 && self.fullscreen_cycle {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            }
            eframe::glow_presentation::set_active(ctx, ctx.viewport_id(), true, false);
            let color = match phase {
                0 => (true, 240.),
                2 => (true, 160.),
                _ => (false, 80.),
            };
            eframe::glow_presentation::native_video(
                ctx,
                ctx.viewport_id(),
                Arc::new(video_hdr::native::Inspection {
                    tag: ((ctx.pixels_per_point().to_bits() as u64) << 32)
                        | (u64::from(ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)))
                            << 24)
                        | (phase << 8)
                        | self.count as u64,
                    color: Some(color),
                    result: self.result.clone(),
                }),
            )
            .unwrap();
            if self.count % 3 != 1 {
                self.texture.as_mut().unwrap().set(
                    ctx,
                    DisplayImage::Decoded(0, self.frame.clone(), self.color),
                );
            }
            let cursor = if phase >= 2 {
                let description = uu_core::native_cursor::Description {
                    kind: if phase == 2 { 2 } else { 1 },
                    width: 8,
                    height: if phase == 2 { 8 } else { 16 },
                    pitch: if phase == 2 { 32 } else { 1 },
                    hotspot: [0, 0],
                };
                let data = if phase == 2 {
                    [255, 0, 0, 255].repeat(64)
                } else {
                    vec![255; 16]
                };
                Some(uu_core::native_cursor::Snapshot {
                    revision: phase,
                    shape_id: phase,
                    desktop: [self.frame.width, self.frame.height],
                    position: Some([
                        (self.frame.width / 4) as i32,
                        (self.frame.height / 4) as i32,
                    ]),
                    shape: Some(Arc::new(uu_core::native_cursor::Shape {
                        description,
                        data,
                    })),
                })
            } else {
                None
            };
            self.texture.as_ref().unwrap().set_native_cursor(cursor);
            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(egui::Color32::BLACK))
                .show(ctx, |ui| {
                    let draw =
                        egui::Rect::from_min_size(egui::pos2(16., 48.), egui::vec2(256., 144.));
                    self.texture.as_ref().unwrap().paint(ui, draw);
                    ui.painter().rect_filled(
                        egui::Rect::from_min_max(egui::pos2(196., 76.), egui::pos2(220., 92.)),
                        0.,
                        egui::Color32::from_rgba_unmultiplied(0, 0, 255, 128),
                    );
                    ui.painter().rect_filled(
                        egui::Rect::from_min_max(egui::pos2(8., 8.), egui::pos2(80., 32.)),
                        0.,
                        match (self.count / 2) % 3 {
                            0 => egui::Color32::RED,
                            1 => egui::Color32::GREEN,
                            _ => egui::Color32::BLUE,
                        },
                    );
                });
            ctx.request_repaint();
        }
        fn on_exit(&mut self, _: Option<&eframe::glow::Context>) {
            self.texture.take();
            eframe::glow_presentation::remove(&self.ctx, self.ctx.viewport_id());
        }
    }
    let checked = Arc::new(Mutex::new(Vec::new()));
    let error = Arc::new(Mutex::new(None));
    let result_checked = checked.clone();
    let result_error = error.clone();
    eframe::run_native(
        "Native generated-pixel composition probe",
        eframe::NativeOptions {
            vsync: false,
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([304., 216.])
                .with_position(position)
                .with_active(false),
            ..Default::default()
        },
        Box::new(move |cc| {
            let gl = cc.gl.clone().ok_or("missing GL context")?;
            video_texture::enable_gpu_video(&gl);
            video_hdr::enable_native(&cc.egui_ctx, gl.clone(), false)
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
            let texture = video_texture::VideoTexture::new(
                &cc.egui_ctx,
                Some(&gl),
                DisplayImage::Decoded(0, frame.clone(), sequence.2),
            );
            Ok(Box::new(Probe {
                gl,
                texture: Some(texture),
                frame,
                color: sequence.2,
                references,
                result: Arc::new(Mutex::new(None)),
                checked,
                error,
                count: 0,
                ctx: cc.egui_ctx.clone(),
                last: 0,
                hidden: false,
                snapshot: false,
                fullscreen_cycle,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    if let Some(e) = result_error.lock().unwrap().as_ref() {
        eprintln!(
            "checks_before_failure={}",
            serde_json::to_string(&*result_checked.lock().unwrap())?
        );
        anyhow::bail!("{e}");
    }
    anyhow::ensure!(
        result_checked.lock().unwrap().len() >= 12,
        "too few native pixel frames"
    );
    anyhow::ensure!(
        result_checked
            .lock()
            .unwrap()
            .iter()
            .any(|v| v["ui_reused"] == true),
        "UI layer reuse was not exercised"
    );
    if fullscreen_cycle {
        let checks = result_checked.lock().unwrap();
        anyhow::ensure!(
            checks.iter().any(|v| v["fullscreen"] == true)
                && checks.last().is_some_and(|v| v["fullscreen"] == false),
            "fullscreen enter/exit was not observed"
        );
    }
    println!(
        "{}",
        serde_json::json!({"passed_frames":12,"scope":"generated native scRGB composition before DWM, no OS display changes; logical HDR override is not desktop HDR acceptance","checks":*result_checked.lock().unwrap()})
    );
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
