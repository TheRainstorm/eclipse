//! H.264 codec built on Cisco OpenH264 (`openh264` crate).
//!
//! OpenH264 is a software codec, so this works on any platform with no GPU
//! and no system libraries — which is exactly what the M2 self-loop needs.
//! Hardware paths (NVENC/QSV/VA-API) slot in behind the same traits later.

use crate::bitstream::patch_sps_allow_gaps;
use crate::codec::{CodecError, VideoDecoder, VideoEncoder};
use crate::convert::to_i420;
use crate::frame::{EncodedFrame, RawFrame};
use bytes::Bytes;
use openh264::decoder::{Decoder as OhDecoder, DecoderConfig, Flush};
use openh264::encoder::{
    BitRate, Encoder as OhEncoder, EncoderConfig, FrameRate, RateControlMode, UsageType, VuiConfig,
};
use openh264::formats::{YUVBuffer, YUVSource};
use openh264::OpenH264API;
use uu_proto::media::VideoCodec;

fn backend_err<E: std::fmt::Display>(e: E) -> CodecError {
    CodecError::Backend(e.to_string())
}

pub struct H264Encoder {
    inner: OhEncoder,
    pending_keyframe: bool,
    frame_rate: u32,
    bitrate_kbps: u32,
    frames: u64,
}

impl H264Encoder {
    pub fn new(bitrate_kbps: u32) -> Result<Self, CodecError> {
        Self::with_frame_rate(bitrate_kbps, 30)
    }

    /// Software screen-content encoding; this OpenH264 version supports 1..=60 Hz.
    pub fn with_frame_rate(bitrate_kbps: u32, frame_rate: u32) -> Result<Self, CodecError> {
        let inner = Self::build(bitrate_kbps, frame_rate)?;
        Ok(Self {
            inner,
            pending_keyframe: true,
            frame_rate,
            bitrate_kbps,
            frames: 0,
        })
    }

    /// OpenH264's encoder has a stricter size bound than the H.264 receive level.
    pub fn validate_dimensions(width:u32,height:u32)->Result<(),CodecError> {
        if width<2 || height<2 || width%2!=0 || height%2!=0 || width.max(height)>3840 || width.min(height)>2160 {
            return Err(CodecError::Backend(format!("software H.264 needs even dimensions up to 3840x2160 or 2160x3840; requested {width}x{height}")));
        }
        Ok(())
    }

    fn build(bitrate_kbps: u32, frame_rate: u32) -> Result<OhEncoder, CodecError> {
        if !(64..=1_000_000).contains(&bitrate_kbps) || !(1..=240).contains(&frame_rate) {
            return Err(CodecError::Backend(
                "software encoding requires 64..=1000000 kbps and 1..=240 fps".into(),
            ));
        }
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(bitrate_kbps * 1000))
            .max_frame_rate(FrameRate::from_hz(frame_rate as f32))
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .skip_frames(true)
            .adaptive_quantization(false)
            .background_detection(false)
            // to_i420's BGRA conversion and synthetic YUV are full-range BT.601.
            .vui(VuiConfig::bt601().full_range(true));
        OhEncoder::with_api_config(OpenH264API::from_source(), config).map_err(backend_err)
    }

    pub fn frames_encoded(&self) -> u64 {
        self.frames
    }
}

impl VideoEncoder for H264Encoder {
    fn backend_name(&self) -> &str { "openh264 (software)" }
    fn codec(&self) -> VideoCodec {
        VideoCodec::H264
    }

    fn encode(&mut self, frame: &RawFrame) -> Result<Vec<EncodedFrame>, CodecError> {
        Self::validate_dimensions(frame.width,frame.height)?;
        if u64::from(frame.width.div_ceil(16))*u64::from(frame.height.div_ceil(16))*u64::from(self.frame_rate)>2_073_600 {
            return Err(CodecError::Backend("H.264 frame rate and dimensions exceed level 5.2".into()));
        }
        let packed = to_i420(frame)?;
        if self.pending_keyframe {
            self.inner.force_intra_frame();
            self.pending_keyframe = false;
        }
        let yuv = YUVBuffer::from_vec(packed, frame.width as usize, frame.height as usize);
        let timestamp = openh264::Timestamp::from_millis(frame.pts.0 / 1000);
        let bitstream = self.inner.encode_at(&yuv, timestamp).map_err(backend_err)?;
        let data = bitstream.to_vec();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        self.frames += 1;
        Ok(vec![EncodedFrame {
            is_keyframe: EncodedFrame::detect_h264_keyframe(&data),
            data: Bytes::from(data),
            pts: frame.pts,
        }])
    }

    fn request_keyframe(&mut self) {
        self.pending_keyframe = true;
    }

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError> {
        if kbps == self.bitrate_kbps {
            return Ok(());
        }
        // Build first, so invalid settings leave the working encoder untouched.
        // Reconfiguration starts a fresh reference chain and emits an IDR.
        let replacement = Self::build(kbps, self.frame_rate)?;
        self.inner = replacement;
        self.pending_keyframe = true;
        self.bitrate_kbps = kbps;
        Ok(())
    }
}

pub struct H264Decoder {
    inner: OhDecoder,
    frames: u64,
}

impl H264Decoder {
    pub fn new() -> Result<Self, CodecError> {
        // The crate's default config flushes after *every* decode call. That
        // tears down the reference picture list mid-GOP, so OpenH264 reports
        // dsRefLost/dsNoParamSets after the first access unit and then never
        // recovers until the next IDR. Real-time streams must decode without
        // per-call flushing.
        let config = DecoderConfig::new().flush_after_decode(Flush::NoFlush);
        let mut inner =
            OhDecoder::with_api_config(OpenH264API::from_source(), config).map_err(backend_err)?;
        // NetEase's encoder drops frames for rate control while leaving
        // `gaps_in_frame_num_value_allowed_flag` clear, and occasionally sends
        // a P slice whose reference was skipped. With error concealment off
        // OpenH264 rejects both and stalls until the next IDR. ERROR_CON_IDC=1
        // (slice copy) lets the decoder conceal the gap and keep the stream
        // alive. The option id is DECODER_OPTION_ERROR_CON_IDC (=8).
        unsafe {
            let mut ec: i32 = 1;
            let _ = inner
                .raw_api()
                .set_option(8, &mut ec as *mut i32 as *mut std::os::raw::c_void);
        }
        Ok(Self { inner, frames: 0 })
    }

    pub fn frames_decoded(&self) -> u64 {
        self.frames
    }
}

impl VideoDecoder for H264Decoder {
    fn codec(&self) -> VideoCodec {
        VideoCodec::H264
    }

    fn backend_name(&self) -> &str {
        "openh264 (software, in-process)"
    }

    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<RawFrame>, CodecError> {
        // Repair the parameter sets the encoder emits: mark that frame_num
        // gaps are permitted so OpenH264 does not treat its own rate-control
        // frame drops as a fatal reference loss.
        let patched = patch_sps_allow_gaps(&frame.data);
        let decoded = self
            .inner
            .decode(patched.as_deref().unwrap_or(&frame.data))
            .map_err(backend_err)?;
        let Some(yuv) = decoded else {
            // Not enough data yet (mid-GOP), or a non-VCL NAL only.
            return Ok(None);
        };
        let (width, height) = yuv.dimensions();
        let (ys, cs, _vs) = yuv.strides();
        let (w, h) = (width as u32, height as u32);

        // Copy out immediately: `DecodedYUV` borrows the decoder.
        let mut packed = vec![0u8; width * height * 3 / 2];
        let luma_len = width * height;
        for row in 0..height {
            packed[row * width..row * width + width]
                .copy_from_slice(&yuv.y()[row * ys..row * ys + width]);
        }
        let (cw, ch) = (width / 2, height / 2);
        let u_off = luma_len;
        let v_off = luma_len + cw * ch;
        for row in 0..ch {
            packed[u_off + row * cw..u_off + row * cw + cw]
                .copy_from_slice(&yuv.u()[row * cs..row * cs + cw]);
            packed[v_off + row * cw..v_off + row * cw + cw]
                .copy_from_slice(&yuv.v()[row * cs..row * cs + cw]);
        }
        self.frames += 1;
        Ok(Some(RawFrame {
            width: w,
            height: h,
            format: crate::frame::PixelFormat::I420,
            stride: w,
            data: Bytes::from(packed),
            pts: frame.pts,
            gpu: false,
            dma_buf: None,
            d3d11: None,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Timestamp;

    /// A horizontal luma ramp is a stable, low-entropy input for the codec.
    fn ramp(w: u32, h: u32, shift: u8) -> RawFrame {
        let mut data = vec![0u8; (w * h) as usize * 3 / 2];
        for y in 0..h as usize {
            for x in 0..w as usize {
                data[y * w as usize + x] = ((x as u32 * 2 + shift as u32) % 256) as u8;
            }
        }
        for p in data.iter_mut().skip((w * h) as usize) {
            *p = 128;
        }
        RawFrame::new_i420(w, h, data, Timestamp(0))
    }

    #[test]
    fn encode_decode_roundtrip_produces_frames() {
        let mut enc = H264Encoder::new(2_000).unwrap();
        let mut dec = H264Decoder::new().unwrap();
        let (w, h) = (64u32, 48u32);

        let first = enc.encode(&ramp(w, h, 0)).unwrap();
        assert_eq!(first.len(), 1, "first frame should produce one access unit");
        assert!(first[0].is_keyframe, "first frame must be a keyframe");

        // The decoder needs the IDR before it can emit anything.
        let out = dec.decode(&first[0]).unwrap();
        assert!(out.is_some(), "decoder should emit the IDR frame");
        let frame = out.unwrap();
        assert_eq!((frame.width, frame.height), (w, h));
        assert_eq!(frame.data.len(), (w * h * 3 / 2) as usize);
        assert!(frame.is_consistent());
    }

    #[test]
    fn delta_frames_decode_after_keyframe() {
        let mut enc = H264Encoder::new(2_000).unwrap();
        let mut dec = H264Decoder::new().unwrap();
        let (w, h) = (64u32, 48u32);

        let key = enc.encode(&ramp(w, h, 0)).unwrap();
        dec.decode(&key[0]).unwrap().expect("keyframe");

        let mut last = None;
        for shift in (2..20u8).step_by(2) {
            let enc_frames = enc.encode(&ramp(w, h, shift)).unwrap();
            for f in &enc_frames {
                if let Some(decoded) = dec.decode(f).unwrap() {
                    last = Some(decoded);
                }
            }
        }
        let last = last.expect("at least one delta frame should decode");
        assert_eq!((last.width, last.height), (w, h));
    }

    #[test]
    fn forced_keyframe_is_requestable() {
        let mut enc = H264Encoder::new(2_000).unwrap();
        let (w, h) = (64u32, 48u32);
        enc.encode(&ramp(w, h, 0)).unwrap();
        enc.encode(&ramp(w, h, 1)).unwrap();
        enc.request_keyframe();
        let f = enc.encode(&ramp(w, h, 2)).unwrap();
        assert!(f[0].is_keyframe, "requested frame must be an IDR");
    }

    #[test]
    fn repeated_recovery_requests_do_not_force_a_long_idr_burst() {
        let mut enc = H264Encoder::with_frame_rate(2000, 60).unwrap();
        let mut frame = ramp(64, 48, 0);
        enc.encode(&frame).unwrap();
        for _ in 0..100 {
            enc.request_keyframe();
        }
        frame.pts = Timestamp(16_667);
        assert!(enc.encode(&frame).unwrap()[0].is_keyframe);
        frame.pts = Timestamp(33_333);
        assert!(!enc.encode(&frame).unwrap()[0].is_keyframe);
    }

    #[test]
    fn bitrate_reconfiguration_recovers_with_a_decodable_idr() {
        let mut enc = H264Encoder::with_frame_rate(2000, 60).unwrap();
        let mut dec = H264Decoder::new().unwrap();
        let mut frame = ramp(128, 96, 0);
        dec.decode(&enc.encode(&frame).unwrap()[0])
            .unwrap()
            .unwrap();
        assert!(enc.set_bitrate(0).is_err());
        enc.set_bitrate(500).unwrap();
        frame.pts = Timestamp(50_000);
        let changed = enc.encode(&frame).unwrap();
        assert!(changed[0].is_keyframe);
        assert_eq!(changed[0].pts, frame.pts);
        assert!(dec.decode(&changed[0]).unwrap().is_some());
        assert!(H264Encoder::with_frame_rate(2000, 120).is_ok());
        assert!(H264Encoder::with_frame_rate(2000, 241).is_err());
    }

    #[test]
    fn configured_bitrate_changes_the_actual_emitted_stream_budget() {
        fn run(kbps: u32) -> (usize, usize) {
            let mut enc = H264Encoder::with_frame_rate(kbps, 60).unwrap();
            let mut dec = H264Decoder::new().unwrap();
            let mut seed = 1u32;
            let (mut bytes, mut frames) = (0, 0);
            for index in 0..120u64 {
                let mut frame = ramp(160, 96, 0);
                let mut pixels = frame.data.to_vec();
                let data = &mut pixels;
                for value in &mut data[..160 * 96] {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    *value = (seed >> 24) as u8;
                }
                frame.data = pixels.into();
                frame.pts = Timestamp(index * 1_000_000 / 60);
                for encoded in enc.encode(&frame).unwrap() {
                    bytes += encoded.data.len();
                    frames += 1;
                    assert!(dec.decode(&encoded).unwrap().is_some());
                }
            }
            (bytes, frames)
        }
        let low = run(64);
        let high = run(1500);
        assert!(low.0 * 2 < high.0, "low={low:?}, high={high:?}");
        assert!(low.1 > 0 && high.1 > 0);
    }

    #[cfg(all(target_os = "linux", feature = "vaapi"))]
    #[test]
    fn encoder_sps_declares_the_full_601_conversion_it_actually_uses() {
        let mut enc = H264Encoder::new(2000).unwrap();
        let encoded = enc.encode(&ramp(64, 48, 0)).unwrap();
        assert_eq!(
            crate::color::from_h264(&encoded[0].data),
            Some(crate::color::YuvColor::Full601)
        );
    }
}

#[cfg(test)]
mod encoder_bounds_tests {
    use super::*;
    #[test]
    fn unsupported_desktop_size_is_rejected_before_pixel_conversion() {
        for (w,h) in [(2,2),(1920,1080),(3840,2160),(2160,3840)] {
            assert!(H264Encoder::validate_dimensions(w,h).is_ok());
        }
        let mut encoder=H264Encoder::with_frame_rate(8000,60).unwrap();
        for (width,height) in [(4096,2160),(5120,1440),(3840,2400),(1921,1080),(0,0)] {
            let frame=RawFrame{width,height,format:crate::frame::PixelFormat::Bgra8,stride:width.saturating_mul(4),data:Bytes::new(),pts:crate::Timestamp(0),gpu:false,dma_buf:None,d3d11:None};
            let error=encoder.encode(&frame).err().expect("unsupported software size");
            assert!(error.to_string().contains("3840x2160"),"size must fail before accessing pixels: {error}");
        }
    }
}
