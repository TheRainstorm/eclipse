//! Validated limited BT.601 NV12 uploads shared by VA-API and Windows MFT.
#[cfg(test)]
use crate::Timestamp;
use crate::{PixelFormat, RawFrame};
#[cfg(test)]
use openh264::formats::{BGRA8Source, RGB8Source, RGBSource, YUVBuffer, YUVSource};

#[cfg(target_arch = "x86_64")]
#[path = "upload/x86.rs"]
mod x86;

/// Write directly into the driver's NV12 surface. BGRA uses limited BT.601;
/// the existing raw YUV inputs use full BT.601 and are range-compressed here.
/// No intermediate frame, allocation, or NV12 -> I420 -> NV12 round trip.
pub(super) fn upload_frame(
    frame: &RawFrame,
    coded: (u32, u32),
    pitches: [u32; 3],
    offsets: [u32; 3],
    out: &mut [u8],
) -> Result<(), String> {
    let (w, h, stride) = crate::convert::validate_cpu_frame(frame).map_err(|e| e.to_string())?;
    let layout = validate_layout(w, h, coded, pitches, offsets, out.len())?;
    let [_, _, yp, uvp, yo, uvo] = layout;
    clear_padding(w, h, layout, out);
    if frame.format == PixelFormat::Bgra8 {
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("avx2") {
            unsafe { x86::bgra_nv12_avx2(&frame.data, w, h, stride, yp, uvp, yo, uvo, out) };
            return Ok(());
        }
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("sse4.1") {
            // Complete source/destination geometry has been validated above.
            unsafe { x86::bgra_nv12(&frame.data, w, h, stride, yp, uvp, yo, uvo, out) };
            return Ok(());
        }
        for row in (0..h).step_by(2) {
            bgra_row_pair(&frame.data, w, row, stride, yp, uvp, yo, uvo, out, 0);
        }
        return Ok(());
    }
    for row in 0..h {
        let src = &frame.data[row * stride..row * stride + w];
        let dst = &mut out[yo + row * yp..yo + row * yp + w];
        range_row::<219>(src, dst);
    }
    let chroma = stride * h;
    for row in 0..h / 2 {
        let dst = &mut out[uvo + row * uvp..uvo + row * uvp + w];
        if frame.format == PixelFormat::Nv12 {
            let src = &frame.data[chroma + row * stride..chroma + row * stride + w];
            range_row::<224>(src, dst);
        } else {
            let start = chroma + row * w / 2;
            let (u, v) = (
                &frame.data[start..start + w / 2],
                &frame.data[start + w * h / 4..start + w * h / 4 + w / 2],
            );
            #[cfg(target_arch = "x86_64")]
            if std::is_x86_feature_detected!("avx2") {
                unsafe { x86::interleave_row(u, v, dst) };
                continue;
            }
            for (dst, (&u, &v)) in dst.chunks_exact_mut(2).zip(u.iter().zip(v)) {
                dst[0] = 16 + ((u32::from(u) * 224 + 127) / 255) as u8;
                dst[1] = 16 + ((u32::from(v) * 224 + 127) / 255) as u8;
            }
        }
    }
    Ok(())
}

fn range_row<const SCALE: i16>(src: &[u8], dst: &mut [u8]) {
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        unsafe { x86::range_row::<SCALE>(src, dst) };
        return;
    }
    for (dst, src) in dst.iter_mut().zip(src) {
        *dst = 16 + ((u32::from(*src) * SCALE as u32 + 127) / 255) as u8;
    }
}

fn validate_layout(
    w: usize,
    h: usize,
    coded: (u32, u32),
    pitches: [u32; 3],
    offsets: [u32; 3],
    length: usize,
) -> Result<[usize; 6], String> {
    let (cw, ch) = (coded.0 as usize, coded.1 as usize);
    if cw < w || ch < h || cw > 8192 || ch > 8192 || cw % 2 != 0 || ch % 2 != 0 {
        return Err("invalid NV12 upload geometry".into());
    }
    let (yp, uvp) = (pitches[0] as usize, pitches[1] as usize);
    let (yo, uvo) = (offsets[0] as usize, offsets[1] as usize);
    let end = |offset: usize, pitch: usize, rows: usize| {
        offset
            .checked_add((rows - 1).checked_mul(pitch)?)
            .and_then(|v| v.checked_add(cw))
    };
    let ye = end(yo, yp, ch).ok_or("NV12 luma offset overflow")?;
    let uve = end(uvo, uvp, ch / 2).ok_or("NV12 chroma offset overflow")?;
    if yp < cw || uvp < cw || ye > length || uve > length || (yo < uve && uvo < ye) {
        return Err("invalid NV12 upload planes".into());
    }
    Ok([cw, ch, yp, uvp, yo, uvo])
}

fn clear_padding(w: usize, h: usize, [cw, ch, yp, uvp, yo, uvo]: [usize; 6], out: &mut [u8]) {
    // Only coded padding needs clearing. Visible pixels are overwritten below;
    // touching them twice is costly on mapped/write-combined GPU memory.
    for row in 0..ch {
        let start = yo + row * yp;
        out[start + if row < h { w } else { 0 }..start + cw].fill(16);
    }
    for row in 0..ch / 2 {
        let start = uvo + row * uvp;
        out[start + if row < h / 2 { w } else { 0 }..start + cw].fill(128);
    }
}

fn bgra_row_pair(
    src: &[u8],
    w: usize,
    row: usize,
    stride: usize,
    yp: usize,
    uvp: usize,
    yo: usize,
    uvo: usize,
    out: &mut [u8],
    start: usize,
) {
    for x in (start..w).step_by(2) {
        let mut sum = [0i32; 3];
        for dy in 0..2 {
            for dx in 0..2 {
                let i = (row + dy) * stride + (x + dx) * 4;
                let (b, g, r) = (src[i] as i32, src[i + 1] as i32, src[i + 2] as i32);
                out[yo + (row + dy) * yp + x + dx] =
                    (16 + ((66 * r + 129 * g + 25 * b) >> 8)) as u8;
                sum[0] += b;
                sum[1] += g;
                sum[2] += r;
            }
        }
        let [b, g, r] = sum.map(|v| (v + 2) >> 2);
        let i = uvo + row / 2 * uvp + x;
        out[i] = (128 + ((-38 * r - 74 * g + 112 * b) >> 8)) as u8;
        out[i + 1] = (128 + ((112 * r - 94 * g - 18 * b) >> 8)) as u8;
    }
}

// The library's standard BGRA adapter supports padded rows in whole pixels.
// Odd byte pitches continue through the existing conversion path.
#[cfg(test)]
struct BgraInput<'a>(&'a RawFrame);
#[cfg(test)]
impl RGBSource for BgraInput<'_> {
    fn dimensions(&self) -> (usize, usize) {
        (self.0.width as usize, self.0.height as usize)
    }
    fn pixel_f32(&self, x: usize, y: usize) -> (f32, f32, f32) {
        let offset = y * self.0.stride as usize + x * 4;
        (
            self.0.data[offset + 2] as f32,
            self.0.data[offset + 1] as f32,
            self.0.data[offset] as f32,
        )
    }
}
#[cfg(test)]
impl RGB8Source for BgraInput<'_> {
    fn dimensions_padded(&self) -> (usize, usize) {
        (self.0.stride as usize / 4, self.0.height as usize)
    }
    fn rgb8_data(&self) -> &[u8] {
        &self.0.data
    }
    fn pixel_stride(&self) -> usize {
        4
    }
    fn rgb_channel_offsets(&self) -> (usize, usize, usize) {
        (2, 1, 0)
    }
}
#[cfg(test)]
impl BGRA8Source for BgraInput<'_> {}

#[cfg(test)]
fn prepare_bgra(buffer: &mut Option<YUVBuffer>, frame: &RawFrame) -> Result<bool, String> {
    if frame.format != PixelFormat::Bgra8 || frame.stride % 4 != 0 {
        return Ok(false);
    }
    let (width, height, _) =
        crate::convert::validate_cpu_frame(frame).map_err(|e| e.to_string())?;
    if buffer
        .as_ref()
        .map_or(true, |buffer| buffer.dimensions() != (width, height))
    {
        *buffer = Some(YUVBuffer::new(width, height));
    }
    buffer
        .as_mut()
        .ok_or("BGRA buffer was not prepared")?
        .read_bgra8(BgraInput(frame));
    Ok(true)
}
#[cfg(test)]
fn upload_i420(
    input: &[u8],
    width: usize,
    height: usize,
    coded: (u32, u32),
    pitches: [u32; 3],
    offsets: [u32; 3],
    out: &mut [u8],
) -> Result<(), String> {
    let pixels = width.checked_mul(height).ok_or("invalid upload size")?;
    let length = pixels.checked_mul(3).ok_or("invalid upload size")? / 2;
    if input.len() != length {
        return Err("invalid VA upload length".into());
    }
    upload_planes(
        [
            &input[..pixels],
            &input[pixels..pixels + pixels / 4],
            &input[pixels + pixels / 4..],
        ],
        true,
        width,
        height,
        coded,
        pitches,
        offsets,
        out,
    )
}

#[cfg(test)]
fn upload_planes(
    planes: [&[u8]; 3],
    full_range: bool,
    width: usize,
    height: usize,
    coded: (u32, u32),
    pitches: [u32; 3],
    offsets: [u32; 3],
    out: &mut [u8],
) -> Result<(), String> {
    let pixels = width.checked_mul(height).ok_or("invalid upload size")?;
    if planes[0].len() != pixels
        || planes[1].len() != pixels / 4
        || planes[2].len() != pixels / 4
        || width > coded.0 as usize
        || height > coded.1 as usize
        || width % 2 != 0
        || height % 2 != 0
        || coded.0 % 2 != 0
        || coded.1 % 2 != 0
        || pitches[0] < coded.0
        || pitches[1] < coded.0
    {
        return Err("invalid VA upload geometry".into());
    }
    let coded_width = coded.0 as usize;
    for row in 0..coded.1 as usize {
        let offset = (offsets[0] as usize)
            .checked_add(
                row.checked_mul(pitches[0] as usize)
                    .ok_or("VA pitch overflow")?,
            )
            .ok_or("VA offset overflow")?;
        let end = offset.checked_add(coded_width).ok_or("VA row overflow")?;
        let dest = out
            .get_mut(offset..end)
            .ok_or("VA luma plane is too short")?;
        dest.fill(16);
        if row < height {
            let source = &planes[0][row * width..(row + 1) * width];
            if full_range {
                for (dest, value) in dest[..width].iter_mut().zip(source) {
                    *dest = 16 + ((u32::from(*value) * 219 + 127) / 255) as u8;
                }
            } else {
                dest[..width].copy_from_slice(source);
            }
        }
    }
    for row in 0..coded.1 as usize / 2 {
        let offset = (offsets[1] as usize)
            .checked_add(
                row.checked_mul(pitches[1] as usize)
                    .ok_or("VA pitch overflow")?,
            )
            .ok_or("VA offset overflow")?;
        let end = offset.checked_add(coded_width).ok_or("VA row overflow")?;
        let dest = out
            .get_mut(offset..end)
            .ok_or("VA chroma plane is too short")?;
        dest.fill(128);
        if row < height / 2 {
            for column in 0..width / 2 {
                let i = row * width / 2 + column;
                let (u, v) = (planes[1][i], planes[2][i]);
                dest[column * 2] = if full_range {
                    16 + ((u32::from(u) * 224 + 127) / 255) as u8
                } else {
                    u
                };
                dest[column * 2 + 1] = if full_range {
                    16 + ((u32::from(v) * 224 + 127) / 255) as u8
                } else {
                    v
                };
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accelerated_range_mapping_checks_every_byte_and_bgra_endpoints() {
        let src: Vec<u8> = (0..=255).collect();
        let mut actual = vec![0; 256];
        range_row::<219>(&src, &mut actual);
        for (&src, actual) in src.iter().zip(&actual) {
            assert_eq!(*actual, 16 + ((u32::from(src) * 219 + 127) / 255) as u8);
        }
        range_row::<224>(&src, &mut actual);
        for (&src, actual) in src.iter().zip(&actual) {
            assert_eq!(*actual, 16 + ((u32::from(src) * 224 + 127) / 255) as u8);
        }
        for (input, expected) in [(0, 16), (255, 235)] {
            let frame = bgra_frame(32, 2, 128, vec![input; 256]);
            let mut actual = vec![0; 96];
            upload_frame(&frame, (32, 2), [32, 32, 0], [0, 64, 0], &mut actual).unwrap();
            assert_eq!(&actual[..64], &[expected; 64]);
            assert_eq!(&actual[64..], &[128; 32]);
        }
    }
    #[test]
    fn fused_upload_matches_reference_for_all_formats_pitches_and_vector_tails() {
        let mut seed = 0x816be2a1u32;
        for (w, h) in [(2, 2), (6, 18), (14, 4), (64, 32), (1920, 8)] {
            for format in [PixelFormat::Bgra8, PixelFormat::Nv12, PixelFormat::I420] {
                for padding in [0, 1, 12] {
                    let stride = w * if format == PixelFormat::Bgra8 { 4 } else { 1 } + padding;
                    let length = match format {
                        PixelFormat::Bgra8 => stride * h,
                        PixelFormat::P010|PixelFormat::ScRgb16=>unreachable!(),
                        PixelFormat::Nv12 => stride * h * 3 / 2,
                        PixelFormat::I420 => stride * h + w * h / 2,
                    };
                    let data = (0..length)
                        .map(|_| {
                            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                            (seed >> 24) as u8
                        })
                        .collect();
                    let mut frame = bgra_frame(w, h, stride, data);
                    frame.format = format;
                    let coded = (w + 2, h + 2);
                    let pitch = w + 19;
                    let offsets = [7, 7 + pitch * coded.1 + 5, 0];
                    let length = (offsets[1] + pitch * coded.1 / 2 + 11) as usize;
                    let mut actual = vec![0xa7; length];
                    let mut expected = actual.clone();
                    upload_frame(&frame, coded, [pitch, pitch, 0], offsets, &mut actual).unwrap();
                    let mut bgra = None;
                    // Odd byte BGRA pitch used to take a different full-range
                    // conversion path. It now uses the same limited matrix;
                    // compare directly to the independent scalar path below.
                    if format != PixelFormat::Bgra8 || padding != 1 {
                        let pixels = (w * h) as usize;
                        if prepare_bgra(&mut bgra, &frame).unwrap() {
                            let yuv = bgra.as_ref().unwrap();
                            upload_planes(
                                [yuv.y(), yuv.u(), yuv.v()],
                                false,
                                w as usize,
                                h as usize,
                                coded,
                                [pitch, pitch, 0],
                                offsets,
                                &mut expected,
                            )
                            .unwrap();
                        } else {
                            let data = crate::convert::to_i420(&frame).unwrap();
                            upload_planes(
                                [
                                    &data[..pixels],
                                    &data[pixels..pixels + pixels / 4],
                                    &data[pixels + pixels / 4..],
                                ],
                                true,
                                w as usize,
                                h as usize,
                                coded,
                                [pitch, pitch, 0],
                                offsets,
                                &mut expected,
                            )
                            .unwrap();
                        }
                        assert_eq!(actual, expected, "{format:?} {w}x{h} padding={padding}");
                    }
                    if format == PixelFormat::Bgra8 {
                        expected.clone_from(&actual);
                        for row in (0..h as usize).step_by(2) {
                            bgra_row_pair(
                                &frame.data,
                                w as usize,
                                row,
                                stride as usize,
                                pitch as usize,
                                pitch as usize,
                                offsets[0] as usize,
                                offsets[1] as usize,
                                &mut expected,
                                0,
                            );
                        }
                        assert_eq!(
                            actual, expected,
                            "SIMD/scalar mismatch {w}x{h} padding={padding}"
                        );
                        #[cfg(target_arch = "x86_64")]
                        if std::is_x86_feature_detected!("sse4.1") {
                            unsafe {
                                x86::bgra_nv12(
                                    &frame.data,
                                    w as usize,
                                    h as usize,
                                    stride as usize,
                                    pitch as usize,
                                    pitch as usize,
                                    offsets[0] as usize,
                                    offsets[1] as usize,
                                    &mut expected,
                                )
                            };
                            assert_eq!(actual, expected, "AVX2/SSE4.1/scalar mismatch");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn fused_upload_rejects_bad_source_and_overlapping_or_short_planes_before_writing() {
        let frame = bgra_frame(2, 2, 8, vec![255; 16]);
        for (coded, pitches, offsets) in [
            ((2, 2), [2, 2, 0], [0, 2, 0]),
            ((2, 2), [1, 2, 0], [0, 4, 0]),
            ((2, 2), [2, 2, 0], [0, 100, 0]),
            ((2, 2), [u32::MAX, 2, 0], [u32::MAX, 4, 0]),
        ] {
            let mut out = vec![0xa7; 12];
            assert!(upload_frame(&frame, coded, pitches, offsets, &mut out).is_err());
            assert_eq!(out, [0xa7; 12]);
        }
        let mut invalid = frame;
        invalid.data = bytes::Bytes::from_static(&[0; 4]);
        let mut out = vec![0xa7; 12];
        assert!(upload_frame(&invalid, (2, 2), [2, 2, 0], [0, 4, 0], &mut out).is_err());
        assert_eq!(out, [0xa7; 12]);
    }

    fn bgra_frame(width: u32, height: u32, stride: u32, data: Vec<u8>) -> RawFrame {
        RawFrame {
            width,
            height,
            stride,
            data: data.into(),
            format: PixelFormat::Bgra8,
            pts: Timestamp(0),
            gpu: false,
            dma_buf: None,
            d3d11: None,
        }
    }
    #[test]
    fn library_bgra_matches_independent_limited_601_for_padded_rows_and_ignores_alpha() {
        let mut seed = 17u32;
        for (width, height, padding) in [(2, 2, 0), (6, 18, 12), (64, 32, 64), (1920, 8, 28)] {
            let stride = width * 4 + padding;
            let mut data = vec![0xee; (stride * height) as usize];
            for row in 0..height as usize {
                for x in 0..width as usize * 4 {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    data[row * stride as usize + x] = (seed >> 24) as u8;
                }
            }
            let mut frame = bgra_frame(width, height, stride, data);
            let mut buffer = None;
            assert!(prepare_bgra(&mut buffer, &frame).unwrap());
            let converted = buffer.as_ref().unwrap();
            let source = |x: usize, y: usize| {
                let i = y * stride as usize + x * 4;
                (
                    frame.data[i + 2] as f64,
                    frame.data[i + 1] as f64,
                    frame.data[i] as f64,
                )
            };
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let (r, g, b) = source(x, y);
                    let expected = 16.0 + (65.481 * r + 128.553 * g + 24.966 * b) / 255.0;
                    assert!((converted.y()[y * width as usize + x] as f64 - expected).abs() <= 2.0);
                }
            }
            for y in 0..height as usize / 2 {
                for x in 0..width as usize / 2 {
                    let (mut r, mut g, mut b) = (0.0, 0.0, 0.0);
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let c = source(x * 2 + dx, y * 2 + dy);
                            r += c.0 / 4.0;
                            g += c.1 / 4.0;
                            b += c.2 / 4.0;
                        }
                    }
                    let i = y * width as usize / 2 + x;
                    let u = 128.0 + (-37.797 * r - 74.203 * g + 112.0 * b) / 255.0;
                    let v = 128.0 + (112.0 * r - 93.786 * g - 18.214 * b) / 255.0;
                    assert!((converted.u()[i] as f64 - u).abs() <= 2.0);
                    assert!((converted.v()[i] as f64 - v).abs() <= 2.0);
                }
            }
            let expected = [
                converted.y().to_vec(),
                converted.u().to_vec(),
                converted.v().to_vec(),
            ];
            let mut changed = frame.data.to_vec();
            for row in 0..height as usize {
                for x in 0..width as usize {
                    changed[row * stride as usize + x * 4 + 3] ^= 255;
                }
            }
            frame.data = changed.into();
            assert!(prepare_bgra(&mut buffer, &frame).unwrap());
            let converted = buffer.as_ref().unwrap();
            assert_eq!(
                [converted.y(), converted.u(), converted.v()],
                [
                    expected[0].as_slice(),
                    expected[1].as_slice(),
                    expected[2].as_slice()
                ]
            );
        }
    }
    #[test]
    fn limited_upload_preserves_white_endpoints_and_padding_and_checks_input_before_access() {
        let frame = bgra_frame(2, 2, 8, vec![255; 16]);
        let mut buffer = None;
        assert!(prepare_bgra(&mut buffer, &frame).unwrap());
        let image = buffer.as_ref().unwrap();
        assert_eq!(image.y(), [235; 4]);
        assert_eq!(image.u(), [128]);
        assert_eq!(image.v(), [128]);
        let mut output = vec![0xac; 80];
        upload_planes(
            [image.y(), image.u(), image.v()],
            false,
            2,
            2,
            (4, 4),
            [8, 8, 0],
            [8, 48, 0],
            &mut output,
        )
        .unwrap();
        assert_eq!(&output[8..12], [235, 235, 16, 16]);
        assert_eq!(&output[16..20], [235, 235, 16, 16]);
        assert_eq!(&output[24..28], [16; 4]);
        assert_eq!(&output[48..52], [128; 4]);
        assert_eq!(&output[12..16], [0xac; 4]);
        let mut invalid = frame.clone();
        invalid.gpu = true;
        let mut unused = None;
        assert!(prepare_bgra(&mut unused, &invalid).is_err());
        assert!(unused.is_none());
        invalid = frame.clone();
        invalid.data = bytes::Bytes::from_static(&[0; 4]);
        assert!(prepare_bgra(&mut unused, &invalid).is_err());
        assert!(unused.is_none());
        let odd_stride = bgra_frame(2, 2, 9, vec![0; 18]);
        assert!(!prepare_bgra(&mut unused, &odd_stride).unwrap());
        assert!(unused.is_none());
        assert!(crate::convert::to_i420(&odd_stride).is_ok());
    }
    #[test]
    fn uploads_preserve_pitch_padding_and_map_full_range_to_limited_nv12() {
        let mut output = vec![0xac; 80];
        upload_i420(
            &[0, 255, 0, 255, 128, 128],
            2,
            2,
            (4, 4),
            [8, 8, 0],
            [8, 48, 0],
            &mut output,
        )
        .unwrap();
        assert_eq!(&output[8..12], &[16, 235, 16, 16]);
        assert_eq!(&output[16..20], &[16, 235, 16, 16]);
        assert_eq!(&output[24..28], &[16; 4]);
        assert_eq!(&output[48..52], &[128; 4]);
        assert_eq!(&output[56..60], &[128; 4]);
        assert_eq!(&output[12..16], &[0xac; 4]);
        assert!(upload_i420(&[0; 6], 2, 2, (4, 4), [2, 8, 0], [8, 48, 0], &mut output).is_err());
        assert!(upload_i420(&[0; 6], 2, 2, (4, 4), [8, 8, 0], [8, 48, 0], &mut [0; 4]).is_err());
    }
}
