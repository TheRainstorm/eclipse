//! Optional SDP attributes preserve the v1 signalling envelope for older peers.
use anyhow::{ensure,Context,Result};
use serde::{Serialize,Deserialize,de::DeserializeOwned};
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use uu_proto::media::VideoCodec;

const LIMIT:usize=2048;
const REQUEST:&str="x-eclipse-client-video";
const RESPONSE:&str="x-eclipse-host-video";
const RUNTIME:&str="x-eclipse-video-runtime";
const STARTUP:&str="x-eclipse-startup-v1";
fn is_false(value:&bool)->bool{!*value}
pub fn runtime_requested(sdp:&RTCSessionDescription)->Result<bool>{Ok(get::<bool>(sdp,RUNTIME)?.unwrap_or(false))}
pub const MAX_FPS:u32=240;
pub const MAX_BITRATE_KBPS:u32=1_000_000;

/// Relative preparation durations on the host's own monotonic clock. This is
/// optional diagnostic metadata, independent of frame latency and wall clocks.
#[derive(Clone,Debug,Default,PartialEq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Startup {
    pub capture_us:u64,
    pub encoder_us:u64,
    pub other_encoders_us:u64,
    pub ice_us:u64,
    pub total_us:u64,
}
impl Startup {
    pub fn add(&self,sdp:&mut RTCSessionDescription)->Result<()> {self.validate()?;put(sdp,STARTUP,self)}
    pub fn read(sdp:&RTCSessionDescription)->Result<Option<Self>> {
        let value:Option<Self>=get(sdp,STARTUP)?;if let Some(value)=&value{value.validate()?;}Ok(value)
    }
    fn validate(&self)->Result<()> {
        ensure!(self.total_us<=600_000_000&&[self.capture_us,self.encoder_us,self.other_encoders_us,self.ice_us].into_iter().all(|value|value<=self.total_us),"主机准备阶段计时无效");Ok(())
    }
}

#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {pub fps:u32,pub bitrate_kbps:Option<u32>}
impl Update {
    pub fn validate(self)->Result<()> {
        ensure!((5..=MAX_FPS).contains(&self.fps),"帧率请求超出编码接口范围");
        ensure!(self.bitrate_kbps.is_none_or(|kbps|(64..=MAX_BITRATE_KBPS).contains(&kbps)),"码率请求无效");Ok(())
    }
    pub fn bitrate(self,width:u32,height:u32)->u32 {
        self.bitrate_kbps.unwrap_or_else(||(u64::from(width)*u64::from(height)*u64::from(self.fps)/12/1000).clamp(1000,u64::from(MAX_BITRATE_KBPS)) as u32)
    }
}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Applied {pub request:u64,pub settings:Update,pub bitrate_kbps:u32,pub error:Option<String>}
pub(crate) struct Change {pub settings:Update,pub reply:tokio::sync::oneshot::Sender<std::result::Result<u32,String>>}
#[derive(Clone)]
pub(crate) struct Runtime(std::sync::mpsc::SyncSender<Change>,std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<dyn Fn()+Send+Sync>>>>);
impl Runtime {
    pub fn channel()->(Self,std::sync::mpsc::Receiver<Change>){let(tx,rx)=std::sync::mpsc::sync_channel(1);(Self(tx,Default::default()),rx)}
    pub fn set_wake(&self,wake:Option<std::sync::Arc<dyn Fn()+Send+Sync>>) { *self.1.lock().unwrap()=wake; }
    pub fn wake(&self) { let wake=self.1.lock().unwrap().clone();if let Some(wake)=wake {wake();} }
    pub fn request(&self,settings:Update)->Result<tokio::sync::oneshot::Receiver<std::result::Result<u32,String>>> {
        settings.validate()?;
        let(reply,rx)=tokio::sync::oneshot::channel();
        self.0.try_send(Change{settings,reply}).map_err(|_|anyhow::anyhow!("画质更新尚未完成或会话已关闭"))?;self.wake();Ok(rx)
    }
}
#[derive(Clone,Debug,Serialize,Deserialize,PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub fps:u32,
    pub bounds:Option<[u32;2]>,
    pub bitrate_kbps:Option<u32>,
    pub codec:Option<VideoCodec>,
}
impl Request {
    pub fn validate(&self)->Result<()> {
        ensure!((5..=MAX_FPS).contains(&self.fps),"客户端请求帧率无效");
        if let Some([w,h])=self.bounds {ensure!((2..=8192).contains(&w)&&(2..=8192).contains(&h)&&w%2==0&&h%2==0,"客户端请求分辨率无效");}
        if let Some(kbps)=self.bitrate_kbps {ensure!((64..=MAX_BITRATE_KBPS).contains(&kbps),"客户端请求码率无效");}
        Ok(())
    }
    pub fn add(&self,sdp:&mut RTCSessionDescription)->Result<()> {self.validate()?;put(sdp,REQUEST,self)?;put(sdp,RUNTIME,&true)}
    pub fn read(sdp:&RTCSessionDescription)->Result<Option<Self>> {let value:Option<Self>=get(sdp,REQUEST)?;if let Some(v)=&value{v.validate()?;}Ok(value)}
}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    #[serde(default,skip_serializing_if="is_false")]
    pub dynamic:bool,
    /// Successfully initialized encoders at the negotiated geometry.
    pub codecs:Vec<VideoCodec>,
    pub max_fps:u32,
    pub max_bitrate_kbps:u32,
}
impl Response {
    pub fn add(&self,sdp:&mut RTCSessionDescription)->Result<()> {self.validate()?;put(sdp,RESPONSE,self)}
    pub fn read(sdp:&RTCSessionDescription)->Result<Option<Self>> {let value:Option<Self>=get(sdp,RESPONSE)?;if let Some(v)=&value{v.validate()?;}Ok(value)}
    fn validate(&self)->Result<()> {
        ensure!(!self.codecs.is_empty()&&self.codecs.len()<=3&&(1..=MAX_FPS).contains(&self.max_fps)&&(64..=MAX_BITRATE_KBPS).contains(&self.max_bitrate_kbps),"原生主机画质能力无效");
        for (i,codec) in self.codecs.iter().enumerate(){ensure!(!self.codecs[..i].contains(codec),"重复的主机编码能力");} Ok(())
    }
}
#[derive(Clone,Debug)]
pub struct Policy {
    pub codecs:Vec<VideoCodec>,
    /// None lets an explicit client request choose; both automatic use AV1 > HEVC > H.264.
    pub preferred_codec:Option<VideoCodec>,
}
impl Policy {
    pub fn validate(&self)->Result<()> {
        ensure!(self.preferred_codec.is_none_or(|c|self.codecs.contains(&c)),"默认编码须包含在允许的编码中");
        Response{dynamic:true,codecs:self.codecs.clone(),max_fps:MAX_FPS,max_bitrate_kbps:MAX_BITRATE_KBPS}.validate()
    }
}
pub(crate) fn put(sdp:&mut RTCSessionDescription,key:&str,value:&impl Serialize)->Result<()> {
    ensure!(get::<serde_json::Value>(sdp,key)?.is_none(),"重复的画质属性");
    let value=serde_json::to_vec(value)?;ensure!(value.len()<=LIMIT/2,"画质属性过长");
    let line=format!("a={key}:{}\r\n",hex::encode(value));
    // Session-level attributes must precede the first m= section.
    let at=sdp.sdp.find("m=").unwrap_or(sdp.sdp.len());sdp.sdp.insert_str(at,&line);
    Ok(())
}
pub(crate) fn get<T:DeserializeOwned>(sdp:&RTCSessionDescription,key:&str)->Result<Option<T>> {
    let prefix=format!("a={key}:");let mut found=None;
    for line in sdp.sdp.lines() {
        if let Some(value)=line.strip_prefix(&prefix) {
            ensure!(found.is_none()&&value.len()<=LIMIT,"画质属性重复或过长");
            let data=hex::decode(value).context("画质属性编码无效")?;
            found=Some(serde_json::from_slice(&data).map_err(|_|anyhow::anyhow!("画质属性格式无效"))?);
        }
    }
    Ok(found)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attributes_round_trip_through_real_sdp_parser_and_leave_v1_envelope_unchanged() {
        let mut sdp=RTCSessionDescription::offer("v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\na=rtpmap:96 H264/90000\r\na=recvonly\r\n".into()).unwrap();
        let request=Request{fps:120,bounds:Some([1920,1080]),bitrate_kbps:Some(8000),codec:Some(VideoCodec::H264)};
        assert!(Request::read(&sdp).unwrap().is_none());
        assert!(Startup::read(&sdp).unwrap().is_none());
        request.add(&mut sdp).unwrap();assert_eq!(Request::read(&sdp).unwrap(),Some(request.clone()));
        let timing=Startup{capture_us:100,encoder_us:200,other_encoders_us:300,ice_us:400,total_us:2000};
        timing.add(&mut sdp).unwrap();assert_eq!(Startup::read(&sdp).unwrap(),Some(timing));
        assert!(Startup{encoder_us:3,total_us:2,..Default::default()}.add(&mut sdp).is_err());
        let reparsed=sdp.unmarshal().unwrap();assert!(reparsed.attributes.iter().any(|a|a.key==REQUEST));
        assert!(crate::native_media::offers_codec(&sdp,VideoCodec::H264));
        assert!(request.add(&mut sdp).is_err());
        sdp.sdp.push_str(&format!("a={REQUEST}:ff\r\n"));assert!(Request::read(&sdp).is_err());
    }
}

#[cfg(test)]
mod owner_wake_tests {
    use super::*;
    #[test]
    fn quality_request_wakes_waiter_and_cancellation_keeps_receiver_closed() {
        use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
        let(runtime,changes)=Runtime::channel();let calls=Arc::new(AtomicUsize::new(0));let seen=calls.clone();
        runtime.set_wake(Some(Arc::new(move ||{seen.fetch_add(1,Ordering::SeqCst);})));
        let reply=runtime.request(Update{fps:120,bitrate_kbps:Some(40000)}).unwrap();assert_eq!(calls.load(Ordering::SeqCst),1);
        drop(reply);let change=changes.recv().unwrap();assert!(change.reply.is_closed());
        runtime.wake();assert_eq!(calls.load(Ordering::SeqCst),2);
        runtime.set_wake(None);runtime.wake();assert_eq!(calls.load(Ordering::SeqCst),2);
    }
}
