//! Compare actual DMA-BUF rendering with CPU plane upload in an owned Sway.
//! Reads only generated test-video pixels from this fixture's own framebuffer.
#[cfg(target_os = "linux")]
#[allow(dead_code)]
#[path = "../src/video_texture.rs"]
mod video_texture;
#[cfg(target_os = "linux")]
mod linux {
    use super::video_texture;
    use eframe::{
        egui,
        glow::{self, HasContext},
    };
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex},
    };
    use uu_core::display_frames::{dma_buf, DisplayImage, YuvColor};
    use uu_media::{EncodedFrame, RawFrame, Timestamp, VideoDecoder};
    #[derive(Default)]
    struct ResultState {
        checked: bool,
        pixels: Vec<u8>,
        pairs: usize,
        error: Option<String>,
    }
    struct Probe {
        warmup_until: std::time::Instant,
        gl: Arc<glow::Context>,
        texture: Option<video_texture::VideoTexture>,
        state: Arc<Mutex<ResultState>>,
        encoded: Vec<EncodedFrame>,
        cpu: Box<dyn VideoDecoder>,
        gpu: Box<dyn VideoDecoder>,
        reference: Option<RawFrame>,
        index: usize,
        phase: u8,
    }
    impl eframe::App for Probe {
        fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
            if std::time::Instant::now() < self.warmup_until {
                ctx.request_repaint_after(std::time::Duration::from_millis(30));
                return;
            }
            if self.state.lock().unwrap().error.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            if self.state.lock().unwrap().checked {
                self.state.lock().unwrap().checked = false;
                if self.phase == 1 {
                    if self.texture.as_ref().unwrap().render_path() != "dma-buf-egl" {
                        self.state.lock().unwrap().error =
                            Some("GPU import silently fell back".into());
                        return;
                    }
                    let reference = self.reference.take().unwrap();
                    self.texture.as_mut().unwrap().set(
                        ctx,
                        DisplayImage::Decoded(0, reference, YuvColor::Limited709),
                    );
                    self.phase = 2;
                } else {
                    self.phase = 0;
                }
            }
            if self.phase == 0 {
                if self.index == self.encoded.len() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
                let encoded = &self.encoded[self.index];
                self.index += 1;
                dma_buf::set_rendering(false);
                let cpu = self.cpu.decode(encoded);
                dma_buf::set_rendering(true);
                let gpu = self.gpu.decode(encoded);
                match (cpu, gpu) {
                    (Ok(Some(cpu)), Ok(Some(gpu)))
                        if gpu.gpu
                            && cpu.width == gpu.width
                            && cpu.height == gpu.height
                            && cpu.pts == gpu.pts =>
                    {
                        self.reference = Some(cpu);
                        let image = DisplayImage::Decoded(0, gpu, YuvColor::Limited709);
                        match &mut self.texture {
                            Some(t) => t.set(ctx, image),
                            None => {
                                self.texture = Some(video_texture::VideoTexture::new(
                                    ctx,
                                    Some(&self.gl),
                                    image,
                                ))
                            }
                        };
                        self.phase = 1;
                    }
                    (Ok(None), Ok(None)) => {
                        ctx.request_repaint();
                        return;
                    }
                    _ => {
                        self.state.lock().unwrap().error = Some(format!(
                            "hardware output/layout mismatch at AU {}",
                            self.index - 1
                        ));
                        return;
                    }
                }
            }
            egui::CentralPanel::default()
                .frame(egui::Frame::none())
                .show(ctx, |ui| {
                    let rect = ui.available_rect_before_wrap();
                    self.texture.as_ref().unwrap().paint(ui, rect);
                    let state = self.state.clone();
                    let phase = self.phase;
                    ui.painter().add(egui::PaintCallback {
                        rect,
                        callback: Arc::new(eframe::egui_glow::CallbackFn::new(
                            move |info, painter| {
                                let mut result = state.lock().unwrap();
                                if result.checked {
                                    return;
                                }
                                let v = info.viewport_in_pixels();
                                let mut pixels =
                                    vec![0; v.width_px as usize * v.height_px as usize * 4];
                                unsafe {
                                    let gl = painter.gl();
                                    gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
                                    gl.read_pixels(
                                        v.left_px,
                                        v.from_bottom_px,
                                        v.width_px,
                                        v.height_px,
                                        glow::RGBA,
                                        glow::UNSIGNED_BYTE,
                                        glow::PixelPackData::Slice(&mut pixels),
                                    );
                                    if gl.get_error() != glow::NO_ERROR {
                                        result.error = Some("OpenGL error".into());
                                    }
                                }
                                if phase == 1 {
                                    result.pixels = pixels;
                                } else {
                                    let max = pixels
                                        .iter()
                                        .zip(&result.pixels)
                                        .map(|(a, b)| a.abs_diff(*b))
                                        .max()
                                        .unwrap_or(255);
                                    let sum:u64=pixels.iter().zip(&result.pixels).map(|(a,b)|a.abs_diff(*b) as u64).sum();
                                    let mean=sum as f64/pixels.len().max(1) as f64;
                                    let above2=pixels.iter().zip(&result.pixels).filter(|(a,b)|a.abs_diff(**b)>2).count();
                                    if pixels.len() != result.pixels.len() || max > 2 {
                                        result.error = Some(format!(
                                            "GPU/CPU rendered pixels differ: max={max}, mean={mean}, above2={above2}, actual_bytes={}, reference_bytes={}, frame={}",pixels.len(),result.pixels.len(),result.pairs
                                        ));
                                    }
                                    result.pairs += 1;
                                }
                                result.checked = true;
                            },
                        )),
                    });
                });
            ctx.request_repaint();
        }
        fn on_exit(&mut self, _: Option<&glow::Context>) {
            self.texture = None;
        }
    }
    pub fn main() -> anyhow::Result<()> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let mut args = std::env::args().skip(1);
        anyhow::ensure!(
            args.next().as_deref() == Some("--owned-wayland"),
            "owned Wayland required"
        );
        let path = PathBuf::from(
            args.next()
                .ok_or_else(|| anyhow::anyhow!("generated AUD file required"))?,
        );
        anyhow::ensure!(args.next().is_none(), "unexpected argument");
        let runtime = PathBuf::from(
            std::env::var_os("XDG_RUNTIME_DIR")
                .ok_or_else(|| anyhow::anyhow!("owned runtime required"))?,
        );
        let meta = std::fs::metadata(&runtime)?;
        anyhow::ensure!(
            runtime
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("uu-owned-gpu-")
                && meta.permissions().mode() & 0o077 == 0
                && meta.uid() == unsafe { libc::geteuid() },
            "private fixture runtime required"
        );
        let pid: u32 = std::env::var("UU_GPU_TEST_SWAY_PID")?.parse()?;
        anyhow::ensure!(
            std::fs::read_to_string(format!("/proc/{pid}/comm"))?.trim() == "sway",
            "owned compositor required"
        );
        let data = std::fs::read(path)?;
        anyhow::ensure!(
            data.len() <= 64 * 1024 * 1024,
            "generated fixture too large"
        );
        let mut starts = Vec::new();
        let mut i = 0;
        while i + 3 < data.len() {
            let n = if data[i..].starts_with(&[0, 0, 0, 1]) {
                4
            } else if data[i..].starts_with(&[0, 0, 1]) {
                3
            } else {
                i += 1;
                continue;
            };
            if data.get(i + n).is_some_and(|v| v & 31 == 9) {
                starts.push(i);
            }
            i += n;
        }
        anyhow::ensure!(!starts.is_empty(), "AUD fixture required");
        starts[0] = 0;
        starts.push(data.len());
        let encoded: Vec<_> = starts
            .windows(2)
            .enumerate()
            .map(|(i, p)| {
                let data: Vec<u8> = data[p[0]..p[1]].to_vec();
                EncodedFrame {
                    is_keyframe: EncodedFrame::detect_h264_keyframe(&data),
                    data: data.into(),
                    pts: Timestamp(i as u64 * 16667),
                }
            })
            .collect();
        let expected_frames = encoded.len();
        let state = Arc::new(Mutex::new(ResultState::default()));
        let report = state.clone();
        eframe::run_native(
            "Owned DMA-BUF fixture",
            eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default().with_inner_size([512., 256.]),
                vsync: true,
                ..Default::default()
            },
            Box::new(move |cc| {
                let gl = cc
                    .gl
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("GL required"))?;
                video_texture::enable_gpu_video(&gl);
                if !dma_buf::rendering() {
                    return Err("EGL import unavailable".into());
                }
                let cpu = uu_media::default_decoder()?;
                let gpu = uu_media::default_decoder()?;
                if !cpu.backend_name().starts_with("vaapi")
                    || !gpu.backend_name().starts_with("vaapi")
                {
                    return Err("Actual VAAPI decoders required".into());
                }
                Ok(Box::new(Probe {
                    warmup_until: std::time::Instant::now() + std::time::Duration::from_millis(500),
                    gl,
                    texture: None,
                    state,
                    encoded,
                    cpu,
                    gpu,
                    reference: None,
                    index: 0,
                    phase: 0,
                }))
            }),
        )
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let result = report.lock().unwrap();
        anyhow::ensure!(
            result.error.is_none(),
            "{}",
            result.error.as_deref().unwrap_or("")
        );
        anyhow::ensure!(result.pairs == expected_frames, "missing hardware pairs");
        println!(
            "{}",
            serde_json::json!({"video":"passed","gpu_cpu_render_pairs":result.pairs,"all_framebuffer_pixels_compared":true,"real_screen_read":false,"input_sent":false,"audio_opened":false,"vsync":true})
        );
        Ok(())
    }
}
#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    linux::main()
}
#[cfg(not(target_os = "linux"))]
fn main() {}
