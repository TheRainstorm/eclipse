//! BGRA capture -> scaled NV12 -> MF sample, without CPU pixel access.
use super::Settings;
use crate::RawFrame;
use std::mem::ManuallyDrop;
use windows::{core::Interface,Win32::{Foundation::RECT,Graphics::{Direct3D11::*,Dxgi::Common::*},Media::MediaFoundation::*}};

pub(super) struct Input {
    device:ID3D11Device,
    context:ID3D11DeviceContext,
    video:ID3D11VideoDevice,
    video_context:ID3D11VideoContext,
    enumerator:ID3D11VideoProcessorEnumerator,
    processor:ID3D11VideoProcessor,
    pub manager:IMFDXGIDeviceManager,
    source:[u32;2],
    settings:Settings,
    hdr:Option<crate::d3d11_color::Converter>,
}
impl Input {
    pub fn new(frame:&RawFrame,settings:Settings)->Result<Self,String> {
        Self::create(frame,settings).map_err(|e|format!("GPU encoder input: {e}"))
    }
    fn create(frame:&RawFrame,settings:Settings)->windows::core::Result<Self> {
        unsafe {
            let surface=frame.d3d11.as_ref().ok_or_else(windows::core::Error::empty)?;
            let (texture,device)=surface.capture_texture().map_err(|_|windows::core::Error::empty())?;
            let device=device.clone();
            let context=device.GetImmediateContext()?;
            let video:ID3D11VideoDevice=device.cast()?;
            let video_context:ID3D11VideoContext=context.cast()?;
            let mut desc=D3D11_TEXTURE2D_DESC::default();texture.GetDesc(&mut desc);
            let source=[desc.Width,desc.Height];
            let hdr=if settings.hdr || frame.format==crate::PixelFormat::ScRgb16 {
                Some(crate::d3d11_color::Converter::new(&device,[settings.width,settings.height],false,settings.hdr,false,settings.source_hdr.map_or(203.,|m|m.sdr_white_nits))
                    .map_err(|e|{tracing::warn!(%e,"HDR GPU conversion unavailable");windows::core::Error::empty()})?)
            }else{None};
            let rate=DXGI_RATIONAL{Numerator:settings.fps,Denominator:1};
            let enumerator=video.CreateVideoProcessorEnumerator(&D3D11_VIDEO_PROCESSOR_CONTENT_DESC{
                InputFrameFormat:D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,InputFrameRate:rate,
                InputWidth:source[0],InputHeight:source[1],OutputFrameRate:rate,
                OutputWidth:settings.width,OutputHeight:settings.height,Usage:D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
            })?;
            let processor=video.CreateVideoProcessor(&enumerator,0)?;
            let src=RECT{left:0,top:0,right:source[0] as i32,bottom:source[1] as i32};
            let dst=RECT{left:0,top:0,right:settings.width as i32,bottom:settings.height as i32};
            video_context.VideoProcessorSetStreamFrameFormat(&processor,0,D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor,0,false);
            video_context.VideoProcessorSetStreamSourceRect(&processor,0,true,Some(&src));
            video_context.VideoProcessorSetStreamDestRect(&processor,0,true,Some(&dst));
            video_context.VideoProcessorSetOutputTargetRect(&processor,true,Some(&dst));
            video_context.VideoProcessorSetStreamColorSpace(&processor,0,&D3D11_VIDEO_PROCESSOR_COLOR_SPACE{_bitfield:2<<4});
            video_context.VideoProcessorSetOutputColorSpace(&processor,&D3D11_VIDEO_PROCESSOR_COLOR_SPACE{_bitfield:1<<4});
            if let Ok(v1)=video_context.cast::<ID3D11VideoContext1>() {
                v1.VideoProcessorSetStreamColorSpace1(&processor,0,DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
                v1.VideoProcessorSetOutputColorSpace1(&processor,DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601);
            }
            let mut token=0;let mut manager=None;
            MFCreateDXGIDeviceManager(&mut token,&mut manager)?;
            let manager=manager.ok_or_else(windows::core::Error::empty)?;
            manager.ResetDevice(&device,token)?;
            Ok(Self{device,context,video,video_context,enumerator,processor,manager,source,settings,hdr})
        }
    }
    pub fn sample(&self,frame:&RawFrame)->Result<IMFSample,String> {
        if let Some(converter)=&self.hdr {
            if !frame.is_consistent() || (frame.width,frame.height)!=(self.settings.width,self.settings.height) {return Err("HDR source geometry changed".into());}
            let surface=frame.d3d11.as_ref().ok_or("missing HDR source")?;
            let (texture,_)=surface.capture_texture()?;
            let output=converter.convert(texture,0,false)?;
            return unsafe{make_sample(&output,frame,self.settings.fps)}.map_err(|e|e.to_string());
        }
        self.convert(frame).map_err(|e|format!("D3D11 BGRA/NV12 conversion: {e}"))
    }
    fn convert(&self,frame:&RawFrame)->windows::core::Result<IMFSample> {
        unsafe {
            let surface=frame.d3d11.as_ref().ok_or_else(windows::core::Error::empty)?;
            let (texture,device)=surface.capture_texture().map_err(|_|windows::core::Error::empty())?;
            let mut desc=D3D11_TEXTURE2D_DESC::default();texture.GetDesc(&mut desc);
            if device.as_raw()!=self.device.as_raw() || [desc.Width,desc.Height]!=self.source
                || !frame.is_consistent() || (frame.width,frame.height)!=(self.settings.width,self.settings.height) {
                return Err(windows::core::Error::empty());
            }
            // Each sample owns its texture. NeedInput/output does not promise
            // that an async MFT has released all references to an older input.
            let mut output=None;
            self.device.CreateTexture2D(&D3D11_TEXTURE2D_DESC{
                Width:self.settings.width,Height:self.settings.height,MipLevels:1,ArraySize:1,
                Format:DXGI_FORMAT_NV12,SampleDesc:DXGI_SAMPLE_DESC{Count:1,Quality:0},
                Usage:D3D11_USAGE_DEFAULT,BindFlags:D3D11_BIND_RENDER_TARGET.0 as u32,..Default::default()
            },None,Some(&mut output))?;
            let output=output.ok_or_else(windows::core::Error::empty)?;
            let mut input_view=None;let mut output_view=None;
            self.video.CreateVideoProcessorInputView(texture,&self.enumerator,&D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC{
                FourCC:0,ViewDimension:D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous:D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0{Texture2D:D3D11_TEX2D_VPIV{MipSlice:0,ArraySlice:0}},
            },Some(&mut input_view))?;
            self.video.CreateVideoProcessorOutputView(&output,&self.enumerator,&D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC{
                ViewDimension:D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous:D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0{Texture2D:D3D11_TEX2D_VPOV{MipSlice:0}},
            },Some(&mut output_view))?;
            let mut stream=D3D11_VIDEO_PROCESSOR_STREAM{Enable:true.into(),pInputSurface:ManuallyDrop::new(input_view),..Default::default()};
            let converted=self.video_context.VideoProcessorBlt(&self.processor,output_view.as_ref().ok_or_else(windows::core::Error::empty)?,0,std::slice::from_ref(&stream));
            ManuallyDrop::drop(&mut stream.pInputSurface);
            converted?;
            self.context.Flush();
            let sample=MFCreateSample()?;
            sample.AddBuffer(&MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID,&output,0,false)?)?;
            let time=frame.pts.0.checked_mul(10).and_then(|t|i64::try_from(t).ok()).ok_or_else(windows::core::Error::empty)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(10_000_000/i64::from(self.settings.fps))?;
            Ok(sample)
        }
    }
}

unsafe fn make_sample(output:&ID3D11Texture2D,frame:&RawFrame,fps:u32)->windows::core::Result<IMFSample> {
    let sample=unsafe{MFCreateSample()?};
    unsafe {
        sample.AddBuffer(&MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID,output,0,false)?)?;
        let time=frame.pts.0.checked_mul(10).and_then(|t|i64::try_from(t).ok()).ok_or_else(windows::core::Error::empty)?;
        sample.SetSampleTime(time)?;sample.SetSampleDuration(10_000_000/i64::from(fps))?;
    }
    Ok(sample)
}
