//! SSE4.1 conversion preserving the scalar path's exact 16.16 arithmetic.
//! Dispatch is runtime checked; older CPUs keep the scalar implementation.
use std::arch::x86_64::*;

/// Caller validates complete, even-sized 4:2:0 planes and the RGBA output.
#[target_feature(enable = "sse4.1")]
pub(super) unsafe fn convert<const NV12: bool>(
    src: &[u8],
    w: usize,
    h: usize,
    stride: usize,
    out: &mut [u8],
) {
    let chroma = stride * h;
    let v_base = chroma + w * h / 4;
    for row in 0..h {
        let y = &src[row * stride..row * stride + w];
        let (u, v) = if NV12 {
            let start = chroma + row / 2 * stride;
            (&src[start..start + w], &[][..])
        } else {
            let start = row / 2 * (w / 2);
            (
                &src[chroma + start..chroma + start + w / 2],
                &src[v_base + start..v_base + start + w / 2],
            )
        };
        row_rgba::<NV12>(y, u, v, &mut out[row * w * 4..(row + 1) * w * 4]);
    }
}

#[target_feature(enable = "sse4.1")]
unsafe fn row_rgba<const NV12: bool>(y: &[u8], u: &[u8], v: &[u8], out: &mut [u8]) {
    let zero = _mm_setzero_si128();
    let center = _mm_set1_epi16(128);
    let mut x = 0;
    while x + 8 <= y.len() {
        let yy = _mm_unpacklo_epi8(_mm_loadl_epi64(y.as_ptr().add(x).cast()), zero);
        let (uu, vv) = if NV12 {
            let uv = _mm_loadl_epi64(u.as_ptr().add(x).cast());
            (
                _mm_and_si128(uv, _mm_set1_epi16(255)),
                _mm_srli_epi16::<8>(uv),
            )
        } else {
            let uu = _mm_cvtsi32_si128(u.as_ptr().add(x / 2).cast::<i32>().read_unaligned());
            let vv = _mm_cvtsi32_si128(v.as_ptr().add(x / 2).cast::<i32>().read_unaligned());
            (_mm_unpacklo_epi8(uu, zero), _mm_unpacklo_epi8(vv, zero))
        };
        let uu = _mm_sub_epi16(_mm_unpacklo_epi16(uu, uu), center);
        let vv = _mm_sub_epi16(_mm_unpacklo_epi16(vv, vv), center);
        let (r0, g0, b0) = rgb4(
            _mm_cvtepi16_epi32(yy),
            _mm_cvtepi16_epi32(uu),
            _mm_cvtepi16_epi32(vv),
        );
        let (r1, g1, b1) = rgb4(
            _mm_cvtepi16_epi32(_mm_srli_si128::<8>(yy)),
            _mm_cvtepi16_epi32(_mm_srli_si128::<8>(uu)),
            _mm_cvtepi16_epi32(_mm_srli_si128::<8>(vv)),
        );
        let r = _mm_packus_epi16(_mm_packs_epi32(r0, r1), zero);
        let g = _mm_packus_epi16(_mm_packs_epi32(g0, g1), zero);
        let b = _mm_packus_epi16(_mm_packs_epi32(b0, b1), zero);
        let rg = _mm_unpacklo_epi8(r, g);
        let ba = _mm_unpacklo_epi8(b, _mm_set1_epi8(-1));
        _mm_storeu_si128(
            out.as_mut_ptr().add(x * 4).cast(),
            _mm_unpacklo_epi16(rg, ba),
        );
        _mm_storeu_si128(
            out.as_mut_ptr().add(x * 4 + 16).cast(),
            _mm_unpackhi_epi16(rg, ba),
        );
        x += 8;
    }
    // Non-vector-width tails use the same scalar arithmetic, without padded
    // loads beyond the visible row (including a tightly packed final plane).
    for i in x..y.len() {
        let (uu, vv) = if NV12 {
            (u[i / 2 * 2], u[i / 2 * 2 + 1])
        } else {
            (u[i / 2], v[i / 2])
        };
        let (r, g, b) = super::yuv_to_rgb(y[i] as i32, uu as i32 - 128, vv as i32 - 128);
        out[i * 4..i * 4 + 4].copy_from_slice(&[
            r.clamp(0, 255) as u8,
            g.clamp(0, 255) as u8,
            b.clamp(0, 255) as u8,
            255,
        ]);
    }
}

#[target_feature(enable = "sse4.1")]
unsafe fn rgb4(y: __m128i, u: __m128i, v: __m128i) -> (__m128i, __m128i, __m128i) {
    let r = _mm_add_epi32(
        y,
        _mm_srai_epi32::<16>(_mm_mullo_epi32(v, _mm_set1_epi32(91881))),
    );
    let g = _mm_sub_epi32(
        y,
        _mm_srai_epi32::<16>(_mm_add_epi32(
            _mm_mullo_epi32(u, _mm_set1_epi32(22554)),
            _mm_mullo_epi32(v, _mm_set1_epi32(46802)),
        )),
    );
    let b = _mm_add_epi32(
        y,
        _mm_srai_epi32::<16>(_mm_mullo_epi32(u, _mm_set1_epi32(116130))),
    );
    (r, g, b)
}
