//! Render shared production controls with generated data on an owned Xvfb.
#[cfg(target_os = "linux")]
#[path = "../../../tools/native/owned_display.rs"]
mod owned_display;
use uu_plugin_sdk::theme;
use eframe::egui;

struct Preview {
    output: std::path::PathBuf,
    frame: usize,
    target: Option<egui::Rect>,
    state: String,
    saved: std::sync::Arc<std::sync::atomic::AtomicBool>,
    address: String,
    enabled: bool,
}
impl eframe::App for Preview {
    fn raw_input_hook(&mut self, _: &egui::Context, input: &mut egui::RawInput) {
        input.focused = true;
        if let Some(rect) = self.target.filter(|_| self.state != "default") {
            input.events.push(egui::Event::PointerMoved(rect.center()));
            if self.state == "pressed" && self.frame == 3 {
                input.events.push(egui::Event::PointerButton {
                    pos: rect.center(),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                });
            }
        }
    }
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        for event in ctx.input(|input| input.events.clone()) {
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
        egui::CentralPanel::default().frame(egui::Frame::none().fill(theme::palette(&ctx.style().visuals).base).inner_margin(theme::SPACE_6))
            .show(ctx,|ui| {
                let p=*theme::palette(ui.visuals());
                ui.horizontal(|ui|{
                    ui.heading("Eclipse");
                    ui.label(egui::RichText::new("Design system").size(theme::FONT_SECTION).color(p.muted));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{
                        theme::pill(ui,if p.base==theme::DARK.base{"DARK"}else{"LIGHT"},theme::Tone::Info);
                    });
                });
                ui.label(egui::RichText::new("Generated interface fixture · no desktop capture or remote connection").size(theme::FONT_META).color(p.muted));
                ui.add_space(theme::SPACE_4);
                theme::card(ui.visuals()).show(ui,|ui|{
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui|{
                        ui.label(egui::RichText::new("Studio desktop").size(theme::FONT_SECTION).strong());
                        theme::pill(ui,"Online",theme::Tone::Success);
                        theme::pill(ui,"AV1 · SDR",theme::Tone::Neutral);
                    });
                    ui.label(egui::RichText::new("1920 × 1080 · 60 Hz · paired device").color(p.muted));
                    ui.add_space(theme::SPACE_2);
                    ui.horizontal(|ui|{
                        self.target=Some(ui.add(theme::primary_button("Start streaming")).rect);
                        ui.add(theme::secondary_button("Device settings"));
                        ui.add(theme::ghost_button("Details"));
                    });
                });
                ui.add_space(theme::SPACE_4);
                ui.columns(2,|columns| {
                    theme::card(columns[0].visuals()).show(&mut columns[0],|ui|{
                        ui.set_width(ui.available_width());
                        ui.label(egui::RichText::new("Connection settings").size(theme::FONT_SECTION).strong());
                        ui.label(egui::RichText::new("A common form with native egui controls").size(theme::FONT_META).color(p.muted));
                        ui.add_space(theme::SPACE_2);
                        ui.label("Computer address");
                        ui.add(egui::TextEdit::singleline(&mut self.address).desired_width(f32::INFINITY));
                        ui.checkbox(&mut self.enabled,"Allow remote input");
                        egui::ComboBox::from_id_salt("fixture-codec").selected_text("Automatic codec").show_ui(ui,|ui|{ui.label("AV1 / HEVC / H.264");});
                        ui.add_space(theme::SPACE_2);
                        ui.horizontal(|ui|{ui.add(theme::primary_button("Save settings"));ui.add(theme::ghost_button("Cancel"));});
                    });
                    theme::card(columns[1].visuals()).show(&mut columns[1],|ui|{
                        ui.set_width(ui.available_width());
                        ui.label(egui::RichText::new("Stream status").size(theme::FONT_SECTION).strong());
                        ui.label(egui::RichText::new("Meaning is expressed in text as well as color").size(theme::FONT_META).color(p.muted));
                        ui.add_space(theme::SPACE_2);
                        ui.horizontal_wrapped(|ui|{
                            theme::pill(ui,"RTT 4.2 ms",theme::Tone::Success);
                            theme::pill(ui,"Bitrate 20 Mb/s",theme::Tone::Info);
                            theme::pill(ui,"Loss 1.8%",theme::Tone::Warning);
                            theme::pill(ui,"Disconnected",theme::Tone::Danger);
                        });
                        ui.add_space(theme::SPACE_3);
                        ui.label(egui::RichText::new("Disabled actions").color(p.muted));
                        ui.horizontal_wrapped(|ui|{
                            ui.add_enabled(false,theme::primary_button("Connect"));
                            ui.add_enabled(false,theme::secondary_button("Edit"));
                            ui.add_enabled(false,theme::ghost_button("More"));
                        });
                    });
                });
                ui.add_space(theme::SPACE_4);
                egui::Frame::none().fill(p.overlay).stroke(egui::Stroke::new(1.,p.border)).rounding(theme::RADIUS).inner_margin(theme::SPACE_4).show(ui,|ui|{
                    ui.set_width(ui.available_width());
                    ui.label(egui::RichText::new("Overlay surface").size(theme::FONT_SECTION).strong());
                    ui.label(egui::RichText::new("8 px radius  /  4 · 8 · 12 · 16 · 24 spacing  /  24 · 16 · 14 · 12 typography").color(p.muted));
                });
                ui.add_space(theme::SPACE_3);
                ui.label(egui::RichText::new(format!("Primary interaction: {}",self.state)).size(theme::FONT_META).color(p.muted));
            });
        if self.frame == 9 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
        }
        self.frame += 1;
        ctx.request_repaint();
    }
}
fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let value = |key: &str| {
        args.iter()
            .position(|arg| arg == key)
            .and_then(|i| args.get(i + 1))
    };
    #[cfg(target_os = "linux")]
    owned_display::check(
        value("--x11-display").ok_or_else(|| anyhow::anyhow!("owned Xvfb required"))?,
    )?;
    #[cfg(not(target_os = "linux"))]
    anyhow::bail!("generated preview requires owned Xvfb");
    let output = std::path::PathBuf::from(
        value("--output").ok_or_else(|| anyhow::anyhow!("--output required"))?,
    );
    anyhow::ensure!(!output.exists(), "refuse to overwrite generated screenshot");
    let state = value("--state")
        .cloned()
        .unwrap_or_else(|| "default".into());
    anyhow::ensure!(
        ["default", "hover", "pressed"].contains(&state.as_str()),
        "invalid generated interaction state"
    );
    let light = args.iter().any(|arg| arg == "--light");
    let saved = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let check = saved.clone();
    eframe::run_native(
        "Eclipse design preview",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([1000., 680.]),
            vsync: false,
            ..Default::default()
        },
        Box::new(move |cc| {
            cc.egui_ctx.set_theme(if light {
                egui::Theme::Light
            } else {
                egui::Theme::Dark
            });
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(Preview {
                output,
                frame: 0,
                target: None,
                state,
                saved,
                address: "192.0.2.10".into(),
                enabled: true,
            }))
        }),
    )
    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    anyhow::ensure!(
        check.load(std::sync::atomic::Ordering::Acquire),
        "generated screenshot missing"
    );
    println!(r#"{{"window":"passed","generated":true}}"#);
    Ok(())
}
