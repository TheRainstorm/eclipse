//! H.264 codec built on Cisco OpenH264 (`openh264` crate).
//!
//! OpenH264 is a software codec, so this works on any platform with no GPU
//! and no system libraries — which is exactly what the M2 self-loop needs.
//! Hardware paths (NVENC/QSV/VA-API) slot in behind the same traits later.

use bytes::Bytes;
use openh264::decoder::{Decoder as OhDecoder, DecoderConfig, Flush};
use openh264::encoder::Encoder as OhEncoder;
use openh264::formats::{YUVBuffer, YUVSource};
use openh264::OpenH264API;
use crate::codec::{CodecError, VideoDecoder, VideoEncoder};
use crate::convert::to_i420;
use crate::frame::{EncodedFrame, RawFrame};
use uu_proto::media::VideoCodec;

fn backend_err<E: std::fmt::Display>(e: E) -> CodecError {
    CodecError::Backend(e.to_string())
}

pub struct H264Encoder {
    inner: OhEncoder,
    /// Forced-IDR counter: `request_keyframe` bumps it, `encode` consumes it.
    pending_keyframes: u32,
    bitrate_kbps: u32,
    frames: u64,
}

impl H264Encoder {
    pub fn new(bitrate_kbps: u32) -> Result<Self, CodecError> {
        let inner = OhEncoder::new().map_err(backend_err)?;
        Ok(Self { inner, pending_keyframes: 1, bitrate_kbps, frames: 0 })
    }

    pub fn frames_encoded(&self) -> u64 {
        self.frames
    }
}

impl VideoEncoder for H264Encoder {
    fn codec(&self) -> VideoCodec {
        VideoCodec::H264
    }

    fn encode(&mut self, frame: &RawFrame) -> Result<Vec<EncodedFrame>, CodecError> {
        if self.pending_keyframes > 0 {
            self.inner.force_intra_frame();
            self.pending_keyframes -= 1;
        }
        let packed = to_i420(frame)?;
        let yuv = YUVBuffer::from_vec(packed, frame.width as usize, frame.height as usize);
        let bitstream = self.inner.encode(&yuv).map_err(backend_err)?;
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
        self.pending_keyframes += 1;
    }

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError> {
        // OpenH264 fixes the rate at initialization; changing it means
        // rebuilding the encoder. Record the intent and let the session
        // supervisor decide (documented, not silently ignored).
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
        let mut inner = OhDecoder::with_api_config(OpenH264API::from_source(), config)
            .map_err(backend_err)?;
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
        }))
    }
}

/// Annex-B scan: if the access unit carries an SPS, return a copy with
/// `gaps_in_frame_num_value_allowed_flag` set to 1. Returns `None` when there
/// is nothing to change (no SPS, already allowed, or an SPS layout this simple
/// parser does not understand).
fn patch_sps_allow_gaps(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut changed = false;
    let mut i = 0usize;
    while i < data.len() {
        let (sc, body) = if data[i..].starts_with(&[0, 0, 0, 1]) {
            (4, i + 4)
        } else if data[i..].starts_with(&[0, 0, 1]) {
            (3, i + 3)
        } else {
            out.push(data[i]);
            i += 1;
            continue;
        };
        let _ = sc;
        let mut j = body;
        while j < data.len() {
            if data[j..].starts_with(&[0, 0, 0, 1]) || data[j..].starts_with(&[0, 0, 1]) {
                break;
            }
            j += 1;
        }
        if body < data.len() && (data[body] & 0x1f) == 7 {
            if let Some(nal) = patch_sps_nal(&data[body..j]) {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(&nal);
                changed = true;
                i = j;
                continue;
            }
        }
        out.extend_from_slice(&data[i..j]);
        i = j;
    }
    if changed {
        Some(out)
    } else {
        None
    }
}

fn unescape_nal(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut zeros = 0;
    for &x in b {
        if zeros >= 2 && x == 3 {
            zeros = 0;
            continue;
        }
        out.push(x);
        zeros = if x == 0 { zeros + 1 } else { 0 };
    }
    out
}

fn escape_nal(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len() + 4);
    let mut zeros = 0;
    for &x in b {
        if zeros >= 2 && x <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(x);
        zeros = if x == 0 { zeros + 1 } else { 0 };
    }
    out
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl BitReader<'_> {
    fn u(&mut self, n: usize) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self.pos / 8;
            if byte >= self.data.len() {
                return None;
            }
            let bit = (self.data[byte] >> (7 - (self.pos % 8))) & 1;
            v = (v << 1) | bit as u32;
            self.pos += 1;
        }
        Some(v)
    }

    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0u32;
        while self.u(1)? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let suffix = if zeros > 0 { self.u(zeros as usize)? } else { 0 };
        Some(((1u32 << zeros) - 1).wrapping_add(suffix))
    }

    fn se(&mut self) -> Option<i32> {
        let ue = self.ue()? as i64;
        let v = if ue & 1 == 1 { (ue + 1) / 2 } else { -(ue / 2) };
        Some(v as i32)
    }
}

/// Bit offset (from the start of the RBSP) of the gaps flag, or `None` when
/// the SPS uses a layout we do not parse.
fn sps_gaps_bit_offset(rbsp: &[u8]) -> Option<usize> {
    let mut r = BitReader { data: rbsp, pos: 0 };
    let profile = r.u(8)?;
    r.u(8)?;
    r.u(8)?;
    r.ue()?;
    if matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        let chroma = r.ue()?;
        if chroma == 3 {
            r.u(1)?;
        }
        r.ue()?;
        r.ue()?;
        r.u(1)?;
        if r.u(1)? == 1 {
            return None; // scaling matrices: not worth parsing here
        }
    }
    r.ue()?;
    match r.ue()? {
        0 => {
            r.ue()?;
        }
        1 => {
            r.u(1)?;
            r.se()?;
            r.se()?;
            let n = r.ue()?;
            for _ in 0..n {
                r.se()?;
            }
        }
        _ => {}
    }
    r.ue()?;
    Some(r.pos)
}

/// Set `gaps_in_frame_num_value_allowed_flag` in one SPS NAL (header + EBSP).
fn patch_sps_nal(nal: &[u8]) -> Option<Vec<u8>> {
    if nal.is_empty() {
        return None;
    }
    let rbsp = unescape_nal(&nal[1..]);
    let bit = sps_gaps_bit_offset(&rbsp)?;
    let byte = bit / 8;
    let shift = 7 - (bit % 8);
    if byte >= rbsp.len() {
        return None;
    }
    if rbsp[byte] & (1 << shift) != 0 {
        return None; // already allowed
    }
    let mut patched = rbsp;
    patched[byte] |= 1 << shift;
    let ebsp = escape_nal(&patched);
    let mut out = Vec::with_capacity(1 + ebsp.len());
    out.push(nal[0]);
    out.extend_from_slice(&ebsp);
    Some(out)
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

    /// A real SPS captured from the UU controlled end (Main profile, 1080p),
    /// which has `gaps_in_frame_num_value_allowed_flag = 0` while the encoder
    /// actually drops frames.
    const UU_SPS: &[u8] = &[
        0x67, 0x4d, 0x04, 0x2a, 0xe8, 0xa0, 0x3c, 0x01, 0x13, 0xf2, 0xff, 0xe0, 0x00, 0x20,
        0x00, 0x28, 0x40, 0x40, 0x40, 0x50, 0x00, 0x00, 0x03, 0x00, 0x10, 0x00, 0x00, 0x03,
        0x03, 0xc8, 0xf0, 0x88, 0x46, 0x58,
    ];

    fn gaps_flag(rbsp: &[u8]) -> u8 {
        let bit = sps_gaps_bit_offset(rbsp).unwrap();
        (rbsp[bit / 8] >> (7 - (bit % 8))) & 1
    }

    #[test]
    fn patching_sets_gaps_flag() {
        let rbsp = unescape_nal(&UU_SPS[1..]);
        assert_eq!(gaps_flag(&rbsp), 0, "fixture starts with the flag clear");

        let mut au = vec![0u8, 0, 0, 1];
        au.extend_from_slice(UU_SPS);
        let patched = patch_sps_allow_gaps(&au).expect("SPS should be rewritten");
        let nal = &patched[4..];
        let rbsp = unescape_nal(&nal[1..]);
        assert_eq!(gaps_flag(&rbsp), 1, "flag must be set after patching");
    }

    #[test]
    fn patching_is_a_noop_for_non_sps_access_units() {
        // AUD + non-IDR slice: no SPS, nothing to change.
        let au = [0u8, 0, 0, 1, 0x09, 0x30, 0, 0, 0, 1, 0x41, 0x9a, 0x11];
        assert!(patch_sps_allow_gaps(&au).is_none());
    }
}
