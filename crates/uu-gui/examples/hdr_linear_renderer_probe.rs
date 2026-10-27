//! Verify production video and egui shaders in an owned FP16 framebuffer.
//! Does not change a display's HDR mode or read another application's pixels.
#[cfg(target_os = "linux")]
#[path = "../../../tools/native/owned_display.rs"]
mod owned_display;
#[cfg(windows)]
#[path = "../../../tools/native/windows_owned_station.rs"]
mod owned_station;
#[allow(dead_code)]
#[path = "../src/video_texture.rs"]
mod video_texture;
#[cfg(windows)]
#[path = "../src/wgl_interop.rs"]
mod wgl_interop;
use eframe::{
    egui,
    glow::{self, HasContext},
    glow_presentation::{self, ColorTarget, Presenter},
};
use std::sync::{Arc, Mutex};
use uu_core::display_frames::{DisplayImage, PixelFormat, RawFrame, Timestamp, YuvColor};

struct FloatTarget {
    gl: Arc<glow::Context>,
    fbo: glow::Framebuffer,
    texture: glow::Texture,
    size: [u32; 2],
}
// Presenter requires Send for its registry; all calls and destruction use the
// owning current GL context on eframe's native thread.
unsafe impl Send for FloatTarget {}
impl Presenter for FloatTarget {
    fn begin(&mut self, size: [u32; 2]) -> Result<ColorTarget, String> {
        unsafe {
            self.gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            if self.size != size {
                self.gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA16F as i32,
                    size[0] as i32,
                    size[1] as i32,
                    0,
                    glow::RGBA,
                    glow::HALF_FLOAT,
                    None,
                );
                self.size = size;
            }
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            self.gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(self.texture),
                0,
            );
            if self.gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
                return Err("FP16 target incomplete".into());
            }
            Ok(ColorTarget {
                hdr: true,
                sdr_white_nits: 240.,
            })
        }
    }
    fn present(&mut self, _: bool) -> Result<(), String> {
        self.finish();
        Ok(())
    }
    fn finish(&mut self) {
        unsafe { self.gl.bind_framebuffer(glow::FRAMEBUFFER, None) }
    }
    fn deactivate(&mut self) -> Result<(), String> {
        self.finish();
        Ok(())
    }
}
impl Drop for FloatTarget {
    fn drop(&mut self) {
        unsafe {
            self.gl.delete_framebuffer(self.fbo);
            self.gl.delete_texture(self.texture);
        }
    }
}

struct Probe {
    ctx: egui::Context,
    texture: Option<video_texture::VideoTexture>,
    step: usize,
    checked: Arc<Mutex<Option<Result<(), String>>>>,
    results: Arc<Mutex<Vec<String>>>,
}
impl eframe::App for Probe {
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        [0., 0., 0., 1.]
    }
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        if let Some(result) = self.checked.lock().unwrap().take() {
            match result {
                Ok(()) => self
                    .results
                    .lock()
                    .unwrap()
                    .push(format!("step {} passed", self.step)),
                Err(e) => {
                    self.results.lock().unwrap().push(e);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
            }
            self.step += 1;
            if self.step < 4 {
                let (frame, color, _) = case(self.step);
                self.texture
                    .as_mut()
                    .unwrap()
                    .set(ctx, DisplayImage::Decoded(0, frame, color));
            }
        }
        if self.step >= 5 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        // Returning to the normal framebuffer must reset the shared painter's
        // encoding. This checks viewport/stream transitions without a new shader.
        glow_presentation::set_active(ctx, ctx.viewport_id(), self.step != 4, false);
        let step = self.step;
        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                let rect = ui.available_rect_before_wrap();
                let expected = if step < 3 {
                    self.texture.as_ref().unwrap().paint(ui, rect);
                    case(step).2
                } else {
                    let a = egui::Rect::from_min_max(rect.min, rect.center());
                    let b = egui::Rect::from_min_max(
                        egui::pos2(rect.center().x, rect.min.y),
                        egui::pos2(rect.max.x, rect.center().y),
                    );
                    let c = egui::Rect::from_min_max(
                        egui::pos2(rect.min.x, rect.center().y),
                        egui::pos2(rect.center().x, rect.max.y),
                    );
                    let d = egui::Rect::from_min_max(rect.center(), rect.max);
                    for (r, color) in [
                        (a, egui::Color32::WHITE),
                        (b, egui::Color32::from_rgba_unmultiplied(255, 0, 0, 128)),
                        (c, egui::Color32::from_gray(128)),
                        (d, egui::Color32::TRANSPARENT),
                    ] {
                        ui.painter().rect_filled(r, 0., color);
                    }
                    if step == 3 {
                        [
                            [3.; 3],
                            [3. * 128. / 255., 0., 0.],
                            [3. * linear(128. / 255.); 3],
                            [0.; 3],
                        ]
                    } else {
                        [
                            [1.; 3],
                            [
                                egui::Color32::from_rgba_unmultiplied(255, 0, 0, 128).r() as f32
                                    / 255.,
                                0.,
                                0.,
                            ],
                            [128. / 255.; 3],
                            [0.; 3],
                        ]
                    }
                };
                let checked = self.checked.clone();
                ui.painter().add(egui::PaintCallback {
                    rect,
                    callback: Arc::new(eframe::egui_glow::CallbackFn::new(move |info, painter| {
                        if checked.lock().unwrap().is_some() {
                            return;
                        }
                        let v = info.viewport_in_pixels();
                        let mut error = None;
                        for (index, (x, y)) in
                            [(1, 3), (3, 3), (1, 1), (3, 1)].into_iter().enumerate()
                        {
                            let mut bytes = [0u8; 16];
                            unsafe {
                                painter.gl().read_pixels(
                                    v.left_px + v.width_px * x / 4,
                                    v.from_bottom_px + v.height_px * y / 4,
                                    1,
                                    1,
                                    glow::RGBA,
                                    glow::FLOAT,
                                    glow::PixelPackData::Slice(&mut bytes),
                                );
                            }
                            let actual = std::array::from_fn::<_, 4, _>(|c| {
                                f32::from_ne_bytes(bytes[c * 4..c * 4 + 4].try_into().unwrap())
                            });
                            for c in 0..3 {
                                let tolerance = if step == 1 {
                                    expected[index][c].abs().max(1.) * 0.003
                                } else {
                                    0.006
                                };
                                if (actual[c] - expected[index][c]).abs() > tolerance {
                                    error = Some(format!(
                                        "step {step} patch {index}: {actual:?}, expected {:?}",
                                        expected[index]
                                    ));
                                }
                            }
                        }
                        let gl_error = unsafe { painter.gl().get_error() };
                        if gl_error != glow::NO_ERROR {
                            error = Some(format!("GL error {gl_error}"));
                        }
                        *checked.lock().unwrap() = Some(error.map_or(Ok(()), Err));
                    })),
                });
            });
        ctx.request_repaint();
    }
    fn on_exit(&mut self, _: Option<&glow::Context>) {
        self.texture = None;
        glow_presentation::remove(&self.ctx, self.ctx.viewport_id());
    }
}
fn linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
fn case(index: usize) -> (RawFrame, YuvColor, [[f32; 3]; 4]) {
    let (width, height) = (32u32, 32u32);
    let format = match index {
        0 => PixelFormat::ScRgb16,
        1 => PixelFormat::P010,
        _ => PixelFormat::Bgra8,
    };
    let stride = width
        * match format {
            PixelFormat::ScRgb16 => 8,
            PixelFormat::P010 => 2,
            _ => 4,
        };
    let mut data = vec![
        0u8;
        if format == PixelFormat::P010 {
            (stride * height * 3 / 2) as usize
        } else {
            (stride * height) as usize
        }
    ];
    let sc_words = [
        [0x4a40u16, 0x3400, 0xb000, 0x3c00],
        [0x3000, 0x4000, 0x3400, 0x3c00],
        [0x3400, 0x3000, 0x3c00, 0x3c00],
        [0x3800, 0x3800, 0x3800, 0x3c00],
    ];
    let mut expected = [
        [12.5, 0.25, -0.125],
        [0.125, 2., 0.25],
        [0.25, 0.125, 1.],
        [0.5; 3],
    ];
    let mut codes = [0u16; 4];
    for i in 0..4 {
        codes[i] = ((64. + 876. * uu_media::hdr::nits_to_pq([0., 203., 1000., 4000.][i])).round()
            as u16)
            << 6;
    }
    if index == 1 {
        expected = std::array::from_fn(|i| {
            uu_media::hdr::p010_to_sc_rgb(codes[i], 512 << 6, 512 << 6, false)
        });
    }
    if index >= 2 {
        expected = [[3., 0., 0.], [0., 3., 0.], [0., 0., 3.], [3.; 3]];
    }
    for y in 0..height {
        for x in 0..width {
            let q = (y >= height / 2) as usize * 2 + (x >= width / 2) as usize;
            let at = (y * stride) as usize;
            match format {
                PixelFormat::ScRgb16 => {
                    for c in 0..4 {
                        data[at + x as usize * 8 + c * 2..at + x as usize * 8 + c * 2 + 2]
                            .copy_from_slice(&sc_words[q][c].to_le_bytes());
                    }
                }
                PixelFormat::P010 => data[at + x as usize * 2..at + x as usize * 2 + 2]
                    .copy_from_slice(&codes[q].to_le_bytes()),
                _ => data[at + x as usize * 4..at + x as usize * 4 + 4].copy_from_slice(
                    &[
                        [0, 0, 255, 255],
                        [0, 255, 0, 255],
                        [255, 0, 0, 255],
                        [255; 4],
                    ][q],
                ),
            }
        }
    }
    if format == PixelFormat::P010 {
        for pair in data[(stride * height) as usize..].chunks_exact_mut(2) {
            pair.copy_from_slice(&(512u16 << 6).to_le_bytes());
        }
    }
    (
        RawFrame {
            width,
            height,
            format,
            stride,
            data: data.into(),
            pts: Timestamp(0),
            gpu: false,
            dma_buf: None,
            d3d11: None,
        },
        if index < 2 {
            YuvColor::Limited2020Pq
        } else {
            YuvColor::Full709
        },
        expected,
    )
}
fn main() -> anyhow::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    #[cfg(target_os = "linux")]
    {
        anyhow::ensure!(
            args.len() == 2 && args[0] == "--x11-display",
            "explicit owned Xvfb required"
        );
        owned_display::check(&args[1])?;
    }
    #[cfg(windows)]
    let _station = {
        anyhow::ensure!(
            args == ["--isolated-window-station"],
            "explicit isolated station required"
        );
        owned_station::Station::create()?
    };
    let results = Arc::new(Mutex::new(Vec::new()));
    let report = results.clone();
    eframe::run_native(
        "Owned FP16 shader fixture",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([192., 192.])
                .with_decorations(false),
            vsync: false,
            ..Default::default()
        },
        Box::new(move |cc| {
            let gl = cc.gl.as_ref().unwrap().clone();
            let target = unsafe {
                FloatTarget {
                    fbo: gl.create_framebuffer()?,
                    texture: gl.create_texture()?,
                    gl,
                    size: [0, 0],
                }
            };
            glow_presentation::install(
                &cc.egui_ctx,
                Box::new(target),
                false,
                ColorTarget {
                    hdr: true,
                    sdr_white_nits: 240.,
                },
            );
            let (frame, color, _) = case(0);
            let texture = video_texture::VideoTexture::new(
                &cc.egui_ctx,
                cc.gl.as_ref(),
                DisplayImage::Decoded(0, frame, color),
            );
            Ok(Box::new(Probe {
                ctx: cc.egui_ctx.clone(),
                texture: Some(texture),
                step: 0,
                checked: Arc::new(Mutex::new(None)),
                results,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let results = report.lock().unwrap();
    println!(
        "{}",
        serde_json::json!({"scope":"owned FP16 only","video":"passed","results":*results})
    );
    anyhow::ensure!(
        results.len() == 5 && results.iter().all(|v| v.ends_with("passed")),
        "FP16 pipeline failed"
    );
    Ok(())
}
