//! BGRA capture -> scaled NV12 -> MF sample, without CPU pixel access.
use super::Settings;
use crate::{RawFrame,PixelFormat};
use std::sync::atomic::{AtomicU64,Ordering};
use std::time::Instant;
use std::mem::ManuallyDrop;
use windows::{core::Interface,Win32::{Foundation::RECT,Graphics::{Direct3D11::*,Dxgi::Common::*},Media::MediaFoundation::*}};

/// Conversion-specific resources share the AMF pool's output/release contract.
pub(super) enum AmfTarget {
    ColorConverter(crate::d3d11_color::EncodeTarget),
    VideoProcessor(VideoTarget),
}
pub(super) struct VideoTarget {
    texture:ID3D11Texture2D,
    view:ID3D11VideoProcessorOutputView,
    device:ID3D11Device,
    enumerator:ID3D11VideoProcessorEnumerator,
}
#[derive(Default)]
struct VpProfile {
    allocations:AtomicU64, views:AtomicU64, fresh:AtomicU64, pooled:AtomicU64,
    prepare_ns:AtomicU64, convert_ns:AtomicU64,
}
impl VpProfile {
    fn add(counter:&AtomicU64,value:u64){counter.fetch_add(value,Ordering::Relaxed);}
    fn get(counter:&AtomicU64)->u64{counter.load(Ordering::Relaxed)}
}

pub(super) struct Input {
    device:ID3D11Device,
    context:ID3D11DeviceContext,
    video:ID3D11VideoDevice,
    video_context:ID3D11VideoContext,
    enumerator:ID3D11VideoProcessorEnumerator,
    processor:ID3D11VideoProcessor,
    pub manager:IMFDXGIDeviceManager,
    source:[u32;2],
    source_format:DXGI_FORMAT,
    pixel_format:PixelFormat,
    profile:Option<VpProfile>,
    settings:Settings,
    hdr:Option<crate::d3d11_color::Converter>,
}
impl Input {
    pub fn amf_path(&self)->&'static str {if self.hdr.is_some(){"color-converter"}else{"video-processor"}}
    pub fn amf_target(&self)->Result<AmfTarget,String> {
        if let Some(converter)=&self.hdr {return converter.target().map(AmfTarget::ColorConverter);}
        self.video_target().map(AmfTarget::VideoProcessor).map_err(|e|e.to_string())
    }
    pub fn amf_convert(&self,frame:&RawFrame,target:&AmfTarget)->Result<ID3D11Texture2D,String> {
        let surface=frame.d3d11.as_ref().ok_or("missing AMF GPU input")?;
        let (texture,device)=surface.capture_texture()?;
        self.validate_source(frame,texture,device).map_err(|e|e.to_string())?;
        let guard=surface.capture_read()?;
        let output=match (&self.hdr,target) {
            (Some(converter),AmfTarget::ColorConverter(target))=>converter.convert_into(texture,target),
            (None,AmfTarget::VideoProcessor(target))=>self.video_convert_into(frame,target,true).map_err(|e|e.to_string()),
            _=>Err("AMF target conversion path changed".into()),
        };
        if let Some(guard)=guard{guard.finish()?;}output
    }
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
            Ok(Self{device,context,video,video_context,enumerator,processor,manager,source,source_format:desc.Format,pixel_format:frame.format,profile:std::env::var_os("UU_ENCODE_PROFILE").map(|_|VpProfile::default()),settings,hdr})
        }
    }
    pub fn sample(&self,frame:&RawFrame)->Result<IMFSample,String> {
        let surface=frame.d3d11.as_ref().ok_or("missing GPU input")?;
        let guard=surface.capture_read()?;
        let result=self.sample_locked(frame);
        if let Some(guard)=guard{guard.finish()?;}result
    }
    pub fn device_identity(&self)->Result<super::DeviceIdentity,String>{super::input_device::device_identity(&self.device)}
    fn sample_locked(&self,frame:&RawFrame)->Result<IMFSample,String> {
        if let Some(converter)=&self.hdr {
            if !frame.is_consistent() || (frame.width,frame.height)!=(self.settings.width,self.settings.height) {return Err("HDR source geometry changed".into());}
            let surface=frame.d3d11.as_ref().ok_or("missing HDR source")?;
            let (texture,_)=surface.capture_texture()?;
            let output=converter.convert(texture,0,false)?;
            return unsafe{make_sample(&output,frame,self.settings.fps)}.map_err(|e|e.to_string());
        }
        self.convert(frame).map_err(|e|format!("D3D11 BGRA/NV12 conversion: {e}"))
    }
    fn validate_source(&self,frame:&RawFrame,texture:&ID3D11Texture2D,device:&ID3D11Device)->windows::core::Result<()> {
        let mut desc=D3D11_TEXTURE2D_DESC::default();unsafe{texture.GetDesc(&mut desc);}
        if device.as_raw()!=self.device.as_raw() || [desc.Width,desc.Height]!=self.source
            || desc.Format!=self.source_format || frame.format!=self.pixel_format
            || !frame.is_consistent() || (frame.width,frame.height)!=(self.settings.width,self.settings.height) {
            return Err(windows::core::Error::empty());
        }
        Ok(())
    }
    fn video_target(&self)->windows::core::Result<VideoTarget> {
        let started=self.profile.as_ref().map(|_|Instant::now());
        unsafe {
            let mut texture=None;let mut view=None;
            self.device.CreateTexture2D(&D3D11_TEXTURE2D_DESC{
                Width:self.settings.width,Height:self.settings.height,MipLevels:1,ArraySize:1,
                Format:DXGI_FORMAT_NV12,SampleDesc:DXGI_SAMPLE_DESC{Count:1,Quality:0},
                Usage:D3D11_USAGE_DEFAULT,BindFlags:D3D11_BIND_RENDER_TARGET.0 as u32,..Default::default()
            },None,Some(&mut texture))?;
            let texture=texture.ok_or_else(windows::core::Error::empty)?;
            self.video.CreateVideoProcessorOutputView(&texture,&self.enumerator,&D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC{
                ViewDimension:D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous:D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0{Texture2D:D3D11_TEX2D_VPOV{MipSlice:0}},
            },Some(&mut view))?;
            let view=view.ok_or_else(windows::core::Error::empty)?;
            if let (Some(profile),Some(started))=(&self.profile,started){
                VpProfile::add(&profile.allocations,1);VpProfile::add(&profile.views,1);
                VpProfile::add(&profile.prepare_ns,started.elapsed().as_nanos() as u64);
            }
            Ok(VideoTarget{texture,view,device:self.device.clone(),enumerator:self.enumerator.clone()})
        }
    }
    fn video_convert_into(&self,frame:&RawFrame,target:&VideoTarget,pooled:bool)->windows::core::Result<ID3D11Texture2D> {
        let started=self.profile.as_ref().map(|_|Instant::now());
        unsafe {
            let surface=frame.d3d11.as_ref().ok_or_else(windows::core::Error::empty)?;
            let (texture,device)=surface.capture_texture().map_err(|_|windows::core::Error::empty())?;
            self.validate_source(frame,texture,device)?;
            let mut desc=D3D11_TEXTURE2D_DESC::default();target.texture.GetDesc(&mut desc);
            if target.device.as_raw()!=self.device.as_raw() || target.enumerator.as_raw()!=self.enumerator.as_raw()
                || [desc.Width,desc.Height]!=[self.settings.width,self.settings.height] || desc.Format!=DXGI_FORMAT_NV12 {
                return Err(windows::core::Error::empty());
            }
            let mut input_view=None;
            self.video.CreateVideoProcessorInputView(texture,&self.enumerator,&D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC{
                FourCC:0,ViewDimension:D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous:D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0{Texture2D:D3D11_TEX2D_VPIV{MipSlice:0,ArraySlice:0}},
            },Some(&mut input_view))?;
            let mut stream=D3D11_VIDEO_PROCESSOR_STREAM{Enable:true.into(),pInputSurface:ManuallyDrop::new(input_view),..Default::default()};
            let converted=self.video_context.VideoProcessorBlt(&self.processor,&target.view,0,std::slice::from_ref(&stream));
            ManuallyDrop::drop(&mut stream.pInputSurface);
            converted?;
            self.context.Flush();
            if let (Some(profile),Some(started))=(&self.profile,started){
                VpProfile::add(if pooled{&profile.pooled}else{&profile.fresh},1);
                VpProfile::add(&profile.convert_ns,started.elapsed().as_nanos() as u64);
            }
            Ok(target.texture.clone())
        }
    }
    fn convert(&self,frame:&RawFrame)->windows::core::Result<IMFSample> {
        // Ordinary MF retains a fresh, independently owned target per sample.
        let target=self.video_target()?;
        let output=self.video_convert_into(frame,&target,false)?;
        unsafe{make_sample(&output,frame,self.settings.fps)}
    }
}
impl Drop for Input {fn drop(&mut self){
    if let Some(p)=&self.profile {
        eprintln!("uu-gpu-input-profile path={} source_format={:?} source_pixel={:?} output_format={} width={} height={} device={:p} target_allocations={} output_views={} fresh_frames={} pooled_frames={} prepare_ns={} convert_ns={}",
            self.amf_path(),self.source_format,self.pixel_format,if self.settings.hdr{"P010"}else{"NV12"},self.settings.width,self.settings.height,self.device.as_raw(),VpProfile::get(&p.allocations),VpProfile::get(&p.views),VpProfile::get(&p.fresh),VpProfile::get(&p.pooled),VpProfile::get(&p.prepare_ns),VpProfile::get(&p.convert_ns));
    }
}}

unsafe fn make_sample(output:&ID3D11Texture2D,frame:&RawFrame,fps:u32)->windows::core::Result<IMFSample> {
    let sample=unsafe{MFCreateSample()?};
    unsafe {
        sample.AddBuffer(&MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID,output,0,false)?)?;
        let time=frame.pts.0.checked_mul(10).and_then(|t|i64::try_from(t).ok()).ok_or_else(windows::core::Error::empty)?;
        sample.SetSampleTime(time)?;sample.SetSampleDuration(10_000_000/i64::from(fps))?;
    }
    Ok(sample)
}
