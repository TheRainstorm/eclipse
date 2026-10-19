//! Screen capture.
//!
//! * Windows: DXGI Desktop Duplication for active whole monitors. Window
//!   capture and compositor cursor composition are not implemented yet.
//! * Linux: X11 GetImage capture for authenticated X11 sessions. Wayland
//!   screencast needs a portal backend; Xwayland cannot substitute for it.
//! * Everywhere: [`synthetic::SyntheticCapture`], used by tests and the
//!   headless self-loop so no display is required.

pub mod backend;
mod pixels;
pub mod synthetic;
#[cfg(windows)]
pub mod windows;
#[cfg(target_os = "linux")]
pub mod x11;

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
        #[cfg(target_os = "linux")]
        other => Ok(Box::new(x11::X11Capture::open(None, other)?)),
        #[cfg(windows)]
        other => Ok(Box::new(windows::DxgiCapture::open(other)?)),
        #[cfg(not(any(target_os = "linux", windows)))]
        other => Err(CaptureError::TargetNotFound(match other {
            CaptureSource::Monitor(name) => CaptureTarget::Output(name.clone()),
            CaptureSource::Window(id) => CaptureTarget::Window(*id),
            _ => CaptureTarget::Primary,
        })),
    }
}
