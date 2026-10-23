//! DXGI Desktop Duplication on the current OS input desktop. Windowless workers
//! bind explicitly; access to secure desktops still requires LocalSystem.
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
            "没有有效显示输出；需要已登录的控制台会话和物理或虚拟显示器。锁屏串流请安装 Windows 后台服务",
        ));
    }
    Ok(list)
}
pub fn monitors() -> Result<Vec<Monitor>, CaptureError> {
    // Callers include GUI threads with windows/hooks; those threads cannot
    // change desktops. Enumerate on a short-lived windowless worker instead.
    std::thread::Builder::new()
        .name("uu-display-inventory".into())
        .spawn(|| {
            uu_input::windows_desktop::sync_input_desktop()
                .map_err(|e| backend(uu_input::windows_desktop::access_error(&e)))?;
            Ok(outputs()?.into_iter().map(|o| o.monitor).collect())
        })
        .map_err(backend)?
        .join()
        .map_err(|_| backend("显示器枚举工作线程异常"))?
}

pub struct DxgiCapture {
    source: Monitor,
    rotation: Rotation,
    last: Option<RawFrame>,
    started: Instant,
    staging: Option<ID3D11Texture2D>,
    gpu: bool,
    adapter_vendor: u16,
    duplication: Option<IDXGIOutputDuplication>,
    retry_at: Instant,
    context: ID3D11DeviceContext,
    device: ID3D11Device,
}
impl DxgiCapture {
    pub fn open(source: &CaptureSource) -> Result<Self, CaptureError> {
        uu_input::windows_desktop::sync_input_desktop()
            .map_err(|e| backend(uu_input::windows_desktop::access_error(&e)))?;
        let selected = match source {
            CaptureSource::Primary | CaptureSource::Desktop => outputs()?
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
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
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
        let multithread: windows::Win32::Graphics::Direct3D10::ID3D10Multithread =
            context.cast().map_err(backend)?;
        unsafe {
            let _ = multithread.SetMultithreadProtected(true);
        }
        let output: IDXGIOutput1 = selected.output.cast().map_err(backend)?;
        let adapter_vendor =
            unsafe { selected.adapter.GetDesc1().map_err(backend)? }.VendorId as u16;
        let duplication = unsafe {
            output.DuplicateOutput(&device).map_err(|e| {
            if e.code() == windows::Win32::Foundation::E_ACCESSDENIED {
                backend("当前桌面拒绝采集；锁屏和 UAC 安全桌面需要 Windows 后台服务以 LocalSystem 运行")
            } else { backend(e) }
        })?
        };
        Ok(Self {
            source: selected.monitor,
            rotation: selected.rotation,
            last: None,
            started: Instant::now(),
            staging: None,
            gpu: false,
            adapter_vendor,
            duplication: Some(duplication),
            retry_at: Instant::now(),
            context,
            device,
        })
    }
    fn timestamp(&self) -> Timestamp {
        Timestamp(self.started.elapsed().as_micros().min(u64::MAX as u128) as u64)
    }
    fn invalidate(&mut self) {
        self.last = None;
        self.staging = None;
        self.duplication = None;
    }
    fn resume(&mut self) -> Result<(), CaptureError> {
        if Instant::now() < self.retry_at {
            return Err(CaptureError::Suspended("等待桌面切换完成".into()));
        }
        self.retry_at = Instant::now() + Duration::from_millis(100);
        let selected = outputs()
            .map_err(|e| CaptureError::Suspended(e.to_string()))?
            .into_iter()
            .find(|o| o.monitor.name == self.source.name)
            .ok_or_else(|| CaptureError::Suspended("等待原显示输出恢复".into()))?;
        // Input coordinates and encoder dimensions are leased to the
        // selected output. A topology change requires fresh negotiation.
        if selected.monitor != self.source || selected.rotation != self.rotation {
            return Err(backend("显示布局已改变，请重新连接"));
        }
        let output: IDXGIOutput1 = selected.output.cast().map_err(backend)?;
        self.duplication = Some(unsafe {
            output.DuplicateOutput(&self.device).map_err(|e| {
                if e.code() == DXGI_ERROR_ACCESS_LOST
                    || e.code() == DXGI_ERROR_SESSION_DISCONNECTED
                    || e.code() == DXGI_ERROR_UNSUPPORTED
                    || e.code() == DXGI_ERROR_NOT_CURRENTLY_AVAILABLE
                    || e.code() == windows::Win32::Foundation::E_ACCESSDENIED
                {
                    CaptureError::Suspended(format!("等待当前桌面采集权限或显示模式恢复：{e}"))
                } else {
                    backend(e)
                }
            })?
        });
        Ok(())
    }
    fn acquire(&mut self) -> Result<Option<RawFrame>, CaptureError> {
        let changed = match uu_input::windows_desktop::sync_input_desktop() {
            Ok(changed) => changed,
            Err(error) => {
                self.invalidate();
                return Err(CaptureError::Suspended(
                    uu_input::windows_desktop::access_error(&error),
                ));
            }
        };
        if changed {
            self.invalidate();
        }
        if self.duplication.is_none() {
            self.resume()?;
        }
        let duplication = self.duplication.as_ref().unwrap().clone();
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        // The producer already paces each frame. Once a cached desktop exists,
        // poll for updates without adding another blocking wait to that interval.
        // Initial acquisition keeps a short wait to avoid spinning on an idle desktop.
        let wait_ms = if self.last.is_some() { 0 } else { 5 };
        match unsafe { duplication.AcquireNextFrame(wait_ms, &mut info, &mut resource) } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
            Err(e)
                if e.code() == DXGI_ERROR_ACCESS_LOST
                    || e.code() == DXGI_ERROR_SESSION_DISCONNECTED
                    || e.code() == DXGI_ERROR_UNSUPPORTED
                    || e.code() == windows::Win32::Foundation::E_ACCESSDENIED =>
            {
                self.invalidate();
                return Err(CaptureError::Suspended(format!("等待 DXGI 桌面恢复：{e}")));
            }
            Err(e) => return Err(backend(e)),
        }
        // Every successful acquisition releases its frame, including all error paths.
        let _lease = FrameLease(duplication);
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
        if self.gpu && self.rotation == Rotation::None {
            if (desc.Width, desc.Height) != (self.source.width, self.source.height) {
                return Err(backend("desktop layout changed; reopen capture"));
            }
            let mut owned = None;
            let owned_desc = D3D11_TEXTURE2D_DESC {
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
                ..desc
            };
            unsafe {
                self.device
                    .CreateTexture2D(&owned_desc, None, Some(&mut owned))
                    .map_err(backend)?;
            }
            let owned = owned.ok_or_else(|| backend("GPU capture texture unavailable"))?;
            unsafe {
                self.context.CopyResource(&owned, &texture);
                self.context.Flush();
            }
            let surface = uu_media::d3d11_frame::Frame::captured(owned, self.device.clone())
                .map_err(backend)?;
            return Ok(Some(RawFrame {
                width: desc.Width,
                height: desc.Height,
                format: PixelFormat::Bgra8,
                stride: desc.Width * 4,
                data: Default::default(),
                pts: self.timestamp(),
                gpu: true,
                dma_buf: None,
                d3d11: Some(std::sync::Arc::new(surface)),
            }));
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
    fn prefer_gpu(&mut self, enabled: bool, vendor: Option<u16>) {
        let enabled = enabled
            && self.rotation == Rotation::None
            && vendor.is_none_or(|v| v == self.adapter_vendor);
        if self.gpu != enabled {
            self.last = None;
            self.gpu = enabled;
        }
    }
    fn resolution(&self) -> (u32, u32) {
        (self.source.width, self.source.height)
    }
    fn desktop_rect(&self) -> Option<crate::DesktopRect> {
        Some(crate::DesktopRect {
            x: self.source.x,
            y: self.source.y,
            width: self.source.width,
            height: self.source.height,
        })
    }
    fn next_frame(&mut self) -> Result<RawFrame, CaptureError> {
        let deadline = Instant::now() + Duration::from_millis(250);
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
                return Err(CaptureError::Suspended("等待当前桌面首帧".into()));
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
