//! Converted image ownership across D3D devices, with bounded decoder leases.
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
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
struct Output {
    converter: Converter,
    sync: ExternalSync,
    leases: Leases<Arc<Frame>,Option<super::frame_observation::FrameObservation>>,
    image: super::native_scale::ImageOwner,
    available:bool,
    half:bool,
    consumed: Arc<AtomicU64>,
}
// Created, used and dropped on the owning render thread. Native immediate
// contexts themselves are protected; only immutable handles cross presenters.
unsafe impl Send for Output {}
impl Output {
    pub fn new(frame: &Frame,half:bool) -> Result<Self, Failure> {
        let converter = (if half {Converter::with_external_sdr_half(frame)}else{Converter::with_external_export(frame)}).map_err(|e|Failure::new("converter_create",e))?;
        let sync = converter.external_sync().map_err(|e|Failure::new("external_sync_create",e))?;
        Ok(Self {
            converter,
            sync,
            leases: Leases::new(8),
            image:super::native_scale::ImageOwner::new(),
            available:true,half,
            consumed: Arc::new(AtomicU64::new(0)),
        })
    }
    pub fn matches(&self, frame: &Frame) -> bool {
        self.converter.matches(frame) && self.available
    }
    pub fn set(&mut self, frame: Arc<Frame>, color: YuvColor, observation:Option<super::frame_observation::FrameObservation>) -> Result<Update, Failure> {
        if !self.available {
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
        self.leases.produced(ready,observation,Instant::now());
        Ok(Update::Updated{transition})
    }
    /// Owner-thread housekeeping is independent of incoming frames and paints.
    /// Only producer completion releases decoder samples; ready/observation,
    /// the converted texture and the consumer timeline remain unchanged.
    pub fn poll_completion(&mut self)->Result<Option<std::time::Duration>,Failure> {
        if !self.available{return Ok(None);}
        let now=Instant::now();
        let Some(retry)=self.leases.completion_retry(now) else{return Ok(None);};
        if !retry.is_zero(){return Ok(Some(retry));}
        let complete=self.sync.completed();
        if complete==u64::MAX {
            // Never interpret device loss as completion or keep polling it.
            // Retirement retains the existing synchronization/Drop contract.
            self.available=false;self.image.invalidate();
            return Err(self.failure("producer_completion","device_lost","native source GPU removed".into()));
        }
        self.leases.observe(complete,now);
        Ok(self.leases.completion_retry(now))
    }
    pub fn completion_retry(&self)->Option<std::time::Duration> {
        self.available.then(||self.leases.completion_retry(Instant::now())).flatten()
    }
    fn failure(&self,phase:&'static str,kind:&'static str,reason:String)->Failure {
        Failure{phase,kind,reason,leases:Some(self.leases.snapshot())}
    }
    pub fn diagnostics(&self)->serde_json::Value {
        serde_json::json!({"source_id":self.image.id,"ready_fence":self.leases.published().map(|v|v.0),"consumed_fence":self.consumed.load(Ordering::Acquire),"leases":self.leases.snapshot()})
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
            id: self.image.id,
            handles: self.sync.handles().map(|h| h as usize),
            ready,
            consumed: self.consumed.clone(),
            valid: self.image.valid.clone(),
            rect,
            hdr,
            source_white,
            cursor,
            observation:observation.clone(),
            half_scale:self.half,
        }
    }
    pub fn backend(&self) -> &'static str {
        self.converter.backend_name()
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        self.image.invalidate();
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

struct Experimental {
    other: Option<Output>,
    half_disabled: bool,
}
/// The default owns exactly the original output. The experiment lazily caches
/// its alternate output; geometry changes never synchronously retire a source.
pub(super) struct Source {
    active: Output,
    experiment: Option<Box<Experimental>>,
    reported_image: Option<u64>,
}
impl Source {
    pub fn new(frame: &Frame, half: bool) -> Result<Self, Failure> {
        let experimental = super::native_scale::enabled();
        let half = half && experimental;
        let (active, half_disabled) = match Output::new(frame, half) {
            Ok(output) => (output, false),
            Err(error) if half => {
                eprintln!("experimental R10 output unavailable; using original native output: {error}");
                (Output::new(frame, false)?, true)
            }
            Err(error) => return Err(error),
        };
        Ok(Self {
            active,
            experiment: experimental.then(|| {
                Box::new(Experimental {
                    other: None,
                    half_disabled,
                })
            }),
            reported_image: None,
        })
    }
    pub fn matches(&self, frame: &Frame) -> bool {
        self.active.matches(frame)
    }
    fn target_half(&self, wanted: bool) -> bool {
        wanted
            && self
                .experiment
                .as_ref()
                .is_some_and(|cache| !cache.half_disabled)
    }
    pub fn needs_selection(&self, wanted: bool) -> bool {
        self.active.half != self.target_half(wanted)
    }
    /// A failed presenter import gets one ordinary-output retry, and this
    /// input generation never attempts the unsupported half output again.
    pub fn disable_half(&mut self) -> bool {
        let Some(cache) = self.experiment.as_mut() else {
            return false;
        };
        if !self.active.half || cache.half_disabled {
            return false;
        }
        cache.half_disabled = true;
        true
    }
    pub fn set(
        &mut self,
        frame: Arc<Frame>,
        color: YuvColor,
        observation: Option<super::frame_observation::FrameObservation>,
        wanted_half: bool,
    ) -> Result<Update, Failure> {
        let half = self.target_half(wanted_half);
        if half == self.active.half {
            return self.active.set(frame, color, observation);
        }
        let cache = self
            .experiment
            .as_mut()
            .expect("only the experiment selects an alternate output");
        if cache.other.is_none() {
            match Output::new(&frame, half) {
                Ok(output) => cache.other = Some(output),
                Err(error) if half => {
                    eprintln!("experimental R10 output unavailable; retaining original native output: {error}");
                    cache.half_disabled = true;
                    return self.active.set(frame, color, observation);
                }
                Err(error) => return Err(error),
            }
        }
        let other = cache.other.as_mut().unwrap();
        debug_assert_eq!(other.half, half);
        let result = other.set(frame, color, observation)?;
        if matches!(result, Update::Updated { .. }) {
            // Even a cached resource has a fresh ready value and publication
            // id on re-entry. Retire late offers without dropping GPU leases.
            self.active.image.invalidate();
            other.image.renew();
            std::mem::swap(&mut self.active, other);
        }
        Ok(result)
    }
    pub fn poll_completion(&mut self) -> Result<Option<std::time::Duration>, Failure> {
        let first = self.active.poll_completion()?;
        let second = if let Some(other) = self
            .experiment
            .as_mut()
            .and_then(|cache| cache.other.as_mut())
        {
            match other.poll_completion() {
                Ok(retry) => retry,
                Err(error) => {
                    self.active.available = false;
                    self.active.image.invalidate();
                    return Err(error);
                }
            }
        } else {
            None
        };
        Ok(match (first, second) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        })
    }
    pub fn completion_retry(&self) -> Option<std::time::Duration> {
        let first = self.active.completion_retry();
        let second = self
            .experiment
            .as_ref()
            .and_then(|cache| cache.other.as_ref())
            .and_then(Output::completion_retry);
        match (first, second) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
    pub fn image(
        &self,
        rect: [f32; 4],
        hdr: bool,
        white: f32,
        cursor: Option<uu_core::native_cursor::Snapshot>,
    ) -> super::NativeImage {
        self.active.image(rect, hdr, white, cursor)
    }
    pub fn backend(&self) -> &'static str {
        self.active.backend()
    }
    pub fn diagnostics(&self) -> serde_json::Value {
        let mut value = self.active.diagnostics();
        value["backend"] = self.backend().into();
        value["output_size"] = serde_json::json!(self.active.converter.output_size());
        value["half_scale"] = self.active.half.into();
        value
    }
    pub fn take_selection_event(&mut self, source_size: [u32; 2]) -> Option<serde_json::Value> {
        if self.experiment.is_none()
            || self.active.leases.published().is_none()
            || self.reported_image == Some(self.active.image.id)
        {
            return None;
        }
        self.reported_image = Some(self.active.image.id);
        Some(
            serde_json::json!({"event":"native_video_conversion","backend":self.backend(),"source_size":source_size,
            "output_size":self.active.converter.output_size(),"half_scale":self.active.half,"source_id":self.active.image.id}),
        )
    }
}
