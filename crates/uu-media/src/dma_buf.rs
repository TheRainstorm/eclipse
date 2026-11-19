//! Owned DMA-BUF metadata. Driver/VA objects never cross the decoder thread.
use crate::PixelFormat;
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

/// Layouts both exported planes can use as ordinary EGL 2D textures.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Default)]
pub struct Modifiers {
    pub linear: bool,
    pub intel_y: bool,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug)]
pub struct RenderTarget {
    pub device: u64,
    pub nv12: Modifiers,
    pub p010: Modifiers,
}

// An explicit legacy NV12 override may try the real import without a modifier
// query. It cannot opt a 10-bit surface into an unknown EGL device or layout.
#[cfg(target_os = "linux")]
static TARGET: std::sync::RwLock<(Option<RenderTarget>, bool)> =
    std::sync::RwLock::new((None, false));

#[cfg(target_os = "linux")]
pub fn configure_target(target: Option<RenderTarget>, force_nv12: bool) {
    *TARGET.write().unwrap() = (target, force_nv12);
    set_rendering(force_nv12 || target.is_some());
}

/// Read at pool creation. No device query or target lock is needed per frame.
#[cfg(target_os = "linux")]
pub(crate) fn supports_pool(format: PixelFormat, device: Option<u64>, modifier: u64) -> bool {
    let (target, force_nv12) = *TARGET.read().unwrap();
    rendering() && accepts_import(target, force_nv12, format, device, modifier)
}

/// Both the decoder pool and the current GL context must admit this layout.
#[cfg(target_os = "linux")]
pub fn accepts_import(
    target: Option<RenderTarget>,
    force_nv12: bool,
    format: PixelFormat,
    device: Option<u64>,
    modifier: u64,
) -> bool {
    (force_nv12 && format == PixelFormat::Nv12)
        || target.is_some_and(|target| target.supports(format, device, modifier))
}

#[cfg(target_os = "linux")]
impl RenderTarget {
    pub fn supports(self, format: PixelFormat, device: Option<u64>, modifier: u64) -> bool {
        if device != Some(self.device) {
            return false;
        }
        let formats = match format {
            PixelFormat::Nv12 => self.nv12,
            PixelFormat::P010 => self.p010,
            _ => return false,
        };
        match modifier {
            0 => formats.linear,
            0x0100_0000_0000_0002 => formats.intel_y,
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
    pub format: PixelFormat,
    pub origin: [u32; 2],
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
    pub fn read_visible(&self, size: [u32; 2]) -> Result<bytes::Bytes, String> {
        crate::hw::vaapi::download::read_export(self, size)
    }

    #[cfg(all(target_os = "linux", feature = "vaapi"))]
    pub fn read_visible_nv12(&self, size: [u32; 2]) -> Result<bytes::Bytes, String> {
        if self.format != PixelFormat::Nv12 {
            return Err("expected an exported NV12 surface".into());
        }
        self.read_visible(size)
    }

    /// Accept only linear or Intel Y NV12/P010 surfaces. Auxiliary
    /// compression planes and unfamiliar modifiers retain CPU readback.
    pub fn valid(&self, visible: [u32; 2]) -> bool {
        let [w, h] = visible;
        if w < 2
            || h < 2
            || w % 2 != 0
            || h % 2 != 0
            || !matches!(self.format, PixelFormat::Nv12 | PixelFormat::P010)
            || self.origin[0] % 2 != 0
            || self.origin[1] % 2 != 0
            || self.origin[0].checked_add(w).is_none_or(|r| r > self.width)
            || self.origin[1]
                .checked_add(h)
                .is_none_or(|b| b > self.height)
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
            if pitch < self.format.row_bytes(self.width) as u64
                || pitch > i32::MAX as u64
                || (self.format == PixelFormat::P010
                    && (pitch % (2 << p) != 0 || self.offset[p] % (2 << p) != 0))
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
            let end = self.offset[p] as u64 + pitch * rows;
            if end > self.allocation_size as u64 || (p == 0 && end > self.offset[1] as u64) {
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
            format: PixelFormat::Nv12,
            origin: [0, 0],
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
    #[test]
    fn ten_bit_layout_checks_each_plane_and_visible_origin() {
        let mut f = fixture();
        f.format = PixelFormat::P010;
        assert!(!f.valid([64, 32]));
        f.pitch = [128, 128];
        f.offset[1] = 4096;
        f.allocation_size *= 2;
        f.origin = [4, 2];
        assert!(f.valid([60, 30]));
        assert!(!f.valid([62, 30]));
        f.origin = [2, u32::MAX - 1];
        assert!(!f.valid([16, 4]));
        f.origin = [3, 2];
        assert!(!f.valid([16, 4]));
        f.origin = [4, 2];
        f.offset[1] -= 4;
        assert!(!f.valid([16, 4]));
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn render_target_checks_the_device_and_exact_plane_modifier() {
        let target = RenderTarget {
            device: 17,
            nv12: Modifiers {
                linear: false,
                intel_y: true,
            },
            p010: Modifiers {
                linear: true,
                intel_y: false,
            },
        };
        assert!(target.supports(PixelFormat::Nv12, Some(17), 0x0100_0000_0000_0002));
        assert!(!target.supports(PixelFormat::Nv12, Some(18), 0x0100_0000_0000_0002));
        assert!(!target.supports(PixelFormat::Nv12, None, 0x0100_0000_0000_0002));
        assert!(!target.supports(PixelFormat::Nv12, Some(17), 0));
        assert!(!target.supports(PixelFormat::Nv12, Some(17), u64::MAX));
        assert!(target.supports(PixelFormat::P010, Some(17), 0));
        assert!(!target.supports(PixelFormat::P010, Some(17), 0x0100_0000_0000_0002));
        assert!(!target.supports(PixelFormat::P010, Some(18), 0));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn forcing_nv12_never_opts_an_unverified_ten_bit_surface_into_import() {
        for force in [false, true] {
            assert_eq!(
                accepts_import(None, force, PixelFormat::Nv12, None, 0),
                force
            );
            assert!(!accepts_import(None, force, PixelFormat::P010, None, 0));
            assert!(!accepts_import(None, force, PixelFormat::Bgra8, None, 0));
            let target = RenderTarget {
                device: 42,
                nv12: Modifiers::default(),
                p010: Modifiers {
                    linear: true,
                    intel_y: false,
                },
            };
            assert!(accepts_import(
                Some(target),
                force,
                PixelFormat::P010,
                Some(42),
                0
            ));
            assert!(!accepts_import(
                Some(target),
                force,
                PixelFormat::P010,
                Some(43),
                0
            ));
            assert!(!accepts_import(
                Some(target),
                force,
                PixelFormat::P010,
                Some(42),
                u64::MAX
            ));
        }
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
