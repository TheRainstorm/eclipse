//! Intel VPL AV1 hardware decode, with explicit CPU NV12 output. Driver objects
//! and borrowed surfaces remain on one worker thread; only copied pixels leave.
use crate::{CodecError, EncodedFrame, PixelFormat, RawFrame, Timestamp, VideoDecoder};
use std::{
    collections::VecDeque,
    ffi::{c_char, c_void, CStr},
    ptr::NonNull,
};
use uu_proto::media::VideoCodec;
extern "C" {
    fn eclipse_vpl_decoder_open(out: *mut *mut c_void, error: *mut c_char, capacity: usize) -> i32;
    fn eclipse_vpl_decoder_prepare(
        decoder: *mut c_void,
        width: u32,
        height: u32,
        error: *mut c_char,
        capacity: usize,
    ) -> i32;
    fn eclipse_vpl_decoder_decode(
        decoder: *mut c_void,
        input: *const u8,
        length: usize,
        timestamp: u64,
        y: *mut *const u8,
        uv: *mut *const u8,
        pitch: *mut u32,
        width: *mut u32,
        height: *mut u32,
        pts: *mut u64,
        error: *mut c_char,
        capacity: usize,
    ) -> i32;
    fn eclipse_vpl_decoder_close(decoder: *mut c_void);
}
fn message(error: &[c_char]) -> String {
    unsafe { CStr::from_ptr(error.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}
pub struct VplAv1Decoder {
    worker: super::Worker,
}
impl VplAv1Decoder {
    pub fn new() -> Result<Self, CodecError> {
        Ok(Self {
            worker: super::Worker::spawn(Driver::open)?,
        })
    }
}
impl VideoDecoder for VplAv1Decoder {
    fn codec(&self) -> VideoCodec {
        VideoCodec::Av1
    }
    fn backend_name(&self) -> &str {
        self.worker.backend_name()
    }
    fn prepare(&mut self, w: u32, h: u32) -> Result<(), CodecError> {
        self.worker.prepare(w, h)
    }
    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<RawFrame>, CodecError> {
        self.worker.decode(frame)
    }
    fn decode_frames(&mut self, frame: &EncodedFrame) -> Result<Vec<RawFrame>, CodecError> {
        self.worker.decode_frames(frame)
    }
}
struct Driver {
    decoder: NonNull<c_void>,
    pending: VecDeque<(u64, Timestamp)>,
    pixels: super::pixels::Pool,
}
impl Driver {
    fn open() -> Result<Self, String> {
        let mut pointer = std::ptr::null_mut();
        let mut error = [0 as c_char; 512];
        if unsafe { eclipse_vpl_decoder_open(&mut pointer, error.as_mut_ptr(), error.len()) } < 0 {
            return Err(message(&error));
        }
        Ok(Self {
            decoder: NonNull::new(pointer).ok_or("Intel decoder returned no session")?,
            pending: VecDeque::new(),
            pixels: Default::default(),
        })
    }
}
impl Drop for Driver {
    fn drop(&mut self) {
        unsafe {
            eclipse_vpl_decoder_close(self.decoder.as_ptr());
        }
    }
}
impl super::Backend for Driver {
    fn backend_name(&self) -> &str {
        "intel-vpl (AV1 hw, CPU NV12 output)"
    }
    fn prepare(&mut self, w: u32, h: u32) -> Result<(), String> {
        let mut error = [0 as c_char; 512];
        if unsafe {
            eclipse_vpl_decoder_prepare(
                self.decoder.as_ptr(),
                w,
                h,
                error.as_mut_ptr(),
                error.len(),
            )
        } < 0
        {
            return Err(message(&error));
        }
        Ok(())
    }
    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<RawFrame>, String> {
        if frame.data.is_empty() || frame.data.len() > 16 * 1024 * 1024 {
            return Err("invalid AV1 access unit".into());
        }
        if self.pending.len() >= 32 {
            return Err("Intel decoder has too many unreturned input timestamps".into());
        }
        let tag = frame.pts.0.saturating_mul(90_000) / 1_000_000;
        self.pending.push_back((tag, frame.pts));
        let (mut y, mut uv, mut pitch, mut width, mut height, mut pts) =
            (std::ptr::null(), std::ptr::null(), 0, 0, 0, 0);
        let mut error = [0 as c_char; 512];
        let status = unsafe {
            eclipse_vpl_decoder_decode(
                self.decoder.as_ptr(),
                frame.data.as_ptr(),
                frame.data.len(),
                tag,
                &mut y,
                &mut uv,
                &mut pitch,
                &mut width,
                &mut height,
                &mut pts,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if status < 0 {
            return Err(message(&error));
        }
        if status == 0 {
            return Ok(None);
        }
        if y.is_null()
            || uv.is_null()
            || width < 2
            || height < 2
            || width > 8192
            || height > 8192
            || width % 2 != 0
            || height % 2 != 0
            || pitch < width
            || pitch > 8192
        {
            return Err("invalid Intel decoded surface".into());
        }
        let size = width as usize * height as usize * 3 / 2;
        if size > 64 * 1024 * 1024 {
            return Err("Intel output exceeds pixel budget".into());
        }
        let index = self
            .pending
            .iter()
            .position(|(tag, _)| *tag == pts)
            .ok_or("Intel decoder returned an unknown timestamp")?;
        let (_, pts) = self.pending.remove(index).unwrap();
        let mut pixels = self.pixels.take(size);
        let out = pixels.as_mut();
        // Native bounds and ownership were checked before exposing these two
        // borrowed pointers; synchronization completed before either read.
        for row in 0..height as usize {
            let source =
                unsafe { std::slice::from_raw_parts(y.add(row * pitch as usize), width as usize) };
            out[row * width as usize..(row + 1) * width as usize].copy_from_slice(source);
        }
        let offset = width as usize * height as usize;
        for row in 0..height as usize / 2 {
            let source =
                unsafe { std::slice::from_raw_parts(uv.add(row * pitch as usize), width as usize) };
            out[offset + row * width as usize..offset + (row + 1) * width as usize]
                .copy_from_slice(source);
        }
        Ok(Some(RawFrame {
            width,
            height,
            stride: width,
            format: PixelFormat::Nv12,
            data: bytes::Bytes::from_owner(pixels),
            pts,
            gpu: false,
            dma_buf: None,
            d3d11: None,
        }))
    }
}
