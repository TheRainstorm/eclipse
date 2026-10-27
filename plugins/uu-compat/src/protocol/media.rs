pub use uu_proto::media::VideoCodec;
use serde::{Serialize,Deserialize};
/// Transport candidates for a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    /// Direct peer-to-peer path.
    Direct,
    /// Relayed through a NetEase edge node.
    Relay,
}

/// A host/relay candidate address pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub kind: TransportKind,
    pub host: String,
    pub port: u16,
    /// ICE-style priority; higher wins.
    pub priority: u32,
}

/// What the host tells the controller it is willing to send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaOffer {
    pub codecs: Vec<VideoCodec>,
    pub candidates: Vec<Candidate>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Target bitrate in kbit/s.
    pub bitrate_kbps: u32,
}

/// Controller-side constraints returned to the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaAnswer {
    pub codec: VideoCodec,
    pub candidates: Vec<Candidate>,
    pub fps: u32,
}
