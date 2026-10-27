//! Render the production HUD on a private Xvfb, using generated metrics only.
#[allow(dead_code)]
#[path = "../src/hud_panel.rs"]
mod hud_panel;
#[cfg(target_os = "linux")]
#[path = "../../../tools/native/owned_display.rs"]
mod owned_display;
use eframe::egui;
use uu_core::client_settings::HudDetail;
struct Preview {
    output: std::path::PathBuf,
    frames: usize,
    saved: std::sync::Arc<std::sync::atomic::AtomicBool>,
    bounds: Option<egui::Rect>,
}
impl eframe::App for Preview {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        for event in ctx.input(|i| i.events.clone()) {
            if let egui::Event::Screenshot { image, .. } = event {
                let bytes: Vec<_> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
                image::save_buffer(
                    &self.output,
                    &bytes,
                    image.size[0] as u32,
                    image.size[1] as u32,
                    image::ColorType::Rgba8,
                )
                .unwrap();
                self.saved.store(true, std::sync::atomic::Ordering::Release);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        let m = hud_panel::Metrics {
            size: [3840, 2160],
            target_fps: 120,
            source_hz: Some(144),
            codec: "AV1 10-bit HDR".into(),
            mode: "CBR".into(),
            backend: "d3d11va".into(),
            fps: [Some(119.9), Some(119.8), Some(119.7)],
            bitrate: Some(37.2),
            rtt: Some([2.1, 0.6]),
            loss: Some(0.42),
            unrecovered_loss: Some(0.),
            process: [Some(0.8), Some(1.1), Some(0.32), Some(0.32)],
            queues: [Some(0.04), Some(0.1), Some(0.2)],
            counters: hud_panel::Counters {
                lost: 0,
                nack: 24,
                rtx: 6,
                fec_packets: 12840,
                fec_recovered: 36,
                fec_bytes: 12840000,
                ..Default::default()
            },
            input_ready: true,
            input_counts: [1420, 0],
            input_timing: [Some(0.04), Some(2.2), Some(0.06), Some(0.03)],
            history: (0..=360).map(|n| {let w=(n as f32/15.).sin();hud_panel::HistorySample {seconds:n as f64/2.,values:[Some(5.+w),Some(0.8),Some(1.1+w*0.2),Some(0.32),Some(0.32),Some(1.05+w*0.1),Some(0.34)]}}).collect(),
            toggle_hint: "Ctrl+Alt+S · Toggle HUD".into(),
            ..Default::default()
        };
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(egui::Color32::from_rgb(42, 49, 62)))
            .show(ctx, |ui| {
                let screen = ui.max_rect();
                let response =
                    hud_panel::Layout::new(ui, &m, HudDetail::Compact, screen.width() - 32.)
                        .paint(ui, screen);
                self.bounds = Some(hud_panel::details_at(
                    ui,
                    &m,
                    screen,
                    response.rect,
                    self.bounds,
                ));
            });

        if self.frames == 6 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
        }
        self.frames += 1;
        ctx.request_repaint();
    }
}
fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    #[cfg(target_os = "linux")]
    {
        let index = args
            .iter()
            .position(|s| s == "--x11-display")
            .ok_or_else(|| anyhow::anyhow!("owned Xvfb required"))?;
        owned_display::check(
            args.get(index + 1)
                .ok_or_else(|| anyhow::anyhow!("missing display"))?,
        )?;
    }
    #[cfg(not(target_os = "linux"))]
    anyhow::bail!("this screenshot probe requires owned Xvfb");
    let index = args
        .iter()
        .position(|s| s == "--output")
        .ok_or_else(|| anyhow::anyhow!("--output required"))?;
    let output = std::path::PathBuf::from(
        args.get(index + 1)
            .ok_or_else(|| anyhow::anyhow!("missing output"))?,
    );
    anyhow::ensure!(!output.exists(), "refuse to overwrite screenshot");
    let saved = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let check = saved.clone();
    let size = if args.iter().any(|a| a == "--narrow") {
        [640., 360.]
    } else {
        [1280., 720.]
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size(size),
        vsync: false,
        ..Default::default()
    };
    eframe::run_native(
        "HUD preview",
        options,
        Box::new(move |cc| {
            let mut fonts = egui::FontDefinitions::default();
            if let Ok(result) = std::process::Command::new("fc-match")
                .args(["-f", "%{file}", "sans-serif:lang=zh-cn"])
                .output()
            {
                if let Ok(data) = std::fs::read(String::from_utf8_lossy(&result.stdout).trim()) {
                    fonts
                        .font_data
                        .insert("cjk".into(), egui::FontData::from_owned(data));
                    fonts
                        .families
                        .get_mut(&egui::FontFamily::Proportional)
                        .unwrap()
                        .push("cjk".into());
                }
            }
            cc.egui_ctx.set_fonts(fonts);
            Ok(Box::new(Preview {
                output,
                frames: 0,
                saved,
                bounds: None,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    anyhow::ensure!(
        check.load(std::sync::atomic::Ordering::Acquire),
        "screenshot not saved"
    );
    println!("{{\"video\":\"passed\",\"fixture\":\"generated HUD on owned Xvfb\"}}");
    Ok(())
}
