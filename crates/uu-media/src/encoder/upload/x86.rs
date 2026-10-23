//! Fused BT.601 BGRA -> NV12 with runtime AVX2/SSE4.1 dispatch.
//! Vector loads and stores cover visible pixels only, never pitch padding.
use std::arch::x86_64::*;

#[target_feature(enable = "avx2")]
unsafe fn limited16<const SCALE: i16>(v: __m256i) -> __m256i {
    let v = _mm256_add_epi16(
        _mm256_mullo_epi16(v, _mm256_set1_epi16(SCALE)),
        _mm256_set1_epi16(127),
    );
    // Exact unsigned division by 255 for this bounded numerator range.
    let v = _mm256_add_epi16(
        _mm256_add_epi16(v, _mm256_set1_epi16(1)),
        _mm256_srli_epi16::<8>(v),
    );
    _mm256_add_epi16(_mm256_srli_epi16::<8>(v), _mm256_set1_epi16(16))
}

#[target_feature(enable = "avx2")]
pub(super) unsafe fn range_row<const SCALE: i16>(src: &[u8], dst: &mut [u8]) {
    let mut x = 0;
    while x + 32 <= src.len() {
        let lo = limited16::<SCALE>(_mm256_cvtepu8_epi16(_mm_loadu_si128(
            src.as_ptr().add(x).cast(),
        )));
        let hi = limited16::<SCALE>(_mm256_cvtepu8_epi16(_mm_loadu_si128(
            src.as_ptr().add(x + 16).cast(),
        )));
        let packed = _mm256_permute4x64_epi64::<216>(_mm256_packus_epi16(lo, hi));
        _mm256_storeu_si256(dst.as_mut_ptr().add(x).cast(), packed);
        x += 32;
    }
    for (dst, src) in dst[x..].iter_mut().zip(&src[x..]) {
        *dst = 16 + ((u32::from(*src) * SCALE as u32 + 127) / 255) as u8;
    }
}

#[target_feature(enable = "avx2")]
pub(super) unsafe fn interleave_row(u: &[u8], v: &[u8], dst: &mut [u8]) {
    let mut x = 0;
    while x + 16 <= u.len() {
        let u = limited16::<224>(_mm256_cvtepu8_epi16(_mm_loadu_si128(
            u.as_ptr().add(x).cast(),
        )));
        let v = limited16::<224>(_mm256_cvtepu8_epi16(_mm_loadu_si128(
            v.as_ptr().add(x).cast(),
        )));
        let u = _mm_packus_epi16(_mm256_castsi256_si128(u), _mm256_extracti128_si256::<1>(u));
        let v = _mm_packus_epi16(_mm256_castsi256_si128(v), _mm256_extracti128_si256::<1>(v));
        _mm_storeu_si128(dst.as_mut_ptr().add(x * 2).cast(), _mm_unpacklo_epi8(u, v));
        _mm_storeu_si128(
            dst.as_mut_ptr().add(x * 2 + 16).cast(),
            _mm_unpackhi_epi8(u, v),
        );
        x += 16;
    }
    for (dst, (&u, &v)) in dst[x * 2..]
        .chunks_exact_mut(2)
        .zip(u[x..].iter().zip(&v[x..]))
    {
        dst[0] = 16 + ((u32::from(u) * 224 + 127) / 255) as u8;
        dst[1] = 16 + ((u32::from(v) * 224 + 127) / 255) as u8;
    }
}

#[target_feature(enable = "avx2")]
unsafe fn channels16(a: __m256i, b: __m256i) -> [__m256i; 3] {
    let mask = _mm256_set1_epi32(255);
    let pack = |a, b| _mm256_permute4x64_epi64::<216>(_mm256_packus_epi32(a, b));
    [
        pack(_mm256_and_si256(a, mask), _mm256_and_si256(b, mask)),
        pack(
            _mm256_and_si256(_mm256_srli_epi32::<8>(a), mask),
            _mm256_and_si256(_mm256_srli_epi32::<8>(b), mask),
        ),
        pack(
            _mm256_and_si256(_mm256_srli_epi32::<16>(a), mask),
            _mm256_and_si256(_mm256_srli_epi32::<16>(b), mask),
        ),
    ]
}

#[target_feature(enable = "avx2")]
unsafe fn weighted16([b, g, r]: [__m256i; 3], c: [i16; 3]) -> __m256i {
    _mm256_add_epi16(
        _mm256_add_epi16(
            _mm256_mullo_epi16(b, _mm256_set1_epi16(c[0])),
            _mm256_mullo_epi16(g, _mm256_set1_epi16(c[1])),
        ),
        _mm256_mullo_epi16(r, _mm256_set1_epi16(c[2])),
    )
}

#[target_feature(enable = "avx2")]
pub(super) unsafe fn bgra_nv12_avx2(
    src: &[u8],
    w: usize,
    h: usize,
    stride: usize,
    yp: usize,
    uvp: usize,
    yo: usize,
    uvo: usize,
    out: &mut [u8],
) {
    for row in (0..h).step_by(2) {
        let mut x = 0;
        while x + 16 <= w {
            let load = |row| {
                let p = src.as_ptr().add(row * stride + x * 4);
                channels16(
                    _mm256_loadu_si256(p.cast()),
                    _mm256_loadu_si256(p.add(32).cast()),
                )
            };
            let top = load(row);
            let bottom = load(row + 1);
            for (dy, rgb) in [top, bottom].into_iter().enumerate() {
                // Y's positive numerator fits unsigned 16 bits (maximum
                // 56100); shift logically before narrowing to bytes.
                let y = _mm256_add_epi16(
                    _mm256_srli_epi16::<8>(weighted16(rgb, [25, 129, 66])),
                    _mm256_set1_epi16(16),
                );
                let y =
                    _mm_packus_epi16(_mm256_castsi256_si128(y), _mm256_extracti128_si256::<1>(y));
                _mm_storeu_si128(out.as_mut_ptr().add(yo + (row + dy) * yp + x).cast(), y);
            }
            let average = std::array::from_fn(|i| {
                let sums = _mm256_add_epi16(top[i], bottom[i]);
                _mm256_srli_epi16::<2>(_mm256_add_epi16(
                    _mm256_hadd_epi16(sums, _mm256_setzero_si256()),
                    _mm256_set1_epi16(2),
                ))
            });
            // Each chroma numerator stays in signed 16 bits. Average the
            // complete 2x2 block once, identically to the scalar reference.
            let u = _mm256_add_epi16(
                _mm256_srai_epi16::<8>(weighted16(average, [112, -74, -38])),
                _mm256_set1_epi16(128),
            );
            let v = _mm256_add_epi16(
                _mm256_srai_epi16::<8>(weighted16(average, [-18, -94, 112])),
                _mm256_set1_epi16(128),
            );
            let uv = _mm256_unpacklo_epi16(u, v);
            let uv = _mm_packus_epi16(
                _mm256_castsi256_si128(uv),
                _mm256_extracti128_si256::<1>(uv),
            );
            _mm_storeu_si128(out.as_mut_ptr().add(uvo + row / 2 * uvp + x).cast(), uv);
            x += 16;
        }
        super::bgra_row_pair(src, w, row, stride, yp, uvp, yo, uvo, out, x);
    }
}

#[target_feature(enable = "sse4.1")]
unsafe fn channels(p: __m128i) -> [__m128i; 3] {
    let mask = _mm_set1_epi32(255);
    [
        _mm_and_si128(p, mask),
        _mm_and_si128(_mm_srli_epi32::<8>(p), mask),
        _mm_and_si128(_mm_srli_epi32::<16>(p), mask),
    ]
}

#[target_feature(enable = "sse4.1")]
unsafe fn weighted([b, g, r]: [__m128i; 3], coefficients: [i32; 3]) -> __m128i {
    _mm_add_epi32(
        _mm_add_epi32(
            _mm_mullo_epi32(b, _mm_set1_epi32(coefficients[0])),
            _mm_mullo_epi32(g, _mm_set1_epi32(coefficients[1])),
        ),
        _mm_mullo_epi32(r, _mm_set1_epi32(coefficients[2])),
    )
}

#[target_feature(enable = "sse4.1")]
unsafe fn bytes(p: __m128i) -> __m128i {
    _mm_packus_epi16(_mm_packs_epi32(p, _mm_setzero_si128()), _mm_setzero_si128())
}

#[target_feature(enable = "sse4.1")]
pub(super) unsafe fn bgra_nv12(
    src: &[u8],
    w: usize,
    h: usize,
    stride: usize,
    yp: usize,
    uvp: usize,
    yo: usize,
    uvo: usize,
    out: &mut [u8],
) {
    for row in (0..h).step_by(2) {
        let mut x = 0;
        while x + 4 <= w {
            let top = channels(_mm_loadu_si128(
                src.as_ptr().add(row * stride + x * 4).cast(),
            ));
            let bottom = channels(_mm_loadu_si128(
                src.as_ptr().add((row + 1) * stride + x * 4).cast(),
            ));
            for (dy, rgb) in [top, bottom].into_iter().enumerate() {
                let y = _mm_add_epi32(
                    _mm_srli_epi32::<8>(weighted(rgb, [25, 129, 66])),
                    _mm_set1_epi32(16),
                );
                out.as_mut_ptr()
                    .add(yo + (row + dy) * yp + x)
                    .cast::<i32>()
                    .write_unaligned(_mm_cvtsi128_si32(bytes(y)));
            }
            let average = std::array::from_fn(|i| {
                let sums = _mm_add_epi32(top[i], bottom[i]);
                _mm_srli_epi32::<2>(_mm_add_epi32(
                    _mm_hadd_epi32(sums, _mm_setzero_si128()),
                    _mm_set1_epi32(2),
                ))
            });
            let u = _mm_add_epi32(
                _mm_srai_epi32::<8>(weighted(average, [112, -74, -38])),
                _mm_set1_epi32(128),
            );
            let v = _mm_add_epi32(
                _mm_srai_epi32::<8>(weighted(average, [-18, -94, 112])),
                _mm_set1_epi32(128),
            );
            let uv = bytes(_mm_unpacklo_epi32(u, v));
            out.as_mut_ptr()
                .add(uvo + row / 2 * uvp + x)
                .cast::<i32>()
                .write_unaligned(_mm_cvtsi128_si32(uv));
            x += 4;
        }
        super::bgra_row_pair(src, w, row, stride, yp, uvp, yo, uvo, out, x);
    }
}
