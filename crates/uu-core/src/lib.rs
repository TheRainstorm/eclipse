//! Orchestration: configuration, signaling, and the session state machine.

pub mod api;
pub mod config;
pub mod control_wire;
pub mod controller;
mod display_frames;
mod video_receiver;
pub mod error;
pub mod media;
mod network_probe;
pub mod selfloop;
pub mod session;
pub mod sign;
pub mod signal;
pub mod socketio;

pub use api::{ClientState, QrLogin, UuApi};
pub use config::Config;
pub use error::CoreError;
pub use session::{Role, Session, SessionState};
pub use signal::SignalClient;
pub use sign::{ClientIdentity, NATIVE_SIGN_KEY};
pub use selfloop::SelfLoopConfig;


/// Select a process-wide rustls CryptoProvider. Both `ring` and `aws-lc-rs`
/// can be enabled transitively, in which case rustls refuses to guess.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub mod rtx;
