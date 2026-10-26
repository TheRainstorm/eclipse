//! AV1 Main 8/10-bit low-overhead OBUs and independent sequence/recovery metadata.
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
            if data[at] & 7 != 0 {
                return Err("invalid AV1 extension reserved bits".into());
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
    pub bit_depth: u8,
}

/// Declare the signal of our encoder input, preserving coded pixels and every
/// other sequence field. Some MFTs emit default 709 tags despite receiving our
/// explicit 601 NV12 (or BT.2020/PQ P010) input media type.
#[cfg(any(windows, test))]
pub(crate) fn declare_encoder_color(data: &[u8], hdr: bool) -> Result<Vec<u8>, String> {
    use cros_codecs::codec::av1::{
        parser::{BitDepth, ColorPrimaries, MatrixCoefficients, Profile, SequenceHeaderObu, TransferCharacteristics},
        synthesizer::Synthesizer,
    };
    let mut output = Vec::with_capacity(data.len());
    for unit in obus(data)? {
        if (unit[0] >> 3) & 15 != 1 {
            output.extend_from_slice(unit);
            continue;
        }
        if unit.len() > MAX_HEADER {
            return Err("oversized encoder AV1 sequence header".into());
        }
        let mut parser = Parser::default();
        let ObuAction::Process(obu) = parser.read_obu(unit)? else {
            return Err("encoder AV1 sequence header was dropped".into());
        };
        let ParsedObu::SequenceHeader(sequence) = parser.parse_obu(obu)? else {
            return Err("invalid encoder AV1 sequence header".into());
        };
        let mut sequence = (*sequence).clone();
        let color = &mut sequence.color_config;
        if sequence.seq_profile != Profile::Profile0
            || sequence.bit_depth != if hdr { BitDepth::Depth10 } else { BitDepth::Depth8 }
            || color.mono_chrome || !color.subsampling_x || !color.subsampling_y
        {
            return Err("encoder AV1 input requires negotiated Main 4:2:0 bit depth".into());
        }
        color.color_description_present_flag = true;
        color.color_range = false;
        color.color_primaries = if hdr { ColorPrimaries::Bt2020 } else { ColorPrimaries::Bt601 };
        color.transfer_characteristics = if hdr { TransferCharacteristics::Smpte2084 } else { TransferCharacteristics::Bt709 };
        color.matrix_coefficients = if hdr { MatrixCoefficients::Bt2020Ncl } else { MatrixCoefficients::Bt601 };
        Synthesizer::<'_, SequenceHeaderObu, _>::synthesize(&sequence, &mut output)
            .map_err(|error| format!("encoder AV1 colour declaration: {error}"))?;
    }
    Ok(output)
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
            if c.twelve_bit
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
            let hdr=c.color_primaries as u32==9 && c.transfer_characteristics as u32==16 && c.matrix_coefficients as u32==9;
            let color = if hdr {if c.color_range {YuvColor::Full2020Pq}else{YuvColor::Limited2020Pq}}else{match (c.color_range, bt709) {
                (true, true) => YuvColor::Full709,
                (false, true) => YuvColor::Limited709,
                (true, false) => YuvColor::Full601,
                (false, false) => YuvColor::Limited601,
            }};
            return Some(Sequence {
                width,
                height,
                color,
                bit_depth:if c.high_bitdepth{10}else{8},
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
    probe_decoder_with_kind(crate::DecoderKind::from_env())
}
pub fn probe_decoder_with_kind(kind:crate::DecoderKind)->Result<(),crate::CodecError> {
    let mut decoder = crate::decoder_for_codec_with_kind(uu_proto::media::VideoCodec::Av1,kind)?;
    validate_decoder(decoder.as_mut())
}

/// Check actual I/P output before selecting or advertising an AV1 backend.
/// Accept an existing decoder so candidate selection never recurses through
/// `decoder_for_codec` and never treats an open device as proven AV1 support.
pub(crate) fn validate_decoder(decoder: &mut dyn crate::VideoDecoder) -> Result<(), crate::CodecError> {
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
    fn malformed_headers_orphan_tiles_and_missing_reference_geometry_do_not_panic() {
        for data in [
            &[0x13, 0][..],    // reserved header bit
            &[0x92, 0][..],    // forbidden bit
            &[0x16, 7, 0][..], // extension reserved bits
            &[0x22, 0][..],    // tile group before a frame header
            &[
                18, 0, 10, 13, 0, 0, 0, 3, 55, 255, 230, 215, 200, 128, 128, 128, 130, 26, 40, 50,
                238, 245, 74, 188, 129, 244, 23, 107, 75, 27, 5, 239, 246, 39, 21, 57, 135, 154,
                191, 204, 17, 9, 67, 227, 204, 19, 21, 17, 187, 74, 22, 210, 69, 219, 226, 155,
                234, 247, 186, 65,
            ][..],
        ] {
            let _ = sequence(data);
            assert!(!is_keyframe(data));
        }
        let mut parser = Parser::default();
        let ObuAction::Process(obu) = parser.read_obu(&[0x22, 0]).unwrap() else {
            panic!("expected OBU");
        };
        assert!(parser.parse_obu(obu).is_err());
    }
    #[test]
    fn two_tile_columns_use_column_geometry_and_reject_oversized_tile() {
        // Generated 256x128 gray, libaom realtime, 2 columns x 1 row, no lookahead.
        const TWO_COLUMNS: &[u8] = &[
            18, 0, 10, 10, 0, 0, 0, 3, 183, 255, 243, 107, 228, 1, 50, 37, 16, 0, 192, 106, 0, 65,
            4, 16, 128, 65, 0, 0, 11, 193, 180, 187, 3, 85, 213, 216, 112, 141, 166, 64, 216, 193,
            180, 187, 3, 85, 213, 216, 112, 141, 166, 64, 216,
        ];
        let mut parser = Parser::default();
        let mut input = TWO_COLUMNS;
        while !input.is_empty() {
            let ObuAction::Process(obu) = parser.read_obu(input).unwrap() else {
                panic!("unexpected dropped OBU");
            };
            let used = obu.bytes_used;
            if let ParsedObu::Frame(frame) = parser.parse_obu(obu).unwrap() {
                let tiles = &frame.tile_group.tiles;
                assert_eq!(tiles.len(), 2);
                assert_eq!((tiles[0].mi_col_start, tiles[0].mi_col_end), (0, 32));
                assert_eq!((tiles[1].mi_col_start, tiles[1].mi_col_end), (32, 64));
                assert!(tiles
                    .iter()
                    .all(|tile| (tile.mi_row_start, tile.mi_row_end) == (0, 32)));
                assert!(is_keyframe(TWO_COLUMNS));
                let group = frame.tile_group.obu.as_ref();
                let size_bytes = frame.header.tile_info.tile_size_bytes as usize;
                let size_at = TWO_COLUMNS.len() - input.len() + used - group.len()
                    + tiles[0].tile_offset as usize - size_bytes;
                let mut corrupt = TWO_COLUMNS.to_vec();
                corrupt[size_at..size_at + size_bytes].fill(255);
                assert!(!is_keyframe(&corrupt), "tile size must fit the remaining OBU");
                return;
            }
            input = &input[used..];
        }
        panic!("generated tiled frame missing");
    }
    #[test]
    fn generated_stream_byte_mutations_remain_bounded() {
        for source in PROBE_UNITS {
            for at in 0..source.len() {
                for byte in 0..=255 {
                    let mut data = source.to_vec();
                    data[at] = byte;
                    let _ = obus(&data);
                    let _ = sequence(&data);
                    let _ = is_keyframe(&data);
                }
            }
        }
    }
    #[test]
    fn generated_keyframe_metadata_and_truncation_are_bounded() {
        assert_eq!(
            sequence(PROBE),
            Some(Sequence {
                width: 128,
                height: 128,
                color: YuvColor::Limited709, bit_depth:8
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
    #[test]
    fn encoder_timing_info_with_full_width_unsigned_fields_is_valid() {
        // Generated uniform grey NV12, 720p60, AV1 Main/8-bit, limited BT.601.
        // Its timing_info_present_flag requires the standard 32-bit fields.
        let frame=include_bytes!("amf_av1_probe.obu");
        assert_eq!(sequence(frame),Some(Sequence{width:1280,height:720,color:YuvColor::Limited601,bit_depth:8}));
        assert!(is_keyframe(frame));
        let mut parser=Parser::default();
        let mut input=frame.as_slice();
        let mut found=false;
        while !input.is_empty() {
            let ObuAction::Process(obu)=parser.read_obu(input).unwrap() else {panic!("unexpected dropped OBU");};
            let used=obu.bytes_used;
            if let ParsedObu::SequenceHeader(s)=parser.parse_obu(obu).unwrap() {
                assert!(s.timing_info_present_flag);
                assert_eq!(s.timing_info.time_scale/s.timing_info.num_units_in_display_tick,60);
                found=true;
            }
            input=&input[used..];
        }
        assert!(found);
        for end in 0..frame.len() {let _=sequence(&frame[..end]);let _=is_keyframe(&frame[..end]);}
    }
}

#[cfg(test)] mod hdr_tests {
    #[test] fn reads_main10_pq_2020_and_rejects_truncation() {
        let bytes=include_bytes!("av1_hdr10_probe.obu");
        let seq=super::sequence(bytes).unwrap();
        assert_eq!((seq.width,seq.height,seq.bit_depth),(128,128,10));
        assert_eq!(seq.color,crate::color::YuvColor::Limited2020Pq);
        for end in 0..bytes.len(){let _=super::sequence(&bytes[..end]);}
    }
}

#[cfg(test)]
mod encoder_color_tests {
    use super::*;
    fn header(data: &[u8]) -> cros_codecs::codec::av1::parser::SequenceHeaderObu {
        let unit = obus(data).unwrap().into_iter().find(|u| (u[0] >> 3) & 15 == 1).unwrap();
        let mut parser = Parser::default();
        let ObuAction::Process(obu) = parser.read_obu(unit).unwrap() else { panic!("sequence dropped") };
        let ParsedObu::SequenceHeader(seq) = parser.parse_obu(obu).unwrap() else { panic!("sequence missing") };
        (*seq).clone()
    }
    #[test]
    fn encoder_color_preserves_sequence_structure_and_coded_payloads() {
        for (input, hdr, expected) in [
            (PROBE, false, YuvColor::Limited601),
            (&include_bytes!("av1_hdr10_probe.obu")[..], true, YuvColor::Limited2020Pq),
        ] {
            let output = declare_encoder_color(input, hdr).unwrap();
            let before = header(input);
            let mut after = header(&output);
            assert_eq!(sequence(&output).unwrap().color, expected);
            assert_eq!(is_keyframe(&output), is_keyframe(input));
            assert_eq!(sequence(&output).unwrap().width, sequence(input).unwrap().width);
            after.color_config = before.color_config.clone();
            assert_eq!(after, before, "only the colour description may change");
            let payloads = |bytes: &[u8]| obus(bytes).unwrap().into_iter()
                .filter(|u| (u[0] >> 3) & 15 != 1).map(Vec::from).collect::<Vec<_>>();
            assert_eq!(payloads(input), payloads(&output));
            assert_eq!(declare_encoder_color(&output, hdr).unwrap(), output);
        }
        assert_eq!(declare_encoder_color(PROBE_UNITS[1], false).unwrap(), PROBE_UNITS[1]);
    }
    #[test]
    fn encoder_color_cannot_disguise_depth_or_truncated_headers() {
        assert!(declare_encoder_color(PROBE, true).is_err());
        assert!(declare_encoder_color(include_bytes!("av1_hdr10_probe.obu"), false).is_err());
        let unit = obus(PROBE).unwrap().into_iter().find(|u| (u[0] >> 3) & 15 == 1).unwrap();
        for end in 1..unit.len() { assert!(declare_encoder_color(&unit[..end], false).is_err()); }
    }
}
