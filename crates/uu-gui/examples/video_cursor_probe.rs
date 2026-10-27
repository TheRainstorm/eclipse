//! Generated pointer pixels only, on a fresh owned display/window station.
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
};
use std::sync::{Arc, Mutex};
use uu_core::{
    display_frames::{DisplayImage, PixelFormat, RawFrame, Timestamp, YuvColor},
    native_cursor::{Description, Shape, Snapshot},
};

#[derive(Clone)]
struct Case {
    state: Option<Snapshot>,
    points: Vec<([f32; 2], [u8; 4])>,
    desktop: [u32; 2],
}
struct Probe {
    texture: video_texture::VideoTexture,
    cases: Vec<Case>,
    index: usize,
    pending: bool,
    checked: Arc<Mutex<bool>>,
    result: Arc<Mutex<Result<usize, String>>>,
}
impl eframe::App for Probe {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.pending && *self.checked.lock().unwrap() {
            self.index += 1;
            self.pending = false;
        }
        if self.index == self.cases.len() || self.result.lock().unwrap().is_err() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if !self.pending {
            self.texture
                .set_native_cursor(self.cases[self.index].state.clone());
            *self.checked.lock().unwrap() = false;
            self.pending = true;
        }
        egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx,|ui| {
            let rect=ui.available_rect_before_wrap();self.texture.paint(ui,rect);
            let case=self.cases[self.index].clone();let checked=self.checked.clone();let result=self.result.clone();let index=self.index;
            ui.painter().add(egui::PaintCallback{rect,callback:Arc::new(eframe::egui_glow::CallbackFn::new(move|info,painter| {
                if *checked.lock().unwrap(){return;}
                let v=info.viewport_in_pixels();
                unsafe {
                    let gl=painter.gl();gl.pixel_store_i32(glow::PACK_ALIGNMENT,1);
                    for (point,expected) in &case.points {
                        let mut actual=[0u8;4];let x=((point[0]+0.5)/case.desktop[0] as f32*v.width_px as f32) as i32;
                        let y=v.height_px-1-((point[1]+0.5)/case.desktop[1] as f32*v.height_px as f32) as i32;
                        gl.read_pixels(v.left_px+x,v.from_bottom_px+y,1,1,glow::RGBA,glow::UNSIGNED_BYTE,glow::PixelPackData::Slice(&mut actual));
                        if actual.iter().zip(expected).any(|(a,b)|a.abs_diff(*b)>2){*result.lock().unwrap()=Err(format!("case {index} at {point:?}: {actual:?}, expected {expected:?}"));}
                    }
                    let error=gl.get_error();if error!=glow::NO_ERROR{*result.lock().unwrap()=Err(format!("GL error {error}"));}
                }
                if let Ok(count)=&mut *result.lock().unwrap(){*count+=1;}*checked.lock().unwrap()=true;
            }))});
        });
        ctx.request_repaint();
    }
}
fn cases() -> Vec<Case> {
    let bg = [153, 102, 51, 255];
    let mut cases = Vec::new();
    let color = Shape {
        description: Description {
            kind: 2,
            width: 4,
            height: 1,
            pitch: 16,
            hotspot: [2, 0],
        },
        data: vec![
            0, 0, 255, 255, 0, 255, 0, 128, 255, 255, 255, 0, 255, 255, 255, 255,
        ],
    };
    let mono = Shape {
        description: Description {
            kind: 1,
            width: 4,
            height: 2,
            pitch: 2,
            hotspot: [2, 0],
        },
        data: vec![0b00110000, 93, 0b01010000, 24],
    };
    let masked = Shape {
        description: Description {
            kind: 4,
            width: 4,
            height: 1,
            pitch: 16,
            hotspot: [2, 0],
        },
        data: vec![
            0, 0, 255, 0, 255, 255, 255, 255, 0, 0, 0, 255, 0x33, 0x55, 0xaa, 255,
        ],
    };
    let expected = [
        vec![[255, 0, 0, 255], [76, 179, 25, 255], bg, [255; 4]],
        vec![[0, 0, 0, 255], [255; 4], bg, [102, 153, 204, 255]],
        vec![
            [255, 0, 0, 255],
            [102, 153, 204, 255],
            bg,
            [153 ^ 0xaa, 102 ^ 0x55, 51 ^ 0x33, 255],
        ],
    ];
    for (shape, pixels) in [color, mono, masked].into_iter().zip(expected) {
        for desktop in [[64, 32], [128, 64]] {
            let mut points: Vec<_> = pixels
                .iter()
                .enumerate()
                .map(|(x, p)| ([16. + x as f32, 8.], *p))
                .collect();
            points.push(([15., 8.], bg));
            points.push(([20., 8.], bg));
            points.push(([18., 7.], bg));
            let snapshot = Snapshot {
                revision: cases.len() as u64 + 1,
                shape_id: cases.len() as u64 + 1,
                position: Some([16, 8]),
                desktop,
                shape: Some(Arc::new(shape.clone())),
            };
            cases.push(Case {
                state: Some(snapshot.clone()),
                points,
                desktop,
            });
            cases.push(Case {
                state: Some(Snapshot {
                    position: None,
                    ..snapshot
                }),
                points: vec![([16., 8.], bg), ([19., 8.], bg)],
                desktop,
            });
        }
        let points = vec![([0., 0.], pixels[2]), ([1., 0.], pixels[3]), ([2., 0.], bg)];
        cases.push(Case {
            state: Some(Snapshot {
                revision: cases.len() as u64 + 1,
                shape_id: cases.len() as u64 + 1,
                position: Some([-2, 0]),
                desktop: [64, 32],
                shape: Some(Arc::new(shape)),
            }),
            points,
            desktop: [64, 32],
        });
        cases.push(Case {
            state: None,
            points: vec![([0., 0.], bg), ([17., 8.], bg)],
            desktop: [64, 32],
        });
    }
    cases
}
fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    #[cfg(target_os = "linux")]
    {
        anyhow::ensure!(
            args.len() >= 2 && args[0] == "--x11-display",
            "use tools/native/input_smoke.py"
        );
        owned_display::check(&args[1])?;
    }
    #[cfg(windows)]
    let _station = {
        anyhow::ensure!(
            args.iter().any(|a| a == "--isolated-window-station"),
            "requires owned station"
        );
        owned_station::Station::create()?
    };
    let rgba = args.iter().any(|a| a == "--rgba-reference");
    let cases = cases();
    let total = cases.len();
    let result = Arc::new(Mutex::new(Ok(0)));
    let report = result.clone();
    eframe::run_native(
        "Owned pointer renderer",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([256., 128.])
                .with_decorations(false),
            vsync: false,
            ..Default::default()
        },
        Box::new(move |cc| {
            let data = [51, 102, 153, 255].repeat(64 * 32);
            let image = if rgba {
                DisplayImage::Rgba((0, 64, 32, [153, 102, 51, 255].repeat(64 * 32)))
            } else {
                DisplayImage::Decoded(
                    0,
                    RawFrame {
                        width: 64,
                        height: 32,
                        stride: 256,
                        format: PixelFormat::Bgra8,
                        data: data.into(),
                        pts: Timestamp(0),
                        gpu: false,
                        dma_buf: None,
                        d3d11: None,
                    },
                    YuvColor::Full709,
                )
            };
            let texture = video_texture::VideoTexture::new(&cc.egui_ctx, cc.gl.as_ref(), image);
            Ok(Box::new(Probe {
                texture,
                cases,
                index: 0,
                pending: false,
                checked: Arc::new(Mutex::new(false)),
                result,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let count = report.lock().unwrap().clone().map_err(anyhow::Error::msg)?;
    anyhow::ensure!(count == total, "missing pointer cases");
    println!(
        "{}",
        serde_json::json!({"video":"passed","rendered_cases":count,"rgba_fallback":rgba,"real_screen_read":false,"input_sent":false})
    );
    Ok(())
}
