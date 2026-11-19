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

/// Formats the current EGL display supports as ordinary 2D textures for both NV12 planes.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug)]
pub struct RenderTarget {
    pub device: u64,
    pub linear: bool,
    pub intel_y: bool,
}

#[cfg(target_os = "linux")]
static TARGET: std::sync::RwLock<Option<RenderTarget>> = std::sync::RwLock::new(None);

#[cfg(target_os = "linux")]
pub fn configure_target(target: Option<RenderTarget>, force: bool) {
    *TARGET.write().unwrap() = target;
    set_rendering(force || target.is_some());
}

/// Read once at pool creation; no device queries or locks on the frame path.
#[cfg(target_os = "linux")]
pub(crate) fn supports_pool(device: Option<u64>, modifier: u64) -> bool {
    rendering()
        && TARGET
            .read()
            .unwrap()
            .is_none_or(|t| t.supports(device, modifier))
}

#[cfg(target_os = "linux")]
impl RenderTarget {
    pub fn supports(self, device: Option<u64>, modifier: u64) -> bool {
        device == Some(self.device)
            && match modifier {
                0 => self.linear,
                0x0100_0000_0000_0002 => self.intel_y,
                _ => false,
            }
    }
}

/// Holding the Send + Sync backing frame prevents decoder-pool reuse until
/// the renderer has finished sampling it. This is not the thread-affine VA handle.
pub trait Lease: std::fmt::Debug + Send + Sync {}
impl<T: std::fmt::Debug + Send + Sync> Lease for T {}

#[derive(Debug)]
pub struct Frame {
    pub file: File,
    pub device: Option<u64>,
    pub allocation_size: u32,
    pub width: u32,
    pub height: u32,
    pub pitch: [u32; 2],
    pub offset: [u32; 2],
    pub modifier: u64,
    pub keep_alive: Arc<dyn Lease>,
    /// Shared only by frames belonging to the same VA surface pool.
    pub importable: Arc<AtomicBool>,
}
impl Frame {
    pub fn import_enabled(&self) -> bool {
        self.importable.load(Ordering::Acquire)
    }

    pub fn reject_import(&self) {
        self.importable.store(false, Ordering::Release);
    }

    #[cfg(all(target_os = "linux", feature = "vaapi"))]
    pub fn read_visible_nv12(&self, size: [u32; 2]) -> Result<bytes::Bytes, String> {
        crate::hw::vaapi::download::read_export(self, size)
    }

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
            device: None,
            file: File::open(std::env::current_exe().unwrap()).unwrap(),
            allocation_size: 64 * 32 * 3 / 2,
            width: 64,
            height: 32,
            pitch: [64, 64],
            offset: [0, 64 * 32],
            modifier: 0,
            keep_alive: Arc::new(()),
            importable: Arc::new(AtomicBool::new(true)),
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
    #[cfg(target_os = "linux")]
    #[test]
    fn render_target_checks_the_device_and_exact_plane_modifier() {
        let target = RenderTarget {
            device: 17,
            linear: false,
            intel_y: true,
        };
        assert!(target.supports(Some(17), 0x0100_0000_0000_0002));
        assert!(!target.supports(Some(18), 0x0100_0000_0000_0002));
        assert!(!target.supports(None, 0x0100_0000_0000_0002));
        assert!(!target.supports(Some(17), 0));
        assert!(!target.supports(Some(17), u64::MAX));
    }

    #[test]
    fn failed_import_downgrades_only_frames_from_its_own_pool() {
        let prior = fixture();
        let mut queued = fixture();
        queued.importable = prior.importable.clone();
        let replacement = fixture();
        prior.reject_import();
        assert!(!queued.import_enabled());
        assert!(replacement.import_enabled());
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
