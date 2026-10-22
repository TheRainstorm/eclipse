//! Thread-owned VA-API H.264 encoding through the existing BSD cros-codecs API.
//! Input is uploaded as limited-range BT.601 NV12; output remains CPU Annex-B.
use super::upload::{prepare_bgra, upload_planes};
use super::Settings;
use crate::{EncodedFrame, RawFrame, Timestamp};
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
use openh264::formats::{YUVBuffer, YUVSource};
use std::{path::PathBuf, rc::Rc};

type DriverEncoder = StatelessEncoder<
    GenericDmaVideoFrame,
    VaapiBackend<GenericDmaVideoFrame, Surface<GenericDmaVideoFrame>>,
>;

pub(super) struct Driver {
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

fn tunings(settings: Settings) -> Tunings {
    Tunings {
        rate_control: RateControl::ConstantBitrate(u64::from(settings.bitrate_kbps) * 1000),
        framerate: settings.fps,
        min_quality: 0,
        max_quality: 51,
    }
}
impl Driver {
    pub(super) fn open(settings: Settings) -> Result<Self, String> {
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

impl super::worker::Driver for Driver {
    fn backend_name(&self) -> &str {
        &self.name
    }
    fn encode(&mut self, frame: &RawFrame, keyframe: bool) -> Result<Vec<EncodedFrame>, String> {
        Driver::encode(self, frame, keyframe)
    }
    fn bitrate(&mut self, kbps: u32) -> Result<(), String> {
        Driver::bitrate(self, kbps)
    }
}
