//! Orchestration: configuration, signaling, and the session state machine.

pub mod api;
mod registration;
pub mod assistance;
mod private_file;
mod listen_socket;
pub mod config;
pub mod control_wire;
pub mod controller;
pub mod display_frames;
mod video_receiver;
pub mod error;
pub mod media;
pub mod native_pairing;
pub mod native_discovery;
pub mod native_lan_store;
pub mod native_lan;
pub mod native_controller;
pub mod capabilities;
pub mod native;
pub mod direct_ice;
pub mod native_signaling;
pub mod internet;
pub mod native_input;
mod native_control;
mod native_clipboard;
mod native_audio;
mod native_media;
mod network_probe;
pub mod connection_info;
pub mod device_trust;
pub mod device_auth;
pub mod device_channel;
pub mod shared_host;
pub mod host_admin;
pub mod device_book;
pub mod client_settings;
pub mod device_internet;
pub mod device_profile;
#[cfg(test)]
mod device_network_tests;
mod decode_gate;
mod test_loss;
mod srtp_policy;
pub mod selfloop;
pub mod session;
pub mod session_diagnostics;
pub mod sign;
mod compatibility_signing;
pub mod signing_key;
pub mod signal;
pub mod socketio;

pub use api::{ClientState, QrLogin, UuApi};
pub use config::Config;
pub use error::CoreError;
pub use session::{Role, Session, SessionState};
pub use signal::SignalClient;
pub use sign::ClientIdentity;
pub use selfloop::SelfLoopConfig;


/// Select a process-wide rustls CryptoProvider. Both `ring` and `aws-lc-rs`
/// can be enabled transitively, in which case rustls refuses to guess.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub mod rtx;

pub mod screen_control;

pub mod cursor;
pub mod clipboard;

mod congestion;

pub mod frame_cadence;
pub mod native_timing;
pub mod native_displays;
mod native_video;
mod av1_rtp;

#[cfg(windows)]
mod video_timing;

pub mod input_ownership;

pub mod native_gamepad;

pub mod native_quality;
