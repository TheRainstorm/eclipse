use crate::color::YuvColor;
use std::{ffi::c_void, mem::ManuallyDrop};
use windows::{
    core::Interface,
    Win32::{
        Foundation::RECT,
        Graphics::{Direct3D11::*, Dxgi::Common::*},
        Media::MediaFoundation::IMFSample,
    },
};

#[derive(Debug)]
pub struct Frame {
    // A cropped view retains the original lease, including capture-pool
    // exclusivity and decoder sample ownership.
    _parent: Option<std::sync::Arc<Frame>>,
    // A COM reference to the texture alone does not prevent MFT sample reuse.
    _sample: Option<IMFSample>,
    _foundation: Option<std::sync::Arc<Foundation>>,
    texture: ID3D11Texture2D,
    device: ID3D11Device,
    index: u32,
    visible: [u32; 2],
    allocation: [u32; 2],
    format: DXGI_FORMAT,
}
// Shared frame leases are immutable. MFT samples retain their decoder slots;
// capture snapshots may be updated only after exclusive ownership is recovered.
// Native source reads and copies use the same protected immediate context, so
// queued GPU work remains ordered after its CPU lease is released.
unsafe impl Send for Frame {}
unsafe impl Sync for Frame {}
impl Frame {
    pub(crate) fn new(
        sample: IMFSample,
        texture: ID3D11Texture2D,
        index: u32,
        device: ID3D11Device,
        visible: [u32; 2],
        allocation: [u32; 2],
        foundation: std::sync::Arc<Foundation>,
    ) -> Result<Self, String> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe {
            texture.GetDesc(&mut desc);
        }
        if desc.MipLevels != 1
            || index >= desc.ArraySize
            || desc.Format != DXGI_FORMAT_NV12
            || desc.SampleDesc.Count != 1
        {
            return Err("unsupported D3D11 decoder texture layout".into());
        }
        let frame = Self {
            _parent: None,
            _sample: Some(sample),
            _foundation: Some(foundation),
            texture,
            device,
            index,
            visible,
            allocation,
            format:desc.Format,
        };
        if !frame.valid(visible) {
            return Err("invalid leased D3D11 surface".into());
        }
        Ok(frame)
    }
    pub fn valid(&self, visible: [u32; 2]) -> bool {
        (visible == self.visible || (self.format==DXGI_FORMAT_B8G8R8A8_UNORM
            && visible[0]<=self.visible[0] && visible[1]<=self.visible[1]))
            && visible.iter().all(|v| *v >= 2 && *v <= 8192 && *v % 2 == 0)
            && self.allocation.iter().all(|v| *v <= 8192)
            && self.allocation[0] >= visible[0]
            && self.allocation[1] >= visible[1]
    }
    pub fn device_ptr(&self) -> *mut c_void {
        self.device.as_raw()
    }
    /// The caller transfers an immutable snapshot for the duration of its frame
    /// leases. A capture pool may refresh it only with exclusive ownership and
    /// ordered GPU copies. The duplication surface must not escape ReleaseFrame.
    /// Shared devices must have ID3D10Multithread protection enabled.
    pub fn captured(texture:ID3D11Texture2D,device:ID3D11Device)->Result<Self,String> {
        let mut desc=D3D11_TEXTURE2D_DESC::default();
        unsafe{texture.GetDesc(&mut desc);}
        if desc.Format!=DXGI_FORMAT_B8G8R8A8_UNORM || desc.ArraySize!=1
            || desc.MipLevels!=1 || desc.SampleDesc.Count!=1 {
            return Err("unsupported GPU capture surface".into());
        }
        let actual=unsafe{texture.GetDevice()}.map_err(|e|e.to_string())?;
        if actual.as_raw()!=device.as_raw(){return Err("capture texture/device mismatch".into());}
        Ok(Self{_parent:None,_sample:None,_foundation:None,texture,device,index:0,
            visible:[desc.Width,desc.Height],allocation:[desc.Width,desc.Height],format:desc.Format})
    }
    pub(crate) fn cropped_view(source:&std::sync::Arc<Self>,visible:[u32;2])->Result<Self,String> {
        if !source.valid(source.visible) || visible.iter().any(|v|*v<2 || *v%2!=0)
            || visible[0]>source.visible[0] || visible[1]>source.visible[1] {
            return Err("invalid decoded GPU display geometry".into());
        }
        Ok(Self{_parent:Some(source.clone()),_sample:None,_foundation:None,
            texture:source.texture.clone(),device:source.device.clone(),index:source.index,
            visible,allocation:source.allocation,format:source.format})
    }
    pub(crate) fn capture_texture(&self)->Result<(&ID3D11Texture2D,&ID3D11Device),String> {
        if self.format!=DXGI_FORMAT_B8G8R8A8_UNORM {return Err("encoder requires a BGRA capture surface".into());}
        Ok((&self.texture,&self.device))
    }
}

/// One reusable RGB output surface on the decoder's device. Called only by the
/// renderer thread, with the GL interop object unlocked during D3D operations.
/// The caller keeps the input Frame leased through GPU synchronization.
pub struct Converter {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    output: ID3D11Texture2D,
    output_view: ID3D11VideoProcessorOutputView,
    completion: ID3D11Query,
    size: [u32; 2],
    allocation: [u32; 2],
}
impl Converter {
    pub fn new(frame: &Frame) -> Result<Self, String> {
        Self::with_shared_export(frame, false)
    }
    /// WGL interop2 needs DEFAULT usage, not a DXGI shared-handle allocation.
    /// Some drivers may still require exportable storage when registering it.
    pub fn with_shared_export(frame: &Frame, shared: bool) -> Result<Self, String> {
        Self::create(frame, shared).map_err(|e| format!("D3D11 video processor: {e}"))
    }
    fn create(frame: &Frame, shared: bool) -> windows::core::Result<Self> {
        unsafe {
            let device = frame.device.clone();
            let context = device.GetImmediateContext()?;
            let video: ID3D11VideoDevice = device.cast()?;
            let video_context: ID3D11VideoContext = context.cast()?;
            let size = frame.visible;
            let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL {
                    Numerator: 60,
                    Denominator: 1,
                },
                InputWidth: frame.allocation[0],
                InputHeight: frame.allocation[1],
                OutputFrameRate: DXGI_RATIONAL {
                    Numerator: 60,
                    Denominator: 1,
                },
                OutputWidth: size[0],
                OutputHeight: size[1],
                Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
            };
            let enumerator = video.CreateVideoProcessorEnumerator(&desc)?;
            let processor = video.CreateVideoProcessor(&enumerator, 0)?;
            let mut output = None;
            device.CreateTexture2D(
                &D3D11_TEXTURE2D_DESC {
                    Width: size[0],
                    Height: size[1],
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
                    MiscFlags: if shared {
                        D3D11_RESOURCE_MISC_SHARED.0 as u32
                    } else {
                        0
                    },
                    ..Default::default()
                },
                None,
                Some(&mut output),
            )?;
            let output = output.unwrap();
            let mut completion = None;
            device.CreateQuery(
                &D3D11_QUERY_DESC {
                    Query: D3D11_QUERY_EVENT,
                    MiscFlags: 0,
                },
                Some(&mut completion),
            )?;
            let desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };
            let mut output_view = None;
            video.CreateVideoProcessorOutputView(
                &output,
                &enumerator,
                &desc,
                Some(&mut output_view),
            )?;
            let rect = RECT {
                left: 0,
                top: 0,
                right: size[0] as i32,
                bottom: size[1] as i32,
            };
            video_context.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            video_context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&rect));
            video_context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&rect));
            video_context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&rect));
            // Full-range RGB output; no enhancement, deinterlacing or frame pacing.
            video_context.VideoProcessorSetOutputColorSpace(
                &processor,
                &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 },
            );
            Ok(Self {
                device,
                context,
                video,
                video_context,
                enumerator,
                processor,
                output,
                output_view: output_view.unwrap(),
                completion: completion.unwrap(),
                size,
                allocation: frame.allocation,
            })
        }
    }
    pub fn matches(&self, frame: &Frame) -> bool {
        self.device.as_raw() == frame.device.as_raw()
            && self.size == frame.visible
            && self.allocation == frame.allocation
    }
    pub fn device_ptr(&self) -> *mut c_void {
        self.device.as_raw()
    }
    pub fn texture_ptr(&self) -> *mut c_void {
        self.output.as_raw()
    }
    /// Queue GPU conversion. The caller must synchronize before releasing frame
    /// or accessing the output in another graphics API (WGL interop lock).
    pub fn convert(&self, frame: &Frame, color: YuvColor) -> Result<(), String> {
        unsafe {
            if !self.matches(frame) {
                return Err("D3D11 converter device/geometry mismatch".into());
            }
            let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: frame.index,
                    },
                },
            };
            let mut input = None;
            self.video
                .CreateVideoProcessorInputView(
                    &frame.texture,
                    &self.enumerator,
                    &desc,
                    Some(&mut input),
                )
                .map_err(|e| format!("video input view: {e}"))?;
            let matrix = matches!(color, YuvColor::Full709 | YuvColor::Limited709) as u32;
            let nominal = if matches!(color, YuvColor::Limited601 | YuvColor::Limited709) {
                D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_16_235
            } else {
                D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_0_255
            };
            // D3D11_VIDEO_PROCESSOR_COLOR_SPACE: YCbCr_Matrix bit2,
            // Nominal_Range bits4..5. Use SDK constants (1=limited, 2=full).
            self.video_context.VideoProcessorSetStreamColorSpace(
                &self.processor,
                0,
                &D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                    _bitfield: (matrix << 2) | ((nominal.0 as u32) << 4),
                },
            );
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(input),
                ..Default::default()
            };
            let result = self.video_context.VideoProcessorBlt(
                &self.processor,
                &self.output_view,
                0,
                std::slice::from_ref(&stream),
            );
            ManuallyDrop::drop(&mut stream.pInputSurface);
            self.context.Flush();
            result.map_err(|e| format!("video GPU conversion: {e}"))
        }
    }
    /// Wait without copying pixels, for offline timing and interop error cleanup.
    /// Normal presentation uses WGL synchronization instead.
    pub fn synchronize(&self) -> Result<(), String> {
        unsafe {
            // Older feature levels need a resource Map to include video work.
            if self.device.GetFeatureLevel().0
                < windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0.0
            {
                return self.readback().map(|_| ());
            }
            self.context.End(&self.completion);
            self.context.Flush();
            let started = std::time::Instant::now();
            loop {
                let mut complete = 0_i32;
                self.context
                    .GetData(
                        &self.completion,
                        Some((&mut complete as *mut i32).cast()),
                        4,
                        D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
                    )
                    .map_err(|e| e.to_string())?;
                if complete != 0 {
                    return Ok(());
                }
                if started.elapsed() > std::time::Duration::from_secs(5) {
                    return Err("D3D11 GPU completion timed out".into());
                }
                std::thread::yield_now();
            }
        }
    }
    /// CPU readback for offline correctness tests and synchronization on interop
    /// failure only. The normal rendering path never calls this method.
    pub fn readback(&self) -> Result<Vec<u8>, String> {
        unsafe {
            let mut staging = None;
            let desc = D3D11_TEXTURE2D_DESC {
                Width: self.size[0],
                Height: self.size[1],
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                ..Default::default()
            };
            self.device
                .CreateTexture2D(&desc, None, Some(&mut staging))
                .map_err(|e| e.to_string())?;
            let staging = staging.unwrap();
            self.context.CopyResource(&staging, &self.output);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| e.to_string())?;
            let row = self.size[0] as usize * 4;
            let mut data = vec![0; row * self.size[1] as usize];
            for y in 0..self.size[1] as usize {
                std::ptr::copy_nonoverlapping(
                    (mapped.pData as *const u8).add(y * mapped.RowPitch as usize),
                    data.as_mut_ptr().add(y * row),
                    row,
                );
            }
            self.context.Unmap(&staging, 0);
            Ok(data)
        }
    }
}

// MF lifetime is process-wide; a retained output sample may outlive decoder
// recreation. COM apartment teardown remains on the decoder worker thread.
#[derive(Debug)]
pub(crate) struct Foundation;
impl Foundation {
    pub(crate) fn start() -> windows::core::Result<std::sync::Arc<Self>> {
        unsafe {
            windows::Win32::Media::MediaFoundation::MFStartup(
                windows::Win32::Media::MediaFoundation::MF_VERSION,
                windows::Win32::Media::MediaFoundation::MFSTARTUP_FULL,
            )?;
        }
        Ok(std::sync::Arc::new(Self))
    }
}
impl Drop for Foundation {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Media::MediaFoundation::MFShutdown();
        }
    }
}
