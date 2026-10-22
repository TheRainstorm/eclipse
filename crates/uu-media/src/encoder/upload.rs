//! Validated limited BT.601 NV12 uploads shared by VA-API and Windows MFT.
#[cfg(test)]
use crate::Timestamp;
use crate::{PixelFormat, RawFrame};
use openh264::formats::{BGRA8Source, RGB8Source, RGBSource, YUVBuffer, YUVSource};

// The library's standard BGRA adapter supports padded rows in whole pixels.
// Odd byte pitches continue through the existing conversion path.
struct BgraInput<'a>(&'a RawFrame);
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
impl BGRA8Source for BgraInput<'_> {}

pub(super) fn prepare_bgra(
    buffer: &mut Option<YUVBuffer>,
    frame: &RawFrame,
) -> Result<bool, String> {
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

pub(super) fn upload_planes(
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
