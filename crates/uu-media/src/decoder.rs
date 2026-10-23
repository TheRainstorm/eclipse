//! Decoder selection: hardware first where the platform offers it, with a
//! software fallback that cannot leave a session without a picture.
//!
//! The controller always prefers a hardware backend (Windows Media Foundation
//! on a D3D11 device manager, Linux VA-API) because it keeps a 1080p60 stream
//! inside the CPU budget the software decoder spends on entropy decoding.
//! Hardware paths fail in ways that are invisible from the outside though —
//! a driver can accept every call and never emit a frame — so the selected
//! hardware decoder is always wrapped in [`FallbackDecoder`], which demotes
//! to OpenH264 on repeated errors or on a stream of `Ok(None)` outputs.
//!
//! `UU_DECODER=auto|sw|hw` overrides the choice (`sw` is also spelled
//! `software`/`cpu`, `hw` as `hardware`/`d3d11va`/`vaapi`).

use crate::codec::{CodecError, VideoDecoder};
use crate::frame::{EncodedFrame, RawFrame};
use uu_proto::media::VideoCodec;

/// AV1 hardware encoders may append black padding. The stream/container's
/// confirmed display dimensions are independent from its coded sequence size.
pub(crate) struct DisplaySizeDecoder {
    inner: Box<dyn VideoDecoder>,
    visible: Option<[u32;2]>,
}
impl DisplaySizeDecoder {
    pub(crate) fn new(inner:Box<dyn VideoDecoder>)->Self { Self{inner,visible:None} }
    fn crop(&self,frame:RawFrame)->Result<RawFrame,CodecError> {
        match self.visible {Some(visible)=>frame.crop_decoded(visible),None=>Ok(frame)}
    }
}
impl VideoDecoder for DisplaySizeDecoder {
    fn codec(&self)->VideoCodec {self.inner.codec()}
    fn backend_name(&self)->&str {self.inner.backend_name()}
    fn prepare(&mut self,width:u32,height:u32)->Result<(),CodecError> {
        if width<2 || height<2 || width%2!=0 || height%2!=0 || width>8192 || height>8192 {
            return Err(CodecError::Backend("invalid negotiated display dimensions".into()));
        }
        self.inner.prepare(width,height)?;
        self.visible=Some([width,height]);Ok(())
    }
    fn decode(&mut self,frame:&EncodedFrame)->Result<Option<RawFrame>,CodecError> {
        self.inner.decode(frame)?.map(|picture|self.crop(picture)).transpose()
    }
    fn decode_frames(&mut self,frame:&EncodedFrame)->Result<Vec<RawFrame>,CodecError> {
        self.inner.decode_frames(frame)?.into_iter().map(|picture|self.crop(picture)).collect()
    }
}

/// A decoder that reports a more descriptive backend name than its inner one.
///
/// Selection sometimes has something to say that the decoder itself does not
/// know — "hardware unavailable: <reason>" — and the HUD reads the name off
/// the decoder instance, so the note has to travel with it.
pub struct NamedDecoder {
    inner: Box<dyn VideoDecoder>,
    name: String,
}

impl NamedDecoder {
    pub fn new(inner: Box<dyn VideoDecoder>, name: impl Into<String>) -> Self {
        Self {
            inner,
            name: name.into(),
        }
    }
}

impl VideoDecoder for NamedDecoder {
    fn codec(&self) -> VideoCodec {
        self.inner.codec()
    }

    fn backend_name(&self) -> &str {
        &self.name
    }

    fn prepare(&mut self, width: u32, height: u32) -> Result<(), CodecError> {
        self.inner.prepare(width, height)
    }

    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<RawFrame>, CodecError> {
        self.inner.decode(frame)
    }
    fn decode_frames(&mut self, frame: &EncodedFrame) -> Result<Vec<RawFrame>, CodecError> {
        self.inner.decode_frames(frame)
    }
}

/// How many access units a hardware decoder may consume without producing a
/// single frame before it is considered hung. Input is already IDR-gated.
/// Bound both startup and a driver that stalls after previously working.
const HARDWARE_GRACE_UNITS: u64 = 30;

/// Consecutive errors tolerated before the hardware path is abandoned.
const ERROR_BUDGET: u32 = 3;

/// Caller's decoder preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderKind {
    /// Try hardware, fall back silently (the default).
    Auto,
    /// Force the in-process software decoder.
    Software,
    /// Ask for hardware; still falls back rather than showing a black screen.
    Hardware,
}

impl DecoderKind {
    /// Read `UU_DECODER` (unset or unrecognised values mean `Auto`).
    pub fn from_env() -> Self {
        match std::env::var("UU_DECODER")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "sw" | "software" | "cpu" | "openh264" => Self::Software,
            "hw" | "hardware" | "d3d11va" | "vaapi" | "mf" | "mediafoundation" => Self::Hardware,
            _ => Self::Auto,
        }
    }
}

/// A hardware decoder guarded by a software decoder it can demote to.
///
/// Demotion is permanent for the lifetime of the instance: once a driver has
/// failed us we stop paying its latency. The HUD shows the demotion (and the
/// reason) because [`VideoDecoder::backend_name`] reports the current path.
pub struct FallbackDecoder {
    primary: Option<Box<dyn VideoDecoder>>,
    backup: Box<dyn VideoDecoder>,
    primary_name: String,
    current: String,
    empty_units: u64,
    empty_since: Option<std::time::Instant>,
    consecutive_errors: u32,
}

impl FallbackDecoder {
    pub fn new(primary: Box<dyn VideoDecoder>, backup: Box<dyn VideoDecoder>) -> Self {
        let primary_name = primary.backend_name().to_string();
        Self {
            primary: Some(primary),
            current: primary_name.clone(),
            backup,
            primary_name,
            empty_units: 0,
            empty_since: None,
            consecutive_errors: 0,
        }
    }

    /// True while the hardware path is still in use.
    pub fn on_hardware(&self) -> bool {
        self.primary.is_some()
    }

    fn demote(&mut self, reason: &str) {
        if self.primary.is_none() {
            return;
        }
        eprintln!(
            "uu-media: hardware decoder '{}' demoted to '{}' ({reason})",
            self.primary_name,
            self.backup.backend_name()
        );
        self.primary = None;
        self.current = format!(
            "{} [hardware {} abandoned: {reason}]",
            self.backup.backend_name(),
            self.primary_name
        );
    }

    fn decode_with<F>(&mut self, frame: &EncodedFrame, decode: F) -> Result<Vec<RawFrame>, CodecError>
    where F: Fn(&mut dyn VideoDecoder, &EncodedFrame) -> Result<Vec<RawFrame>, CodecError> {
        let result = match self.primary.as_mut() {
            Some(primary) => decode(primary.as_mut(), frame),
            None => return decode(self.backup.as_mut(), frame),
        };
        match result {
            Ok(frames) if !frames.is_empty() => {
                self.empty_units = 0;
                self.empty_since = None;
                self.consecutive_errors = 0;
                Ok(frames)
            }
            Ok(frames) => {
                self.empty_units += 1;
                let since = *self.empty_since.get_or_insert_with(std::time::Instant::now);
                if self.empty_units > HARDWARE_GRACE_UNITS || since.elapsed() >= std::time::Duration::from_millis(750) {
                    self.demote("no output");
                    return decode(self.backup.as_mut(), frame);
                }
                Ok(frames)
            }
            Err(e) => {
                self.consecutive_errors += 1;
                if self.consecutive_errors >= ERROR_BUDGET {
                    self.demote(&e.to_string());
                    return decode(self.backup.as_mut(), frame);
                }
                Err(e)
            }
        }
    }

}

impl VideoDecoder for FallbackDecoder {
    fn codec(&self) -> uu_proto::media::VideoCodec {
        self.backup.codec()
    }

    fn backend_name(&self) -> &str {
        &self.current
    }

    fn prepare(&mut self, width: u32, height: u32) -> Result<(), CodecError> {
        self.backup.prepare(width, height)?;
        if let Some(primary) = &mut self.primary {
            if let Err(error) = primary.prepare(width, height) {
                self.demote(&format!("preparation failed: {error}"));
            }
        }
        Ok(())
    }

    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<RawFrame>, CodecError> {
        self.decode_with(frame, |decoder, frame| Ok(decoder.decode(frame)?.into_iter().collect()))
            .map(|mut frames| frames.pop())
    }
    fn decode_frames(&mut self, frame: &EncodedFrame) -> Result<Vec<RawFrame>, CodecError> {
        self.decode_with(frame, |decoder, frame| decoder.decode_frames(frame))
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{EncodedFrame, PixelFormat, RawFrame};
    use bytes::Bytes;
    use uu_proto::media::VideoCodec;

    struct Scripted {
        name: &'static str,
        /// Errors to return before the scripted frames start.
        errors: u32,
        frames: u32,
        emitted: u32,
    }

    impl Scripted {
        fn new(name: &'static str, errors: u32, frames: u32) -> Self {
            Self { name, errors, frames, emitted: 0 }
        }
    }

    impl VideoDecoder for Scripted {
        fn codec(&self) -> VideoCodec {
            VideoCodec::H264
        }
        fn backend_name(&self) -> &str {
            self.name
        }
        fn decode(&mut self, _frame: &EncodedFrame) -> Result<Option<RawFrame>, CodecError> {
            if self.errors > 0 {
                self.errors -= 1;
                return Err(CodecError::Backend("scripted failure".into()));
            }
            if self.emitted < self.frames {
                self.emitted += 1;
                return Ok(Some(RawFrame {
                    width: 2,
                    height: 2,
                    format: PixelFormat::I420,
                    stride: 2,
                    data: Bytes::from(vec![0u8; 6]),
                    pts: crate::Timestamp(0),
                    gpu: false,
                    dma_buf: None,
                    d3d11: None,
                }));
            }
            Ok(None)
        }
    }

    fn unit() -> EncodedFrame {
        EncodedFrame { data: Bytes::from_static(&[0, 0, 0, 1, 0x65]), pts: crate::Timestamp(0), is_keyframe: true }
    }

    #[test]
    fn display_size_preserves_delayed_batches_and_changes_only_after_valid_preparation() {
        struct Padded;
        impl VideoDecoder for Padded {
            fn codec(&self)->VideoCodec {VideoCodec::Av1}
            fn prepare(&mut self,w:u32,_:u32)->Result<(),CodecError> {
                if w==8 {return Err(CodecError::Backend("fixture rejects geometry".into()));}Ok(())
            }
            fn decode(&mut self,_:&EncodedFrame)->Result<Option<RawFrame>,CodecError> {panic!("batch fixture")}
            fn decode_frames(&mut self,f:&EncodedFrame)->Result<Vec<RawFrame>,CodecError> {
                if f.pts.0==0 {return Ok(vec![]);}
                Ok([f.pts.0-1,f.pts.0].into_iter().map(|pts|RawFrame::new_i420(6,6,vec![128;54],crate::Timestamp(pts))).collect())
            }
        }
        let mut decoder=DisplaySizeDecoder::new(Box::new(Padded));
        let mut frame=unit();
        assert!(decoder.decode_frames(&frame).unwrap().is_empty());
        frame.pts=crate::Timestamp(1);
        assert_eq!(decoder.decode_frames(&frame).unwrap()[0].width,6);
        decoder.prepare(4,4).unwrap();
        assert!(decoder.prepare(8,4).is_err());
        assert!(decoder.prepare(3,4).is_err());
        let pictures=decoder.decode_frames(&frame).unwrap();
        assert_eq!(pictures.iter().map(|f|(f.width,f.height,f.pts.0,f.data.len())).collect::<Vec<_>>(),[(4,4,0,24),(4,4,1,24)]);
        decoder.prepare(6,4).unwrap();
        assert_eq!(decoder.decode_frames(&frame).unwrap()[0].width,6);
        decoder.prepare(6,8).unwrap();
        assert!(decoder.decode_frames(&frame).is_err(),"never stretch an undersized decoded picture");
    }

    #[test]
    fn named_and_fallback_keep_a_delayed_batch_without_replaying_input() {
        use std::sync::{Arc, Mutex};
        struct Burst(Arc<Mutex<Vec<u64>>>);
        impl VideoDecoder for Burst {
            fn codec(&self) -> VideoCodec { VideoCodec::H264 }
            fn decode(&mut self, _: &EncodedFrame) -> Result<Option<RawFrame>, CodecError> {
                panic!("the batch route must remain intact through wrappers")
            }
            fn decode_frames(&mut self, frame: &EncodedFrame) -> Result<Vec<RawFrame>, CodecError> {
                self.0.lock().unwrap().push(frame.pts.0);
                let indexes: &[u64] = if frame.pts.0 == 0 { &[] } else if frame.pts.0 == 1 { &[0, 1] } else { &[2] };
                Ok(indexes.iter().map(|pts| RawFrame {
                    width:2,height:2,format:PixelFormat::I420,stride:2,
                    data:Bytes::from(vec![0;6]),pts:crate::Timestamp(*pts),
                    gpu:false,dma_buf:None,d3d11:None,
                }).collect())
            }
        }
        let calls = Arc::new(Mutex::new(Vec::new()));
        let named = NamedDecoder::new(Box::new(Burst(calls.clone())), "batch fixture");
        let mut decoder = FallbackDecoder::new(Box::new(named), Box::new(Scripted::new("unused", 0, 0)));
        let mut frame = unit();
        assert!(decoder.decode_frames(&frame).unwrap().is_empty());
        frame.pts = crate::Timestamp(1);
        assert_eq!(decoder.decode_frames(&frame).unwrap().iter().map(|f|f.pts.0).collect::<Vec<_>>(), [0,1]);
        frame.pts = crate::Timestamp(2);
        assert_eq!(decoder.decode_frames(&frame).unwrap()[0].pts.0, 2);
        assert!(decoder.on_hardware());
        assert_eq!(*calls.lock().unwrap(), [0,1,2]);
    }

    #[test]
    fn failed_preparation_falls_back_before_any_video_arrives() {
        struct UnsupportedGeometry;
        impl VideoDecoder for UnsupportedGeometry {
            fn codec(&self) -> VideoCodec { VideoCodec::H264 }
            fn prepare(&mut self, _: u32, _: u32) -> Result<(), CodecError> {
                Err(CodecError::Backend("unsupported fixture geometry".into()))
            }
            fn decode(&mut self, _: &EncodedFrame) -> Result<Option<RawFrame>, CodecError> {
                panic!("failed hardware preparation must not receive video")
            }
        }
        let mut primary = NamedDecoder::new(Box::new(UnsupportedGeometry), "fixture hw");
        assert!(primary.prepare(1280, 720).is_err());
        let mut decoder = FallbackDecoder::new(Box::new(primary), Box::new(Scripted::new("sw", 0, 10)));
        decoder.prepare(1280, 720).unwrap();
        assert!(!decoder.on_hardware());
        assert!(decoder.backend_name().contains("preparation failed"));
        assert!(decoder.decode(&unit()).unwrap().is_some());
    }

    #[test]
    fn hardware_that_stalls_after_a_good_frame_also_falls_back() {
        let mut decoder = FallbackDecoder::new(
            Box::new(Scripted::new("hw", 0, 1)),
            Box::new(Scripted::new("sw", 0, 10)),
        );
        assert!(decoder.decode(&unit()).unwrap().is_some());
        assert!(decoder.decode(&unit()).unwrap().is_none());
        decoder.empty_since = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        assert!(decoder.decode(&unit()).unwrap().is_some());
        assert!(!decoder.on_hardware());
    }

    #[test]
    fn healthy_hardware_is_used_directly() {
        let mut d = FallbackDecoder::new(
            Box::new(Scripted::new("hw", 0, 100)),
            Box::new(Scripted::new("sw", 0, 100)),
        );
        for _ in 0..10 {
            assert!(d.decode(&unit()).unwrap().is_some());
        }
        assert_eq!(d.backend_name(), "hw");
        assert!(d.on_hardware());
    }

    #[test]
    fn persistent_errors_demote_to_software() {
        let mut d = FallbackDecoder::new(
            Box::new(Scripted::new("hw", u32::MAX, 0)),
            Box::new(Scripted::new("sw", 0, 100)),
        );
        // The first two failures are surfaced, the third demotes and decodes.
        assert!(d.decode(&unit()).is_err());
        assert!(d.decode(&unit()).is_err());
        assert!(d.decode(&unit()).unwrap().is_some());
        assert!(!d.on_hardware());
        assert!(d.backend_name().contains("sw"), "got {}", d.backend_name());
        // Subsequent frames never touch the hardware again.
        assert!(d.decode(&unit()).unwrap().is_some());
    }

    #[test]
    fn a_silent_hardware_decoder_is_demoted() {
        let mut d = FallbackDecoder::new(
            Box::new(Scripted::new("hw", 0, 0)),
            Box::new(Scripted::new("sw", 0, 100)),
        );
        for _ in 0..(HARDWARE_GRACE_UNITS + 1) {
            let _ = d.decode(&unit()).unwrap();
        }
        assert!(!d.on_hardware(), "silent hardware must be abandoned");
    }

    #[test]
    fn a_single_error_does_not_demote() {
        let mut d = FallbackDecoder::new(
            Box::new(Scripted::new("hw", 1, 100)),
            Box::new(Scripted::new("sw", 0, 100)),
        );
        assert!(d.decode(&unit()).is_err());
        assert!(d.decode(&unit()).unwrap().is_some());
        assert!(d.on_hardware());
    }
}
