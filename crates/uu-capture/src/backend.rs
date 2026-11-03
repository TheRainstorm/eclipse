//! Backend trait plus per-platform selection.

use uu_media::frame::RawFrame;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

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
    /// Independently updated pointer metadata, scoped to this capture source.
    fn cursor(&self)->Option<crate::cursor::Stream>{None}
    /// Opt-in overlap requires a real bounded source wait and immutable GPU snapshots.
    /// Unsupported backends fail explicitly; they never poll their cached next_frame.
    fn enable_separate_encode_device(&mut self)->Result<(),CaptureError>{Err(CaptureError::Backend("separate encode device requires unscaled unrotated AMD DXGI capture".into()))}
    fn enable_overlap(&mut self)->Result<(),CaptureError>{Err(CaptureError::Backend("capture/encode overlap requires unscaled same-device DXGI GPU capture".into()))}
    fn wait_updated_frame(&mut self,_wait:std::time::Duration)->Result<Option<RawFrame>,CaptureError>{Err(CaptureError::NoBackend)}
    /// Last returned source image; unsupported metadata is unknown, never inferred from PTS.
    fn frame_info(&self) -> Option<crate::frame_info::FrameInfo> { None }
    /// Actual HDR source metadata, never inferred from bit depth alone.
    fn hdr_metadata(&self)->Option<uu_media::hdr::Metadata>{None}
    /// Hint capture pacing without replacing the authorized desktop session.
    /// Backends may retain the source cadence; the producer controls output FPS.
    fn set_frame_rate(&mut self,_fps:u32)->Result<(),CaptureError>{Ok(())}
    /// Opt in only when the selected encoder accepts native GPU surfaces.
    fn prefer_gpu(&mut self,_enabled:bool,_vendor:Option<u16>) {}
    /// Optional immutable capture-device lease for encoder initialization.
    fn encoder_input_device(&self)->Option<uu_media::encoder::InputDevice>{None}
    fn resolution(&self) -> (u32, u32);
    /// Global pixels represented by a whole-monitor capture. Synthetic and
    /// window sources do not authorize desktop input through this interface.
    fn desktop_rect(&self) -> Option<DesktopRect> {
        None
    }
    /// A live physical input region, cleared while the selected desktop image
    /// is invalid. The input owner must not inject using a retired rectangle.
    fn desktop_mapping(&self) -> Option<DesktopMapping> { None }
    /// A compositor-authorized injector belonging to this capture session.
    /// Platforms with independent injection leave this empty.
    fn input_injector(&self) -> Option<Box<dyn uu_input::InputInjector>> {
        None
    }
    /// Block until the next frame is available.
    fn next_frame(&mut self) -> Result<RawFrame, CaptureError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone)]
pub struct DesktopMapping(std::sync::Arc<MappingState>);
struct MappingState {
    bounds:std::sync::Mutex<(Option<DesktopRect>,u64)>,
    live:std::sync::atomic::AtomicBool,
    revision:std::sync::atomic::AtomicU64,
}
impl DesktopMapping {
    pub fn new(rect:DesktopRect)->Self {Self(std::sync::Arc::new(MappingState {
        bounds:std::sync::Mutex::new((Some(rect),0)),live:std::sync::atomic::AtomicBool::new(true),revision:std::sync::atomic::AtomicU64::new(0),
    }))}
    pub fn get(&self)->Option<DesktopRect> {self.0.bounds.lock().unwrap().0}
    /// Serialize coordinate injection with retirement.
    pub fn lock(&self)->std::sync::MutexGuard<'_,(Option<DesktopRect>,u64)> {self.0.bounds.lock().unwrap()}
    /// Every update represents a new capture binding, even at the same origin.
    pub fn update(&self,rect:Option<DesktopRect>) {
        use std::sync::atomic::Ordering;
        let mut value=self.0.bounds.lock().unwrap();
        self.0.live.store(false,Ordering::Release);
        value.0=rect;value.1=value.1.wrapping_add(1);
        self.0.revision.store(value.1,Ordering::Release);
        self.0.live.store(rect.is_some(),Ordering::Release);
    }
    /// Nonblocking gate for input devices whose release path waits for its own
    /// worker. Such workers must not take the coordinate lock during reset.
    pub fn live_revision(&self)->Option<u64> {
        use std::sync::atomic::Ordering;
        let revision=self.0.revision.load(Ordering::Acquire);
        (self.0.live.load(Ordering::Acquire) && self.0.revision.load(Ordering::Acquire)==revision).then_some(revision)
    }
}

/// Describe the backends compiled into this binary.
pub fn available_backends() -> Vec<&'static str> {
    let mut v = Vec::new();
    #[cfg(target_os = "linux")]
    {
        v.push("portal-pipewire");
        v.push("wayland-screencopy");
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
    fn desktop_mapping_shares_retirement_and_relocation_without_changing_pixels() {
        let before=DesktopRect{x:-1920,y:0,width:1920,height:1080};
        let mapping=DesktopMapping::new(before);let input=mapping.clone();
        assert_eq!(input.get(),Some(before));assert_eq!(input.live_revision(),Some(0));
        mapping.update(None);assert_eq!(input.get(),None);assert_eq!(input.live_revision(),None);
        let after=DesktopRect{x:640,y:-1080,..before};mapping.update(Some(after));
        assert_eq!(input.get(),Some(after));assert_eq!(input.live_revision(),Some(2));
        mapping.update(Some(after));assert_eq!(input.live_revision(),Some(3));
        assert_eq!(after.width,before.width);
    }
    #[test]
    fn synthetic_backend_is_always_compiled_in() {
        assert!(available_backends().contains(&"synthetic"));
    }
}
