//! DXGI Desktop Duplication, using only the caller's current interactive desktop.
use crate::{
    pixels::{bgra_rows, Rotation},
    Capture, CaptureError, CaptureSource, CaptureTarget,
};
use std::time::{Duration, Instant};
use uu_media::{
    frame::{PixelFormat, RawFrame},
    Timestamp,
};
use windows::core::Interface;
use windows::Win32::{
    Foundation::HMODULE,
    Graphics::{
        Direct3D::{
            D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1,
            D3D_FEATURE_LEVEL_11_0,
        },
        Direct3D11::*,
        Dxgi::{Common::*, *},
    },
};

fn backend(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::Backend(format!("DXGI: {error}"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}
struct Output {
    monitor: Monitor,
    adapter: IDXGIAdapter1,
    output: IDXGIOutput,
    rotation: Rotation,
}
fn outputs() -> Result<Vec<Output>, CaptureError> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1().map_err(backend)? };
    let mut list = Vec::new();
    for index in 0..128 {
        let adapter = match unsafe { factory.EnumAdapters1(index) } {
            Ok(a) => a,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(backend(e)),
        };
        for index in 0..128 {
            let output = match unsafe { adapter.EnumOutputs(index) } {
                Ok(o) => o,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(backend(e)),
            };
            let desc = unsafe { output.GetDesc().map_err(backend)? };
            if !desc.AttachedToDesktop.as_bool() {
                continue;
            }
            let rect = desc.DesktopCoordinates;
            let width = u32::try_from(rect.right - rect.left).map_err(backend)?;
            let height = u32::try_from(rect.bottom - rect.top).map_err(backend)?;
            if width == 0 || height == 0 {
                continue;
            }
            let end = desc
                .DeviceName
                .iter()
                .position(|v| *v == 0)
                .unwrap_or(desc.DeviceName.len());
            let rotation = match desc.Rotation {
                DXGI_MODE_ROTATION_ROTATE90 => Rotation::Clockwise90,
                DXGI_MODE_ROTATION_ROTATE180 => Rotation::Half,
                DXGI_MODE_ROTATION_ROTATE270 => Rotation::Clockwise270,
                _ => Rotation::None,
            };
            list.push(Output {
                monitor: Monitor {
                    name: String::from_utf16_lossy(&desc.DeviceName[..end]),
                    x: rect.left,
                    y: rect.top,
                    width,
                    height,
                    primary: rect.left == 0 && rect.top == 0,
                },
                adapter: adapter.clone(),
                output,
                rotation,
            });
        }
    }
    if list.is_empty() {
        return Err(backend(
            "no active desktop output; run in the logged-in interactive session",
        ));
    }
    Ok(list)
}
pub fn monitors() -> Result<Vec<Monitor>, CaptureError> {
    Ok(outputs()?.into_iter().map(|o| o.monitor).collect())
}

pub struct DxgiCapture {
    source: Monitor,
    rotation: Rotation,
    last: Option<RawFrame>,
    started: Instant,
    staging: Option<ID3D11Texture2D>,
    duplication: IDXGIOutputDuplication,
    context: ID3D11DeviceContext,
    device: ID3D11Device,
}
impl DxgiCapture {
    pub fn open(source: &CaptureSource) -> Result<Self, CaptureError> {
        let selected = match source {
            CaptureSource::Primary => outputs()?
                .into_iter()
                .find(|o| o.monitor.primary)
                .ok_or(CaptureError::NoBackend)?,
            CaptureSource::Monitor(name) => outputs()?
                .into_iter()
                .find(|o| &o.monitor.name == name)
                .ok_or_else(|| CaptureError::TargetNotFound(CaptureTarget::Output(name.clone())))?,
            CaptureSource::Window(id) => {
                return Err(CaptureError::TargetNotFound(CaptureTarget::Window(*id)))
            }
            _ => return Err(CaptureError::NoBackend),
        };
        let (mut device, mut context) = (None, None);
        unsafe {
            D3D11CreateDevice(
                &selected.adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[
                    D3D_FEATURE_LEVEL_11_0,
                    D3D_FEATURE_LEVEL_10_1,
                    D3D_FEATURE_LEVEL_10_0,
                ]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(backend)?;
        }
        let device = device.ok_or_else(|| backend("device unavailable"))?;
        let context = context.ok_or_else(|| backend("device context unavailable"))?;
        let output: IDXGIOutput1 = selected.output.cast().map_err(backend)?;
        let duplication = unsafe { output.DuplicateOutput(&device).map_err(backend)? };
        Ok(Self {
            source: selected.monitor,
            rotation: selected.rotation,
            last: None,
            started: Instant::now(),
            staging: None,
            duplication,
            context,
            device,
        })
    }
    fn timestamp(&self) -> Timestamp {
        Timestamp(self.started.elapsed().as_micros().min(u64::MAX as u128) as u64)
    }
    fn acquire(&mut self) -> Result<Option<RawFrame>, CaptureError> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        // The producer already paces each frame. Once a cached desktop exists,
        // poll for updates without adding another blocking wait to that interval.
        // Initial acquisition keeps a short wait to avoid spinning on an idle desktop.
        let wait_ms = if self.last.is_some() { 0 } else { 5 };
        match unsafe {
            self.duplication
                .AcquireNextFrame(wait_ms, &mut info, &mut resource)
        } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
            Err(e) => return Err(backend(e)),
        }
        // Every successful acquisition releases its frame, including all error paths.
        let _lease = FrameLease(self.duplication.clone());
        let texture: ID3D11Texture2D = resource
            .ok_or_else(|| backend("frame resource unavailable"))?
            .cast()
            .map_err(backend)?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe {
            texture.GetDesc(&mut desc);
        }
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(backend("unsupported desktop surface format"));
        }
        let mut needs_staging = true;
        if let Some(staging) = &self.staging {
            let mut old = D3D11_TEXTURE2D_DESC::default();
            unsafe {
                staging.GetDesc(&mut old);
            }
            needs_staging = old.Width != desc.Width || old.Height != desc.Height;
        }
        if needs_staging {
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: desc.Width,
                Height: desc.Height,
                MipLevels: 1,
                ArraySize: 1,
                Format: desc.Format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging = None;
            unsafe {
                self.device
                    .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                    .map_err(backend)?;
            }
            self.staging = Some(staging.ok_or_else(|| backend("staging surface unavailable"))?);
        }
        let staging = self
            .staging
            .as_ref()
            .ok_or_else(|| backend("staging surface unavailable"))?;
        unsafe {
            self.context.CopyResource(staging, &texture);
        }
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(backend)?;
        }
        let _mapped = MappedSurface {
            context: self.context.clone(),
            surface: staging.clone(),
        };
        let len = (mapped.RowPitch as usize)
            .checked_mul(desc.Height as usize)
            .filter(|n| *n <= 256 * 1024 * 1024)
            .ok_or_else(|| backend("desktop frame too large"))?;
        if mapped.pData.is_null() {
            return Err(backend("mapped desktop surface has no pixels"));
        }
        // D3D11 Map guarantees RowPitch bytes per row for the mapped texture.
        let data = unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), len) };
        let (width, height, data) = bgra_rows(
            data,
            desc.Width as usize,
            desc.Height as usize,
            mapped.RowPitch as usize,
            self.rotation,
        )?;
        if width != self.source.width as usize || height != self.source.height as usize {
            return Err(backend("desktop layout changed; reopen capture"));
        }
        Ok(Some(RawFrame {
            width: width as u32,
            height: height as u32,
            format: PixelFormat::Bgra8,
            stride: width as u32 * 4,
            data: data.into(),
            pts: self.timestamp(),
            gpu: false,
            dma_buf: None,
            d3d11: None,
        }))
    }
}
impl Capture for DxgiCapture {
    fn resolution(&self) -> (u32, u32) {
        (self.source.width, self.source.height)
    }
    fn desktop_rect(&self) -> Option<crate::DesktopRect> {
        Some(crate::DesktopRect {x:self.source.x,y:self.source.y,width:self.source.width,height:self.source.height})
    }
    fn next_frame(&mut self) -> Result<RawFrame, CaptureError> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(frame) = self.acquire()? {
                self.last = Some(frame.clone());
                return Ok(frame);
            }
            if let Some(last) = &self.last {
                let mut frame = last.clone();
                frame.pts = self.timestamp();
                return Ok(frame);
            }
            if Instant::now() >= deadline {
                return Err(backend("waiting for initial desktop frame timed out"));
            }
        }
    }
}
struct FrameLease(IDXGIOutputDuplication);
impl Drop for FrameLease {
    fn drop(&mut self) {
        unsafe {
            let _ = self.0.ReleaseFrame();
        }
    }
}
struct MappedSurface {
    context: ID3D11DeviceContext,
    surface: ID3D11Texture2D,
}
impl Drop for MappedSurface {
    fn drop(&mut self) {
        unsafe {
            self.context.Unmap(&self.surface, 0);
        }
    }
}
