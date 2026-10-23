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

#[cfg(all(windows, any(feature = "d3d11va", feature = "mf-encode")))]
#[path = "d3d11_frame_windows.rs"]
mod platform;
#[cfg(all(windows, any(feature = "d3d11va", feature = "mf-encode")))]
pub use platform::*;

#[cfg(not(all(windows, any(feature = "d3d11va", feature = "mf-encode"))))]
#[derive(Debug)]
pub struct Frame {
    _private: (),
}
#[cfg(not(all(windows, any(feature = "d3d11va", feature = "mf-encode"))))]
impl Frame {
    pub fn valid(&self, _: [u32; 2]) -> bool {
        false
    }
    pub(crate) fn cropped_view(_: &std::sync::Arc<Self>, _: [u32;2]) -> Result<Self,String> {
        Err("D3D11 display views are unavailable".into())
    }
}
