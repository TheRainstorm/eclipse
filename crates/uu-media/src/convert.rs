//! Colour-space conversion into the encoder's I420 input.
//!
//! Capture backends hand us BGRA (GDI/DXGI desktop duplication) or NV12
//! (VA-API and most GPU video paths). OpenH264 wants planar I420, so this
//! module is the single place that bridges the two.

use crate::codec::CodecError;
use crate::frame::{PixelFormat, RawFrame};

/// Convert any supported frame to tightly packed I420.
pub fn to_i420(frame: &RawFrame) -> Result<Vec<u8>, CodecError> {
    match frame.format {
        // Already packed I420 with stride == width is passed through; a
        // padded I420 would need re-packing, which no current backend emits.
        PixelFormat::I420 => {
            if frame.stride == frame.width {
                Ok(frame.data.to_vec())
            } else {
                Ok(repack_i420(&frame.data, frame.width, frame.height, frame.stride))
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
        RawFrame { width: w, height: h, format, stride, data: Bytes::from(data), pts: crate::Timestamp(0), gpu: false }
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
pub fn i420_to_rgba(src: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let cw = w / 2;
    let ch = h / 2;
    let mut out = vec![0u8; w * h * 4];
    let cl = |v: i32| v.clamp(0, 255) as u8;
    for j in 0..h {
        for i in 0..w {
            let yy = src[j * w + i] as i32;
            let uu = src[w * h + (j / 2) * cw + i / 2] as i32 - 128;
            let vv = src[w * h + cw * ch + (j / 2) * cw + i / 2] as i32 - 128;
            let r = yy + (1.402 * vv as f32) as i32;
            let g = yy - (0.344136 * uu as f32) as i32 - (0.714136 * vv as f32) as i32;
            let b = yy + (1.772 * uu as f32) as i32;
            let o = (j * w + i) * 4;
            out[o] = cl(r);
            out[o + 1] = cl(g);
            out[o + 2] = cl(b);
            out[o + 3] = 255;
        }
    }
    out
}
