//! Upload decoded planes directly. No per-pixel ColorImage or CPU YUV conversion.
//! D3D11/VAAPI readback remains in the decoder; this is not zero-copy decoding.
use eframe::{
    egui,
    glow::{self, HasContext},
};
use std::sync::{Arc, Mutex};
use uu_core::display_frames::{DisplayImage, PixelFormat, RawFrame, YuvColor};

pub struct VideoTexture {
    size: [usize; 2],
    gpu: Option<Arc<Mutex<Planes>>>,
    fallback: Option<egui::TextureHandle>,
}
impl VideoTexture {
    pub fn new(ctx: &egui::Context, gl: Option<&Arc<glow::Context>>, image: DisplayImage) -> Self {
        let gpu = gl.and_then(|gl| match unsafe { Planes::new(gl.clone()) } {
            Ok(p) => Some(Arc::new(Mutex::new(p))),
            Err(e) => {
                eprintln!("planar video renderer unavailable, using RGBA: {e}");
                None
            }
        });
        let mut texture = Self {
            size: [0, 0],
            gpu,
            fallback: None,
        };
        texture.set(ctx, image);
        texture
    }
    pub fn set(&mut self, ctx: &egui::Context, image: DisplayImage) {
        if let DisplayImage::Decoded(_, frame, color) = &image {
            if let Some(gpu) = &self.gpu {
                if valid(frame) {
                    self.size = [frame.width as usize, frame.height as usize];
                    gpu.lock().unwrap().pending = Some((frame.clone(), *color));
                    return;
                }
            }
        }
        // An early legacy frame must not permanently disable planar rendering.
        if let Some(gpu) = &self.gpu {
            let mut gpu = gpu.lock().unwrap();
            gpu.size = [0, 0];
            gpu.layout = None;
            gpu.pending = None;
        }
        let (_, w, h, rgba) = image.into_rgba();
        self.size = [w as usize, h as usize];
        let image = egui::ColorImage::from_rgba_unmultiplied(self.size, &rgba);
        match &mut self.fallback {
            Some(t) => t.set(image, egui::TextureOptions::LINEAR),
            None => {
                self.fallback =
                    Some(ctx.load_texture("remote-fallback", image, egui::TextureOptions::LINEAR))
            }
        }
    }
    pub fn uses_planar_renderer(&self) -> bool {
        self.gpu.is_some()
    }
    pub fn size(&self) -> [usize; 2] {
        self.size
    }
    pub fn size_vec2(&self) -> egui::Vec2 {
        egui::vec2(self.size[0] as f32, self.size[1] as f32)
    }
    pub fn paint(&self, ui: &egui::Ui, rect: egui::Rect) {
        if let Some(gpu) = &self.gpu {
            // A legacy RGBA frame can occur before the receiver opts in.
            let ready = {
                let state = gpu.lock().unwrap();
                state.pending.is_some() || state.size != [0, 0]
            };
            if ready {
                let gpu = gpu.clone();
                ui.painter().add(egui::PaintCallback {
                    rect,
                    callback: Arc::new(eframe::egui_glow::CallbackFn::new(move |_, _| unsafe {
                        gpu.lock().unwrap().paint();
                    })),
                });
                return;
            }
        }
        if let Some(t) = &self.fallback {
            ui.painter().image(
                t.id(),
                rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
    }
}
fn valid(frame: &RawFrame) -> bool {
    let (w, h, s) = (
        frame.width as usize,
        frame.height as usize,
        frame.stride as usize,
    );
    if frame.gpu || w < 2 || h < 2 || w > 8192 || h > 8192 || w % 2 != 0 || h % 2 != 0 {
        return false;
    }
    let (row, len) = match frame.format {
        PixelFormat::Nv12 => (w, s * h + s * (h / 2)),
        PixelFormat::I420 => (w, s * h + w * h / 2),
        PixelFormat::Bgra8 => (w * 4, s * h),
    };
    s >= row
        && (frame.format != PixelFormat::Bgra8 || s % 4 == 0)
        && (frame.format != PixelFormat::Nv12 || s % 2 == 0)
        && frame.data.len() >= len
}
struct Planes {
    gl: Arc<glow::Context>,
    program: glow::Program,
    layout_uniform: Option<glow::UniformLocation>,
    coefficients_uniform: Option<glow::UniformLocation>,
    levels_uniform: Option<glow::UniformLocation>,
    vao: glow::VertexArray,
    textures: [glow::Texture; 3],
    size: [u32; 2],
    layout: Option<PixelFormat>,
    color: YuvColor,
    pending: Option<(RawFrame, YuvColor)>,
}
impl Planes {
    unsafe fn new(gl: Arc<glow::Context>) -> Result<Self, String> {
        let header = if gl.version().is_embedded {
            "#version 300 es\nprecision highp float;\n"
        } else {
            "#version 330\n"
        };
        let program = gl.create_program()?;
        for (kind, source) in [
            (glow::VERTEX_SHADER, include_str!("video.vert")),
            (glow::FRAGMENT_SHADER, include_str!("video.frag")),
        ] {
            let shader = match gl.create_shader(kind) {
                Ok(s) => s,
                Err(e) => {
                    gl.delete_program(program);
                    return Err(e);
                }
            };
            gl.shader_source(shader, &format!("{header}{source}"));
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                let e = gl.get_shader_info_log(shader);
                gl.delete_shader(shader);
                gl.delete_program(program);
                return Err(e);
            }
            gl.attach_shader(program, shader);
            gl.delete_shader(shader);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            let e = gl.get_program_info_log(program);
            gl.delete_program(program);
            return Err(e);
        }
        let vao = match gl.create_vertex_array() {
            Ok(v) => v,
            Err(e) => {
                gl.delete_program(program);
                return Err(e);
            }
        };
        let mut textures = Vec::new();
        for _ in 0..3 {
            match gl.create_texture() {
                Ok(t) => textures.push(t),
                Err(e) => {
                    for t in textures {
                        gl.delete_texture(t)
                    }
                    gl.delete_vertex_array(vao);
                    gl.delete_program(program);
                    return Err(e);
                }
            }
        }
        let layout_uniform = gl.get_uniform_location(program, "pixel_layout");
        let coefficients_uniform = gl.get_uniform_location(program, "coeff");
        let levels_uniform = gl.get_uniform_location(program, "levels");
        let previous_program = gl.get_parameter_program(glow::CURRENT_PROGRAM);
        gl.use_program(Some(program));
        for (unit, name) in ["plane0", "plane1", "plane2"].iter().enumerate() {
            gl.uniform_1_i32(gl.get_uniform_location(program, name).as_ref(), unit as i32);
        }
        gl.use_program(previous_program);
        Ok(Self {
            gl,
            program,
            layout_uniform,
            coefficients_uniform,
            levels_uniform,
            vao,
            textures: textures.try_into().unwrap(),
            size: [0, 0],
            layout: None,
            color: YuvColor::default(),
            pending: None,
        })
    }
    unsafe fn upload(
        &self,
        unit: usize,
        w: u32,
        h: u32,
        channels: u32,
        row_bytes: u32,
        data: &[u8],
        reallocate: bool,
    ) {
        let gl = &self.gl;
        gl.active_texture(glow::TEXTURE0 + unit as u32);
        gl.bind_texture(glow::TEXTURE_2D, Some(self.textures[unit]));
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, (row_bytes / channels) as i32);
        let (internal, format) = match channels {
            1 => (glow::R8, glow::RED),
            2 => (glow::RG8, glow::RG),
            _ => (glow::RGBA8, glow::RGBA),
        };
        if reallocate {
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
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
                internal as i32,
                w as i32,
                h as i32,
                0,
                format,
                glow::UNSIGNED_BYTE,
                Some(data),
            );
        } else {
            gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                0,
                0,
                w as i32,
                h as i32,
                format,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(data),
            );
        }
        gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
    }
    unsafe fn paint(&mut self) {
        if let Some((f, color)) = self.pending.take() {
            let changed = self.size != [f.width, f.height] || self.layout != Some(f.format);
            let y = f.stride as usize * f.height as usize;
            match f.format {
                PixelFormat::Nv12 => {
                    self.upload(0, f.width, f.height, 1, f.stride, &f.data[..y], changed);
                    self.upload(
                        1,
                        f.width / 2,
                        f.height / 2,
                        2,
                        f.stride,
                        &f.data[y..],
                        changed,
                    );
                }
                PixelFormat::I420 => {
                    let c = (f.width * f.height / 4) as usize;
                    self.upload(0, f.width, f.height, 1, f.stride, &f.data[..y], changed);
                    self.upload(
                        1,
                        f.width / 2,
                        f.height / 2,
                        1,
                        f.width / 2,
                        &f.data[y..y + c],
                        changed,
                    );
                    self.upload(
                        2,
                        f.width / 2,
                        f.height / 2,
                        1,
                        f.width / 2,
                        &f.data[y + c..],
                        changed,
                    );
                }
                PixelFormat::Bgra8 => {
                    self.upload(0, f.width, f.height, 4, f.stride, &f.data, changed)
                }
            }
            self.size = [f.width, f.height];
            self.layout = Some(f.format);
            self.color = color;
        }
        let gl = &self.gl;
        gl.use_program(Some(self.program));
        gl.bind_vertex_array(Some(self.vao));
        for (i, t) in self.textures.iter().enumerate() {
            gl.active_texture(glow::TEXTURE0 + i as u32);
            gl.bind_texture(glow::TEXTURE_2D, Some(*t));
        }
        gl.uniform_1_i32(
            self.layout_uniform.as_ref(),
            match self.layout {
                Some(PixelFormat::Nv12) => 0,
                Some(PixelFormat::I420) => 1,
                _ => 2,
            },
        );
        let (offset, scale, r, gu, gv, b) = match self.color {
            YuvColor::Full601 => (0., 1., 1.402, 0.344136, 0.714136, 1.772),
            YuvColor::Full709 => (0., 1., 1.5748, 0.187324, 0.468124, 1.8556),
            YuvColor::Limited601 => (
                16. / 255.,
                255. / 219.,
                1.596027,
                0.391762,
                0.812968,
                2.017232,
            ),
            YuvColor::Limited709 => (
                16. / 255.,
                255. / 219.,
                1.792741,
                0.213249,
                0.532909,
                2.112402,
            ),
        };
        gl.uniform_4_f32(self.coefficients_uniform.as_ref(), r, gu, gv, b);
        gl.uniform_2_f32(self.levels_uniform.as_ref(), offset, scale);
        gl.draw_arrays(glow::TRIANGLES, 0, 3);
        // egui restores its program, VAO and blend state after the callback.
        // Restore texture unit and pixel unpack state explicitly.
        gl.active_texture(glow::TEXTURE0);
    }
}
impl Drop for Planes {
    fn drop(&mut self) {
        unsafe {
            for t in self.textures {
                self.gl.delete_texture(t)
            }
            self.gl.delete_vertex_array(self.vao);
            self.gl.delete_program(self.program)
        }
    }
}
