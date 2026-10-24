//! Optional output scaling keeps the capture session and physical input mapping.
use crate::{Capture,CaptureError,DesktopRect};
use uu_media::frame::{RawFrame,PixelFormat};

pub fn dimensions(source:(u32,u32),bounds:Option<[u32;2]>)->(u32,u32) {
    let (w,h)=source;
    let Some([bw,bh])=bounds else{return source;};
    if w<=bw&&h<=bh {return source;}
    let scale=(bw as f64/w as f64).min(bh as f64/h as f64);
    (((w as f64*scale) as u32/2*2).max(2),((h as f64*scale) as u32/2*2).max(2))
}
pub fn bounded(inner:Box<dyn Capture>,bounds:Option<[u32;2]>)->Box<dyn Capture> {
    let output=dimensions(inner.resolution(),bounds);
    if output==inner.resolution(){inner}else{Box::new(Scaled{inner,output})}
}
struct Scaled {inner:Box<dyn Capture>,output:(u32,u32)}
impl Capture for Scaled {
    fn frame_info(&self)->Option<crate::frame_info::FrameInfo>{self.inner.frame_info()}
    fn hdr_metadata(&self)->Option<uu_media::hdr::Metadata>{self.inner.hdr_metadata()}
    fn encoder_input_device(&self)->Option<uu_media::encoder::InputDevice>{self.inner.encoder_input_device()}
    fn set_frame_rate(&mut self,fps:u32)->Result<(),CaptureError>{self.inner.set_frame_rate(fps)}
    fn prefer_gpu(&mut self,enabled:bool,vendor:Option<u16>){self.inner.prefer_gpu(enabled,vendor);}
    fn resolution(&self)->(u32,u32){self.output}
    fn desktop_rect(&self)->Option<DesktopRect>{self.inner.desktop_rect()}
    fn input_injector(&self)->Option<Box<dyn uu_input::InputInjector>>{self.inner.input_injector()}
    fn next_frame(&mut self)->Result<RawFrame,CaptureError> {
        let frame=self.inner.next_frame()?;
        if frame.gpu && frame.d3d11.is_some() {
            // BGRA capture surfaces retain their source geometry; the hardware
            // encoder scales directly to these negotiated output dimensions.
            let (width,height)=self.output;
            return Ok(RawFrame{width,height,stride:frame.format.row_bytes(width),..frame});
        }
        if frame.gpu||!frame.is_consistent()||frame.width%2!=0||frame.height%2!=0 {
            return Err(CaptureError::Backend("采集画面缺少可缩放的 CPU 像素".into()));
        }
        if matches!(frame.format,PixelFormat::P010|PixelFormat::ScRgb16) {return Err(CaptureError::Backend("HDR scaling requires GPU storage".into()));}
        let (w,h)=self.output;let (sw,sh,stride)=(frame.width as usize,frame.height as usize,frame.stride as usize);
        let (w,h)=(w as usize,h as usize);
        let row=if frame.format==PixelFormat::Bgra8 {sw*4}else{sw};
        if stride<row{return Err(CaptureError::Backend("采集画面步长无效".into()));}
        let mut output=vec![0u8;if frame.format==PixelFormat::Bgra8 {w*h*4}else{w*h*3/2}];
        match frame.format {
            PixelFormat::P010|PixelFormat::ScRgb16=>unreachable!(),
            PixelFormat::Bgra8=>plane(&frame.data,stride,sw,sh,&mut output,w*4,w,h,4),
            PixelFormat::Nv12=>{
                plane(&frame.data[..stride*sh],stride,sw,sh,&mut output[..w*h],w,w,h,1);
                plane(&frame.data[stride*sh..],stride,sw/2,sh/2,&mut output[w*h..],w,w/2,h/2,2);
            },
            PixelFormat::I420=>{
                plane(&frame.data[..stride*sh],stride,sw,sh,&mut output[..w*h],w,w,h,1);
                let base=stride*sh;let chroma=sw*sh/4;
                for i in 0..2 {plane(&frame.data[base+i*chroma..base+(i+1)*chroma],sw/2,sw/2,sh/2,&mut output[w*h+i*w*h/4..w*h+(i+1)*w*h/4],w/2,w/2,h/2,1);}
            }
        }
        Ok(RawFrame{width:w as u32,height:h as u32,stride:if frame.format==PixelFormat::Bgra8 {w*4}else{w} as u32,data:output.into(),..frame})
    }
}
// Scale each native plane directly; do not allocate a full-size RGBA conversion.
fn plane(source:&[u8],stride:usize,sw:usize,sh:usize,target:&mut[u8],out_stride:usize,w:usize,h:usize,channels:usize) {
    for y in 0..h {let sy=y*sh/h;for x in 0..w {
        let start=sy*stride+x*sw/w*channels;let dest=y*out_stride+x*channels;
        target[dest..dest+channels].copy_from_slice(&source[start..start+channels]);
    }}
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn padded_pixels_and_physical_input_bounds_survive_scaling() {
        struct Source(RawFrame);
        impl Capture for Source {
            fn resolution(&self)->(u32,u32){(16,8)}
            fn desktop_rect(&self)->Option<DesktopRect>{Some(DesktopRect{x:-200,y:50,width:16,height:8})}
            fn next_frame(&mut self)->Result<RawFrame,CaptureError>{Ok(self.0.clone())}
        }
        let stride=80usize;let mut data=vec![17;stride*8];
        for y in 0..8 {for x in 0..16 {data[y*stride+x*4..y*stride+x*4+4].copy_from_slice(&[x as u8,y as u8,100,255]);}}
        let frame=RawFrame{width:16,height:8,stride:80,format:PixelFormat::Bgra8,data:data.into(),pts:uu_media::Timestamp(12345),gpu:false,dma_buf:None,d3d11:None};
        let mut scaled=bounded(Box::new(Source(frame)),Some([8,4]));let rect=scaled.desktop_rect().unwrap();
        assert_eq!((rect.x,rect.y,rect.width,rect.height),(-200,50,16,8));
        let image=scaled.next_frame().unwrap();assert_eq!(image.pts,uu_media::Timestamp(12345));assert_eq!(image.stride,32);
        for y in 0..4 {for x in 0..8 {assert_eq!(&image.data[(y*8+x)*4..(y*8+x+1)*4],&[x as u8*2,y as u8*2,100,255]);}}
    }
    #[test]
    fn scaling_preserves_aspect_stride_and_timestamp_without_upscaling() {
        assert_eq!(dimensions((3840,2160),Some([1920,1080])),(1920,1080));
        assert_eq!(dimensions((2560,1600),Some([1920,1080])),(1728,1080));
        assert_eq!(dimensions((640,360),Some([3840,2160])),(640,360));
        let inner=Box::new(crate::SyntheticCapture::new(640,360,60));
        let mut capture=bounded(inner,Some([320,180]));let frame=capture.next_frame().unwrap();
        assert_eq!((frame.width,frame.height,frame.stride),(320,180,320));assert!(frame.is_consistent());
        let info=capture.frame_info().unwrap();assert_eq!(info.frame_id,1);assert!(!info.duplicate);
    }
}
