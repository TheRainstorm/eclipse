//! Windows GPU-driver H.264 / HEVC / AV1 MFTs through Media Foundation.
//! Only hardware-registered async transforms are enumerated. NV12 input is
//! uploaded from CPU memory; this is hardware encoding, not GPU zero-copy.
use super::{upload::upload_frame, Settings};
use crate::{EncodedFrame, RawFrame};
use uu_proto::media::VideoCodec;
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
fn subtype(codec:VideoCodec)->GUID {
    match codec {VideoCodec::H264=>MFVideoFormat_H264,VideoCodec::Hevc=>MFVideoFormat_HEVC,VideoCodec::Av1=>MFVideoFormat_AV1}
}
fn codec_label(codec:VideoCodec)->&'static str {
    match codec {VideoCodec::H264=>"H.264",VideoCodec::Hevc=>"HEVC",VideoCodec::Av1=>"AV1"}
}

// Declared last in Driver: the transform is released before COM/MF shut down.
pub(super) struct Runtime {
    timer_requested: bool,
}
impl Runtime {
    pub(super) fn start() -> Result<Self, String> {
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
    fn hardware(codec:VideoCodec) -> Result<Self, String> {
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
                guidSubtype: subtype(codec),
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

fn vendor(activation:&IMFActivate) -> Option<u16> {
    let mut text=[0u16;128];
    if unsafe{activation.GetString(&MFT_ENUM_HARDWARE_VENDOR_ID_Attribute,&mut text,None).is_ok()} {
        let id=String::from_utf16_lossy(&text).to_ascii_lowercase();
        return [0x8086,0x10de,0x1002].into_iter().find(|id_value|id.contains(&format!("{id_value:04x}")));
    }
    let name=friendly(activation).to_ascii_lowercase();
    if name.contains("intel"){Some(0x8086)}else if name.contains("nvidia"){Some(0x10de)}
    else if name.contains("amd")||name.contains("advanced micro"){Some(0x1002)}else{None}
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
    sequence: Vec<u8>,
    headers: super::headers::Headers,
    name: String,
    profile: Option<[u128; 4]>,
    gpu:Option<super::gpu::Input>,
    gpu_capable:bool,
    gpu_frames:u64,
    _runtime: Runtime,
}
impl Driver {
    pub(super) fn open(settings: Settings) -> Result<Self, String> {
        let runtime = Runtime::start()?;
        let activations = Activations::hardware(settings.codec)?;
        let mut failures = Vec::new();
        for activation in activations.entries().iter().flatten() {
            let name = friendly(activation);
            if settings.preference.vendor_id().is_some_and(|id|vendor(activation)!=Some(id)) {
                continue;
            }
            match unsafe { Self::activate(settings, activation,None) } {
                Ok((transform, events, codec, input_id, output_id)) => {
                    let gpu_capable=unsafe{transform.GetAttributes().ok().and_then(|a|a.GetUINT32(&MF_SA_D3D11_AWARE).ok())==Some(1)};
                    return Ok(Self {
                        transform,
                        events,
                        codec,
                        activation: activation.clone(),
                        input_id,
                        output_id,
                        input_credit: 0,
                        settings,
                        sequence: Vec::new(),
                        headers: Default::default(),
                        name: format!(
                            "mf ({} {}, {name}{})",codec_label(settings.codec),
                            if matches!(settings.rate_control, super::RateControl::Vbr { .. }) {
                                "VBR"
                            } else {
                                "CBR"
                            },
                            if vendor(activation)==Some(0x10de) {settings.tuning.nvidia_quality_vs_speed.map(|v|format!(", quality/speed={v}")).unwrap_or_default()}else{String::new()}
                        ),
                        profile: std::env::var_os("UU_ENCODE_PROFILE").map(|_| [0; 4]),
                        gpu:None,gpu_capable,gpu_frames:0,
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
        Err(format!("no usable Windows hardware {} MFT; install/enable the Intel/AMD/NVIDIA graphics driver: {}",codec_label(settings.codec),
            if failures.is_empty(){"no hardware encoder was registered".into()}else{failures.join("; ")}))
    }
    unsafe fn activate(
        settings: Settings,
        activation: &IMFActivate,
        manager:Option<&IMFDXGIDeviceManager>,
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
            if let Some(manager)=manager {
                transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER,manager.as_raw() as usize).map_err(reason)?;
            }
            let codec: ICodecAPI = transform.cast().map_err(reason)?;
            let vendor=vendor(activation);
            if vendor==Some(0x1002) && settings.tuning.amd_quality!=super::AmdQuality::Speed {
                return Err("自定义 AMD 画质预设需要原生 AMF，不能通过 MFT 忽略该设置".into());
            }
            if vendor==Some(0x8086) && settings.tuning.custom_intel() {
                return Err("自定义 Intel 参数需要 oneVPL 后端".into());
            }
            let quality=(vendor==Some(0x10de)).then_some(settings.tuning.nvidia_quality_vs_speed).flatten();
            // Drivers differ in optional tuning support. Actual one-frame completion
            // and IDR recovery are checked below, rather than trusting these flags.
            for (id, value) in [
                (CODECAPI_AVLowLatencyMode, VARIANT::from(true)),
                (CODECAPI_AVEncCommonRealTime, VARIANT::from(true)),
                (
                    CODECAPI_AVScenarioInfo,
                    VARIANT::from(eAVScenarioInfo_DisplayRemoting.0 as u32),
                ),
                (CODECAPI_AVEncMPVDefaultBPictureCount, VARIANT::from(0u32)),
                (CODECAPI_AVEncMPVGOPSize, VARIANT::from(settings.fps * 2)),
            ] {
                if let Err(error) = codec.SetValue(&id, &value) {
                    tracing::debug!(?id,%error,"hardware MFT optional tuning unsupported");
                }
            }
            let quality_value=VARIANT::from(u32::from(quality.unwrap_or(0)));
            let quality_result=codec.SetValue(&CODECAPI_AVEncCommonQualityVsSpeed,&quality_value);
            if quality.is_some() {
                quality_result.map_err(|error|format!("NVIDIA 驱动不支持自定义画质/速度：{error}"))?;
            }
            let (rate_mode, peak_kbps) = match settings.rate_control {
                super::RateControl::Cbr => (eAVEncCommonRateControlMode_CBR, None),
                super::RateControl::Vbr { peak_kbps } => (
                    eAVEncCommonRateControlMode_PeakConstrainedVBR,
                    Some(peak_kbps),
                ),
            };
            codec
                .SetValue(
                    &CODECAPI_AVEncCommonRateControlMode,
                    &VARIANT::from(rate_mode.0 as u32),
                )
                .map_err(|error| format!("hardware rate-control mode: {error}"))?;
            codec
                .SetValue(
                    &CODECAPI_AVEncCommonMeanBitRate,
                    &VARIANT::from(settings.bitrate_kbps * 1000),
                )
                .map_err(|error| format!("hardware mean bitrate: {error}"))?;
            if let Some(peak_kbps) = peak_kbps {
                codec
                    .SetValue(
                        &CODECAPI_AVEncCommonMaxBitRate,
                        &VARIANT::from(peak_kbps * 1000),
                    )
                    .map_err(|error| format!("hardware peak bitrate: {error}"))?;
            }
            let mut input = [0u32];
            let mut output = [0u32];
            match transform.GetStreamIDs(&mut input, &mut output) {
                Ok(()) => {}
                Err(e) if e.code() == E_NOTIMPL => {}
                Err(e) => return Err(reason(e)),
            }
            let out = video_type(settings, subtype(settings.codec))?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, settings.bitrate_kbps * 1000)
                .map_err(reason)?;
            let profile=match settings.codec {
                VideoCodec::H264=>eAVEncH264VProfile_Base.0,
                VideoCodec::Hevc=>eAVEncH265VProfile_Main_420_8.0,
                VideoCodec::Av1=>eAVEncAV1VProfile_Main_420_8.0,
            };
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, profile as u32)
                .map_err(reason)?;
            transform
                .SetOutputType(output[0], &out, 0)
                .map_err(|e| format!("{} output type: {e}",codec_label(settings.codec)))?;
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
            if let Some(requested)=quality {
                let actual=codec.GetValue(&CODECAPI_AVEncCommonQualityVsSpeed).map_err(reason)?;
                let actual=windows::Win32::System::Variant::VariantToUInt32(&actual).map_err(reason)?;
                if actual!=u32::from(requested) {return Err(format!("NVIDIA 驱动将画质/速度 {requested} 改为 {actual}，请选择支持的值"));}
            }
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
            // A fresh MF-owned sample keeps its storage alive for the
            // asynchronous transform. Never recycle a buffer on input credit:
            // the transform may still retain its sample reference.
            let sample = if frame.gpu {
                if !self.gpu_capable {return Err("hardware MFT has no D3D11 input support".into());}
                if self.gpu.is_none() {
                    let gpu=super::gpu::Input::new(frame,self.settings)?;
                    // Bind the capture device before negotiating input types.
                    // No CPU samples have been submitted on this GPU session.
                    self.activation.ShutdownObject().map_err(reason)?;
                    let (transform,events,codec,input_id,output_id)=Self::activate(self.settings,&self.activation,Some(&gpu.manager))?;
                    self.transform=transform;self.events=events;self.codec=codec;
                    self.input_id=input_id;self.output_id=output_id;self.input_credit=0;
                    self.gpu=Some(gpu);
                }
                self.gpu_frames+=1;
                self.gpu.as_ref().unwrap().sample(frame)?
            } else {input_sample(frame, self.settings.fps)?};
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
            let codec=self.settings.codec;
            let has_sequence=|bytes:&[u8]|match codec {
                VideoCodec::H264=>crate::bitstream::h264_dimensions(bytes).is_some(),
                VideoCodec::Hevc=>crate::hevc::sequence(bytes).is_some(),
                VideoCodec::Av1=>crate::av1::sequence(bytes).is_some(),
            };
            let clean=sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0)!=0;
            if (keyframe || clean) && !has_sequence(&data) {
                let ty=self.transform.GetOutputCurrentType(self.output_id).map_err(reason)?;
                if let Ok(size)=ty.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) {
                    if size>0 && size<=65536 {
                        self.sequence.resize(size as usize,0);
                        ty.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER,&mut self.sequence,None).map_err(reason)?;
                    }
                }
                if !self.sequence.is_empty() {
                    let mut complete=self.sequence.clone();complete.extend_from_slice(&data);data=complete;
                }
            }
            let idr=match codec {
                VideoCodec::H264=>EncodedFrame::detect_h264_keyframe(&data),
                VideoCodec::Hevc=>crate::hevc::is_keyframe(&data),
                VideoCodec::Av1=>crate::av1::is_keyframe(&data),
            };
            if keyframe && !idr {return Err("hardware MFT did not honour the requested recovery keyframe".into());}
            let mut encoded=EncodedFrame{data:data.into(),pts:frame.pts,is_keyframe:idr};
            self.headers.prepare(codec,&mut encoded)?;
            if idr {
                if codec==VideoCodec::H264 {encoded.data=super::low_delay::declare_no_reordering(&encoded.data)?.into();}
                let metadata=match codec {
                    VideoCodec::H264=>crate::bitstream::h264_dimensions(&encoded.data).zip(crate::color::from_h264(&encoded.data)),
                    VideoCodec::Hevc=>crate::hevc::sequence(&encoded.data).map(|s|((s.width,s.height),s.color)),
                    VideoCodec::Av1=>crate::av1::sequence(&encoded.data).map(|s|((s.width,s.height),s.color)),
                };
                if metadata!=Some(((frame.width,frame.height),crate::color::YuvColor::Limited601)) {
                    return Err(format!("hardware recovery frame has incompatible geometry/colour: {metadata:?}"));
                }
            }
            Ok(Some(encoded))
        }
    }
}
impl super::worker::Driver for Driver {
    fn codec(&self)->VideoCodec {self.settings.codec}
    fn gpu_input(&self)->bool {self.gpu_capable}
    fn backend_name(&self) -> &str {
        &self.name
    }
    fn encode(&mut self, frame: &RawFrame, keyframe: bool) -> Result<Vec<EncodedFrame>, String> {
        Driver::encode(self, frame, keyframe)
    }
    fn bitrate(&mut self, kbps: u32) -> Result<(), String> {
        if let super::RateControl::Vbr { peak_kbps } = self.settings.rate_control {
            if kbps > peak_kbps {
                return Err(format!(
                    "requested bitrate {kbps} kbps exceeds configured VBR peak {peak_kbps} kbps"
                ));
            }
        }
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
                eprintln!("uu-mf-input gpu_frames={} cpu_frames={}",self.gpu_frames,profile[0].saturating_sub(self.gpu_frames as u128));
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
// Unlock on every conversion/validation error before releasing the buffer.
struct LockedInput<'a>(Option<&'a IMFMediaBuffer>);
impl LockedInput<'_> {
    unsafe fn finish(mut self) -> Result<(), String> {
        self.0
            .take()
            .ok_or("MFT input was already unlocked")?
            .Unlock()
            .map_err(reason)
    }
}
impl Drop for LockedInput<'_> {
    fn drop(&mut self) {
        unsafe {
            if let Some(buffer) = self.0.take() {
                let _ = buffer.Unlock();
            }
        }
    }
}

unsafe fn input_sample(frame: &RawFrame, fps: u32) -> Result<IMFSample, String> {
    unsafe {
        let (w, h, _) = crate::convert::validate_cpu_frame(frame).map_err(reason)?;
        let timestamp = frame
            .pts
            .0
            .checked_mul(10)
            .and_then(|v| i64::try_from(v).ok())
            .ok_or("MFT timestamp overflow")?;
        if fps == 0 {
            return Err("MFT frame rate must be positive".into());
        }
        let length = w * h * 3 / 2;
        let sample = MFCreateSample().map_err(reason)?;
        let buffer = MFCreateMemoryBuffer(length as u32).map_err(reason)?;
        let mut pointer = ptr::null_mut();
        let mut capacity = 0;
        buffer
            .Lock(&mut pointer, Some(&mut capacity), None)
            .map_err(reason)?;
        {
            let locked = LockedInput(Some(&buffer));
            if pointer.is_null() || (capacity as usize) < length {
                return Err("MFT input buffer is shorter than the NV12 frame".into());
            }
            let target = std::slice::from_raw_parts_mut(pointer, length);
            upload_frame(
                frame,
                (frame.width, frame.height),
                [frame.width, frame.width, 0],
                [0, (w * h) as u32, 0],
                target,
            )?;
            locked.finish()?;
        }
        buffer.SetCurrentLength(length as u32).map_err(reason)?;
        sample.AddBuffer(&buffer).map_err(reason)?;
        sample.SetSampleTime(timestamp).map_err(reason)?;
        sample
            .SetSampleDuration(10_000_000 / i64::from(fps))
            .map_err(reason)?;
        Ok(sample)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PixelFormat, Timestamp};

    unsafe fn sample_bytes(sample: &IMFSample) -> Vec<u8> {
        let buffer = sample.GetBufferByIndex(0).unwrap();
        let mut pointer = ptr::null_mut();
        let mut length = 0;
        buffer.Lock(&mut pointer, None, Some(&mut length)).unwrap();
        let _locked = LockedInput(Some(&buffer));
        std::slice::from_raw_parts(pointer, length as usize).to_vec()
    }

    #[test]
    fn owned_mf_samples_convert_padded_inputs_and_keep_independent_storage() {
        let _runtime = Runtime::start().unwrap();
        for format in [PixelFormat::Bgra8, PixelFormat::Nv12, PixelFormat::I420] {
            let (w, h) = (34u32, 4u32);
            let stride = w * if format == PixelFormat::Bgra8 { 4 } else { 1 } + 7;
            let mut data = vec![
                255;
                match format {
                    PixelFormat::Bgra8 => (stride * h) as usize,
                    PixelFormat::Nv12 => (stride * h * 3 / 2) as usize,
                    PixelFormat::I420 => (stride * h + w * h / 2) as usize,
                }
            ];
            if format != PixelFormat::Bgra8 {
                data[(stride * h) as usize..].fill(128);
            }
            let mut frame = RawFrame {
                width: w,
                height: h,
                stride,
                format,
                data: data.into(),
                pts: Timestamp(123456),
                gpu: false,
                dma_buf: None,
                d3d11: None,
            };
            unsafe {
                let first = input_sample(&frame, 60).unwrap();
                assert_eq!(first.GetSampleTime().unwrap(), 1_234_560);
                assert_eq!(first.GetSampleDuration().unwrap(), 10_000_000 / 60);
                let before = sample_bytes(&first);
                assert_eq!(&before[..(w * h) as usize], &vec![235; (w * h) as usize]);
                assert_eq!(
                    &before[(w * h) as usize..],
                    &vec![128; (w * h / 2) as usize]
                );
                let mut next = frame.data.to_vec();
                next[..(stride * h) as usize].fill(0);
                frame.data = next.into();
                let second = input_sample(&frame, 60).unwrap();
                assert_eq!(
                    &sample_bytes(&second)[..(w * h) as usize],
                    &vec![16; (w * h) as usize]
                );
                assert_eq!(sample_bytes(&first), before);
                frame.pts = Timestamp(u64::MAX);
                assert!(input_sample(&frame, 60).is_err());
                frame.pts = Timestamp(0);
                assert!(input_sample(&frame, 0).is_err());
                frame.data = bytes::Bytes::from_static(&[0; 4]);
                assert!(input_sample(&frame, 60).is_err());
            }
        }
    }
}
