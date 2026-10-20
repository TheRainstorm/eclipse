//! Windows Media Foundation H.264 decoding on a D3D11 device manager.
//!
//! This is the "D3D11VA" path of the system video stack: a D3D11 device is
//! handed to the H.264 decoder MFT through `IMFDXGIDeviceManager`, so entropy
//! decoding, motion compensation and in-loop filtering run on the GPU and the
//! decoded NV12 surfaces never leave video memory.
//!
//! The controller still needs pixels in system memory (the GUI uploads an
//! RGBA texture through glow), so every frame is copied into a staging texture
//! and mapped. That readback costs a fraction of what a software decode spends
//! at the 55 Mbit/s preset the streamer runs, and it keeps the media plane
//! unchanged: hardware frames arrive as [`PixelFormat::Nv12`] with a row
//! pitch, exactly like VA-API frames on Linux.
//!
//! Everything runs on the decoder's own thread (see `hw::Worker`) because
//! Media Foundation wants a COM apartment and D3D11 wants a single driver
//! thread; the async decode loop we are plugged into promises neither.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::ptr;

use bytes::Bytes;
use windows::core::Interface;
use windows::Win32::Foundation::{E_NOTIMPL, HMODULE, RPC_E_CHANGED_MODE};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1,
    D3D_FEATURE_LEVEL_11_0,
};
use windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_DECODER,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIDevice};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED,
};

use windows::Win32::System::Variant::VARIANT;

use crate::bitstream::{h264_dimensions, patch_sps_allow_gaps};
use crate::codec::{CodecError, VideoDecoder};
use crate::frame::{EncodedFrame, PixelFormat, RawFrame};
use crate::Timestamp;
use uu_proto::media::VideoCodec;

fn describe<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Public handle: a `Worker` thread owning all Media Foundation state.
pub struct D3d11VaDecoder {
    worker: super::Worker,
}

impl D3d11VaDecoder {
    pub fn new() -> Result<Self, CodecError> {
        Ok(Self {
            worker: super::Worker::spawn(Decoder::open)?,
        })
    }
}

impl VideoDecoder for D3d11VaDecoder {
    fn codec(&self) -> VideoCodec {
        VideoCodec::H264
    }

    fn backend_name(&self) -> &str {
        self.worker.backend_name()
    }

    fn prepare(&mut self, width: u32, height: u32) -> Result<(), CodecError> {
        self.worker.prepare(width, height)
    }

    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<RawFrame>, CodecError> {
        self.worker.decode(frame)
    }
}

/// Media Foundation and COM are brought up together on the decoder thread and
/// must go down in the opposite order, so this is the last field of [`Decoder`].
struct Runtime {
    owns_apartment: bool,
}

impl Runtime {
    fn start() -> Result<Self, String> {
        unsafe {
            let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            if hr.is_err() && hr != RPC_E_CHANGED_MODE {
                return Err(format!("CoInitializeEx: {hr:?}"));
            }
            if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                // Only undo an apartment this call actually took.
                if hr.is_ok() {
                    CoUninitialize();
                }
                return Err(format!("MFStartup: {e}"));
            }
            Ok(Self {
                owns_apartment: hr.is_ok(),
            })
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            if self.owns_apartment {
                CoUninitialize();
            }
        }
    }
}

/// The decoder. Field order matters: the transform — which holds the only
/// long-lived reference into the device manager — drops before the device
/// manager and the D3D11 device.
struct Decoder {
    pending: VecDeque<RawFrame>,
    active: Option<Transform>,
    name: String,
    context: ID3D11DeviceContext,
    device: ID3D11Device,
    manager: IMFDXGIDeviceManager,
    /// Held only for its `Drop`: Media Foundation must be shut down after the
    /// transform, the device manager and the device are gone.
    #[allow(dead_code)]
    runtime: Runtime,
}

impl Decoder {
    fn prepare_geometry(&mut self, width: u32, height: u32) -> Result<(), String> {
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return Err("decoder geometry is outside the supported bounds".into());
        }
        if self.active.as_ref().map_or(true, |active| active.width != width || active.height != height) {
            self.active = Some(self.open_transform(width, height)?);
            self.pending.clear();
        }
        Ok(())
    }

    fn open() -> Result<Self, String> {
        let runtime = Runtime::start()?;
        let (device, context) = create_device()?;
        let manager = device_manager(&device)?;
        let name = match adapter_name(&device) {
            Some(adapter) => format!("d3d11va (Media Foundation hw: {adapter})"),
            None => "d3d11va (Media Foundation hw)".to_string(),
        };
        Ok(Self {
            pending: VecDeque::new(),
            active: None,
            name,
            context,
            device,
            manager,
            runtime,
        })
    }

    /// Negotiate a transform for `width` x `height`.
    fn open_transform(&self, width: u32, height: u32) -> Result<Transform, String> {
        let mut last = "no H.264 decoder MFT available".to_string();
        // Vendor decoders first (those are what the HARDWARE flag selects),
        // then the in-box Microsoft decoder MFT, which is D3D11-aware and is
        // what most machines actually use for DXVA decode.
        for flags in [
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
        ] {
            let candidates = Activations::enumerate(flags)?;
            for activation in candidates.entries().iter().flatten() {
                let attempt = Transform::create(
                    activation,
                    &self.manager,
                    &self.device,
                    &self.context,
                    width,
                    height,
                );
                match attempt {
                    Ok(transform) => return Ok(transform),
                    Err(e) => last = e,
                }
            }
        }
        Err(last)
    }
}

impl super::Backend for Decoder {
    fn backend_name(&self) -> &str {
        &self.name
    }

    fn prepare(&mut self, width: u32, height: u32) -> Result<(), String> {
        self.prepare_geometry(width, height)
    }

    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<RawFrame>, String> {
        // Always submit the new access unit, even if earlier output is queued:
        // returning a queued picture first would silently drop this input.
        // Parameter sets travel inside the IDR access units, so the first
        // keyframe after joining tells us the geometry. A different geometry
        // means the host switched resolution and the transform must be rebuilt.
        if let Some((width, height)) = h264_dimensions(&frame.data) {
            self.prepare_geometry(width, height)?;
        }

        let Decoder {
            active, pending, ..
        } = self;
        let Some(transform) = active.as_mut() else {
            // No SPS yet: we joined mid-GOP. The controller's PLI/FIR loop
            // asks the host for an IDR, so this resolves within a few hundred
            // milliseconds — and `FallbackDecoder` gives up if it does not.
            return Ok(None);
        };

        let patched = patch_sps_allow_gaps(&frame.data);
        let data = patched.as_deref().unwrap_or(&frame.data);
        transform.submit(data, frame.is_keyframe, frame.pts, pending)?;
        Ok(pending.pop_front())
    }
}

/// One negotiated H.264 decoder MFT plus the staging texture it reads back to.
struct Transform {
    transform: IMFTransform,
    /// Keeps the activation alive so the MFT is shut down the way it was
    /// created (Media Foundation expects `ShutdownObject` for pool cleanup).
    activation: IMFActivate,
    /// Clones of the decoder's device and immediate context: the staging copy
    /// is issued here, on the decoder thread.
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    input_id: u32,
    output_id: u32,
    width: u32,
    height: u32,
    /// True when the MFT hands us its own (D3D11) samples, which is the normal
    /// case for a D3D11-aware decoder.
    provides_samples: bool,
    output_size: u32,
    staging: Option<Staging>,
    /// Surfaces decoded so far, and whether any of them was a D3D11 texture.
    /// A decoder that accepted the device manager but keeps handing back
    /// system memory is decoding in software, which the software decoder does
    /// better, so `read_surface` gives up on it.
    frames_read: u64,
    saw_d3d11_surface: bool,
    profile: DecodeProfile,
    pixels: super::pixels::Pool,
}

#[derive(Default)]
struct DecodeProfile {
    enabled: bool,
    calls: u64,
    total_us: u64,
    map_us: u64,
    cpu_copy_us: u64,
    surfaces: u64,
}

struct Staging {
    texture: ID3D11Texture2D,
    width: u32,
    height: u32,
}

impl Transform {
    fn create(
        activation: &IMFActivate,
        manager: &IMFDXGIDeviceManager,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        unsafe {
            let transform: IMFTransform = activation
                .ActivateObject()
                .map_err(|e| format!("ActivateObject: {e}"))?;
            // Asynchronous MFTs signal output through events; we drive decode
            // one access unit at a time, so they are not usable here. The flag
            // lives on the activation object, and older decoders (including
            // the in-box Microsoft one) do not implement
            // `IMFTransform::GetAttributes` at all, so both are consulted.
            let attributes = transform.GetAttributes().ok();
            let asynchronous = activation
                .GetUINT32(&MF_TRANSFORM_ASYNC)
                .map(|flag| flag != 0)
                .or_else(|_| {
                    attributes
                        .as_ref()
                        .ok_or(())
                        .and_then(|a| a.GetUINT32(&MF_TRANSFORM_ASYNC).map_err(|_| ()))
                        .map(|flag| flag != 0)
                })
                .unwrap_or(false);
            if asynchronous {
                return Err("asynchronous MFT (event-driven decode not implemented)".into());
            }
            // A decoder that advertises no D3D11 support cannot be switched to
            // the GPU, so there is nothing to gain over OpenH264.
            if let Some(attributes) = &attributes {
                if attributes.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
                    return Err("MFT is not D3D11 aware".into());
                }
                let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
                let _ = attributes.SetUINT32(&MF_SA_D3D11_BINDFLAGS, D3D11_BIND_DECODER.0 as u32);
            }
            // The in-box H.264 MFT may return E_NOTIMPL from GetAttributes.
            // MF_LOW_LATENCY on an optional IMFAttributes cannot configure it.
            // Microsoft's H.264 decoder specifically requires VT_UI4 here;
            // other codecs may use VT_BOOL. This is a best-effort latency hint,
            // never a reason to discard otherwise working hardware decode.
            let codec_low_latency = transform.cast::<ICodecAPI>().ok().is_some_and(|codec| {
                codec.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(1u32)).is_ok()
                    || codec.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true)).is_ok()
            });
            if std::env::var_os("UU_DECODE_PROFILE").is_some() {
                eprintln!("d3d11 low_latency_codec_api={codec_low_latency}");
            }
            // Whether or not the decoder advertises awareness, handing it a
            // device manager is the switch that turns on GPU decode; a decoder
            // that cannot use it fails here and is skipped.
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                .map_err(|e| format!("MFT_MESSAGE_SET_D3D_MANAGER: {e}"))?;

            let (input_id, output_id) = stream_ids(&transform)?;
            set_input_type(&transform, input_id, width, height)?;
            if set_output_type(&transform, output_id, width, height).is_err() {
                select_nv12_output(&transform, output_id)?;
            }
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(|e| format!("NOTIFY_BEGIN_STREAMING: {e}"))?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(|e| format!("NOTIFY_START_OF_STREAM: {e}"))?;
            let info = transform
                .GetOutputStreamInfo(output_id)
                .map_err(|e| format!("GetOutputStreamInfo: {e}"))?;
            Ok(Self {
                transform,
                activation: activation.clone(),
                device: device.clone(),
                context: context.clone(),
                input_id,
                output_id,
                width,
                height,
                provides_samples: info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0,
                output_size: info.cbSize,
                staging: None,
                frames_read: 0,
                saw_d3d11_surface: false,
                pixels: super::pixels::Pool::default(),
                profile: DecodeProfile {enabled: std::env::var("UU_DECODE_PROFILE").is_ok_and(|v|v!="0"), ..Default::default()},
            })
        }
    }

    /// Feed one access unit and drain whatever frames it produced.
    fn submit(
        &mut self,
        data: &[u8],
        keyframe: bool,
        pts: Timestamp,
        out: &mut VecDeque<RawFrame>,
    ) -> Result<(), String> {
        // Preserve the transport clock, including skipped frames and decoder
        // restarts. A synthetic 60 Hz counter drifts on 30 Hz input and resets
        // to zero after loss, making a new IDR look older than its generation.
        let profile_started = self.profile.enabled.then(std::time::Instant::now);
        let time_100ns = pts.as_micros().saturating_mul(10).min(i64::MAX as u64) as i64;
        let sample = input_sample(data, time_100ns, keyframe)?;
        unsafe {
            match self.transform.ProcessInput(self.input_id, &sample, 0) {
                Err(e) if e.code() == MF_E_NOTACCEPTING => {
                    // The decoder wants its output collected before it takes
                    // more input.
                    self.drain(out)?;
                    self.transform
                        .ProcessInput(self.input_id, &sample, 0)
                        .map_err(|e| format!("ProcessInput: {e}"))?;
                }
                result => result.map_err(|e| format!("ProcessInput: {e}"))?,
            }
        }
        let result = self.drain(out);
        if let Some(started) = profile_started {
            self.profile.calls += 1;
            self.profile.total_us += started.elapsed().as_micros() as u64;
            if self.profile.calls >= 120 {
                let p=&self.profile;let n=p.calls.max(1);let frames=p.surfaces.max(1);
                eprintln!("d3d11 decode profile calls={} surfaces={} total_ms={:.3} readback_ms={:.3} cpu_copy_ms={:.3} mft_other_ms={:.3}",p.calls,p.surfaces,p.total_us as f64/n as f64/1000.,p.map_us as f64/frames as f64/1000.,p.cpu_copy_us as f64/frames as f64/1000.,p.total_us.saturating_sub(p.map_us+p.cpu_copy_us) as f64/n as f64/1000.);
                self.profile=DecodeProfile {enabled:true,..Default::default()};
            }
        }
        result
    }

    /// Pull decoded frames until the MFT runs dry.
    fn drain(&mut self, out: &mut VecDeque<RawFrame>) -> Result<(), String> {
        loop {
            let mut request = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: self.output_id,
                pSample: ManuallyDrop::new(None),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            };
            // A decoder that does not supply its own samples needs one to
            // write into (system-memory NV12 in that case).
            if !self.provides_samples {
                let sample = empty_sample(self.output_size)?;
                request.pSample = ManuallyDrop::new(Some(sample));
            }
            let mut requests = [request];
            let mut status = 0u32;
            let result = unsafe { self.transform.ProcessOutput(0, &mut requests, &mut status) };
            // Anything the MFT allocated for us is ours to release.
            unsafe {
                if let Some(events) = ManuallyDrop::take(&mut requests[0].pEvents) {
                    drop(events);
                }
            }
            // pSample is ManuallyDrop in the bindings. Take ownership even
            // on NEED_MORE_INPUT / STREAM_CHANGE so error paths do not leak.
            let sample = unsafe { ManuallyDrop::take(&mut requests[0].pSample) };
            match result {
                Ok(()) => {
                    let Some(sample) = sample else {
                        return Ok(());
                    };
                    out.push_back(self.read_surface(&sample)?);
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.renegotiate()?,
                Err(e) => return Err(format!("ProcessOutput: {e}")),
            }
        }
    }

    /// The decoder re-announced its output type (resolution or format change):
    /// re-negotiate NV12 and drop the now-wrong staging texture.
    fn renegotiate(&mut self) -> Result<(), String> {
        let (width, height) = (self.width, self.height);
        if set_output_type(&self.transform, self.output_id, width, height).is_err() {
            select_nv12_output(&self.transform, self.output_id)?;
        }
        let info = unsafe { self.transform.GetOutputStreamInfo(self.output_id) }
            .map_err(|e| format!("GetOutputStreamInfo: {e}"))?;
        self.provides_samples = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
        self.output_size = info.cbSize;
        self.staging = None;
        Ok(())
    }

    /// Read one decoded surface back into system memory.
    ///
    /// Hardware decoders hand back either an `IMFDXGIBuffer` (the GPU texture
    /// we asked for) or, when the driver decided otherwise, a system-memory
    /// buffer. Both are handled, so a driver quirk costs a copy rather than a
    /// stalled session.
    fn read_surface(&mut self, sample: &IMFSample) -> Result<RawFrame, String> {
        let pts = unsafe { sample.GetSampleTime() }.unwrap_or(0).max(0) as u64 / 10;
        let buffer =
            unsafe { sample.GetBufferByIndex(0) }.map_err(|e| format!("GetBufferByIndex: {e}"))?;
        self.frames_read += 1;

        if let Ok(dxgi) = buffer.cast::<IMFDXGIBuffer>() {
            self.saw_d3d11_surface = true;
            let mut raw: *mut c_void = ptr::null_mut();
            unsafe { dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw) }
                .map_err(|e| format!("IMFDXGIBuffer::GetResource: {e}"))?;
            if raw.is_null() {
                return Err("decoded surface has no D3D11 resource".into());
            }
            // SAFETY: GetResource returned a referenced ID3D11Texture2D.
            let texture = unsafe { ID3D11Texture2D::from_raw(raw) };
            let index = unsafe { dxgi.GetSubresourceIndex() }
                .map_err(|e| format!("GetSubresourceIndex: {e}"))?;
            let (width, height, format) = texture_description(&texture);
            if format != DXGI_FORMAT_NV12 {
                return Err(format!(
                    "decoded surface is not NV12 (DXGI format {})",
                    format.0
                ));
            }
            // A handle so the mutable borrow of `self.staging` ends here.
            let staging = self.staging_texture(width, height)?.clone();
            let map_started=self.profile.enabled.then(std::time::Instant::now);
            unsafe {
                self.context
                    .CopySubresourceRegion(&staging, 0, 0, 0, 0, &texture, index, None);
            }
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            unsafe {
                self.context
                    .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            }
            .map_err(|e| format!("staging Map: {e}"))?;
            if let Some(started)=map_started {self.profile.map_us+=started.elapsed().as_micros() as u64;}
            let copy_started=self.profile.enabled.then(std::time::Instant::now);
            // The texture includes decoder alignment (1080p commonly uses
            // 1088 luma rows). UV starts at the allocated, not visible, height.
            let stride = mapped.RowPitch;
            let len = stride as usize * height as usize * 3 / 2;
            let mut pixels=self.pixels.take(self.width as usize*self.height as usize*3/2);
            unsafe {
                let source = std::slice::from_raw_parts(mapped.pData as *const u8, len);
                let result =
                    super::surface::crop_nv12_into(source, stride, height, self.width, self.height, pixels.as_mut());
                self.context.Unmap(&staging, 0);
                result?;
            };
            if let Some(started)=copy_started {self.profile.cpu_copy_us+=started.elapsed().as_micros() as u64;self.profile.surfaces+=1;}
            return Ok(RawFrame {
                width: self.width,
                height: self.height,
                format: PixelFormat::Nv12,
                stride: self.width,
                data: Bytes::from_owner(pixels),
                pts: Timestamp(pts),
                gpu: false,
                dma_buf: None,
            });
        }

        // System memory, but the MFT may still have padded the rows.
        if let Ok(surface) = buffer.cast::<IMF2DBuffer>() {
            self.reject_software_output()?;
            let mut rows: *mut u8 = ptr::null_mut();
            let mut pitch = 0i32;
            unsafe { surface.Lock2D(&mut rows, &mut pitch) }
                .map_err(|e| format!("IMF2DBuffer::Lock2D: {e}"))?;
            let stride = if pitch > 0 { pitch as u32 } else { self.width };
            let len = stride as usize * self.height as usize * 3 / 2;
            let mut pixels = vec![0u8; len];
            unsafe {
                ptr::copy_nonoverlapping(rows, pixels.as_mut_ptr(), len);
                let _ = surface.Unlock2D();
            }
            return Ok(RawFrame {
                width: self.width,
                height: self.height,
                format: PixelFormat::Nv12,
                stride,
                data: Bytes::from(pixels),
                pts: Timestamp(pts),
                gpu: false,
                dma_buf: None,
            });
        }

        // Contiguous system memory: rows are packed at the frame width.
        self.reject_software_output()?;
        let mut rows: *mut u8 = ptr::null_mut();
        let mut current = 0u32;
        unsafe { buffer.Lock(&mut rows, None, Some(&mut current)) }
            .map_err(|e| format!("IMFMediaBuffer::Lock: {e}"))?;
        let stride = self.width;
        let len = stride as usize * self.height as usize * 3 / 2;
        let len = len.min(current as usize);
        let mut pixels = vec![0u8; len];
        unsafe {
            ptr::copy_nonoverlapping(rows, pixels.as_mut_ptr(), len);
            buffer
                .Unlock()
                .map_err(|e| format!("IMFMediaBuffer::Unlock: {e}"))?;
        }
        Ok(RawFrame {
            width: self.width,
            height: self.height,
            format: PixelFormat::Nv12,
            stride,
            data: Bytes::from(pixels),
            pts: Timestamp(pts),
            gpu: false,
            dma_buf: None,
        })
    }

    /// Give up on a decoder that accepted our D3D11 device manager but keeps
    /// returning system-memory frames: that is a software decode behind a
    /// slower API, and OpenH264 is the better fallback.
    fn reject_software_output(&self) -> Result<(), String> {
        if self.saw_d3d11_surface {
            return Ok(());
        }
        if self.frames_read > 30 {
            return Err(format!(
                "decoder returned {} system-memory frames and no D3D11 surface: hardware decode is not active",
                self.frames_read
            ));
        }
        Ok(())
    }

    /// Create (or reuse) the staging texture matching a decoded surface.
    fn staging_texture(&mut self, width: u32, height: u32) -> Result<&ID3D11Texture2D, String> {
        let recreate = match &self.staging {
            Some(staging) => staging.width != width || staging.height != height,
            None => true,
        };
        if recreate {
            let description = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                // A staging texture cannot be bound to the pipeline; it only
                // has to be readable by the CPU.
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut texture: Option<ID3D11Texture2D> = None;
            unsafe {
                self.device
                    .CreateTexture2D(&description, None, Some(&mut texture))
            }
            .map_err(|e| format!("staging CreateTexture2D: {e}"))?;
            self.staging = Some(Staging {
                texture: texture.ok_or("staging texture was not created")?,
                width,
                height,
            });
        }
        Ok(&self
            .staging
            .as_ref()
            .expect("staging texture just ensured")
            .texture)
    }
}

impl Drop for Transform {
    fn drop(&mut self) {
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            let _ = self.activation.ShutdownObject();
        }
    }
}

fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let levels = [
        D3D_FEATURE_LEVEL_11_0,
        D3D_FEATURE_LEVEL_10_1,
        D3D_FEATURE_LEVEL_10_0,
    ];
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    unsafe {
        D3D11CreateDevice(
            None::<&IDXGIAdapter>,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE(ptr::null_mut()),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .map_err(|e| format!("D3D11CreateDevice: {e}"))?;
    }
    let device = device.ok_or("D3D11CreateDevice returned no device")?;
    let context = context.ok_or("D3D11CreateDevice returned no immediate context")?;
    // The decoder MFT decodes on its own threads, so the immediate context has
    // to be safe to touch from more than one of them.
    if let Ok(multithread) = device.cast::<ID3D10Multithread>() {
        let _ = unsafe { multithread.SetMultithreadProtected(true) };
    }
    Ok((device, context))
}

fn device_manager(device: &ID3D11Device) -> Result<IMFDXGIDeviceManager, String> {
    let mut token = 0u32;
    let mut manager: Option<IMFDXGIDeviceManager> = None;
    unsafe {
        MFCreateDXGIDeviceManager(&mut token, &mut manager)
            .map_err(|e| format!("MFCreateDXGIDeviceManager: {e}"))?;
    }
    let manager = manager.ok_or("no D3D11 device manager")?;
    unsafe {
        manager
            .ResetDevice(device, token)
            .map_err(|e| format!("IMFDXGIDeviceManager::ResetDevice: {e}"))?;
    }
    Ok(manager)
}

fn adapter_name(device: &ID3D11Device) -> Option<String> {
    let dxgi: IDXGIDevice = device.cast().ok()?;
    let adapter = unsafe { dxgi.GetAdapter() }.ok()?;
    let description = unsafe { adapter.GetDesc() }.ok()?;
    let end = description
        .Description
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(description.Description.len());
    let name = String::from_utf16_lossy(&description.Description[..end]);
    let name = name.trim().to_string();
    (!name.is_empty()).then_some(name)
}

fn texture_description(
    texture: &ID3D11Texture2D,
) -> (
    u32,
    u32,
    windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT,
) {
    let mut description = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut description) };
    (description.Width, description.Height, description.Format)
}

fn stream_ids(transform: &IMFTransform) -> Result<(u32, u32), String> {
    let mut inputs = 0u32;
    let mut outputs = 0u32;
    unsafe { transform.GetStreamCount(&mut inputs, &mut outputs) }
        .map_err(|e| format!("GetStreamCount: {e}"))?;
    if inputs != 1 || outputs != 1 {
        return Err(format!(
            "MFT exposes {inputs} input / {outputs} output streams"
        ));
    }
    let mut input_ids = [0u32];
    let mut output_ids = [0u32];
    match unsafe { transform.GetStreamIDs(&mut input_ids, &mut output_ids) } {
        Ok(()) => Ok((input_ids[0], output_ids[0])),
        // Fixed-stream MFTs are allowed to decline: ids are then zero-based.
        Err(e) if e.code() == E_NOTIMPL => Ok((0, 0)),
        Err(e) => Err(format!("GetStreamIDs: {e}")),
    }
}

fn set_input_type(
    transform: &IMFTransform,
    id: u32,
    width: u32,
    height: u32,
) -> Result<(), String> {
    unsafe {
        let media_type = MFCreateMediaType().map_err(describe)?;
        media_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(describe)?;
        media_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
            .map_err(describe)?;
        media_type
            .SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))
            .map_err(describe)?;
        media_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(describe)?;
        transform
            .SetInputType(id, &media_type, 0)
            .map_err(|e| format!("SetInputType: {e}"))
    }
}

fn set_output_type(
    transform: &IMFTransform,
    id: u32,
    width: u32,
    height: u32,
) -> Result<(), String> {
    unsafe {
        let media_type = MFCreateMediaType().map_err(describe)?;
        media_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(describe)?;
        media_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
            .map_err(describe)?;
        media_type
            .SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))
            .map_err(describe)?;
        media_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(describe)?;
        media_type
            .SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)
            .map_err(describe)?;
        // Frame rate is not known from dimensions or the requested UI rate.
        // Let the MFT derive it from the bitstream and renegotiate as needed;
        // declaring 30 here contradicts 60/120/custom-rate input.
        media_type
            .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))
            .map_err(describe)?;
        transform
            .SetOutputType(id, &media_type, 0)
            .map_err(|e| format!("SetOutputType: {e}"))
    }
}

/// Fall back to whatever NV12 output type the MFT advertises.
fn select_nv12_output(transform: &IMFTransform, id: u32) -> Result<(), String> {
    let mut last = "no NV12 output type offered".to_string();
    for index in 0..64u32 {
        let candidate = match unsafe { transform.GetOutputAvailableType(id, index) } {
            Ok(candidate) => candidate,
            Err(e) if e.code() == MF_E_NO_MORE_TYPES => break,
            Err(e) => return Err(format!("GetOutputAvailableType: {e}")),
        };
        let is_nv12 = unsafe { candidate.GetGUID(&MF_MT_SUBTYPE) }
            .map(|subtype| subtype == MFVideoFormat_NV12)
            .unwrap_or(false);
        if !is_nv12 {
            continue;
        }
        match unsafe { transform.SetOutputType(id, &candidate, 0) } {
            Ok(()) => return Ok(()),
            Err(e) => last = format!("SetOutputType(advertised): {e}"),
        }
    }
    Err(last)
}

fn input_sample(data: &[u8], time_100ns: i64, keyframe: bool) -> Result<IMFSample, String> {
    let len = u32::try_from(data.len()).map_err(|_| "access unit larger than 4 GiB")?;
    unsafe {
        let sample = MFCreateSample().map_err(describe)?;
        let buffer = MFCreateMemoryBuffer(len).map_err(describe)?;
        let mut destination: *mut u8 = ptr::null_mut();
        buffer
            .Lock(&mut destination, None, None)
            .map_err(describe)?;
        ptr::copy_nonoverlapping(data.as_ptr(), destination, data.len());
        buffer.Unlock().map_err(describe)?;
        buffer.SetCurrentLength(len).map_err(describe)?;
        sample.AddBuffer(&buffer).map_err(describe)?;
        sample.SetSampleTime(time_100ns).map_err(describe)?;
        // RTP gives a presentation timestamp, not this frame's duration.
        // Duration may be omitted for compressed input; a fabricated 33 ms
        // makes high-rate samples overlap. Keep the real PTS unchanged.
        if keyframe {
            // Tells the decoder (and the D3D11 decoder's DPB management) that
            // this access unit starts a new sequence.
            sample
                .SetUINT32(&MFSampleExtension_CleanPoint, 1)
                .map_err(describe)?;
        }
        Ok(sample)
    }
}

fn empty_sample(size: u32) -> Result<IMFSample, String> {
    unsafe {
        let sample = MFCreateSample().map_err(describe)?;
        let buffer = MFCreateMemoryBuffer(size.max(1)).map_err(describe)?;
        sample.AddBuffer(&buffer).map_err(describe)?;
        Ok(sample)
    }
}

/// `MF_MT_FRAME_SIZE` / `MF_MT_FRAME_RATE` style packed pair: high then low.
fn pack(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

/// MFT enumeration results, freed the way `MFTEnumEx` allocates them.
struct Activations {
    pointer: *mut Option<IMFActivate>,
    count: u32,
}

impl Activations {
    fn enumerate(flags: MFT_ENUM_FLAG) -> Result<Self, String> {
        let input = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };
        let mut pointer: *mut Option<IMFActivate> = ptr::null_mut();
        let mut count = 0u32;
        unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_DECODER,
                flags,
                Some(&input),
                None,
                &mut pointer,
                &mut count,
            )
            .map_err(|e| format!("MFTEnumEx: {e}"))?;
        }
        Ok(Self { pointer, count })
    }

    fn entries(&self) -> &[Option<IMFActivate>] {
        if self.count == 0 || self.pointer.is_null() {
            &[]
        } else {
            // SAFETY: `MFTEnumEx` allocated `count` entries with CoTaskMemAlloc.
            unsafe { std::slice::from_raw_parts(self.pointer, self.count as usize) }
        }
    }
}

impl Drop for Activations {
    fn drop(&mut self) {
        if self.pointer.is_null() {
            return;
        }
        unsafe {
            ptr::drop_in_place(ptr::slice_from_raw_parts_mut(
                self.pointer,
                self.count as usize,
            ));
            CoTaskMemFree(Some(self.pointer.cast()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_size_and_ratio_are_packed_high_then_low() {
        // MF_MT_FRAME_SIZE is (width << 32) | height, MF_MT_FRAME_RATE is
        // (numerator << 32) | denominator.
        assert_eq!(pack(1920, 1080), (1920u64 << 32) | 1080);
        assert_eq!(pack(30, 1), (30u64 << 32) | 1);
    }
}
