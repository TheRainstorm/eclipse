//! Owned DMA-BUF metadata. Driver/VA objects never cross the decoder thread.
use std::{
    fs::File,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

static RENDERING: AtomicBool = AtomicBool::new(false);
pub fn set_rendering(enabled: bool) {
    RENDERING.store(enabled, Ordering::Release);
}
pub fn rendering() -> bool {
    RENDERING.load(Ordering::Acquire)
}

/// Holding the Send + Sync backing frame prevents decoder-pool reuse until
/// the renderer has finished sampling it. This is not the thread-affine VA handle.
pub trait Lease: std::fmt::Debug + Send + Sync {}
impl<T: std::fmt::Debug + Send + Sync> Lease for T {}

#[derive(Debug)]
pub struct Frame {
    pub file: File,
    pub allocation_size: u32,
    pub width: u32,
    pub height: u32,
    pub pitch: [u32; 2],
    pub offset: [u32; 2],
    pub modifier: u64,
    pub keep_alive: Arc<dyn Lease>,
}
impl Frame {
    /// Initially support only uncompressed linear/Intel Y NV12. Auxiliary
    /// compression planes and unfamiliar modifiers retain CPU readback.
    pub fn valid(&self, visible: [u32; 2]) -> bool {
        let [w, h] = visible;
        if w < 2
            || h < 2
            || w % 2 != 0
            || h % 2 != 0
            || self.width < w
            || self.height < h
            || self.width > 8192
            || self.height > 8192
            || self.width % 2 != 0
            || self.height % 2 != 0
            || !matches!(self.modifier, 0 | 0x0100_0000_0000_0002)
            || self.allocation_size == 0
            || self.allocation_size > 128 * 1024 * 1024
        {
            return false;
        }
        for p in 0..2 {
            let rows = self.height / if p == 0 { 1 } else { 2 };
            let pitch = self.pitch[p] as u64;
            if pitch < self.width as u64
                || pitch > i32::MAX as u64
                || self.offset[p] > i32::MAX as u32
            {
                return false;
            }
            let rows = if self.modifier == 0 {
                rows as u64
            } else {
                if pitch % 128 != 0 || self.offset[p] % 4096 != 0 {
                    return false;
                }
                (rows as u64 + 31) / 32 * 32
            };
            if self.offset[p] as u64 + pitch * rows > self.allocation_size as u64 {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Frame {
        Frame {
            file: File::open(std::env::current_exe().unwrap()).unwrap(),
            allocation_size: 64 * 32 * 3 / 2,
            width: 64,
            height: 32,
            pitch: [64, 64],
            offset: [0, 64 * 32],
            modifier: 0,
            keep_alive: Arc::new(()),
        }
    }
    #[test]
    fn crop_and_plane_ranges_reject_unsupported_or_truncated_imports() {
        let mut f = fixture();
        assert!(f.valid([64, 32]));
        assert!(f.valid([62, 30]));
        for visible in [[65, 32], [64, 33], [64, 0], [63, 32]] {
            assert!(!f.valid(visible));
        }
        f.allocation_size -= 1;
        assert!(!f.valid([64, 32]));
        f.allocation_size += 1;
        f.pitch[1] = 63;
        assert!(!f.valid([64, 32]));
        f.pitch[1] = 64;
        f.offset[1] = u32::MAX;
        assert!(!f.valid([64, 32]));
        f.offset[1] = 64 * 32;
        f.modifier = 0x0100_0000_0000_0004;
        assert!(!f.valid([64, 32]));
        f.modifier = 0x0100_0000_0000_0002;
        assert!(!f.valid([64, 32]));
        f.pitch = [128, 128];
        f.offset = [0, 4096];
        f.allocation_size = 8192;
        assert!(f.valid([64, 32]));
        f.offset[1] += 1;
        assert!(!f.valid([64, 32]));
    }
    #[test]
    fn clones_keep_backing_storage_until_last_consumer_releases() {
        #[derive(Debug)]
        struct Backing(Arc<AtomicBool>);
        impl Drop for Backing {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let released = Arc::new(AtomicBool::new(false));
        let mut f = fixture();
        f.keep_alive = Arc::new(Backing(released.clone()));
        let frame = Arc::new(f);
        let render = frame.clone();
        drop(frame);
        assert!(!released.load(Ordering::Acquire));
        drop(render);
        assert!(released.load(Ordering::Acquire));
    }
}
