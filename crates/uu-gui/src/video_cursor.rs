//! Small pointer textures composited by the existing video draw call.
use eframe::{
    egui,
    glow::{self, HasContext},
};
use std::sync::Arc;
use uu_core::native_cursor::{Shape, Snapshot};

pub(super) struct Textures {
    enabled: Option<glow::UniformLocation>,
    bounds: Option<glow::UniformLocation>,
    textures: Option<[glow::Texture; 2]>,
    shape: Option<Arc<Shape>>,
    pub state: Option<Snapshot>,
}
impl Textures {
    pub unsafe fn new(gl: &glow::Context, program: glow::Program) -> Self {
        gl.uniform_1_i32(gl.get_uniform_location(program, "cursor_alpha").as_ref(), 3);
        gl.uniform_1_i32(gl.get_uniform_location(program, "cursor_xor").as_ref(), 4);
        Self {
            enabled: gl.get_uniform_location(program, "cursor_enabled"),
            bounds: gl.get_uniform_location(program, "cursor_bounds"),
            textures: None,
            shape: None,
            state: None,
        }
    }
    pub unsafe fn bind(&mut self, gl: &glow::Context) {
        gl.uniform_1_i32(self.enabled.as_ref(), 0);
        let Some(s) = self.state.as_ref() else {
            return;
        };
        let (Some(position), Some(shape)) = (s.position, s.shape.as_ref()) else {
            return;
        };
        if s.desktop.contains(&0) {
            return;
        }
        let Some(size) = shape.description.size() else {
            return;
        };
        if self
            .shape
            .as_ref()
            .is_none_or(|old| !Arc::ptr_eq(old, shape))
        {
            let Some((alpha, xor)) = shape.layers() else {
                return;
            };
            if self.textures.is_none() {
                let Ok(a) = gl.create_texture() else {
                    return;
                };
                let Ok(x) = gl.create_texture() else {
                    gl.delete_texture(a);
                    return;
                };
                self.textures = Some([a, x]);
            }
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
            for (index, data) in [alpha, xor].into_iter().enumerate() {
                gl.active_texture(glow::TEXTURE3 + index as u32);
                gl.bind_texture(glow::TEXTURE_2D, Some(self.textures.unwrap()[index]));
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MIN_FILTER,
                    glow::NEAREST as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MAG_FILTER,
                    glow::NEAREST as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_WRAP_S,
                    glow::CLAMP_TO_EDGE as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_WRAP_T,
                    glow::CLAMP_TO_EDGE as i32,
                );
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA8 as i32,
                    size[0] as i32,
                    size[1] as i32,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    Some(&data),
                );
            }
            self.shape = Some(shape.clone());
        }
        for (index, texture) in self.textures.unwrap().into_iter().enumerate() {
            gl.active_texture(glow::TEXTURE3 + index as u32);
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        }
        gl.uniform_4_f32(
            self.bounds.as_ref(),
            position[0] as f32 / s.desktop[0] as f32,
            position[1] as f32 / s.desktop[1] as f32,
            size[0] as f32 / s.desktop[0] as f32,
            size[1] as f32 / s.desktop[1] as f32,
        );
        gl.uniform_1_i32(self.enabled.as_ref(), 1);
    }
    pub unsafe fn destroy(&mut self, gl: &glow::Context) {
        if let Some(textures) = self.textures.take() {
            for t in textures {
                gl.delete_texture(t);
            }
        }
    }
}

#[derive(Default)]
pub(super) struct Fallback {
    texture: Option<egui::TextureHandle>,
    key: Option<(u64, usize)>,
}
impl Fallback {
    /// Only the small cursor patch is rebuilt; the RGBA fallback already owns
    /// CPU video pixels, so this never downloads or copies a complete frame.
    pub fn paint(
        &mut self,
        ui: &egui::Ui,
        draw: egui::Rect,
        s: &Snapshot,
        background: &Arc<egui::ColorImage>,
    ) {
        let (Some(position), Some(shape)) = (s.position, s.shape.as_ref()) else {
            return;
        };
        if s.desktop.contains(&0) {
            return;
        }
        let Some(size) = shape.description.size() else {
            return;
        };
        let key = (s.revision, Arc::as_ptr(background) as usize);
        if self.key != Some(key) {
            let Some((alpha, xor)) = shape.layers() else {
                return;
            };
            let mut pixels = Vec::with_capacity(size[0] as usize * size[1] as usize * 4);
            for y in 0..size[1] {
                for x in 0..size[0] {
                    let dx = i64::from(position[0]) + i64::from(x);
                    let dy = i64::from(position[1]) + i64::from(y);
                    if dx < 0
                        || dy < 0
                        || dx >= i64::from(s.desktop[0])
                        || dy >= i64::from(s.desktop[1])
                    {
                        pixels.extend([0; 4]);
                        continue;
                    }
                    let bx = (dx as usize * background.size[0] / s.desktop[0] as usize)
                        .min(background.size[0] - 1);
                    let by = (dy as usize * background.size[1] / s.desktop[1] as usize)
                        .min(background.size[1] - 1);
                    let bg = background.pixels[by * background.size[0] + bx].to_array();
                    let at = (y * size[0] + x) as usize * 4;
                    let a = u32::from(alpha[at + 3]);
                    for c in 0..3 {
                        pixels.push(
                            ((u32::from(alpha[at + c]) * a + u32::from(bg[c]) * (255 - a) + 127)
                                / 255) as u8
                                ^ xor[at + c],
                        );
                    }
                    pixels.push(255);
                }
            }
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [size[0] as usize, size[1] as usize],
                &pixels,
            );
            if let Some(texture) = self.texture.as_mut() {
                texture.set(image, egui::TextureOptions::NEAREST);
            } else {
                self.texture = Some(ui.ctx().load_texture(
                    "native-cursor-patch",
                    image,
                    egui::TextureOptions::NEAREST,
                ));
            }
            self.key = Some(key);
        }
        if let Some(texture) = &self.texture {
            let scale = draw.size() / egui::vec2(s.desktop[0] as f32, s.desktop[1] as f32);
            let min = draw.min + egui::vec2(position[0] as f32, position[1] as f32) * scale;
            ui.painter().with_clip_rect(draw).image(
                texture.id(),
                egui::Rect::from_min_size(min, egui::vec2(size[0] as f32, size[1] as f32) * scale),
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1., 1.)),
                egui::Color32::WHITE,
            );
        }
    }
}
