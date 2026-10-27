//! Engine services available to in-process protocol plugins. Capture, decode,
//! queues, device access and GPU frame ownership remain engine responsibilities.
use std::sync::{Arc, atomic::AtomicU64};
use crate::media::{FrameSender, MediaStats};
use webrtc::{peer_connection::RTCPeerConnection, track::track_remote::TrackRemote};

pub async fn receive_audio(track: Arc<TrackRemote>, stats: Option<Arc<MediaStats>>) {
    crate::media::receive_audio(track, stats).await;
}
pub async fn receive_video(pc: Arc<RTCPeerConnection>, track: Arc<TrackRemote>, index: u32,
    frames: Option<FrameSender>, stats: Option<Arc<MediaStats>>, repairs: Arc<crate::rtx::RepairBus>) {
    crate::media::receive_video(pc, track, index, frames, stats, repairs, None, None, None).await;
}
pub fn configure_congestion(registry: webrtc::interceptor::registry::Registry,
    media: &mut webrtc::api::media_engine::MediaEngine, dropped: Arc<AtomicU64>, recorded: Arc<AtomicU64>)
    -> anyhow::Result<webrtc::interceptor::registry::Registry> {
    crate::congestion::configure(registry, media, dropped, recorded)
}
pub fn configure_srtp(settings: &mut webrtc::api::setting_engine::SettingEngine) {
    crate::srtp_policy::configure(settings);
}
pub fn write_private_file(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    crate::private_file::write_atomic(path, data)
}
pub use crate::network_probe::NetworkProbe;
pub use crate::native::{EncodedCapture,CapturedSample,CaptureOptions};
pub use crate::native_control::InputSession;
pub fn stop_clipboard(state:&crate::clipboard::Clipboard) {crate::native_clipboard::stop(state);}

/// Standard RTP only: no private native FEC or control handshake is implied.
pub struct VideoSender(crate::native_video::Sender);
impl VideoSender {
    pub fn new(track:Arc<webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP>,codec:uu_proto::media::VideoCodec)->Self {
        Self(crate::native_video::Sender::new(track,codec))
    }
    pub async fn send(&mut self,frame:&uu_media::EncodedFrame)->anyhow::Result<()> {self.0.frame(frame).await}
}
pub struct OpusSender(crate::native_media::AudioSender);
impl OpusSender {
    pub fn new(track:Arc<webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP>)->Self {
        Self(crate::native_media::AudioSender::new(track,false))
    }
    pub async fn send(&mut self,frame:uu_media::audio_capture::EncodedAudio)->anyhow::Result<usize>{self.0.send(frame).await}
}
pub struct AudioCapture {
    worker:crate::native_audio::Worker,
    frames:tokio::sync::mpsc::Receiver<uu_media::audio_capture::EncodedAudio>,
    state:tokio::sync::watch::Receiver<crate::native_audio::State>,
}
impl AudioCapture {
    pub fn start(source:uu_media::audio_capture::Source)->anyhow::Result<Self>{
        let(worker,frames,state)=crate::native_audio::Worker::start(source)?;Ok(Self{worker,frames,state})
    }
    pub fn status(&self)->String{self.state.borrow().label()}
    pub fn counters(&self)->(u64,u64){(self.worker.errors.load(std::sync::atomic::Ordering::Relaxed),self.worker.dropped.load(std::sync::atomic::Ordering::Relaxed))}
    pub async fn next(&mut self)->Option<uu_media::audio_capture::EncodedAudio>{self.frames.recv().await}
    pub async fn stop(&mut self){self.frames.close();self.worker.stop().await;}
}
pub fn receive_video_codec(capability: &webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability) -> Option<uu_proto::media::VideoCodec> {
    crate::media::video_codec(capability)
}

pub fn pack_envelope_sdp(value:serde_json::Value)->anyhow::Result<String>{crate::internet::pack_sdp(value)}
pub fn unpack_envelope_sdp(sdp:&str)->anyhow::Result<serde_json::Value>{crate::internet::unpack_sdp(sdp)}
