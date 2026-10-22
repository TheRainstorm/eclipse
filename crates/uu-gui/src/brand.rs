//! Product identity and the local About window.
use eframe::egui;

pub const NAME: &str = "uu-client";
pub const TAGLINE: &str = "远端，如在眼前";
pub const REPOSITORY: &str = "https://github.com/TheRainstorm/uu-client";
const ICON_PNG: &[u8] = include_bytes!("../../../assets/branding/eclipse-256.png");

pub fn window_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(ICON_PNG).expect("embedded Eclipse icon must be a valid PNG")
}

pub fn paint_logo(painter: &egui::Painter, rect: egui::Rect) {
    let id = egui::Id::new("eclipse-brand-texture");
    let ctx = painter.ctx();
    let texture = ctx
        .data_mut(|data| data.get_temp::<egui::TextureHandle>(id))
        .unwrap_or_else(|| {
            let image = window_icon();
            let texture = ctx.load_texture(
                "eclipse-brand",
                egui::ColorImage::from_rgba_unmultiplied(
                    [image.width as usize, image.height as usize],
                    &image.rgba,
                ),
                egui::TextureOptions::LINEAR,
            );
            ctx.data_mut(|data| data.insert_temp(id, texture.clone()));
            texture
        });
    let square = egui::Rect::from_center_size(
        rect.center(),
        egui::Vec2::splat(rect.width().min(rect.height())),
    );
    painter.image(
        texture.id(),
        square,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

impl super::App {
    pub(super) fn local_dialog_open(&self) -> bool {
        self.settings_open || self.about_open || self.native_ui.open || self.native_host.open || self.lan.host.open || self.connection_details_open || self.workspace.pairing_visible
    }

    pub(super) fn open_about(&mut self) {
        self.about_open = true;
        self.stream_menu.open = false;
        self.displays.menu_open = false;
        self.context.memory_mut(|memory| memory.close_popup());
        self.release_all_keys();
        self.release_pointer_capture(&self.context.clone());
    }

    pub(super) fn about_ui(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let mut open = true;
        egui::Window::new("关于 uu-client")
            .id(egui::Id::new("about-client"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(400.0_f32.min((ctx.screen_rect().width() - 48.0).max(260.0)))
            .max_width((ctx.screen_rect().width() - 40.0).max(260.0))
            .default_height((ctx.screen_rect().height() - 100.0).clamp(180.0, 600.0))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height((ctx.screen_rect().height() - 100.0).max(120.0))
                    .show(ui, |ui| {
                        ui.add_space(12.0);
                        ui.vertical_centered(|ui| {
                            let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(64.0), egui::Sense::hover());
                            super::ui::logo(ui.painter(), rect);
                            ui.heading(NAME);
                            ui.label(TAGLINE);
                            ui.add_space(8.0);
                            ui.label("开源的 Windows / Linux 远程串流客户端");
                        });
                        ui.add_space(12.0);
                        ui.separator();
                        egui::Grid::new("about-details").spacing([24.0, 10.0])
                            .max_col_width((ui.available_width() - 100.0).max(140.0)).show(ui, |ui| {
                            ui.label("版本");
                            ui.label(env!("CARGO_PKG_VERSION"));
                            ui.end_row();
                            ui.label("构建");
                            ui.add(egui::Label::new(&self.build_label).wrap());
                            ui.end_row();
                            ui.label("平台");
                            ui.label(format!("{} / {}", std::env::consts::OS, std::env::consts::ARCH));
                            ui.end_row();
                            ui.label("维护者");
                            ui.label("TheRainstorm");
                            ui.end_row();
                            ui.label("开源许可");
                            ui.label(env!("CARGO_PKG_LICENSE"));
                            ui.end_row();
                        });
                        ui.add_space(12.0);
                        ui.hyperlink_to("GitHub · TheRainstorm/uu-client", REPOSITORY);
                        ui.add_space(8.0);
                        ui.small("独立开源项目，与所兼容服务的提供方无隶属关系。");
                        ui.collapsing("诊断信息", |ui| {
                            ui.label("程序路径");
                            ui.add(egui::Label::new(&self.executable_path).wrap());
                            if ui.button("复制版本与诊断信息").clicked() {
                                ui.output_mut(|output| output.copied_text = format!(
                                    "{NAME} {}\n构建：{}\n平台：{} / {}\n{REPOSITORY}",
                                    env!("CARGO_PKG_VERSION"), self.build_label,
                                    std::env::consts::OS, std::env::consts::ARCH));
                            }
                            ui.small("复制内容仅含版本、构建、平台与项目链接，不含程序路径或账号信息。");
                        });
                    });
            });
        self.about_open = open;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn embedded_icon_decodes_and_survives_repeated_context_frames() {
        let icon = super::window_icon();
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
        let ctx = eframe::egui::Context::default();
        for _ in 0..2 {
            let output = ctx.run(Default::default(), |ctx| {
                super::paint_logo(
                    &ctx.debug_painter(),
                    eframe::egui::Rect::from_min_size(
                        eframe::egui::Pos2::ZERO,
                        eframe::egui::Vec2::splat(32.0),
                    ),
                );
            });
            assert!(!output.shapes.is_empty());
        }
    }
}
