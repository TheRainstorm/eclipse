//! Converted image ownership across D3D devices, with bounded decoder leases.
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};
use uu_core::display_frames::{
    d3d11_frame::{Converter, ExternalSync, Frame},
    YuvColor,
};
static NEXT: AtomicU64 = AtomicU64::new(1);
pub(super) struct Source {
    converter: Converter,
    sync: ExternalSync,
    leases: VecDeque<(u64, Arc<Frame>)>,
    id: u64,
    ready: u64,
    consumed: Arc<AtomicU64>,
    valid: Arc<AtomicBool>,
}
// Created, used and dropped on the owning render thread. Native immediate
// contexts themselves are protected; only immutable handles cross presenters.
unsafe impl Send for Source {}
impl Source {
    pub fn new(frame: &Frame) -> Result<Self, String> {
        let converter = Converter::with_external_export(frame)?;
        let sync = converter.external_sync()?;
        Ok(Self {
            converter,
            sync,
            leases: VecDeque::new(),
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            ready: 0,
            consumed: Arc::new(AtomicU64::new(0)),
            valid: Arc::new(AtomicBool::new(true)),
        })
    }
    pub fn matches(&self, frame: &Frame) -> bool {
        self.converter.matches(frame) && self.valid.load(Ordering::Acquire)
    }
    pub fn set(&mut self, frame: Arc<Frame>, color: YuvColor) -> Result<(), String> {
        if !self.valid.load(Ordering::Acquire) {
            return Err("native source ownership is retired".into());
        }
        let complete = self.sync.completed();
        if complete == u64::MAX {
            return Err("native source GPU removed".into());
        }
        while self.leases.front().is_some_and(|(v, _)| *v <= complete) {
            self.leases.pop_front();
        }
        if self.leases.len() >= 8 {
            return Err("native decoder lease bound reached".into());
        }
        self.sync
            .before_write(self.consumed.load(Ordering::Acquire))?;
        self.leases.push_back((u64::MAX, frame.clone()));
        self.converter.convert(&frame, color)?;
        self.ready = self.sync.ready()?;
        self.leases.back_mut().unwrap().0 = self.ready;
        Ok(())
    }
    pub fn image(
        &self,
        rect: [f32; 4],
        hdr: bool,
        source_white: f32,
        cursor: Option<uu_core::native_cursor::Snapshot>,
    ) -> super::NativeImage {
        super::NativeImage {
            id: self.id,
            handles: self.sync.handles().map(|h| h as usize),
            ready: self.ready,
            consumed: self.consumed.clone(),
            valid: self.valid.clone(),
            rect,
            hdr,
            source_white,
            cursor,
        }
    }
    pub fn backend(&self) -> &'static str {
        self.converter.backend_name()
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        self.valid.store(false, Ordering::Release);
        let result = self
            .sync
            .before_write(self.consumed.load(Ordering::Acquire))
            .and_then(|_| self.sync.synchronize());
        if let Err(e) = result {
            eprintln!("native image retirement failed: {e}");
        }
        self.leases.clear();
    }
}
