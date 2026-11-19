//! Exercise a single decoder with independently decodable AV1 sequences.
#![cfg(all(target_os = "linux", feature = "vaapi"))]

use bytes::Bytes;
use uu_media::hw::vaapi::VaapiDecoder;
use uu_media::{av1, hdr, EncodedFrame, PixelFormat, Timestamp, VideoDecoder};
use uu_proto::media::VideoCodec;

#[test]
#[ignore = "requires a VA-API device with AV1 Main 8-bit and 10-bit decoding"]
fn new_sequence_geometry_precedes_the_first_frame() {
    let large = include_bytes!("../src/amf_av1_probe.obu").as_slice();
    let small = av1::PROBE;
    // The HDR capability fixture contains several temporal units. Use its
    // first keyframe as a distinct sequence at the small stream's geometry.
    let obus = av1::obus(hdr::AV1_PROBE).unwrap();
    let first_unit_len = obus[0].len()
        + obus[1..]
            .iter()
            .take_while(|obu| (obu[0] >> 3) & 15 != 2)
            .map(|obu| obu.len())
            .sum::<usize>();
    let ten_bit = &hdr::AV1_PROBE[..first_unit_len];

    let mut decoder = VaapiDecoder::with_codec(VideoCodec::Av1).unwrap();
    let sequences = [
        (large, 1280, 720, PixelFormat::Nv12),
        (small, 128, 128, PixelFormat::Nv12),
        (ten_bit, 128, 128, PixelFormat::P010),
        (large, 1280, 720, PixelFormat::Nv12),
    ];
    for (index, (data, width, height, format)) in sequences.into_iter().enumerate() {
        let pts = Timestamp(20_000 * index as u64);
        let unit = EncodedFrame {
            data: Bytes::from_static(data),
            pts,
            is_keyframe: true,
        };
        let frames = decoder.decode_frames(&unit).unwrap();
        let [frame] = frames.as_slice() else {
            panic!("expected one output for sequence {index}")
        };
        assert_eq!(
            (frame.width, frame.height, frame.pts, frame.format),
            (width, height, pts, format)
        );
        assert!(frame.is_consistent());
    }
}
