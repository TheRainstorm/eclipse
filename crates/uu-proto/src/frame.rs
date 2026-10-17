//! Length-prefixed framing.
//!
//! The control channel is a stream of `u32` little-endian length-prefixed
//! payloads. `FrameCodec` implements the encode/decode halves against a
//! `BytesMut`, mirroring how tokio-util's `LengthDelimitedCodec` would be
//! used, but without the extra dependency.

use bytes::{Buf, BufMut, Bytes, BytesMut};

/// Hard cap on a single control frame. The official client's control
/// messages are small (JSON/CBOR-sized); anything larger is a protocol
/// violation or a desync, so we fail loudly instead of allocating.
pub const MAX_FRAME_LEN: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame(pub Bytes);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("frame of {0} bytes exceeds the {MAX_FRAME_LEN} byte limit")]
    TooLarge(usize),
    #[error("stream is empty")]
    Incomplete,
}

pub struct FrameCodec;

impl FrameCodec {
    /// Encode one frame into `dst`.
    pub fn encode(frame: &Frame, dst: &mut BytesMut) -> Result<(), FrameError> {
        if frame.0.len() > MAX_FRAME_LEN {
            return Err(FrameError::TooLarge(frame.0.len()));
        }
        dst.reserve(4 + frame.0.len());
        dst.put_u32_le(frame.0.len() as u32);
        dst.put_slice(&frame.0);
        Ok(())
    }

    /// Try to decode one frame from `src`.
    ///
    /// Returns `Ok(None)` when more bytes are required. On success the
    /// consumed bytes are removed from `src`.
    pub fn decode(src: &mut BytesMut) -> Result<Option<Frame>, FrameError> {
        if src.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes([src[0], src[1], src[2], src[3]]) as usize;
        if len > MAX_FRAME_LEN {
            return Err(FrameError::TooLarge(len));
        }
        if src.len() < 4 + len {
            return Ok(None);
        }
        src.advance(4);
        let payload = src.split_to(len).freeze();
        Ok(Some(Frame(payload)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_single() {
        let mut buf = BytesMut::new();
        FrameCodec::encode(&Frame(Bytes::from_static(b"hello")), &mut buf).unwrap();
        let got = FrameCodec::decode(&mut buf).unwrap().unwrap();
        assert_eq!(got.0, Bytes::from_static(b"hello"));
        assert!(buf.is_empty());
    }

    #[test]
    fn partial_frame_needs_more() {
        let mut buf = BytesMut::new();
        FrameCodec::encode(&Frame(Bytes::from_static(b"abcdef")), &mut buf).unwrap();
        let mut partial = buf.split_to(5);
        assert_eq!(FrameCodec::decode(&mut partial).unwrap(), None);
    }

    #[test]
    fn two_frames_back_to_back() {
        let mut buf = BytesMut::new();
        for s in [&b"a"[..], &b"bb"[..]] {
            FrameCodec::encode(&Frame(Bytes::copy_from_slice(s)), &mut buf).unwrap();
        }
        assert_eq!(FrameCodec::decode(&mut buf).unwrap().unwrap().0, Bytes::from_static(b"a"));
        assert_eq!(FrameCodec::decode(&mut buf).unwrap().unwrap().0, Bytes::from_static(b"bb"));
        assert_eq!(FrameCodec::decode(&mut buf).unwrap(), None);
    }

    #[test]
    fn rejects_oversized() {
        let mut buf = BytesMut::new();
        buf.put_u32_le(u32::MAX);
        buf.put_slice(&[0u8; 8]);
        assert!(matches!(FrameCodec::decode(&mut buf), Err(FrameError::TooLarge(_))));
    }
}
