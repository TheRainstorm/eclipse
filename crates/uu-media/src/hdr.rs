//! HDR10 signal math. scRGB is linear BT.709 with 1.0 = 80 cd/m².
//! PQ uses absolute luminance; it must never be interpreted as an SDR gamma.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub primaries: [[f32; 2]; 3],
    pub white: [f32; 2],
    pub min_nits: f32,
    pub max_nits: f32,
    pub max_full_frame_nits: f32,
    pub sdr_white_nits: f32,
}
impl Default for Metadata {
    fn default() -> Self {
        Self {
            primaries: [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]],
            white: [0.3127, 0.3290],
            min_nits: 0.,
            max_nits: 1000.,
            max_full_frame_nits: 203.,
            sdr_white_nits: 203.,
        }
    }
}
impl Metadata {
    /// Display drivers may omit EDID mastering data. HDR detection comes from
    /// the OS color space; missing metadata must not disable float capture.
    pub fn with_display_defaults(mut self) -> Self {
        let fallback = Self::default();
        if !self.primaries.iter().all(|p| chromaticity(*p)) {
            self.primaries = fallback.primaries;
        }
        if !chromaticity(self.white) {
            self.white = fallback.white;
        }
        if !self.min_nits.is_finite() || !(0.0..=10.0).contains(&self.min_nits) {
            self.min_nits = fallback.min_nits;
        }
        if !self.max_nits.is_finite() || !(80.0..=10000.0).contains(&self.max_nits) {
            self.max_nits = fallback.max_nits;
        }
        if !self.max_full_frame_nits.is_finite()
            || !(self.min_nits..=self.max_nits).contains(&self.max_full_frame_nits)
        {
            self.max_full_frame_nits = self.max_nits.min(fallback.max_full_frame_nits);
        }
        if !self.sdr_white_nits.is_finite() || !(40.0..=1000.0).contains(&self.sdr_white_nits) {
            self.sdr_white_nits = fallback.sdr_white_nits;
        }
        self
    }
    pub fn valid(&self) -> bool {
        self.primaries.iter().all(|p| chromaticity(*p))
            && chromaticity(self.white)
            && self.min_nits.is_finite()
            && (0.0..=10.0).contains(&self.min_nits)
            && self.max_nits.is_finite()
            && (80.0..=10_000.0).contains(&self.max_nits)
            && self.max_full_frame_nits.is_finite()
            && (0.0..=self.max_nits).contains(&self.max_full_frame_nits)
            && self.sdr_white_nits.is_finite()
            && (40.0..=1_000.0).contains(&self.sdr_white_nits)
    }
}
fn chromaticity(p: [f32; 2]) -> bool {
    p.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
        && p[0] + p[1] > 0.
        && p[0] + p[1] <= 1.001
}
/// SMPTE ST 2084 EOTF, returning absolute cd/m².
pub fn pq_to_nits(pq: f32) -> f32 {
    let p = pq.clamp(0., 1.).powf(32. / 2523.);
    ((p - 3424. / 4096.).max(0.) / (2413. / 128. - 2392. / 128. * p)).powf(16384. / 2610.) * 10_000.
}
/// Inverse ST 2084, accepting absolute cd/m².
pub fn nits_to_pq(nits: f32) -> f32 {
    let p = (nits / 10_000.).clamp(0., 1.).powf(2610. / 16384.);
    ((3424. / 4096. + 2413. / 128. * p) / (1. + 2392. / 128. * p)).powf(2523. / 32.)
}
pub fn bt2020_to_709(rgb: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = rgb;
    [
        1.660491 * r - 0.587641 * g - 0.072850 * b,
        -0.124550 * r + 1.132900 * g - 0.008349 * b,
        -0.018151 * r - 0.100579 * g + 1.118730 * b,
    ]
}
pub fn bt709_to_2020(rgb: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = rgb;
    [
        0.627404 * r + 0.329283 * g + 0.043313 * b,
        0.069097 * r + 0.919540 * g + 0.011362 * b,
        0.016391 * r + 0.088013 * g + 0.895595 * b,
    ]
}
pub fn linear_to_srgb(value: f32) -> f32 {
    if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1. / 2.4) - 0.055
    }
}
/// A continuous highlight shoulder for the SDR fallback. Preserve darker
/// values and channel ratios, with the source desktop's configured SDR white.
pub fn sc_rgb_to_sdr(rgb: [f32; 3], white_nits: f32) -> [f32; 3] {
    let rgb = rgb.map(|v| (v * 80. / white_nits).max(0.));
    let peak = rgb.into_iter().fold(0_f32, f32::max);
    let mapped = if peak <= 0.75 {
        rgb
    } else {
        let shoulder = 1. - 0.25 * (-4. * (peak - 0.75)).exp();
        rgb.map(|v| (v / peak) * shoulder)
    };
    mapped.map(|v| linear_to_srgb(v).clamp(0., 1.))
}
/// P010 MSB-aligned limited-range BT.2020 NCL / PQ -> linear scRGB.
pub fn p010_to_sc_rgb(y: u16, u: u16, v: u16, full: bool) -> [f32; 3] {
    let (offset, luma, chroma) = if full {
        (0., 1023., 1023.)
    } else {
        (64., 876., 896.)
    };
    let y = (f32::from(y >> 6) - offset) / luma;
    let u = (f32::from(u >> 6) - 512.) / chroma;
    let v = (f32::from(v >> 6) - 512.) / chroma;
    let rgb = [
        y + 1.4746 * v,
        y - 0.164553 * u - 0.571353 * v,
        y + 1.8814 * u,
    ]
    .map(|v| pq_to_nits(v) / 80.);
    bt2020_to_709(rgb)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pq_absolute_luminance_and_gamut_roundtrip() {
        for (nits, pq) in [
            (0., 0.),
            (100., 0.5080784),
            (203., 0.5806889),
            (1000., 0.7518271),
            (10_000., 1.),
        ] {
            assert!((nits_to_pq(nits) - pq).abs() < 0.00002, "{nits}");
            assert!(
                (pq_to_nits(pq) - nits).abs() < 0.8_f32.max(nits * 0.0001),
                "{nits}"
            );
        }
        for rgb in [
            [1., 0., 0.],
            [0., 1., 0.],
            [0., 0., 1.],
            [12.5, 12.5, 12.5],
            [0.1, 0.4, 8.],
        ] {
            let actual = bt2020_to_709(bt709_to_2020(rgb));
            for i in 0..3 {
                assert!((actual[i] - rgb[i]).abs() < 0.0001);
            }
        }
        let code = (64. + 876. * nits_to_pq(1000.)).round() as u16;
        let rgb = p010_to_sc_rgb(code << 6, 512 << 6, 512 << 6, false);
        for v in rgb {
            assert!((v - 12.5).abs() < 0.1);
        }
    }
    #[test]
    fn sdr_highlight_shoulder_preserves_dark_values_without_clipping_hdr_input() {
        let dark = sc_rgb_to_sdr([0.25, 0.25, 0.25], 80.);
        assert!((dark[0] - 0.5370987).abs() < 0.00001);
        let mut previous = 0.;
        for n in 0..1000 {
            let value = sc_rgb_to_sdr([n as f32 / 20.; 3], 203.)[0];
            assert!(value >= previous && value <= 1.);
            previous = value;
        }
        assert_eq!(sc_rgb_to_sdr([0.; 3], 80.), [0.; 3]);
    }
}

#[cfg(all(windows, any(feature = "mf-encode", feature = "d3d11va")))]
#[path = "hdr_windows.rs"]
mod platform;
#[cfg(all(windows, any(feature = "mf-encode", feature = "d3d11va")))]
pub use platform::{desktop_sdr_white, Presentation};

/// Generated 128×128 Main10/PQ/BT.2020 probes; no third-party video content.
pub const HEVC_PROBE: &[u8] = include_bytes!("hevc_hdr10_probe.h265");
pub const AV1_PROBE: &[u8] = include_bytes!("av1_hdr10_probe.obu");

/// Prove 10-bit output with generated Main10/PQ access units before advertising
/// a codec. This is separate from the ordinary 8-bit codec capability probe.
pub fn probe_decoder(
    codec: uu_proto::media::VideoCodec,
    kind: crate::DecoderKind,
) -> Result<(), crate::CodecError> {
    use uu_proto::media::VideoCodec;
    if kind == crate::DecoderKind::Software || !cfg!(windows) {
        return Err(crate::CodecError::Backend(
            "HDR requires a supported 10-bit hardware decoder".into(),
        ));
    }
    let data: &'static [u8] = match codec {
        VideoCodec::Hevc => HEVC_PROBE,
        VideoCodec::Av1 => AV1_PROBE,
        VideoCodec::H264 => {
            return Err(crate::CodecError::Backend(
                "HDR is unavailable with H.264".into(),
            ))
        }
    };
    let mut starts = vec![];
    match codec {
        VideoCodec::Hevc => {
            for i in 0..data.len().saturating_sub(4) {
                if data[i..].starts_with(&[0, 0, 0, 1, 70]) {
                    starts.push(i);
                }
            }
        }
        VideoCodec::Av1 => {
            for obu in crate::av1::obus(data).map_err(crate::CodecError::Backend)? {
                if (obu[0] >> 3) & 15 == 2 {
                    starts.push(obu.as_ptr() as usize - data.as_ptr() as usize);
                }
            }
        }
        VideoCodec::H264 => unreachable!(),
    }
    if starts.first() != Some(&0) {
        starts.insert(0, 0);
    }
    starts.push(data.len());
    let mut decoder = crate::decoder_for_codec_with_kind(codec, kind)?;
    decoder.prepare(128, 128)?;
    let units: Vec<_> = starts.windows(2).map(|s| &data[s[0]..s[1]]).collect();
    let mut produced = 0;
    for (i, data) in units.iter().cycle().take(units.len() * 2).enumerate() {
        let frames = decoder.decode_frames(&crate::EncodedFrame {
            data: bytes::Bytes::copy_from_slice(data),
            pts: crate::Timestamp(i as u64 * 16667),
            is_keyframe: match codec {
                VideoCodec::Hevc => crate::hevc::is_keyframe(data),
                _ => crate::av1::is_keyframe(data),
            },
        })?;
        for frame in frames {
            if frame.format != crate::PixelFormat::P010
                || !frame.is_consistent()
                || (frame.width, frame.height) != (128, 128)
            {
                return Err(crate::CodecError::Backend(
                    "HDR decoder did not produce valid P010 frames".into(),
                ));
            }
            produced += 1;
        }
    }
    if produced < 2 {
        return Err(crate::CodecError::Backend(
            "HDR decoder produced too few frames".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    #[test]
    fn missing_local_display_fields_have_defaults_but_invalid_wire_fields_are_rejected() {
        let missing = Metadata {
            primaries: [[0.; 2]; 3],
            white: [0.; 2],
            min_nits: f32::NAN,
            max_nits: 0.,
            max_full_frame_nits: f32::INFINITY,
            sdr_white_nits: 0.,
        };
        assert!(!missing.valid());
        let normalized = missing.with_display_defaults();
        assert!(normalized.valid());
        assert_eq!(normalized, Metadata::default());
        let actual = Metadata {
            sdr_white_nits: 240.,
            max_nits: 1370.,
            max_full_frame_nits: 1370.,
            ..Default::default()
        };
        assert_eq!(actual.with_display_defaults(), actual);
    }
}
