//! One native RTP send path: capture PTS owns the media clock for every codec.
use anyhow::{ensure,Result};
use bytes::Bytes;
use std::sync::Arc;
use uu_proto::media::VideoCodec;
use webrtc::{rtp::{packet::Packet,header::Header,packetizer::Payloader,sequence::{Sequencer,new_random_sequencer}},track::track_local::{track_local_static_rtp::TrackLocalStaticRTP,TrackLocalWriter}};
#[derive(Clone,Debug,Default)]
struct HevcPayloader;
impl Payloader for HevcPayloader {
    fn payload(&mut self,mtu:usize,data:&Bytes)->std::result::Result<Vec<Bytes>,webrtc::rtp::Error>{
        let mut packets=Vec::new();if mtu<4{return Ok(packets);}
        for nal in uu_media::bitstream::annexb_nals(data){
            if nal.len()<2 || nal[0]&128!=0 || nal[1]&7==0 || (nal[0]>>1)&63>=48{return Ok(Vec::new());}
            if nal.len()<=mtu {packets.push(Bytes::copy_from_slice(nal));continue;}
            let kind=(nal[0]>>1)&63;let chunks=nal[2..].chunks(mtu-3);let count=chunks.len();
            for (index,chunk) in chunks.enumerate(){
                let mut fragment=Vec::with_capacity(chunk.len()+3);
                fragment.extend_from_slice(&[(nal[0]&0x81)|(49<<1),nal[1],kind|if index==0{128}else{0}|if index+1==count{64}else{0}]);
                fragment.extend_from_slice(chunk);packets.push(fragment.into());
            }
        }Ok(packets)
    }
    fn clone_to(&self)->Box<dyn Payloader+Send+Sync>{Box::new(self.clone())}
}
pub(crate) struct Sender {
    track:Arc<TrackLocalStaticRTP>,payloader:Box<dyn Payloader+Send+Sync>,sequence:Box<dyn Sequencer+Send+Sync>,
    origin:Option<u64>,last_pts:Option<u64>,timestamp:u32,
    fec:Option<crate::native_fec::Encoder>,
    ssrc:u32,last_rtp:Option<u32>,

}
pub(crate) struct SendTiming {
    pub timestamp: u32,
    pub first: std::time::Instant,
    pub last: std::time::Instant,
    pub last_qpc: Option<uu_capture::frame_info::SourceTime>,
}
impl Sender {
    pub fn new(track:Arc<TrackLocalStaticRTP>,codec:VideoCodec)->Self{
        let payloader:Box<dyn Payloader+Send+Sync>=match codec{VideoCodec::H264=>Box::<webrtc::rtp::codecs::h264::H264Payloader>::default(),VideoCodec::Hevc=>Box::<HevcPayloader>::default(),VideoCodec::Av1=>Box::<webrtc::rtp::codecs::av1::Av1Payloader>::default(),VideoCodec::Pyrowave=>Box::<crate::pyrowave_rtp::Fragmenter>::default()};
        Self{track,payloader,sequence:Box::new(new_random_sequencer()),origin:None,last_pts:None,timestamp:rand::random(),fec:None,ssrc:0,last_rtp:None}
    }
    pub fn with_fec(track:Arc<TrackLocalStaticRTP>,codec:VideoCodec,transport:Arc<crate::native_fec::Transport>)->Self {
        let mut sender=Self::new(track,codec);sender.fec=Some(crate::native_fec::Encoder::new(transport));sender
    }
    pub(crate) fn with_ssrc(mut self,ssrc:u32)->Self {self.ssrc=ssrc;self}
    pub(crate) fn ssrc(&self)->u32 {self.ssrc}
    pub(crate) fn last_timestamp(&self)->Option<u32> {self.last_rtp}
    pub async fn frame(&mut self,encoded:&uu_media::EncodedFrame)->Result<()>{
        self.transmit(encoded,false).await.map(|_|())
    }
    pub async fn frame_timed(&mut self,encoded:&uu_media::EncodedFrame)->Result<SendTiming>{
        Ok(self.transmit(encoded,true).await?.expect("packetized video has first/last writes"))
    }
    async fn transmit(&mut self,encoded:&uu_media::EncodedFrame,timed:bool)->Result<Option<SendTiming>>{
        ensure!(encoded.data.len()<=crate::video_receiver::MAX_AU,"native encoded frame exceeds access-unit budget");
        let origin=*self.origin.get_or_insert(encoded.pts.0);
        ensure!(encoded.pts.0>=origin && self.last_pts.map_or(true,|previous|encoded.pts.0>previous),"native encoder timestamp did not advance");
        let timestamp=self.timestamp.wrapping_add(((encoded.pts.0-origin) as u128*90_000/1_000_000) as u32);
        let mtu=if self.fec.as_ref().is_some_and(|fec|fec.enabled()){crate::native_fec::PAYLOAD}else{1188};
        let payloads=self.payloader.payload(mtu,&encoded.data)?;
        ensure!(!payloads.is_empty(),"native encoder emitted no packetizable video");
        let count=payloads.len();
        let mut first=None;let mut last=None;let mut last_qpc=None;
        for (index,payload) in payloads.into_iter().enumerate(){
            let packet=Packet{header:Header{version:2,sequence_number:self.sequence.next_sequence_number(),timestamp,marker:index+1==count,..Default::default()},payload};
            self.track.write_rtp(&packet).await?;
            if timed && (index==0 || index+1==count) {
                let now=std::time::Instant::now();
                if index==0 {first=Some(now);}last=Some(now);
                if index+1==count {last_qpc=crate::frame_trace::source_clock();}
            }
            if let Some(fec)=&mut self.fec {fec.packet(&packet);}
        }self.last_pts=Some(encoded.pts.0);self.last_rtp=Some(timestamp);
        Ok(first.zip(last).map(|(first,last)|SendTiming{timestamp,first,last,last_qpc}))
    }

}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::video_receiver::VideoReceiver;
    #[test]
    fn hevc_single_and_fragmented_nals_roundtrip_with_sequence_wrap(){
        let data=uu_media::hevc::PROBE;let mut payloader=HevcPayloader;
        // The sample contains four access units. Packetize its first AUD span.
        let end=data.windows(5).enumerate().skip(1).find(|(_,w)|*w==[0,0,0,1,70]).map_or(data.len(),|(i,_)|i);
        for mtu in [9,1200]{
            let payloads=payloader.payload(mtu,&Bytes::copy_from_slice(&data[..end])).unwrap();let count=payloads.len();
            let mut receiver=VideoReceiver::with_codec(std::time::Duration::from_millis(20),VideoCodec::Hevc);let mut units=Vec::new();
            for (i,payload) in payloads.into_iter().enumerate(){units.extend(receiver.push(Packet{header:Header{sequence_number:65534u16.wrapping_add(i as u16),timestamp:123,marker:i+1==count,..Default::default()},payload},std::time::Instant::now()));}
            assert_eq!(units.len(),1);assert!(uu_media::hevc::is_keyframe(&units[0].data));assert_eq!(uu_media::hevc::sequence(&units[0].data),uu_media::hevc::sequence(data));
        }
    }
}
