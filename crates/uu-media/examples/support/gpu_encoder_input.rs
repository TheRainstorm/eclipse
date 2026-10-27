//! Generates D3D11 input without reading a desktop. Upload is outside encode timing.
use anyhow::{ensure, Context, Result};
use uu_media::{d3d11_frame::Frame, PixelFormat, RawFrame};
use windows::{
    core::Interface,
    Win32::{
        Foundation::{HMODULE, RECT},
        Graphics::{
            Direct3D::*,
            Direct3D10::ID3D10Multithread,
            Direct3D11::*,
            Dxgi::{Common::*, IDXGIAdapter},
        },
    },
};
pub struct Upload {
    device: ID3D11Device,
    input: uu_media::encoder::InputDevice,
    pool: std::cell::RefCell<Option<uu_media::encoder::CapturePool>>,
    last: std::cell::RefCell<Option<RawFrame>>,
}
impl Upload {
    pub fn input_device(&self) -> Result<uu_media::encoder::InputDevice> {
        Ok(self.input.clone())
    }
    pub fn new() -> Result<Self> {
        let (mut device, mut context) = (None, None);
        unsafe {
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        let context = context.context("missing D3D11 context")?;
        let multithread: ID3D10Multithread = context.cast()?;
        unsafe {
            let _ = multithread.SetMultithreadProtected(true);
        }
        let device = device.context("missing D3D11 device")?;
        let input = uu_media::encoder::InputDevice::from_d3d11(device.clone())?;
        Ok(Self {
            device,
            input,
            pool: Default::default(),
            last: Default::default(),
        })
    }
    pub fn frame(&self, frame: &RawFrame) -> Result<RawFrame> {
        ensure!(
            frame.format == PixelFormat::Bgra8 && !frame.gpu && frame.is_consistent(),
            "fixture needs CPU BGRA pixels"
        );
        let mut texture = None;
        unsafe {
            self.device.CreateTexture2D(
                &D3D11_TEXTURE2D_DESC {
                    Width: frame.width,
                    Height: frame.height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
                    ..Default::default()
                },
                Some(&D3D11_SUBRESOURCE_DATA {
                    pSysMem: frame.data.as_ptr().cast(),
                    SysMemPitch: frame.stride,
                    SysMemSlicePitch: 0,
                }),
                Some(&mut texture),
            )?;
        }
        let surface = Frame::captured(
            texture.context("missing fixture texture")?,
            self.device.clone(),
        )
        .map_err(anyhow::Error::msg)?;
        let output = RawFrame {
            data: Default::default(),
            gpu: true,
            d3d11: Some(std::sync::Arc::new(surface)),
            ..frame.clone()
        };
        ensure!(output.is_consistent(), "invalid GPU fixture frame");
        Ok(output)
    }
    /// Draw the same moving bars directly into a private GPU texture. This
    /// exercises encoder input without CPU rasterization or desktop access.
    pub fn generated(
        &self,
        width: u32,
        height: u32,
        index: u32,
        pts: uu_media::Timestamp,
    ) -> Result<RawFrame> {
        self.generated_with_copy(width, height, index, pts, None)
    }
    /// Optional DXGI-style snapshot copy; both modes use the same generated source.
    pub fn generated_with_copy(
        &self,
        width: u32,
        height: u32,
        index: u32,
        pts: uu_media::Timestamp,
        pooled: Option<bool>,
    ) -> Result<RawFrame> {
        ensure!(
            (2..=8192).contains(&width) && (2..=8192).contains(&height),
            "invalid generated GPU dimensions"
        );
        let mut texture = None;
        unsafe {
            self.device.CreateTexture2D(
                &D3D11_TEXTURE2D_DESC {
                    Width: width,
                    Height: height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
                    ..Default::default()
                },
                None,
                Some(&mut texture),
            )?;
        }
        let texture = texture.context("missing generated GPU texture")?;
        unsafe {
            let context = self.device.GetImmediateContext()?;
            let clear: ID3D11DeviceContext1 = context.cast()?;
            let mut view = None;
            self.device
                .CreateRenderTargetView(&texture, None, Some(&mut view))?;
            let view = view.context("missing generated render target")?;
            context.ClearRenderTargetView(&view, &[0.0, 0.0, 0.0, 1.0]);
            let colors = [
                [0.0, 0.0, 0.0, 1.0],
                [1.0, 1.0, 1.0, 1.0],
                [254.0 / 255.0, 0.0, 0.0, 1.0],
                [0.0, 1.0, 1.0 / 255.0, 1.0],
            ];
            let offset = (u64::from(index) * 2 % u64::from(width)) as u32;
            for bar in 1..4u32 {
                let start = (bar * width + 3) / 4;
                let end = ((bar + 1) * width + 3) / 4;
                if start == end {
                    continue;
                }
                let left = (start + width - offset) % width;
                let right = left + end - start;
                let rect = RECT {
                    left: left as i32,
                    top: 0,
                    right: right.min(width) as i32,
                    bottom: height as i32,
                };
                clear.ClearView(&view, &colors[bar as usize], Some(&[rect]));
                if right > width {
                    clear.ClearView(
                        &view,
                        &colors[bar as usize],
                        Some(&[RECT {
                            left: 0,
                            top: 0,
                            right: (right - width) as i32,
                            bottom: height as i32,
                        }]),
                    );
                }
            }
            context.Flush();
        }
        if let Some(pooled) = pooled {
            if pooled {
                let mut pool = self.pool.borrow_mut();
                if pool.is_none() {
                    *pool = Some(uu_media::encoder::CapturePool::new(
                        self.input_device()?,
                        width,
                        height,
                    )?);
                }
                let frame = pool
                    .as_mut()
                    .unwrap()
                    .capture(&texture, pts)?
                    .context("all generated capture leases remain held")?;
                *self.last.borrow_mut() = Some(frame.clone());
                return Ok(frame);
            }
            let mut fresh =
                uu_media::encoder::CapturePool::new(self.input_device()?, width, height)?;
            let frame = fresh
                .capture(&texture, pts)?
                .context("fresh generated snapshot unavailable")?;
            *self.last.borrow_mut() = Some(frame.clone());
            return Ok(frame);
        }
        let surface = Frame::captured(texture, self.device.clone()).map_err(anyhow::Error::msg)?;
        let frame = RawFrame {
            width,
            height,
            stride: width * 4,
            format: PixelFormat::Bgra8,
            data: Default::default(),
            pts,
            gpu: true,
            dma_buf: None,
            d3d11: Some(std::sync::Arc::new(surface)),
        };
        ensure!(frame.is_consistent(), "invalid generated GPU frame");
        Ok(frame)
    }
}
