//! H.264 bitstream surgery shared by every decoder backend.
//!
//! NetEase's encoder drops frames for rate control while leaving
//! `gaps_in_frame_num_value_allowed_flag` clear in the SPS. Decoders that
//! honour the flag (OpenH264, and the Media Foundation H.264 MFT) then treat
//! the gap as a fatal reference loss and stall until the next IDR. Both the
//! software and the hardware paths therefore run access units through
//! [`patch_sps_allow_gaps`] before handing them to a decoder.
//!
//! This module is deliberately not feature-gated: it needs no codec library,
//! so a build with hardware decode but no OpenH264 can still repair streams.

const MAX_SPS_BYTES: usize = 64 * 1024;

/// Annex-B scan: if the access unit carries an SPS, return a copy with
/// `gaps_in_frame_num_value_allowed_flag` set to 1. Returns `None` when there
/// is nothing to change (no SPS, already allowed, or an SPS layout this simple
/// parser does not understand).
pub fn patch_sps_allow_gaps(data: &[u8]) -> Option<Vec<u8>> {
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
        let suffix = if zeros > 0 {
            self.u(zeros as usize)?
        } else {
            0
        };
        Some(((1u32 << zeros) - 1).wrapping_add(suffix))
    }

    fn se(&mut self) -> Option<i32> {
        let ue = self.ue()? as i64;
        let v = if ue & 1 == 1 { (ue + 1) / 2 } else { -(ue / 2) };
        Some(v as i32)
    }
}

/// Parse the shared prefix once for gap repair, dimensions and colour metadata.
fn sps_prefix(rbsp: &[u8]) -> Option<(BitReader<'_>, u32)> {
    let mut r = BitReader { data: rbsp, pos: 0 };
    let profile = r.u(8)?;
    r.u(8)?;
    r.u(8)?;
    r.ue()?;
    let mut chroma = 1;
    if matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        chroma = r.ue()?;
        if chroma > 3 {
            return None;
        }
        let separate = chroma == 3 && r.u(1)? != 0;
        r.ue()?;
        r.ue()?;
        r.u(1)?;
        if r.u(1)? != 0 {
            for index in 0..if chroma == 3 { 12 } else { 8 } {
                if r.u(1)? != 0 {
                    let mut last = 8i32;
                    let mut next = 8i32;
                    for _ in 0..if index < 6 { 16 } else { 64 } {
                        if next != 0 {
                            let delta = r.se()?;
                            if !(-128..=127).contains(&delta) {
                                return None;
                            }
                            next = (last + delta).rem_euclid(256);
                        }
                        if next != 0 {
                            last = next;
                        }
                    }
                }
            }
        }
        if separate {
            chroma = 0;
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
            let count = r.ue()?;
            if count > 255 {
                return None;
            }
            for _ in 0..count {
                r.se()?;
            }
        }
        2 => {}
        _ => return None,
    }
    r.ue()?;
    Some((r, chroma))
}

/// Bit offset of gaps_in_frame_num_value_allowed_flag in the SPS RBSP.
fn sps_gaps_bit_offset(rbsp: &[u8]) -> Option<usize> {
    Some(sps_prefix(rbsp)?.0.pos)
}

fn sps_geometry<'a>(mut r: BitReader<'a>, chroma: u32) -> Option<(BitReader<'a>, u32, u32)> {
    r.u(1)?;
    let width_mbs = r.ue()?.checked_add(1)?;
    let height_units = r.ue()?.checked_add(1)?;
    let frame_factor = if r.u(1)? == 1 {
        1
    } else {
        r.u(1)?;
        2
    };
    r.u(1)?;
    let crop = if r.u(1)? == 1 {
        [r.ue()?, r.ue()?, r.ue()?, r.ue()?]
    } else {
        [0; 4]
    };
    let (crop_x, crop_y) = match chroma {
        0 | 3 => (1, frame_factor),
        1 => (2, 2 * frame_factor),
        2 => (2, frame_factor),
        _ => return None,
    };
    let width = width_mbs
        .checked_mul(16)?
        .checked_sub(crop[0].checked_add(crop[1])?.checked_mul(crop_x)?)?;
    let height = height_units
        .checked_mul(16)?
        .checked_mul(frame_factor)?
        .checked_sub(crop[2].checked_add(crop[3])?.checked_mul(crop_y)?)?;
    (width > 0 && height > 0).then_some((r, width, height))
}

/// Range and matrix from H.264 VUI, independent of hardware/OS features.
/// An SPS without video_signal_type uses limited range, with BT.601 fallback.
pub(crate) fn h264_video_signal(data: &[u8]) -> Option<(bool, u32)> {
    let mut cursor = 0;
    while cursor < data.len() {
        let start = data[cursor..].windows(3).position(|v| v == [0, 0, 1])?;
        let body = cursor + start + 3;
        let header = *data.get(body)?;
        let end = data[body..]
            .windows(3)
            .position(|v| v == [0, 0, 1])
            .map(|next| body + next)
            .unwrap_or(data.len());
        if header & 0x1f == 7 {
            if header & 0x80 != 0 || end - body > MAX_SPS_BYTES {
                return None;
            }
            let rbsp = unescape_nal(&data[body + 1..end]);
            let (r, chroma) = sps_prefix(&rbsp)?;
            let (mut r, _, _) = sps_geometry(r, chroma)?;
            if r.u(1)? == 0 {
                return Some((false, 6));
            }
            if r.u(1)? != 0 {
                if r.u(8)? == 255 {
                    r.u(16)?;
                    r.u(16)?;
                }
            }
            if r.u(1)? != 0 {
                r.u(1)?;
            }
            if r.u(1)? == 0 {
                return Some((false, 6));
            }
            r.u(3)?;
            let full = r.u(1)? != 0;
            let matrix = if r.u(1)? != 0 {
                r.u(8)?;
                r.u(8)?;
                r.u(8)?
            } else {
                6
            };
            return Some((full, matrix));
        }
        cursor = end;
    }
    None
}

/// Dimensions of the first SPS found in an Annex-B access unit, honouring
/// cropping. Used by the out-of-process decoder, which needs to know the raw
/// frame size before reading frames from ffmpeg's stdout.
pub fn h264_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let mut i = 0usize;
    while i < data.len() {
        let (sc, body) = if data[i..].starts_with(&[0, 0, 0, 1]) {
            (4, i + 4)
        } else if data[i..].starts_with(&[0, 0, 1]) {
            (3, i + 3)
        } else {
            i += 1;
            continue;
        };
        let _ = sc;
        if body >= data.len() {
            return None;
        }
        if data[body] & 0x1f == 7 {
            // NAL end = next start code (or end of buffer).
            let mut j = body + 1;
            while j < data.len() {
                if data[j..].starts_with(&[0, 0, 0, 1]) || data[j..].starts_with(&[0, 0, 1]) {
                    break;
                }
                j += 1;
            }
            if j - body > MAX_SPS_BYTES {
                return None;
            }
            let rbsp = unescape_nal(&data[body + 1..j]);
            if let Some(dims) = sps_dimensions(&rbsp) {
                return Some(dims);
            }
        }
        i = body;
    }
    None
}

fn sps_dimensions(rbsp: &[u8]) -> Option<(u32, u32)> {
    let (r, chroma) = sps_prefix(rbsp)?;
    let (_, width, height) = sps_geometry(r, chroma)?;
    Some((width, height))
}

/// Set `gaps_in_frame_num_value_allowed_flag` in one SPS NAL (header + EBSP).
fn patch_sps_nal(nal: &[u8]) -> Option<Vec<u8>> {
    if nal.is_empty() || nal.len() > MAX_SPS_BYTES {
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

    #[derive(Default)]
    struct Writer(Vec<bool>);
    impl Writer {
        fn u(&mut self, value: u32, bits: u32) {
            self.0
                .extend((0..bits).rev().map(|shift| value & (1 << shift) != 0));
        }
        fn ue(&mut self, value: u32) {
            let code = u64::from(value) + 1;
            let bits = 64 - code.leading_zeros();
            self.0
                .extend(std::iter::repeat_n(false, (bits - 1) as usize));
            self.0
                .extend((0..bits).rev().map(|shift| code & (1 << shift) != 0));
        }
        fn finish(mut self) -> Vec<u8> {
            self.0.push(true);
            while self.0.len() % 8 != 0 {
                self.0.push(false);
            }
            self.0
                .chunks_exact(8)
                .map(|bits| {
                    bits.iter()
                        .fold(0, |byte, bit| (byte << 1) | u8::from(*bit))
                })
                .collect()
        }
    }

    fn synthetic_sps(
        chroma: u32,
        interlaced: bool,
        crop: [u32; 4],
        scaling: bool,
        cycles: Option<u32>,
        width: u32,
    ) -> Vec<u8> {
        let mut w = Writer::default();
        w.u(100, 8);
        w.u(0, 8);
        w.u(31, 8);
        w.ue(0);
        w.ue(chroma);
        if chroma == 3 {
            w.u(0, 1);
        }
        w.ue(0);
        w.ue(0);
        w.u(0, 1);
        w.u(u32::from(scaling), 1);
        if scaling {
            for index in 0..if chroma == 3 { 12 } else { 8 } {
                w.u(u32::from(index == 0), 1);
                if index == 0 {
                    for _ in 0..16 {
                        w.ue(0);
                    }
                }
            }
        }
        w.ue(0);
        if let Some(count) = cycles {
            w.ue(1);
            w.u(0, 1);
            w.ue(0);
            w.ue(0);
            w.ue(count);
            for _ in 0..count {
                w.ue(0);
            }
        } else {
            w.ue(0);
            w.ue(0);
        }
        w.ue(1);
        w.u(0, 1);
        w.ue(width);
        w.ue(3);
        w.u(u32::from(!interlaced), 1);
        if interlaced {
            w.u(0, 1);
        }
        w.u(1, 1);
        w.u(1, 1);
        for value in crop {
            w.ue(value);
        }
        w.u(1, 1);
        w.u(0, 1);
        w.u(0, 1);
        w.u(1, 1);
        w.u(5, 3);
        w.u(1, 1);
        w.u(1, 1);
        w.u(1, 8);
        w.u(1, 8);
        w.u(1, 8);
        for _ in 0..6 {
            w.u(0, 1);
        }
        let mut data = vec![0, 0, 0, 1, 0x67];
        data.extend(escape_nal(&w.finish()));
        data
    }

    #[test]
    fn scaling_lists_and_poc_cycles_keep_color_dimensions_and_gap_repair_consistent() {
        let data = synthetic_sps(1, false, [0; 4], true, Some(1), 3);
        assert_eq!(h264_dimensions(&data), Some((64, 64)));
        assert_eq!(h264_video_signal(&data), Some((true, 1)));
        let repaired = patch_sps_allow_gaps(&data).unwrap();
        assert_eq!(h264_video_signal(&repaired), Some((true, 1)));
        assert_eq!(h264_dimensions(&repaired), Some((64, 64)));
        assert!(patch_sps_allow_gaps(&repaired).is_none());
    }

    #[test]
    fn crop_units_follow_chroma_sampling_and_interlaced_frame_height() {
        for (chroma, interlaced, expected) in [
            (0, false, (63, 63)),
            (1, false, (62, 62)),
            (1, true, (62, 124)),
            (2, false, (62, 63)),
            (3, false, (63, 63)),
        ] {
            assert_eq!(
                h264_dimensions(&synthetic_sps(
                    chroma,
                    interlaced,
                    [0, 1, 0, 1],
                    false,
                    None,
                    3
                )),
                Some(expected)
            );
        }
    }

    #[test]
    fn malformed_sps_dimensions_and_cycles_are_bounded() {
        for data in [
            synthetic_sps(1, false, [0; 4], false, None, u32::MAX - 1),
            synthetic_sps(1, false, [0, u32::MAX - 1, 0, 0], false, None, 3),
            synthetic_sps(1, false, [0; 4], false, Some(256), 3),
        ] {
            assert_eq!(h264_dimensions(&data), None);
            assert_eq!(h264_video_signal(&data), None);
        }
        let mut data = synthetic_sps(1, false, [0; 4], false, None, 3);
        data.resize(MAX_SPS_BYTES + 5, 0xff);
        assert_eq!(h264_dimensions(&data), None);
        assert_eq!(h264_video_signal(&data), None);
        assert!(patch_sps_allow_gaps(&data).is_none());
    }

    /// A real SPS captured from the UU controlled end (Main profile, 1080p),
    /// which has `gaps_in_frame_num_value_allowed_flag = 0` while the encoder
    /// actually drops frames.
    const UU_SPS: &[u8] = &[
        0x67, 0x4d, 0x04, 0x2a, 0xe8, 0xa0, 0x3c, 0x01, 0x13, 0xf2, 0xff, 0xe0, 0x00, 0x20, 0x00,
        0x28, 0x40, 0x40, 0x40, 0x50, 0x00, 0x00, 0x03, 0x00, 0x10, 0x00, 0x00, 0x03, 0x03, 0xc8,
        0xf0, 0x88, 0x46, 0x58,
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

    #[test]
    fn patching_keeps_other_nals_and_their_start_codes() {
        // SPS (patched) followed by an IDR slice (untouched).
        let mut au = vec![0u8, 0, 0, 1];
        au.extend_from_slice(UU_SPS);
        au.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88, 0x84]);
        let patched = patch_sps_allow_gaps(&au).expect("SPS should be rewritten");
        assert!(patched.ends_with(&[0, 0, 0, 1, 0x65, 0x88, 0x84]));
        assert_eq!(
            unicoded_len(&patched),
            unicoded_len(&au),
            "the patch only flips a bit inside the existing SPS"
        );
        assert_eq!(patched.len(), au.len(), "start codes are preserved too");
    }

    #[test]
    fn dimensions_are_parsed_from_sps_with_cropping() {
        let mut au = vec![0u8, 0, 0, 1];
        au.extend_from_slice(UU_SPS);
        assert_eq!(h264_dimensions(&au), Some((1920, 1080)));
    }

    #[test]
    fn dimensions_are_none_without_sps() {
        let au = [0u8, 0, 0, 1, 0x09, 0x30, 0, 0, 0, 1, 0x41, 0x9a];
        assert_eq!(h264_dimensions(&au), None);
    }

    /// The payload length of an access unit, ignoring start codes: the patch
    /// only flips a bit in the existing SPS, so this must not change.
    fn unicoded_len(au: &[u8]) -> usize {
        let mut total = 0usize;
        let mut i = 0usize;
        while i < au.len() {
            let sc = if au[i..].starts_with(&[0, 0, 0, 1]) {
                4
            } else if au[i..].starts_with(&[0, 0, 1]) {
                3
            } else {
                i += 1;
                continue;
            };
            let body = i + sc;
            let mut j = body;
            while j < au.len()
                && !(au[j..].starts_with(&[0, 0, 0, 1]) || au[j..].starts_with(&[0, 0, 1]))
            {
                j += 1;
            }
            total += j - body;
            i = j;
        }
        total
    }
}
