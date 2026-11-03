//! Converted image ownership across D3D devices, with bounded decoder leases.
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};
use uu_core::display_frames::{
    d3d11_frame::{Converter, ExternalSync, Frame},
    YuvColor,
};
use super::native_capacity::{Admission,Leases,Snapshot,Transition};
#[derive(Clone,Copy,Debug,serde::Serialize)]
#[serde(rename_all="snake_case")]
pub(super) enum DeferredReason { Capacity }
pub(super) enum Update {
    Updated{transition:Option<Transition>},
    Deferred{reason:DeferredReason,retry_after:std::time::Duration,transition:Option<Transition>},
}
#[derive(Debug)]
pub(super) struct Failure {
    pub phase:&'static str,
    pub kind:&'static str,
    pub reason:String,
    pub leases:Option<Snapshot>,
}
impl std::fmt::Display for Failure {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{write!(f,"{}: {}",self.phase,self.reason)}
}
impl Failure {
    fn new(phase:&'static str,reason:String)->Self{Self{phase,kind:"backend_error",reason,leases:None}}
}
static NEXT: AtomicU64 = AtomicU64::new(1);
pub(super) struct Source {
    converter: Converter,
    sync: ExternalSync,
    leases: Leases<Arc<Frame>,Option<super::frame_observation::FrameObservation>>,
    id: u64,
    consumed: Arc<AtomicU64>,
    valid: Arc<AtomicBool>,
}
// Created, used and dropped on the owning render thread. Native immediate
// contexts themselves are protected; only immutable handles cross presenters.
unsafe impl Send for Source {}
impl Source {
    pub fn new(frame: &Frame) -> Result<Self, Failure> {
        let converter = Converter::with_external_export(frame).map_err(|e|Failure::new("converter_create",e))?;
        let sync = converter.external_sync().map_err(|e|Failure::new("external_sync_create",e))?;
        Ok(Self {
            converter,
            sync,
            leases: Leases::new(8),
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            consumed: Arc::new(AtomicU64::new(0)),
            valid: Arc::new(AtomicBool::new(true)),
        })
    }
    pub fn matches(&self, frame: &Frame) -> bool {
        self.converter.matches(frame) && self.valid.load(Ordering::Acquire)
    }
    pub fn set(&mut self, frame: Arc<Frame>, color: YuvColor, observation:Option<super::frame_observation::FrameObservation>) -> Result<Update, Failure> {
        if !self.valid.load(Ordering::Acquire) {
            return Err(self.failure("source_owner","retired_generation","native source ownership is retired".into()));
        }
        let now=Instant::now();
        if let Some(Admission::Deferred{retry_after,transition})=self.leases.pending(now){return Ok(Update::Deferred{reason:DeferredReason::Capacity,retry_after,transition});}
        let complete = self.sync.completed();
        if complete == u64::MAX {
            return Err(self.failure("producer_fence","device_lost","native source GPU removed".into()));
        }
        let transition=match self.leases.observe(complete,now) {
            Admission::Ready{transition}=>transition,
            Admission::Deferred{retry_after,transition}=>return Ok(Update::Deferred{reason:DeferredReason::Capacity,retry_after,transition}),
        };
        // Busy leaves the last image identity and GPU timeline untouched.
        self.sync
            .before_write(self.consumed.load(Ordering::Acquire)).map_err(|e|self.failure("reader_wait_submit","backend_error",e))?;
        self.leases.reserve(frame.clone());
        self.converter.convert(&frame, color).map_err(|e|self.failure("convert_submit","backend_error",e))?;
        let ready = self.sync.ready().map_err(|e|self.failure("producer_signal","backend_error",e))?;
        self.leases.produced(ready,observation);
        Ok(Update::Updated{transition})
    }
    fn failure(&self,phase:&'static str,kind:&'static str,reason:String)->Failure {
        Failure{phase,kind,reason,leases:Some(self.leases.snapshot())}
    }
    pub fn diagnostics(&self)->serde_json::Value {
        serde_json::json!({"source_id":self.id,"ready_fence":self.leases.published().map(|v|v.0),"consumed_fence":self.consumed.load(Ordering::Acquire),"leases":self.leases.snapshot()})
    }
    pub fn image(
        &self,
        rect: [f32; 4],
        hdr: bool,
        source_white: f32,
        cursor: Option<uu_core::native_cursor::Snapshot>,
    ) -> super::NativeImage {
        let (ready,observation)=self.leases.published().expect("native image needs an admitted conversion");
        super::NativeImage {
            id: self.id,
            handles: self.sync.handles().map(|h| h as usize),
            ready,
            consumed: self.consumed.clone(),
            valid: self.valid.clone(),
            rect,
            hdr,
            source_white,
            cursor,
            observation:observation.clone(),
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
