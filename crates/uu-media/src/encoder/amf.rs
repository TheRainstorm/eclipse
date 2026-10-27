//! Native AMD encoder, using only the installed driver and vendored MIT API headers.
use super::Settings;
use crate::{EncodedFrame,RawFrame};
use std::{ffi::{c_char,c_void,CStr},ptr::{self,NonNull}};
use uu_proto::media::VideoCodec;
use windows::{core::Interface,Win32::{Graphics::Direct3D11::ID3D11Texture2D,Media::MediaFoundation::IMFDXGIBuffer}};
unsafe extern "C" {
    fn eclipse_amf_open(codec:i32,width:u32,height:u32,fps:u32,kbps:u32,peak:u32,vbr:i32,quality:i32,hdr:i32,default_adapter:i32,device:*mut c_void,error:*mut c_char,capacity:usize)->*mut c_void;
    fn eclipse_amf_close(session:*mut c_void)->u64;
    fn eclipse_amf_properties(session:*mut c_void,output:*mut c_char,capacity:usize)->i32;
    fn eclipse_amf_device(session:*mut c_void)->*mut c_void;
    fn eclipse_amf_bitrate(session:*mut c_void,kbps:u32,error:*mut c_char,capacity:usize)->i32;
    fn eclipse_amf_lease_new()->*mut c_void;
    fn eclipse_amf_lease_released(lease:*mut c_void)->i32;
    fn eclipse_amf_lease_close(lease:*mut c_void);
    fn eclipse_amf_encode(session:*mut c_void,texture:*mut c_void,cpu:*const u8,cpu_length:usize,pts:i64,keyframe:i32,
        lease:*mut c_void,data:*mut *const u8,len:*mut usize,timestamp:*mut i64,error:*mut c_char,capacity:usize)->i32;
}
fn message(error:&[c_char])->String {unsafe{CStr::from_ptr(error.as_ptr())}.to_string_lossy().into_owned()}
struct Native(Option<NonNull<c_void>>);
impl Native {
    fn ptr(&self)->*mut c_void {self.0.expect("closed AMF session").as_ptr()}
    fn close(&mut self)->u64 {self.0.take().map_or(0,|p|unsafe{eclipse_amf_close(p.as_ptr())})}
    fn open(settings:Settings,device:*mut c_void)->Result<Self,String> {
        let mut error=[0;512];
        let codec=match settings.codec {VideoCodec::H264=>0,VideoCodec::Hevc=>1,VideoCodec::Av1=>2,VideoCodec::Pyrowave=>return Err("PyroWave uses the Vulkan backend".into())};
        let session=unsafe{eclipse_amf_open(codec,settings.width,settings.height,settings.fps,settings.bitrate_kbps,
            settings.rate_control.peak(settings.bitrate_kbps),i32::from(matches!(settings.rate_control,super::RateControl::Vbr{..})),match settings.tuning.amd_quality{super::AmdQuality::Speed=>0,super::AmdQuality::Balanced=>1,super::AmdQuality::Quality=>2},i32::from(settings.hdr),i32::from(settings.preference!=super::Preference::Amd),device,error.as_mut_ptr(),error.len())};
        NonNull::new(session).map(|p|Self(Some(p))).ok_or_else(||message(&error))
    }
}
impl Drop for Native {fn drop(&mut self){let pending=self.close();if pending!=0 {tracing::error!(pending,"AMF input leases remain after termination; retained targets cannot be recycled");}}}
struct Lease(NonNull<c_void>);
impl Lease {fn new()->Result<Self,String>{NonNull::new(unsafe{eclipse_amf_lease_new()}).map(Self).ok_or_else(||"AMF lease allocation failed".into())}}
impl super::input_pool::Released for Lease {fn released(&self)->bool{unsafe{eclipse_amf_lease_released(self.0.as_ptr())!=0}}}
impl Drop for Lease {fn drop(&mut self){unsafe{eclipse_amf_lease_close(self.0.as_ptr());}}}
struct Inputs(super::input_pool::Pool<super::gpu::AmfTarget,Lease>,&'static str);
impl Drop for Inputs {
    fn drop(&mut self) {
        self.0.retire();
        if std::env::var_os("UU_ENCODE_PROFILE").is_some(){eprintln!("uu-amf-input-pool generation={} slots={} allocations={} reuses={} submissions={} pending={} path={}",
            self.0.generation(),self.0.slots(),self.0.allocations,self.0.reuses,self.0.submissions,self.0.pending(),self.1);}
    }
}
pub(super) struct Driver {
    native:Native,
    shared_proof:Option<serde_json::Value>,
    observation:Option<super::amf_workload::Observation>,
    closed_observations:Vec<super::amf_workload::Observation>,
    observation_generation_overflow:u64,
    observation_requested:bool,
    gpu:Option<super::gpu::Input>,
    inputs:Inputs,
    pool_enabled:bool,
    upload:Vec<u8>,
    settings:Settings,
    headers:super::headers::Headers,
    name:String,
    profile:super::profile::Profile,
    _runtime:super::mf::Runtime,
}
impl Driver {
    pub fn open_with_device(settings:Settings,input:Option<super::InputDevice>)->Result<Self,String> {
        let runtime=super::mf::Runtime::start()?;
        let native=Native::open(settings,input.as_ref().map_or(ptr::null_mut(),|device|device.device.as_raw()))?;
        let codec=match settings.codec{VideoCodec::H264=>"H.264",VideoCodec::Hevc=>"HEVC",VideoCodec::Av1=>"AV1",VideoCodec::Pyrowave=>"PyroWave"};
        let mode=if matches!(settings.rate_control,super::RateControl::Vbr{..}){"VBR"}else{"CBR"};
        // Reuse only after matching output and surface release; an explicit
        // override keeps the existing 1/other-value diagnostic behavior.
        let pool_enabled=std::env::var_os("UU_AMF_INPUT_POOL").is_none_or(|v|v=="1");
        if std::env::var_os("UU_ENCODE_PROFILE").is_some(){eprintln!("uu-amf-input-mode pool_enabled={pool_enabled}");}
        let observation=if settings.codec==VideoCodec::Av1{super::amf_workload::Observation::enabled(1,64*1024*1024)}else{None};
        let observation_requested=observation.is_some();
        let mut driver=Self{native,observation_requested,observation,closed_observations:Vec::new(),observation_generation_overflow:0,shared_proof:None,gpu:None,inputs:Inputs(super::input_pool::Pool::new(1),"uninitialized"),pool_enabled,upload:Vec::new(),settings,headers:Default::default(),name:format!("amf ({codec} {mode}, AMD D3D11, {:?})",settings.tuning.amd_quality),profile:super::profile::Profile::new("amf"),_runtime:runtime};
        driver.observe_properties("initialized",None);
        Ok(driver)
    }
}
impl Driver {
 fn observe_properties(&mut self,phase:&str,pts:Option<u64>){
  if self.observation.is_none()||self.native.0.is_none(){return}
  let start=std::time::Instant::now();let mut buffer=[0 as c_char;16384];
  let result=unsafe{eclipse_amf_properties(self.native.ptr(),buffer.as_mut_ptr(),buffer.len())};
  let properties=if result==0{serde_json::from_str(&message(&buffer)).unwrap_or_else(|e|serde_json::json!({"decode_error":e.to_string()}))}else{serde_json::json!({"read_error":result})};
  self.observation.as_mut().unwrap().snapshot(phase,pts,properties,start.elapsed().as_nanos());
 }
 fn retire_observation(&mut self){self.observe_properties("before_close",None);if let Some(d)=self.observation.take(){self.closed_observations.push(d);}}
 fn close_observation(&mut self){self.retire_observation();for d in self.closed_observations.drain(..){if let Err(e)=d.finish(){tracing::error!(error=%e,"AMF workload diagnostic write failed");}}if self.observation_generation_overflow>0{tracing::error!(generations=self.observation_generation_overflow,"AMF workload diagnostic generation cap exceeded");}}
}
impl super::worker::Driver for Driver {
    fn codec(&self)->VideoCodec {self.settings.codec}
    fn gpu_input(&self)->bool {true}
    fn gpu_vendor(&self)->Option<u16> {Some(0x1002)}
    fn backend_name(&self)->&str {&self.name}
    fn encode(&mut self,frame:&RawFrame,keyframe:bool)->Result<Vec<EncodedFrame>,String> {
        if self.native.0.is_none(){return Err("AMF input generation is closed".into());}
        let started=self.profile.start();
        if let Some(d)=self.observation.as_mut(){d.input();}
        let sample;
        let mut texture=None;
        let timestamp=frame.pts.0.checked_mul(10).and_then(|v|i64::try_from(v).ok()).ok_or("AMF timestamp overflow")?;
        let mut lease_key=None;
        if frame.gpu {
            let surface=frame.d3d11.as_ref().ok_or("missing GPU capture surface")?;
            let (_,device)=surface.capture_texture()?;
            if unsafe{eclipse_amf_device(self.native.ptr())}!=device.as_raw() {
                if surface.shared_capture_info().is_some(){return Err("separate capture: native AMF device mismatch; refusing capture-device rebind".into());}
                // Rebind before accepting this capture device's first sample.
                let next=Native::open(self.settings,device.as_raw())?;
                self.retire_observation();
                if self.native.close()!=0 {
                    self.inputs.0.retire();
                    return Err("AMF retained inputs after termination; refusing device rebuild".into());
                }
                self.native=next;
                let budget=(64*1024*1024usize).saturating_sub(self.closed_observations.iter().map(|d|d.retained_bytes()).sum());
                self.observation=if self.observation_requested&&self.closed_observations.len()<4{super::amf_workload::Observation::enabled(self.inputs.0.generation()+1,budget)}else{if self.observation_requested{self.observation_generation_overflow+=1;}None};
                self.observe_properties("initialized",None);
                if let Some(d)=self.observation.as_mut(){d.input();}
                self.gpu=None;
                self.inputs=Inputs(super::input_pool::Pool::new(self.inputs.0.generation()+1),"uninitialized");
            }
            if self.gpu.is_none(){self.gpu=Some(super::gpu::Input::new(frame,self.settings)?);}
            let input=self.gpu.as_ref().unwrap();
            if self.shared_proof.is_none(){
                let actual=super::input_device::device_identity(device)?;
                let mut proof=surface.shared_capture_info().unwrap_or_else(||serde_json::json!({"capture":actual,"encode":actual,"actual_separate":false}));
                let conversion=input.device_identity()?;
                let native_raw=unsafe{eclipse_amf_device(self.native.ptr())};
                let native_device=unsafe{windows::Win32::Graphics::Direct3D11::ID3D11Device::from_raw_borrowed(&native_raw)}.ok_or("native AMF device absent")?;
                let native=super::input_device::device_identity(native_device)?;
                if serde_json::to_value(conversion).map_err(|e|e.to_string())?!=proof["encode"] || native!=conversion {
                    return Err("capture: conversion/native device identity or LUID mismatch".into());
                }
                proof["conversion"]=serde_json::to_value(conversion).map_err(|e|e.to_string())?;
                proof["native_amf"]=serde_json::to_value(native).map_err(|e|e.to_string())?;
                let mut first=proof.clone();first["event"]="shared_capture_first_input".into();crate::shared_capture::log(first);
                self.shared_proof=Some(proof);
            }
            self.inputs.1=input.amf_path();
            if self.pool_enabled {
                let lease=match Lease::new(){Ok(lease)=>lease,Err(error)=>{self.inputs.0.retire();return Err(error);}};
                let key=match self.inputs.0.acquire(timestamp,lease,||input.amf_target()) {
                    Ok(key)=>key,Err(error)=>{self.inputs.0.retire();return Err(error);}
                };
                let (target,_)=self.inputs.0.target(key)?;
                match input.amf_convert(frame,target) {
                    Ok(output)=>texture=Some(output),
                    Err(error)=>{self.inputs.0.retire();return Err(error);}
                }
                lease_key=Some(key);
            }else {
                // Pool0 uses the same fresh sample path as ordinary MF. Pooled AMF
                // targets need no intermediate MFSample/DXGI buffer wrapper.
                sample=input.sample(frame)?;
                unsafe {
                let buffer:IMFDXGIBuffer=sample.GetBufferByIndex(0).map_err(|e|e.to_string())?.cast().map_err(|e|e.to_string())?;
                let mut raw=ptr::null_mut();
                buffer.GetResource(&ID3D11Texture2D::IID,&mut raw).map_err(|e|e.to_string())?;
                texture=Some(ID3D11Texture2D::from_raw(raw));
                }
            }
        } else {
            if self.settings.hdr {return Err("HDR encoding requires a floating-point GPU capture surface".into());}
            let (w,h)=(self.settings.width,self.settings.height);
            self.upload.resize(w as usize*h as usize*3/2,0);
            super::upload::upload_frame(frame,(w,h),[w,w,0],[0,w*h,0],&mut self.upload)?;
        }
        let uploaded=started.map(|at|at.elapsed());
        let mut error=[0;512];let mut data=ptr::null();let mut len=0;let mut pts=0;
        let lease=lease_key.map(|key|self.inputs.0.target(key).map(|(_,lease)|lease.0.as_ptr())).transpose()?.unwrap_or(ptr::null_mut());
        let result=unsafe{eclipse_amf_encode(self.native.ptr(),texture.as_ref().map_or(ptr::null_mut(),Interface::as_raw),
            self.upload.as_ptr(),self.upload.len(),timestamp,i32::from(keyframe),lease,&mut data,&mut len,&mut pts,error.as_mut_ptr(),error.len())};
        if result!=0{self.inputs.0.retire();return Err(message(&error));}
        if data.is_null() || len==0 || len>64*1024*1024 || pts!=timestamp {self.inputs.0.retire();return Err("AMF returned an invalid or reordered frame".into());}
        if let Some(key)=lease_key {if let Err(error)=self.inputs.0.complete(key,pts){self.inputs.0.retire();return Err(error);}}
        let mut bytes=unsafe{std::slice::from_raw_parts(data,len)}.to_vec();
        let codec=self.settings.codec;
        let idr=match codec {VideoCodec::H264=>EncodedFrame::detect_h264_keyframe(&bytes),VideoCodec::Hevc=>crate::hevc::is_keyframe(&bytes),VideoCodec::Av1=>crate::av1::is_keyframe(&bytes),VideoCodec::Pyrowave=>false};
        if keyframe && !idr {return Err("AMF did not honor the requested recovery keyframe".into());}
        if codec==VideoCodec::H264 {bytes=super::low_delay::declare_no_reordering(&bytes)?;}
        let mut frame=EncodedFrame{data:bytes.into(),pts:frame.pts,is_keyframe:idr};
        self.headers.prepare(codec,&mut frame)?;
        super::validate_hdr_output(&frame,codec,self.settings.hdr)?;
        if let Some(d)=self.observation.as_mut(){d.output(unsafe{std::slice::from_raw_parts(data,len)},&frame.data,frame.pts.0,frame.is_keyframe);}
        if self.observation.as_ref().is_some_and(|d|d.wants_snapshot()){self.observe_properties("output",Some(frame.pts.0));}
        self.profile.record(started,uploaded);
        Ok(vec![frame])
    }
    fn bitrate(&mut self,kbps:u32)->Result<super::worker::BitrateChange,String> {
        if self.native.0.is_none(){return Err("AMF input generation is closed".into());}
        let mut error=[0;512];
        let result=unsafe{eclipse_amf_bitrate(self.native.ptr(),kbps,error.as_mut_ptr(),error.len())};
        if let Some(d)=self.observation.as_mut(){d.update(kbps,result);}
        if result!=0 {return Err(message(&error));}
        self.settings.bitrate_kbps=kbps;Ok(super::worker::BitrateChange::Preserved)
    }
}

impl Drop for Driver {fn drop(&mut self){
    self.close_observation();
    if let Some(mut proof)=self.shared_proof.take(){
        let pending=self.native.close();
        proof["event"]="shared_capture_amf_closed".into();proof["native_pending"]=pending.into();
        crate::shared_capture::log(proof);
    }
}}
