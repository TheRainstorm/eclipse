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
    /// The current session/desktop is temporarily unavailable. No cached pixels
    /// may be emitted until capture resumes; callers may retry with cancellation.
    #[error("采集暂停：{0}")]
    Suspended(String),
    #[error("系统未授权屏幕共享，连接已取消")]
    Denied,
}

/// A running capture stream.
///
/// Frames come back as `uu_media::RawFrame` so the same value flows straight
/// into the encoder; `RawFrame::gpu` distinguishes a CPU buffer from a GPU
/// handle that still needs a copy/download step.
pub trait Capture: Send {
    /// Update capture pacing without replacing the authorized desktop session.
    fn set_frame_rate(&mut self,_fps:u32)->Result<(),CaptureError>{Ok(())}
    /// Opt in only when the selected encoder accepts native GPU surfaces.
    fn prefer_gpu(&mut self,_enabled:bool,_vendor:Option<u16>) {}
    fn resolution(&self) -> (u32, u32);
    /// Global pixels represented by a whole-monitor capture. Synthetic and
    /// window sources do not authorize desktop input through this interface.
    fn desktop_rect(&self) -> Option<DesktopRect> {
        None
    }
    /// A compositor-authorized injector belonging to this capture session.
    /// Platforms with independent injection leave this empty.
    fn input_injector(&self) -> Option<Box<dyn uu_input::InputInjector>> {
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
        v.push("portal-pipewire");
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
