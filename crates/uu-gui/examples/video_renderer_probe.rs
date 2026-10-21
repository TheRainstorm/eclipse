//! Generated planes on an explicitly owned Xvfb; read only our GL framebuffer.
#[cfg(target_os = "linux")]
#[path = "../../../tools/native/owned_display.rs"]
mod owned_display;
#[cfg(windows)]
#[path = "../../../tools/native/windows_owned_station.rs"]
mod owned_station;
#[allow(dead_code)]
#[path = "../src/video_texture.rs"]
mod video_texture;
use eframe::{
    egui,
    glow::{self, HasContext},
};
use std::sync::{Arc, Mutex};
use uu_core::display_frames::{DisplayImage, PixelFormat, RawFrame, Timestamp, YuvColor};

struct Probe {
    texture: Option<video_texture::VideoTexture>,
    cases: Vec<(RawFrame, YuvColor)>,
    result: Arc<Mutex<Result<usize, String>>>,
    checked: Arc<Mutex<bool>>,
    index: usize,
    pending: bool,
    benchmark: bool,
    rgba: bool,
    timings: Arc<Mutex<Vec<(f32, f32)>>>,
    previous_conversion: f32,
}
impl eframe::App for Probe {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if self.benchmark && self.index >= 30 && self.pending {
            if let Some(seconds) = frame.info().cpu_usage {
                self.timings
                    .lock()
                    .unwrap()
                    .push((seconds * 1000., self.previous_conversion));
            }
        }
        if self.pending && *self.checked.lock().unwrap() {
            self.index += 1;
            self.pending = false;
        }
        if self.index == self.cases.len() || self.result.lock().unwrap().is_err() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if !self.pending {
            let (f, c) = self.cases[self.index].clone();
            let started = std::time::Instant::now();
            let image = DisplayImage::Decoded(0, f, c);
            let image = if self.rgba {
                DisplayImage::Rgba(image.into_rgba())
            } else {
                image
            };
            self.previous_conversion = if self.rgba {
                started.elapsed().as_secs_f32() * 1000.
            } else {
                0.
            };
            self.texture.as_mut().unwrap().set(ctx, image);
            if !self.rgba && !self.texture.as_ref().unwrap().uses_planar_renderer() {
                *self.result.lock().unwrap() = Err("GPU renderer silently fell back".into());
                return;
            }
            *self.checked.lock().unwrap() = false;
            self.pending = true;
        }
        let (frame, color) = &self.cases[self.index];
        let reference = if self.benchmark {
            Vec::new()
        } else {
            DisplayImage::Decoded(0, frame.clone(), *color)
                .into_rgba()
                .3
        };
        let (w, h) = (frame.width as usize, frame.height as usize);
        let expected: Vec<[u8; 4]> = if self.benchmark {
            Vec::new()
        } else {
            [
                (w / 4, h / 4),
                (w * 3 / 4, h / 4),
                (w / 4, h * 3 / 4),
                (w * 3 / 4, h * 3 / 4),
            ]
            .iter()
            .map(|&(x, y)| {
                reference[(y * w + x) * 4..(y * w + x) * 4 + 4]
                    .try_into()
                    .unwrap()
            })
            .collect()
        };
        egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx,|ui|{
            let rect=ui.available_rect_before_wrap();self.texture.as_ref().unwrap().paint(ui,rect);
            let result=self.result.clone();let checked=self.checked.clone();
            ui.painter().add(egui::PaintCallback {rect,callback:Arc::new(eframe::egui_glow::CallbackFn::new(move |info,painter|{
                if *checked.lock().unwrap() {return;}
                let viewport=info.viewport_in_pixels();
                let positions=[(1,3),(3,3),(1,1),(3,1)];
                unsafe {
                    let gl=painter.gl();gl.pixel_store_i32(glow::PACK_ALIGNMENT,1);
                    for ((x,y),expected) in positions.iter().zip(&expected) {
                        let mut pixel=[0u8;4];
                        gl.read_pixels(viewport.left_px+viewport.width_px*x/4,viewport.from_bottom_px+viewport.height_px*y/4,1,1,glow::RGBA,glow::UNSIGNED_BYTE,glow::PixelPackData::Slice(&mut pixel));
                        if pixel.iter().zip(expected).any(|(a,b)|a.abs_diff(*b)>2) {*result.lock().unwrap()=Err(format!("plane color/orientation mismatch: {pixel:?} expected {expected:?}"));}
                    }
                    let error=gl.get_error();if error!=glow::NO_ERROR {*result.lock().unwrap()=Err(format!("OpenGL error {error}"));}
                }
                if let Ok(count)=&mut *result.lock().unwrap() {*count+=1;}
                *checked.lock().unwrap()=true;
            }))});
        });
        ctx.request_repaint();
    }
    fn on_exit(&mut self, _: Option<&glow::Context>) {
        self.texture = None;
    }
}
fn cases() -> Vec<(RawFrame, YuvColor)> {
    let mut cases = Vec::new();
    // Change layouts and dimensions on the same texture, then reuse each allocation.
    for (w, h) in [(16, 8), (8, 16)] {
        for format in [PixelFormat::Nv12, PixelFormat::I420, PixelFormat::Bgra8] {
            for color in [
                YuvColor::Full601,
                YuvColor::Full709,
                YuvColor::Limited601,
                YuvColor::Limited709,
            ] {
                let stride = if format == PixelFormat::Bgra8 {
                    w * 4 + 16
                } else {
                    w + 8
                };
                let y = (stride * h) as usize;
                let mut data =
                    vec![99; y + (w * h) as usize / 2 + stride as usize * h as usize / 2];
                if format == PixelFormat::Bgra8 {
                    data.resize(y, 99);
                }
                for row in 0..h as usize {
                    for col in 0..w as usize {
                        let quadrant =
                            (row >= h as usize / 2) as usize * 2 + (col >= w as usize / 2) as usize;
                        if format == PixelFormat::Bgra8 {
                            data[row * stride as usize + col * 4
                                ..row * stride as usize + col * 4 + 4]
                                .copy_from_slice(
                                    &[
                                        [20, 30, 220, 0],
                                        [20, 220, 30, 17],
                                        [220, 30, 20, 128],
                                        [220, 220, 220, 255],
                                    ][quadrant],
                                );
                        } else {
                            data[row * stride as usize + col] = [81, 145, 41, 235][quadrant];
                        }
                    }
                }
                if format != PixelFormat::Bgra8 {
                    for row in 0..h as usize / 2 {
                        for col in 0..w as usize / 2 {
                            let q = (row >= h as usize / 4) as usize * 2
                                + (col >= w as usize / 4) as usize;
                            let (u, v) = [(90, 240), (54, 34), (240, 110), (128, 128)][q];
                            if format == PixelFormat::Nv12 {
                                let i = y + row * stride as usize + col * 2;
                                data[i] = u;
                                data[i + 1] = v;
                            } else {
                                let i = row * w as usize / 2 + col;
                                data[y + i] = u;
                                data[y + (w * h / 4) as usize + i] = v;
                            }
                        }
                    }
                }
                let frame = RawFrame {
                    width: w,
                    height: h,
                    format,
                    stride,
                    data: data.into(),
                    pts: Timestamp(0),
                    gpu: false,
                    dma_buf: None,
                    d3d11: None,
                };
                cases.push((frame.clone(), color));
                cases.push((frame, color));
            }
        }
    }
    cases
}
fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let benchmark = args.iter().any(|s| s == "--benchmark");
    let rgba = args.iter().any(|s| s == "--rgba-reference");
    #[cfg(target_os = "linux")]
    {
        anyhow::ensure!(
            args.len() >= 2 && args[0] == "--x11-display",
            "explicit owned display required"
        );
        owned_display::check(&args[1])?;
        anyhow::ensure!(
            args[2..]
                .iter()
                .all(|s| s == "--benchmark" || s == "--rgba-reference"),
            "unexpected probe argument"
        );
    }
    #[cfg(windows)]
    let _station = {
        anyhow::ensure!(
            args.first().map(String::as_str) == Some("--isolated-window-station")
                && args[1..]
                    .iter()
                    .all(|s| s == "--benchmark" || s == "--rgba-reference"),
            "explicit isolated station required"
        );
        owned_station::Station::create()?
    };
    let cases = if benchmark {
        let (w, h) = (1920, 1080);
        let mut data = vec![100; (w * h * 3 / 2) as usize];
        data[(w * h) as usize..].fill(128);
        let f = RawFrame {
            width: w,
            height: h,
            format: PixelFormat::Nv12,
            stride: w,
            data: data.into(),
            pts: Timestamp(0),
            gpu: false,
            dma_buf: None,
            d3d11: None,
        };
        vec![(f, YuvColor::Limited709); 150]
    } else {
        cases()
    };
    let count = cases.len();
    let result = Arc::new(Mutex::new(Ok(0usize)));
    let report = result.clone();
    let timings = Arc::new(Mutex::new(Vec::new()));
    let output_timings = timings.clone();
    eframe::run_native(
        "Owned planar renderer fixture",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([192., 192.])
                .with_decorations(false),
            vsync: true,
            ..Default::default()
        },
        Box::new(move |cc| {
            let texture = video_texture::VideoTexture::new(
                &cc.egui_ctx,
                cc.gl.as_ref(),
                DisplayImage::Decoded(0, cases[0].0.clone(), cases[0].1),
            );
            Ok(Box::new(Probe {
                texture: Some(texture),
                cases,
                result,
                checked: Arc::new(Mutex::new(false)),
                index: 0,
                pending: false,
                benchmark,
                rgba,
                timings,
                previous_conversion: 0.,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let checked = report
        .lock()
        .unwrap()
        .clone()
        .map_err(|e| anyhow::anyhow!(e))?;
    anyhow::ensure!(checked == count, "missing rendered cases");
    let times = output_timings.lock().unwrap();
    let n = times.len().max(1) as f32;
    let cpu_ms = times.iter().map(|p| p.0).sum::<f32>() / n;
    let conversion_ms = times.iter().map(|p| p.1).sum::<f32>() / n;
    println!(
        "{}",
        serde_json::json!({"benchmark":benchmark,"rgba_reference":rgba,"samples":times.len(),"pixel_pipeline_cpu_ms":cpu_ms,"cpu_color_conversion_ms":conversion_ms,"ui_and_submit_ms":(cpu_ms-conversion_ms).max(0.),"video":"passed","rendered_cases":checked,"layouts":if benchmark {1}else{3},"color_matrices":if benchmark {1}else{4},"padded_rows":!benchmark,"resolution_changes":!benchmark,"allocation_reuse":true,"vsync":true,"real_screen_read":false})
    );
    Ok(())
}
