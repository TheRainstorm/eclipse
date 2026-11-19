//! Feed Intel-owned oneVPL surfaces on the capture device's protected immediate context.
use super::{gpu, mf, upload::upload_frame, InputDevice, Settings};
use crate::RawFrame;
use std::ffi::c_void;
use windows::{
    core::{IUnknown, Interface},
    Win32::Graphics::{Direct3D11::*, Dxgi::Common::DXGI_FORMAT_NV12},
};

pub(super) struct Input {
    device: InputDevice,
    identity: IUnknown,
    context: ID3D11DeviceContext,
    conversion: Option<(gpu::Input, gpu::ConversionTarget)>,
    neutral: Option<ID3D11Texture2D>,
    upload: Vec<u8>,
    allocation: [u32; 2],
    _runtime: mf::Runtime,
}

impl Input {
    pub fn new(device: InputDevice) -> Result<Self, String> {
        let runtime = mf::Runtime::start()?;
        let identity = device.device.cast().map_err(|e| e.to_string())?;
        let context = unsafe { device.device.GetImmediateContext() }.map_err(|e| e.to_string())?;
        Ok(Self {
            device,
            identity,
            context,
            conversion: None,
            neutral: None,
            allocation: [0, 0],
            upload: Vec::new(),
            _runtime: runtime,
        })
    }

    pub fn device_ptr(&self) -> *mut c_void {
        self.device.device.as_raw()
    }

    /// The VPL frame owns this resource. Clone the COM reference only for this call.
    pub fn texture(
        &self,
        raw: *mut c_void,
        settings: Settings,
    ) -> Result<(ID3D11Texture2D, [u32; 2]), String> {
        let texture = unsafe { ID3D11Texture2D::from_raw_borrowed(&raw) }
            .ok_or("oneVPL returned no D3D11 surface")?;
        let owner = unsafe { texture.GetDevice() }.map_err(|e| e.to_string())?;
        let owner: IUnknown = owner.cast().map_err(|e| e.to_string())?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe {
            texture.GetDesc(&mut desc);
        }
        if owner != self.identity
            || desc.Format != DXGI_FORMAT_NV12
            || desc.ArraySize != 1
            || desc.MipLevels != 1
            || desc.SampleDesc.Count != 1
            || desc.Width < settings.width
            || desc.Height < settings.height
            || desc.Width > 8192
            || desc.Height > 8192
            || (desc.Width | desc.Height) & 1 != 0
        {
            return Err(
                "oneVPL input requires a single NV12 texture on the bound capture device".into(),
            );
        }
        Ok((texture.clone(), [desc.Width, desc.Height]))
    }

    pub fn write(
        &mut self,
        raw: *mut c_void,
        frame: &RawFrame,
        settings: Settings,
    ) -> Result<(), String> {
        let (dest, size) = self.texture(raw, settings)?;
        if self.allocation != size {
            self.allocation = size;
            self.neutral = None;
        }
        let [width, height] = size;
        let y_bytes = width as usize * height as usize;
        if !frame.gpu {
            // Desktop fallback can return a CPU frame within an established GPU session.
            self.upload.resize(y_bytes * 3 / 2, 0);
            upload_frame(
                frame,
                (width, height),
                [width, width, 0],
                [0, width * height, 0],
                &mut self.upload,
            )?;
            unsafe {
                self.context.UpdateSubresource(
                    &dest,
                    0,
                    None,
                    self.upload.as_ptr().cast(),
                    width,
                    0,
                );
            }
            return Ok(());
        }

        if self.conversion.is_none() {
            let converter = gpu::Input::new(frame, settings)?;
            if converter.device_identity()? != self.device.identity().map_err(|e| e.to_string())? {
                return Err(
                    "oneVPL cannot import a capture frame from a different D3D11 device".into(),
                );
            }
            let target = converter.target()?;
            self.conversion = Some((converter, target));
        }
        let (converter, target) = self.conversion.as_ref().unwrap();
        let visible = converter.convert_into(frame, target)?;
        unsafe {
            if size == [settings.width, settings.height] {
                self.context.CopyResource(&dest, &visible);
            } else {
                // Each allocator surface can be reused. Initialize all coded padding
                // before copying the visible region, including both NV12 planes.
                if self.neutral.is_none() {
                    let mut data = vec![128u8; y_bytes * 3 / 2];
                    data[..y_bytes].fill(16);
                    let mut desc = D3D11_TEXTURE2D_DESC::default();
                    dest.GetDesc(&mut desc);
                    desc.Usage = D3D11_USAGE_DEFAULT;
                    desc.BindFlags = 0;
                    desc.CPUAccessFlags = 0;
                    desc.MiscFlags = 0;
                    let init = D3D11_SUBRESOURCE_DATA {
                        pSysMem: data.as_ptr().cast(),
                        SysMemPitch: width,
                        SysMemSlicePitch: 0,
                    };
                    self.device
                        .device
                        .CreateTexture2D(&desc, Some(&init), Some(&mut self.neutral))
                        .map_err(|e| format!("oneVPL padded {width}x{height} NV12 texture: {e}"))?;
                }
                let neutral = self
                    .neutral
                    .as_ref()
                    .ok_or("missing oneVPL padding texture")?;
                self.context.CopyResource(&dest, neutral);
                self.context
                    .CopySubresourceRegion(&dest, 0, 0, 0, 0, &visible, 0, None);
            }
        }
        // The following oneVPL submission and the next reuse of our conversion
        // target are ordered on this same context. oneVPL retains its own surface.
        Ok(())
    }
}
