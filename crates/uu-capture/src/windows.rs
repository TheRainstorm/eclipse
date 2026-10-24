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
    // DXGI caches DPI-virtualized coordinates when its factory is created.
    // Establish physical pixels before CreateDXGIFactory1, not just GetDesc,
    // so a service's first negotiation matches duplication textures and input.
    let _coordinates = uu_input::windows_desktop::PhysicalCoordinates::enter().map_err(backend)?;
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
    source_frames: crate::frame_info::SourceFrames,
    present_info: Option<crate::frame_info::SourceTime>,
    accumulated_frames: u32,
    qpc_frequency: u64,
    started: Instant,
    staging: Option<ID3D11Texture2D>,
    capture_pool: Option<uu_media::encoder::CapturePool>,
    gpu: bool,
    hdr: Option<uu_media::hdr::Metadata>,
    allow_hdr:bool,
    adapter_vendor: u16,
    duplication: Option<IDXGIOutputDuplication>,
    retry_at: Instant,
    context: ID3D11DeviceContext,
    device: ID3D11Device,
}
impl DxgiCapture {
    pub fn open(source: &CaptureSource) -> Result<Self, CaptureError> {Self::open_hdr(source,false)}
    pub fn open_hdr(source: &CaptureSource,allow_hdr:bool) -> Result<Self,CaptureError> {
        uu_input::windows_desktop::sync_input_desktop()
            .map_err(|e| backend(uu_input::windows_desktop::access_error(&e)))?;
        let selected = match source {
            CaptureSource::Primary | CaptureSource::Desktop { initial_monitor:None } => outputs()?
                .into_iter()
                .find(|o| o.monitor.primary)
                .ok_or_else(|| backend("当前没有活动的桌面显示输出；请确认被控用户已登录，显示器或虚拟显示输出保持启用。锁屏共享需安装并启动 Windows 后台服务"))?,
            CaptureSource::Monitor(name) | CaptureSource::Desktop {initial_monitor:Some(name)} => outputs()?
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
            .map_err(|error| backend(format!("D3D11CreateDevice for {}: {error}", selected.monitor.name)))?;
        }
        let device = device.ok_or_else(|| backend("device unavailable"))?;
        let context = context.ok_or_else(|| backend("device context unavailable"))?;
        let multithread: windows::Win32::Graphics::Direct3D10::ID3D10Multithread =
            context.cast().map_err(backend)?;
        unsafe {
            let _ = multithread.SetMultithreadProtected(true);
        }
        let adapter_vendor =
            unsafe { selected.adapter.GetDesc1().map_err(backend)? }.VendorId as u16;
        let hdr=if allow_hdr && selected.rotation==Rotation::None {output_hdr(&selected.output)}else{None};
        let duplication=duplicate(&selected.output,&device,hdr.is_some()).map_err(|e| {
            if e.code()==windows::Win32::Foundation::E_ACCESSDENIED {backend("当前桌面拒绝采集；锁屏和 UAC 安全桌面需要 Windows 后台服务以 LocalSystem 运行")}
            else{backend(format!("{} for {}: {e}", if hdr.is_some(){"DuplicateOutput1 HDR"}else{"DuplicateOutput SDR"}, selected.monitor.name))}
        })?;
        Ok(Self {
            source: selected.monitor,
            rotation: selected.rotation,
            last: None,
            source_frames: Default::default(),
            present_info: None,
            accumulated_frames: 0,
            qpc_frequency: qpc_frequency(),
            started: Instant::now(),
            staging: None,
            capture_pool: None,
            gpu: false,
            hdr,
            allow_hdr:allow_hdr && selected.rotation==Rotation::None,
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
        if self.duplication.is_some() { self.source_frames.retire(); }
        self.last = None;
        self.staging = None;
        self.capture_pool = None;
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
        self.duplication = Some(unsafe {
            let now_hdr=output_hdr(&selected.output);
            if self.allow_hdr && self.hdr.is_some()!=now_hdr.is_some() {return Err(backend("显示器 HDR 状态已改变，请重新连接"));}
            duplicate(&selected.output,&self.device,self.hdr.is_some()).map_err(|e| {
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
        // AcquireNextFrame also reports pointer-only changes. Their resource
        // is not a new desktop image (some drivers expose the display's native
        // RGB10 surface here). Keep the last real image instead of inspecting
        // or submitting this resource as a BGRA video frame.
        if info.LastPresentTime == 0 && info.AccumulatedFrames == 0 {
            return Ok(None);
        }
        self.present_info = u64::try_from(info.LastPresentTime).ok().filter(|ticks| *ticks > 0)
            .filter(|_| self.qpc_frequency > 0)
            .map(|qpc_ticks| crate::frame_info::SourceTime { qpc_ticks, frequency_hz: self.qpc_frequency });
        self.accumulated_frames = info.AccumulatedFrames;
        let texture: ID3D11Texture2D = resource
            .ok_or_else(|| backend("frame resource unavailable"))?
            .cast()
            .map_err(backend)?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe {
            texture.GetDesc(&mut desc);
        }
        if !matches!(desc.Format,DXGI_FORMAT_B8G8R8A8_UNORM|DXGI_FORMAT_R16G16B16A16_FLOAT) {
            return Err(backend(format!("unsupported desktop surface format: format={}, size={}x{}, accumulated={}, present={}",desc.Format.0,desc.Width,desc.Height,info.AccumulatedFrames,info.LastPresentTime)));
        }
        if self.gpu && self.rotation == Rotation::None {
            if (desc.Width, desc.Height) != (self.source.width, self.source.height) {
                return Err(backend(format!("desktop layout changed: expected {}x{}, acquired {}x{}; reopen capture",self.source.width,self.source.height,desc.Width,desc.Height)));
            }
            if self.capture_pool.is_none() {
                let device = uu_media::encoder::InputDevice::from_d3d11(self.device.clone()).map_err(backend)?;
                self.capture_pool = Some(uu_media::encoder::CapturePool::new(device, desc.Width, desc.Height).map_err(backend)?);
            }
            let pts = self.timestamp();
            return self.capture_pool.as_mut().unwrap().capture(&texture, pts).map_err(backend);
        }
        let mut needs_staging = true;
        if let Some(staging) = &self.staging {
            let mut old = D3D11_TEXTURE2D_DESC::default();
            unsafe {
                staging.GetDesc(&mut old);
            }
            needs_staging = old.Width != desc.Width || old.Height != desc.Height || old.Format!=desc.Format;
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
        if desc.Format==DXGI_FORMAT_R16G16B16A16_FLOAT {
            if self.rotation!=Rotation::None || desc.Width!=self.source.width || desc.Height!=self.source.height || mapped.RowPitch<desc.Width*8 {return Err(backend("HDR desktop CPU layout changed"));}
            let row=desc.Width as usize*8;let mut pixels=vec![0u8;row*desc.Height as usize];
            for y in 0..desc.Height as usize {pixels[y*row..(y+1)*row].copy_from_slice(&data[y*mapped.RowPitch as usize..y*mapped.RowPitch as usize+row]);}
            let frame=RawFrame{width:desc.Width,height:desc.Height,format:PixelFormat::ScRgb16,stride:desc.Width*8,data:pixels.into(),pts:self.timestamp(),gpu:false,dma_buf:None,d3d11:None};
            let mut bgra=uu_media::convert::hdr_frame_to_rgba(&frame,uu_media::color::YuvColor::Limited2020Pq,self.hdr.map_or(203.,|m|m.sdr_white_nits));
            for pixel in bgra.chunks_exact_mut(4){pixel.swap(0,2);}
            return Ok(Some(RawFrame{format:PixelFormat::Bgra8,stride:desc.Width*4,data:bgra.into(),..frame}));
        }
        let (width, height, data) = bgra_rows(
            data,
            desc.Width as usize,
            desc.Height as usize,
            mapped.RowPitch as usize,
            self.rotation,
        )?;
        if width != self.source.width as usize || height != self.source.height as usize {
            return Err(backend(format!("desktop layout changed: expected {}x{}, acquired {}x{}; reopen capture",self.source.width,self.source.height,width,height)));
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
    fn frame_info(&self)->Option<crate::frame_info::FrameInfo> {
        self.source_frames.snapshot(qpc_now())
    }
    fn hdr_metadata(&self)->Option<uu_media::hdr::Metadata>{self.hdr}
    fn encoder_input_device(&self)->Option<uu_media::encoder::InputDevice> {
        uu_media::encoder::InputDevice::from_d3d11(self.device.clone()).ok()
    }
    fn prefer_gpu(&mut self, enabled: bool, vendor: Option<u16>) {
        let enabled = enabled
            && self.rotation == Rotation::None
            && vendor.is_none_or(|v| v == self.adapter_vendor);
        if self.gpu != enabled {
            self.last = None;
            self.capture_pool = None;
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
                self.source_frames.new_frame(self.present_info, self.accumulated_frames);
                self.last = Some(frame.clone());
                return Ok(frame);
            }
            if let Some(last) = &self.last {
                self.source_frames.repeated();
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
fn qpc_now() -> Option<u64> {
    let mut value = 0;
    unsafe { windows::Win32::System::Performance::QueryPerformanceCounter(&mut value).ok()?; }
    u64::try_from(value).ok()
}
fn qpc_frequency() -> u64 {
    let mut value = 0;
    unsafe { if windows::Win32::System::Performance::QueryPerformanceFrequency(&mut value).is_err() { return 0; } }
    u64::try_from(value).unwrap_or(0)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an interactive output; checks duplication geometry without acquiring desktop pixels"]
    fn capture_inventory_and_duplication_use_same_physical_geometry() {
        std::thread::spawn(|| {
            let capture=DxgiCapture::open(&CaptureSource::Primary).unwrap();
            let duplication=capture.duplication.as_ref().expect("active duplication is required");
            let desc=unsafe {duplication.GetDesc()};
            let (width,height)=match capture.rotation {
                Rotation::Clockwise90|Rotation::Clockwise270=>(desc.ModeDesc.Height,desc.ModeDesc.Width),
                _=>(desc.ModeDesc.Width,desc.ModeDesc.Height),
            };
            println!("capture={}x{} duplication={}x{}",capture.source.width,capture.source.height,width,height);
            assert_eq!((capture.source.width,capture.source.height),(width,height));
        }).join().unwrap();
    }
}

fn output_hdr(output:&IDXGIOutput)->Option<uu_media::hdr::Metadata> {
    let desc=unsafe{output.cast::<IDXGIOutput6>().ok()?.GetDesc1().ok()?};
    if desc.ColorSpace!=DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 {return None;}
    let metadata=uu_media::hdr::Metadata {
        primaries:[desc.RedPrimary,desc.GreenPrimary,desc.BluePrimary],white:desc.WhitePoint,
        min_nits:desc.MinLuminance,max_nits:desc.MaxLuminance,max_full_frame_nits:desc.MaxFullFrameLuminance,
        sdr_white_nits:uu_media::hdr::desktop_sdr_white(&desc.DeviceName),
    };
    let metadata=metadata.with_display_defaults();
    metadata.valid().then_some(metadata)
}
fn duplicate(output:&IDXGIOutput,device:&ID3D11Device,hdr:bool)->windows::core::Result<IDXGIOutputDuplication> {
    unsafe {
        if hdr {output.cast::<IDXGIOutput5>()?.DuplicateOutput1(device,0,&[DXGI_FORMAT_R16G16B16A16_FLOAT,DXGI_FORMAT_B8G8R8A8_UNORM])}
        else{output.cast::<IDXGIOutput1>()?.DuplicateOutput(device)}
    }
}
