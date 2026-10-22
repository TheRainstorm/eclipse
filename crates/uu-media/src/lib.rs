//! Media pipeline.
//!
//! The pipeline is split into a *producer* (host side: capture -> encode) and
//! a *consumer* (controller side: decode -> present), mirroring how the
//! official client separates `GameViewerServer` (host) from the controller UI.
//!
//! The official media plane is standard WebRTC with H.264/HEVC video and
//! Opus audio (see `docs/protocol-notes.md`), so the codec layer here is
//! deliberately codec-agnostic and pluggable.

pub mod av1;
pub mod audio;
pub mod audio_capture;
pub mod audio_red;
pub mod bitstream;
pub mod codec;
#[cfg(feature = "h264")]
pub mod encoder;
pub mod color;
pub mod convert;
pub mod decoder;
pub mod dma_buf;
pub mod d3d11_frame;
pub mod frame;
#[cfg(feature = "h264")]
pub mod h264;
pub mod hevc;
pub mod hw;

pub use codec::{CodecError, NullEncoder, VideoDecoder, VideoEncoder};
pub use decoder::{DecoderKind, FallbackDecoder, NamedDecoder};
pub use frame::{EncodedFrame, PixelFormat, RawFrame};

#[cfg(feature = "h264")]
pub use h264::{H264Decoder, H264Encoder};

/// Build the default software encoder for this build.
pub fn default_encoder(bitrate_kbps: u32) -> Result<Box<dyn VideoEncoder>, CodecError> {
    default_encoder_with_frame_rate(bitrate_kbps, 30)
}

pub fn default_encoder_with_frame_rate(
    bitrate_kbps: u32,
    fps: u32,
) -> Result<Box<dyn VideoEncoder>, CodecError> {
    #[cfg(feature = "h264")]
    let encoder: Box<dyn VideoEncoder> = Box::new(H264Encoder::with_frame_rate(bitrate_kbps, fps)?);
    #[cfg(not(feature = "h264"))]
    let encoder: Box<dyn VideoEncoder> = {
        let _ = (bitrate_kbps, fps);
        Box::new(NullEncoder::new(uu_proto::media::VideoCodec::H264))
    };
    Ok(encoder)
}

/// Build the in-process software (OpenH264) decoder.
///
/// Benchmarks: ~2.9 ms/frame for 1080p at low bitrates, but ~20 ms/frame on
/// the 55 Mbit/s preset the streamer actually runs, which is why hardware
/// decode matters. Kept as the guaranteed fallback for every platform.
pub fn software_decoder() -> Result<Box<dyn VideoDecoder>, CodecError> {
    #[cfg(feature = "h264")]
    let decoder: Box<dyn VideoDecoder> = Box::new(H264Decoder::new()?);
    #[cfg(not(feature = "h264"))]
    let decoder: Box<dyn VideoDecoder> = {
        return Err(CodecError::Unsupported(uu_proto::media::VideoCodec::H264));
    };
    Ok(decoder)
}

/// Build the decoder the session should use.
///
/// Hardware is preferred (`UU_DECODER=hw` asks for it explicitly, `sw` forces
/// OpenH264) and is always paired with a software decoder it can demote to,
/// so neither a missing driver nor a broken one can leave a session without a
/// picture. See [`decoder`] for the selection rules.
pub fn default_decoder() -> Result<Box<dyn VideoDecoder>, CodecError> {
    let kind = DecoderKind::from_env();
    let mut hardware = None;
    let mut why = String::new();
    if kind != DecoderKind::Software {
        match hw::create_hardware_decoder() {
            Ok(d) => hardware = Some(d),
            Err(e) => why = e.to_string(),
        }
    }

    match hardware {
        // Hardware paths keep a software decoder in reserve: they fail in
        // ways the transport cannot see (a driver that accepts every call and
        // never emits a frame), so the wrapper demotes at runtime.
        Some(primary) => match software_decoder() {
            Ok(backup) => {
                let decoder = FallbackDecoder::new(primary, backup);
                let name = decoder.backend_name().to_string();
                Ok(record_backend(&name, Box::new(decoder)))
            }
            Err(_) => {
                let name = primary.backend_name().to_string();
                Ok(record_backend(&name, primary))
            }
        },
        None => {
            let decoder = software_decoder()?;
            let name = if kind == DecoderKind::Software || why.is_empty() {
                decoder.backend_name().to_string()
            } else {
                // Keep the reason attached to the decoder itself: the HUD
                // reads the name off the instance, not off the global.
                format!("{} [hardware unavailable: {why}]", decoder.backend_name())
            };
            let decoder = name_decoder(decoder, name);
            let name = decoder.backend_name().to_string();
            Ok(record_backend(&name, decoder))
        }
    }
}

/// Select a decoder for the negotiated codec. OpenH264 cannot decode HEVC;
/// never silently pass HEVC access units to the H.264 software fallback.
pub fn decoder_for_codec(codec: uu_proto::media::VideoCodec) -> Result<Box<dyn VideoDecoder>, CodecError> {
    use uu_proto::media::VideoCodec;
    if codec == VideoCodec::H264 { return default_decoder(); }
    #[cfg(all(windows, feature = "d3d11va"))]
    if codec == VideoCodec::Hevc && DecoderKind::from_env() != DecoderKind::Software {
        return Ok(Box::new(hw::d3d11va::D3d11VaDecoder::with_codec(codec)?));
    }
    #[cfg(all(target_os="linux",feature="vaapi"))]
    if matches!(codec,VideoCodec::Hevc|VideoCodec::Av1) && DecoderKind::from_env() != DecoderKind::Software {
        return Ok(Box::new(hw::vaapi::VaapiDecoder::with_codec(codec)?));
    }
    #[cfg(all(feature="intel-vpl",any(windows,target_os="linux")))]
    if codec==VideoCodec::Av1 && DecoderKind::from_env()!=DecoderKind::Software {
        return Ok(Box::new(hw::vpl::VplAv1Decoder::new()?));
    }
    Err(CodecError::Unsupported(codec))
}

/// Backend chosen by the first [`default_decoder`] call in this process.
///
/// The HUD and `uu probe` print this before any session exists, so it also
/// answers with the *intended* backend when nothing has been built yet.
pub fn decoder_backend_name() -> String {
    match BACKEND.get() {
        Some(name) => name.clone(),
        None => match DecoderKind::from_env() {
            DecoderKind::Software => "openh264 (software, in-process)".to_string(),
            _ => format!("{} (hardware, preferred)", hw::backend_label()),
        },
    }
}

/// Attach `name` to a decoder, but only when it says more than the decoder
/// already does — the common case then keeps its own name and one less box.
fn name_decoder(decoder: Box<dyn VideoDecoder>, name: String) -> Box<dyn VideoDecoder> {
    if decoder.backend_name() == name {
        decoder
    } else {
        Box::new(NamedDecoder::new(decoder, name))
    }
}

/// Remember the chosen backend for [`decoder_backend_name`] and pass the
/// decoder through.
fn record_backend(name: &str, decoder: Box<dyn VideoDecoder>) -> Box<dyn VideoDecoder> {
    let _ = BACKEND.set(name.to_string());
    decoder
}

/// Backend selected by the first [`default_decoder`] call in this process.
static BACKEND: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Monotonic microsecond timestamp used across capture/encode/decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(pub u64);

impl Timestamp {
    pub fn as_micros(self) -> u64 {
        self.0
    }
}

/// Mean squared error of the luma plane between two same-sized I420 frames.
///
/// Used by the self-loop quality gate: an encode/decode round trip must stay
/// close to the source, so a regression (e.g. feeding the encoder the wrong
/// colour plane) shows up as a test failure rather than a black picture.
pub fn luma_mse(a: &RawFrame, b: &RawFrame) -> Option<f64> {
    if a.width != b.width || a.height != b.height {
        return None;
    }
    let n = (a.width * a.height) as usize;
    let (pa, pb) = (a.data.as_ref(), b.data.as_ref());
    if pa.len() < n || pb.len() < n {
        return None;
    }
    let sum: f64 = pa[..n]
        .iter()
        .zip(&pb[..n])
        .map(|(x, y)| {
            let d = *x as f64 - *y as f64;
            d * d
        })
        .sum();
    Some(sum / n as f64)
}

/// Peak signal-to-noise ratio in dB, given a mean squared error.
pub fn psnr_from_mse(mse: f64) -> f64 {
    if mse <= 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0f64 * 255.0 / mse).log10()
}

#[cfg(test)]
mod decoder_tests {
    /// Whatever this machine offers, a session must always get a decoder: the
    /// hardware path is allowed to fail, but it must fail into software rather
    /// than into an error. Prints the selection so `--nocapture` shows which
    /// backend a machine actually picked.
    #[test]
    fn a_decoder_is_always_available() {
        // The environment override is process-wide, so keep this test to the
        // default preference.
        let decoder = crate::default_decoder().expect("a decoder must always be constructible");
        let name = decoder.backend_name().to_string();
        println!("selected decoder: {name}");
        assert!(!name.is_empty());
    }
}
