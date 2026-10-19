//! Frame representations.

use bytes::Bytes;

use crate::Timestamp;

/// Packed pixel formats we handle before handing frames to an encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// 8-bit BGRA, the DXGI/GPU-native layout on Windows (and GDI capture).
    Bgra8,
    /// Semi-planar YUV 4:2:0 (NV12); what VA-API/QSV and DXGI video produce.
    Nv12,
    /// Planar YUV 4:2:0 (I420); the layout OpenH264 consumes.
    I420,
}

/// A captured but not-yet-encoded frame.
#[derive(Debug, Clone)]
pub struct RawFrame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// Bytes per row of the *first* plane. Not `width * bpp` in general:
    /// capture APIs align rows to 64/256 bytes.
    pub stride: u32,
    pub data: Bytes,
    pub pts: Timestamp,
    /// True when `data` is an opaque GPU handle rather than CPU memory.
    /// Zero-copy paths set this and convert lazily.
    pub gpu: bool,
}

impl RawFrame {
    /// A CPU-side I420 frame, the common currency of the encoder path.
    pub fn new_i420(width: u32, height: u32, data: Vec<u8>, pts: Timestamp) -> Self {
        Self {
            width,
            height,
            format: PixelFormat::I420,
            stride: width,
            data: Bytes::from(data),
            pts,
            gpu: false,
        }
    }

    /// Tightly packed byte length for the frame's format.
    pub fn expected_len(&self) -> usize {
        let stride = self.stride as usize;
        let height = self.height as usize;
        match self.format {
            PixelFormat::Bgra8 => stride * height,
            // Both 4:2:0 layouts are luma + two half-resolution chroma planes.
            PixelFormat::Nv12 | PixelFormat::I420 => stride * height * 3 / 2,
        }
    }

    /// Validate that the buffer can actually hold a full frame.
    pub fn is_consistent(&self) -> bool {
        self.data.len() >= self.expected_len()
    }
}

/// One encoded access unit (e.g. an H.264 NAL chain).
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub data: Bytes,
    pub pts: Timestamp,
    pub is_keyframe: bool,
}

impl EncodedFrame {
    /// H.264 start-code scan used to tag IDR frames.
    ///
    /// The controller uses `is_keyframe` to decide whether it can join
    /// immediately or must wait for the next IDR.
    pub fn detect_h264_keyframe(data: &[u8]) -> bool {
        let mut i = 0usize;
        while i + 3 < data.len() {
            let sc = if data[i..].starts_with(&[0, 0, 0, 1]) {
                4
            } else if data[i..].starts_with(&[0, 0, 1]) {
                3
            } else {
                i += 1;
                continue;
            };
            let header_at = i + sc;
            if header_at < data.len() {
                let nal_type = data[header_at] & 0x1f;
                // SPS/PPS alone cannot repair a broken reference chain. Only
                // an IDR is a safe join point after packet loss or queue drops.
                if nal_type == 5 {
                    return true;
                }
            }
            i = header_at;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv12_size_is_one_and_a_half_planes() {
        let f = RawFrame {
            width: 4,
            height: 4,
            format: PixelFormat::Nv12,
            stride: 4,
            data: Bytes::from(vec![0u8; 4 * 4 * 3 / 2]),
            pts: Timestamp(0),
            gpu: false,
        };
        assert_eq!(f.expected_len(), 24);
        assert!(f.is_consistent());
    }

    #[test]
    fn idr_detection_finds_nal_type_5() {
        // Annex-B: 00 00 00 01 then NAL header with type 5 (IDR).
        let data = [0u8, 0, 0, 1, 0x65, 0x88, 0x84];
        assert!(EncodedFrame::detect_h264_keyframe(&data));
    }

    #[test]
    fn idr_detection_rejects_non_idr() {
        // NAL type 1 = non-IDR slice.
        let data = [0u8, 0, 0, 1, 0x41, 0x9a, 0x11];
        assert!(!EncodedFrame::detect_h264_keyframe(&data));
    }

    #[test]
    fn three_byte_start_code_is_honoured() {
        let data = [0u8, 0, 1, 0x65, 0x42]; // type 5 = IDR
        assert!(EncodedFrame::detect_h264_keyframe(&data));
    }

    #[test]
    fn parameter_sets_are_not_reference_recovery_points() {
        assert!(!EncodedFrame::detect_h264_keyframe(&[0,0,1,0x67,0x42,0,0,1,0x68,0xce]));
    }
}
