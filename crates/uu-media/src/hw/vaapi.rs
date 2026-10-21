//! Linux VA-API H.264 decoding, via `cros-codecs`.
//!
//! VA-API splits the work: the driver owns the GPU surfaces and the entropy
//! engine, while the *client* parses the bitstream and hands over parameter
//! buffers plus the reference picture lists. `cros-codecs` is the stateless
//! implementation ChromeOS ships, so this backend is mostly plumbing: a VA
//! display on a render node, a driver-allocated DMA surface pool, and the NAL-by-NAL
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

use bytes::Bytes;
use cros_codecs::decoder::stateless::h264::H264;
use cros_codecs::decoder::stateless::{
    DecodeError, StatelessDecoder, StatelessVideoDecoder,
};
use cros_codecs::decoder::BlockingMode;
use cros_codecs::decoder::{DecodedHandle, DecoderEvent, StreamInfo};
use cros_codecs::video_frame::frame_pool::FramePool;
use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;
use cros_codecs::video_frame::VideoFrame;
use cros_codecs::{DecodedFormat, Fourcc, FrameLayout, PlaneLayout};
use cros_libva::{Display, UsageHint, VAEntrypoint, VAProfile};

use crate::codec::{CodecError, VideoDecoder};
use crate::frame::{EncodedFrame, PixelFormat, RawFrame};
use crate::Timestamp;
use uu_proto::media::VideoCodec;

#[path = "vaapi_download.rs"]
mod download;

/// One driver-allocated DMA surface owned by the decoder's pool.
type Surface = cros_codecs::video_frame::frame_pool::PooledVideoFrame<GenericDmaVideoFrame>;

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
    decoder: StatelessDecoder<H264, cros_codecs::backend::vaapi::decoder::VaapiBackend<Surface>>,
    display: std::rc::Rc<Display>,
    pool: Option<FramePool<GenericDmaVideoFrame>>,
    name: String,
    /// Frames decoded before the caller picks them up; one `decode` call can
    /// release several pictures (reordering).
    pending: VecDeque<RawFrame>,
    pixels: super::pixels::Pool,
    dma_readback: bool,
    profile: DecodeProfile,
}

#[derive(Default)]
struct DecodeProfile {
    enabled: bool,
    calls: u64,
    frames: u64,
    gpu_frames: u64,
    total_us: u64,
    sync_us: u64,
    download_us: u64,
}

impl Decoder {
    fn open() -> Result<Self, String> {
        let (path, display) = open_display()?;
        let decoder =
            StatelessDecoder::<H264, _>::new_vaapi(display.clone(), BlockingMode::Blocking)
                .map_err(|e| format!("VA-API H.264 decoder: {e}"))?;
        Ok(Self {
            decoder,
            display,
            pool: None,
            name: format!("vaapi (cros-codecs hw, {})", path.display()),
            pending: VecDeque::new(),
            pixels: super::pixels::Pool::default(),
            dma_readback: std::env::var("UU_VAAPI_READBACK").as_deref() != Ok("legacy"),
            profile: DecodeProfile { enabled: std::env::var("UU_DECODE_PROFILE").is_ok_and(|v|v!="0"), ..Default::default() },
        })
    }
}

impl super::Backend for Decoder {
    fn backend_name(&self) -> &str {
        &self.name
    }

    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<RawFrame>, String> {
        let started = self.profile.enabled.then(std::time::Instant::now);
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
            // The upstream NAL reader excludes Annex-B trailing_zero_8bits
            // from its consumed count. They may remain after the final NAL
            // (including VA encoder alignment padding); there is no next NAL
            // to parse. Only accept zero padding after actual NAL progress.
            if input.len() < data.len() && input.iter().all(|byte| *byte == 0) {
                break;
            }
            let mut pool = self.pool.take();
            let result = self
                .decoder
                .decode(timestamp, input, &mut || pool.as_mut()?.alloc());
            self.pool = pool;
            let consumed = match result {
                Ok(consumed) => consumed,
                Err(DecodeError::CheckEvents) | Err(DecodeError::NotEnoughOutputBuffers(_)) => {
                    if self.pump_events()? == 0 {
                        // cros-codecs can enter FlushingForDRC with an empty
                        // ready queue. There is no event to drain: the next
                        // decode call advances reset_backend/renegotiation.
                        // Retry the same NAL, but never spin on a stuck pool.
                        stalls += 1;
                        if stalls > 2 {
                            return Err("VA-API decoder made no progress after event retries".into());
                        }
                    } else {
                        stalls = 0;
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
            stalls = 0;
            input = &input[consumed..];
            self.pump_events()?;
        }
        self.decoder.finish_access_unit().map_err(|e| format!("VA access-unit submission: {e}"))?;
        self.pump_events()?;
        if let Some(started) = started {
            self.profile.calls += 1;
            self.profile.total_us += started.elapsed().as_micros() as u64;
            if self.profile.calls >= 120 {
                let p = &self.profile;
                let n = p.calls.max(1) as f64 * 1000.0;
                let frames = p.frames.max(1) as f64 * 1000.0;
                eprintln!("vaapi decode profile calls={} frames={} readback={} total_ms={:.3} sync_ms={:.3} download_ms={:.3} parse_submit_other_ms={:.3}",p.calls,p.frames,if p.gpu_frames>0 {"gpu-import"} else if self.dma_readback {"dma-buf"} else {"vaimage"},p.total_us as f64/n,p.sync_us as f64/frames,p.download_us as f64/frames,p.total_us.saturating_sub(p.sync_us+p.download_us) as f64/n);
                self.profile = DecodeProfile { enabled: true, ..Default::default() };
            }
        }
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
                    let sync_started = self.profile.enabled.then(std::time::Instant::now);
                    handle.sync().map_err(|e| format!("surface sync: {e}"))?;
                    if let Some(started) = sync_started { self.profile.sync_us += started.elapsed().as_micros() as u64; }
                    let download_started = self.profile.enabled.then(std::time::Instant::now);
                    let frame = handle.video_frame();
                    let resolution = handle.display_resolution();
                    if frame.fourcc() != Fourcc::from(b"NV12") {
                        return Err("decode surface is not NV12".to_string());
                    }
                    if crate::dma_buf::rendering() {
                        let exported = handle.borrow().surface().export_prime();
                        if let Ok(mut descriptor) = exported {
                            if descriptor.fourcc == cros_libva::VA_FOURCC_NV12
                                && descriptor.objects.len()==1 && descriptor.layers.len()==1
                                && descriptor.layers[0].num_planes==2
                                && descriptor.layers[0].object_index[..2]==[0,0] {
                                let layer=&descriptor.layers[0];
                                let object=descriptor.objects.remove(0);
                                let gpu=crate::dma_buf::Frame {
                                    file:object.fd.into(),allocation_size:object.size,
                                    width:descriptor.width,height:descriptor.height,
                                    pitch:[layer.pitch[0],layer.pitch[1]],offset:[layer.offset[0],layer.offset[1]],
                                    modifier:object.drm_format_modifier,keep_alive:frame.clone(),
                                };
                                if gpu.valid([resolution.width,resolution.height]) {
                                    if self.profile.enabled {self.profile.frames+=1;self.profile.gpu_frames+=1;}
                                    self.pending.push_back(RawFrame {width:resolution.width,height:resolution.height,
                                        format:PixelFormat::Nv12,stride:resolution.width,data:Bytes::new(),
                                        pts:Timestamp(handle.timestamp()),gpu:true,dma_buf:Some(std::sync::Arc::new(gpu)),d3d11:None});
                                    continue;
                                }
                            }
                        }
                        // One unsupported surface disables this process's import
                        // path; the current frame still uses the established download.
                        crate::dma_buf::set_rendering(false);
                    }
                    // Map through VA, which understands the driver's tiling.
                    // The upstream generic DMA mapper assumes every padding
                    // row is a complete Intel Y tile and panics on iHD exports.
                    let picture = handle.borrow();
                    let surface = picture.surface();
                    let direct = if self.dma_readback {
                        match surface.export_prime().map_err(|e| e.to_string())
                            .and_then(|descriptor| download::download(&descriptor,resolution.width,resolution.height,&self.pixels)) {
                            Ok(pixels) => Some(pixels),
                            Err(reason) => {
                                // An unknown modifier or DMA mapping failure must
                                // keep hardware decode, using the driver layout.
                                tracing::debug!(%reason,"VAAPI DMA-BUF download unavailable; keeping driver mapping");
                                self.dma_readback = false;
                                None
                            }
                        }
                    } else { None };
                    let pixels = if let Some(pixels) = direct { pixels } else {
                    let image = cros_libva::Image::derive_from(
                        surface,
                        (resolution.width, resolution.height),
                    )
                    .map_err(|e| format!("VA image mapping: {e}"))?;
                    let layout = image.image();
                    if layout.format.fourcc != cros_libva::VA_FOURCC_NV12 || layout.num_planes != 2
                    {
                        return Err("mapped VA image is not two-plane NV12".into());
                    }
                    let planes = [
                        image
                            .as_ref()
                            .get(layout.offsets[0] as usize..)
                            .ok_or("VA luma offset outside mapping")?,
                        image
                            .as_ref()
                            .get(layout.offsets[1] as usize..)
                            .ok_or("VA chroma offset outside mapping")?,
                    ];
                    let pitches = [layout.pitches[0] as usize, layout.pitches[1] as usize];
                    let pixels = pack_nv12(&planes, &pitches, resolution.width, resolution.height)?;
                    Bytes::from(pixels)
                    };
                    if let Some(started) = download_started {
                        self.profile.download_us += started.elapsed().as_micros() as u64;
                        self.profile.frames += 1;
                    }
                    self.pending.push_back(RawFrame {
                        width: resolution.width,
                        height: resolution.height,
                        format: PixelFormat::Nv12,
                        stride: resolution.width,
                        data: pixels,
                        pts: Timestamp(handle.timestamp()),
                        gpu: false,
                        dma_buf: None,
                        d3d11: None,
                    });
                }
            }
        }
        Ok(handled)
    }

    /// (Re)build the surface pool with the size the decoder advertises.
    fn resize(&mut self, info: &StreamInfo) -> Result<(), String> {
        if info.format != DecodedFormat::NV12 {
            return Err(format!(
                "driver chose {:?}, only NV12 is supported",
                info.format
            ));
        }
        if !(1..=64).contains(&info.min_num_frames) {
            return Err(format!("driver wants {} surfaces", info.min_num_frames));
        }
        if info.display_resolution.width == 0 || info.display_resolution.height == 0 {
            return Err("stream has no display resolution".to_string());
        }
        // Allocate before entering the pool callback: that callback cannot
        // report failures, so every frame it hands out must already exist.
        let mut pool_info=info.clone();
        if crate::dma_buf::rendering() {pool_info.min_num_frames+=6;}
        let mut prepared = VecDeque::new();
        for _ in 0..pool_info.min_num_frames {
            // Let the VA driver choose its native allocation. ChromeOS GBM's
            // HW_VIDEO_DECODER flag is not supported by desktop Mesa drivers.
            let surface = self
                .display
                .create_surfaces(
                    cros_libva::VA_RT_FORMAT_YUV420,
                    Some(cros_libva::VA_FOURCC_NV12),
                    info.coded_resolution.width,
                    info.coded_resolution.height,
                    Some(UsageHint::USAGE_HINT_DECODER | UsageHint::USAGE_HINT_EXPORT),
                    vec![()],
                )
                .map_err(|e| format!("VA surface allocation: {e}"))?
                .pop()
                .ok_or("VA driver allocated no surface")?;
            let descriptor = surface
                .export_prime()
                .map_err(|e| format!("exporting VA surface: {e}"))?;
            // GenericDmaVideoFrame currently supports one contiguous object.
            // Reject unfamiliar layouts instead of interpreting tiled memory
            // with the wrong pitch or silently producing damaged pictures.
            if descriptor.objects.len() != 1
                || descriptor.layers.len() != 1
                || descriptor.layers[0].num_planes != 2
                || descriptor.layers[0].object_index[..2] != [0, 0]
            {
                return Err("VA driver exported an unsupported NV12 layout".into());
            }
            let layer = &descriptor.layers[0];
            let layout = FrameLayout {
                format: (
                    Fourcc::from(b"NV12"),
                    descriptor.objects[0].drm_format_modifier,
                ),
                size: info.coded_resolution,
                planes: (0..2)
                    .map(|i| PlaneLayout {
                        buffer_index: 0,
                        offset: layer.offset[i] as usize,
                        stride: layer.pitch[i] as usize,
                    })
                    .collect(),
            };
            let handles = descriptor
                .objects
                .into_iter()
                .map(|o| std::fs::File::from(o.fd))
                .collect();
            let frame = GenericDmaVideoFrame::new(handles, layout)?;
            prepared.push_back(frame);
        }
        let mut pool = FramePool::new(move |_| {
            prepared
                .pop_front()
                .expect("one prepared surface per advertised pool slot")
        });
        pool.resize(&pool_info);
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
    if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 {
        return Err("NV12 dimensions must be positive and even".into());
    }
    for (i, rows) in [h, h / 2].into_iter().enumerate() {
        let needed = pitches[i]
            .checked_mul(rows - 1)
            .and_then(|n| n.checked_add(w));
        if pitches[i] < w || needed.map_or(true, |n| planes[i].len() < n) {
            return Err(format!(
                "NV12 plane {i} is truncated or has an invalid pitch"
            ));
        }
    }
    let length = w
        .checked_mul(h)
        .and_then(|n| n.checked_mul(3))
        .map(|n| n / 2)
        .ok_or("NV12 dimensions overflow")?;
    let mut out = vec![0u8; length];
    for row in 0..h {
        out[row * w..(row + 1) * w]
            .copy_from_slice(&planes[0][row * pitches[0]..row * pitches[0] + w]);
    }
    let chroma = w * h;
    for row in 0..h / 2 {
        out[chroma + row * w..chroma + (row + 1) * w]
            .copy_from_slice(&planes[1][row * pitches[1]..row * pitches[1] + w]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immediate_output_keeps_h264_reference_surfaces_alive() {
        use cros_codecs::codec::h264::{dpb::Dpb, picture::PictureData};
        use std::sync::Arc;
        let mut dpb = Dpb::default();
        dpb.set_limits(2, 0);
        let handle = Arc::new(42);
        let mut picture = PictureData::new_non_existing(0, 0);
        picture.nonexisting = false;
        dpb.store_picture(picture.into_rc(), Some(handle.clone())).unwrap();
        let shown = dpb.output_without_reordering();
        assert_eq!(shown, vec![Some(handle.clone())]);
        assert!(dpb.output_without_reordering().is_empty());
        assert_eq!(dpb.entries().len(), 1);
        assert!(dpb.entries()[0].reference.is_some());
        assert!(dpb.entries()[0].decoded_frame.is_none());
        drop(shown);
        assert_eq!(Arc::strong_count(&handle), 2);
    }

    #[test]
    fn reordered_or_interlaced_h264_never_uses_immediate_output() {
        use cros_codecs::codec::h264::{dpb::Dpb, picture::PictureData};
        for (reorder, interlaced) in [(2, false), (0, true)] {
            let mut dpb = Dpb::default();
            dpb.set_limits(2, reorder);
            dpb.set_interlaced(interlaced);
            // IDR/reset clears pictures, not the SPS reorder constraints.
            dpb.clear();
            let mut picture = PictureData::new_non_existing(0, 0);
            picture.nonexisting = false;
            dpb.store_picture(picture.into_rc(), Some(42)).unwrap();
            assert!(dpb.output_without_reordering().is_empty());
            assert_eq!(dpb.entries()[0].decoded_frame, Some(42));
        }
    }

    #[test]
    fn nv12_planes_are_repacked_row_by_row() {
        // 2x2 luma (pitch 4) and one interleaved chroma row (pitch 4).
        let y = [1u8, 2, 9, 9, 3, 4, 9, 9];
        let uv = [10u8, 20, 30, 40];
        let packed = pack_nv12(&[&y, &uv], &[4, 4], 2, 2).unwrap();
        assert_eq!(packed, vec![1, 2, 3, 4, 10, 20]);
    }

    #[test]
    fn truncated_planes_and_invalid_layouts_are_rejected() {
        assert!(pack_nv12(&[&[7u8], &[]], &[8, 8], 2, 2).is_err());
        assert!(pack_nv12(&[&[0; 8], &[0; 4]], &[1, 4], 2, 2).is_err());
        assert!(pack_nv12(&[&[0; 8], &[0; 4]], &[4, 4], 3, 2).is_err());
    }
}
