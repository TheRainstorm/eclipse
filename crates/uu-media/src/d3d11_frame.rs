//! Leased D3D11 decoder output. The sample, not just its texture, owns the
//! decoder-pool slot. Dropping an unrendered frame never waits for the GPU.
use std::sync::atomic::{AtomicBool, Ordering};
static RENDERING: AtomicBool = AtomicBool::new(false);
pub fn set_rendering(enabled: bool) {
    RENDERING.store(enabled, Ordering::Release);
}
pub fn rendering() -> bool {
    RENDERING.load(Ordering::Acquire)
}

#[cfg(all(windows, feature = "d3d11va"))]
#[path = "d3d11_frame_windows.rs"]
mod platform;
#[cfg(all(windows, feature = "d3d11va"))]
pub use platform::*;

#[cfg(not(all(windows, feature = "d3d11va")))]
#[derive(Debug)]
pub struct Frame {
    _private: (),
}
#[cfg(not(all(windows, feature = "d3d11va")))]
impl Frame {
    pub fn valid(&self, _: [u32; 2]) -> bool {
        false
    }
}
