//! Orchestration: configuration, signaling, and the session state machine.

pub mod api;
mod private_file;
pub mod config;
pub mod control_wire;
pub mod controller;
mod display_frames;
mod video_receiver;
pub mod error;
pub mod media;
pub mod native_pairing;
pub mod native_controller;
pub mod capabilities;
pub mod native;
pub mod native_input;
mod native_control;
mod native_audio;
mod native_media;
mod network_probe;
mod decode_gate;
mod test_loss;
pub mod selfloop;
pub mod session;
pub mod session_diagnostics;
pub mod sign;
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
