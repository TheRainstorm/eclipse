//! HEVC Main 8-bit metadata using the existing ChromiumOS parser (BSD-3-Clause).
//! Parsing stays independent of the platform decoder and its GPU backend.
use crate::color::YuvColor;
use cros_codecs::codec::h265::parser::{Nalu, NaluType, Parser};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sequence {
    pub width: u32,
    pub height: u32,
    pub color: YuvColor,
}

/// Return SPS geometry and VUI only for the negotiated 8-bit 4:2:0 format.
pub fn sequence(data: &[u8]) -> Option<Sequence> {
    let mut cursor = std::io::Cursor::new(data);
    while let Ok(nal) = Nalu::next(&mut cursor) {
        if !nal.as_ref().first().is_some_and(|b| b & 0x80 == 0)
            || nal.header.nuh_temporal_id_plus1 == 0
        {
            return None;
        }
        if nal.header.type_ != NaluType::SpsNut {
            continue;
        }
        if nal.size > 64 * 1024 {
            return None;
        }
        let mut parser = Parser::default();
        let sps = parser.parse_sps(&nal).ok()?;
        if sps.chroma_format_idc != 1
            || sps.bit_depth_luma_minus8 != 0
            || sps.bit_depth_chroma_minus8 != 0
        {
            return None;
        }
        let width = u32::from(sps.width()).checked_sub(
            sps.conf_win_left_offset
                .checked_add(sps.conf_win_right_offset)?
                .checked_mul(2)?,
        )?;
        let height = u32::from(sps.height()).checked_sub(
            sps.conf_win_top_offset
                .checked_add(sps.conf_win_bottom_offset)?
                .checked_mul(2)?,
        )?;
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return None;
        }
        let vui = &sps.vui_parameters;
        // Unspecified matrix follows the HD convention; explicit 601/709 and
        // full/limited range are retained for the shared CPU/GPU color path.
        let bt709 = !matches!(vui.matrix_coeffs, 5 | 6);
        let color = match (vui.video_full_range_flag, bt709) {
            (true, true) => YuvColor::Full709,
            (false, true) => YuvColor::Limited709,
            (true, false) => YuvColor::Full601,
            (false, false) => YuvColor::Limited601,
        };
        return Some(Sequence {
            width,
            height,
            color,
        });
    }
    None
}

/// IRAP pictures reset references; CRA leading RASL pictures are discarded by
/// the HEVC decoder when joining at random access.
pub fn is_keyframe(data: &[u8]) -> bool {
    let mut cursor = std::io::Cursor::new(data);
    while let Ok(nal) = Nalu::next(&mut cursor) {
        if nal.as_ref().len() < 3
            || nal.as_ref()[0] & 0x80 != 0
            || nal.header.nuh_temporal_id_plus1 == 0
        {
            return false;
        }
        if nal.header.type_.is_irap() {
            return true;
        }
    }
    false
}

/// A generated gray 128x128, 60 Hz Main/8-bit stream (four access units).
/// ffmpeg color source, libx265 ultrafast/zerolatency, aud=1, repeat-headers=1,
/// keyint=60, BT.709 limited. Encoder identification SEI was removed.
pub const PROBE: &[u8] = include_bytes!("hevc_probe.h265");

/// Validate actual decode output before advertising HEVC. `prepare` alone can
/// succeed even when the installed MFT cannot decode on this device. The HEVC
/// backend rejects system-memory MFT output, including while CPU rendering is
/// selected, so this probe also verifies D3D11 surfaces without global changes.
pub fn probe_decoder() -> Result<(), crate::CodecError> {
    let mut decoder = crate::decoder_for_codec(uu_proto::media::VideoCodec::Hevc)?;
    let mut starts = Vec::new();
    for i in 0..PROBE.len().saturating_sub(4) {
        if PROBE[i..].starts_with(&[0, 0, 0, 1, 70]) {
            starts.push(i);
        }
    }
    starts.push(PROBE.len());
    let mut produced = 0;
    for (i, span) in starts.windows(2).enumerate() {
        let data = &PROBE[span[0]..span[1]];
        if let Some(frame) = decoder.decode(&crate::EncodedFrame {
            is_keyframe: is_keyframe(data),
            data: bytes::Bytes::copy_from_slice(data),
            pts: crate::Timestamp(i as u64 * 16_667),
        })? {
            if frame.width != 128 || frame.height != 128 || !frame.is_consistent() {
                return Err(crate::CodecError::Backend(
                    "HEVC probe output is invalid".into(),
                ));
            }
            produced += 1;
        }
    }
    if produced < 2 {
        return Err(crate::CodecError::Backend(
            "HEVC probe produced too few frames".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_main_stream_has_geometry_color_and_a_real_irap() {
        assert_eq!(
            sequence(PROBE),
            Some(Sequence {
                width: 128,
                height: 128,
                color: YuvColor::Limited709
            })
        );
        assert!(is_keyframe(PROBE));
        assert!(!is_keyframe(&[0, 0, 1, 0x42, 1, 0])); // SPS is not recovery.
        assert!(!is_keyframe(&[0, 0, 1, 2, 1, 0])); // TRAIL_R.
        for kind in [16, 17, 18, 19, 20, 21] {
            assert!(is_keyframe(&[0, 0, 1, kind << 1, 1, 0x80]));
        }
    }
    #[test]
    fn truncated_headers_never_panic_or_invent_a_valid_geometry() {
        assert_eq!(sequence(&[]), None);
        for len in 0..PROBE.len() {
            let _ = sequence(&PROBE[..len]);
            let _ = is_keyframe(&PROBE[..len]);
        }
        for header in 0..=255 {
            let bad = [0, 0, 1, header, 0];
            assert_eq!(sequence(&bad), None);
            assert!(!is_keyframe(&bad));
        }
    }
    #[cfg(windows)]
    #[test]
    #[ignore = "requires installed HEVC MFT and a compatible GPU"]
    fn windows_hevc_advertisement_requires_actual_gpu_decode() {
        probe_decoder().unwrap();
    }
}
