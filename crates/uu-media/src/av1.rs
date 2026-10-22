//! AV1 Main 8-bit low-overhead OBUs and independent sequence/recovery metadata.
use crate::color::YuvColor;
use cros_codecs::codec::av1::parser::{FrameType, ObuAction, ParsedObu, Parser};
const MAX_HEADER: usize = 64 * 1024;
/// Low-overhead AV1 OBUs with explicit size fields, as produced by VPL.
pub fn obus(data: &[u8]) -> Result<Vec<&[u8]>, String> {
    let mut units = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let begin = at;
        let header = data[at];
        at += 1;
        if header & 0x81 != 0 || header & 2 == 0 {
            return Err("invalid AV1 OBU header/size field".into());
        }
        if header & 4 != 0 {
            if at >= data.len() {
                return Err("truncated AV1 extension".into());
            }
            at += 1;
        }
        let mut size = 0usize;
        let mut done = false;
        for index in 0..8 {
            let byte = *data.get(at).ok_or("truncated AV1 OBU length")?;
            at += 1;
            let part = usize::from(byte & 0x7f)
                .checked_shl(index * 7)
                .ok_or("AV1 length overflow")?;
            size = size.checked_add(part).ok_or("AV1 length overflow")?;
            if byte & 0x80 == 0 {
                done = true;
                break;
            }
        }
        if !done {
            return Err("oversized AV1 OBU length".into());
        }
        at = at
            .checked_add(size)
            .filter(|end| *end <= data.len())
            .ok_or("truncated AV1 OBU")?;
        units.push(&data[begin..at]);
        if units.len() > 4096 {
            return Err("too many AV1 OBUs".into());
        }
    }
    Ok(units)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sequence {
    pub width: u32,
    pub height: u32,
    pub color: YuvColor,
}
pub fn sequence(data: &[u8]) -> Option<Sequence> {
    let mut parser = Parser::default();
    let mut input = data;
    while !input.is_empty() {
        let obu = match parser.read_obu(input).ok()? {
            ObuAction::Drop(n) => {
                if n == 0 {
                    return None;
                }
                input = input.get(n as usize..)?;
                continue;
            }
            ObuAction::Process(obu) => obu,
        };
        let used = obu.bytes_used;
        if used == 0 || used > input.len() {
            return None;
        }
        if used > MAX_HEADER && (input[0] >> 3) & 15 == 1 {
            return None;
        }
        if let ParsedObu::SequenceHeader(seq) = parser.parse_obu(obu).ok()? {
            let c = &seq.color_config;
            if c.high_bitdepth
                || c.twelve_bit
                || c.mono_chrome
                || !c.subsampling_x
                || !c.subsampling_y
            {
                return None;
            }
            let width = u32::from(seq.max_frame_width_minus_1) + 1;
            let height = u32::from(seq.max_frame_height_minus_1) + 1;
            if width < 2 || height < 2 || width > 8192 || height > 8192 {
                return None;
            }
            let bt709 = !matches!(c.matrix_coefficients as u32, 5 | 6);
            let color = match (c.color_range, bt709) {
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
        input = &input[used..];
    }
    None
}
pub fn is_keyframe(data: &[u8]) -> bool {
    let mut parser = Parser::default();
    let mut input = data;
    while !input.is_empty() {
        let obu = match parser.read_obu(input) {
            Ok(ObuAction::Drop(n)) => {
                if n == 0 {
                    return false;
                }
                let Some(rest) = input.get(n as usize..) else {
                    return false;
                };
                input = rest;
                continue;
            }
            Ok(ObuAction::Process(obu)) => obu,
            _ => return false,
        };
        let used = obu.bytes_used;
        if used == 0 || used > input.len() {
            return false;
        }
        match parser.parse_obu(obu) {
            Ok(ParsedObu::Frame(frame)) => {
                return frame.header.frame_type == FrameType::KeyFrame
                    && !frame.header.show_existing_frame
                    && frame.header.show_frame
            }
            Ok(ParsedObu::FrameHeader(header)) => {
                return header.frame_type == FrameType::KeyFrame
                    && !header.show_existing_frame
                    && header.show_frame
            }
            Ok(_) => {}
            Err(_) => return false,
        }
        input = &input[used..];
    }
    false
}
/// Generated gray 128x128/60 Hz, libaom realtime/no-lookahead, 709 limited.
pub const PROBE_UNITS: [&[u8]; 4] = [
    &[
        18, 0, 10, 13, 0, 0, 0, 3, 55, 255, 230, 215, 200, 128, 128, 128, 130, 50, 22, 16, 0, 132,
        0, 2, 8, 32, 136, 0, 0, 193, 180, 208, 193, 77, 245, 16, 137, 79, 197, 162, 188,
    ],
    &[
        18, 0, 50, 20, 48, 3, 128, 128, 253, 248, 6, 144, 0, 22, 89, 101, 138, 4, 16, 128, 0, 150,
        207, 104,
    ],
    &[
        18, 0, 50, 20, 48, 4, 1, 5, 125, 248, 6, 144, 0, 22, 89, 101, 138, 4, 16, 128, 0, 155, 154,
        128,
    ],
    &[
        18, 0, 50, 19, 48, 6, 2, 9, 253, 248, 6, 144, 0, 22, 89, 101, 138, 4, 16, 128, 0, 158, 100,
    ],
];
pub const PROBE: &[u8] = PROBE_UNITS[0];
pub fn probe_decoder() -> Result<(), crate::CodecError> {
    let mut decoder = crate::decoder_for_codec(uu_proto::media::VideoCodec::Av1)?;
    let mut produced = 0;
    for (i, data) in PROBE_UNITS.iter().cycle().take(8).enumerate() {
        if let Some(frame) = decoder.decode(&crate::EncodedFrame {
            data: bytes::Bytes::from_static(data),
            pts: crate::Timestamp(i as u64 * 16667),
            is_keyframe: is_keyframe(data),
        })? {
            if frame.width != 128 || frame.height != 128 || !frame.is_consistent() {
                return Err(crate::CodecError::Backend(
                    "invalid AV1 probe output".into(),
                ));
            }
            produced += 1;
        }
    }
    if produced < 6 {
        return Err(crate::CodecError::Backend(
            "AV1 probe produced too few frames".into(),
        ));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_keyframe_metadata_and_truncation_are_bounded() {
        assert_eq!(
            sequence(PROBE),
            Some(Sequence {
                width: 128,
                height: 128,
                color: YuvColor::Limited709
            })
        );
        assert!(is_keyframe(PROBE));
        for end in 0..PROBE.len() {
            let _ = sequence(&PROBE[..end]);
            let _ = is_keyframe(&PROBE[..end]);
            let _ = obus(&PROBE[..end]);
        }
        assert!(obus(&[0x0a, 0xff]).is_err());
    }
}
