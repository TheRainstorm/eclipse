//! Describe the actual input signal of our own encoder, never received media.
//! H.265 7.3.2.2 / 7.3.3 / 7.3.4 / 7.3.7: preserve all SPS syntax except
//! video_signal_type, and preserve every other NAL byte for byte.
use cros_codecs::codec::h265::parser::{Nalu, NaluType, Parser, Sps};

struct Bits<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Bits<'_> {
    fn read(&mut self, count: usize) -> Result<u32, String> {
        if count > 32
            || self
                .at
                .checked_add(count)
                .is_none_or(|end| end > self.bytes.len() * 8)
        {
            return Err("truncated encoder HEVC SPS".into());
        }
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1) | u32::from((self.bytes[self.at / 8] >> (7 - self.at % 8)) & 1);
            self.at += 1;
        }
        Ok(value)
    }
    fn skip(&mut self, count: usize) -> Result<(), String> {
        if self
            .at
            .checked_add(count)
            .is_none_or(|end| end > self.bytes.len() * 8)
        {
            return Err("truncated encoder HEVC SPS".into());
        }
        self.at += count;
        Ok(())
    }
    fn flag(&mut self) -> Result<bool, String> {
        Ok(self.read(1)? != 0)
    }
    fn ue(&mut self) -> Result<u32, String> {
        let mut zeros = 0;
        while !self.flag()? {
            zeros += 1;
            if zeros >= 32 {
                return Err("oversized encoder HEVC integer".into());
            }
        }
        Ok((1u32 << zeros) - 1 + self.read(zeros)?)
    }
}
#[derive(Default)]
struct Writer {
    bytes: Vec<u8>,
    at: usize,
}
impl Writer {
    fn bit(&mut self, value: bool) {
        if self.at % 8 == 0 {
            self.bytes.push(0);
        }
        if value {
            *self.bytes.last_mut().unwrap() |= 1 << (7 - self.at % 8);
        }
        self.at += 1;
    }
    fn number(&mut self, value: u32, count: usize) {
        for shift in (0..count).rev() {
            self.bit((value >> shift) & 1 != 0);
        }
    }
    fn copy(&mut self, bytes: &[u8], start: usize, end: usize) {
        for at in start..end {
            self.bit(bytes[at / 8] & (1 << (7 - at % 8)) != 0);
        }
    }
}

fn unescape(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut zeros = 0;
    for (index, &byte) in bytes.iter().enumerate() {
        if zeros == 2 && byte == 3 {
            if bytes.get(index + 1).is_none_or(|next| *next > 3) {
                return Err("invalid encoder HEVC emulation prevention".into());
            }
            zeros = 0;
            continue;
        }
        out.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    Ok(out)
}
fn escape(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut zeros = 0;
    for &byte in bytes {
        if zeros == 2 && byte <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    out
}

/// The independent parser validates the complete SPS first. This cursor only
/// locates its VUI; it never interprets or changes transform/reference policy.
fn vui_offset(bits: &mut Bits<'_>, sps: &Sps) -> Result<usize, String> {
    bits.skip(4)?;
    let layers = bits.read(3)? as usize;
    bits.skip(1 + 96)?;
    let mut sub = Vec::with_capacity(layers);
    for _ in 0..layers {
        sub.push((bits.flag()?, bits.flag()?));
    }
    if layers > 0 {
        bits.skip((8 - layers) * 2)?;
    }
    for (profile, level) in sub {
        if profile {
            bits.skip(88)?;
        }
        if level {
            bits.skip(8)?;
        }
    }
    bits.ue()?;
    let chroma = bits.ue()?;
    if chroma == 3 {
        bits.skip(1)?;
    }
    bits.ue()?;
    bits.ue()?;
    if bits.flag()? {
        for _ in 0..4 {
            bits.ue()?;
        }
    }
    bits.ue()?;
    bits.ue()?;
    let poc = bits.ue()? as usize + 4;
    let all_layers = bits.flag()?;
    for _ in (if all_layers { 0 } else { layers })..=layers {
        for _ in 0..3 {
            bits.ue()?;
        }
    }
    for _ in 0..6 {
        bits.ue()?;
    }
    if bits.flag()? && bits.flag()? {
        for size in 0..4 {
            for _ in (0..6).step_by(if size == 3 { 3 } else { 1 }) {
                if bits.flag()? {
                    if size > 1 {
                        bits.ue()?;
                    }
                    for _ in 0..64usize.min(1 << (4 + size * 2)) {
                        bits.ue()?;
                    }
                } else {
                    bits.ue()?;
                }
            }
        }
    }
    bits.skip(2)?;
    if bits.flag()? {
        bits.skip(8)?;
        bits.ue()?;
        bits.ue()?;
        bits.skip(1)?;
    }
    let sets = bits.ue()? as usize;
    if sets != sps.short_term_ref_pic_set.len() {
        return Err("encoder HEVC reference-set count changed".into());
    }
    for index in 0..sets {
        if index > 0 && bits.flag()? {
            bits.skip(1)?;
            bits.ue()?;
            for _ in 0..=sps.short_term_ref_pic_set[index - 1].num_delta_pocs {
                if !bits.flag()? {
                    bits.skip(1)?;
                }
            }
        } else {
            let negative = bits.ue()?;
            let positive = bits.ue()?;
            if negative != u32::from(sps.short_term_ref_pic_set[index].num_negative_pics)
                || positive != u32::from(sps.short_term_ref_pic_set[index].num_positive_pics)
            {
                return Err("encoder HEVC reference-set syntax changed".into());
            }
            for _ in 0..negative + positive {
                bits.ue()?;
                bits.skip(1)?;
            }
        }
    }
    if bits.flag()? {
        let count = bits.ue()?;
        if count != u32::from(sps.num_long_term_ref_pics_sps) {
            return Err("encoder HEVC long-term references changed".into());
        }
        for _ in 0..count {
            bits.skip(poc + 1)?;
        }
    }
    bits.skip(2)?;
    Ok(bits.at)
}

fn rewrite(nal: &[u8], sps: &Sps, hdr: bool) -> Result<Vec<u8>, String> {
    if sps.chroma_format_idc != 1
        || sps.bit_depth_luma_minus8 != if hdr { 2 } else { 0 }
        || sps.bit_depth_chroma_minus8 != sps.bit_depth_luma_minus8
    {
        return Err("encoder HEVC input requires negotiated 4:2:0 bit depth".into());
    }
    let mut rbsp = unescape(&nal[2..])?;
    while rbsp.last() == Some(&0) {
        rbsp.pop();
    }
    let last = *rbsp.last().ok_or("empty encoder HEVC SPS")?;
    let end = rbsp.len() * 8 - last.trailing_zeros() as usize;
    let mut bits = Bits {
        bytes: &rbsp,
        at: 0,
    };
    let vui = vui_offset(&mut bits, sps)?;
    let present = bits.flag()?;
    let mut video_format = 5;
    let start = if present {
        if bits.flag()? {
            if bits.read(8)? == 255 {
                bits.skip(32)?;
            }
        }
        if bits.flag()? {
            bits.skip(1)?;
        }
        let start = bits.at;
        if bits.flag()? {
            video_format = bits.read(3)?;
            bits.skip(1)?;
            if bits.flag()? {
                bits.skip(24)?;
            }
        }
        start
    } else {
        vui
    };
    if bits.at > end {
        return Err("truncated encoder HEVC VUI".into());
    }
    let mut out = Writer::default();
    out.copy(&rbsp, 0, start);
    if !present {
        out.bit(true);
        out.bit(false);
        out.bit(false);
    }
    out.bit(true);
    out.number(video_format, 3);
    out.bit(false);
    out.bit(true);
    for value in if hdr { [9, 16, 9] } else { [6, 6, 6] } {
        out.number(value, 8);
    }
    if !present {
        for _ in 0..7 {
            out.bit(false);
        }
    }
    out.copy(&rbsp, bits.at, end);
    let mut result = nal[..2].to_vec();
    result.extend(escape(&out.bytes));
    Ok(result)
}

pub(crate) fn declare_encoder_color(data: &[u8], hdr: bool) -> Result<Vec<u8>, String> {
    use std::io::Cursor;
    let mut cursor = Cursor::new(data);
    let mut output = Vec::with_capacity(data.len() + 16);
    let mut copied = 0;
    while (cursor.position() as usize) < data.len() {
        if data[cursor.position() as usize..]
            .iter()
            .all(|byte| *byte == 0)
        {
            break;
        }
        let nal = Nalu::next(&mut cursor)?;
        let start = cursor.position() as usize;
        let end = start
            .checked_add(nal.size)
            .ok_or("oversized encoder HEVC NAL")?;
        if end > data.len() {
            return Err("truncated encoder HEVC NAL".into());
        }
        if nal.header.type_ == NaluType::SpsNut {
            if nal.size > 64 * 1024 {
                return Err("oversized encoder HEVC SPS".into());
            }
            let mut parser = Parser::default();
            let sps = parser.parse_sps(&nal)?;
            output.extend_from_slice(&data[copied..start]);
            output.extend(rewrite(nal.as_ref(), sps, hdr)?);
            copied = end;
        }
        cursor.set_position(end as u64);
    }
    output.extend_from_slice(&data[copied..]);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn own_sequence_color_changes_without_changing_geometry_or_picture_nals() {
        for (bytes, hdr, color) in [
            (
                crate::hevc::PROBE,
                false,
                crate::color::YuvColor::Limited601,
            ),
            (
                include_bytes!("hevc_hdr10_probe.h265").as_slice(),
                true,
                crate::color::YuvColor::Limited2020Pq,
            ),
        ] {
            let before = crate::hevc::sequence(bytes).unwrap();
            let after = declare_encoder_color(bytes, hdr).unwrap();
            assert_eq!(
                crate::hevc::sequence(&after),
                Some(crate::hevc::Sequence { color, ..before })
            );
            assert_eq!(declare_encoder_color(&after, hdr).unwrap(), after);
            let pictures = |data: &[u8]| {
                let mut cursor = std::io::Cursor::new(data);
                let mut values = Vec::new();
                while let Ok(nal) = Nalu::next(&mut cursor) {
                    cursor.set_position(cursor.position() + nal.size as u64);
                    if nal.header.type_ != NaluType::SpsNut {
                        values.push(nal.as_ref().to_vec());
                    }
                }
                values
            };
            assert_eq!(pictures(bytes), pictures(&after));
        }
        assert!(declare_encoder_color(crate::hevc::PROBE, true).is_err());
    }
}
