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
pub fn receive_video_codec(capability: &webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability) -> Option<uu_proto::media::VideoCodec> {
    crate::media::video_codec(capability)
}

pub fn pack_envelope_sdp(value:serde_json::Value)->anyhow::Result<String>{crate::internet::pack_sdp(value)}
pub fn unpack_envelope_sdp(sdp:&str)->anyhow::Result<serde_json::Value>{crate::internet::unpack_sdp(sdp)}
