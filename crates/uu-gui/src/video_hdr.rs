//! Render one complete stream viewport through a floating-point scRGB surface.
//! The regular GL backend and other viewports retain their own presentation.
#[path="video_native.rs"]
pub(crate) mod native;
pub(crate) use native::enable as enable_native;
use crate::wgl_interop::{Api, Handle};
use eframe::{
    egui,
    glow::{self, HasContext},
    glow_presentation::{ColorTarget, Presenter},
};
use std::{ptr, sync::Arc};
use uu_core::display_frames::hdr::Presentation;
struct Target {
    texture: glow::Texture,
    framebuffer: glow::Framebuffer,
}
struct HdrPresenter {
    gl: Arc<glow::Context>,
    native: Presentation,
    api: Api,
    device: Handle,
    object: Handle,
    gui: Option<Target>,
    shared: Option<Target>,
    size: [u32; 2],
    program: glow::Program,
    vao: glow::VertexArray,
    white: Option<glow::UniformLocation>,
    color: ColorTarget,
    locked: bool,
}
// eframe's registry is Send; all methods and destruction run on its native GL
// thread with the owning context current, as required by Presenter.
unsafe impl Send for HdrPresenter {}
/// Prefer the verified D3D video path before negotiating HDR capability.
/// A surface on an SDR output stays available for SDR video, but never
/// advertises HDR just because it can allocate a floating-point swap chain.
pub fn enable_for_stream(ctx:&egui::Context,gl:Arc<glow::Context>,vsync:bool)->Result<bool,String>{
    if std::env::var("UU_D3D11_NATIVE_VIDEO").as_deref()!=Ok("0") {
        if let Err(error)=enable_native(ctx,gl.clone(),vsync){eprintln!("native video target unavailable: {error}");}
        if eframe::glow_presentation::native_video_available(ctx,ctx.viewport_id()) {
            return Ok(eframe::glow_presentation::available_color_for(ctx,ctx.viewport_id()).is_some_and(|c|c.hdr));
        }
    }
    enable(ctx,gl,vsync)
}
pub fn enable(ctx: &egui::Context, gl: Arc<glow::Context>, vsync: bool) -> Result<bool, String> {
    if let Some(color) = eframe::glow_presentation::available_color_for(ctx, ctx.viewport_id()) {
        return Ok(color.hdr);
    }
    let key = egui::Id::new(("hdr-target-attempt", ctx.viewport_id()));
    let now = std::time::Instant::now();
    if ctx
        .data(|data| data.get_temp::<std::time::Instant>(key))
        .is_some_and(|last| now.duration_since(last) < std::time::Duration::from_secs(1))
    {
        return Ok(false);
    }
    ctx.data_mut(|data| data.insert_temp(key, now));
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let window = eframe::viewport_window_for(ctx, ctx.viewport_id()).ok_or(uu_i18n::text("串流窗口尚未建立"))?;
    let RawWindowHandle::Win32(handle) =
        window.window_handle().map_err(|e| e.to_string())?.as_raw()
    else {
        return Ok(false);
    };
    let mut presenter = unsafe { HdrPresenter::new(gl, handle.hwnd.get() as Handle)? };
    // Verify texture allocation, FP16 framebuffer completeness and actual WGL
    // registration before advertising HDR to the peer.
    let color = presenter.begin([2, 2])?;
    presenter.finish();
    eframe::glow_presentation::install(ctx, Box::new(presenter), vsync, color);
    Ok(color.hdr)
}
impl HdrPresenter {
    unsafe fn new(gl: Arc<glow::Context>, hwnd: Handle) -> Result<Self, String> {
        let api = Api::load().ok_or(uu_i18n::text("HDR 需要 WGL/D3D11 共享纹理支持"))?;
        let native = Presentation::new(hwnd)?;
        let program = program(&gl)?;
        let vao = match gl.create_vertex_array() {
            Ok(v) => v,
            Err(e) => {
                gl.delete_program(program);
                return Err(e);
            }
        };
        let device = (api.open)(native.device_ptr());
        if device.is_null() {
            gl.delete_vertex_array(vao);
            gl.delete_program(program);
            return Err(uu_i18n::text("HDR 呈现设备不能与 OpenGL 共享纹理").into());
        }
        let white = gl.get_uniform_location(program, "sdr_white");
        Ok(Self {
            gl,
            native,
            api,
            device,
            object: ptr::null_mut(),
            gui: None,
            shared: None,
            size: [0, 0],
            program,
            vao,
            white,
            color: ColorTarget {
                hdr: true,
                sdr_white_nits: 80.,
            },
            locked: false,
        })
    }
    unsafe fn clear_targets(&mut self) {
        if self.locked {
            (self.api.unlock)(self.device, 1, &mut self.object);
            self.locked = false;
        }
        self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        if !self.object.is_null() {
            (self.api.unregister)(self.device, self.object);
            self.object = ptr::null_mut();
        }
        for target in [self.shared.take(), self.gui.take()].into_iter().flatten() {
            self.gl.delete_framebuffer(target.framebuffer);
            self.gl.delete_texture(target.texture);
        }
        self.size = [0, 0];
    }
    unsafe fn resize(&mut self, size: [u32; 2]) -> Result<(), String> {
        if self.size == size {
            return Ok(());
        }
        self.clear_targets();
        self.native.resize(size)?;
        let gui_texture = self.gl.create_texture()?;
        let gui_fbo = match self.gl.create_framebuffer() {
            Ok(v) => v,
            Err(e) => {
                self.gl.delete_texture(gui_texture);
                return Err(e);
            }
        };
        self.gui = Some(Target {
            texture: gui_texture,
            framebuffer: gui_fbo,
        });
        self.gl.active_texture(glow::TEXTURE0);
        self.gl.bind_texture(glow::TEXTURE_2D, Some(gui_texture));
        self.gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::LINEAR as i32,
        );
        self.gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::LINEAR as i32,
        );
        self.gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_S,
            glow::CLAMP_TO_EDGE as i32,
        );
        self.gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_T,
            glow::CLAMP_TO_EDGE as i32,
        );
        self.gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
        self.gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA16F as i32,
            size[0] as i32,
            size[1] as i32,
            0,
            glow::RGBA,
            glow::HALF_FLOAT,
            None,
        );
        self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(gui_fbo));
        self.gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(gui_texture),
            0,
        );
        if self.gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
            return Err(uu_i18n::text("HDR UI 浮点缓冲不完整").into());
        }
        let shared_texture = self.gl.create_texture()?;
        let shared_fbo = match self.gl.create_framebuffer() {
            Ok(v) => v,
            Err(e) => {
                self.gl.delete_texture(shared_texture);
                return Err(e);
            }
        };
        self.shared = Some(Target {
            texture: shared_texture,
            framebuffer: shared_fbo,
        });
        self.object = (self.api.register)(
            self.device,
            self.native.texture_ptr(),
            shared_texture.0.get(),
            glow::TEXTURE_2D,
            2,
        ); // WRITE_DISCARD_NV
        if self.object.is_null() {
            return Err(uu_i18n::text("HDR FP16 纹理注册失败").into());
        }
        if (self.api.lock)(self.device, 1, &mut self.object) == 0 {
            return Err(uu_i18n::text("HDR FP16 纹理锁定失败").into());
        }
        self.locked = true;
        self.gl
            .bind_framebuffer(glow::FRAMEBUFFER, Some(shared_fbo));
        self.gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(shared_texture),
            0,
        );
        if self.gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
            return Err(uu_i18n::text("HDR Windows 浮点缓冲不完整").into());
        }
        if (self.api.unlock)(self.device, 1, &mut self.object) == 0 {
            return Err(uu_i18n::text("HDR FP16 纹理解锁失败").into());
        }
        self.locked = false;
        self.size = size;
        Ok(())
    }
}
impl Presenter for HdrPresenter {
    fn begin(&mut self, size: [u32; 2]) -> Result<ColorTarget, String> {
        let size = size.map(|v| v.max(1));
        unsafe {
            self.resize(size)?;
            self.gl.bind_framebuffer(
                glow::FRAMEBUFFER,
                Some(self.gui.as_ref().unwrap().framebuffer),
            );
        }
        let (hdr, white) = self.native.info();
        self.color = ColorTarget {
            hdr,
            sdr_white_nits: white,
        };
        Ok(self.color)
    }
    fn present(&mut self, vsync: bool) -> Result<(), String> {
        unsafe {
            if (self.api.lock)(self.device, 1, &mut self.object) == 0 {
                return Err(uu_i18n::text("HDR 呈现纹理锁定失败").into());
            }
            self.locked = true;
            let gl = &self.gl;
            gl.bind_framebuffer(
                glow::FRAMEBUFFER,
                Some(self.shared.as_ref().unwrap().framebuffer),
            );
            gl.viewport(0, 0, self.size[0] as i32, self.size[1] as i32);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::FRAMEBUFFER_SRGB);
            gl.color_mask(true, true, true, true);
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_sampler(0, None);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.gui.as_ref().unwrap().texture));
            gl.uniform_1_f32(self.white.as_ref(), self.color.sdr_white_nits / 80.);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            if (self.api.unlock)(self.device, 1, &mut self.object) == 0 {
                return Err(uu_i18n::text("HDR 呈现纹理解锁失败").into());
            }
            self.locked = false;
        }
        self.native.present(vsync)
    }
    fn finish(&mut self) {
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
    }
    fn deactivate(&mut self) -> Result<(), String> {
        self.native.deactivate()
    }
}
impl Drop for HdrPresenter {
    fn drop(&mut self) {
        unsafe {
            self.clear_targets();
            (self.api.close)(self.device);
            self.gl.delete_program(self.program);
            self.gl.delete_vertex_array(self.vao);
        }
    }
}
unsafe fn program(gl: &glow::Context) -> Result<glow::Program, String> {
    let program = gl.create_program()?;
    for (kind, source) in [
        (
            glow::VERTEX_SHADER,
            r#"#version 330
out vec2 uv;
void main(){vec2 p=vec2(float((gl_VertexID<<1)&2),float(gl_VertexID&2));gl_Position=vec4(p*2.0-1.0,0,1);uv=vec2(p.x,1.0-p.y);}"#,
        ),
        (
            glow::FRAGMENT_SHADER,
            r#"#version 330
in vec2 uv;out vec4 result;uniform sampler2D image;uniform float sdr_white;
float linear_channel(float v){float a=abs(v);return sign(v)*(a<=0.04045?a/12.92:pow((a+0.055)/1.055,2.4));}
void main(){vec3 v=texture(image,uv).rgb;result=vec4(vec3(linear_channel(v.r),linear_channel(v.g),linear_channel(v.b))*sdr_white,1);}"#,
        ),
    ] {
        let shader = match gl.create_shader(kind) {
            Ok(v) => v,
            Err(e) => {
                gl.delete_program(program);
                return Err(e);
            }
        };
        gl.shader_source(shader, source);
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
    Ok(program)
}
