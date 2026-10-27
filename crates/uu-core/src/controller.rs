//! Provider-independent ownership of a controller session.
use tokio::sync::mpsc::UnboundedSender;
pub enum ControllerEvent {
    Status(String),
    Error(String),
}


pub struct ControllerHandle {
    /// True only after release, transport close and runtime shutdown complete.
    pub stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub events: std::sync::mpsc::Receiver<ControllerEvent>,
    pub frames: crate::media::FrameReceiver,
    /// Non-coalesced CONTROL bytes: JSON input or protobuf RPC commands.
    pub input: UnboundedSender<Vec<u8>>,
    /// Explicit native commands; interoperability sessions never expose it.
    pub native_input: Option<crate::native_input::NativeInputSender>,
    /// Raw CONTROL messages (e.g. CaptureSettingRequest protobufs) to send.
    pub settings: UnboundedSender<Vec<u8>>,
    pub close: UnboundedSender<()>,
    /// Live media statistics for the HUD.
    pub stats: std::sync::Arc<crate::media::MediaStats>,
}
