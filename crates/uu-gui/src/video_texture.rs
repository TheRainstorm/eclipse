#[cfg(windows)]
#[path = "video_d3d11.rs"]
mod d3d11_renderer;
#[cfg(target_os = "linux")]
#[path = "video_dma_buf.rs"]
mod dma_buf_renderer;
#[path = "video_timing.rs"]
mod video_timing;
use uu_core::frame_continuity;
pub use frame_continuity::Snapshot as ContinuitySnapshot;
#[path = "video_cursor.rs"]
mod video_cursor;
#[path="video_frame_observation.rs"]
pub(crate) mod frame_observation;
#[path="video_present_trace.rs"]
#[cfg(any(windows,test))]
pub(crate) mod present_trace;
#[path="video_dxgi_receipt.rs"]
#[cfg(any(windows,test))]
pub(crate) mod dxgi_receipt;
use frame_observation::FrameObservation;

// Upload decoded planes directly. No per-pixel ColorImage or CPU YUV conversion.
// EGL-capable Linux clients may import leased DMA-BUF planes directly.
use eframe::{
    egui,
    glow::{self, HasContext},
};
use std::sync::{Arc, Mutex};
use uu_core::display_frames::{DisplayImage, PixelFormat, RawFrame, YuvColor};

#[cfg(windows)]
#[path="video_native_source.rs"]
mod native_source;
#[cfg(windows)]
#[derive(Clone)]
pub(crate) struct NativeImage {
    pub id:u64,pub handles:[usize;3],pub ready:u64,
    pub consumed:Arc<std::sync::atomic::AtomicU64>,pub valid:Arc<std::sync::atomic::AtomicBool>,
    pub rect:[f32;4],pub hdr:bool,pub source_white:f32,pub cursor:Option<uu_core::native_cursor::Snapshot>,
    pub observation:Option<FrameObservation>,
}
pub struct VideoTexture {
    continuity:Mutex<frame_continuity::Continuity>,
    cursor:Mutex<Option<uu_core::native_cursor::Snapshot>>,
    fallback_cursor:Mutex<video_cursor::Fallback>,
    size: [usize; 2],
    gpu: Option<Arc<Mutex<Planes>>>,
    fallback: Option<egui::TextureHandle>,
    fallback_pixels:Option<Arc<egui::ColorImage>>,
    fallback_observation:Option<FrameObservation>,
    source_white_nits:f32,
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
        Self::new_with_observation(ctx,gl,image,None)
    }
    pub fn new_observed(ctx:&egui::Context,gl:Option<&Arc<glow::Context>>,image:DisplayImage,stats:&Arc<uu_core::media::MediaStats>,frozen:Option<uu_core::display_frames::DecodedObservation>)->Self {
        let observation=FrameObservation::new(stats,&image,frozen);
        Self::new_with_observation(ctx,gl,image,observation)
    }
    fn new_with_observation(ctx:&egui::Context,gl:Option<&Arc<glow::Context>>,image:DisplayImage,observation:Option<FrameObservation>)->Self {
        let gpu = gl.and_then(|gl| match unsafe { Planes::new(gl.clone()) } {
            Ok(p) => Some(Arc::new(Mutex::new(p))),
            Err(e) => {
                eprintln!("planar video renderer unavailable, using RGBA: {e}");
                None
            }
        });
        let mut texture = Self {
            continuity:Mutex::new(Default::default()),
            cursor:Mutex::new(None),fallback_cursor:Mutex::new(Default::default()),
            size: [0, 0],
            gpu,
            fallback: None,
            fallback_pixels:None,
            fallback_observation:None,
            source_white_nits:203.,
        };
        texture.set_with_observation(ctx, image,observation);
        texture
    }
    pub fn set_source_white(&mut self,white:f32){if white.is_finite()&&(40.0..=1000.0).contains(&white){self.source_white_nits=white;}}
    pub fn set_continuity_active(&self,active:bool,fps:u32){self.continuity.lock().unwrap().set_active(active,fps);}
    pub fn record_new_image(&self){self.continuity.lock().unwrap().submit(std::time::Instant::now());}
    pub fn continuity(&self)->Option<ContinuitySnapshot>{self.continuity.lock().unwrap().snapshot(std::time::Instant::now())}
    pub fn continuity_window(&self,seconds:u64)->Option<ContinuitySnapshot>{self.continuity.lock().unwrap().snapshot_window(std::time::Instant::now(),seconds)}
    pub fn set(&mut self, ctx: &egui::Context, image: DisplayImage) {
        self.set_with_observation(ctx,image,None)
    }
    pub fn set_observed(&mut self,ctx:&egui::Context,image:DisplayImage,stats:&Arc<uu_core::media::MediaStats>,frozen:Option<uu_core::display_frames::DecodedObservation>) {
        let observation=FrameObservation::new(stats,&image,frozen);
        self.set_with_observation(ctx,image,observation)
    }
    fn set_with_observation(&mut self,ctx:&egui::Context,image:DisplayImage,observation:Option<FrameObservation>) {
        if let DisplayImage::Decoded(_, frame, color) = &image {
            if let Some(gpu) = &self.gpu {
                if valid(frame) || (frame.gpu && frame.is_consistent()) {
                    let mut state=gpu.lock().unwrap();
                    // Normalized 16-bit planes require EXT_texture_norm16 on
                    // GLES. Desktop GL supports them; other GLES clients use
                    // explicit CPU tone mapping rather than an invalid upload.
                    let supported=frame.gpu || frame.format!=PixelFormat::P010 || !state.gl.version().is_embedded || state.gl.supported_extensions().contains("GL_EXT_texture_norm16");
                    if supported {
                        self.size = [frame.width as usize, frame.height as usize];
                        state.pending = Some((frame.clone(), *color,observation));
                        return;
                    }
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
        self.fallback_observation=observation;
        match &mut self.fallback {
            Some(t) => t.set(egui::ImageData::Color(image), egui::TextureOptions::LINEAR),
            None => {
                self.fallback =
                    Some(ctx.load_texture("remote-fallback", egui::ImageData::Color(image), egui::TextureOptions::LINEAR))
            }
        }
    }
    /// Identity of these exact fallback pixels, retained for a future GL/UI
    /// submission observer. Absence never triggers a lookup of newer inputs.
    #[allow(dead_code)]
    pub(crate) fn fallback_frame_identity(&self)->Option<frame_observation::FrameIdentity> {
        self.fallback_observation.as_ref()?.current_identity()
    }
    #[allow(
        dead_code,
        reason = "Used by the separately built video_renderer_probe example"
    )]
    pub fn uses_planar_renderer(&self) -> bool {
        self.gpu.is_some()
    }
    pub fn render_path(&self) -> &'static str {
        #[cfg(windows)]
        if self.gpu.as_ref().is_some_and(|g|g.lock().unwrap().native_active){return "d3d11-native-gpu";}
        match &self.gpu {
            Some(gpu) if gpu.lock().unwrap().d3d_active => "d3d11-wgl-gpu",
            Some(gpu) if gpu.lock().unwrap().dma_active => "dma-buf-egl",
            Some(_) => "cpu-planar-upload",
            None => "cpu-rgba-upload",
        }
    }
    pub fn color_diagnostics(&self) -> Option<serde_json::Value> {
        let gpu=self.gpu.as_ref()?.lock().unwrap();
        #[cfg(windows)]
        let conversion=if gpu.d3d_active {gpu.d3d_importer.as_ref().and_then(|i|i.color_backend())}else{None};
        #[cfg(not(windows))]
        let conversion:Option<&str>=None;
        Some(serde_json::json!({"layout":format!("{:?}",gpu.layout),"yuv_color":format!("{:?}",gpu.color),"conversion":conversion}))
    }
    pub fn paint_diagnostics(&self) -> Option<serde_json::Value> {
        self.gpu.as_ref()?.lock().unwrap().paint_timings.report()
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
    /// Native composition is useful only for actual D3D images. Software
    /// frames and unsupported sharing keep the regular GL presentation path.
    #[cfg(windows)]
    pub fn native_gpu_source(&self)->bool {
        self.gpu.as_ref().is_some_and(|gpu|{
            let state=gpu.lock().unwrap();
            !state.native_disabled&&(state.native_active||state.d3d_active||state.pending.as_ref().is_some_and(|(frame,..)|frame.gpu&&frame.d3d11.is_some()))
        })
    }
    pub fn set_native_cursor(&self,state:Option<uu_core::native_cursor::Snapshot>) {
        if let Some(gpu)=&self.gpu {gpu.lock().unwrap().cursor.state=state.clone();}
        *self.cursor.lock().unwrap()=state;
    }
    /// Call on the UI thread with the window's current GL context, once at disconnect.
    /// This draws only remote pixels, including imported hardware frames.
    pub fn snapshot(&self)->Result<egui::ColorImage,String> {
        if self.size.contains(&0){return Err(uu_i18n::text("尚无串流画面").into());}
        let ratio=(480.0/self.size[0] as f64).min(270.0/self.size[1] as f64).min(1.);
        let size=[(self.size[0] as f64*ratio).max(1.) as usize,(self.size[1] as f64*ratio).max(1.) as usize];
        if let Some(gpu)=&self.gpu {
            let mut state=gpu.lock().unwrap();
            #[cfg(windows)]
            state.restore_native_frame();
            if state.pending.is_some()||state.size!=[0,0] {return unsafe{state.snapshot(size,self.source_white_nits)};}
        }
        let image=self.fallback_pixels.as_ref().ok_or(uu_i18n::text("没有可保存的远端画面"))?;
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
                let ctx=ui.ctx().clone();let viewport=ctx.viewport_id();let source_white=self.source_white_nits;
                ui.painter().add(egui::PaintCallback {
                    rect,
                    callback: Arc::new(eframe::egui_glow::CallbackFn::new(move |info, _| unsafe {
                        let color=eframe::glow_presentation::color_target_for(&ctx,viewport);
                        let mut state=gpu.lock().unwrap();
                        let new_image=state.pending.is_some();
                        #[cfg(windows)]
                        if eframe::glow_presentation::native_video_available(&ctx,viewport) {
                            let started=std::time::Instant::now();let pixels=info.viewport_in_pixels();
                            let rect=[pixels.left_px as f32,pixels.top_px as f32,pixels.width_px as f32,pixels.height_px as f32];
                            if let Some(image)=state.native_image(rect,source_white) {
                                match eframe::glow_presentation::native_video(&ctx,viewport,Arc::new(image)) {
                                    Ok(true)=>{
                                        if !eframe::glow_presentation::ui_reused(&ctx,viewport) {
                                        let gl=&state.gl;let mut scissor=[0;4];gl.get_parameter_i32_slice(glow::SCISSOR_BOX,&mut scissor);
                                        gl.enable(glow::SCISSOR_TEST);gl.scissor(pixels.left_px,pixels.from_bottom_px,pixels.width_px,pixels.height_px);gl.clear_color(0.,0.,0.,0.);gl.clear(glow::COLOR_BUFFER_BIT);gl.scissor(scissor[0],scissor[1],scissor[2],scissor[3]);
                                        }
                                        state.paint_timings.record(new_image,[started.elapsed().as_secs_f64()*1000.,0.,0.,0.,0.]);return;
                                    },
                                    Ok(false)=>{},Err(e)=>{eprintln!("native video composition unavailable: {e}");state.native_disabled=true;},
                                }
                                state.restore_native_frame();
                            }
                        }
                        #[cfg(windows)]
                        {
                            if eframe::glow_presentation::ui_reused(&ctx,viewport){eframe::glow_presentation::invalidate_ui(&ctx,viewport);}
                            state.restore_native_frame();
                        }
                        if let Some(phases)=state.paint(color.is_some_and(|c|c.hdr),color.map_or(80.,|c|c.sdr_white_nits),source_white) {
                            state.paint_timings.record(new_image,phases);
                        }
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
            if let (Some(cursor),Some(background))=(&*self.cursor.lock().unwrap(),&self.fallback_pixels) {
                self.fallback_cursor.lock().unwrap().paint(ui,rect,cursor,background);
            }
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
        PixelFormat::P010=>(w*2,s*h*3/2),
        PixelFormat::ScRgb16=>(w*8,s*h),
        PixelFormat::Nv12 => (w, s * h + s * (h / 2)),
        PixelFormat::I420 => (w, s * h + w * h / 2),
        PixelFormat::Bgra8 => (w * 4, s * h),
    };
    s >= row
        && (frame.format != PixelFormat::Bgra8 || s % 4 == 0)
        && (frame.format != PixelFormat::Nv12 || s % 2 == 0)
        && (frame.format != PixelFormat::P010 || s % 4 == 0)
        && (frame.format != PixelFormat::ScRgb16 || s % 8 == 0)
        && frame.data.len() >= len
}
struct Planes {
    cursor:video_cursor::Textures,
    gl: Arc<glow::Context>,
    program: glow::Program,
    layout_uniform: Option<glow::UniformLocation>,
    coefficients_uniform: Option<glow::UniformLocation>,
    levels_uniform: Option<glow::UniformLocation>,
    hdr_input_uniform: Option<glow::UniformLocation>,
    hdr_output_uniform: Option<glow::UniformLocation>,
    sdr_white_uniform: Option<glow::UniformLocation>,
    source_white_uniform: Option<glow::UniformLocation>,
    vao: glow::VertexArray,
    textures: [glow::Texture; 3],
    size: [u32; 2],
    layout: Option<PixelFormat>,
    color: YuvColor,
    paint_timings: video_timing::Timings,
    pending: Option<(RawFrame, YuvColor,Option<FrameObservation>)>,
    uv_scale_uniform: Option<glow::UniformLocation>,
    dma_active: bool,
    d3d_active: bool,
    #[cfg(windows)]
    d3d_importer: Option<d3d11_renderer::Importer>,
    #[cfg(windows)]
    native:Option<native_source::Source>,
    #[cfg(windows)]
    native_last:Option<(RawFrame,YuvColor,Option<FrameObservation>)>,
    #[cfg(windows)]
    native_active:bool,
    #[cfg(windows)]
    native_disabled:bool,
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
        // Locations belong to the linked program, including when HDR is off.
        // Resolve them once, like the existing SDR shader uniforms.
        let hdr_input_uniform = gl.get_uniform_location(program, "hdr_input");
        let hdr_output_uniform = gl.get_uniform_location(program, "hdr_output");
        let sdr_white_uniform = gl.get_uniform_location(program, "sdr_white");
        let source_white_uniform = gl.get_uniform_location(program, "source_white");
        let previous_program = gl.get_parameter_program(glow::CURRENT_PROGRAM);
        gl.use_program(Some(program));
        for (unit, name) in ["plane0", "plane1", "plane2"].iter().enumerate() {
            gl.uniform_1_i32(gl.get_uniform_location(program, name).as_ref(), unit as i32);
        }
        let cursor=video_cursor::Textures::new(&gl,program);
        gl.use_program(previous_program);
        #[cfg(target_os = "linux")]
        let importer = dma_buf_renderer::Importer::new(gl.clone());
        #[cfg(windows)]
        let d3d_importer = d3d11_renderer::Importer::new(gl.clone());
        let uv_scale_uniform = gl.get_uniform_location(program, "video_uv_layout");
        Ok(Self {
            cursor,
            gl,
            program,
            layout_uniform,
            coefficients_uniform,
            levels_uniform,
            hdr_input_uniform,
            hdr_output_uniform,
            sdr_white_uniform,
            source_white_uniform,
            vao,
            textures: textures.try_into().unwrap(),
            size: [0, 0],
            layout: None,
            color: YuvColor::default(),
            paint_timings: Default::default(),
            pending: None,
            uv_scale_uniform,
            dma_active: false,
            d3d_active: false,
            #[cfg(windows)]
            d3d_importer,
            #[cfg(windows)]
            native:None,
            #[cfg(windows)]
            native_last:None,
            #[cfg(windows)]
            native_active:false,
            #[cfg(windows)]
            native_disabled:false,
            #[cfg(target_os = "linux")]
            importer,
        })
    }
    unsafe fn snapshot(&mut self,size:[usize;2],source_white:f32)->Result<egui::ColorImage,String> {
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
            if gl.check_framebuffer_status(glow::FRAMEBUFFER)!=glow::FRAMEBUFFER_COMPLETE {return Err(uu_i18n::text("无法建立远端预览缓冲").into());}
            gl.viewport(0,0,size[0] as i32,size[1] as i32);gl.disable(glow::SCISSOR_TEST);gl.disable(glow::BLEND);
            for unit in 0..3 {gl.bind_sampler(unit,None);}
            self.paint(false,80.,source_white);
            if self.size==[0,0]{return Err(uu_i18n::text("远端 GPU 画面暂不可用").into());}
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER,None);gl.pixel_store_i32(glow::PACK_ALIGNMENT,1);
            gl.pixel_store_i32(glow::PACK_ROW_LENGTH,0);gl.pixel_store_i32(glow::PACK_SKIP_PIXELS,0);gl.pixel_store_i32(glow::PACK_SKIP_ROWS,0);
            let mut rgba=vec![0;size[0]*size[1]*4];
            gl.read_pixels(0,0,size[0] as i32,size[1] as i32,glow::RGBA,glow::UNSIGNED_BYTE,glow::PixelPackData::Slice(&mut rgba));
            if gl.get_error()!=glow::NO_ERROR{return Err(uu_i18n::text("读取远端预览失败").into());}
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
        self.upload_pixels(unit,w,h,channels,1,false,row_bytes,data,reallocate);
    }
    unsafe fn upload_pixels(&self,unit:usize,w:u32,h:u32,channels:u32,sample_bytes:u32,floating:bool,row_bytes:u32,data:&[u8],reallocate:bool) {
        let gl = &self.gl;
        gl.active_texture(glow::TEXTURE0 + unit as u32);
        gl.bind_texture(glow::TEXTURE_2D, Some(self.textures[unit]));
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, (row_bytes / (channels*sample_bytes)) as i32);
        let (internal,format)=match (channels,sample_bytes,floating) {
            (1,2,_) =>(glow::R16,glow::RED),
            (2,2,_) =>(glow::RG16,glow::RG),
            (4,2,true)=>(glow::RGBA16F,glow::RGBA),
            (1,_,_)=>(glow::R8,glow::RED),
            (2,_,_)=>(glow::RG8,glow::RG),
            _=>(glow::RGBA8,glow::RGBA),
        };
        let scalar=if floating{glow::HALF_FLOAT}else if sample_bytes==2{glow::UNSIGNED_SHORT}else{glow::UNSIGNED_BYTE};
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
                scalar,
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
                scalar,
                glow::PixelUnpackData::Slice(data),
            );
        }
        gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
    }
    #[cfg(windows)]
    fn restore_native_frame(&mut self){
        if self.native_active {if self.pending.is_none(){self.pending=self.native_last.take();}else{self.native_last=None;}self.native_active=false;self.native.take();}
    }
    #[cfg(windows)]
    fn native_image(&mut self,rect:[f32;4],source_white:f32)->Option<NativeImage>{
        if self.native_disabled{return None;}
        if let Some((frame,color,observation))=self.pending.as_ref(){
            let Some(surface)=frame.d3d11.as_ref().filter(|_|frame.gpu) else{self.restore_native_frame();return None;};
            let prepared=(||->Result<(),String>{
                if self.native.as_ref().is_none_or(|n|!n.matches(surface)){self.native.take();self.native=Some(native_source::Source::new(surface)?);}
                self.native.as_mut().unwrap().set(surface.clone(),*color,observation.clone())
            })();
            if let Err(e)=prepared {eprintln!("native video image unavailable, using WGL: {e}");self.native_disabled=true;self.native.take();self.native_active=false;return None;}
            let frame=self.pending.take().unwrap();self.size=[frame.0.width,frame.0.height];self.layout=Some(frame.0.format);self.color=frame.1;
            self.native_last=Some(frame);self.native_active=true;self.d3d_active=false;
        }
        self.native_active.then(||self.native.as_ref().unwrap().image(rect,self.color.hdr(),source_white,self.cursor.state.clone()))
    }
    unsafe fn paint(&mut self,hdr_output:bool,white_nits:f32,source_white:f32) -> Option<[f64;5]> {
        let started=std::time::Instant::now();
        if let Some((f, color, observation)) = self.pending.take() {
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
                            self.pending = Some((f, color,observation));
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
                    PixelFormat::P010=>{
                        self.upload_pixels(0,f.width,f.height,1,2,false,f.stride,&f.data[..y],changed);
                        self.upload_pixels(1,f.width/2,f.height/2,2,2,false,f.stride,&f.data[y..],changed);
                    },
                    PixelFormat::ScRgb16=>self.upload_pixels(0,f.width,f.height,4,2,true,f.stride,&f.data,changed),
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
        let uploaded=started.elapsed().as_secs_f64();
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
                    return None;
                }
            }
        }
        if self.size == [0, 0] {
            return None;
        }
        let bound=started.elapsed().as_secs_f64();
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
                    Some(PixelFormat::P010)=>5,
                    Some(PixelFormat::ScRgb16)=>4,
                    Some(PixelFormat::Nv12) => 0,
                    Some(PixelFormat::I420) => 1,
                    _ => 2,
                }
            },
        );
        let (levels,matrix)=self.color.shader_conversion(self.layout==Some(PixelFormat::P010)&&!self.d3d_active);
        gl.uniform_4_f32(self.coefficients_uniform.as_ref(),matrix[0],matrix[1],matrix[2],matrix[3]);
        gl.uniform_4_f32(self.levels_uniform.as_ref(),levels[0],levels[1],levels[2],levels[3]);
        gl.uniform_1_i32(self.hdr_input_uniform.as_ref(),i32::from(self.color.hdr() || self.layout==Some(PixelFormat::ScRgb16)));
        gl.uniform_1_i32(self.hdr_output_uniform.as_ref(),i32::from(hdr_output));
        gl.uniform_1_f32(self.sdr_white_uniform.as_ref(),white_nits/80.);
        gl.uniform_1_f32(self.source_white_uniform.as_ref(),source_white);
        self.cursor.bind(gl);
        let configured=started.elapsed().as_secs_f64();
        gl.draw_arrays(glow::TRIANGLES, 0, 3);
        let drawn=started.elapsed().as_secs_f64();
        #[cfg(target_os = "linux")]
        if self.dma_active {
            if let Some(importer) = &mut self.importer {
                importer.sampled();
            }
        }
        // egui restores its program, VAO and blend state after the callback.
        // Restore texture unit and pixel unpack state explicitly.
        gl.active_texture(glow::TEXTURE0);
        let finished=started.elapsed().as_secs_f64();
        Some([uploaded,bound-uploaded,configured-bound,drawn-configured,finished-drawn].map(|v|v*1000.))
    }
}
impl Drop for Planes {
    fn drop(&mut self) {
        unsafe {
            self.cursor.destroy(&self.gl);
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
