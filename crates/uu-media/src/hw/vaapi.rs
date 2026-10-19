//! Linux VA-API H.264 decoding, via `cros-codecs`.
//!
//! VA-API splits the work: the driver owns the GPU surfaces and the entropy
//! engine, while the *client* parses the bitstream and hands over parameter
//! buffers plus the reference picture lists. `cros-codecs` is the stateless
//! implementation ChromeOS ships, so this backend is mostly plumbing: a VA
//! display on a render node, a GBM-backed surface pool, and the NAL-by-NAL
//! feed loop its H.264 decoder expects.
//!
//! Two properties of that API shape this file. First, `decode` consumes *one*
//! NAL and returns how many bytes it took, so an access unit is fed in a loop.
//! Second, decoding can be interrupted at any point by `DecodeError::
//! CheckEvents` — a format change or an exhausted surface pool — at which
//! point the client must drain events and retry the same bytes.
//!
//! Surfaces stay on the GPU; frames are mapped and repacked into tightly
//! packed NV12 so the rest of the media plane (which knows the I420/NV12
//! layouts and nothing about GBM) stays unchanged.
//!
//! Build with `--features vaapi`, which needs the `libva`, `libgbm` and
//! `libdrm` development packages.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use cros_codecs::decoder::stateless::h264::H264;
use cros_codecs::decoder::stateless::{
    DecodeError, DynStatelessVideoDecoder, StatelessDecoder, StatelessVideoDecoder,
};
use cros_codecs::decoder::{DecodedHandle, DecoderEvent, StreamInfo};
use cros_codecs::decoder::BlockingMode;
use cros_codecs::video_frame::frame_pool::FramePool;
use cros_codecs::video_frame::gbm_video_frame::{GbmDevice, GbmUsage, GbmVideoFrame};
use cros_codecs::video_frame::VideoFrame;
use cros_codecs::{DecodedFormat, Fourcc};
use cros_libva::{Display, VAEntrypoint, VAProfile};

use crate::codec::{CodecError, VideoDecoder};
use crate::frame::{EncodedFrame, PixelFormat, RawFrame};
use crate::Timestamp;
use uu_proto::media::VideoCodec;

/// One GBM surface owned by the decoder's pool.
type Surface = cros_codecs::video_frame::frame_pool::PooledVideoFrame<GbmVideoFrame>;

/// Public handle: a `Worker` thread owning the VA display and its surfaces.
pub struct VaapiDecoder {
    worker: super::Worker,
}

impl VaapiDecoder {
    pub fn new() -> Result<Self, CodecError> {
        Ok(Self {
            worker: super::Worker::spawn(Decoder::open)?,
        })
    }
}

impl VideoDecoder for VaapiDecoder {
    fn codec(&self) -> VideoCodec {
        VideoCodec::H264
    }

    fn backend_name(&self) -> &str {
        self.worker.backend_name()
    }

    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<RawFrame>, CodecError> {
        self.worker.decode(frame)
    }
}

struct Decoder {
    decoder: DynStatelessVideoDecoder<Surface>,
    device: Arc<GbmDevice>,
    pool: Option<FramePool<GbmVideoFrame>>,
    name: String,
    /// Frames decoded before the caller picks them up; one `decode` call can
    /// release several pictures (reordering).
    pending: VecDeque<RawFrame>,
}

impl Decoder {
    fn open() -> Result<Self, String> {
        let (path, display) = open_display()?;
        let device = GbmDevice::open(&path).map_err(|e| format!("GBM device: {e}"))?;
        let decoder = StatelessDecoder::<H264, _>::new_vaapi(display, BlockingMode::Blocking)
            .map_err(|e| format!("VA-API H.264 decoder: {e}"))?
            .into_trait_object();
        Ok(Self {
            decoder,
            device,
            pool: None,
            name: format!("vaapi (cros-codecs hw, {})", path.display()),
            pending: VecDeque::new(),
        })
    }
}

impl super::Backend for Decoder {
    fn backend_name(&self) -> &str {
        &self.name
    }

    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<RawFrame>, String> {
        // Feed every new access unit before returning queued output, otherwise
        // a burst of reordered pictures makes us discard new compressed data.
        // The IDR access units carry SPS/PPS, and the decoder needs them
        // before any slice: feeding mid-GOP data first is harmless (it is
        // skipped) but produces nothing until a keyframe arrives.
        let patched = crate::bitstream::patch_sps_allow_gaps(&frame.data);
        let data = patched.as_deref().unwrap_or(&frame.data);
        let mut input = data;
        let timestamp = frame.pts.as_micros();
        // One call consumes one NAL. `CheckEvents` and `NotEnoughOutputBuffers`
        // are not failures: they ask the client to drain events and retry the
        // very same bytes, which is why this loop retries instead of advancing.
        let mut stalls = 0u32;
        while !input.is_empty() {
            let mut pool = self.pool.take();
            let result = self
                .decoder
                .decode(timestamp, input, &mut || pool.as_mut()?.alloc());
            self.pool = pool;
            let consumed = match result {
                Ok(consumed) => consumed,
                Err(DecodeError::CheckEvents) | Err(DecodeError::NotEnoughOutputBuffers(_)) => {
                    if self.pump_events()? == 0 {
                        return Err("VA-API decoder is waiting for events but has none".to_string());
                    }
                    continue;
                }
                Err(e) => return Err(format!("vaapi decode: {e}")),
            };
            if consumed == 0 || consumed > input.len() {
                // No progress even though the decoder accepted the call: ask
                // for events once, then stop instead of spinning.
                if self.pump_events()? == 0 {
                    stalls += 1;
                    if stalls > 2 {
                        return Err("VA-API decoder stopped consuming input".to_string());
                    }
                }
                continue;
            }
            input = &input[consumed..];
            self.pump_events()?;
        }
        self.pump_events()?;
        Ok(self.pending.pop_front())
    }
}

impl Decoder {
    /// Handle every pending decoder event, returning how many were processed.
    fn pump_events(&mut self) -> Result<usize, String> {
        let mut handled = 0usize;
        while let Some(event) = self.decoder.next_event() {
            handled += 1;
            match event {
                DecoderEvent::FormatChanged => {
                    let info = self
                        .decoder
                        .stream_info()
                        .ok_or("format event without stream information")?
                        .clone();
                    self.resize(&info)?;
                }
                DecoderEvent::FrameReady(handle) => {
                    handle.sync().map_err(|e| format!("surface sync: {e}"))?;
                    let frame = handle.video_frame();
                    let resolution = handle.display_resolution();
                    if frame.fourcc() != Fourcc::from(b"NV12") {
                        return Err("decode surface is not NV12".to_string());
                    }
                    let pitches = frame.get_plane_pitch();
                    let mapping = frame.map().map_err(|e| format!("surface map: {e}"))?;
                    let planes = mapping.get();
                    let pixels = pack_nv12(&planes, &pitches, resolution.width, resolution.height)?;
                    self.pending.push_back(RawFrame {
                        width: resolution.width,
                        height: resolution.height,
                        format: PixelFormat::Nv12,
                        stride: resolution.width,
                        data: Bytes::from(pixels),
                        pts: Timestamp(handle.timestamp()),
                        gpu: false,
                    });
                }
            }
        }
        Ok(handled)
    }

    /// (Re)build the surface pool with the size the decoder advertises.
    fn resize(&mut self, info: &StreamInfo) -> Result<(), String> {
        if info.format != DecodedFormat::NV12 {
            return Err(format!("driver chose {:?}, only NV12 is supported", info.format));
        }
        if !(1..=64).contains(&info.min_num_frames) {
            return Err(format!("driver wants {} surfaces", info.min_num_frames));
        }
        if info.display_resolution.width == 0 || info.display_resolution.height == 0 {
            return Err("stream has no display resolution".to_string());
        }
        // Allocate before entering the pool callback: that callback cannot
        // report failures, so every frame it hands out must already exist.
        let mut prepared = VecDeque::new();
        for _ in 0..info.min_num_frames {
            let frame = self
                .device
                .clone()
                .new_frame(
                    Fourcc::from(b"NV12"),
                    info.display_resolution,
                    info.coded_resolution,
                    GbmUsage::Decode,
                )
                .map_err(|e| format!("surface allocation: {e}"))?;
            prepared.push_back(frame);
        }
        let mut pool = FramePool::new(move |_| {
            prepared
                .pop_front()
                .expect("one prepared surface per advertised pool slot")
        });
        pool.resize(info);
        self.pool = Some(pool);
        Ok(())
    }
}

/// Open the first DRM render node whose driver can decode H.264.
fn open_display() -> Result<(PathBuf, std::rc::Rc<Display>), String> {
    let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev/dri")
        .map_err(|e| format!("reading /dev/dri: {e}"))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("renderD"))
        })
        .collect();
    nodes.sort();
    if nodes.is_empty() {
        return Err("no /dev/dri/renderD* device (no GPU for VA-API)".to_string());
    }
    let mut failures = Vec::new();
    for path in nodes {
        match open_node(&path) {
            Ok(display) => return Ok((path, display)),
            Err(e) => failures.push(format!("{}: {e}", path.display())),
        }
    }
    Err(format!(
        "no render node decodes H.264 with VA-API: {}",
        failures.join("; ")
    ))
}

fn open_node(path: &Path) -> Result<std::rc::Rc<Display>, String> {
    let display = Display::open_drm_display(path).map_err(|e| format!("VA display: {e}"))?;
    let profiles = display
        .query_config_profiles()
        .map_err(|e| format!("querying VA profiles: {e}"))?;
    for profile in [VAProfile::VAProfileH264High, VAProfile::VAProfileH264Main] {
        if !profiles.contains(&profile) {
            continue;
        }
        let entrypoints = display
            .query_config_entrypoints(profile)
            .map_err(|e| format!("querying VA entry points: {e}"))?;
        if entrypoints.contains(&VAEntrypoint::VAEntrypointVLD) {
            return Ok(display);
        }
    }
    Err("driver offers no H.264 VLD entry point".to_string())
}

/// Repack the driver's (possibly padded) NV12 planes into a tightly packed
/// frame, which is the layout `crate::convert` expects.
fn pack_nv12(
    planes: &[&[u8]],
    pitches: &[usize],
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let (w, h) = (width as usize, height as usize);
    if planes.len() < 2 || pitches.len() < 2 {
        return Err(format!("NV12 surface has {} planes", planes.len()));
    }
    let mut out = vec![0u8; w * h * 3 / 2];
    for row in 0..h {
        copy_row(&mut out[row * w..row * w + w], planes[0], row * pitches[0]);
    }
    let chroma = w * h;
    for row in 0..h / 2 {
        copy_row(
            &mut out[chroma + row * w..chroma + row * w + w],
            planes[1],
            row * pitches[1],
        );
    }
    Ok(out)
}

/// Copy one row, tolerating a mapping that is shorter than the pitch implies:
/// the frames are handed to a decoder that has already accepted the stream, so
/// a partially mapped tail row costs a little noise instead of a hard failure.
fn copy_row(destination: &mut [u8], plane: &[u8], offset: usize) {
    let Some(tail) = plane.get(offset..) else {
        return;
    };
    let available = tail.len().min(destination.len());
    destination[..available].copy_from_slice(&tail[..available]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv12_planes_are_repacked_row_by_row() {
        // 2x2 luma (pitch 4) and one interleaved chroma row (pitch 4).
        let y = [1u8, 2, 9, 9, 3, 4, 9, 9];
        let uv = [10u8, 20, 30, 40];
        let packed = pack_nv12(&[&y, &uv], &[4, 4], 2, 2).unwrap();
        assert_eq!(packed, vec![1, 2, 3, 4, 10, 20]);
    }

    #[test]
    fn short_mappings_copy_what_exists_and_zero_the_rest() {
        let packed = pack_nv12(&[&[7u8], &[]], &[8, 8], 2, 2).unwrap();
        assert_eq!(packed.len(), 6);
        assert_eq!(packed[0], 7);
        assert_eq!(packed[1], 0);
    }
}
