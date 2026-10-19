//! Features actually supported by a live session, independent of its transport.
use std::sync::atomic::{AtomicU32, Ordering};
#[derive(Clone, Copy)]
#[repr(u32)]
pub enum Capability {
    Input = 1,
    Audio = 2,
    Clipboard = 4,
    Quality = 8,
    Displays = 16,
}
#[derive(Default)]
pub struct Capabilities(AtomicU32);
impl Capabilities {
    pub const ALL: u32 = 31;
    pub fn allows(&self, capability: Capability) -> bool {
        self.0.load(Ordering::Acquire) & capability as u32 != 0
    }
    pub fn set(&self, flags: u32) {
        self.0.store(flags & Self::ALL, Ordering::Release);
    }
    pub fn enable(&self, capability: Capability, enabled: bool) {
        if enabled {self.0.fetch_or(capability as u32,Ordering::AcqRel);}
        else {self.0.fetch_and(!(capability as u32),Ordering::AcqRel);}
    }
}
