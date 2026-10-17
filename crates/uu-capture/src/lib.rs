//! Screen capture.
//!
//! * Windows: Windows.Graphics.Capture (WGC) for windows, DXGI Desktop
//!   Duplication for whole monitors. The official client ships an Indirect
//!   Display Driver to fabricate extra monitors; on Linux we instead rely on
//!   the compositor (Xorg dummy/Xvfb or the session's real outputs).
//! * Linux: PipeWire screencast via `xdg-desktop-portal` on Wayland, and
//!   X11 shared memory capture as a fallback.
//! * Everywhere: [`synthetic::SyntheticCapture`], used by tests and the
//!   headless self-loop so no display is required.

pub mod backend;
pub mod synthetic;

pub use backend::{available_backends, Capture, CaptureError, CaptureTarget};
pub use synthetic::SyntheticCapture;

/// Which source to capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureSource {
    /// A whole monitor identified by compositor output name.
    Monitor(String),
    /// A single window, identified by native handle.
    Window(u64),
    /// Let the backend pick the primary output.
    Primary,
    /// Deterministic test pattern (no display required).
    Synthetic { width: u32, height: u32, fps: u32 },
}

/// Open the best available capture for `source`.
pub fn open(source: &CaptureSource) -> Result<Box<dyn Capture>, CaptureError> {
    match source {
        CaptureSource::Synthetic { width, height, fps } => {
            Ok(Box::new(SyntheticCapture::new(*width, *height, *fps)))
        }
        other => Err(CaptureError::TargetNotFound(match other {
            CaptureSource::Monitor(name) => CaptureTarget::Output(name.clone()),
            CaptureSource::Window(id) => CaptureTarget::Window(*id),
            _ => CaptureTarget::Primary,
        })),
    }
}
