//! Exercise real swap control on an isolated window; never connects to a host.
use eframe::egui;

#[derive(Default)]
struct Probe { step: usize, child_step: usize, frames: usize }
fn advance(ctx: &egui::Context, step: &mut usize) {
    match *step {
        0 | 2 | 4 => {
            eframe::request_vsync(ctx, *step != 2);
            *step += 1;
        }
        1 | 3 | 5 => if let Some(result)=eframe::vsync_result(ctx) {
            let expected=*step!=3;
            assert_eq!(result.unwrap_or_else(|e|panic!("swap control: {e}")),expected);
            *step+=1;
        },
        _=>{}
    }
    ctx.request_repaint();
}
impl eframe::App for Probe {
    fn update(&mut self, ctx:&egui::Context, _frame:&mut eframe::Frame) {
        self.frames+=1;
        assert!(self.frames<300,"swap control timed out");
        advance(ctx,&mut self.step);
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("probe-child"),
            egui::ViewportBuilder::default().with_title("Swap interval child probe"), |ctx,_| {
                egui::CentralPanel::default().show(ctx,|ui|{ui.label("Generated UI only");});
                advance(ctx,&mut self.child_step);
            });
        egui::CentralPanel::default().show(ctx,|ui|{ui.label("Generated UI only");});
        if self.step==6 && self.child_step==6 {
            println!("root and child: vsync on/off/on accepted on existing surfaces");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}
fn main()->eframe::Result {
    eframe::run_native("Swap interval probe",eframe::NativeOptions::default(),
        Box::new(|_|Ok(Box::<Probe>::default())))
}
