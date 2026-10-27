//! Shared clipboard consent, content and counters.
mod content;
pub use content::{Content,Image,MAX_IMAGE_BYTES,MAX_IMAGE_PIXELS,MAX_IMAGE_DIMENSION};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
};
pub const MAX_TEXT: usize = 30 * 1024;
#[derive(Default)]
pub struct Clipboard {
    /// Transport lease, independent of focus and permission changes.
    pub(crate) transport_epoch:AtomicU64,
    pub available: AtomicBool,
    pub enabled: AtomicBool,
    /// Frontend focus/consent generation, used to reject stale native messages.
    pub generation: AtomicU64,
    pub active: AtomicBool,
    pub read_only: AtomicBool,
    pub stopped: AtomicBool,
    pub supported: AtomicBool,
    pub images_supported: AtomicBool,
    pub local: Mutex<Option<Content>>,
    pub remote: Mutex<Option<Content>>,
    pub last_sent: AtomicU64,
    pub sent: AtomicU64,
    pub received: AtomicU64,
    pub status: Mutex<String>,
    /// Invalidates pending transport assemblies after local content/focus changes.
    pub reset_generation: AtomicU64,
}
impl Clipboard {
    pub fn status(&self, text: impl Into<String>) {
        *self.status.lock().unwrap() = text.into();
    }
    pub fn ready(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
            && self.active.load(Ordering::Acquire)
            && !self.stopped.load(Ordering::Acquire)
    }
}
