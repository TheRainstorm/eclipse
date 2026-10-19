//! Locally present remote PNG cursor shapes; unknown formats retain OS arrow.
use eframe::egui;
#[derive(Default)]
pub struct Cursor {
    revision: u64,
    texture: Option<egui::TextureHandle>,
    shape: Option<uu_core::cursor::Shape>,
}
impl Cursor {
    pub fn revision(&self)->u64 {self.revision}
    pub fn paint(
        &mut self,
        ctx: &egui::Context,
        shape: Option<(u64, uu_core::cursor::Shape)>,
        position: Option<egui::Pos2>,
        draw: egui::Rect,
        screen: u32,
    ) {
        if let Some((revision, shape)) = shape {
            if self.revision != revision {
                self.revision = revision;
                self.texture = None;
                if !shape.image.is_empty() && shape.size[0] > 0 && shape.size[1] > 0 {
                    let mut reader = image::ImageReader::new(std::io::Cursor::new(&shape.image));
                    if let Ok(format) = image::guess_format(&shape.image) {
                        reader.set_format(format);
                        let mut limits = image::Limits::default();
                        limits.max_image_width = Some(512);
                        limits.max_image_height = Some(512);
                        limits.max_alloc = Some(1024 * 1024);
                        reader.limits(limits);
                        if let Ok(image) = reader.decode() {
                            let rgba = image.to_rgba8();
                            if [rgba.width(), rgba.height()] == shape.size {
                                self.texture = Some(ctx.load_texture(
                                    "remote-cursor",
                                    egui::ColorImage::from_rgba_unmultiplied(
                                        [rgba.width() as usize, rgba.height() as usize],
                                        rgba.as_raw(),
                                    ),
                                    egui::TextureOptions::LINEAR,
                                ));
                            }
                        }
                    }
                }
                self.shape = Some(shape);
            }
        }
        let Some(position) = position.filter(|p| draw.contains(*p)) else {
            return;
        };
        let Some(shape) = &self.shape else {
            return;
        };
        if shape.screen != 0 && shape.screen != screen as i32 {
            return;
        }
        if let Some(texture) = &self.texture {
            ctx.set_cursor_icon(egui::CursorIcon::None);
            let size = egui::vec2(
                shape.size[0] as f32 / shape.scale[0] as f32,
                shape.size[1] as f32 / shape.scale[1] as f32,
            );
            let hotspot = egui::vec2(
                shape.hotspot[0].clamp(0, shape.size[0] as i32) as f32 / shape.scale[0] as f32,
                shape.hotspot[1].clamp(0, shape.size[1] as i32) as f32 / shape.scale[1] as f32,
            );
            ctx.layer_painter(egui::LayerId::new(
                egui::Order::Tooltip,
                "remote-pointer".into(),
            ))
            .with_clip_rect(draw)
            .image(
                texture.id(),
                egui::Rect::from_min_size(position - hotspot, size),
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        } else {
            let icon = match shape.kind {
                32513 => egui::CursorIcon::Text,
                32514 => egui::CursorIcon::Wait,
                32515 => egui::CursorIcon::Crosshair,
                32642 => egui::CursorIcon::ResizeNwSe,
                32643 => egui::CursorIcon::ResizeNeSw,
                32644 => egui::CursorIcon::ResizeHorizontal,
                32645 => egui::CursorIcon::ResizeVertical,
                32646 => egui::CursorIcon::Move,
                32648 => egui::CursorIcon::NotAllowed,
                32649 => egui::CursorIcon::PointingHand,
                32650 => egui::CursorIcon::Progress,
                32651 => egui::CursorIcon::Help,
                _ => egui::CursorIcon::Default,
            };
            ctx.set_cursor_icon(icon);
        }
    }
}
