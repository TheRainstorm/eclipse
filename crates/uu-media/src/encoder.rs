//! Native-host encoding selection. Interoperability decoding is independent.
use uu_proto::media::VideoCodec;
use crate::{codec::CodecError, EncodedFrame, H264Encoder, RawFrame, VideoEncoder};

#[cfg(all(windows, feature = "mf-encode"))]
mod mf;
#[cfg(all(windows, feature = "mf-encode"))]
mod gpu;
#[cfg(all(windows, feature = "mf-encode"))]
mod amf;
#[cfg(any(test,feature="intel-vpl",all(windows,feature="mf-encode")))]
mod low_delay;
mod upload;
mod profile;
mod amf_workload;
#[cfg(any(windows,test))]
mod input_pool;
mod tuning;
pub use tuning::{AmdQuality,LowPower,Tuning};
#[cfg(all(target_os = "linux", feature = "vaapi"))]
mod vaapi;
mod worker;
#[cfg(any(test,feature="intel-vpl",all(windows,feature="mf-encode")))]
mod headers;
#[cfg(all(feature="intel-vpl",any(windows,target_os="linux")))]
mod vpl;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Preference {
    #[default]
    Software,
    Auto,
    /// Require the current platform's hardware encoder, without software fallback.
    Hardware,
    Intel,
    Nvidia,
    Amd,
    Vaapi,
}

impl Preference {
    pub fn vendor_id(self) -> Option<u16> {
        match self { Self::Intel=>Some(0x8086),Self::Nvidia=>Some(0x10de),Self::Amd=>Some(0x1002),_=>None }
    }
}

#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub enum RateControl {
    #[default]
    Cbr,
    Vbr { peak_kbps:u32 },
}
impl RateControl {
    pub fn peak(self,target:u32)->u32 {match self {Self::Cbr=>target,Self::Vbr{peak_kbps}=>peak_kbps}}
}
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub tuning: Tuning,
    /// HDR10 PQ/BT.2020 output requires 10-bit HEVC or AV1 and GPU capture.
    pub hdr: bool,
    /// Actual source desktop metadata; needed when mapping HDR into SDR.
    pub source_hdr: Option<crate::hdr::Metadata>,
    pub codec:VideoCodec,
    pub rate_control:RateControl,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub preference: Preference,
}
impl Settings {
    pub fn validate(self) -> Result<(), CodecError> {
        self.tuning.validate()?;
        if self.source_hdr.is_some_and(|m|!m.valid()) {return Err(error("invalid source HDR metadata"));}
        if self.hdr && self.codec==VideoCodec::H264 {return Err(error("HDR requires 10-bit HEVC or AV1"));}
        if !(2..=8192).contains(&self.width)
            || !(2..=8192).contains(&self.height)
            || self.width % 2 != 0
            || self.height % 2 != 0
            || u64::from(self.width)*u64::from(self.height)*3/2>64*1024*1024
            || !(1..=240).contains(&self.fps)
            || !(64..=1_000_000).contains(&self.bitrate_kbps)
        {
            return Err(error(
                "encoding needs even 2..=8192 dimensions, valid FPS and 64..=1000000 kbps",
            ));
        }
        let peak=self.rate_control.peak(self.bitrate_kbps);
        if peak<self.bitrate_kbps || peak>2_000_000{return Err(error("VBR peak must be at least target bitrate and at most 2000000 kbps"));}
        let blocks = u64::from(self.width.div_ceil(16)) * u64::from(self.height.div_ceil(16));
        if self.codec==VideoCodec::H264 && (blocks > 36_864 || blocks * u64::from(self.fps) > 2_073_600) {
            return Err(error(
                "H.264 encoding geometry exceeds the advertised level 5.2",
            ));
        }
        Ok(())
    }
}

fn error(message: impl Into<String>) -> CodecError {
    CodecError::Backend(message.into())
}

fn software(settings: Settings) -> Result<Box<dyn VideoEncoder>, CodecError> {
    if settings.codec!=VideoCodec::H264 || settings.rate_control!=RateControl::Cbr {
        return Err(error("所选编码格式或 VBR 模式需要硬件编码；软件模式支持 H.264 平均码率控制"));
    }
    if settings.hdr {return Err(error("HDR requires a 10-bit hardware encoder"));}
    H264Encoder::validate_dimensions(settings.width, settings.height)?;
    Ok(Box::new(H264Encoder::with_frame_rate(
        settings.bitrate_kbps,
        settings.fps,
    )?))
}
/// Whether the binary includes its platform's hardware encoding API.
pub fn hardware_compiled() -> bool {
    cfg!(any(
        all(target_os = "linux", feature = "vaapi"),
        all(windows, feature = "mf-encode"),
        all(feature="intel-vpl",any(windows,target_os="linux"))
    ))
}

/// Platform API name, including builds without that optional backend.
pub fn hardware_name() -> &'static str {
    if cfg!(windows) {
        "Media Foundation"
    } else if cfg!(target_os = "linux") {
        "VA-API"
    } else {
        "hardware encoder"
    }
}
fn requires_vpl(settings: Settings, windows: bool) -> bool {
    // Windows Media Foundation supports H.264 CBR and peak-constrained VBR.
    // Linux VA-API remains CBR-only here, so other rate-control modes use VPL.
    settings.preference == Preference::Intel
        || (settings.tuning.custom_intel() && matches!(settings.preference,Preference::Auto|Preference::Hardware))
        || (!windows && (settings.codec != VideoCodec::H264 || settings.rate_control != RateControl::Cbr))
}

mod input_device;
pub use input_device::InputDevice;
#[cfg(all(windows,feature="mf-encode"))]
pub use input_device::DeviceIdentity;
#[cfg(all(windows, feature="mf-encode"))]
mod capture_pool;
#[cfg(all(windows, feature="mf-encode"))]
pub use capture_pool::{CapturePool,capture_probe_lease,capture_rgb_samples};

fn hardware(settings: Settings, input:Option<InputDevice>) -> Result<Box<dyn VideoEncoder>, CodecError> {
    if settings.hdr && !cfg!(windows) {return Err(error("HDR encoding is unavailable on this platform"));}
    #[cfg(not(all(windows,feature="mf-encode")))]
    let _=input;
    #[cfg(all(windows,feature="mf-encode"))]
    if matches!(settings.preference,Preference::Auto|Preference::Hardware|Preference::Amd) {
        // Cross-adapter encoding still uses the established CPU fallback.
        // Only bind AMF to a capture device belonging to AMD itself.
        let input=input.filter(|device|device.vendor==0x1002);
        match worker::Worker::spawn(move||amf::Driver::open_with_device(settings,input)) {
            Ok(encoder)=>return Ok(Box::new(encoder)),
            Err(reason) if settings.preference==Preference::Amd=>return Err(reason),
            Err(reason)=>tracing::debug!(%reason,"native AMF unavailable; trying platform hardware API"),
        }
    }
    let windows = cfg!(windows);
    let platform_cbr_only = !windows;
    let vpl_required = requires_vpl(settings, windows);
    if !windows && matches!(settings.preference,Preference::Nvidia|Preference::Amd)
        && (settings.codec!=VideoCodec::H264
            || (platform_cbr_only && settings.rate_control!=RateControl::Cbr))
    {
        return Err(error(if cfg!(windows) {
            "当前 NVIDIA / AMD 后端支持 H.264 CBR / VBR；HEVC / AV1 厂商接口尚未接入"
        } else {
            "当前 NVIDIA / AMD 后端支持 H.264 CBR；HEVC / AV1 和 VBR 厂商接口尚未接入"
        }));
    }
    #[cfg(all(windows,feature="mf-encode"))]
    if settings.preference!=Preference::Intel || settings.hdr || settings.source_hdr.is_some() {
        match worker::Worker::spawn(move||mf::Driver::open(settings)) {
            Ok(encoder)=>return Ok(Box::new(encoder)),
            Err(mf_error)=>{
                #[cfg(feature="intel-vpl")]
                if matches!(settings.preference,Preference::Auto|Preference::Hardware|Preference::Intel) && (settings.codec!=VideoCodec::H264 || settings.tuning.custom_intel() || settings.preference==Preference::Intel) {
                    return worker::Worker::spawn(move||vpl::Driver::open(settings))
                        .map(|encoder|Box::new(encoder) as Box<dyn VideoEncoder>)
                        .map_err(|vpl_error|error(format!("Media Foundation: {mf_error}; Intel VPL: {vpl_error}")));
                }
                return Err(mf_error);
            }
        }
    }
    #[cfg(all(feature="intel-vpl",any(windows,target_os="linux")))]
    if vpl_required
        || !cfg!(any(all(target_os="linux",feature="vaapi"),all(windows,feature="mf-encode")))
    {
        return Ok(Box::new(worker::Worker::spawn(move||vpl::Driver::open(settings))?));
    }
    if vpl_required {
        return Err(error("selected encoder requires the intel-vpl backend in this build"));
    }
    #[cfg(all(target_os = "linux", feature = "vaapi"))]
    return Ok(Box::new(worker::Worker::spawn(move || {
        vaapi::Driver::open(settings)
    })?));
    #[cfg(all(windows, feature = "mf-encode"))]
    return Ok(Box::new(worker::Worker::spawn(move || {
        mf::Driver::open(settings)
    })?));
    #[cfg(not(any(
        all(target_os = "linux", feature = "vaapi"),
        all(windows, feature = "mf-encode")
    )))]
    {
        let _ = settings;
        Err(error(format!(
            "{} encoding is unavailable in this platform/build",
            hardware_name()
        )))
    }
}

pub fn create(settings: Settings) -> Result<Box<dyn VideoEncoder>, CodecError> {
    create_with_device(settings,None)
}

/// Initialize against the capture device when the selected backend supports it.
/// Other backends keep their existing initialization and fallback policy.
pub fn create_with_device(settings: Settings,input:Option<InputDevice>) -> Result<Box<dyn VideoEncoder>, CodecError> {
    settings.validate()?;
    if settings.codec==VideoCodec::Pyrowave {
        #[cfg(feature="pyrowave")]
        {return Ok(Box::new(crate::pyrowave::Encoder::new(settings,input)?));}
        #[cfg(not(feature="pyrowave"))]
        {return Err(CodecError::Unsupported(settings.codec));}
    }
    let (active, can_fallback, note) = match settings.preference {
        Preference::Software => (software(settings)?, false, false),
        Preference::Hardware | Preference::Intel | Preference::Nvidia | Preference::Amd => (hardware(settings,input)?, false, false),
        Preference::Vaapi => {
            if settings.codec!=VideoCodec::H264 || settings.rate_control!=RateControl::Cbr {
                return Err(error("显式 VA-API 后端仅支持 H.264 CBR；其它模式请选择自动或硬件编码"));
            }
            if !cfg!(all(target_os = "linux", feature = "vaapi")) {
                return Err(error(
                    "VA-API requires a Linux build with the vaapi feature",
                ));
            }
            (hardware(settings,input)?, false, false)
        }
        Preference::Auto => match hardware(settings,input) {
            Ok(encoder) => (encoder, settings.codec==VideoCodec::H264 && settings.rate_control==RateControl::Cbr && settings.tuning==Tuning::default(), false),
            Err(reason) => {
                if settings.codec!=VideoCodec::H264 || settings.rate_control!=RateControl::Cbr || settings.tuning!=Tuning::default(){return Err(reason);}
                tracing::warn!(%reason,backend=hardware_name(),"native hardware encoder unavailable; using software");
                (software(settings)?, false, true)
            }
        },
    };
    let name = format!(
        "{}{}",
        active.backend_name(),
        if note {
            format!(" · {} unavailable", hardware_name())
        } else {
            String::new()
        }
    );
    Ok(Box::new(Selected {
        active,
        settings,
        can_fallback,
        name,
    }))
}

struct Selected {
    active: Box<dyn VideoEncoder>,
    settings: Settings,
    can_fallback: bool,
    name: String,
}
impl Selected {
    fn fallback(&mut self, settings: Settings) -> Result<(), CodecError> {
        let replacement = software(settings)?;
        self.name = format!(
            "{} · fallback from {}",
            replacement.backend_name(),
            hardware_name()
        );
        self.active = replacement;
        self.settings = settings;
        self.can_fallback = false;
        self.active.request_keyframe();
        Ok(())
    }
}
impl VideoEncoder for Selected {
    fn is_hardware(&self)->bool {self.active.is_hardware()}
    fn gpu_input(&self)->bool {self.active.gpu_input()}
    fn gpu_vendor(&self)->Option<u16> {self.active.gpu_vendor()}
    fn backend_name(&self) -> &str {
        &self.name
    }
    fn codec(&self) -> uu_proto::media::VideoCodec {
        self.active.codec()
    }
    fn encode(&mut self, frame: &RawFrame) -> Result<Vec<EncodedFrame>, CodecError> {
        if (frame.width, frame.height) != (self.settings.width, self.settings.height) {
            return Err(error("capture dimensions changed; reopen native sharing"));
        }
        match self.active.encode(frame) {
            Ok(frames) => Ok(frames),
            Err(reason) if self.can_fallback && !frame.gpu => {
                tracing::warn!(%reason,"native hardware encoding failed; restarting reference chain in software");
                self.fallback(self.settings)?;
                self.active.encode(frame)
            }
            Err(reason) => Err(reason),
        }
    }
    fn request_keyframe(&mut self) {
        self.active.request_keyframe();
    }
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError> {
        if kbps==self.settings.bitrate_kbps{return Ok(());}
        let settings = Settings {
            bitrate_kbps: kbps,
            ..self.settings
        };
        settings.validate()?;
        match self.active.set_bitrate(kbps) {
            Ok(()) => {
                self.settings = settings;
                Ok(())
            }
            Err(reason) => Err(reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PixelFormat, Timestamp, VideoDecoder};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    fn settings() -> Settings {
        Settings {
            hdr:false,source_hdr:None,tuning:Default::default(), width: 64,
            height: 64,
            fps: 30,
            bitrate_kbps: 1000,
            preference: Preference::Software,
            codec:VideoCodec::H264,rate_control:RateControl::Cbr,
        }
    }
    fn frame() -> RawFrame {
        RawFrame {
            width: 64,
            height: 64,
            stride: 64,
            format: PixelFormat::I420,
            data: vec![128; 64 * 64 * 3 / 2].into(),
            pts: Timestamp(0),
            gpu: false,
            dma_buf: None,
            d3d11: None,
        }
    }
    struct Broken {
        encodes: Arc<AtomicUsize>,
        tunes: Arc<AtomicUsize>,
    }
    impl VideoEncoder for Broken {
        fn codec(&self) -> uu_proto::media::VideoCodec {
            uu_proto::media::VideoCodec::H264
        }
        fn backend_name(&self) -> &str {
            "simulated hardware"
        }
        fn encode(&mut self, _: &RawFrame) -> Result<Vec<EncodedFrame>, CodecError> {
            self.encodes.fetch_add(1, Ordering::Relaxed);
            Err(error("simulated driver failure"))
        }
        fn request_keyframe(&mut self) {}
        fn set_bitrate(&mut self, _: u32) -> Result<(), CodecError> {
            self.tunes.fetch_add(1, Ordering::Relaxed);
            Err(error("simulated tuning failure"))
        }
    }
    fn selected() -> (Selected, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let encodes = Arc::new(AtomicUsize::new(0));
        let tunes = Arc::new(AtomicUsize::new(0));
        (
            Selected {
                active: Box::new(Broken {
                    encodes: encodes.clone(),
                    tunes: tunes.clone(),
                }),
                settings: settings(),
                can_fallback: true,
                name: "simulated hardware".into(),
            },
            encodes,
            tunes,
        )
    }
    #[test]
    fn backend_policy_uses_windows_mf_for_all_formats_and_vpl_for_explicit_intel() {
        let base = settings();
        let vbr = Settings {
            rate_control: RateControl::Vbr { peak_kbps: 2000 },
            ..base
        };
        assert!(!requires_vpl(vbr, true));
        assert!(requires_vpl(vbr, false));
        assert!(!requires_vpl(
            Settings {
                codec: VideoCodec::Hevc,
                ..base
            },
            true
        ));
        assert!(requires_vpl(
            Settings {
                preference: Preference::Intel,
                ..base
            },
            true
        ));
        assert!(!requires_vpl(base, true));
    }

    #[test]
    #[cfg(not(all(windows,feature="mf-encode")))]
    fn explicit_non_intel_backends_reject_unimplemented_codecs_without_falling_back() {
        for preference in [Preference::Nvidia,Preference::Amd] {
            for codec in [VideoCodec::Hevc,VideoCodec::Av1] {
                let result=create(Settings {preference,codec,..settings()});
                assert!(result.is_err());
                assert!(result.err().unwrap().to_string().contains("NVIDIA / AMD"));
            }
        }
    }
    #[test]
    fn codec_and_peak_policy_rejects_silent_software_substitution_and_invalid_budgets(){
        let base=settings();
        for codec in [VideoCodec::Hevc,VideoCodec::Av1]{
            assert!(create(Settings{codec,..base}).is_err());
            assert!(Settings{codec,fps:120,..base}.validate().is_ok());
        }
        for peak_kbps in [999,2_000_001]{assert!(Settings{rate_control:RateControl::Vbr{peak_kbps},..base}.validate().is_err());}
        assert!(create(Settings{rate_control:RateControl::Vbr{peak_kbps:2000},..base}).is_err());
    }
    #[test]
    fn runtime_failure_falls_back_once_with_an_independently_decodable_idr() {
        let (mut encoder, encodes, _) = selected();
        let output = encoder.encode(&frame()).unwrap();
        assert_eq!(output.len(), 1);
        assert!(output[0].is_keyframe);
        assert!(crate::H264Decoder::new()
            .unwrap()
            .decode(&output[0])
            .unwrap()
            .is_some());
        assert!(encoder
            .backend_name()
            .contains(&format!("fallback from {}", hardware_name())));
        let mut next = frame();
        next.pts = Timestamp(33_333);
        encoder.encode(&next).unwrap();
        assert_eq!(encodes.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn invalid_parameters_preserve_the_current_encoder_before_tuning() {
        let (mut encoder, _, tunes) = selected();
        assert!(encoder.set_bitrate(0).is_err());
        assert_eq!(tunes.load(Ordering::Relaxed), 0);
        assert_eq!(encoder.backend_name(), "simulated hardware");
        assert!(encoder.set_bitrate(2000).is_err());
        assert_eq!(tunes.load(Ordering::Relaxed), 1);
        assert_eq!(encoder.backend_name(),"simulated hardware");
        assert_eq!(encoder.settings.bitrate_kbps,1000);
    }
    #[test]
    fn size_changes_are_rejected_before_touching_the_driver() {
        let (mut encoder, encodes, _) = selected();
        let mut changed = frame();
        changed.width = 66;
        assert!(encoder.encode(&changed).is_err());
        assert_eq!(encodes.load(Ordering::Relaxed), 0);
        for bad in [
            Settings {
                width: 0,
                ..settings()
            },
            Settings {
                width: 65,
                ..settings()
            },
            Settings {
                fps: 241,
                ..settings()
            },
            Settings {
                width: 8192,
                height: 8192,
                ..settings()
            },
        ] {
            assert!(bad.validate().is_err());
        }
    }
    #[cfg(not(any(
        all(target_os = "linux", feature = "vaapi"),
        all(windows, feature = "mf-encode"),
        all(feature="intel-vpl",any(windows,target_os="linux"))
    )))]
    #[test]
    fn builds_without_vaapi_report_unavailable_and_auto_uses_software() {
        assert!(create(Settings {
            preference: Preference::Vaapi,
            ..settings()
        })
        .is_err());
        let mut encoder = create(Settings {
            preference: Preference::Auto,
            ..settings()
        })
        .unwrap();
        assert!(encoder.backend_name().starts_with("openh264"));
        assert!(encoder.encode(&frame()).unwrap()[0].is_keyframe);
    }
}

#[cfg(all(windows,feature="mf-encode"))]
fn validate_hdr_output(frame:&EncodedFrame,codec:VideoCodec,hdr:bool)->Result<(),String> {
    if !hdr || !frame.is_keyframe {return Ok(());}
    let actual=match codec {
        VideoCodec::Hevc=>crate::hevc::sequence(&frame.data).map(|s|(s.bit_depth,s.color)),
        VideoCodec::Av1=>crate::av1::sequence(&frame.data).map(|s|(s.bit_depth,s.color)),
        VideoCodec::H264|VideoCodec::Pyrowave=>None,
    };
    if actual==Some((10,crate::color::YuvColor::Limited2020Pq)) {Ok(())}
    else {Err("encoder did not produce the negotiated 10-bit PQ/BT.2020 signal".into())}
}
