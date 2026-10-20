//! Thread-owned VA-API H.264 encoding through the existing BSD cros-codecs API.
//! Input is uploaded as limited-range BT.601 NV12; output remains CPU Annex-B.
use super::{error, Settings};
use crate::{codec::CodecError, EncodedFrame, RawFrame, Timestamp, VideoEncoder};
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
    settings: Settings,
    name: String,
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
        let packed = crate::convert::to_i420(frame).map_err(|e| e.to_string())?;
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
            upload_i420(
                &packed,
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
    if input.len() != length
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
            for (dest, value) in dest[..width]
                .iter_mut()
                .zip(&input[row * width..(row + 1) * width])
            {
                *dest = 16 + ((u32::from(*value) * 219 + 127) / 255) as u8;
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
                dest[column * 2] = 16 + ((u32::from(input[pixels + i]) * 224 + 127) / 255) as u8;
                dest[column * 2 + 1] =
                    16 + ((u32::from(input[pixels + pixels / 4 + i]) * 224 + 127) / 255) as u8;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
