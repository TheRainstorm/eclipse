//! Native AMD encoder, using only the installed driver and vendored MIT API headers.
use super::Settings;
use crate::{EncodedFrame,RawFrame};
use std::{ffi::{c_char,c_void,CStr},ptr::{self,NonNull}};
use uu_proto::media::VideoCodec;
use windows::{core::Interface,Win32::{Graphics::Direct3D11::ID3D11Texture2D,Media::MediaFoundation::IMFDXGIBuffer}};
unsafe extern "C" {
    fn eclipse_amf_open(codec:i32,width:u32,height:u32,fps:u32,kbps:u32,peak:u32,vbr:i32,quality:i32,default_adapter:i32,device:*mut c_void,error:*mut c_char,capacity:usize)->*mut c_void;
    fn eclipse_amf_close(session:*mut c_void);
    fn eclipse_amf_device(session:*mut c_void)->*mut c_void;
    fn eclipse_amf_bitrate(session:*mut c_void,kbps:u32,error:*mut c_char,capacity:usize)->i32;
    fn eclipse_amf_encode(session:*mut c_void,texture:*mut c_void,cpu:*const u8,cpu_length:usize,pts:i64,keyframe:i32,
        data:*mut *const u8,len:*mut usize,timestamp:*mut i64,error:*mut c_char,capacity:usize)->i32;
}
fn message(error:&[c_char])->String {unsafe{CStr::from_ptr(error.as_ptr())}.to_string_lossy().into_owned()}
struct Native(NonNull<c_void>);
impl Native {
    fn open(settings:Settings,device:*mut c_void)->Result<Self,String> {
        let mut error=[0;512];
        let codec=match settings.codec {VideoCodec::H264=>0,VideoCodec::Hevc=>1,VideoCodec::Av1=>2};
        let session=unsafe{eclipse_amf_open(codec,settings.width,settings.height,settings.fps,settings.bitrate_kbps,
            settings.rate_control.peak(settings.bitrate_kbps),i32::from(matches!(settings.rate_control,super::RateControl::Vbr{..})),match settings.tuning.amd_quality{super::AmdQuality::Speed=>0,super::AmdQuality::Balanced=>1,super::AmdQuality::Quality=>2},i32::from(settings.preference!=super::Preference::Amd),device,error.as_mut_ptr(),error.len())};
        NonNull::new(session).map(Self).ok_or_else(||message(&error))
    }
}
impl Drop for Native {fn drop(&mut self){unsafe{eclipse_amf_close(self.0.as_ptr());}}}
pub(super) struct Driver {
    native:Native,
    gpu:Option<super::gpu::Input>,
    upload:Vec<u8>,
    settings:Settings,
    headers:super::headers::Headers,
    name:String,
    profile:super::profile::Profile,
    _runtime:super::mf::Runtime,
}
impl Driver {
    pub fn open(settings:Settings)->Result<Self,String> {
        let runtime=super::mf::Runtime::start()?;
        let native=Native::open(settings,ptr::null_mut())?;
        let codec=match settings.codec{VideoCodec::H264=>"H.264",VideoCodec::Hevc=>"HEVC",VideoCodec::Av1=>"AV1"};
        let mode=if matches!(settings.rate_control,super::RateControl::Vbr{..}){"VBR"}else{"CBR"};
        Ok(Self{native,gpu:None,upload:Vec::new(),settings,headers:Default::default(),name:format!("amf ({codec} {mode}, AMD D3D11, {:?})",settings.tuning.amd_quality),profile:super::profile::Profile::new("amf"),_runtime:runtime})
    }
}
impl super::worker::Driver for Driver {
    fn codec(&self)->VideoCodec {self.settings.codec}
    fn gpu_input(&self)->bool {true}
    fn gpu_vendor(&self)->Option<u16> {Some(0x1002)}
    fn backend_name(&self)->&str {&self.name}
    fn encode(&mut self,frame:&RawFrame,keyframe:bool)->Result<Vec<EncodedFrame>,String> {
        let started=self.profile.start();
        let sample;
        let mut texture=None;
        if frame.gpu {
            let surface=frame.d3d11.as_ref().ok_or("missing GPU capture surface")?;
            let (_,device)=surface.capture_texture()?;
            if unsafe{eclipse_amf_device(self.native.0.as_ptr())}!=device.as_raw() {
                // Rebind before accepting this capture device's first sample.
                self.native=Native::open(self.settings,device.as_raw())?;
                self.gpu=None;
            }
            if self.gpu.is_none(){self.gpu=Some(super::gpu::Input::new(frame,self.settings)?);}
            sample=self.gpu.as_ref().unwrap().sample(frame)?;
            unsafe {
                let buffer:IMFDXGIBuffer=sample.GetBufferByIndex(0).map_err(|e|e.to_string())?.cast().map_err(|e|e.to_string())?;
                let mut raw=ptr::null_mut();
                buffer.GetResource(&ID3D11Texture2D::IID,&mut raw).map_err(|e|e.to_string())?;
                texture=Some(ID3D11Texture2D::from_raw(raw));
            }
        } else {
            let (w,h)=(self.settings.width,self.settings.height);
            self.upload.resize(w as usize*h as usize*3/2,0);
            super::upload::upload_frame(frame,(w,h),[w,w,0],[0,w*h,0],&mut self.upload)?;
        }
        let uploaded=started.map(|at|at.elapsed());
        let mut error=[0;512];let mut data=ptr::null();let mut len=0;let mut pts=0;
        let timestamp=frame.pts.0.checked_mul(10).and_then(|v|i64::try_from(v).ok()).ok_or("AMF timestamp overflow")?;
        let result=unsafe{eclipse_amf_encode(self.native.0.as_ptr(),texture.as_ref().map_or(ptr::null_mut(),Interface::as_raw),
            self.upload.as_ptr(),self.upload.len(),timestamp,i32::from(keyframe),&mut data,&mut len,&mut pts,error.as_mut_ptr(),error.len())};
        if result!=0{return Err(message(&error));}
        if data.is_null() || len==0 || len>64*1024*1024 || pts!=timestamp {return Err("AMF returned an invalid or reordered frame".into());}
        let mut bytes=unsafe{std::slice::from_raw_parts(data,len)}.to_vec();
        let codec=self.settings.codec;
        let idr=match codec {VideoCodec::H264=>EncodedFrame::detect_h264_keyframe(&bytes),VideoCodec::Hevc=>crate::hevc::is_keyframe(&bytes),VideoCodec::Av1=>crate::av1::is_keyframe(&bytes)};
        if keyframe && !idr {return Err("AMF did not honor the requested recovery keyframe".into());}
        if codec==VideoCodec::H264 {bytes=super::low_delay::declare_no_reordering(&bytes)?;}
        let mut frame=EncodedFrame{data:bytes.into(),pts:frame.pts,is_keyframe:idr};
        self.headers.prepare(codec,&mut frame)?;
        self.profile.record(started,uploaded);
        Ok(vec![frame])
    }
    fn bitrate(&mut self,kbps:u32)->Result<super::worker::BitrateChange,String> {
        let mut error=[0;512];
        let result=unsafe{eclipse_amf_bitrate(self.native.0.as_ptr(),kbps,error.as_mut_ptr(),error.len())};
        if result!=0 {return Err(message(&error));}
        self.settings.bitrate_kbps=kbps;Ok(super::worker::BitrateChange::Preserved)
    }
}
