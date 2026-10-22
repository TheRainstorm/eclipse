//! Describe the no-B-frame structure of our own low-delay encoders in VUI.
//! Never apply this to arbitrary received streams: they may actually reorder.
use cros_codecs::codec::h264::{
    parser::{Nalu, NaluType, Parser, Sps},
    synthesizer::Synthesizer,
};
use std::io::Cursor;

pub(super) fn declare_no_reordering(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut cursor = Cursor::new(data);
    let mut output = Vec::with_capacity(data.len() + 32);
    while (cursor.position() as usize) < data.len() {
        if data[cursor.position() as usize..]
            .iter()
            .all(|value| *value == 0)
        {
            break;
        }
        let nalu = Nalu::next(&mut cursor)?;
        cursor.set_position(cursor.position() + nalu.size as u64);
        if nalu.header.type_ == NaluType::Sps {
            let mut parser = Parser::default();
            let parsed = parser.parse_sps(&nalu)?.clone();
            drop(parser);
            let mut sps = std::rc::Rc::try_unwrap(parsed)
                .map_err(|_| "SPS parser retained unexpected ownership")?;
            sps.vui_parameters_present_flag = true;
            let vui = &mut sps.vui_parameters;
            vui.bitstream_restriction_flag = true;
            vui.motion_vectors_over_pic_boundaries_flag = true;
            vui.max_bytes_per_pic_denom = 0;
            vui.max_bits_per_mb_denom = 0;
            vui.log2_max_mv_length_horizontal = 16;
            vui.log2_max_mv_length_vertical = 16;
            vui.max_num_reorder_frames = 0;
            vui.max_dec_frame_buffering = u32::from(sps.max_num_ref_frames).max(1);
            Synthesizer::<Sps, &mut Vec<u8>>::synthesize(
                nalu.header.ref_idc,
                &sps,
                &mut output,
                true,
            )
            .map_err(|e| format!("low-delay SPS: {e}"))?;
        } else {
            output.extend_from_slice(&[0, 0, 0, 1]);
            output.extend_from_slice(nalu.as_ref());
        }
    }
    Ok(output)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        H264Decoder, H264Encoder, PixelFormat, RawFrame, Timestamp, VideoDecoder, VideoEncoder,
    };
    #[test]
    fn own_encoder_vui_preserves_colour_geometry_and_cold_recovery() {
        let mut encoder = H264Encoder::new(1000).unwrap();
        let raw = RawFrame {
            width: 64,
            height: 64,
            stride: 64,
            format: PixelFormat::I420,
            data: vec![128; 64 * 64 * 3 / 2].into(),
            pts: Timestamp(0),
            gpu: false,
            dma_buf: None,
            d3d11: None,
        };
        let mut frame = encoder.encode(&raw).unwrap().remove(0);
        let original = frame.data.clone();
        frame.data = declare_no_reordering(&frame.data).unwrap().into();
        assert_eq!(
            crate::bitstream::h264_dimensions(&frame.data),
            Some((64, 64))
        );
        assert_eq!(
            crate::color::from_h264(&frame.data),
            crate::color::from_h264(&original)
        );
        let mut parser = Parser::default();
        let mut cursor = Cursor::new(frame.data.as_ref());
        let mut checked = false;
        while (cursor.position() as usize) < frame.data.len() {
            if frame.data[cursor.position() as usize..]
                .iter()
                .all(|value| *value == 0)
            {
                break;
            }
            let nalu = Nalu::next(&mut cursor).unwrap();
            cursor.set_position(cursor.position() + nalu.size as u64);
            if nalu.header.type_ == NaluType::Sps {
                let sps = parser.parse_sps(&nalu).unwrap();
                assert_eq!(sps.max_num_order_frames(), 0);
                assert!(sps.vui_parameters.bitstream_restriction_flag);
                checked = true;
            }
        }
        assert!(checked);
        assert!(H264Decoder::new()
            .unwrap()
            .decode(&frame)
            .unwrap()
            .is_some());
        frame.data = declare_no_reordering(&frame.data).unwrap().into();
        assert!(H264Decoder::new()
            .unwrap()
            .decode(&frame)
            .unwrap()
            .is_some());
    }
}
