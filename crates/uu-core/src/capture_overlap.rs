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
/// Must precede encoder initialization: the encoder lease is the encode device.
pub fn prepare_device(capture:&mut dyn Capture,overlap:bool,separate:bool)->Result<()>{
    if separate {anyhow::ensure!(overlap,"separate encode device requires capture/encode overlap");capture.enable_separate_encode_device()?;}Ok(())
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Config {
    fps: u32,
    gpu: bool,
    vendor: Option<u16>,
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
        let config = Config { fps, gpu, vendor };
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
            println!(
                "{}",
                serde_json::json!({"event":"capture_overlap","action":"start","requested":true,"actual":true,"pending_limit":1,"capture_pool_limit":3,"owner_thread":owner.summary().owner_thread,"owner_native_thread":owner.summary().owner_native_thread,"generation":owner.summary().generation})
            );
            Mode::Overlap {
                owner,
                device,
                last: None,
                epoch: None,
            }
        } else {
            println!(
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
    pub fn encoder_input_device(&self) -> Option<InputDevice> {
        match &self.mode {
            Mode::Serial(c) => c.encoder_input_device(),
            Mode::Overlap { device, .. } => device.clone(),
        }
    }
    pub fn set_frame_rate(&mut self, fps: u32) -> Result<()> {
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
                let fresh = match owner.latest(wait) {
                    Ok(f) => f,
                    Err(latest_capture::Failure::Paused(e)) => {
                        *last = None;
                        return Err(CaptureError::Suspended(e));
                    }
                    Err(latest_capture::Failure::Failed(e)) => {
                        *last = None;
                        return Err(CaptureError::Backend(e));
                    }
                };
                let mut repeated = fresh.is_none();
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
                    repeated = self.last_source == Some(id);
                    self.last_source = Some(id);
                }
                // Repeating at output cadence is explicit, never a capture-owner
                // cache publication. Observation timestamps remain unchanged.
                if repeated {
                    self.repeats += 1;
                    frame.pts.0 = frame.pts.0.saturating_add(
                        image.ready.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
                    );
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
            println!(
                "{}",
                serde_json::json!({"event":"capture_overlap","action":"stop","actual":true,"pending_limit":1,"capture_pool_limit":3,"produced":s.produced,"replaced":s.replaced,"pauses":s.pauses,"configurations":s.configurations,"owner_thread":s.owner_thread,"owner_native_thread":s.owner_native_thread,"generation":s.generation,"phase":s.phase,"joined":s.joined,"repeated_outputs":self.repeats,"retired_outputs":self.retired_outputs,"error":stopped.as_ref().err()})
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

#[cfg(test)] mod separate_flag_tests {use super::*;
#[test] fn strict_default_off_and_overlap_required(){assert!(!separate_requested(None).unwrap());assert!(!separate_requested(Some("0")).unwrap());assert!(separate_requested(Some("1")).unwrap());for v in ["true","", "2"," 1"]{assert!(separate_requested(Some(v)).is_err());}}
}
