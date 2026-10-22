//! Native-host encoding selection. Interoperability decoding is independent.
use uu_proto::media::VideoCodec;
use crate::{codec::CodecError, EncodedFrame, H264Encoder, RawFrame, VideoEncoder};

#[cfg(all(windows, feature = "mf-encode"))]
mod mf;
#[cfg(any(test,feature="intel-vpl",all(windows,feature="mf-encode")))]
mod low_delay;
mod upload;
#[cfg(all(target_os = "linux", feature = "vaapi"))]
mod vaapi;
mod worker;
#[cfg(any(test,feature="intel-vpl"))]
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
    Vaapi,
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
        if !(2..=8192).contains(&self.width)
            || !(2..=8192).contains(&self.height)
            || self.width % 2 != 0
            || self.height % 2 != 0
            || u64::from(self.width)*u64::from(self.height)*3/2>64*1024*1024
            || !(1..=if self.codec==VideoCodec::H264{60}else{240}).contains(&self.fps)
            || !(64..=100_000).contains(&self.bitrate_kbps)
        {
            return Err(error(
                "encoding needs even 2..=8192 dimensions, valid FPS and 64..=100000 kbps",
            ));
        }
        let peak=self.rate_control.peak(self.bitrate_kbps);
        if peak<self.bitrate_kbps || peak>200_000{return Err(error("VBR peak must be at least target bitrate and at most 200000 kbps"));}
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
fn hardware(settings: Settings) -> Result<Box<dyn VideoEncoder>, CodecError> {
    #[cfg(all(feature="intel-vpl",any(windows,target_os="linux")))]
    if settings.codec!=VideoCodec::H264 || settings.rate_control!=RateControl::Cbr
        || !cfg!(any(all(target_os="linux",feature="vaapi"),all(windows,feature="mf-encode"))) {
        return Ok(Box::new(worker::Worker::spawn(move||vpl::Driver::open(settings))?));
    }
    if settings.codec!=VideoCodec::H264 || settings.rate_control!=RateControl::Cbr {
        return Err(error("HEVC/AV1 and VBR require the intel-vpl backend in this build"));
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
    settings.validate()?;
    let (active, can_fallback, note) = match settings.preference {
        Preference::Software => (software(settings)?, false, false),
        Preference::Hardware => (hardware(settings)?, false, false),
        Preference::Vaapi => {
            if settings.codec!=VideoCodec::H264 || settings.rate_control!=RateControl::Cbr {
                return Err(error("显式 VA-API 后端仅支持 H.264 CBR；其它模式请选择自动或硬件编码"));
            }
            if !cfg!(all(target_os = "linux", feature = "vaapi")) {
                return Err(error(
                    "VA-API requires a Linux build with the vaapi feature",
                ));
            }
            (hardware(settings)?, false, false)
        }
        Preference::Auto => match hardware(settings) {
            Ok(encoder) => (encoder, settings.codec==VideoCodec::H264 && settings.rate_control==RateControl::Cbr, false),
            Err(reason) => {
                if settings.codec!=VideoCodec::H264 || settings.rate_control!=RateControl::Cbr{return Err(reason);}
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
            Err(reason) if self.can_fallback => {
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
            Err(reason) if self.can_fallback => {
                tracing::warn!(%reason,"native hardware tuning failed; using software");
                self.fallback(settings)
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
            width: 64,
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
    fn codec_and_peak_policy_rejects_silent_software_substitution_and_invalid_budgets(){
        let base=settings();
        for codec in [VideoCodec::Hevc,VideoCodec::Av1]{
            assert!(create(Settings{codec,..base}).is_err());
            assert!(Settings{codec,fps:120,..base}.validate().is_ok());
        }
        for peak_kbps in [999,200001]{assert!(Settings{rate_control:RateControl::Vbr{peak_kbps},..base}.validate().is_err());}
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
        encoder.set_bitrate(2000).unwrap();
        assert_eq!(tunes.load(Ordering::Relaxed), 1);
        assert!(encoder.backend_name().contains("fallback"));
        assert!(encoder.encode(&frame()).unwrap()[0].is_keyframe);
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
                fps: 61,
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
