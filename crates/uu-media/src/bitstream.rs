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
    let mut r = BitReader { data: rbsp, pos: 0 };
    let profile = r.u(8)?;
    r.u(8)?;
    r.u(8)?;
    r.ue()?; // sps_id
    let mut chroma_format_idc = 1u32;
    if matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        chroma_format_idc = r.ue()?;
        if chroma_format_idc == 3 {
            r.u(1)?;
        }
        r.ue()?; // bit_depth_luma_minus8
        r.ue()?; // bit_depth_chroma_minus8
        r.u(1)?; // qpprime
        if r.u(1)? == 1 {
            return None; // scaling matrices: unsupported here
        }
    }
    r.ue()?; // log2_max_frame_num_minus4
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
    r.ue()?; // max_num_ref_frames
    r.u(1)?; // gaps_in_frame_num_value_allowed_flag
    let width_mbs = r.ue()?.checked_add(1)?;
    let height_map_units = r.ue()?.checked_add(1)?;
    let frame_mbs_only = r.u(1)? == 1;
    if !frame_mbs_only {
        r.u(1)?; // mb_adaptive_frame_field_flag
    }
    r.u(1)?; // direct_8x8_inference_flag
    let (mut crop_l, mut crop_r, mut crop_t, mut crop_b) = (0u32, 0u32, 0u32, 0u32);
    if r.u(1)? == 1 {
        crop_l = r.ue()?;
        crop_r = r.ue()?;
        crop_t = r.ue()?;
        crop_b = r.ue()?;
    }
    let frame_height_mbs = if frame_mbs_only { height_map_units } else { height_map_units * 2 };
    let (crop_unit_x, crop_unit_y) = if chroma_format_idc == 0 {
        (1, if frame_mbs_only { 2 } else { 1 })
    } else {
        (2, if frame_mbs_only { 2 } else { 1 })
    };
    let width = width_mbs * 16 - (crop_l + crop_r) * crop_unit_x;
    let height = frame_height_mbs * 16 - (crop_t + crop_b) * crop_unit_y;
    Some((width, height))
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
            while j < au.len() && !(au[j..].starts_with(&[0, 0, 0, 1]) || au[j..].starts_with(&[0, 0, 1])) {
                j += 1;
            }
            total += j - body;
            i = j;
        }
        total
    }
}
