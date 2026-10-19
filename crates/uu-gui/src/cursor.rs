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
        native_size: egui::Vec2,
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
            let Some((size,hotspot))=geometry(shape.size,shape.hotspot,native_size) else {ctx.set_cursor_icon(egui::CursorIcon::Default);return;};
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

/// Remote coordinate scaling describes the remote desktop, not client cursor
/// DPI. Normalize the bitmap and hotspot together to this monitor's OS size.
fn geometry(size:[u32;2],hotspot:[i32;2],native:egui::Vec2)->Option<(egui::Vec2,egui::Vec2)> {
    if size.contains(&0) || hotspot[0]<0 || hotspot[1]<0 || hotspot[0]>=size[0] as i32 || hotspot[1]>=size[1] as i32 {return None;}
    let factor=(native.x/size[0] as f32).min(native.y/size[1] as f32);
    if !factor.is_finite()||factor<=0.0{return None;}
    Some((egui::vec2(size[0] as f32,size[1] as f32)*factor,egui::vec2(hotspot[0] as f32,hotspot[1] as f32)*factor))
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn cursor_size_and_hotspot_follow_client_dpi_together() {
        for pixels_per_point in [1.0,1.5,2.0] {
            let native=egui::Vec2::splat(32.0*pixels_per_point)/pixels_per_point;
            let (size,hotspot)=geometry([64,64],[8,4],native).unwrap();
            assert_eq!(size,egui::vec2(32.0,32.0));assert_eq!(hotspot,egui::vec2(4.0,2.0));
        }
        assert!(geometry([32,32],[1000,500],egui::Vec2::splat(32.0)).is_none());
    }
}
