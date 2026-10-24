//! Authenticated, optional HDR10 negotiation. Absence always means SDR.
//! Independent attributes preserve older strict video-setting envelopes.
use crate::native_quality::{get, put};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use uu_proto::media::VideoCodec;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
const REQUEST: &str = "x-eclipse-hdr10-client-v1";
const RESPONSE: &str = "x-eclipse-hdr10-host-v1";
pub use uu_media::hdr::Metadata;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub codecs: Vec<VideoCodec>,
}
impl Request {
    fn validate(&self) -> Result<()> {
        ensure!(
            !self.codecs.is_empty() && self.codecs.len() <= 2,
            "HDR 解码能力列表无效"
        );
        for (i, codec) in self.codecs.iter().enumerate() {
            ensure!(
                *codec != VideoCodec::H264 && !self.codecs[..i].contains(codec),
                "HDR 需要 10 位 HEVC 或 AV1 解码"
            );
        }
        Ok(())
    }
    pub fn add(&self, sdp: &mut RTCSessionDescription) -> Result<()> {
        self.validate()?;
        put(sdp, REQUEST, self)
    }
    pub fn read(sdp: &RTCSessionDescription) -> Result<Option<Self>> {
        let value: Option<Self> = get(sdp, REQUEST)?;
        if let Some(v) = &value {
            v.validate()?;
        }
        Ok(value)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub codec: VideoCodec,
    pub metadata: Metadata,
}
impl Response {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.codec != VideoCodec::H264 && self.metadata.valid(),
            "主机 HDR10 色彩信息无效"
        );
        Ok(())
    }
    pub fn add(&self, sdp: &mut RTCSessionDescription) -> Result<()> {
        self.validate()?;
        put(sdp, RESPONSE, self)
    }
    pub fn read(sdp: &RTCSessionDescription) -> Result<Option<Self>> {
        let value: Option<Self> = get(sdp, RESPONSE)?;
        if let Some(v) = &value {
            v.validate()?;
        }
        Ok(value)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hdr_attributes_are_optional_bounded_and_independent_of_legacy_video_settings() {
        let mut sdp=RTCSessionDescription::offer("v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nm=video 9 UDP/TLS/RTP/SAVPF 104\r\na=rtpmap:104 H265/90000\r\n".into()).unwrap();
        assert_eq!(Request::read(&sdp).unwrap(), None);
        assert_eq!(Response::read(&sdp).unwrap(), None);
        let legacy = crate::native_quality::Request {
            fps: 60,
            bounds: None,
            bitrate_kbps: None,
            codec: None,
        };
        legacy.add(&mut sdp).unwrap();
        let hdr = Request {
            codecs: vec![VideoCodec::Hevc, VideoCodec::Av1],
        };
        hdr.add(&mut sdp).unwrap();
        assert_eq!(Request::read(&sdp).unwrap(), Some(hdr.clone()));
        assert_eq!(
            crate::native_quality::Request::read(&sdp).unwrap(),
            Some(legacy)
        );
        assert!(sdp.unmarshal().is_ok());
        assert!(hdr.add(&mut sdp).is_err());
        assert!(Request {
            codecs: vec![VideoCodec::H264]
        }
        .add(&mut sdp)
        .is_err());
        assert!(Request {
            codecs: vec![VideoCodec::Hevc; 2]
        }
        .validate()
        .is_err());
    }
}
