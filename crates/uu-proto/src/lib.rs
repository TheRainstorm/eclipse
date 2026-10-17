//! UU Remote wire protocol.
//!
//! This crate is deliberately dependency-light: it models the on-the-wire
//! shapes only. Everything here is derived from reverse engineering the
//! official clients (see `docs/protocol-notes.md` for the evidence trail).
//!
//! Layout of the crate:
//! * [`frame`]  — length-prefixed framing used on the control channel
//! * [`msg`]    — control-plane message types
//! * [`ids`]    — device / session identifiers
//! * [`media`]  — media-plane negotiation parameters (ICE, codecs)

pub mod frame;
pub mod ids;
pub mod media;
pub mod msg;
pub mod pck;
pub mod pkt;

pub use frame::{Frame, FrameCodec, FrameError};
pub use ids::{DeviceId, SessionId, UserId};
pub use msg::{ClientMessage, ServerMessage};
pub use pkt::{packetize, Reassembled, Reassembler, DEFAULT_MTU};

/// Protocol revision this crate implements.
///
/// The official Windows client reports `4.42.x` and embeds streamer
/// `V4.8.9.x`; see `docs/protocol-notes.md`.
pub const PROTOCOL_REVISION: &str = "uu/1";

/// Default signaling host, recovered from the Android client's dex and the
/// Windows client's resources.
pub const SIGNALING_HOST: &str = "api.nrd.nie.163.com";
