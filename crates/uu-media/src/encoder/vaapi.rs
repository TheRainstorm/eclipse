//! Thread-owned VA-API H.264 encoding through the existing BSD cros-codecs API.
//! Input is uploaded as limited-range BT.601 NV12; output remains CPU Annex-B.
use super::{error, Settings};
use crate::{codec::CodecError, EncodedFrame, PixelFormat, RawFrame, Timestamp, VideoEncoder};
use cros_codecs::{
    backend::vaapi::encoder::VaapiBackend,
    codec::h264::parser::{Level, Profile},
    encoder::h264::EncoderConfig,
    encoder::stateless::h264::StatelessEncoder,
    encoder::{
        FrameMetadata, PredictionStructure, RateControl, Tunings, VideoEncoder as CrosEncoder,
    },
    video_frame::generic_dma_video_frame::GenericDmaVideoFrame,
    BlockingMode, Fourcc, FrameLayout, PlaneLayout, Resolution,
};
use cros_libva::{Display, Surface, UsageHint, VAEntrypoint, VAProfile};
use openh264::formats::{BGRA8Source, RGB8Source, RGBSource, YUVBuffer, YUVSource};
use std::{
    path::PathBuf,
    rc::Rc,
    sync::mpsc::{self, SyncSender},
    thread::JoinHandle,
    time::Duration,
};

type DriverEncoder = StatelessEncoder<
    GenericDmaVideoFrame,
    VaapiBackend<GenericDmaVideoFrame, Surface<GenericDmaVideoFrame>>,
>;

enum Job {
    Encode(
        RawFrame,
        bool,
        mpsc::Sender<Result<Vec<EncodedFrame>, String>>,
    ),
    Bitrate(u32, mpsc::Sender<Result<(), String>>),
}
pub(super) struct VaapiEncoder {
    sender: Option<SyncSender<Job>>,
    thread: Option<JoinHandle<()>>,
    name: String,
    keyframe: bool,
}
impl VaapiEncoder {
    pub fn new(settings: Settings) -> Result<Self, CodecError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let (ready, ready_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("uu-hwenc".into())
            .spawn(move || {
                let mut driver = match Driver::open(settings) {
                    Ok(driver) => driver,
                    Err(reason) => {
                        let _ = ready.send(Err(reason));
                        return;
                    }
                };
                if ready.send(Ok(driver.name.clone())).is_err() {
                    return;
                }
                while let Ok(job) = receiver.recv() {
                    let replied = match job {
                        Job::Encode(frame, keyframe, reply) => {
                            reply.send(driver.encode(&frame, keyframe)).is_ok()
                        }
                        Job::Bitrate(kbps, reply) => reply.send(driver.bitrate(kbps)).is_ok(),
                    };
                    if !replied {
                        break;
                    }
                }
                // Driver-owned VA resources are released on this same OS thread.
            })
            .map_err(|e| error(format!("hardware encoder thread: {e}")))?;
        let name = match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(name)) => name,
            Ok(Err(reason)) => {
                let _ = thread.join();
                return Err(error(reason));
            }
            Err(_) => {
                return Err(error(
                    "hardware encoder did not initialize within 10 seconds",
                ))
            }
        };
        Ok(Self {
            sender: Some(sender),
            thread: Some(thread),
            name,
            keyframe: true,
        })
    }
    fn reply<T>(&mut self, receiver: mpsc::Receiver<Result<T, String>>) -> Result<T, CodecError> {
        match receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(result) => result.map_err(error),
            Err(_) => {
                // Do not enqueue more work or block Drop on a stalled driver.
                // A returning driver sees the disconnected reply/command queue
                // and releases its own resources; no thread is forcibly killed.
                self.sender = None;
                Err(error("hardware encoder stopped responding"))
            }
        }
    }
}
impl VideoEncoder for VaapiEncoder {
    fn backend_name(&self) -> &str {
        &self.name
    }
    fn codec(&self) -> uu_proto::media::VideoCodec {
        uu_proto::media::VideoCodec::H264
    }
    fn encode(&mut self, frame: &RawFrame) -> Result<Vec<EncodedFrame>, CodecError> {
        let (reply, receiver) = mpsc::channel();
        self.sender
            .as_ref()
            .ok_or_else(|| error("hardware encoder is unavailable"))?
            .send(Job::Encode(frame.clone(), self.keyframe, reply))
            .map_err(|_| error("hardware encoder thread ended"))?;
        let result = self.reply(receiver);
        if result.is_ok() {
            self.keyframe = false;
        }
        result
    }
    fn request_keyframe(&mut self) {
        self.keyframe = true;
    }
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError> {
        let (reply, receiver) = mpsc::channel();
        self.sender
            .as_ref()
            .ok_or_else(|| error("hardware encoder is unavailable"))?
            .send(Job::Bitrate(kbps, reply))
            .map_err(|_| error("hardware encoder thread ended"))?;
        self.reply(receiver)
    }
}
impl Drop for VaapiEncoder {
    fn drop(&mut self) {
        let responding = self.sender.take().is_some();
        if let Some(thread) = self.thread.take() {
            if responding || thread.is_finished() {
                let _ = thread.join();
            }
        }
    }
}

struct Driver {
    encoder: DriverEncoder,
    input: Surface<()>,
    input_fd: std::fs::File,
    layout: FrameLayout,
    image_format: cros_libva::VAImageFormat,
    direct_upload: bool,
    bgra_buffer: Option<YUVBuffer>,
    settings: Settings,
    name: String,
}

// The library's standard BGRA adapter supports padded rows in whole pixels.
// Odd byte pitches continue through the existing conversion path.
struct BgraInput<'a>(&'a RawFrame);
impl RGBSource for BgraInput<'_> {
    fn dimensions(&self) -> (usize, usize) {
        (self.0.width as usize, self.0.height as usize)
    }
    fn pixel_f32(&self, x: usize, y: usize) -> (f32, f32, f32) {
        let offset = y * self.0.stride as usize + x * 4;
        (
            self.0.data[offset + 2] as f32,
            self.0.data[offset + 1] as f32,
            self.0.data[offset] as f32,
        )
    }
}
impl RGB8Source for BgraInput<'_> {
    fn dimensions_padded(&self) -> (usize, usize) {
        (self.0.stride as usize / 4, self.0.height as usize)
    }
    fn rgb8_data(&self) -> &[u8] {
        &self.0.data
    }
    fn pixel_stride(&self) -> usize {
        4
    }
    fn rgb_channel_offsets(&self) -> (usize, usize, usize) {
        (2, 1, 0)
    }
}
impl BGRA8Source for BgraInput<'_> {}

fn prepare_bgra(buffer: &mut Option<YUVBuffer>, frame: &RawFrame) -> Result<bool, String> {
    if frame.format != PixelFormat::Bgra8 || frame.stride % 4 != 0 {
        return Ok(false);
    }
    let (width, height, _) =
        crate::convert::validate_cpu_frame(frame).map_err(|e| e.to_string())?;
    if buffer
        .as_ref()
        .map_or(true, |buffer| buffer.dimensions() != (width, height))
    {
        *buffer = Some(YUVBuffer::new(width, height));
    }
    buffer
        .as_mut()
        .ok_or("BGRA buffer was not prepared")?
        .read_bgra8(BgraInput(frame));
    Ok(true)
}
fn tunings(settings: Settings) -> Tunings {
    Tunings {
        rate_control: RateControl::ConstantBitrate(u64::from(settings.bitrate_kbps) * 1000),
        framerate: settings.fps,
        min_quality: 0,
        max_quality: 51,
    }
}
impl Driver {
    fn open(settings: Settings) -> Result<Self, String> {
        let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev/dri")
            .map_err(|e| format!("VA-API render nodes: {e}"))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|v| v.to_str())
                    .is_some_and(|v| v.starts_with("renderD"))
            })
            .collect();
        nodes.sort();
        let mut failures = Vec::new();
        for path in nodes {
            for low_power in [true, false] {
                match Self::open_node(settings, &path, low_power) {
                    Ok(driver) => return Ok(driver),
                    Err(reason) => failures.push(reason),
                }
            }
        }
        Err(format!(
            "no usable VA-API H.264 encoder: {}",
            failures.join("; ")
        ))
    }
    fn open_node(
        settings: Settings,
        path: &std::path::Path,
        low_power: bool,
    ) -> Result<Self, String> {
        let display = Display::open_drm_display(path).map_err(|e| format!("VA display: {e}"))?;
        let entries = display
            .query_config_entrypoints(VAProfile::VAProfileH264ConstrainedBaseline)
            .map_err(|e| e.to_string())?;
        let entry = if low_power {
            VAEntrypoint::VAEntrypointEncSliceLP
        } else {
            VAEntrypoint::VAEntrypointEncSlice
        };
        if !entries.contains(&entry) {
            return Err("driver has no requested baseline encoding entry point".into());
        }
        let coded = Resolution {
            width: settings.width.next_multiple_of(16),
            height: settings.height.next_multiple_of(16),
        };
        let config = EncoderConfig {
            resolution: Resolution {
                width: settings.width,
                height: settings.height,
            },
            profile: Profile::Baseline,
            level: Level::L5_2,
            pred_structure: PredictionStructure::LowDelay {
                limit: (settings.fps * 2) as u16,
            },
            initial_tunings: tunings(settings),
        };
        let encoder = DriverEncoder::new_vaapi(
            Rc::clone(&display),
            config,
            Fourcc::from(b"NV12"),
            coded,
            low_power,
            BlockingMode::Blocking,
        )
        .map_err(|e| format!("VA H.264 encoder: {e}"))?;
        let input = display
            .create_surfaces(
                cros_libva::VA_RT_FORMAT_YUV420,
                Some(cros_libva::VA_FOURCC_NV12),
                coded.width,
                coded.height,
                Some(UsageHint::USAGE_HINT_ENCODER | UsageHint::USAGE_HINT_EXPORT),
                vec![()],
            )
            .map_err(|e| e.to_string())?
            .pop()
            .ok_or("VA driver created no input surface")?;
        let mut descriptor = input.export_prime().map_err(|e| e.to_string())?;
        if descriptor.objects.len() != 1
            || descriptor.layers.len() != 1
            || descriptor.layers[0].num_planes != 2
            || descriptor.layers[0].object_index[..2] != [0, 0]
        {
            return Err("unsupported VA input NV12 DMA layout".into());
        }
        let layer = &descriptor.layers[0];
        let layout = FrameLayout {
            format: (
                Fourcc::from(b"NV12"),
                descriptor.objects[0].drm_format_modifier,
            ),
            size: coded,
            planes: (0..2)
                .map(|i| PlaneLayout {
                    buffer_index: 0,
                    offset: layer.offset[i] as usize,
                    stride: layer.pitch[i] as usize,
                })
                .collect(),
        };
        let input_fd = std::fs::File::from(descriptor.objects.pop().unwrap().fd);
        let image_format = display
            .query_image_formats()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|format| format.fourcc == cros_libva::VA_FOURCC_NV12)
            .ok_or("driver cannot map NV12 uploads")?;
        // Probe a direct CPU view before streaming. Drivers that cannot derive
        // NV12 retain the conservative get/put-image upload path.
        let size = (coded.width, coded.height);
        let direct_upload = cros_libva::Image::derive_from(&input, size)
            .map(|image| {
                let info = image.image();
                info.format.fourcc == cros_libva::VA_FOURCC_NV12
                    && info.num_planes >= 2
                    && info.pitches[0] >= coded.width
                    && info.pitches[1] >= coded.width
            })
            .unwrap_or(false);
        Ok(Self {
            encoder,
            input,
            input_fd,
            layout,
            image_format,
            direct_upload,
            bgra_buffer: None,
            settings,
            name: format!(
                "vaapi (H.264 hw, {}, {}, {} upload)",
                path.display(),
                if low_power { "low power" } else { "slice" },
                if direct_upload { "mapped" } else { "copy" }
            ),
        })
    }
    fn encode(&mut self, frame: &RawFrame, keyframe: bool) -> Result<Vec<EncodedFrame>, String> {
        if (frame.width, frame.height) != (self.settings.width, self.settings.height) {
            return Err("VA input dimensions changed".into());
        }
        let packed = if prepare_bgra(&mut self.bgra_buffer, frame)? {
            None
        } else {
            Some(crate::convert::to_i420(frame).map_err(|e| e.to_string())?)
        };
        let pixels = frame.width as usize * frame.height as usize;
        let (planes, full_range) = if let Some(packed) = packed.as_ref() {
            (
                [
                    &packed[..pixels],
                    &packed[pixels..pixels + pixels / 4],
                    &packed[pixels + pixels / 4..],
                ],
                true,
            )
        } else {
            let input = self
                .bgra_buffer
                .as_ref()
                .ok_or("BGRA buffer was not prepared")?;
            ([input.y(), input.u(), input.v()], false)
        };
        {
            let size = (self.layout.size.width, self.layout.size.height);
            let mut image = if self.direct_upload {
                cros_libva::Image::derive_from(&self.input, size)
            } else {
                cros_libva::Image::create_from(&self.input, self.image_format, size, size)
            }
            .map_err(|e| e.to_string())?;
            if image.image().format.fourcc != cros_libva::VA_FOURCC_NV12 {
                return Err("driver changed its NV12 upload format".into());
            }
            let info = *image.image();
            upload_planes(
                planes,
                full_range,
                self.settings.width as usize,
                self.settings.height as usize,
                size,
                info.pitches,
                info.offsets,
                image.as_mut(),
            )?;
        }
        self.input.sync().map_err(|e| e.to_string())?;
        let handle = GenericDmaVideoFrame::new(
            vec![self.input_fd.try_clone().map_err(|e| e.to_string())?],
            self.layout.clone(),
        )?;
        self.encoder
            .encode(
                FrameMetadata {
                    timestamp: frame.pts.0,
                    layout: self.layout.clone(),
                    force_keyframe: keyframe,
                },
                handle,
            )
            .map_err(|e| format!("VA encode: {e}"))?;
        let mut output = Vec::new();
        while let Some(coded) = self
            .encoder
            .poll()
            .map_err(|e| format!("VA encoder completion: {e}"))?
        {
            let is_keyframe = EncodedFrame::detect_h264_keyframe(&coded.bitstream);
            if is_keyframe
                && crate::color::from_h264(&coded.bitstream)
                    != Some(crate::color::YuvColor::Limited601)
            {
                return Err("VA encoder SPS does not match the limited BT.601 upload".into());
            }
            output.push(EncodedFrame {
                pts: Timestamp(coded.metadata.timestamp),
                is_keyframe,
                data: coded.bitstream.into(),
            });
        }
        if output.len() != 1 || output[0].pts != frame.pts {
            return Err("VA low-delay encoder did not complete exactly its current frame".into());
        }
        Ok(output)
    }
    fn bitrate(&mut self, kbps: u32) -> Result<(), String> {
        let settings = Settings {
            bitrate_kbps: kbps,
            ..self.settings
        };
        self.encoder
            .tune(tunings(settings))
            .map_err(|e| format!("VA encoder tuning: {e}"))?;
        self.settings = settings;
        Ok(())
    }
}

#[cfg(test)]
fn upload_i420(
    input: &[u8],
    width: usize,
    height: usize,
    coded: (u32, u32),
    pitches: [u32; 3],
    offsets: [u32; 3],
    out: &mut [u8],
) -> Result<(), String> {
    let pixels = width.checked_mul(height).ok_or("invalid upload size")?;
    let length = pixels.checked_mul(3).ok_or("invalid upload size")? / 2;
    if input.len() != length {
        return Err("invalid VA upload length".into());
    }
    upload_planes(
        [
            &input[..pixels],
            &input[pixels..pixels + pixels / 4],
            &input[pixels + pixels / 4..],
        ],
        true,
        width,
        height,
        coded,
        pitches,
        offsets,
        out,
    )
}

fn upload_planes(
    planes: [&[u8]; 3],
    full_range: bool,
    width: usize,
    height: usize,
    coded: (u32, u32),
    pitches: [u32; 3],
    offsets: [u32; 3],
    out: &mut [u8],
) -> Result<(), String> {
    let pixels = width.checked_mul(height).ok_or("invalid upload size")?;
    if planes[0].len() != pixels
        || planes[1].len() != pixels / 4
        || planes[2].len() != pixels / 4
        || width > coded.0 as usize
        || height > coded.1 as usize
        || width % 2 != 0
        || height % 2 != 0
        || coded.0 % 2 != 0
        || coded.1 % 2 != 0
        || pitches[0] < coded.0
        || pitches[1] < coded.0
    {
        return Err("invalid VA upload geometry".into());
    }
    let coded_width = coded.0 as usize;
    for row in 0..coded.1 as usize {
        let offset = (offsets[0] as usize)
            .checked_add(
                row.checked_mul(pitches[0] as usize)
                    .ok_or("VA pitch overflow")?,
            )
            .ok_or("VA offset overflow")?;
        let end = offset.checked_add(coded_width).ok_or("VA row overflow")?;
        let dest = out
            .get_mut(offset..end)
            .ok_or("VA luma plane is too short")?;
        dest.fill(16);
        if row < height {
            let source = &planes[0][row * width..(row + 1) * width];
            if full_range {
                for (dest, value) in dest[..width].iter_mut().zip(source) {
                    *dest = 16 + ((u32::from(*value) * 219 + 127) / 255) as u8;
                }
            } else {
                dest[..width].copy_from_slice(source);
            }
        }
    }
    for row in 0..coded.1 as usize / 2 {
        let offset = (offsets[1] as usize)
            .checked_add(
                row.checked_mul(pitches[1] as usize)
                    .ok_or("VA pitch overflow")?,
            )
            .ok_or("VA offset overflow")?;
        let end = offset.checked_add(coded_width).ok_or("VA row overflow")?;
        let dest = out
            .get_mut(offset..end)
            .ok_or("VA chroma plane is too short")?;
        dest.fill(128);
        if row < height / 2 {
            for column in 0..width / 2 {
                let i = row * width / 2 + column;
                let (u, v) = (planes[1][i], planes[2][i]);
                dest[column * 2] = if full_range {
                    16 + ((u32::from(u) * 224 + 127) / 255) as u8
                } else {
                    u
                };
                dest[column * 2 + 1] = if full_range {
                    16 + ((u32::from(v) * 224 + 127) / 255) as u8
                } else {
                    v
                };
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bgra_frame(width: u32, height: u32, stride: u32, data: Vec<u8>) -> RawFrame {
        RawFrame {
            width,
            height,
            stride,
            data: data.into(),
            format: PixelFormat::Bgra8,
            pts: Timestamp(0),
            gpu: false,
            dma_buf: None,
        }
    }
    #[test]
    fn library_bgra_matches_independent_limited_601_for_padded_rows_and_ignores_alpha() {
        let mut seed = 17u32;
        for (width, height, padding) in [(2, 2, 0), (6, 18, 12), (64, 32, 64), (1920, 8, 28)] {
            let stride = width * 4 + padding;
            let mut data = vec![0xee; (stride * height) as usize];
            for row in 0..height as usize {
                for x in 0..width as usize * 4 {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    data[row * stride as usize + x] = (seed >> 24) as u8;
                }
            }
            let mut frame = bgra_frame(width, height, stride, data);
            let mut buffer = None;
            assert!(prepare_bgra(&mut buffer, &frame).unwrap());
            let converted = buffer.as_ref().unwrap();
            let source = |x: usize, y: usize| {
                let i = y * stride as usize + x * 4;
                (
                    frame.data[i + 2] as f64,
                    frame.data[i + 1] as f64,
                    frame.data[i] as f64,
                )
            };
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let (r, g, b) = source(x, y);
                    let expected = 16.0 + (65.481 * r + 128.553 * g + 24.966 * b) / 255.0;
                    assert!((converted.y()[y * width as usize + x] as f64 - expected).abs() <= 2.0);
                }
            }
            for y in 0..height as usize / 2 {
                for x in 0..width as usize / 2 {
                    let (mut r, mut g, mut b) = (0.0, 0.0, 0.0);
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let c = source(x * 2 + dx, y * 2 + dy);
                            r += c.0 / 4.0;
                            g += c.1 / 4.0;
                            b += c.2 / 4.0;
                        }
                    }
                    let i = y * width as usize / 2 + x;
                    let u = 128.0 + (-37.797 * r - 74.203 * g + 112.0 * b) / 255.0;
                    let v = 128.0 + (112.0 * r - 93.786 * g - 18.214 * b) / 255.0;
                    assert!((converted.u()[i] as f64 - u).abs() <= 2.0);
                    assert!((converted.v()[i] as f64 - v).abs() <= 2.0);
                }
            }
            let expected = [
                converted.y().to_vec(),
                converted.u().to_vec(),
                converted.v().to_vec(),
            ];
            let mut changed = frame.data.to_vec();
            for row in 0..height as usize {
                for x in 0..width as usize {
                    changed[row * stride as usize + x * 4 + 3] ^= 255;
                }
            }
            frame.data = changed.into();
            assert!(prepare_bgra(&mut buffer, &frame).unwrap());
            let converted = buffer.as_ref().unwrap();
            assert_eq!(
                [converted.y(), converted.u(), converted.v()],
                [
                    expected[0].as_slice(),
                    expected[1].as_slice(),
                    expected[2].as_slice()
                ]
            );
        }
    }
    #[test]
    fn limited_upload_preserves_white_endpoints_and_padding_and_checks_input_before_access() {
        let frame = bgra_frame(2, 2, 8, vec![255; 16]);
        let mut buffer = None;
        assert!(prepare_bgra(&mut buffer, &frame).unwrap());
        let image = buffer.as_ref().unwrap();
        assert_eq!(image.y(), [235; 4]);
        assert_eq!(image.u(), [128]);
        assert_eq!(image.v(), [128]);
        let mut output = vec![0xac; 80];
        upload_planes(
            [image.y(), image.u(), image.v()],
            false,
            2,
            2,
            (4, 4),
            [8, 8, 0],
            [8, 48, 0],
            &mut output,
        )
        .unwrap();
        assert_eq!(&output[8..12], [235, 235, 16, 16]);
        assert_eq!(&output[16..20], [235, 235, 16, 16]);
        assert_eq!(&output[24..28], [16; 4]);
        assert_eq!(&output[48..52], [128; 4]);
        assert_eq!(&output[12..16], [0xac; 4]);
        let mut invalid = frame.clone();
        invalid.gpu = true;
        let mut unused = None;
        assert!(prepare_bgra(&mut unused, &invalid).is_err());
        assert!(unused.is_none());
        invalid = frame.clone();
        invalid.data = bytes::Bytes::from_static(&[0; 4]);
        assert!(prepare_bgra(&mut unused, &invalid).is_err());
        assert!(unused.is_none());
        let odd_stride = bgra_frame(2, 2, 9, vec![0; 18]);
        assert!(!prepare_bgra(&mut unused, &odd_stride).unwrap());
        assert!(unused.is_none());
        assert!(crate::convert::to_i420(&odd_stride).is_ok());
    }
    #[test]
    fn uploads_preserve_pitch_padding_and_map_full_range_to_limited_nv12() {
        let mut output = vec![0xac; 80];
        upload_i420(
            &[0, 255, 0, 255, 128, 128],
            2,
            2,
            (4, 4),
            [8, 8, 0],
            [8, 48, 0],
            &mut output,
        )
        .unwrap();
        assert_eq!(&output[8..12], &[16, 235, 16, 16]);
        assert_eq!(&output[16..20], &[16, 235, 16, 16]);
        assert_eq!(&output[24..28], &[16; 4]);
        assert_eq!(&output[48..52], &[128; 4]);
        assert_eq!(&output[56..60], &[128; 4]);
        assert_eq!(&output[12..16], &[0xac; 4]);
        assert!(upload_i420(&[0; 6], 2, 2, (4, 4), [2, 8, 0], [8, 48, 0], &mut output).is_err());
        assert!(upload_i420(&[0; 6], 2, 2, (4, 4), [8, 8, 0], [8, 48, 0], &mut [0; 4]).is_err());
    }
}
