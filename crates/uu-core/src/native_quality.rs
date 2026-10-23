//! Optional SDP attributes preserve the v1 signalling envelope for older peers.
use anyhow::{ensure,Context,Result};
use serde::{Serialize,Deserialize,de::DeserializeOwned};
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use uu_proto::media::VideoCodec;

const LIMIT:usize=2048;
const REQUEST:&str="x-eclipse-client-video";
const RESPONSE:&str="x-eclipse-host-video";
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
        ensure!((5..=144).contains(&self.fps),"客户端请求帧率无效");
        if let Some([w,h])=self.bounds {ensure!((2..=8192).contains(&w)&&(2..=8192).contains(&h)&&w%2==0&&h%2==0,"客户端请求分辨率无效");}
        if let Some(kbps)=self.bitrate_kbps {ensure!((64..=100_000).contains(&kbps),"客户端请求码率无效");}
        Ok(())
    }
    pub fn add(&self,sdp:&mut RTCSessionDescription)->Result<()> {self.validate()?;put(sdp,REQUEST,self)}
    pub fn read(sdp:&RTCSessionDescription)->Result<Option<Self>> {let value:Option<Self>=get(sdp,REQUEST)?;if let Some(v)=&value{v.validate()?;}Ok(value)}
}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    /// Successfully initialized encoders at the negotiated geometry.
    pub codecs:Vec<VideoCodec>,
    pub max_fps:u32,
    pub max_bitrate_kbps:u32,
}
impl Response {
    pub fn add(&self,sdp:&mut RTCSessionDescription)->Result<()> {self.validate()?;put(sdp,RESPONSE,self)}
    pub fn read(sdp:&RTCSessionDescription)->Result<Option<Self>> {let value:Option<Self>=get(sdp,RESPONSE)?;if let Some(v)=&value{v.validate()?;}Ok(value)}
    fn validate(&self)->Result<()> {
        ensure!(!self.codecs.is_empty()&&self.codecs.len()<=3&&(1..=60).contains(&self.max_fps)&&(64..=100_000).contains(&self.max_bitrate_kbps),"原生主机画质能力无效");
        for (i,codec) in self.codecs.iter().enumerate(){ensure!(!self.codecs[..i].contains(codec),"重复的主机编码能力");} Ok(())
    }
}
#[derive(Clone,Debug)]
pub struct Policy {pub codecs:Vec<VideoCodec>,pub max_bitrate_kbps:u32}
impl Policy {
    pub fn validate(&self)->Result<()> {
        Response{codecs:self.codecs.clone(),max_fps:60,max_bitrate_kbps:self.max_bitrate_kbps}.validate()
    }
}
fn put(sdp:&mut RTCSessionDescription,key:&str,value:&impl Serialize)->Result<()> {
    ensure!(get::<serde_json::Value>(sdp,key)?.is_none(),"重复的画质属性");
    let value=serde_json::to_vec(value)?;ensure!(value.len()<=LIMIT/2,"画质属性过长");
    let line=format!("a={key}:{}\r\n",hex::encode(value));
    // Session-level attributes must precede the first m= section.
    let at=sdp.sdp.find("m=").unwrap_or(sdp.sdp.len());sdp.sdp.insert_str(at,&line);
    Ok(())
}
fn get<T:DeserializeOwned>(sdp:&RTCSessionDescription,key:&str)->Result<Option<T>> {
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
        request.add(&mut sdp).unwrap();assert_eq!(Request::read(&sdp).unwrap(),Some(request.clone()));
        let reparsed=sdp.unmarshal().unwrap();assert!(reparsed.attributes.iter().any(|a|a.key==REQUEST));
        assert!(crate::native_media::offers_codec(&sdp,VideoCodec::H264));
        assert!(request.add(&mut sdp).is_err());
        sdp.sdp.push_str(&format!("a={REQUEST}:ff\r\n"));assert!(Request::read(&sdp).is_err());
    }
}
