#[cfg(windows)]
#[path = "video_d3d11.rs"]
mod d3d11_renderer;
#[cfg(target_os = "linux")]
#[path = "video_dma_buf.rs"]
mod dma_buf_renderer;

// Upload decoded planes directly. No per-pixel ColorImage or CPU YUV conversion.
// EGL-capable Linux clients may import leased DMA-BUF planes directly.
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
    fallback_pixels:Option<Arc<egui::ColorImage>>,
}
pub fn enable_gpu_video(gl: &glow::Context) {
    #[cfg(target_os = "linux")]
    dma_buf_renderer::enable(gl);
    #[cfg(windows)]
    d3d11_renderer::enable(gl);
    #[cfg(not(any(windows, target_os = "linux")))]
    let _ = gl;
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
            fallback_pixels:None,
        };
        texture.set(ctx, image);
        texture
    }
    pub fn set(&mut self, ctx: &egui::Context, image: DisplayImage) {
        if let DisplayImage::Decoded(_, frame, color) = &image {
            if let Some(gpu) = &self.gpu {
                if valid(frame) || (frame.gpu && frame.is_consistent()) {
                    self.size = [frame.width as usize, frame.height as usize];
                    gpu.lock().unwrap().pending = Some((frame.clone(), *color));
                    return;
                }
            }
        }
        if let DisplayImage::Decoded(_, frame, _) = &image {
            if frame.gpu {
                uu_core::display_frames::dma_buf::set_rendering(false);
                uu_core::display_frames::d3d11_frame::set_rendering(false);
                self.size = [frame.width as usize, frame.height as usize];
                return; // The next decoded frame uses CPU storage; never read a GPU handle as pixels.
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
        let image = Arc::new(egui::ColorImage::from_rgba_unmultiplied(self.size, &rgba));
        self.fallback_pixels=Some(image.clone());
        match &mut self.fallback {
            Some(t) => t.set(egui::ImageData::Color(image), egui::TextureOptions::LINEAR),
            None => {
                self.fallback =
                    Some(ctx.load_texture("remote-fallback", egui::ImageData::Color(image), egui::TextureOptions::LINEAR))
            }
        }
    }
    #[allow(
        dead_code,
        reason = "Used by the separately built video_renderer_probe example"
    )]
    pub fn uses_planar_renderer(&self) -> bool {
        self.gpu.is_some()
    }
    pub fn render_path(&self) -> &'static str {
        match &self.gpu {
            Some(gpu) if gpu.lock().unwrap().d3d_active => "d3d11-wgl-gpu",
            Some(gpu) if gpu.lock().unwrap().dma_active => "dma-buf-egl",
            Some(_) => "cpu-planar-upload",
            None => "cpu-rgba-upload",
        }
    }
    pub fn gpu_shared_export(&self) -> Option<bool> {
        #[cfg(windows)]
        if let Some(gpu) = &self.gpu {
            let state = gpu.lock().unwrap();
            if state.d3d_active {
                return state.d3d_importer.as_ref().and_then(|i| i.shared_export());
            }
        }
        None
    }
    /// Session means: GPU conversion submission, WGL acquire, WGL release.
    pub fn gpu_timings_ms(&self) -> Option<[f64; 3]> {
        #[cfg(windows)]
        if let Some(gpu) = &self.gpu {
            let state = gpu.lock().unwrap();
            if state.d3d_active {
                return state.d3d_importer.as_ref().map(|i| i.timings_ms());
            }
        }
        None
    }
    pub fn size(&self) -> [usize; 2] {
        self.size
    }
    pub fn size_vec2(&self) -> egui::Vec2 {
        egui::vec2(self.size[0] as f32, self.size[1] as f32)
    }
    /// Call on the UI thread with the window's current GL context, once at disconnect.
    /// This draws only remote pixels, including imported hardware frames.
    pub fn snapshot(&self)->Result<egui::ColorImage,String> {
        if self.size.contains(&0){return Err("尚无串流画面".into());}
        let ratio=(480.0/self.size[0] as f64).min(270.0/self.size[1] as f64).min(1.);
        let size=[(self.size[0] as f64*ratio).max(1.) as usize,(self.size[1] as f64*ratio).max(1.) as usize];
        if let Some(gpu)=&self.gpu {
            let mut state=gpu.lock().unwrap();
            if state.pending.is_some()||state.size!=[0,0] {return unsafe{state.snapshot(size)};}
        }
        let image=self.fallback_pixels.as_ref().ok_or("没有可保存的远端画面")?;
        let mut output=egui::ColorImage::new(size,egui::Color32::BLACK);
        for y in 0..size[1] {for x in 0..size[0] {
            output.pixels[y*size[0]+x]=image.pixels[(y*image.size[1]/size[1])*image.size[0]+x*image.size[0]/size[0]];
        }}
        Ok(output)
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
    uv_scale_uniform: Option<glow::UniformLocation>,
    dma_active: bool,
    d3d_active: bool,
    #[cfg(windows)]
    d3d_importer: Option<d3d11_renderer::Importer>,
    #[cfg(target_os = "linux")]
    importer: Option<dma_buf_renderer::Importer>,
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
        #[cfg(target_os = "linux")]
        let importer = dma_buf_renderer::Importer::new(gl.clone());
        #[cfg(windows)]
        let d3d_importer = d3d11_renderer::Importer::new(gl.clone());
        let uv_scale_uniform = gl.get_uniform_location(program, "video_uv_layout");
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
            uv_scale_uniform,
            dma_active: false,
            d3d_active: false,
            #[cfg(windows)]
            d3d_importer,
            #[cfg(target_os = "linux")]
            importer,
        })
    }
    unsafe fn snapshot(&mut self,size:[usize;2])->Result<egui::ColorImage,String> {
        let gl=self.gl.clone();
        let draw=gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING);
        let read=gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING);
        let program=gl.get_parameter_program(glow::CURRENT_PROGRAM);
        let vao=gl.get_parameter_vertex_array(glow::VERTEX_ARRAY_BINDING);
        let active=gl.get_parameter_i32(glow::ACTIVE_TEXTURE) as u32;
        let pack=gl.get_parameter_i32(glow::PACK_ALIGNMENT);
        let pack_row=gl.get_parameter_i32(glow::PACK_ROW_LENGTH);
        let pack_skip_x=gl.get_parameter_i32(glow::PACK_SKIP_PIXELS);
        let pack_skip_y=gl.get_parameter_i32(glow::PACK_SKIP_ROWS);
        let unpack_buffer=gl.get_parameter_buffer(glow::PIXEL_UNPACK_BUFFER_BINDING);
        let unpack=gl.get_parameter_i32(glow::UNPACK_ALIGNMENT);
        let row=gl.get_parameter_i32(glow::UNPACK_ROW_LENGTH);
        let pack_buffer=gl.get_parameter_buffer(glow::PIXEL_PACK_BUFFER_BINDING);
        let mut viewport=[0;4];gl.get_parameter_i32_slice(glow::VIEWPORT,&mut viewport);
        let scissor=gl.is_enabled(glow::SCISSOR_TEST);let blend=gl.is_enabled(glow::BLEND);
        let mut textures=Vec::new();let mut samplers=Vec::new();
        for unit in 0..3 {gl.active_texture(glow::TEXTURE0+unit);textures.push(gl.get_parameter_texture(glow::TEXTURE_BINDING_2D));samplers.push(gl.get_parameter_sampler(glow::SAMPLER_BINDING));}
        gl.active_texture(active);
        let texture=gl.create_texture()?;
        let framebuffer=match gl.create_framebuffer(){Ok(f)=>f,Err(e)=>{gl.delete_texture(texture);return Err(e);}};
        let result=(|| {
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER,None);
            gl.active_texture(glow::TEXTURE0);gl.bind_texture(glow::TEXTURE_2D,Some(texture));
            gl.tex_image_2d(glow::TEXTURE_2D,0,glow::RGBA8 as i32,size[0] as i32,size[1] as i32,0,glow::RGBA,glow::UNSIGNED_BYTE,None);
            gl.bind_framebuffer(glow::FRAMEBUFFER,Some(framebuffer));
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER,glow::COLOR_ATTACHMENT0,glow::TEXTURE_2D,Some(texture),0);
            if gl.check_framebuffer_status(glow::FRAMEBUFFER)!=glow::FRAMEBUFFER_COMPLETE {return Err("无法建立远端预览缓冲".into());}
            gl.viewport(0,0,size[0] as i32,size[1] as i32);gl.disable(glow::SCISSOR_TEST);gl.disable(glow::BLEND);
            for unit in 0..3 {gl.bind_sampler(unit,None);}
            self.paint();
            if self.size==[0,0]{return Err("远端 GPU 画面暂不可用".into());}
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER,None);gl.pixel_store_i32(glow::PACK_ALIGNMENT,1);
            gl.pixel_store_i32(glow::PACK_ROW_LENGTH,0);gl.pixel_store_i32(glow::PACK_SKIP_PIXELS,0);gl.pixel_store_i32(glow::PACK_SKIP_ROWS,0);
            let mut rgba=vec![0;size[0]*size[1]*4];
            gl.read_pixels(0,0,size[0] as i32,size[1] as i32,glow::RGBA,glow::UNSIGNED_BYTE,glow::PixelPackData::Slice(&mut rgba));
            if gl.get_error()!=glow::NO_ERROR{return Err("读取远端预览失败".into());}
            let row=size[0]*4;
            for y in 0..size[1]/2 {let (top,bottom)=rgba.split_at_mut((size[1]-1-y)*row);top[y*row..(y+1)*row].swap_with_slice(&mut bottom[..row]);}
            Ok(egui::ColorImage::from_rgba_unmultiplied(size,&rgba))
        })();
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER,draw);gl.bind_framebuffer(glow::READ_FRAMEBUFFER,read);
        gl.viewport(viewport[0],viewport[1],viewport[2],viewport[3]);
        if scissor{gl.enable(glow::SCISSOR_TEST);}else{gl.disable(glow::SCISSOR_TEST);}
        if blend{gl.enable(glow::BLEND);}else{gl.disable(glow::BLEND);}
        gl.use_program(program);gl.bind_vertex_array(vao);
        gl.bind_buffer(glow::PIXEL_PACK_BUFFER,pack_buffer);gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER,unpack_buffer);
        gl.pixel_store_i32(glow::PACK_ROW_LENGTH,pack_row);gl.pixel_store_i32(glow::PACK_SKIP_PIXELS,pack_skip_x);gl.pixel_store_i32(glow::PACK_SKIP_ROWS,pack_skip_y);
        gl.pixel_store_i32(glow::PACK_ALIGNMENT,pack);gl.pixel_store_i32(glow::UNPACK_ALIGNMENT,unpack);gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH,row);
        for (unit,texture) in textures.into_iter().enumerate(){gl.active_texture(glow::TEXTURE0+unit as u32);gl.bind_texture(glow::TEXTURE_2D,texture);gl.bind_sampler(unit as u32,samplers[unit]);}
        gl.active_texture(active);gl.delete_framebuffer(framebuffer);gl.delete_texture(texture);
        result
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
            if f.gpu {
                #[cfg(windows)]
                if let (Some(importer), Some(surface)) = (&mut self.d3d_importer, &f.d3d11) {
                    match importer.set(surface.clone(), color) {
                        Ok(()) => {
                            self.size = [f.width, f.height];
                            self.layout = Some(f.format);
                            self.color = color;
                            self.d3d_active = true;
                        }
                        Err(error) => {
                            eprintln!("D3D11 GPU video unavailable, using readback: {error}");
                            uu_core::display_frames::d3d11_frame::set_rendering(false);
                            importer.clear();
                            self.d3d_active = false;
                            self.size = [0, 0];
                        }
                    }
                } else {
                    uu_core::display_frames::d3d11_frame::set_rendering(false);
                }
                #[cfg(target_os = "linux")]
                if let Some(importer) = &mut self.importer {
                    match importer.set(f.clone()) {
                        Ok(true) => {
                            self.size = [f.width, f.height];
                            self.layout = Some(f.format);
                            self.color = color;
                            self.dma_active = true;
                        }
                        Ok(false) => {
                            self.pending = Some((f, color));
                        }
                        Err(error) => {
                            eprintln!(
                                "DMA-BUF rendering unavailable, retaining CPU upload: {error}"
                            );
                            uu_core::display_frames::dma_buf::set_rendering(false);
                        }
                    }
                } else {
                    uu_core::display_frames::dma_buf::set_rendering(false);
                }
            } else {
                let changed = self.dma_active
                    || self.d3d_active
                    || self.size != [f.width, f.height]
                    || self.layout != Some(f.format);
                #[cfg(target_os = "linux")]
                if let Some(importer) = &mut self.importer {
                    importer.clear();
                }
                self.dma_active = false;
                #[cfg(windows)]
                if let Some(importer) = &mut self.d3d_importer {
                    importer.clear();
                }
                self.d3d_active = false;
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
        }
        let gl = &self.gl;
        gl.use_program(Some(self.program));
        gl.bind_vertex_array(Some(self.vao));
        for (i, t) in self.textures.iter().enumerate() {
            gl.active_texture(glow::TEXTURE0 + i as u32);
            gl.bind_texture(glow::TEXTURE_2D, Some(*t));
        }
        #[cfg(windows)]
        if self.d3d_active {
            if let Some(importer) = &mut self.d3d_importer {
                if let Err(error) = importer.bind() {
                    eprintln!("D3D11 GPU video lock failed, using readback: {error}");
                    uu_core::display_frames::d3d11_frame::set_rendering(false);
                    importer.clear();
                    self.d3d_active = false;
                    self.size = [0, 0];
                    return;
                }
            }
        }
        if self.size == [0, 0] {
            return;
        }
        let uv_scale = [
            1.0,
            1.0,
            1.0 / self.size[0].max(1) as f32,
            1.0 / self.size[1].max(1) as f32,
        ];
        #[cfg(target_os = "linux")]
        let uv_scale = if self.dma_active {
            self.importer
                .as_ref()
                .map_or(uv_scale, |importer| importer.bind())
        } else {
            uv_scale
        };
        gl.uniform_4_f32(
            self.uv_scale_uniform.as_ref(),
            uv_scale[0],
            uv_scale[1],
            uv_scale[2],
            uv_scale[3],
        );
        gl.uniform_1_i32(
            self.layout_uniform.as_ref(),
            if self.d3d_active {
                3
            } else {
                match self.layout {
                    Some(PixelFormat::Nv12) => 0,
                    Some(PixelFormat::I420) => 1,
                    _ => 2,
                }
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
        #[cfg(target_os = "linux")]
        if self.dma_active {
            if let Some(importer) = &mut self.importer {
                importer.sampled();
            }
        }
        // egui restores its program, VAO and blend state after the callback.
        // Restore texture unit and pixel unpack state explicitly.
        gl.active_texture(glow::TEXTURE0);
    }
}
impl Drop for Planes {
    fn drop(&mut self) {
        unsafe {
            #[cfg(windows)]
            drop(self.d3d_importer.take());
            #[cfg(target_os = "linux")]
            drop(self.importer.take());
            for t in self.textures {
                self.gl.delete_texture(t)
            }
            self.gl.delete_vertex_array(self.vao);
            self.gl.delete_program(self.program)
        }
    }
}
