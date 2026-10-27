//! Generated-pixel FP16 validation independent of desktop HDR configuration.
use eframe::{egui,glow::{self,HasContext},glow_presentation::{ColorTarget,Presenter}};
use std::sync::Arc;
struct Target {gl:Arc<glow::Context>,texture:glow::Texture,fbo:glow::Framebuffer,size:[u32;2]}
unsafe impl Send for Target {}
impl Presenter for Target {
    fn begin(&mut self,size:[u32;2])->Result<ColorTarget,String>{unsafe{
        self.gl.bind_texture(glow::TEXTURE_2D,Some(self.texture));
        if self.size!=size {
            self.gl.tex_image_2d(glow::TEXTURE_2D,0,glow::RGBA16F as i32,size[0] as i32,size[1] as i32,0,glow::RGBA,glow::HALF_FLOAT,None);
            self.gl.tex_parameter_i32(glow::TEXTURE_2D,glow::TEXTURE_MIN_FILTER,glow::LINEAR as i32);self.gl.tex_parameter_i32(glow::TEXTURE_2D,glow::TEXTURE_MAG_FILTER,glow::LINEAR as i32);self.size=size;
        }
        self.gl.bind_framebuffer(glow::FRAMEBUFFER,Some(self.fbo));self.gl.framebuffer_texture_2d(glow::FRAMEBUFFER,glow::COLOR_ATTACHMENT0,glow::TEXTURE_2D,Some(self.texture),0);
        if self.gl.check_framebuffer_status(glow::FRAMEBUFFER)!=glow::FRAMEBUFFER_COMPLETE{return Err("offscreen FP16 target incomplete".into());}
        Ok(ColorTarget{hdr:true,sdr_white_nits:80.})
    }}
    fn present(&mut self,_:bool)->Result<(),String>{self.finish();Ok(())}
    fn finish(&mut self){unsafe{self.gl.bind_framebuffer(glow::FRAMEBUFFER,None);}}
    fn deactivate(&mut self)->Result<(),String>{self.finish();Ok(())}
}
impl Drop for Target {fn drop(&mut self){unsafe{self.gl.delete_framebuffer(self.fbo);self.gl.delete_texture(self.texture);}}}
pub fn enable(ctx:&egui::Context,gl:Arc<glow::Context>)->Result<bool,String>{
    if eframe::glow_presentation::available_color_for(ctx,ctx.viewport_id()).is_some(){return Ok(true);}
    unsafe{
        let texture=gl.create_texture()?;let fbo=match gl.create_framebuffer(){Ok(f)=>f,Err(e)=>{gl.delete_texture(texture);return Err(e);}};
        let mut target=Target{gl,texture,fbo,size:[0;2]};let color=target.begin([2;2])?;target.finish();
        eframe::glow_presentation::install(ctx,Box::new(target),false,color);
    }
    Ok(true)
}
