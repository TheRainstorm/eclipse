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
use cros_codecs::decoder::stateless::h265::H265;
use cros_codecs::decoder::stateless::av1::Av1;
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
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::sync::{atomic::{AtomicBool, Ordering}, Arc};
use uu_proto::media::VideoCodec;

#[path = "vaapi_download.rs"]
pub(crate) mod download;

/// One driver-allocated DMA surface owned by the decoder's pool.
type Surface = cros_codecs::video_frame::frame_pool::PooledVideoFrame<GenericDmaVideoFrame>;

/// Public handle: a `Worker` thread owning the VA display and its surfaces.
pub struct VaapiDecoder {
    worker: super::Worker,
    codec: VideoCodec,
}

impl VaapiDecoder {
    pub fn new() -> Result<Self, CodecError> {
        Self::with_codec(VideoCodec::H264)
    }
    pub fn with_codec(codec:VideoCodec) -> Result<Self, CodecError> {
        Ok(Self {
            worker: super::Worker::spawn(move || Decoder::open(codec))?,
            codec,
        })
    }
}

impl VideoDecoder for VaapiDecoder {
    fn codec(&self) -> VideoCodec {
        self.codec
    }

    fn backend_name(&self) -> &str {
        self.worker.backend_name()
    }

    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<RawFrame>, CodecError> {
        self.worker.decode(frame)
    }
    fn decode_frames(&mut self, frame: &EncodedFrame) -> Result<Vec<RawFrame>, CodecError> {
        self.worker.decode_frames(frame)
    }
}

type VaBackend=cros_codecs::backend::vaapi::decoder::VaapiBackend<Surface>;
type Handle=std::rc::Rc<std::cell::RefCell<cros_codecs::backend::vaapi::decoder::VaapiDecodedHandle<Surface>>>;
enum BitstreamDecoder { H264(StatelessDecoder<H264,VaBackend>), Hevc(StatelessDecoder<H265,VaBackend>), Av1(StatelessDecoder<Av1,VaBackend>) }
impl BitstreamDecoder {
    fn decode(&mut self,timestamp:u64,input:&[u8],alloc:&mut dyn FnMut()->Option<Surface>)->Result<usize,DecodeError> {
        match self {Self::H264(d)=>d.decode(timestamp,input,alloc),Self::Hevc(d)=>d.decode(timestamp,input,alloc),Self::Av1(d)=>d.decode(timestamp,input,alloc)}
    }
    fn next_event(&mut self)->Option<DecoderEvent<Handle>> {
        match self {Self::H264(d)=>d.next_event(),Self::Hevc(d)=>d.next_event(),Self::Av1(d)=>d.next_event()}
    }
    fn stream_info(&self)->Option<&StreamInfo> {
        match self {Self::H264(d)=>d.stream_info(),Self::Hevc(d)=>d.stream_info(),Self::Av1(d)=>d.stream_info()}
    }
    fn finish_access_unit(&mut self)->Result<(),DecodeError> {
        match self {Self::H264(d)=>d.finish_access_unit(),Self::Hevc(d)=>d.finish_access_unit(),Self::Av1(_)=>Ok(())}
    }
}
struct Decoder {
    decoder: BitstreamDecoder,
    codec: VideoCodec,
    display: std::rc::Rc<Display>,
    pool: Option<FramePool<GenericDmaVideoFrame>>,
    name: String,
    /// Frames decoded before the caller picks them up; one `decode` call can
    /// release several pictures (reordering).
    pending: VecDeque<RawFrame>,
    pixels: super::pixels::Pool,
    drm_device: Option<u64>,
    importable: Arc<AtomicBool>,
    dma_readback: bool,
    mappings: download::Mappings,
    p010_image_format: Option<cros_libva::VAImageFormat>,
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
    fn open(codec:VideoCodec) -> Result<Self, String> {
        let (path, display) = open_display(codec)?;
        let decoder=match codec {
            VideoCodec::Pyrowave=>return Err("PyroWave uses the Vulkan decoder".into()),
            VideoCodec::H264=>BitstreamDecoder::H264(StatelessDecoder::<H264,_>::new_vaapi(display.clone(),BlockingMode::Blocking).map_err(|e|format!("VA-API H.264 decoder: {e}"))?),
            VideoCodec::Hevc=>BitstreamDecoder::Hevc(StatelessDecoder::<H265,_>::new_vaapi(display.clone(),BlockingMode::Blocking).map_err(|e|format!("VA-API HEVC decoder: {e}"))?),
            VideoCodec::Av1=>BitstreamDecoder::Av1(StatelessDecoder::<Av1,_>::new_vaapi(display.clone(),BlockingMode::Blocking).map_err(|e|format!("VA-API AV1 decoder: {e}"))?),
        };
        Ok(Self {
            decoder,
            codec,
            display,
            pool: None,
            name: format!("vaapi (cros-codecs hw, {}{})", match codec {VideoCodec::H264=>"",VideoCodec::Hevc=>"HEVC, ",VideoCodec::Av1=>"AV1, ",VideoCodec::Pyrowave=>"PyroWave, "}, path.display()),
            pending: VecDeque::new(),
            pixels: super::pixels::Pool::default(),
            mappings: download::Mappings::default(),
            drm_device: path.metadata().ok().filter(|m| m.file_type().is_char_device()).map(|m| m.rdev()),
            importable: Default::default(),
            p010_image_format: None,
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
        let patched = if self.codec==VideoCodec::H264 {crate::bitstream::patch_sps_allow_gaps(&frame.data)}else{None};
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
            if self.codec!=VideoCodec::Av1 && input.len() < data.len() && input.iter().all(|byte| *byte == 0) {
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
                    if self.pump_events(timestamp)? == 0 {
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
                if self.pump_events(timestamp)? == 0 {
                    stalls += 1;
                    if stalls > 2 {
                        return Err("VA-API decoder stopped consuming input".to_string());
                    }
                }
                continue;
            }
            stalls = 0;
            input = &input[consumed..];
            self.pump_events(timestamp)?;
        }
        self.decoder.finish_access_unit().map_err(|e| format!("VA access-unit submission: {e}"))?;
        self.pump_events(timestamp)?;
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
    fn decode_frames(&mut self, frame: &EncodedFrame) -> Result<Vec<RawFrame>, String> {
        let mut frames: Vec<_> = self.decode(frame)?.into_iter().collect();
        frames.extend(self.pending.drain(..));
        Ok(frames)
    }
}

impl Decoder {
    /// Handle every pending decoder event, returning how many were processed.
    fn pump_events(&mut self,display_timestamp:u64) -> Result<usize, String> {
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
                    // AV1 show_existing_frame displays a retained reference in
                    // the current temporal unit. Its original pixel timestamp
                    // is not the new presentation timestamp. H26x keeps DPB PTS.

                    let sync_started = self.profile.enabled.then(std::time::Instant::now);
                    handle.sync().map_err(|e| format!("surface sync: {e}"))?;
                    if let Some(started) = sync_started { self.profile.sync_us += started.elapsed().as_micros() as u64; }
                    let download_started = self.profile.enabled.then(std::time::Instant::now);
                    let frame = handle.video_frame();
                    let resolution = handle.display_resolution();
                    let format = match u32::from(frame.fourcc()) {
                        cros_libva::VA_FOURCC_NV12 => PixelFormat::Nv12,
                        cros_libva::VA_FOURCC_P010 => PixelFormat::P010,
                        other => return Err(format!("Unsupported VA decode surface {other:#x}")),
                    };
                    let rect = handle.borrow().visible_rect();
                    let origin = [rect.x, rect.y];
                    let native_nv12 = format == PixelFormat::Nv12 && origin == [0, 0];
                    if (native_nv12 || format == PixelFormat::P010) && crate::dma_buf::rendering() && self.importable.load(Ordering::Acquire) {
                        let exported = handle.borrow().surface().export_prime();
                        if let Ok(mut descriptor) = exported {
                            if descriptor.fourcc == u32::from(frame.fourcc())
                                && descriptor.objects.len()==1 && descriptor.layers.len()==1
                                && descriptor.layers[0].drm_format == descriptor.fourcc
                                && descriptor.layers[0].num_planes==2
                                && descriptor.layers[0].object_index[..2]==[0,0] {
                                let layer=&descriptor.layers[0];
                                let object=descriptor.objects.remove(0);
                                let gpu=crate::dma_buf::Frame {
                                    format, origin, file:object.fd.into(),allocation_size:object.size,device:self.drm_device,
                                    width:descriptor.width,height:descriptor.height,
                                    pitch:[layer.pitch[0],layer.pitch[1]],offset:[layer.offset[0],layer.offset[1]],
                                    modifier:object.drm_format_modifier,keep_alive:frame.clone(),importable:self.importable.clone(),
                                };
                                if gpu.valid([resolution.width,resolution.height]) {
                                    if self.profile.enabled {self.profile.frames+=1;self.profile.gpu_frames+=1;}
                                    self.pending.push_back(RawFrame {width:resolution.width,height:resolution.height,
                                        format,stride:format.row_bytes(resolution.width),data:Bytes::new(),
                                        pts:Timestamp(if self.codec==VideoCodec::Av1 {display_timestamp}else{handle.timestamp()}),gpu:true,dma_buf:Some(std::sync::Arc::new(gpu)),d3d11:None});
                                    continue;
                                }
                            }
                        }
                        // This pool falls back until a later format configures a new pool.
                        self.importable.store(false, Ordering::Release);
                    }
                    // Map through VA, which understands the driver's tiling.
                    // The upstream generic DMA mapper assumes every padding
                    // row is a complete Intel Y tile and panics on iHD exports.
                    let picture = handle.borrow();
                    let surface = picture.surface();
                    let direct = if native_nv12 && self.dma_readback {
                        match surface.export_prime().map_err(|e| e.to_string())
                            .and_then(|descriptor| download::download(descriptor,&mut self.mappings,resolution.width,resolution.height,&self.pixels)) {
                            Ok(pixels) => Some(pixels),
                            Err(reason) => {
                                // An unknown modifier or DMA mapping failure must
                                // keep hardware decode, using the driver layout.
                                tracing::debug!(%reason,"VAAPI DMA-BUF download unavailable; keeping driver mapping");
                                self.dma_readback = false;
                                self.mappings = download::Mappings::default();
                                None
                            }
                        }
                    } else { None };
                    let pixels = if let Some(pixels) = direct { pixels } else {
                        let visible = (resolution.width + origin[0], resolution.height + origin[1]);
                        let derived = cros_libva::Image::derive_from(surface, visible);
                        let image = match format {
                            PixelFormat::P010 => match derived {
                                Ok(image) if image.image().format.fourcc == cros_libva::VA_FOURCC_P010 => image,
                                _ => cros_libva::Image::create_from(
                                    surface, self.p010_image_format.ok_or("VA P010 image format is unavailable")?,
                                    surface.size(), visible,
                                ).map_err(|e| format!("VA P010 image download: {e}"))?,
                            },
                            _ => derived.map_err(|e| format!("VA image mapping: {e}"))?,
                        };
                        let layout = image.image();
                        if layout.format.fourcc != u32::from(frame.fourcc()) || layout.num_planes != 2
                            || u32::from(layout.width) < visible.0 || u32::from(layout.height) < visible.1 {
                            return Err("Mapped VA image does not match the decoded surface".into());
                        }
                        let uv = layout.offsets[1] as usize;
                        let y = layout.offsets[0] as usize;
                        let data = image.as_ref();
                        let planes = [
                            data.get(y..uv).ok_or("VA luma range outside mapping")?,
                            data.get(uv..).ok_or("VA chroma offset outside mapping")?,
                        ];
                        let pitches = [layout.pitches[0] as usize, layout.pitches[1] as usize];
                        Bytes::from(pack_420(&planes, &pitches, resolution.width, resolution.height, format, origin)?)
                    };
                    if let Some(started) = download_started {
                        self.profile.download_us += started.elapsed().as_micros() as u64;
                        self.profile.frames += 1;
                    }
                    self.pending.push_back(RawFrame {
                        width: resolution.width,
                        height: resolution.height,
                        format,
                        stride: format.row_bytes(resolution.width),
                        data: pixels,
                        pts: Timestamp(if self.codec==VideoCodec::Av1 {display_timestamp}else{handle.timestamp()}),
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
        let (rt_format, fourcc) = match info.format {
            DecodedFormat::NV12 => (cros_libva::VA_RT_FORMAT_YUV420, cros_libva::VA_FOURCC_NV12),
            DecodedFormat::P010 => (cros_libva::VA_RT_FORMAT_YUV420_10, cros_libva::VA_FOURCC_P010),
            other => return Err(format!("Unsupported VA decoded format {other:?}")),
        };
        let p010_image_format = if info.format == DecodedFormat::P010 {
            self.display.query_image_formats().map_err(|e| format!("Querying VA image formats: {e}"))?
                .into_iter().find(|f| f.fourcc == cros_libva::VA_FOURCC_P010)
        } else { None };
        if !(1..=64).contains(&info.min_num_frames) {
            return Err(format!("driver wants {} surfaces", info.min_num_frames));
        }
        if info.display_resolution.width == 0 || info.display_resolution.height == 0 {
            return Err("stream has no display resolution".to_string());
        }
        // Allocate before entering the pool callback: that callback cannot
        // report failures, so every frame it hands out must already exist.
        let mut pool_info=info.clone();
        let mut importable = false;
        let mut mappings = download::Mappings::default();
        let mut prepared = VecDeque::new();
        let mut prepare = || -> Result<_, String> {
            // Let the VA driver choose its native allocation. ChromeOS GBM's
            // HW_VIDEO_DECODER flag is not supported by desktop Mesa drivers.
            let surface = self
                .display
                .create_surfaces(
                    rt_format,
                    Some(fourcc),
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
            if descriptor.fourcc != fourcc
                || descriptor.width < info.coded_resolution.width || descriptor.height < info.coded_resolution.height
                || descriptor.objects.len() != 1
                || descriptor.layers.len() != 1
                || descriptor.layers[0].drm_format != fourcc
                || descriptor.layers[0].num_planes != 2
                || descriptor.layers[0].object_index[..2] != [0, 0]
            {
                return Err(format!("VA driver exported an unsupported {:?} layout", info.format));
            }
            let modifier = descriptor.objects[0].drm_format_modifier;
            let layer = &descriptor.layers[0];
            if !(layer.offset[0] < layer.offset[1] && layer.offset[1] < descriptor.objects[0].size) {
                return Err("VA decode plane ranges overlap or exceed the allocation".into());
            }
            let layout = FrameLayout {
                format: (
                    Fourcc::from(fourcc),
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
            let object_size = descriptor.objects[0].size as usize;
            let handles: Vec<_> = descriptor
                .objects
                .into_iter()
                .map(|o| std::fs::File::from(o.fd))
                .collect();
            if info.format == DecodedFormat::NV12 && self.dma_readback { mappings.register(&handles[0], object_size); }
            let frame = GenericDmaVideoFrame::new(handles, layout)?;
            Ok((frame, modifier))
        };
        while prepared.len() < pool_info.min_num_frames {
            match prepare() {
                Ok((frame, modifier)) => {
                    let format = if info.format == DecodedFormat::P010 { PixelFormat::P010 } else { PixelFormat::Nv12 };
                    let supported = crate::dma_buf::supports_pool(format, self.drm_device, modifier);
                    if prepared.is_empty() && supported {
                        // Allow a visible picture, a pending frame and bounded GL retirement.
                        importable = true;
                        pool_info.min_num_frames += 6;
                    } else if importable && !supported {
                        importable = false;
                        pool_info.min_num_frames = info.min_num_frames.max(prepared.len()+1);
                    }
                    prepared.push_back(frame);
                }
                Err(error) if prepared.len() >= info.min_num_frames => {
                    // Optional renderer slots must not turn a viable CPU decoder into a startup failure.
                    eprintln!("VA surface import disabled for this pool: {error}");
                    importable = false;
                    pool_info.min_num_frames = prepared.len();
                }
                Err(error) => return Err(error),
            }
        }
        let mut pool = FramePool::new(move |_| {
            prepared
                .pop_front()
                .expect("one prepared surface per advertised pool slot")
        });
        // Each prepared frame owns immutable planes in one driver allocation.
        // Returning the final decoder or GUI lease is the only route back to its slot.
        pool.resize_for_va_surface_reuse(&pool_info);
        self.pool = Some(pool);
        self.mappings = mappings;
        self.importable = Arc::new(AtomicBool::new(importable));
        self.p010_image_format = p010_image_format;
        Ok(())
    }
}

/// Open the first DRM render node whose driver can decode H.264.
fn open_display(codec:VideoCodec) -> Result<(PathBuf, std::rc::Rc<Display>), String> {
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
        match open_node(&path,codec) {
            Ok(display) => return Ok((path, display)),
            Err(e) => failures.push(format!("{}: {e}", path.display())),
        }
    }
    Err(format!(
        "no render node decodes {codec:?} with VA-API: {}",
        failures.join("; ")
    ))
}

fn open_node(path: &Path,codec:VideoCodec) -> Result<std::rc::Rc<Display>, String> {
    let display = Display::open_drm_display(path).map_err(|e| format!("VA display: {e}"))?;
    let profiles = display
        .query_config_profiles()
        .map_err(|e| format!("querying VA profiles: {e}"))?;
    let requested:&[VAProfile::Type]=match codec {
        VideoCodec::H264=>&[VAProfile::VAProfileH264High,VAProfile::VAProfileH264Main],
        VideoCodec::Hevc=>&[VAProfile::VAProfileHEVCMain, VAProfile::VAProfileHEVCMain10],
        VideoCodec::Av1=>&[VAProfile::VAProfileAV1Profile0],
        VideoCodec::Pyrowave=>return Err("PyroWave uses the Vulkan decoder".into()),
    };
    for &profile in requested {
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
    Err(format!("driver offers no {codec:?} VLD entry point"))
}

/// Copy the visible rectangle of a padded NV12 or P010 image to tight Y and UV planes.
fn pack_420(
    planes: &[&[u8]],
    pitches: &[usize],
    width: u32,
    height: u32,
    format: PixelFormat,
    origin: [u32; 2],
) -> Result<Vec<u8>, String> {
    let h = height as usize;
    let bytes_per_sample = match format {
        PixelFormat::Nv12 => 1,
        PixelFormat::P010 => 2,
        _ => return Err("Expected a semi-planar VA image".into()),
    };
    let w = (width as usize).checked_mul(bytes_per_sample).ok_or("VA row size overflow")?;
    if planes.len() < 2 || pitches.len() < 2 {
        return Err(format!("VA surface has {} planes", planes.len()));
    }
    if width == 0 || h == 0 || width % 2 != 0 || h % 2 != 0 {
        return Err("4:2:0 dimensions must be positive and even".into());
    }
    if origin[0] % 2 != 0 || origin[1] % 2 != 0 {
        return Err("4:2:0 crop origin must be even".into());
    }
    let x = (origin[0] as usize).checked_mul(bytes_per_sample).ok_or("VA crop offset overflow")?;
    let right = x.checked_add(w).ok_or("VA crop width overflow")?;
    let top = origin[1] as usize;
    let mut visible = [&[][..]; 2];
    for (i, rows) in [h, h / 2].into_iter().enumerate() {
        let start = pitches[i].checked_mul(top >> i).and_then(|n| n.checked_add(x))
            .ok_or("VA crop offset overflow")?;
        visible[i] = planes[i].get(start..).ok_or("VA crop outside mapping")?;
        let needed = pitches[i]
            .checked_mul(rows - 1)
            .and_then(|n| n.checked_add(w));
        if pitches[i] < right || needed.map_or(true, |n| visible[i].len() < n) {
            return Err(format!(
                "VA plane {i} is truncated or has an invalid pitch"
            ));
        }
    }
    let length = w
        .checked_mul(h)
        .and_then(|n| n.checked_mul(3))
        .map(|n| n / 2)
        .ok_or("VA dimensions overflow")?;
    let mut out = vec![0u8; length];
    for row in 0..h {
        out[row * w..(row + 1) * w]
            .copy_from_slice(&visible[0][row * pitches[0]..row * pitches[0] + w]);
    }
    let chroma = w * h;
    for row in 0..h / 2 {
        out[chroma + row * w..chroma + (row + 1) * w]
            .copy_from_slice(&visible[1][row * pitches[1]..row * pitches[1] + w]);
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
        let packed = pack_420(&[&y, &uv], &[4, 4], 2, 2, PixelFormat::Nv12, [0, 0]).unwrap();
        assert_eq!(packed, vec![1, 2, 3, 4, 10, 20]);
    }

    #[test]
    fn p010_crop_keeps_both_high_aligned_planes_and_their_padding_separate() {
        let y: Vec<u8> = (0u16..24).flat_map(|v| (v << 6).to_le_bytes()).collect();
        let uv: Vec<u8> = (100u16..112).flat_map(|v| (v << 6).to_le_bytes()).collect();
        let expected: Vec<u8> = [14u16, 15, 20, 21, 108, 109].into_iter()
            .flat_map(|v| (v << 6).to_le_bytes()).collect();
        let packed = pack_420(&[&y, &uv], &[12, 12], 2, 2, PixelFormat::P010, [2, 2]).unwrap();
        assert_eq!(packed, expected);
        assert!(pack_420(&[&y, &uv], &[12, 12], 2, 2, PixelFormat::P010, [1, 2]).is_err());
        assert!(pack_420(&[&y, &uv], &[12, 12], 2, 2, PixelFormat::P010, [6, 2]).is_err());
        assert!(pack_420(&[&y, &uv[..17]], &[12, 12], 2, 2, PixelFormat::P010, [2, 2]).is_err());
    }

    #[test]
    fn truncated_planes_and_invalid_layouts_are_rejected() {
        assert!(pack_420(&[&[7u8], &[]], &[8, 8], 2, 2, PixelFormat::Nv12, [0, 0]).is_err());
        assert!(pack_420(&[&[0; 8], &[0; 4]], &[1, 4], 2, 2, PixelFormat::Nv12, [0, 0]).is_err());
        assert!(pack_420(&[&[0; 8], &[0; 4]], &[4, 4], 3, 2, PixelFormat::Nv12, [0, 0]).is_err());
    }
}
