//! Two immutable capture snapshots. Only a slot with no frame leases may be
//! overwritten; saturation drops a new capture rather than growing VRAM use.
use std::sync::Arc;
use windows::{core::Interface, Win32::Graphics::{Direct3D11::*, Dxgi::Common::*}};
use crate::{d3d11_frame::Frame, CodecError, PixelFormat, RawFrame, Timestamp};
use super::InputDevice;

pub struct CapturePool {
    input: InputDevice,
    context: ID3D11DeviceContext,
    size: [u32; 2],
    slots: Vec<Arc<Frame>>,
    limit:usize,
}
impl CapturePool {
    pub fn new(input: InputDevice, width: u32, height: u32) -> Result<Self, CodecError> {
        Self::with_limit(input,width,height,2)
    }
    /// One encoding, one pending, one producer snapshot; immutable leases still gate reuse.
    pub fn with_limit(input:InputDevice,width:u32,height:u32,limit:usize)->Result<Self,CodecError> {
        if !(2..=3).contains(&limit){return Err(super::error("capture pool limit must be 2..3"));}
        if !(2..=8192).contains(&width) || !(2..=8192).contains(&height) {
            return Err(super::error("capture pool dimensions must be 2..8192"));
        }
        let context = unsafe { input.device.GetImmediateContext() }
            .map_err(|e| super::error(e.to_string()))?;
        Ok(Self { input, context, size: [width, height], slots: Vec::with_capacity(limit), limit })
    }

    /// Snapshot an acquired BGRA resource on this device's protected immediate
    /// context. All consumers enqueue their source reads on that same context;
    /// the next copy follows those reads in GPU order. A live Frame lease also
    /// prevents CPU-side reuse. No desktop resource survives ReleaseFrame.
    pub fn capture(&mut self, source: &ID3D11Texture2D, pts: Timestamp) -> Result<Option<RawFrame>, CodecError> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { source.GetDesc(&mut desc); }
        let actual = unsafe { source.GetDevice() }.map_err(|e|super::error(e.to_string()))?;
        if actual.as_raw() != self.input.device.as_raw()
            || [desc.Width, desc.Height] != self.size || !matches!(desc.Format,DXGI_FORMAT_B8G8R8A8_UNORM|DXGI_FORMAT_R16G16B16A16_FLOAT)
            || desc.ArraySize != 1 || desc.MipLevels != 1 || desc.SampleDesc.Count != 1 {
            return Err(super::error("capture pool source/device layout changed"));
        }
        if self.slots.first().is_some_and(|slot|slot.capture_texture().ok().is_some_and(|(old,_)|{let mut before=D3D11_TEXTURE2D_DESC::default();unsafe{old.GetDesc(&mut before);}before.Format!=desc.Format})) {
            self.slots.clear(); // Old frames retain their own immutable leases.
        }
        let free = self.slots.iter_mut().position(|slot| Arc::get_mut(slot).is_some());
        let index = match free {
            Some(index) => index,
            None if self.slots.len() < self.limit => {
                let owned_desc = D3D11_TEXTURE2D_DESC {
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
                    CPUAccessFlags: 0, MiscFlags: 0, ..desc
                };
                let mut texture = None;
                unsafe { self.input.device.CreateTexture2D(&owned_desc, None, Some(&mut texture)) }
                    .map_err(|e| super::error(e.to_string()))?;
                let texture = texture.ok_or_else(||super::error("capture pool texture unavailable"))?;
                let snapshot = Frame::captured(texture, self.input.device.clone()).map_err(super::error)?;
                self.slots.push(Arc::new(snapshot));
                self.slots.len() - 1
            }
            None => return Ok(None),
        };
        let snapshot = Arc::get_mut(&mut self.slots[index]).expect("capture slot exclusively owned");
        let (texture, _) = snapshot.capture_texture().map_err(super::error)?;
        unsafe {
            self.context.CopyResource(texture, source);
            self.context.Flush();
        }
        Ok(Some(RawFrame {
            width: self.size[0], height: self.size[1], stride: self.size[0] * if desc.Format==DXGI_FORMAT_R16G16B16A16_FLOAT{8}else{4},
            format: if desc.Format==DXGI_FORMAT_R16G16B16A16_FLOAT{PixelFormat::ScRgb16}else{PixelFormat::Bgra8}, data: Default::default(), pts, gpu: true,
            dma_buf: None, d3d11: Some(self.slots[index].clone()),
        }))
    }
}

/// Read a few immutable capture pixels for offline correctness probes. This
/// function exposes no mutable texture/device handle and is never called by
/// the media path. Points use the underlying physical capture coordinates.
pub fn capture_rgb_samples(frame:&RawFrame,points:&[(usize,usize)],white:f32)->Result<Vec<[u8;3]>,String> {
    let surface=frame.d3d11.as_ref().ok_or("missing capture texture")?;
    let(texture,device)=surface.capture_texture()?;
    let mut desc=D3D11_TEXTURE2D_DESC::default();unsafe{texture.GetDesc(&mut desc);}
    if !matches!(desc.Format,DXGI_FORMAT_B8G8R8A8_UNORM|DXGI_FORMAT_R16G16B16A16_FLOAT)
        || points.len()>64 || points.iter().any(|&(x,y)|x>=desc.Width as usize||y>=desc.Height as usize)
        || !(40.0..=1000.0).contains(&white) {return Err("invalid capture readback points/layout/white".into());}
    desc.Usage=D3D11_USAGE_STAGING;desc.BindFlags=0;desc.CPUAccessFlags=D3D11_CPU_ACCESS_READ.0 as u32;desc.MiscFlags=0;
    let mut staging=None;unsafe{device.CreateTexture2D(&desc,None,Some(&mut staging)).map_err(|e|e.to_string())?;}
    let staging=staging.ok_or("missing capture staging texture")?;
    let context=unsafe{device.GetImmediateContext().map_err(|e|e.to_string())?};
    let mut mapped=D3D11_MAPPED_SUBRESOURCE::default();
    unsafe{context.CopyResource(&staging,texture);context.Map(&staging,0,D3D11_MAP_READ,0,Some(&mut mapped)).map_err(|e|e.to_string())?;}
    if mapped.pData.is_null(){unsafe{context.Unmap(&staging,0);}return Err("capture staging pixels absent".into());}
    let values=points.iter().map(|&(x,y)|{
        let ptr=unsafe{mapped.pData.cast::<u8>().add(y*mapped.RowPitch as usize+x*if desc.Format==DXGI_FORMAT_R16G16B16A16_FLOAT{8}else{4})};
        if desc.Format==DXGI_FORMAT_R16G16B16A16_FLOAT {
            let rgb=std::array::from_fn(|i|half(unsafe{std::ptr::read_unaligned(ptr.add(i*2).cast::<u16>())}));
            crate::hdr::sc_rgb_to_sdr(rgb,white).map(|v|(v*255.).round() as u8)
        }else{unsafe{[*ptr.add(2),*ptr.add(1),*ptr]}}
    }).collect();unsafe{context.Unmap(&staging,0);}Ok(values)
}
fn half(v:u16)->f32 {let sign=if v&0x8000!=0{-1.}else{1.};let e=(v>>10)&31;let m=v&1023;sign*match e{0=>f32::from(m)*2_f32.powi(-24),31=>0.,_=>(1.+f32::from(m)/1024.)*2_f32.powi(i32::from(e)-15)}}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::{Foundation::HMODULE, Graphics::Direct3D::D3D_DRIVER_TYPE_WARP};
    fn device() -> ID3D11Device {
        let mut device = None;
        unsafe { D3D11CreateDevice(None, D3D_DRIVER_TYPE_WARP, HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION,
            Some(&mut device), None, None).unwrap(); }
        device.unwrap()
    }
    fn source(device: &ID3D11Device) -> (ID3D11Texture2D, ID3D11RenderTargetView) {
        let mut texture = None;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: 4, Height: 4, MipLevels: 1, ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            ..Default::default()
        };
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)).unwrap(); }
        let texture = texture.unwrap();
        let mut view = None;
        unsafe { device.CreateRenderTargetView(&texture, None, Some(&mut view)).unwrap(); }
        (texture, view.unwrap())
    }
    fn pixel(device: &ID3D11Device, frame: &RawFrame) -> [u8; 4] {
        let (texture, _) = frame.d3d11.as_ref().unwrap().capture_texture().unwrap();
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc); }
        desc.Usage = D3D11_USAGE_STAGING; desc.BindFlags = 0; desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        let mut staging = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut staging)).unwrap(); }
        let staging = staging.unwrap();
        let context = unsafe { device.GetImmediateContext().unwrap() };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            context.CopyResource(&staging, texture);
            context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).unwrap();
            let mut pixel = [0;4];
            pixel.copy_from_slice(std::slice::from_raw_parts(mapped.pData.cast::<u8>(), 4));
            context.Unmap(&staging, 0);
            pixel
        }
    }
    #[test]
    fn capture_leases_preserve_pixels_and_bound_slots_across_reuse_and_reset() {
        let device = device();
        let input = InputDevice::from_d3d11(device.clone()).unwrap();
        let context = unsafe { device.GetImmediateContext().unwrap() };
        let (source, view) = source(&device);
        let mut pool = CapturePool::new(input.clone(), 4, 4).unwrap();
        unsafe { context.ClearRenderTargetView(&view, &[1.,0.,0.,1.]); }
        let red = pool.capture(&source, Timestamp(0)).unwrap().unwrap();
        unsafe { context.ClearRenderTargetView(&view, &[0.,0.,1.,1.]); }
        let blue = pool.capture(&source, Timestamp(1)).unwrap().unwrap();
        assert!(pool.capture(&source, Timestamp(2)).unwrap().is_none());
        assert_eq!(pool.slots.len(), 2);
        assert_eq!(pixel(&device,&red), [0,0,255,255]);
        let first = red.d3d11.as_ref().unwrap().capture_texture().unwrap().0.as_raw();
        let cropped = red.clone().crop_decoded([2,2]).unwrap();
        drop(red);
        unsafe { context.ClearRenderTargetView(&view, &[0.,1.,0.,1.]); }
        assert!(pool.capture(&source, Timestamp(3)).unwrap().is_none(), "cropped views retain the original pool lease");
        assert_eq!(pixel(&device,&cropped), [0,0,255,255]);
        drop(cropped);
        let green = pool.capture(&source, Timestamp(3)).unwrap().unwrap();
        assert_eq!(green.d3d11.as_ref().unwrap().capture_texture().unwrap().0.as_raw(), first);
        assert_eq!(pixel(&device,&blue), [255,0,0,255]);
        assert_eq!(pixel(&device,&green), [0,255,0,255]);
        let held = green.clone(); drop(green);
        assert!(pool.capture(&source, Timestamp(4)).unwrap().is_none());
        let weak = Arc::downgrade(blue.d3d11.as_ref().unwrap());
        drop(blue);
        assert!(pool.capture(&source, Timestamp(4)).unwrap().is_none(), "a weak lease may still upgrade to the old snapshot");
        assert!(weak.upgrade().is_some());
        drop(weak);
        drop(pool);
        let mut replacement = CapturePool::new(input, 4, 4).unwrap();
        unsafe { context.ClearRenderTargetView(&view, &[1.,1.,1.,1.]); }
        let white = replacement.capture(&source, Timestamp(5)).unwrap().unwrap();
        assert_eq!(pixel(&device,&held), [0,255,0,255]);
        assert_eq!(pixel(&device,&white), [255,255,255,255]);
    }
    #[test]
    fn capture_pool_rejects_other_devices_and_layouts_without_mutating_leases() {
        let device = device();
        let mut pool = CapturePool::new(InputDevice::from_d3d11(device.clone()).unwrap(), 4, 4).unwrap();
        let (local, view) = source(&device);
        unsafe { device.GetImmediateContext().unwrap().ClearRenderTargetView(&view, &[1.,0.,0.,1.]); }
        let held = pool.capture(&local, Timestamp(0)).unwrap().unwrap();
        let other = super::tests::device(); let (foreign, _) = source(&other);
        assert!(pool.capture(&foreign, Timestamp(0)).is_err());
        let mut wrong = CapturePool::new(InputDevice::from_d3d11(device.clone()).unwrap(), 8, 4).unwrap();
        assert!(wrong.capture(&local, Timestamp(0)).is_err());
        assert_eq!(pixel(&device,&held), [0,0,255,255]);
        assert!(CapturePool::new(InputDevice::from_d3d11(device).unwrap(), 8193, 4).is_err());
        assert_eq!(pool.slots.len(), 1);
    }
}

#[cfg(test)]
mod overlap_pool_tests {
    use super::*;
    use windows::Win32::{Foundation::HMODULE,Graphics::Direct3D::D3D_DRIVER_TYPE_WARP};
    #[test]
    fn three_snapshot_overlap_limit_preserves_every_live_gpu_lease(){
        let mut device=None;unsafe{D3D11CreateDevice(None,D3D_DRIVER_TYPE_WARP,HMODULE::default(),D3D11_CREATE_DEVICE_BGRA_SUPPORT,None,D3D11_SDK_VERSION,Some(&mut device),None,None).unwrap();}
        let device=device.unwrap();let context=unsafe{device.GetImmediateContext().unwrap()};let mut texture=None;let mut view=None;
        let desc=D3D11_TEXTURE2D_DESC{Width:4,Height:4,MipLevels:1,ArraySize:1,Format:DXGI_FORMAT_B8G8R8A8_UNORM,SampleDesc:DXGI_SAMPLE_DESC{Count:1,Quality:0},Usage:D3D11_USAGE_DEFAULT,BindFlags:D3D11_BIND_RENDER_TARGET.0 as u32,..Default::default()};
        unsafe{device.CreateTexture2D(&desc,None,Some(&mut texture)).unwrap();device.CreateRenderTargetView(texture.as_ref().unwrap(),None,Some(&mut view)).unwrap();}
        let texture=texture.unwrap();let view=view.unwrap();let mut pool=CapturePool::with_limit(InputDevice::from_d3d11(device).unwrap(),4,4,3).unwrap();let mut held=Vec::new();
        for (i,color) in [[1.,0.,0.,1.],[0.,1.,0.,1.],[0.,0.,1.,1.]].into_iter().enumerate(){unsafe{context.ClearRenderTargetView(&view,&color);}held.push(pool.capture(&texture,Timestamp(i as u64)).unwrap().unwrap());}
        assert!(pool.capture(&texture,Timestamp(4)).unwrap().is_none());assert_eq!(pool.slots.len(),3);
        for (frame,expected) in held.iter().zip([[255,0,0],[0,255,0],[0,0,255]]){assert_eq!(capture_rgb_samples(frame,&[(1,1)],203.).unwrap(),vec![expected]);}
        held.remove(1);unsafe{context.ClearRenderTargetView(&view,&[1.,1.,0.,1.]);}let yellow=pool.capture(&texture,Timestamp(5)).unwrap().unwrap();assert_eq!(pool.slots.len(),3);assert_eq!(capture_rgb_samples(&yellow,&[(1,1)],203.).unwrap(),vec![[255,255,0]]);
        assert_eq!(capture_rgb_samples(&held[0],&[(1,1)],203.).unwrap(),vec![[255,0,0]]);assert_eq!(capture_rgb_samples(&held[1],&[(1,1)],203.).unwrap(),vec![[0,0,255]]);
    }
}
