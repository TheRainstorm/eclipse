//! Hardware video decoding backends.
//!
//! One backend per platform, selected at runtime by [`create_hardware_decoder`]
//! and wrapped by [`crate::decoder::FallbackDecoder`] so the session always has
//! a picture even when the driver does not cooperate.
//!
//! * Windows: Media Foundation H.264 MFT driven by a D3D11 device manager
//!   (`d3d11va`), i.e. the same DXVA path the system video stack uses.
//! * Linux: VA-API stateless decode through `cros-codecs` (`vaapi`), which is
//!   the same implementation ChromeOS ships.
//!
//! Both live behind cargo features so that a machine without the vendor SDKs
//! still builds the rest of the controller.

#[cfg(all(windows, feature = "d3d11va"))]
pub mod d3d11va;
#[cfg(all(target_os = "linux", feature = "vaapi"))]
pub mod vaapi;

#[cfg(any(test, all(windows, feature = "d3d11va")))]
mod surface;
#[cfg(any(test, all(windows, feature = "d3d11va")))]
mod pixels;

mod thread;
pub use thread::{Backend, Worker};

use crate::codec::{CodecError, VideoDecoder};

/// Label for a hardware backend that has not been opened yet, used by
/// `uu probe` and the pre-session HUD line.
pub fn backend_label() -> &'static str {
    #[cfg(all(windows, feature = "d3d11va"))]
    {
        "d3d11va (Media Foundation)"
    }
    #[cfg(all(target_os = "linux", feature = "vaapi"))]
    {
        "vaapi (cros-codecs)"
    }
    #[cfg(not(any(
        all(windows, feature = "d3d11va"),
        all(target_os = "linux", feature = "vaapi")
    )))]
    {
        "none"
    }
}

/// Open the platform hardware decoder, or explain why there is none.
pub fn create_hardware_decoder() -> Result<Box<dyn VideoDecoder>, CodecError> {
    #[cfg(all(windows, feature = "d3d11va"))]
    {
        return Ok(Box::new(d3d11va::D3d11VaDecoder::new()?));
    }
    #[cfg(all(target_os = "linux", feature = "vaapi"))]
    {
        return Ok(Box::new(vaapi::VaapiDecoder::new()?));
    }
    #[cfg(not(any(
        all(windows, feature = "d3d11va"),
        all(target_os = "linux", feature = "vaapi")
    )))]
    {
        Err(CodecError::Backend(format!(
            "no hardware decoder backend compiled in (build with --features {} )",
            if cfg!(windows) { "d3d11va" } else { "vaapi" }
        )))
    }
}

#[cfg(all(test, target_os = "windows", feature = "h264"))]
mod windows_timing_tests {
    use crate::{H264Encoder, RawFrame, Timestamp, VideoEncoder};
    #[test]
    #[ignore = "requires a Windows D3D11 hardware decoder; run explicitly on test GPUs"]
    fn hardware_output_keeps_transport_pts_across_decoder_recreation() {
        // Different source rates expose synthetic fixed-rate timestamps;
        // a nonzero origin and a second decoder expose resets after loss.
        for (origin, step) in [(10_000_000, 33_333), (20_000_000, 16_667),
            (30_000_000, 8_333), (40_000_000, 13_333)] {
            let mut encoder = H264Encoder::new(1_000).unwrap();
            let mut decoder = super::create_hardware_decoder().expect("hardware decoder required");
            let mut produced = 0;
            let mut expected = std::collections::HashSet::new();
            for i in 0..20 {
                let pts = Timestamp(origin + i * step);
                expected.insert(pts);
                let mut data = vec![90; 64 * 64];
                data.extend(vec![128; 64 * 64 / 2]);
                let raw = RawFrame::new_i420(64, 64, data, pts);
                for encoded in encoder.encode(&raw).unwrap() {
                    if let Some(frame) = decoder.decode(&encoded).unwrap() {
                        assert!(
                            expected.contains(&frame.pts),
                            "output {:?} is not a submitted transport PTS",
                            frame.pts
                        );
                        produced += 1;
                    }
                }
            }
            assert!(
                produced >= 10,
                "hardware decoder must produce actual frames"
            );
        }
    }
}
