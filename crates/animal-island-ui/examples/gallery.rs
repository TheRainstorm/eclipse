//! Interactive native gallery. Optional --screenshot PATH captures this window.
use animal_island_ui::{
    self as island, egui, Button, ButtonKind, Card, Input, Ribbon, Select, Switch, Tag,
};
use island::containers::Tone;
struct Gallery {
    name: String,
    enabled: bool,
    quality: String,
    night: bool,
    saved: bool,
    output: Option<std::path::PathBuf>,
    frames: usize,
    started: std::time::Instant,
}
impl eframe::App for Gallery {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        for event in ctx.input(|i| i.events.clone()) {
            if let egui::Event::Screenshot { image, .. } = event {
                if let Some(path) = &self.output {
                    let pixels: Vec<_> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
                    image::save_buffer(
                        path,
                        &pixels,
                        image.size[0] as u32,
                        image.size[1] as u32,
                        image::ColorType::Rgba8,
                    )
                    .expect("save own gallery screenshot");
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        egui::CentralPanel::default().frame(egui::Frame::none().fill(ctx.style().visuals.panel_fill).inner_margin(28.0)).show(ctx,|ui| {
            ui.horizontal(|ui| {
                ui.add(Ribbon::new("Animal Island UI"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui| {
                    if ui.add(Switch::new(&mut self.night,"Warm night")).changed() {
                        ctx.set_theme(if self.night {egui::Theme::Dark}else{egui::Theme::Light});
                    }
                });
            });
            ui.add_space(10.0);
            ui.label("Reusable Rust / egui components");
            ui.weak("Hover, press, type, select or navigate with Tab. The application owns the data.");
            ui.add_space(20.0);
            Card::frame(ui.visuals()).show(ui,|ui| {
                ui.set_width(ui.available_width());
                ui.heading("Buttons");ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.add(Button::new("Save profile",ButtonKind::Primary)).clicked(){self.saved=true;}
                    ui.add(Button::new("Secondary",ButtonKind::Secondary));
                    ui.add(Button::new("Quiet action",ButtonKind::Ghost));
                    ui.add(Button::new("Delete",ButtonKind::Danger));
                    ui.add_enabled(false,Button::new("Unavailable",ButtonKind::Primary));
                });
                ui.add_space(10.0);
                ui.weak(if self.saved {"Saved locally in this demo."}else{"Primary buttons have tactile depth. Secondary actions stay light."});
            });
            ui.add_space(20.0);
            ui.columns(2,|columns| {
                Card::frame(columns[0].visuals()).show(&mut columns[0],|ui| {
                    ui.set_width(ui.available_width());ui.heading("Inputs & choices");ui.add_space(10.0);
                    ui.label("Profile name");ui.add(Input::new(&mut self.name).hint_text("Enter a name").desired_width(ui.available_width()));
                    ui.add_space(10.0);ui.label("Video format");
                    Select::from_id_salt("gallery-codec").selected_text(&self.quality).width(220.0).show_ui(ui,|ui| {
                        for label in ["Automatic","AV1","HEVC","H.264"] {ui.selectable_value(&mut self.quality,label.to_owned(),label);}
                    });
                    ui.add_space(12.0);ui.add(Switch::new(&mut self.enabled,"Streaming enabled"));
                    let mut fixed=true;ui.add_enabled(false,Switch::new(&mut fixed,"Managed setting"));
                });
                Card::frame(columns[1].visuals()).show(&mut columns[1],|ui| {
                    ui.set_width(ui.available_width());ui.heading("Cards & status");ui.add_space(10.0);
                    ui.label("Warm paper surfaces and readable semantic colors.");
                    ui.add_space(12.0);
                    for (label,tone) in [("Connected",Tone::Success),("Low bandwidth",Tone::Warning),("Disconnected",Tone::Danger),("AV1 / 60 FPS",Tone::Info)] {
                        ui.add(Tag::new(label,tone));ui.add_space(4.0);
                    }
                    ui.weak("Status is always expressed in words as well as color.");
                });
            });
            ui.add_space(18.0);ui.weak("Native widget gallery with generated data. No remote session or performance measurement.");
        });
        self.frames += 1;
        if self.output.is_some() {
            if self.frames == 5 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            }
            if self.started.elapsed() > std::time::Duration::from_secs(12) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
    }
}
fn main() -> eframe::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let output = args
        .iter()
        .position(|arg| arg == "--screenshot")
        .map(|i| std::path::PathBuf::from(args.get(i + 1).expect("screenshot path")));
    if let Some(path) = &output {
        assert!(!path.exists(), "refuse to overwrite screenshot");
    }
    let night = args.iter().any(|arg| arg == "--night");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1024.0, 760.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Animal Island UI — Rust gallery",
        options,
        Box::new(move |cc| {
            island::apply(&cc.egui_ctx);
            cc.egui_ctx.set_theme(if night {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            });
            Ok(Box::new(Gallery {
                name: "Desktop 4K".into(),
                enabled: true,
                quality: "Automatic".into(),
                night,
                saved: false,
                output,
                frames: 0,
                started: std::time::Instant::now(),
            }))
        }),
    )
}
