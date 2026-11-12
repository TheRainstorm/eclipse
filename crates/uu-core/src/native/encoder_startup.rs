//! Admit one new encoder at a time and verify its actual input before Answer.
//! Existing encoders keep running; neither admission nor fallback touches them.
use anyhow::{ensure,Result};
use std::{sync::{atomic::{AtomicBool,Ordering},Mutex,MutexGuard,TryLockError},time::{Duration,Instant}};
use uu_media::{VideoEncoder,encoder::{Settings,Preference,RateControl}};
use uu_proto::media::VideoCodec;

static ADMISSION:Mutex<()>=Mutex::new(());
fn admission(cancel:&AtomicBool)->Result<MutexGuard<'static,()>> {
    loop {
        ensure!(!cancel.load(Ordering::Acquire),"capture preparation cancelled");
        match ADMISSION.try_lock() {
            Ok(guard)=>return Ok(guard),
            Err(TryLockError::Poisoned(error))=>return Ok(error.into_inner()),
            Err(TryLockError::WouldBlock)=>std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn verify(capture:&mut dyn uu_capture::Capture,encoder:&mut dyn VideoEncoder,cancel:&AtomicBool)->Result<()> {
    verify_with_budget(capture,encoder,cancel,Duration::from_secs(5))
}

fn verify_with_budget(capture:&mut dyn uu_capture::Capture,encoder:&mut dyn VideoEncoder,cancel:&AtomicBool,budget:Duration)->Result<()> {
    capture.prefer_gpu(encoder.gpu_input(),encoder.gpu_vendor());
    encoder.request_keyframe();
    let deadline=Instant::now()+budget;
    let mut inputs=0;
    let mut suspended=None;
    loop {
        ensure!(!cancel.load(Ordering::Acquire),"capture preparation cancelled");
        if Instant::now()>=deadline || inputs>=8 {
            if inputs==0 {
                return Err(uu_capture::CaptureError::Suspended(uu_i18n::format!("桌面未提供首帧：{}",suspended.as_deref().unwrap_or(uu_i18n::text("等待当前桌面首帧")))).into());
            }
            anyhow::bail!("encoder produced no startup picture after {inputs} input frames within its preparation budget");
        }
        let frame=match capture.next_frame() {
            Ok(frame)=>frame,
            Err(uu_capture::CaptureError::Suspended(reason))=>{suspended=Some(reason);std::thread::sleep(Duration::from_millis(25));continue;},
            Err(error)=>return Err(error.into()),
        };
        inputs+=1;
        let frames=encoder.encode(&frame)?;
        if frames.iter().any(|frame|frame.is_keyframe&&!frame.data.is_empty()) {
            // This picture only proves the reservation/input path. The first
            // transmitted picture must start its own decodable reference chain.
            encoder.request_keyframe();return Ok(());
        }
    }
}

pub(super) fn select(
    capture:&mut dyn uu_capture::Capture,
    attempts:&[(VideoCodec,bool)],
    settings:impl Fn(VideoCodec,bool)->Settings,
    software_fallback:bool,
    cancel:&AtomicBool,
)->Result<(Box<dyn VideoEncoder>,Settings)> {
    select_with(capture,attempts,settings,software_fallback,cancel,uu_media::encoder::create_with_device)
}
fn select_with(
    capture:&mut dyn uu_capture::Capture,attempts:&[(VideoCodec,bool)],
    settings:impl Fn(VideoCodec,bool)->Settings,software_fallback:bool,cancel:&AtomicBool,
    mut create:impl FnMut(Settings,Option<uu_media::encoder::InputDevice>)->Result<Box<dyn VideoEncoder>,uu_media::CodecError>,
)->Result<(Box<dyn VideoEncoder>,Settings)> {
    let _admission=admission(cancel)?;
    let mut last_error=None;
    let mut attempt=|settings:Settings|->Result<Box<dyn VideoEncoder>> {
        ensure!(!cancel.load(Ordering::Acquire),"capture preparation cancelled");
        let mut encoder=create(settings,capture.encoder_input_device())?;
        ensure!(!settings.hdr||encoder.gpu_input(),"HDR source requires same-device GPU color conversion");
        verify(capture,encoder.as_mut(),cancel)?;
        Ok(encoder)
    };
    for &(codec,hdr) in attempts {
        let settings=settings(codec,hdr);
        match attempt(settings) {
            Ok(encoder)=>return Ok((encoder,settings)),
            Err(error)=>{
                if error.is::<uu_capture::CaptureError>() { return Err(error); }
                tracing::warn!(?codec,hdr,preference=?settings.preference,%error,"native encoder preparation failed");
                last_error=Some(error);
            }
        }
    }
    if software_fallback&&attempts.iter().any(|&(codec,hdr)|codec==VideoCodec::H264&&!hdr) {
        let settings=settings(VideoCodec::H264,false);
        if settings.preference==Preference::Auto&&settings.tuning==Default::default() {
            let settings=Settings{preference:Preference::Software,rate_control:RateControl::Cbr,..settings};
            match attempt(settings) {
                Ok(encoder)=>return Ok((encoder,settings)),
                Err(error)=>{if error.is::<uu_capture::CaptureError>() {return Err(error);}last_error=Some(error);},
            }
        }
    }
    Err(last_error.unwrap_or_else(||anyhow::anyhow!("no encoder candidate")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uu_media::{RawFrame,EncodedFrame,CodecError};
    struct Slot {codec:VideoCodec,live:std::sync::Arc<std::sync::atomic::AtomicUsize>}
    impl Drop for Slot {fn drop(&mut self){if self.codec==VideoCodec::Hevc{self.live.fetch_sub(1,Ordering::AcqRel);}}}
    impl VideoEncoder for Slot {
        fn codec(&self)->VideoCodec{self.codec}
        fn encode(&mut self,frame:&RawFrame)->Result<Vec<EncodedFrame>,CodecError>{Ok(vec![EncodedFrame{data:vec![1].into(),pts:frame.pts,is_keyframe:true}])}
        fn request_keyframe(&mut self){}
        fn set_bitrate(&mut self,_:u32)->Result<(),CodecError>{Ok(())}
    }
    #[test]
    fn software_floor_uses_cbr_only_in_auto_and_never_overrides_a_hardware_requirement() {
        let live=Default::default();let calls=std::cell::RefCell::new(Vec::new());
        let create=|settings:Settings,_|->Result<Box<dyn VideoEncoder>,CodecError> {
            calls.borrow_mut().push((settings.preference,settings.rate_control));
            if settings.preference!=Preference::Software {return Err(CodecError::Backend("generated busy hardware".into()));}
            Ok(Box::new(Slot{codec:settings.codec,live:std::sync::Arc::clone(&live)}))
        };
        let settings=|preference|Settings{tuning:Default::default(),hdr:false,source_hdr:None,codec:VideoCodec::H264,rate_control:RateControl::Vbr{peak_kbps:2000},width:64,height:64,fps:30,bitrate_kbps:1000,preference};
        let mut capture=uu_capture::open(&uu_capture::CaptureSource::Synthetic{width:64,height:64,fps:30}).unwrap();
        let cancel=AtomicBool::new(false);let attempts=[(VideoCodec::H264,false)];
        let (_,applied)=select_with(capture.as_mut(),&attempts,|_,_|settings(Preference::Auto),true,&cancel,create).unwrap();
        assert_eq!(applied.preference,Preference::Software);assert_eq!(applied.rate_control,RateControl::Cbr);
        calls.borrow_mut().clear();
        assert!(select_with(capture.as_mut(),&attempts,|_,_|settings(Preference::Hardware),true,&cancel,create).is_err());
        assert_eq!(calls.borrow().as_slice(),&[(Preference::Hardware,RateControl::Vbr{peak_kbps:2000})]);
    }
    #[test]
    fn fallback_respects_live_reservations_and_never_retires_existing_encoders() {
        let live=std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let create=|settings:Settings,_|->Result<Box<dyn VideoEncoder>,CodecError> {
            if settings.codec==VideoCodec::Hevc {
                if live.load(Ordering::Acquire)>=2 {return Err(CodecError::Backend("generated capacity exhausted".into()));}
                live.fetch_add(1,Ordering::AcqRel);
            }
            Ok(Box::new(Slot{codec:settings.codec,live:live.clone()}))
        };
        let settings=|codec,hdr|Settings{tuning:Default::default(),hdr,source_hdr:None,codec,rate_control:RateControl::Cbr,width:64,height:64,fps:30,bitrate_kbps:1000,preference:Preference::Hardware};
        let mut capture=uu_capture::open(&uu_capture::CaptureSource::Synthetic{width:64,height:64,fps:30}).unwrap();
        let cancel=AtomicBool::new(false);
        let mut existing=Vec::new();
        for _ in 0..2 {existing.push(select_with(capture.as_mut(),&[(VideoCodec::Hevc,false)],settings,false,&cancel,create).unwrap().0);}
        let (fallback,applied)=select_with(capture.as_mut(),&[(VideoCodec::Hevc,false),(VideoCodec::H264,false)],settings,false,&cancel,create).unwrap();
        assert_eq!(applied.codec,VideoCodec::H264);assert_eq!(live.load(Ordering::Acquire),2);
        assert!(select_with(capture.as_mut(),&[(VideoCodec::Hevc,false)],settings,false,&cancel,create).is_err());
        for encoder in &mut existing {assert!(!encoder.encode(&capture.next_frame().unwrap()).unwrap().is_empty());}
        drop(fallback);assert_eq!(live.load(Ordering::Acquire),2);
        drop(existing);assert_eq!(live.load(Ordering::Acquire),0);
    }
    struct RejectFirst {drop_count:std::sync::Arc<std::sync::atomic::AtomicUsize>}
    impl Drop for RejectFirst {fn drop(&mut self){self.drop_count.fetch_add(1,Ordering::Relaxed);}}
    impl VideoEncoder for RejectFirst {
        fn codec(&self)->VideoCodec{VideoCodec::H264}
        fn encode(&mut self,_:&RawFrame)->Result<Vec<EncodedFrame>,CodecError>{Err(CodecError::Backend("generated capacity exhaustion".into()))}
        fn request_keyframe(&mut self){}
        fn set_bitrate(&mut self,_:u32)->Result<(),CodecError>{Ok(())}
    }
    #[test]
    fn opening_is_not_capacity_proof_and_cancellation_prevents_input() {
        let mut capture=uu_capture::open(&uu_capture::CaptureSource::Synthetic{width:64,height:64,fps:30}).unwrap();
        let drops=Default::default();let mut encoder=RejectFirst{drop_count:drops};
        let error=verify(capture.as_mut(),&mut encoder,&AtomicBool::new(false)).unwrap_err();
        assert!(error.to_string().contains("capacity exhaustion"));
        let error=verify(capture.as_mut(),&mut encoder,&AtomicBool::new(true)).unwrap_err();
        assert!(error.to_string().contains("cancelled"));
    }
    #[test]
    fn real_software_output_is_verified_and_keeps_a_fresh_keyframe() {
        let settings=Settings{tuning:Default::default(),hdr:false,source_hdr:None,codec:VideoCodec::H264,rate_control:RateControl::Cbr,width:64,height:64,fps:30,bitrate_kbps:1000,preference:Preference::Software};
        let mut capture=uu_capture::open(&uu_capture::CaptureSource::Synthetic{width:64,height:64,fps:30}).unwrap();
        let (mut encoder,_)=select(capture.as_mut(),&[(VideoCodec::H264,false)],|_,_|settings,false,&AtomicBool::new(false)).unwrap();
        let frame=capture.next_frame().unwrap();let encoded=encoder.encode(&frame).unwrap();
        assert!(encoded.iter().any(|frame|frame.is_keyframe&&!frame.data.is_empty()));
    }
    #[test]
    fn missing_capture_is_reported_as_capture_and_not_retried_with_other_codecs() {
        struct Missing;
        impl uu_capture::Capture for Missing {
            fn resolution(&self)->(u32,u32){(64,64)}
            fn next_frame(&mut self)->Result<RawFrame,uu_capture::CaptureError>{Err(uu_capture::CaptureError::Backend("desktop unavailable".into()))}
        }
        let settings=|codec,hdr|Settings{tuning:Default::default(),hdr,source_hdr:None,codec,rate_control:RateControl::Cbr,width:64,height:64,fps:30,bitrate_kbps:1000,preference:Preference::Auto};
        let calls=std::cell::Cell::new(0);
        let result=select_with(&mut Missing,&[(VideoCodec::Av1,false),(VideoCodec::Hevc,false),(VideoCodec::H264,false)],settings,true,&AtomicBool::new(false),|settings,_|{
            calls.set(calls.get()+1);
            Ok(Box::new(Slot{codec:settings.codec,live:Default::default()}))
        });
        let error=result.err().expect("capture must fail");
        assert!(error.is::<uu_capture::CaptureError>());
        assert!(error.to_string().contains("desktop unavailable"));
        assert_eq!(calls.get(),1,"changing codecs cannot repair a missing desktop");
        struct Paused;
        impl uu_capture::Capture for Paused {
            fn resolution(&self)->(u32,u32){(64,64)}
            fn next_frame(&mut self)->Result<RawFrame,uu_capture::CaptureError>{Err(uu_capture::CaptureError::Suspended("input desktop unavailable".into()))}
        }
        let mut encoder=Slot{codec:VideoCodec::H264,live:Default::default()};
        let error=verify_with_budget(&mut Paused,&mut encoder,&AtomicBool::new(false),Duration::from_millis(1)).unwrap_err();
        assert!(error.is::<uu_capture::CaptureError>());
        assert!(error.to_string().contains("input desktop unavailable"));
    }


}
