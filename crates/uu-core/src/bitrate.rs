//! LAN Quality targets, not a bandwidth estimator or a universal quality floor.
//! PyroWave follows upstream d7a03df's SDR/420 good-quality regression. Inter
//! codecs use a Moonlight-style spatial table and gentler scaling above 60 FPS;
//! HEVC/AV1 factors are Eclipse policy estimates, not measured quality equivalence.
use uu_proto::media::VideoCodec;

/// Video payload kbps; RTP/SRTP, audio and repair traffic need extra capacity.
pub fn recommended_kbps(width: u32, height: u32, fps: u32, codec: VideoCodec) -> u32 {
    let pixels = f64::from(width) * f64::from(height);
    let fps = f64::from(fps.max(1));
    let kbps = if codec == VideoCodec::Pyrowave {
        125_000. * (pixels / (1280. * 720.)).powf(0.38) * fps / 60.
    } else {
        let table = [
            (640. * 360., 2_000.),
            (854. * 480., 4_000.),
            (1280. * 720., 10_000.),
            (1920. * 1080., 20_000.),
            (2560. * 1440., 40_000.),
            (3840. * 2160., 80_000.),
        ];
        let spatial = if pixels <= table[0].0 {
            table[0].1
        } else if let Some(pair) = table.windows(2).find(|p| pixels <= p[1].0) {
            pair[0].1 + (pixels - pair[0].0) / (pair[1].0 - pair[0].0) * (pair[1].1 - pair[0].1)
        } else {
            80_000. * pixels / (3840. * 2160.)
        };
        let temporal = if fps <= 60. { fps / 60. } else { (fps / 60.).sqrt() };
        let efficiency = match codec {
            VideoCodec::Hevc => 0.75,
            VideoCodec::Av1 => 0.60,
            _ => 1.,
        };
        spatial * temporal * efficiency
    };
    // Round upward to 0.1 Mbps. Keep the existing protocol/encoder bounds.
    ((kbps / 100.).ceil() * 100.).clamp(1_000., f64::from(crate::native_quality::MAX_BITRATE_KBPS)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_anchors_distinguish_codec_efficiency() {
        for (codec, expected) in [(VideoCodec::H264, 20_000), (VideoCodec::Hevc, 15_000), (VideoCodec::Av1, 12_000)] {
            assert_eq!(recommended_kbps(1920, 1080, 60, codec), expected);
            assert_eq!(recommended_kbps(3840, 2160, 60, codec), expected * 4);
            assert_eq!(recommended_kbps(1920, 1080, 30, codec), expected / 2);
        }
    }

    #[test]
    fn intra_and_inter_codecs_have_different_frame_rate_scaling() {
        assert_eq!(recommended_kbps(1920, 1080, 120, VideoCodec::H264), 28_300);
        assert_eq!(recommended_kbps(1920, 1080, 120, VideoCodec::Pyrowave), 340_300);
        assert_eq!(recommended_kbps(1280, 720, 30, VideoCodec::Pyrowave), 62_500);
        assert_eq!(recommended_kbps(2560, 1440, 60, VideoCodec::Pyrowave), 211_700);
        assert_eq!(recommended_kbps(3840, 2160, 60, VideoCodec::Pyrowave), 288_100);
    }

    #[test]
    fn unusual_geometry_interpolates_and_extremes_stay_in_protocol_bounds() {
        let wide = recommended_kbps(3440, 1440, 60, VideoCodec::H264);
        assert!(wide > 40_000 && wide < 80_000);
        assert!(recommended_kbps(7680, 4320, 60, VideoCodec::H264) > 80_000);
        assert_eq!(recommended_kbps(0, 0, 0, VideoCodec::Pyrowave), 1_000);
        assert_eq!(recommended_kbps(u32::MAX, u32::MAX, u32::MAX, VideoCodec::Pyrowave), 1_000_000);
    }
}
