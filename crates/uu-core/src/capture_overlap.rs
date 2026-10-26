//! Default-off DXGI capture ownership. Encoding/AMF remains on its driver thread.
use crate::latest_capture::{self, Owner, Packet};
use anyhow::Result;
use std::time::{Duration, Instant};
use uu_capture::{frame_info::FrameInfo, Capture, CaptureError};
use uu_media::{encoder::InputDevice, RawFrame};

pub fn requested(value: Option<&str>) -> Result<bool> {
    match value {
        None | Some("0") => Ok(false),
        Some("1") => Ok(true),
        _ => anyhow::bail!("UU_CAPTURE_ENCODE_OVERLAP must be exactly 0 or 1"),
    }
}
pub(crate) fn requested_from_env() -> Result<bool> {
    match std::env::var("UU_CAPTURE_ENCODE_OVERLAP") {
        Ok(v) => requested(Some(&v)),
        Err(std::env::VarError::NotPresent) => Ok(false),
        Err(e) => Err(e.into()),
    }
}
pub fn separate_requested(value:Option<&str>)->Result<bool>{match value{None|Some("0")=>Ok(false),Some("1")=>Ok(true),_=>anyhow::bail!("UU_CAPTURE_ENCODE_SEPARATE_DEVICE must be exactly 0 or 1")}}
pub(crate) fn separate_from_env()->Result<bool>{match std::env::var("UU_CAPTURE_ENCODE_SEPARATE_DEVICE"){Ok(v)=>separate_requested(Some(&v)),Err(std::env::VarError::NotPresent)=>Ok(false),Err(e)=>Err(e.into())}}
pub fn owner_pacing_requested(value:Option<&str>)->Result<bool> {
    match value { None|Some("0")=>Ok(false),Some("1")=>Ok(true),
        _=>anyhow::bail!("UU_CAPTURE_OWNER_PACING must be exactly 0 or 1") }
}
fn pacing_from_env()->Result<bool> {
    match std::env::var("UU_CAPTURE_OWNER_PACING") { Ok(v)=>owner_pacing_requested(Some(&v)),
        Err(std::env::VarError::NotPresent)=>Ok(false),Err(e)=>Err(e.into()) }
}
pub fn validate_owner_pacing(paced:bool,overlap:bool,separate:bool)->Result<()> {
    anyhow::ensure!(!paced || (overlap && separate),"capture owner pacing requires overlap and separate encode device");Ok(())
}
/// Must precede encoder initialization: the encoder lease is the encode device.
pub fn prepare_device(capture:&mut dyn Capture,overlap:bool,separate:bool)->Result<()>{
    validate_owner_pacing(pacing_from_env()?,overlap,separate)?;
    if separate {anyhow::ensure!(overlap,"separate encode device requires capture/encode overlap");capture.enable_separate_encode_device()?;}Ok(())
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Config {
    fps: u32,
    gpu: bool,
    vendor: Option<u16>,
    owner_pacing: bool,
}
#[derive(Clone)]
struct Image {
    frame: RawFrame,
    source: Option<FrameInfo>,
    started: Instant,
    ready: Instant,
}
struct Desktop {
    capture: Box<dyn Capture>,
    last: Option<Image>,
}
impl latest_capture::Source for Desktop {
    type Frame = Image;
    type Config = Config;
    fn cadence(c:&Config)->Option<Duration> {
        c.owner_pacing.then(||Duration::from_secs_f64(1.0/f64::from(c.fps)))
    }
    fn configure(&mut self, c: Config) -> std::result::Result<Option<Image>, String> {
        self.capture.prefer_gpu(c.gpu, c.vendor);
        self.capture.enable_overlap().map_err(|e| e.to_string())?;
        self.capture
            .set_frame_rate(c.fps)
            .map_err(|e| e.to_string())?;
        // Re-check the actual input desktop on its owner before acknowledging
        // a new generation. Static desktops may yield an explicit duplicate;
        // retain its original observation times rather than relabel dequeue.
        let started = Instant::now();
        let frame = match self.capture.next_frame() {
            Ok(frame) => frame,
            Err(CaptureError::Suspended(_)) => {
                self.last = None;
                return Ok(None);
            }
            Err(error) => return Err(error.to_string()),
        };
        let ready = Instant::now();
        let source = self.capture.frame_info();
        let same = source
            .zip(self.last.as_ref().and_then(|v| v.source))
            .is_some_and(|(a, b)| a.generation == b.generation && a.frame_id == b.frame_id);
        let (started, ready) = if same {
            let previous = self.last.as_ref().unwrap();
            (previous.started, previous.ready)
        } else {
            (started, ready)
        };
        let image = Image {
            frame,
            source,
            started,
            ready,
        };
        self.last = Some(image.clone());
        Ok(Some(image))
    }
    fn next(
        &mut self,
        wait: Duration,
    ) -> std::result::Result<Option<Image>, latest_capture::Failure> {
        let started = Instant::now();
        match self.capture.wait_updated_frame(wait) {
            Ok(Some(frame)) => {
                let ready = Instant::now();
                let source = self.capture.frame_info();
                let image = Image {
                    frame,
                    source,
                    started,
                    ready,
                };
                self.last = Some(image.clone());
                Ok(Some(image))
            }
            Ok(None) => Ok(None),
            Err(CaptureError::Suspended(error)) => {
                self.last = None;
                Err(latest_capture::Failure::Paused(error))
            }
            Err(error) => Err(latest_capture::Failure::Failed(error.to_string())),
        }
    }
}
pub struct Observed {
    pub frame: RawFrame,
    pub source: Option<FrameInfo>,
    pub started: Instant,
    pub ready: Instant,
    pub retired: bool,
    pub(crate) lease: Option<latest_capture::Lease>,
}
impl Observed {
    pub fn current(&self) -> bool {
        self.lease.as_ref().is_none_or(|v| v.valid())
    }
}
enum Mode {
    Serial(Box<dyn Capture>),
    Overlap {
        owner: Owner<Desktop>,
        device: Option<InputDevice>,
        last: Option<Packet<Image>>,
        epoch: Option<u64>,
    },
}
pub struct Input {
    mode: Mode,
    config: Config,
    last_pts: Option<u64>,
    last_source: Option<(u64, u64)>,
    repeats: u64,
    retired_outputs: u64,
    finished: bool,
}
impl Input {
    pub fn new(
        capture: Box<dyn Capture>,
        enabled: bool,
        fps: u32,
        gpu: bool,
        vendor: Option<u16>,
    ) -> Result<Self> {
        let owner_pacing=pacing_from_env()?;
        validate_owner_pacing(owner_pacing,enabled,separate_from_env()?)?;
        anyhow::ensure!((1..=240).contains(&fps),"invalid capture frame rate");
        let config = Config { fps, gpu, vendor, owner_pacing };
        let mode = if enabled {
            let device = capture.encoder_input_device();
            let owner = Owner::start(
                Desktop {
                    capture,
                    last: None,
                },
                config,
            )
            .map_err(anyhow::Error::msg)?;
            eprintln!(
                "{}",
                serde_json::json!({"event":"capture_overlap","action":"start","requested":true,"actual":true,"pending_limit":1,"capture_pool_limit":3,"owner_thread":owner.summary().owner_thread,"owner_native_thread":owner.summary().owner_native_thread,"generation":owner.summary().generation,"owner_pacing":owner_pacing,"target_fps":fps})
            );
            Mode::Overlap {
                owner,
                device,
                last: None,
                epoch: None,
            }
        } else {
            eprintln!(
                "{}",
                serde_json::json!({"event":"capture_overlap","action":"policy","requested":false,"actual":false})
            );
            Mode::Serial(capture)
        };
        Ok(Self {
            mode,
            config,
            last_pts: None,
            last_source: None,
            repeats: 0,
            retired_outputs: 0,
            finished: false,
        })
    }
    pub fn owner_pacing(&self)->bool { self.config.owner_pacing }
    pub fn interrupt_handle(&self)->Option<std::sync::Arc<dyn Fn()+Send+Sync>> {
        match &self.mode { Mode::Overlap{owner,..} if self.config.owner_pacing=>Some(owner.interrupt_handle()),_=>None }
    }
    pub fn encoder_input_device(&self) -> Option<InputDevice> {
        match &self.mode {
            Mode::Serial(c) => c.encoder_input_device(),
            Mode::Overlap { device, .. } => device.clone(),
        }
    }
    pub fn set_frame_rate(&mut self, fps: u32) -> Result<()> {
        anyhow::ensure!((1..=240).contains(&fps),"invalid capture frame rate");
        match &mut self.mode {
            Mode::Serial(c) => c.set_frame_rate(fps)?,
            Mode::Overlap {
                owner, last, epoch, ..
            } => {
                owner
                    .configure(Config { fps, ..self.config })
                    .map_err(anyhow::Error::msg)?;
                *last = None;
                *epoch = None;
            }
        }
        self.config.fps = fps;
        if self.config.owner_pacing {
            eprintln!("{}",serde_json::json!({"event":"capture_owner_pacing","action":"configure","actual":true,"target_fps":fps}));
        }
        Ok(())
    }
    pub fn prefer_gpu(&mut self, gpu: bool, vendor: Option<u16>) -> Result<()> {
        match &mut self.mode {
            Mode::Serial(c) => c.prefer_gpu(gpu, vendor),
            Mode::Overlap {
                owner, last, epoch, ..
            } if (gpu, vendor) != (self.config.gpu, self.config.vendor) => {
                owner
                    .configure(Config {
                        gpu,
                        vendor,
                        ..self.config
                    })
                    .map_err(anyhow::Error::msg)?;
                *last = None;
                *epoch = None;
            }
            _ => {}
        }
        self.config.gpu = gpu;
        self.config.vendor = vendor;
        Ok(())
    }
    pub fn next(&mut self, metadata: bool) -> std::result::Result<Observed, CaptureError> {
        match &mut self.mode {
            Mode::Serial(capture) => {
                let started = Instant::now();
                let frame = capture.next_frame()?;
                let ready = Instant::now();
                let source = if metadata { capture.frame_info() } else { None };
                Ok(Observed {
                    frame,
                    source,
                    started,
                    ready,
                    retired: false,
                    lease: None,
                })
            }
            Mode::Overlap {
                owner, last, epoch, ..
            } => {
                let wait = if last.is_some() {
                    Duration::ZERO
                } else {
                    Duration::from_millis(250)
                };
                let delivery = if self.config.owner_pacing { owner.next_event(Duration::from_millis(250)) }
                    else { owner.latest(wait).map(|v|v.map(latest_capture::Delivery::Fresh)) };
                let (fresh, repeat_allowed) = match delivery {
                    Ok(Some(latest_capture::Delivery::Fresh(packet)))=>(Some(packet),false),
                    Ok(Some(latest_capture::Delivery::Repeat))=>(None,true),
                    Ok(Some(latest_capture::Delivery::NoFrame))|Ok(None) if self.config.owner_pacing=>{
                        return Err(CaptureError::Suspended("等待采集周期的新图像".into()));
                    },
                    Ok(None)|Ok(Some(latest_capture::Delivery::NoFrame))=>(None,true),
                    Err(latest_capture::Failure::Paused(e)) => {
                        *last = None;
                        return Err(CaptureError::Suspended(e));
                    }
                    Err(latest_capture::Failure::Failed(e)) => {
                        *last = None;
                        return Err(CaptureError::Backend(e));
                    }
                };
                let mut repeated = fresh.is_none() && repeat_allowed;
                if let Some(image) = fresh {
                    *last = Some(image);
                }
                let packet = last
                    .as_ref()
                    .filter(|p| p.lease.valid())
                    .ok_or_else(|| CaptureError::Suspended("等待当前桌面首帧".into()))?;
                let current = packet.lease.generation();
                let retired = epoch.is_some_and(|old| old != current);
                *epoch = Some(current);
                let image = &packet.frame;
                let mut frame = image.frame.clone();
                let mut source = image.source;
                if let Some(info) = source {
                    let id = (info.generation, info.frame_id);
                    repeated = self.last_source == Some(id) || (self.config.owner_pacing && (info.duplicate || repeat_allowed));
                    self.last_source = Some(id);
                }
                // Repeating at output cadence is explicit, never a capture-owner
                // cache publication. Observation timestamps remain unchanged.
                if repeated {
                    self.repeats += 1;
                    frame.pts.0 = repeat_pts(frame.pts.0,image.ready.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,self.last_pts);
                }
                if let Some(info) = &mut source {
                    info.duplicate = repeated;
                    if repeated {
                        info.accumulated_frames = 0;
                    }
                }
                if let Some(previous) = self.last_pts {
                    frame.pts.0 = frame.pts.0.max(previous.saturating_add(1));
                }
                self.last_pts = Some(frame.pts.0);
                Ok(Observed {
                    frame,
                    source,
                    started: image.started,
                    ready: image.ready,
                    retired,
                    lease: Some(packet.lease.clone()),
                })
            }
        }
    }
    pub fn retired_output(&mut self) {
        self.retired_outputs += 1;
    }
    pub fn finish(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        if let Mode::Overlap { owner, last, .. } = &mut self.mode {
            *last = None;
            let stopped = owner.stop();
            let s = owner.summary();
            eprintln!(
                "{}",
                serde_json::json!({"event":"capture_overlap","action":"stop","actual":true,"pending_limit":1,"capture_pool_limit":3,"produced":s.produced,"replaced":s.replaced,"pauses":s.pauses,"configurations":s.configurations,"owner_thread":s.owner_thread,"owner_native_thread":s.owner_native_thread,"generation":s.generation,"phase":s.phase,"joined":s.joined,"repeated_outputs":self.repeats,"retired_outputs":self.retired_outputs,"error":stopped.as_ref().err(),"owner_pacing":s.owner_pacing,"target_fps":self.config.fps,"ticks":s.ticks,"repeat_opportunities":s.repeat_opportunities,"no_frame_opportunities":s.no_frame_opportunities,"skipped_periods":s.skipped_periods})
            );
            stopped.map_err(anyhow::Error::msg)?;
        }
        Ok(())
    }
}
impl Drop for Input {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

fn repeat_pts(base:u64,elapsed:u64,previous:Option<u64>)->u64 {
    let pts=base.saturating_add(elapsed);
    previous.map_or(pts,|previous|pts.max(previous.saturating_add(1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn option_is_strict_and_default_off() {
        assert!(!requested(None).unwrap());
        assert!(!requested(Some("0")).unwrap());
        assert!(requested(Some("1")).unwrap());
        for value in ["true", "false", "", " 1", "01", "2"] {
            assert!(requested(Some(value)).is_err());
        }
    }
    #[test]
    fn unsupported_capture_is_rejected_without_silent_serial_fallback() {
        let capture = Box::new(uu_capture::SyntheticCapture::new(64, 64, 60));
        assert!(Input::new(capture, true, 60, false, None).is_err());
    }
}

#[cfg(test)]
mod separate_flag_tests {
    use super::*;
    #[test]
    fn strict_default_off_and_overlap_required() {
        assert!(!separate_requested(None).unwrap());assert!(!separate_requested(Some("0")).unwrap());assert!(separate_requested(Some("1")).unwrap());
        for v in ["true","", "2"," 1"]{assert!(separate_requested(Some(v)).is_err());}
        let mut capture=uu_capture::SyntheticCapture::new(64,64,60);
        assert!(prepare_device(&mut capture,false,true).unwrap_err().to_string().contains("requires capture/encode overlap"));
    }
    #[test]
    fn production_prepare_rejects_unsupported_requested_backend() {
        let mut capture=uu_capture::SyntheticCapture::new(64,64,60);
        assert!(prepare_device(&mut capture,false,false).is_ok());
        assert!(prepare_device(&mut capture,true,false).is_ok());
        assert!(prepare_device(&mut capture,true,true).is_err());
    }
}

#[cfg(test)]
mod owner_pacing_tests {
    use super::*;
    #[test]
    fn strict_default_off_only_supported_with_overlap_and_separate() {
        assert!(!owner_pacing_requested(None).unwrap());assert!(!owner_pacing_requested(Some("0")).unwrap());assert!(owner_pacing_requested(Some("1")).unwrap());
        for v in ["", "true", "false", "01", " 1", "2"] {assert!(owner_pacing_requested(Some(v)).is_err());}
        for (o,s) in [(false,false),(true,false),(false,true)] {assert!(validate_owner_pacing(true,o,s).is_err());}
        assert!(validate_owner_pacing(true,true,true).is_ok());assert!(validate_owner_pacing(false,false,false).is_ok());
    }
    #[test]
    fn static_repeat_pts_remain_monotonic_without_changing_capture_identity() {
        let mut previous=None;
        for elapsed in [0,16_667,16_667,33_333,33_332] {
            let pts=repeat_pts(1000,elapsed,previous);assert!(previous.is_none_or(|p|pts>p));previous=Some(pts);
        }
        assert_eq!(repeat_pts(u64::MAX,1,None),u64::MAX);
    }
}
