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
    fn resolution(&self)->(u32,u32){self.output}
    fn desktop_rect(&self)->Option<DesktopRect>{self.inner.desktop_rect()}
    fn input_injector(&self)->Option<Box<dyn uu_input::InputInjector>>{self.inner.input_injector()}
    fn next_frame(&mut self)->Result<RawFrame,CaptureError> {
        let frame=self.inner.next_frame()?;
        if frame.gpu||!frame.is_consistent()||frame.width%2!=0||frame.height%2!=0 {
            return Err(CaptureError::Backend("采集画面缺少可缩放的 CPU 像素".into()));
        }
        let (w,h)=self.output;let (sw,sh,stride)=(frame.width as usize,frame.height as usize,frame.stride as usize);
        let (w,h)=(w as usize,h as usize);
        let row=if frame.format==PixelFormat::Bgra8 {sw*4}else{sw};
        if stride<row{return Err(CaptureError::Backend("采集画面步长无效".into()));}
        let mut output=vec![0u8;if frame.format==PixelFormat::Bgra8 {w*h*4}else{w*h*3/2}];
        match frame.format {
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
    fn scaling_preserves_aspect_stride_and_timestamp_without_upscaling() {
        assert_eq!(dimensions((3840,2160),Some([1920,1080])),(1920,1080));
        assert_eq!(dimensions((2560,1600),Some([1920,1080])),(1728,1080));
        assert_eq!(dimensions((640,360),Some([3840,2160])),(640,360));
        let inner=Box::new(crate::SyntheticCapture::new(640,360,60));
        let mut capture=bounded(inner,Some([320,180]));let frame=capture.next_frame().unwrap();
        assert_eq!((frame.width,frame.height,frame.stride),(320,180,320));assert!(frame.is_consistent());
    }
}
