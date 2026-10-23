//! Encoder/decoder traits.

use crate::frame::{EncodedFrame, RawFrame};
use uu_proto::media::VideoCodec;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("codec {0:?} is not supported by this build")]
    Unsupported(VideoCodec),
    #[error("backend failure: {0}")]
    Backend(String),
    #[error("no more frames")]
    EndOfStream,
}

/// Host-side encoder.
pub trait VideoEncoder: Send {
    fn is_hardware(&self)->bool {false}
    fn backend_name(&self) -> &str { "video encoder" }
    /// Accept immutable D3D11 BGRA surfaces, including GPU output scaling.
    fn gpu_input(&self)->bool {false}
    fn gpu_vendor(&self)->Option<u16> {None}
    fn codec(&self) -> VideoCodec;
    /// Encode one frame. May return zero encoded frames while the encoder is
    /// still filling its GOP.
    fn encode(&mut self, frame: &RawFrame) -> Result<Vec<EncodedFrame>, CodecError>;
    /// Force the next frame to be a keyframe (used after a controller joins).
    fn request_keyframe(&mut self);
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError>;
}

/// Controller-side decoder.
pub trait VideoDecoder: Send {
    fn codec(&self) -> VideoCodec;
    /// Prepare a host-confirmed geometry before video starts. Backends that
    /// require the actual bitstream can defer setup until the first keyframe.
    fn prepare(&mut self, _width: u32, _height: u32) -> Result<(), CodecError> {
        Ok(())
    }
    /// Feed one access unit, producing a decoded frame when the decoder has
    /// enough data (B-frames and reordering mean this is not 1:1).
    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<RawFrame>, CodecError>;
    /// Submit the access unit once and collect every picture currently ready.
    /// A driver may return no picture, then several pictures on a later input.
    fn decode_frames(&mut self, frame: &EncodedFrame) -> Result<Vec<RawFrame>, CodecError> {
        Ok(self.decode(frame)?.into_iter().collect())
    }
    /// Human-readable name of the backend actually in use.
    ///
    /// Surfaced in the HUD and in `uu probe` so a session always shows whether
    /// it is on a hardware path, on the software fallback, or demoted from
    /// hardware at runtime (see [`crate::decoder::FallbackDecoder`]).
    fn backend_name(&self) -> &str {
        "video decoder"
    }
}

/// A software stand-in so the rest of the stack can be exercised before the
/// hardware backends land. It never produces real bitstreams.
pub struct NullEncoder {
    codec: VideoCodec,
}

impl NullEncoder {
    pub fn new(codec: VideoCodec) -> Self {
        Self { codec }
    }
}

impl VideoEncoder for NullEncoder {
    fn codec(&self) -> VideoCodec {
        self.codec
    }

    fn encode(&mut self, _frame: &RawFrame) -> Result<Vec<EncodedFrame>, CodecError> {
        Ok(Vec::new())
    }

    fn request_keyframe(&mut self) {}

    fn set_bitrate(&mut self, _kbps: u32) -> Result<(), CodecError> {
        Ok(())
    }
}
