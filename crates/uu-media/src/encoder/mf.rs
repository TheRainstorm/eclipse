//! Windows GPU-driver H.264 MFT, written against the Media Foundation API.
//! Only hardware-registered async transforms are enumerated. NV12 input is
//! uploaded from CPU memory; this is hardware encoding, not GPU zero-copy.
use super::{
    upload::{prepare_bgra, upload_planes},
    Settings,
};
use crate::{EncodedFrame, RawFrame};
use openh264::formats::{YUVBuffer, YUVSource};
use std::{mem::ManuallyDrop, ptr};
use windows::core::{Interface, GUID};
use windows::Win32::{
    Foundation::E_NOTIMPL,
    Media::MediaFoundation::*,
    System::{
        Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED},
        Variant::VARIANT,
    },
};

fn reason(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn pack(a: u32, b: u32) -> u64 {
    (u64::from(a) << 32) | u64::from(b)
}

// Declared last in Driver: the transform is released before COM/MF shut down.
struct Runtime {
    timer_requested: bool,
}
impl Runtime {
    fn start() -> Result<Self, String> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(reason)?;
            if let Err(error) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                CoUninitialize();
                return Err(reason(error));
            }
            // Driver work queues can use Windows timed waits internally.
            // Keep a balanced 1 ms request only while this encoder is active.
            Ok(Self {
                timer_requested: windows::Win32::Media::timeBeginPeriod(1) == 0,
            })
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe {
            if self.timer_requested {
                windows::Win32::Media::timeEndPeriod(1);
            }
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}

struct Activations {
    entries: *mut Option<IMFActivate>,
    count: u32,
}
impl Activations {
    fn hardware() -> Result<Self, String> {
        unsafe {
            let mut value = Self {
                entries: ptr::null_mut(),
                count: 0,
            };
            let input = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Video,
                guidSubtype: MFVideoFormat_NV12,
            };
            let output = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Video,
                guidSubtype: MFVideoFormat_H264,
            };
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input),
                Some(&output),
                &mut value.entries,
                &mut value.count,
            )
            .map_err(reason)?;
            Ok(value)
        }
    }
    fn entries(&self) -> &[Option<IMFActivate>] {
        if self.entries.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.entries, self.count as usize) }
        }
    }
}
impl Drop for Activations {
    fn drop(&mut self) {
        unsafe {
            if !self.entries.is_null() {
                for entry in std::slice::from_raw_parts_mut(self.entries, self.count as usize) {
                    *entry = None;
                }
                CoTaskMemFree(Some(self.entries.cast()));
            }
        }
    }
}
fn friendly(activation: &IMFActivate) -> String {
    unsafe {
        let len = activation
            .GetStringLength(&MFT_FRIENDLY_NAME_Attribute)
            .unwrap_or(0)
            .min(1024);
        let mut text = vec![0u16; len as usize + 1];
        if activation
            .GetString(&MFT_FRIENDLY_NAME_Attribute, &mut text, None)
            .is_ok()
        {
            String::from_utf16_lossy(&text[..len as usize])
        } else {
            "GPU encoder".into()
        }
    }
}

pub(super) struct Driver {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    codec: ICodecAPI,
    activation: IMFActivate,
    input_id: u32,
    output_id: u32,
    input_credit: u32,
    settings: Settings,
    bgra: Option<YUVBuffer>,
    nv12: Vec<u8>,
    sequence: Vec<u8>,
    name: String,
    profile: Option<[u128; 4]>,
    _runtime: Runtime,
}
impl Driver {
    pub(super) fn open(settings: Settings) -> Result<Self, String> {
        let runtime = Runtime::start()?;
        let activations = Activations::hardware()?;
        let mut failures = Vec::new();
        for activation in activations.entries().iter().flatten() {
            let name = friendly(activation);
            match unsafe { Self::activate(settings, activation) } {
                Ok((transform, events, codec, input_id, output_id)) => {
                    return Ok(Self {
                        transform,
                        events,
                        codec,
                        activation: activation.clone(),
                        input_id,
                        output_id,
                        input_credit: 0,
                        settings,
                        bgra: None,
                        nv12: Vec::new(),
                        sequence: Vec::new(),
                        name: format!("mf (H.264 hardware, {name})"),
                        profile: std::env::var_os("UU_ENCODE_PROFILE").map(|_| [0; 4]),
                        _runtime: runtime,
                    });
                }
                Err(e) => {
                    failures.push(format!("{name}: {e}"));
                    unsafe {
                        let _ = activation.ShutdownObject();
                    }
                }
            }
        }
        Err(format!("no usable Windows hardware H.264 MFT; install/enable the Intel/AMD/NVIDIA graphics driver: {}",
            if failures.is_empty(){"no hardware encoder was registered".into()}else{failures.join("; ")}))
    }
    unsafe fn activate(
        settings: Settings,
        activation: &IMFActivate,
    ) -> Result<(IMFTransform, IMFMediaEventGenerator, ICodecAPI, u32, u32), String> {
        unsafe {
            let transform: IMFTransform = activation.ActivateObject().map_err(reason)?;
            let attributes = transform.GetAttributes().map_err(reason)?;
            if attributes.GetUINT32(&MF_TRANSFORM_ASYNC).map_err(reason)? == 0 {
                return Err("hardware MFT is not asynchronous".into());
            }
            attributes
                .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
                .map_err(reason)?;
            let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
            let codec: ICodecAPI = transform.cast().map_err(reason)?;
            // Drivers differ in optional tuning support. Actual one-frame completion
            // and IDR recovery are checked below, rather than trusting these flags.
            for (id, value) in [
                (CODECAPI_AVLowLatencyMode, VARIANT::from(true)),
                (CODECAPI_AVEncCommonRealTime, VARIANT::from(true)),
                // Interactive desktop sharing prioritizes completion time over
                // the driver's offline/high-quality encoding preset.
                (CODECAPI_AVEncCommonQualityVsSpeed, VARIANT::from(0u32)),
                (
                    CODECAPI_AVScenarioInfo,
                    VARIANT::from(eAVScenarioInfo_DisplayRemoting.0 as u32),
                ),
                (CODECAPI_AVEncMPVDefaultBPictureCount, VARIANT::from(0u32)),
                (
                    CODECAPI_AVEncCommonRateControlMode,
                    VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32),
                ),
                (CODECAPI_AVEncMPVGOPSize, VARIANT::from(settings.fps * 2)),
                (
                    CODECAPI_AVEncCommonMeanBitRate,
                    VARIANT::from(settings.bitrate_kbps * 1000),
                ),
            ] {
                if let Err(error) = codec.SetValue(&id, &value) {
                    tracing::debug!(?id,%error,"hardware MFT optional tuning unsupported");
                }
            }
            let mut input = [0u32];
            let mut output = [0u32];
            match transform.GetStreamIDs(&mut input, &mut output) {
                Ok(()) => {}
                Err(e) if e.code() == E_NOTIMPL => {}
                Err(e) => return Err(reason(e)),
            }
            let out = video_type(settings, MFVideoFormat_H264)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, settings.bitrate_kbps * 1000)
                .map_err(reason)?;
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32)
                .map_err(reason)?;
            transform
                .SetOutputType(output[0], &out, 0)
                .map_err(|e| format!("H.264 output type: {e}"))?;
            let input_type = video_type(settings, MFVideoFormat_NV12)?;
            input_type
                .SetUINT32(&MF_MT_DEFAULT_STRIDE, settings.width)
                .map_err(reason)?;
            input_type
                .SetUINT32(&MF_MT_SAMPLE_SIZE, settings.width * settings.height * 3 / 2)
                .map_err(reason)?;
            transform
                .SetInputType(input[0], &input_type, 0)
                .map_err(|e| format!("NV12 input type: {e}"))?;
            let events = transform.cast().map_err(reason)?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(reason)?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(reason)?;
            Ok((transform, events, codec, input[0], output[0]))
        }
    }
    fn next_event(&self) -> Result<u32, String> {
        unsafe {
            // This is a dedicated, bounded-request worker. Blocking on the driver's
            // native event avoids polling and adding a timer quantum per frame.
            let event = self
                .events
                .GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0))
                .map_err(reason)?;
            event.GetStatus().map_err(reason)?.ok().map_err(reason)?;
            event.GetType().map_err(reason)
        }
    }
    fn encode(&mut self, frame: &RawFrame, keyframe: bool) -> Result<Vec<EncodedFrame>, String> {
        unsafe {
            let started = std::time::Instant::now();
            if (frame.width, frame.height) != (self.settings.width, self.settings.height) {
                return Err("MFT input dimensions changed".into());
            }
            let pixels = frame.width as usize * frame.height as usize;
            let packed = if prepare_bgra(&mut self.bgra, frame)? {
                None
            } else {
                Some(crate::convert::to_i420(frame).map_err(reason)?)
            };
            let (planes, full) = if let Some(data) = packed.as_ref() {
                (
                    [
                        &data[..pixels],
                        &data[pixels..pixels + pixels / 4],
                        &data[pixels + pixels / 4..],
                    ],
                    true,
                )
            } else {
                let yuv = self.bgra.as_ref().ok_or("missing BGRA conversion")?;
                ([yuv.y(), yuv.u(), yuv.v()], false)
            };
            self.nv12.resize(pixels * 3 / 2, 0);
            upload_planes(
                planes,
                full,
                frame.width as usize,
                frame.height as usize,
                (frame.width, frame.height),
                [frame.width, frame.width, 0],
                [0, pixels as u32, 0],
                &mut self.nv12,
            )?;
            let converted = started.elapsed();
            if keyframe {
                self.codec
                    .SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32))
                    .map_err(|e| format!("hardware IDR request: {e}"))?;
            }
            while self.input_credit == 0 {
                let event = self.next_event()?;
                if event == METransformNeedInput.0 as u32 {
                    self.input_credit += 1;
                } else if event == METransformHaveOutput.0 as u32 {
                    return Err("MFT retained unexpected old output".into());
                }
            }
            let sample = input_sample(&self.nv12, frame.pts.0, self.settings.fps)?;
            self.transform
                .ProcessInput(self.input_id, &sample, 0)
                .map_err(|e| format!("hardware ProcessInput: {e}"))?;
            self.input_credit -= 1;
            let submitted = started.elapsed();
            loop {
                let event = self.next_event()?;
                if event == METransformNeedInput.0 as u32 {
                    self.input_credit += 1;
                } else if event == METransformHaveOutput.0 as u32 {
                    if let Some(coded) = self.output(frame, keyframe)? {
                        if let Some(profile) = &mut self.profile {
                            profile[0] += 1;
                            profile[1] += converted.as_nanos();
                            profile[2] += (submitted - converted).as_nanos();
                            profile[3] += (started.elapsed() - submitted).as_nanos();
                        }
                        return Ok(vec![coded]);
                    }
                }
            }
        }
    }
    unsafe fn output(
        &mut self,
        frame: &RawFrame,
        keyframe: bool,
    ) -> Result<Option<EncodedFrame>, String> {
        unsafe {
            let info = self
                .transform
                .GetOutputStreamInfo(self.output_id)
                .map_err(reason)?;
            let sample = if info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 == 0 {
                let sample = MFCreateSample().map_err(reason)?;
                sample
                    .AddBuffer(
                        &MFCreateMemoryBuffer(
                            info.cbSize
                                .max(self.settings.width * self.settings.height * 3 / 2),
                        )
                        .map_err(reason)?,
                    )
                    .map_err(reason)?;
                Some(sample)
            } else {
                None
            };
            let mut requests = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: self.output_id,
                pSample: ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0;
            let result = self.transform.ProcessOutput(0, &mut requests, &mut status);
            let sample = ManuallyDrop::take(&mut requests[0].pSample);
            drop(ManuallyDrop::take(&mut requests[0].pEvents));
            match result {
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    // Negotiate the type containing the driver's new sequence header.
                    let ty = self
                        .transform
                        .GetOutputAvailableType(self.output_id, 0)
                        .map_err(reason)?;
                    if self.profile.is_some() {
                        eprintln!("uu-mf-type-change requested={}x{}@{} available_size={} available_rate={}",
                            self.settings.width,self.settings.height,self.settings.fps,
                            ty.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0),ty.GetUINT64(&MF_MT_FRAME_RATE).unwrap_or(0));
                    }
                    self.transform
                        .SetOutputType(self.output_id, &ty, 0)
                        .map_err(reason)?;
                    // One output call consumes one HaveOutput event, including
                    // a type change. Wait for the next event before calling again.
                    return Ok(None);
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                Err(e) => {
                    return Err(format!(
                        "hardware ProcessOutput (flags={}, size={}, alignment={}): {e}",
                        info.dwFlags, info.cbSize, info.cbAlignment
                    ))
                }
                Ok(()) => {}
            }
            let sample = sample.ok_or("hardware MFT returned no encoded sample")?;
            let pts = sample.GetSampleTime().map_err(reason)?;
            if pts < 0 || pts as u64 != frame.pts.0 * 10 {
                return Err("hardware MFT reordered or changed the current frame timestamp".into());
            }
            let buffer = sample.ConvertToContiguousBuffer().map_err(reason)?;
            let mut pointer = ptr::null_mut();
            let mut size = 0;
            buffer
                .Lock(&mut pointer, None, Some(&mut size))
                .map_err(reason)?;
            let mut data = if size > 0 && !pointer.is_null() {
                std::slice::from_raw_parts(pointer, size as usize).to_vec()
            } else {
                Vec::new()
            };
            buffer.Unlock().map_err(reason)?;
            if data.is_empty() {
                return Err("hardware MFT returned an empty access unit".into());
            }
            let idr = EncodedFrame::detect_h264_keyframe(&data);
            if keyframe && !idr {
                return Err("hardware MFT did not honour the requested recovery IDR".into());
            }
            if idr {
                let ty = self
                    .transform
                    .GetOutputCurrentType(self.output_id)
                    .map_err(reason)?;
                if let Ok(size) = ty.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) {
                    if size > 0 && size <= 65536 {
                        self.sequence.resize(size as usize, 0);
                        ty.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut self.sequence, None)
                            .map_err(reason)?;
                    }
                }
                if crate::bitstream::h264_dimensions(&data).is_none() && !self.sequence.is_empty() {
                    let mut complete = self.sequence.clone();
                    complete.extend_from_slice(&data);
                    data = complete;
                }
                data = super::low_delay::declare_no_reordering(&data)?;
                if crate::bitstream::h264_dimensions(&data) != Some((frame.width, frame.height)) {
                    return Err("hardware recovery IDR has no matching SPS/PPS geometry".into());
                }
                if crate::color::from_h264(&data) != Some(crate::color::YuvColor::Limited601) {
                    return Err(
                        "hardware SPS colour does not match the limited BT.601 NV12 input".into(),
                    );
                }
            }
            Ok(Some(EncodedFrame {
                data: data.into(),
                pts: frame.pts,
                is_keyframe: idr,
            }))
        }
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
        unsafe {
            self.codec
                .SetValue(
                    &CODECAPI_AVEncCommonMeanBitRate,
                    &VARIANT::from(kbps * 1000),
                )
                .map_err(reason)?;
            self.settings.bitrate_kbps = kbps;
            Ok(())
        }
    }
}
impl Drop for Driver {
    fn drop(&mut self) {
        unsafe {
            if let Some(profile) = self.profile.filter(|p| p[0] > 0) {
                let divisor = profile[0] as f64 * 1_000_000.0;
                eprintln!("uu-mf-profile frames={} conversion_ms={:.3} submission_ms={:.3} completion_ms={:.3}",
                    profile[0],profile[1] as f64/divisor,profile[2] as f64/divisor,profile[3] as f64/divisor);
            }
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            let _ = self.activation.ShutdownObject();
        }
    }
}
unsafe fn video_type(settings: Settings, subtype: GUID) -> Result<IMFMediaType, String> {
    unsafe {
        let ty = MFCreateMediaType().map_err(reason)?;
        ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(reason)?;
        ty.SetGUID(&MF_MT_SUBTYPE, &subtype).map_err(reason)?;
        ty.SetUINT64(&MF_MT_FRAME_SIZE, pack(settings.width, settings.height))
            .map_err(reason)?;
        ty.SetUINT64(&MF_MT_FRAME_RATE, pack(settings.fps, 1))
            .map_err(reason)?;
        ty.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))
            .map_err(reason)?;
        for (id, value) in [
            (MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32),
            (MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT601.0 as u32),
            (MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32),
            (MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_SMPTE170M.0 as u32),
            (MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32),
        ] {
            ty.SetUINT32(&id, value).map_err(reason)?;
        }
        Ok(ty)
    }
}
unsafe fn input_sample(data: &[u8], pts: u64, fps: u32) -> Result<IMFSample, String> {
    unsafe {
        let sample = MFCreateSample().map_err(reason)?;
        let buffer = MFCreateMemoryBuffer(data.len() as u32).map_err(reason)?;
        let mut pointer = ptr::null_mut();
        buffer.Lock(&mut pointer, None, None).map_err(reason)?;
        ptr::copy_nonoverlapping(data.as_ptr(), pointer, data.len());
        buffer.Unlock().map_err(reason)?;
        buffer.SetCurrentLength(data.len() as u32).map_err(reason)?;
        sample.AddBuffer(&buffer).map_err(reason)?;
        sample
            .SetSampleTime(pts.checked_mul(10).ok_or("MFT timestamp overflow")? as i64)
            .map_err(reason)?;
        sample
            .SetSampleDuration(10_000_000 / i64::from(fps))
            .map_err(reason)?;
        Ok(sample)
    }
}
