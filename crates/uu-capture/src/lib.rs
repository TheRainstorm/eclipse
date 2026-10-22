//! Screen capture.
//!
//! * Windows: DXGI Desktop Duplication for active whole monitors. Window
//!   capture and compositor cursor composition are not implemented yet.
//! * Linux: desktop portal / PipeWire on Wayland, authenticated X11 GetImage
//!   on X11. Xwayland cannot substitute for Wayland desktop authorization.
//! * Everywhere: [`synthetic::SyntheticCapture`], used by tests and the
//!   headless self-loop so no display is required.

pub mod backend;
mod pixels;
#[cfg(target_os = "linux")]
pub mod portal;
pub mod synthetic;
#[cfg(windows)]
pub mod windows;
#[cfg(target_os = "linux")]
pub mod x11;

pub use backend::{available_backends, Capture, CaptureError, CaptureTarget, DesktopRect};
pub use synthetic::SyntheticCapture;

#[derive(Clone)]
pub struct OpenOptions {
    pub allow_input: bool,
    pub fps: u32,
    /// Cancels a pending compositor permission request as well as capture.
    pub cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            allow_input: false,
            fps: 60,
            cancelled: Default::default(),
        }
    }
}

pub fn uses_desktop_portal() -> bool {
    cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
}

/// Which source to capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureSource {
    /// A whole monitor identified by compositor output name.
    Monitor(String),
    /// A single window, identified by native handle.
    Window(u64),
    /// Let the backend pick the primary output.
    Primary,
    /// Explicit permission to select among desktop monitors during a session.
    Desktop,
    /// Deterministic test pattern (no display required).
    Synthetic { width: u32, height: u32, fps: u32 },
}

/// Open the best available capture for `source`.
pub fn open(source: &CaptureSource) -> Result<Box<dyn Capture>, CaptureError> {
    open_with_options(source, OpenOptions::default())
}

pub fn open_with_options(
    source: &CaptureSource,
    options: OpenOptions,
) -> Result<Box<dyn Capture>, CaptureError> {
    match source {
        CaptureSource::Synthetic { width, height, fps } => {
            Ok(Box::new(SyntheticCapture::new(*width, *height, *fps)))
        }
        #[cfg(target_os = "linux")]
        other if uses_desktop_portal() => {
            Ok(Box::new(portal::PortalCapture::open(other, options)?))
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
