//! Provider-independent encoded output bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureResolution {
    Unchanged,
    Native,
    /// Encoded output bounds; preserves the desktop and its aspect ratio.
    Exact(u32, u32),
}
impl CaptureResolution {
    pub fn output_bounds(self) -> (u32, u32) {
        match self { Self::Exact(width, height) => (width, height), _ => (3840, 2160) }
    }
}
