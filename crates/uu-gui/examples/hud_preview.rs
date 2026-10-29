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
    measured_fixture: bool,
    chart_tooltip: bool,
    state: hud_panel::DetailState,
}
impl eframe::App for Preview {
    fn raw_input_hook(&mut self,_:&egui::Context,input:&mut egui::RawInput) {
        if self.chart_tooltip {input.events.push(egui::Event::PointerMoved(egui::pos2(1040.,250.)));}
    }
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
            size: [1920, 1080],
            target_fps: 60,
            source_hz: Some(165),
            codec: "AV1 SDR".into(),
            mode: "CBR".into(),
            backend: "d3d11va".into(),
            fps: [Some(59.9), Some(59.4), Some(59.9)],
            bitrate: Some(12.5),
            rtt: Some([1.5, 0.39]),
            loss: Some(0.),
            unrecovered_loss: Some(0.),
            process: [Some(0.24), Some(6.7), Some(0.62), Some(2.9)],
            queues: [Some(0.01), Some(0.03), Some(1.5)],
            counters: hud_panel::Counters {
                lost: 0,
                nack: 24,
                rtx: 6,
                fec_packets: 12840,
                fec_recovered: 36,
                fec_bytes: 12840000,
                ..Default::default()
            },
            link:Some(uu_core::link_metrics::Snapshot{ping_ms:Some(1.),rx_mbps:Some(20.4),tx_mbps:Some(0.6)}),
            host_video:Some(uu_core::frame_trace::VideoLatency {encoder:"Intel / Media Foundation".into(),source_to_last_rtp:Some(uu_core::frame_trace::Distribution{samples:120,mean_us:9200,p50_us:9100,p95_us:12900,p99_us:14500,p999_us:14700,max_us:14700}),rtp_write:Some(uu_core::frame_trace::Distribution{samples:120,mean_us:300,p50_us:290,p95_us:500,p99_us:600,p999_us:650,max_us:650})}),
            statistics_coverage:[180.,280.,280.],
            statistics:std::array::from_fn(|i|hud_panel::Counters{nack:if i==0{4}else{9},rtx:3,fec_recovered:2,fec_packets:200,original_received:40_000,..Default::default()}),
            continuity_windows:std::array::from_fn(|_|Some(uu_core::frame_continuity::Snapshot{long_gaps:1,max_gap_ms:56.9,threshold_ms:50.,ongoing_gap_ms:None,observed_seconds:180.})),
            input_ready: true,
            input_counts: [1420, 0],
            input_timing: [Some(0.04), Some(2.2), Some(0.06), Some(0.03)],
            continuity:Some(uu_core::frame_continuity::Snapshot{long_gaps:1,max_gap_ms:56.9,threshold_ms:50.,ongoing_gap_ms:None,observed_seconds:60.}),
            video_e2e:Some(uu_core::video_e2e::Snapshot {
                generation:1,relative_drift_ppm:1000,
                measurement:self.measured_fixture.then_some(uu_core::video_e2e::Measurement {
                    samples:24,mean_us:26_910,p50_us:24_780,p95_us:34_190,uncertainty_us:5260,max_sample_uncertainty_us:8000,
                    method:uu_core::video_e2e::DisplayMethod::DxgiReceiptInterval,
                }),
                unknown:(!self.measured_fixture).then_some(uu_core::video_e2e::UnknownReason::DisplayTimestampUnavailable),
                source_frames:120,displayed_frames:if self.measured_fixture{24}else{0},observations_dropped:0,history_evicted:0,
            }),
            history: (0..=360).map(|n| {let w=(n as f32/15.).sin();hud_panel::HistorySample {seconds:n as f64/2.,values:[self.measured_fixture.then_some(26.91+w*3.),Some(0.24),Some(6.7+w*0.2),Some(0.62),Some(2.9),Some(0.75+w*0.1),Some(1.54)],video_uncertainty_ms:self.measured_fixture.then_some(5.26)}}).collect(),
            toggle_hint: "Ctrl+Alt+S · Toggle HUD".into(),
            ..Default::default()
        };
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(egui::Color32::from_rgb(42, 49, 62)))
            .show(ctx, |ui| {
                let view = ui.max_rect();
                let screen = egui::Rect::from_min_max(view.min,view.max-egui::vec2(0.,22.));
                ui.painter().text(view.left_bottom()-egui::vec2(-8.,2.),egui::Align2::LEFT_BOTTOM,uu_i18n::text("生成数据预览 · 非实测"),egui::FontId::proportional(12.),egui::Color32::from_gray(200));
                let response =
                    hud_panel::Layout::new(ui, &m, HudDetail::Compact, screen.width() - 32.)
                        .paint(ui, screen, uu_core::client_settings::HudAppearance::default());
                self.bounds = Some(hud_panel::details_at_with_state(
                    ui,
                    &m,
                    screen,
                    response.rect,
                    self.bounds,
                    &mut self.state,
                    uu_core::client_settings::HudAppearance::default(),
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
    if args.iter().any(|a|a=="--english"){uu_i18n::set_language(uu_i18n::Language::English);}
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
                        .insert(0,"cjk".into());
                }
            }
            cc.egui_ctx.set_fonts(fonts);
            Ok(Box::new(Preview {
                output,
                frames: 0,
                saved,
                bounds: None,
                measured_fixture:args.iter().any(|arg|arg=="--measured-fixture"),
                chart_tooltip:args.iter().any(|arg|arg=="--chart-tooltip"),
                state:Default::default(),
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
