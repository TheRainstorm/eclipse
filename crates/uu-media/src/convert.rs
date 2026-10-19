//! Colour-space conversion into the encoder's I420 input.
//!
//! Capture backends hand us BGRA (GDI/DXGI desktop duplication) or NV12
//! (VA-API and most GPU video paths). OpenH264 wants planar I420, so this
//! module is the single place that bridges the two.

use crate::codec::CodecError;
use crate::frame::{PixelFormat, RawFrame};

#[cfg(target_arch = "x86_64")]
mod x86;

/// Convert any supported frame to tightly packed I420.
pub fn to_i420(frame: &RawFrame) -> Result<Vec<u8>, CodecError> {
    let invalid =
        || CodecError::Backend("invalid or incomplete CPU frame for I420 conversion".into());
    let (w, h, s) = (
        frame.width as usize,
        frame.height as usize,
        frame.stride as usize,
    );
    if frame.gpu || !(2..=8192).contains(&w) || !(2..=8192).contains(&h) || w % 2 != 0 || h % 2 != 0
    {
        return Err(invalid());
    }
    let row = if frame.format == PixelFormat::Bgra8 {
        w.checked_mul(4)
    } else {
        Some(w)
    }
    .ok_or_else(invalid)?;
    let plane = s.checked_mul(h).ok_or_else(invalid)?;
    let needed = match frame.format {
        PixelFormat::Bgra8 => Some(plane),
        PixelFormat::Nv12 => plane.checked_add(s.checked_mul(h / 2).ok_or_else(invalid)?),
        PixelFormat::I420 => plane.checked_add(w * h / 2),
    }
    .filter(|n| *n <= 256 * 1024 * 1024)
    .ok_or_else(invalid)?;
    if s < row || frame.data.len() < needed {
        return Err(invalid());
    }
    match frame.format {
        // Already packed I420 with stride == width is passed through; a
        // padded I420 would need re-packing, which no current backend emits.
        PixelFormat::I420 => {
            if frame.stride == frame.width {
                Ok(frame.data[..w * h * 3 / 2].to_vec())
            } else {
                Ok(repack_i420(
                    &frame.data,
                    frame.width,
                    frame.height,
                    frame.stride,
                ))
            }
        }
        PixelFormat::Nv12 => Ok(nv12_to_i420(
            &frame.data,
            frame.width,
            frame.height,
            frame.stride,
        )),
        PixelFormat::Bgra8 => Ok(bgra_to_i420(
            &frame.data,
            frame.width,
            frame.height,
            frame.stride,
        )),
    }
}

fn repack_i420(src: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8> {
    let (w, h, s) = (width as usize, height as usize, stride as usize);
    let mut out = vec![0u8; w * h * 3 / 2];
    for row in 0..h {
        out[row * w..row * w + w].copy_from_slice(&src[row * s..row * s + w]);
    }
    let (csw, csh) = (w / 2, h / 2);
    let y_plane = s * h;
    let mut u = vec![0u8; csw * csh];
    let mut v = vec![0u8; csw * csh];
    for row in 0..csh {
        u[row * csw..row * csw + csw]
            .copy_from_slice(&src[y_plane + row * csw..y_plane + row * csw + csw]);
    }
    // V plane follows U; offsets are expressed in the source's own layout.
    let u_plane = y_plane + csw * csh;
    for row in 0..csh {
        v[row * csw..row * csw + csw]
            .copy_from_slice(&src[u_plane + row * csw..u_plane + row * csw + csw]);
    }
    let dw = w;
    out[dw * h..dw * h + csw * csh].copy_from_slice(&u);
    out[dw * h + csw * csh..].copy_from_slice(&v);
    out
}

/// NV12 (`Y` plane then interleaved `UV`) to I420 (`Y`, `U`, `V`).
pub fn nv12_to_i420(src: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8> {
    let (w, h, s) = (width as usize, height as usize, stride as usize);
    let (csw, csh) = (w / 2, h / 2);
    let mut out = vec![0u8; w * h * 3 / 2];
    for row in 0..h {
        out[row * w..row * w + w].copy_from_slice(&src[row * s..row * s + w]);
    }
    let uv_base = s * h;
    let (u_off, v_off) = (w * h, w * h + csw * csh);
    for row in 0..csh {
        for col in 0..csw {
            out[u_off + row * csw + col] = src[uv_base + row * s + col * 2];
            out[v_off + row * csw + col] = src[uv_base + row * s + col * 2 + 1];
        }
    }
    out
}

/// BGRA to I420 using the BT.601 full-range coefficients.
pub fn bgra_to_i420(src: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8> {
    let (w, h, s) = (width as usize, height as usize, stride as usize);
    let (csw, csh) = (w / 2, h / 2);
    let mut out = vec![0u8; w * h * 3 / 2];
    let (u_off, v_off) = (w * h, w * h + csw * csh);

    let luma = |out: &mut [u8], row: usize| {
        for col in 0..w {
            let p = row * s + col * 4;
            let (b, g, r) = (src[p] as f32, src[p + 1] as f32, src[p + 2] as f32);
            let y = 0.299 * r + 0.587 * g + 0.114 * b;
            out[row * w + col] = y.round().clamp(0.0, 255.0) as u8;
        }
    };
    for row in 0..h {
        luma(&mut out, row);
    }

    // Chroma is averaged over each 2x2 luma block, which is what the
    // 4:2:0 subsampling actually means.
    for cy in 0..csh {
        for cx in 0..csw {
            let (mut u_sum, mut v_sum) = (0.0f32, 0.0f32);
            for dy in 0..2 {
                for dx in 0..2 {
                    let p = (cy * 2 + dy) * s + (cx * 2 + dx) * 4;
                    let (b, g, r) = (src[p] as f32, src[p + 1] as f32, src[p + 2] as f32);
                    // BT.601 full-range chroma.
                    u_sum += -0.168_736 * r - 0.331_264 * g + 0.5 * b + 128.0;
                    v_sum += 0.5 * r - 0.418_688 * g - 0.081_312 * b + 128.0;
                }
            }
            let idx = cy * csw + cx;
            out[u_off + idx] = (u_sum / 4.0).round().clamp(0.0, 255.0) as u8;
            out[v_off + idx] = (v_sum / 4.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn frame(format: PixelFormat, w: u32, h: u32, stride: u32, data: Vec<u8>) -> RawFrame {
        RawFrame {
            width: w,
            height: h,
            format,
            stride,
            data: Bytes::from(data),
            pts: crate::Timestamp(0),
            gpu: false,
        }
    }

    #[test]
    fn nv12_uv_deinterleaves_correctly() {
        // 2x2 luma; one chroma pair (U=10, V=20).
        let data = vec![1, 2, 3, 4, 10, 20];
        let out = nv12_to_i420(&data, 2, 2, 2);
        assert_eq!(out.len(), 2 * 2 * 3 / 2);
        assert_eq!(&out[0..4], &[1, 2, 3, 4]);
        assert_eq!(out[4], 10); // U plane
        assert_eq!(out[5], 20); // V plane
    }

    #[test]
    fn bgra_white_is_full_luma() {
        let data = vec![255u8; 2 * 2 * 4];
        let out = bgra_to_i420(&data, 2, 2, 8);
        assert_eq!(&out[0..4], &[255, 255, 255, 255]);
    }

    #[test]
    fn bgra_black_is_zero_luma_and_neutral_chroma() {
        let mut data = vec![0u8; 2 * 2 * 4];
        for px in data.chunks_exact_mut(4) {
            px[3] = 255; // opaque black
        }
        let out = bgra_to_i420(&data, 2, 2, 8);
        assert_eq!(&out[0..4], &[0, 0, 0, 0]);
        assert_eq!(out[4], 128);
        assert_eq!(out[5], 128);
    }

    #[test]
    fn bgra_stride_padding_is_respected() {
        // 2x2 with 4 bytes of padding per row (stride 12).
        let mut data = vec![0u8; 12 * 2];
        for row in 0..2 {
            for col in 0..2 {
                let p = row * 12 + col * 4;
                data[p] = 255; // B
            }
        }
        let out = bgra_to_i420(&data, 2, 2, 12);
        // Pure blue: Y = 0.114*255 ~= 29.
        assert!((out[0] as i32 - 29).abs() <= 1, "got {}", out[0]);
    }

    #[test]
    fn i420_passthrough_when_tightly_packed() {
        let data = vec![7u8; 4 * 4 * 3 / 2];
        let f = frame(PixelFormat::I420, 4, 4, 4, data.clone());
        assert_eq!(to_i420(&f).unwrap(), data);
    }
}

/// Convert tightly packed I420 to RGBA (for the GUI texture upload path).
///
/// BT.601 with 16.16 fixed point (integer, no float): this runs per frame on
/// the decode path, so it must stay cheap.
pub fn i420_to_rgba(src: &[u8], width: u32, height: u32) -> Vec<u8> {
    i420_to_rgba_strided(src, width, height, width)
}

/// Convert a decoded frame of *any* supported layout to RGBA8.
///
/// The software decoder emits tightly packed I420, while the hardware
/// backends hand back GPU-style NV12 (D3D11 staging rows and VA-API surfaces
/// are pitched, so the stride must be honoured). Keeping the dispatch here
/// means the transport/GUI code never has to know which backend produced the
/// frame.
pub fn frame_to_rgba(frame: &RawFrame) -> Vec<u8> {
    match frame.format {
        PixelFormat::I420 => {
            i420_to_rgba_strided(&frame.data, frame.width, frame.height, frame.stride)
        }
        PixelFormat::Nv12 => nv12_to_rgba(&frame.data, frame.width, frame.height, frame.stride),
        PixelFormat::Bgra8 => bgra_to_rgba(&frame.data, frame.width, frame.height, frame.stride),
    }
}

/// I420 to RGBA8 with pitched luma and packed U/V planes, as emitted by
/// our decoder adapters. 4:2:0 visible dimensions must both be even.
pub fn i420_to_rgba_strided(src: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let s = stride.max(width) as usize;
    let cw = w / 2;
    let ch = h / 2;
    let mut out = vec![0u8; w * h * 4];
    // Planes are stacked with the source's own pitch: Y at 0, U at s*h,
    // V at s*h + cw*ch. A tightly packed frame has s == w.
    let u_base = s * h;
    let v_base = u_base + cw * ch;
    if w % 2 != 0 || h % 2 != 0 || src.len() < u_base + cw * ch * 2 {
        return out;
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("sse4.1") {
        // Complete even-sized planes and output are validated above. The
        // kernel loads only complete 8-pixel groups; tails stay scalar.
        unsafe {
            x86::convert::<false>(src, w, h, s, &mut out);
        }
        return out;
    }
    for j in 0..h {
        let yrow = j * s;
        let crow = u_base + (j >> 1) * cw;
        let vrow = v_base + (j >> 1) * cw;
        let orow = j * w * 4;
        for i in 0..w {
            let y = src[yrow + i] as i32;
            let ci = i >> 1;
            let u = src[crow + ci] as i32 - 128;
            let v = src[vrow + ci] as i32 - 128;
            let (r, g, b) = yuv_to_rgb(y, u, v);
            let o = orow + i * 4;
            out[o] = r.clamp(0, 255) as u8;
            out[o + 1] = g.clamp(0, 255) as u8;
            out[o + 2] = b.clamp(0, 255) as u8;
            out[o + 3] = 255;
        }
    }
    out
}

/// NV12 (interleaved UV, pitched rows) to RGBA8.
pub fn nv12_to_rgba(src: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let s = stride.max(width) as usize;
    let ch = h / 2;
    let mut out = vec![0u8; w * h * 4];
    let uv_base = s * h;
    if w % 2 != 0 || h % 2 != 0 || src.len() < uv_base + s * ch {
        return out;
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("sse4.1") {
        // Pitched UV starts after every allocated luma row, as in the scalar
        // path; no SIMD load may cross the last visible chroma pair.
        unsafe {
            x86::convert::<true>(src, w, h, s, &mut out);
        }
        return out;
    }
    for j in 0..h {
        let yrow = j * s;
        let uvrow = uv_base + (j >> 1) * s;
        let orow = j * w * 4;
        for i in 0..w {
            let y = src[yrow + i] as i32;
            let ci = (i >> 1) * 2;
            let u = src[uvrow + ci] as i32 - 128;
            let v = src[uvrow + ci + 1] as i32 - 128;
            let (r, g, b) = yuv_to_rgb(y, u, v);
            let o = orow + i * 4;
            out[o] = r.clamp(0, 255) as u8;
            out[o + 1] = g.clamp(0, 255) as u8;
            out[o + 2] = b.clamp(0, 255) as u8;
            out[o + 3] = 255;
        }
    }
    out
}

/// BGRA (top-down, pitched rows) to RGBA8: a channel swap per pixel.
pub fn bgra_to_rgba(src: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let s = stride.max(width * 4) as usize;
    let mut out = vec![0u8; w * h * 4];
    let needed = if h == 0 { 0 } else { (h - 1) * s + w * 4 };
    if src.len() < needed {
        return out;
    }
    for j in 0..h {
        let srow = j * s;
        let orow = j * w * 4;
        for i in 0..w {
            let p = srow + i * 4;
            out[orow + i * 4] = src[p + 2];
            out[orow + i * 4 + 1] = src[p + 1];
            out[orow + i * 4 + 2] = src[p];
            out[orow + i * 4 + 3] = 255;
        }
    }
    out
}

/// BT.601 YCbCr (centred chroma) to RGB, 16.16 fixed point.
#[inline]
fn yuv_to_rgb(y: i32, u: i32, v: i32) -> (i32, i32, i32) {
    (
        y + ((91881 * v) >> 16),
        y - ((22554 * u + 46802 * v) >> 16),
        y + ((116130 * u) >> 16),
    )
}

#[cfg(test)]
mod rgba_tests {
    use super::*;
    use bytes::Bytes;

    fn frame(format: PixelFormat, w: u32, h: u32, stride: u32, data: Vec<u8>) -> RawFrame {
        RawFrame {
            width: w,
            height: h,
            format,
            stride,
            data: Bytes::from(data),
            pts: crate::Timestamp(0),
            gpu: false,
        }
    }

    #[test]
    fn tight_i420_wrapper_matches_strided() {
        let data = vec![100u8; 4 * 4 * 3 / 2];
        assert_eq!(
            i420_to_rgba(&data, 4, 4),
            i420_to_rgba_strided(&data, 4, 4, 4)
        );
    }

    #[test]
    fn strided_i420_ignores_row_padding() {
        // 2x2 luma with 2 bytes of padding per luma row; chroma rows are
        // padded to the same stride.
        let mut data = vec![0u8; 6 * 2 + 1 * 1 * 2];
        data[0] = 200;
        data[1] = 200;
        data[6] = 200;
        data[7] = 200;
        data[12] = 128; // U at s*h
        data[13] = 128; // V at s*h + cw*ch
        let rgba = i420_to_rgba_strided(&data, 2, 2, 6);
        assert_eq!(&rgba[0..4], &[200, 200, 200, 255]);
        assert_eq!(&rgba[8..12], &[200, 200, 200, 255]);
    }

    #[test]
    fn nv12_to_rgba_deinterleaves_uv() {
        // 2x2: luma 4x50, one UV pair (U=128 -> neutral, V=128 -> neutral).
        let data = vec![50, 50, 50, 50, 128, 128];
        let rgba = nv12_to_rgba(&data, 2, 2, 2);
        assert_eq!(&rgba[0..4], &[50, 50, 50, 255]);
    }

    #[test]
    fn nv12_red_shifts_towards_red() {
        // U=90 (below centre), V=240: strongly red.
        let data = vec![80, 80, 80, 80, 90, 240];
        let rgba = nv12_to_rgba(&data, 2, 2, 2);
        assert!(
            rgba[0] > rgba[2],
            "R {} should exceed B {}",
            rgba[0],
            rgba[2]
        );
        assert_eq!(rgba[3], 255);
    }

    #[test]
    fn bgra_to_rgba_swaps_channels() {
        // BGRA blue -> RGBA: R/G/B = 0,0,255.
        let data = vec![255, 0, 0, 0];
        let rgba = bgra_to_rgba(&data, 1, 1, 4);
        assert_eq!(rgba, vec![0, 0, 255, 255]);
    }

    #[test]
    fn frame_to_rgba_dispatches_on_format() {
        let i420 = frame(PixelFormat::I420, 2, 2, 2, vec![10, 10, 10, 10, 128, 128]);
        let nv12 = frame(PixelFormat::Nv12, 2, 2, 2, vec![10, 10, 10, 10, 128, 128]);
        assert_eq!(frame_to_rgba(&i420), frame_to_rgba(&nv12));
    }
}

#[cfg(test)]
mod conversion_equivalence_tests {
    use super::*;

    fn reference(src: &[u8], w: usize, h: usize, stride: usize, nv12: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(w * h * 4);
        for row in 0..h {
            for col in 0..w {
                let chroma = stride * h + row / 2 * if nv12 { stride } else { w / 2 };
                let u = src[chroma + col / 2 * if nv12 { 2 } else { 1 }];
                let v = if nv12 {
                    src[chroma + col / 2 * 2 + 1]
                } else {
                    src[chroma + w * h / 4 + col / 2]
                };
                let (r, g, b) = yuv_to_rgb(
                    src[row * stride + col] as i32,
                    u as i32 - 128,
                    v as i32 - 128,
                );
                out.extend_from_slice(&[
                    r.clamp(0, 255) as u8,
                    g.clamp(0, 255) as u8,
                    b.clamp(0, 255) as u8,
                    255,
                ]);
            }
        }
        out
    }
    #[test]
    fn pitched_yuv_with_vector_tails_matches_scalar_reference_byte_for_byte() {
        let mut seed = 12345u32;
        for w in [2usize, 4, 6, 8, 10, 14, 16, 18, 30, 32, 66] {
            for h in [2usize, 4, 6, 10] {
                for padding in [0, 1, 8] {
                    for nv12 in [false, true] {
                        let stride = w + padding;
                        let len = if nv12 {
                            stride * h * 3 / 2
                        } else {
                            stride * h + w * h / 2
                        };
                        let src: Vec<u8> = (0..len)
                            .map(|_| {
                                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                                (seed >> 24) as u8
                            })
                            .collect();
                        let actual = if nv12 {
                            nv12_to_rgba(&src, w as u32, h as u32, stride as u32)
                        } else {
                            i420_to_rgba_strided(&src, w as u32, h as u32, stride as u32)
                        };
                        assert_eq!(
                            actual,
                            reference(&src, w, h, stride, nv12),
                            "{w}x{h} stride={stride} NV12={nv12}"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn every_chroma_pair_preserves_scalar_rounding_and_clipping() {
        let y = [
            0, 1, 15, 16, 64, 128, 235, 255, 255, 235, 128, 64, 16, 15, 1, 0,
        ];
        for u in 0..=255u8 {
            for v in 0..=255u8 {
                let mut nv12 = y.to_vec();
                nv12.extend([u, v, u, v, u, v, u, v]);
                let mut i420 = y.to_vec();
                i420.extend([u; 4]);
                i420.extend([v; 4]);
                let expected = reference(&nv12, 8, 2, 8, true);
                assert_eq!(nv12_to_rgba(&nv12, 8, 2, 8), expected, "NV12 U={u} V={v}");
                assert_eq!(i420_to_rgba(&i420, 8, 2), expected, "I420 U={u} V={v}");
            }
        }
    }
    #[test]
    fn invalid_420_planes_do_not_read_past_visible_data() {
        for (w, h) in [(3, 2), (2, 3), (1, 1)] {
            let src = vec![128; w * h * 2 + 16];
            assert_eq!(
                nv12_to_rgba(&src, w as u32, h as u32, w as u32),
                vec![0; w * h * 4]
            );
            assert_eq!(i420_to_rgba(&src, w as u32, h as u32), vec![0; w * h * 4]);
        }
        for (w, h) in [(3, 2), (2, 3), (1, 1), (4, 4)] {
            let src = vec![128; w * h * 3 / 2 - 1];
            assert_eq!(
                nv12_to_rgba(&src, w as u32, h as u32, w as u32),
                vec![0; w * h * 4]
            );
            assert_eq!(i420_to_rgba(&src, w as u32, h as u32), vec![0; w * h * 4]);
        }
    }
}

/// Convert with the stream's range/matrix, preserving the vector upload path.
pub fn frame_to_rgba_with_color(frame: &RawFrame, color: crate::color::YuvColor) -> Vec<u8> {
    if color == crate::color::YuvColor::Full601 || frame.format == PixelFormat::Bgra8 {
        return frame_to_rgba(frame);
    }
    let (w, h, s) = (
        frame.width as usize,
        frame.height as usize,
        frame.stride.max(frame.width) as usize,
    );
    let nv12 = frame.format == PixelFormat::Nv12;
    let mut out = vec![0; w * h * 4];
    let needed = if nv12 {
        s * h + s * (h / 2)
    } else {
        s * h + w * h / 2
    };
    if w % 2 != 0 || h % 2 != 0 || frame.data.len() < needed {
        return out;
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("sse4.1") {
        unsafe {
            if nv12 {
                x86::convert_color::<true>(&frame.data, w, h, s, &mut out, color)
            } else {
                x86::convert_color::<false>(&frame.data, w, h, s, &mut out, color)
            }
        }
        return out;
    }
    for y in 0..h {
        for x in 0..w {
            let (u, v) = if nv12 {
                let i = s * h + y / 2 * s + x / 2 * 2;
                (frame.data[i], frame.data[i + 1])
            } else {
                let i = y / 2 * (w / 2) + x / 2;
                (frame.data[s * h + i], frame.data[s * h + w * h / 4 + i])
            };
            let (r, g, b) = color.rgb(frame.data[y * s + x] as i32, u as i32 - 128, v as i32 - 128);
            out[(y * w + x) * 4..(y * w + x + 1) * 4].copy_from_slice(&[
                r.clamp(0, 255) as u8,
                g.clamp(0, 255) as u8,
                b.clamp(0, 255) as u8,
                255,
            ]);
        }
    }
    out
}

#[cfg(test)]
mod encode_input_tests {
    use super::*;
    use crate::Timestamp;
    #[test]
    fn invalid_capture_planes_are_rejected_before_conversion_or_ffi() {
        for (w, h, stride, len) in [
            (0, 0, 0, 0),
            (3, 2, 12, 24),
            (2, 2, 1, 6),
            (2, 2, 8, 15),
            (8194, 2, 32776, 65552),
        ] {
            let frame = RawFrame {
                width: w,
                height: h,
                stride,
                format: PixelFormat::Bgra8,
                data: vec![0; len].into(),
                pts: Timestamp(0),
                gpu: false,
            };
            assert!(to_i420(&frame).is_err());
        }
        let frame = RawFrame {
            width: 2,
            height: 2,
            stride: 2,
            format: PixelFormat::I420,
            data: vec![128; 7].into(),
            pts: Timestamp(0),
            gpu: false,
        };
        assert_eq!(to_i420(&frame).unwrap().len(), 6);
        let mut gpu = frame.clone();
        gpu.gpu = true;
        assert!(to_i420(&gpu).is_err());
    }
}
