//! Media pipeline.
//!
//! The pipeline is split into a *producer* (host side: capture -> encode) and
//! a *consumer* (controller side: decode -> present), mirroring how the
//! official client separates `GameViewerServer` (host) from the controller UI.
//!
//! The official media plane is standard WebRTC with H.264/HEVC video and
//! Opus audio (see `docs/protocol-notes.md`), so the codec layer here is
//! deliberately codec-agnostic and pluggable.

pub mod codec;
pub mod convert;
pub mod frame;
#[cfg(feature = "h264")]
pub mod h264;

pub use codec::{CodecError, NullEncoder, VideoDecoder, VideoEncoder};
pub use frame::{EncodedFrame, PixelFormat, RawFrame};

#[cfg(feature = "h264")]
pub use h264::{H264Decoder, H264Encoder};

/// Build the default software encoder for this build.
pub fn default_encoder(bitrate_kbps: u32) -> Result<Box<dyn VideoEncoder>, CodecError> {
    #[cfg(feature = "h264")]
    let encoder: Box<dyn VideoEncoder> = Box::new(H264Encoder::new(bitrate_kbps)?);
    #[cfg(not(feature = "h264"))]
    let encoder: Box<dyn VideoEncoder> = {
        let _ = bitrate_kbps;
        Box::new(NullEncoder::new(uu_proto::media::VideoCodec::H264))
    };
    Ok(encoder)
}

/// Build the default decoder for this build.
///
/// In-process OpenH264 software decode. Benchmarks: ~2.9 ms/frame for 1080p
/// (~340 fps headroom), which is ample for 60 fps streaming and avoids the
/// latency of any out-of-process decoder. Hardware decode (Windows Media
/// Foundation / Linux VA-API) is a planned in-process backend.
pub fn default_decoder() -> Result<Box<dyn VideoDecoder>, CodecError> {
    #[cfg(feature = "h264")]
    let decoder: Box<dyn VideoDecoder> = Box::new(H264Decoder::new()?);
    #[cfg(not(feature = "h264"))]
    let decoder: Box<dyn VideoDecoder> = {
        return Err(CodecError::Unsupported(uu_proto::media::VideoCodec::H264));
    };
    Ok(decoder)
}

/// Human-readable name of the decoder backend (for `uu probe` and the HUD).
pub fn decoder_backend_name() -> String {
    "openh264 (software, in-process)".to_string()
}

/// Monotonic microsecond timestamp used across capture/encode/decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(pub u64);

impl Timestamp {
    pub fn as_micros(self) -> u64 {
        self.0
    }
}

/// Mean squared error of the luma plane between two same-sized I420 frames.
///
/// Used by the self-loop quality gate: an encode/decode round trip must stay
/// close to the source, so a regression (e.g. feeding the encoder the wrong
/// colour plane) shows up as a test failure rather than a black picture.
pub fn luma_mse(a: &RawFrame, b: &RawFrame) -> Option<f64> {
    if a.width != b.width || a.height != b.height {
        return None;
    }
    let n = (a.width * a.height) as usize;
    let (pa, pb) = (a.data.as_ref(), b.data.as_ref());
    if pa.len() < n || pb.len() < n {
        return None;
    }
    let sum: f64 = pa[..n]
        .iter()
        .zip(&pb[..n])
        .map(|(x, y)| {
            let d = *x as f64 - *y as f64;
            d * d
        })
        .sum();
    Some(sum / n as f64)
}

/// Peak signal-to-noise ratio in dB, given a mean squared error.
pub fn psnr_from_mse(mse: f64) -> f64 {
    if mse <= 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0f64 * 255.0 / mse).log10()
}
