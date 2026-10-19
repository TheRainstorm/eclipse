//! Backend trait plus per-platform selection.

use uu_media::frame::RawFrame;

/// Which source to capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureTarget {
    Primary,
    Output(String),
    Window(u64),
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("no capture backend is available in this environment")]
    NoBackend,
    #[error("target {0:?} not found")]
    TargetNotFound(CaptureTarget),
    #[error("backend error: {0}")]
    Backend(String),
    #[error("capture was denied by the compositor")]
    Denied,
}

/// A running capture stream.
///
/// Frames come back as `uu_media::RawFrame` so the same value flows straight
/// into the encoder; `RawFrame::gpu` distinguishes a CPU buffer from a GPU
/// handle that still needs a copy/download step.
pub trait Capture: Send {
    fn resolution(&self) -> (u32, u32);
    /// Global pixels represented by a whole-monitor capture. Synthetic and
    /// window sources do not authorize desktop input through this interface.
    fn desktop_rect(&self) -> Option<DesktopRect> {
        None
    }
    /// Block until the next frame is available.
    fn next_frame(&mut self) -> Result<RawFrame, CaptureError>;
}

#[derive(Debug, Clone, Copy)]
pub struct DesktopRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Describe the backends compiled into this binary.
pub fn available_backends() -> Vec<&'static str> {
    let mut v = Vec::new();
    #[cfg(target_os = "linux")]
    {
        v.push("x11-getimage");
    }
    #[cfg(target_os = "windows")]
    {
        v.push("dxgi-duplication");
    }
    // Always available, on every platform: used by tests and by the
    // self-loop harness when there is no display to capture.
    v.push("synthetic");
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_backend_is_always_compiled_in() {
        assert!(available_backends().contains(&"synthetic"));
    }
}
