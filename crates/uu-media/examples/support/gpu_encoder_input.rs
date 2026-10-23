//! Generates D3D11 input without reading a desktop. Upload is outside encode timing.
use anyhow::{ensure,Context,Result};
use uu_media::{RawFrame,PixelFormat,d3d11_frame::Frame};
use windows::{core::Interface,Win32::{Foundation::HMODULE,Graphics::{Direct3D::*,Direct3D10::ID3D10Multithread,Direct3D11::*,Dxgi::{IDXGIAdapter,Common::*}}}};
pub struct Upload {device:ID3D11Device}
impl Upload {
    pub fn new()->Result<Self> {
        let (mut device,mut context)=(None,None);
        unsafe{D3D11CreateDevice(None::<&IDXGIAdapter>,D3D_DRIVER_TYPE_HARDWARE,HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT|D3D11_CREATE_DEVICE_BGRA_SUPPORT,Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,Some(&mut device),None,Some(&mut context))?;}
        let context=context.context("missing D3D11 context")?;
        let multithread:ID3D10Multithread=context.cast()?;
        unsafe{let _=multithread.SetMultithreadProtected(true);}
        Ok(Self{device:device.context("missing D3D11 device")?})
    }
    pub fn frame(&self,frame:&RawFrame)->Result<RawFrame> {
        ensure!(frame.format==PixelFormat::Bgra8 && !frame.gpu && frame.is_consistent(),"fixture needs CPU BGRA pixels");
        let mut texture=None;
        unsafe{self.device.CreateTexture2D(&D3D11_TEXTURE2D_DESC{
            Width:frame.width,Height:frame.height,MipLevels:1,ArraySize:1,Format:DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc:DXGI_SAMPLE_DESC{Count:1,Quality:0},Usage:D3D11_USAGE_DEFAULT,
            BindFlags:(D3D11_BIND_RENDER_TARGET|D3D11_BIND_SHADER_RESOURCE).0 as u32,..Default::default()
        },Some(&D3D11_SUBRESOURCE_DATA{pSysMem:frame.data.as_ptr().cast(),SysMemPitch:frame.stride,SysMemSlicePitch:0}),Some(&mut texture))?;}
        let surface=Frame::captured(texture.context("missing fixture texture")?,self.device.clone()).map_err(anyhow::Error::msg)?;
        let output=RawFrame{data:Default::default(),gpu:true,d3d11:Some(std::sync::Arc::new(surface)),..frame.clone()};
        ensure!(output.is_consistent(),"invalid GPU fixture frame");
        Ok(output)
    }
}
