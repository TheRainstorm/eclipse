//! Control-plane messages.

use serde::{Deserialize, Serialize};

use crate::ids::{DeviceId, SessionId, UserId};
use crate::media::{MediaAnswer, MediaOffer};

/// Controller -> service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Authenticate the control channel.
    Auth { user: UserId, token: String },
    /// Ask the service for the account's controllable devices.
    ListDevices,
    /// Request a session against a specific host.
    OpenSession { device: DeviceId },
    /// Complete negotiation after the host offered media parameters.
    Answer { session: SessionId, answer: MediaAnswer },
    /// Tear a session down.
    CloseSession { session: SessionId },
    KeepAlive,
}

/// Service -> controller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMessage {
    AuthOk { user: UserId },
    Devices { devices: Vec<DeviceId> },
    /// Session accepted; the host is preparing media.
    SessionOpened { session: SessionId, host: DeviceId },
    /// Media parameters proposed by the host.
    Offer { session: SessionId, offer: MediaOffer },
    /// Peer is ready; media can start flowing.
    Ready { session: SessionId },
    Closed { session: SessionId, reason: String },
    Error { code: i32, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_message_json_shape_is_stable() {
        let m = ClientMessage::OpenSession { device: DeviceId("abc".into()) };
        let j = serde_json::to_string(&m).unwrap();
        assert_eq!(j, r#"{"t":"open_session","device":"abc"}"#);
    }

    #[test]
    fn server_message_roundtrip() {
        let m = ServerMessage::Error { code: 7, message: "nope".into() };
        let j = serde_json::to_string(&m).unwrap();
        let back: ServerMessage = serde_json::from_str(&j).unwrap();
        assert_eq!(m, back);
    }
}
