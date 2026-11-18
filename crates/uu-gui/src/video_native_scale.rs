//! Opt-in exact-ratio selection and publication identity for native SDR output.
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, OnceLock,
};

pub(super) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED
        .get_or_init(|| std::env::var("ECLIPSE_EXPERIMENT_SDR_HALF_SCALE").as_deref() == Ok("1"))
}

/// Rect is the actual native compositor's physical-pixel video rectangle,
/// including its origin. Fractional origins change the original filter phase.
pub(super) fn select(enabled: bool, nv12: bool, source: [u32; 2], rect: [f32; 4]) -> bool {
    enabled
        && nv12
        && rect.iter().all(|v| v.is_finite() && v.fract() == 0.)
        && rect[2] > 0.
        && rect[3] > 0.
        && source[0] as f64 == 2. * f64::from(rect[2])
        && source[1] as f64 == 2. * f64::from(rect[3])
}

/// A cached GPU resource is not a reusable publication identity. Each return
/// to that output retires late offers and publishes a new id/valid token.
pub(super) struct ImageOwner {
    pub id: u64,
    pub valid: Arc<AtomicBool>,
}
impl ImageOwner {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            valid: Arc::new(AtomicBool::new(true)),
        }
    }
    pub fn invalidate(&self) {
        self.valid.store(false, Ordering::Release);
    }
    pub fn renew(&mut self) {
        self.invalidate();
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_physical_ratio_requires_opt_in_nv12_and_aligned_origin() {
        let source = [3840, 2160];
        let rect = [0., 60., 1920., 1080.];
        assert!(select(true, true, source, rect));
        assert!(select(true, true, source, [-20., 60., 1920., 1080.]));
        assert!(!select(false, true, source, rect));
        assert!(!select(true, false, source, rect));
        for rect in [
            [0.5, 60., 1920., 1080.],
            [0., 60., 1921., 1080.],
            [0., 60., 1920., 1079.5],
            [0., 60., 0., 0.],
            [f32::NAN, 60., 1920., 1080.],
            [0., f32::INFINITY, 1920., 1080.],
        ] {
            assert!(!select(true, true, source, rect));
        }
    }
    #[test]
    fn returning_to_cached_output_cannot_reuse_an_old_offer() {
        let mut resource = ImageOwner::new();
        let first = (resource.id, resource.valid.clone());
        resource.invalidate();
        assert!(!first.1.load(Ordering::Acquire));
        resource.renew();
        let second = (resource.id, resource.valid.clone());
        assert_ne!(first.0, second.0);
        assert!(!first.1.load(Ordering::Acquire));
        assert!(second.1.load(Ordering::Acquire));
        resource.renew();
        assert!(!second.1.load(Ordering::Acquire));
        assert_ne!(second.0, resource.id);
    }
}
