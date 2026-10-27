//! Pinned PyroWave C API. Sessions are owned by the encoder/decoder worker;
//! decoded GPU slots remain leased until the renderer releases their frames.
use crate::{CodecError, EncodedFrame, RawFrame, VideoDecoder, VideoEncoder};
use std::{
    ffi::{c_char, c_void, CStr, CString},
    path::{Path, PathBuf},
    ptr::NonNull,
    sync::{Arc, Mutex},
};
use uu_proto::media::VideoCodec;

pub const MAX_FRAME: usize = 16 * 1024 * 1024;
pub const FMTP: &str = uu_proto::media::PYROWAVE_FMTP;
#[repr(C)]
struct Config {
    width: u32,
    height: u32,
    fps: u32,
    kbps: u32,
    encode: u32,
    gpu: u32,
}
unsafe extern "C" {
    fn eclipse_pw_create(
        library: *const c_char,
        info: *const Config,
        input: *mut c_void,
        error: *mut c_char,
        capacity: usize,
    ) -> *mut c_void;
    fn eclipse_pw_destroy(session: *mut c_void);
    fn eclipse_pw_set_rate(
        session: *mut c_void,
        kbps: u32,
        fps: u32,
        error: *mut c_char,
        capacity: usize,
    ) -> i32;
    fn eclipse_pw_encode(
        session: *mut c_void,
        pixels: *const c_void,
        size: usize,
        texture: *mut c_void,
        data: *mut *const u8,
        length: *mut usize,
        error: *mut c_char,
        capacity: usize,
    ) -> i32;
    fn eclipse_pw_decode(
        session: *mut c_void,
        data: *const u8,
        length: usize,
        pixels: *mut c_void,
        size: usize,
        slot: *mut i32,
        texture: *mut *mut c_void,
        error: *mut c_char,
        capacity: usize,
    ) -> i32;
    fn eclipse_pw_release(session: *mut c_void, slot: i32);
    fn eclipse_pw_stats(session: *mut c_void, text: *mut c_char, capacity: usize) -> i32;
}
fn fail(message: impl Into<String>) -> CodecError {
    CodecError::Backend(message.into())
}
fn checked(result: i32, error: &[c_char]) -> Result<(), CodecError> {
    if result == 0 {
        Ok(())
    } else {
        Err(fail(
            unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy(),
        ))
    }
}
pub fn runtime_path() -> Result<PathBuf, CodecError> {
    let executable = std::env::current_exe().map_err(|e| fail(e.to_string()))?;
    Ok(executable
        .parent()
        .ok_or_else(|| fail("missing installation directory"))?
        .join(if cfg!(windows) {
            "libpyrowave-shared-1.dll"
        } else {
            "libpyrowave-shared.so.1"
        }))
}
fn geometry(width: u32, height: u32) -> Result<(), CodecError> {
    if !(2..=8192).contains(&width)
        || !(2..=8192).contains(&height)
        || width % 2 != 0
        || height % 2 != 0
        || u64::from(width) * u64::from(height) * 3 / 2 > 64 * 1024 * 1024
    {
        return Err(fail("invalid PyroWave display geometry"));
    }
    Ok(())
}

/// Validate the fixed SDR/420 wire contract before either CPU code or a GPU
/// shader sees untrusted block indices, codewords or payload lengths.
pub fn frame_size(data: &[u8]) -> Result<[u32; 2], CodecError> {
    let invalid = || fail("invalid or unsupported PyroWave access unit");
    let [w, h] = sequence_header(data)?;
    let first = u32::from_le_bytes(data[..4].try_into().unwrap());
    let second = u32::from_le_bytes(data[4..8].try_into().unwrap());
    validate_blocks(data, w, h, first, second).map_err(|_| invalid())?;
    Ok([w, h])
}
/// Constant-time header check for locally encoded frames and stream routing.
/// The decoder additionally validates all block payloads with `frame_size`.
pub fn sequence_header(data: &[u8]) -> Result<[u32; 2], CodecError> {
    let invalid = || fail("invalid or unsupported PyroWave sequence header");
    if !(8..=MAX_FRAME).contains(&data.len()) || data.len() % 4 != 0 {
        return Err(invalid());
    }
    let first = u32::from_le_bytes(data[..4].try_into().unwrap());
    let second = u32::from_le_bytes(data[4..8].try_into().unwrap());
    if first >> 31 != 1 || second >> 24 != 0 {
        return Err(invalid());
    }
    let [w, h] = [(first & 0x3fff) + 1, ((first >> 14) & 0x3fff) + 1];
    geometry(w, h)?;
    Ok([w, h])
}
fn validate_blocks(data: &[u8], w: u32, h: u32, first: u32, second: u32) -> Result<(), ()> {
    let invalid = || ();
    let seq = (first >> 28) & 7;
    let (aw, ah) = (
        w.next_multiple_of(32).max(128),
        h.next_multiple_of(32).max(128),
    );
    let mut max_blocks = 0usize;
    for level in 0..5 {
        max_blocks += ((aw >> (level + 1)).div_ceil(32) * (ah >> (level + 1)).div_ceil(32))
            as usize
            * if level == 0 {
                3
            } else if level == 4 {
                12
            } else {
                9
            };
    }
    let count = (second & 0xffffff) as usize;
    if count > max_blocks || count > (data.len() - 8) / 8 {
        return Err(invalid());
    }
    let mut seen = vec![0u64; max_blocks.div_ceil(64)];
    let mut offset = 8;
    let mut blocks = 0;
    while offset < data.len() {
        let header = data.get(offset..offset + 8).ok_or_else(invalid)?;
        let ballot = u16::from_le_bytes(header[..2].try_into().unwrap());
        let flags = u16::from_le_bytes(header[2..4].try_into().unwrap());
        let index = (u32::from_le_bytes(header[4..8].try_into().unwrap()) >> 8) as usize;
        let bytes = usize::from(flags & 4095) * 4;
        if ballot == 0
            || flags >> 15 != 0
            || u32::from((flags >> 12) & 7) != seq
            || index >= max_blocks
            || bytes < 8
            || seen[index / 64] & (1 << (index % 64)) != 0
        {
            return Err(invalid());
        }
        seen[index / 64] |= 1 << (index % 64);
        let block = data.get(offset..offset + bytes).ok_or_else(invalid)?;
        let n = ballot.count_ones() as usize;
        let mut payload = 8 + 3 * n;
        if payload > bytes {
            return Err(invalid());
        }
        let mut sign_bits = 0usize;
        for i in 0..n {
            let code = u16::from_le_bytes(block[8 + 2 * i..10 + 2 * i].try_into().unwrap());
            let scale = block[8 + 2 * n + i] & 15;
            for sub in 0..8 {
                let planes = usize::from(scale + ((code >> (2 * sub)) & 3) as u8);
                let values = block.get(payload..payload + planes).ok_or_else(invalid)?;
                sign_bits += values.iter().fold(0u8, |a, b| a | b).count_ones() as usize;
                payload += planes;
            }
        }
        let used = payload + sign_bits.div_ceil(8);
        if used > bytes || bytes - used > 3 {
            return Err(invalid());
        }
        blocks += 1;
        offset += bytes;
    }
    if blocks != count {
        return Err(invalid());
    }
    Ok(())
}

struct Session(NonNull<c_void>);
// The C API is not thread safe. Every shared decoder access (including slot
// release from the renderer) is serialized by its Mutex; encoders are exclusive.
unsafe impl Send for Session {}
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PyroWaveSession")
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            eclipse_pw_destroy(self.0.as_ptr());
        }
    }
}
impl Session {
    fn open(runtime: &Path, info: &Config, input: *mut c_void) -> Result<Self, CodecError> {
        geometry(info.width, info.height)?;
        if !runtime.is_absolute() {
            return Err(fail("PyroWave runtime path must be absolute"));
        }
        let path = CString::new(
            runtime
                .to_str()
                .ok_or_else(|| fail("PyroWave runtime path is not UTF-8"))?,
        )
        .map_err(|e| fail(e.to_string()))?;
        let mut error = [0; 512];
        let raw = unsafe {
            eclipse_pw_create(path.as_ptr(), info, input, error.as_mut_ptr(), error.len())
        };
        NonNull::new(raw)
            .map(Self)
            .ok_or_else(|| fail(unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy()))
    }
    fn stats(&self) -> Result<String, CodecError> {
        let mut text = vec![0; 8193];
        checked(
            unsafe { eclipse_pw_stats(self.0.as_ptr(), text.as_mut_ptr(), text.len()) },
            &text,
        )?;
        Ok(unsafe { CStr::from_ptr(text.as_ptr()) }
            .to_string_lossy()
            .into_owned())
    }
}

// Hardware-specific H.264/HEVC/AV1 knobs do not apply to this Vulkan codec.
// Keep the user's shared host tuning intact when selecting a different backend.
fn validate_encoder_profile(settings:&crate::encoder::Settings)->Result<(),CodecError> {
    settings.validate()?;
    if settings.hdr || settings.rate_control!=crate::encoder::RateControl::Cbr {
        return Err(fail("PyroWave v1 supports SDR 4:2:0 with a fixed maximum frame budget; select CBR"));
    }
    Ok(())
}
pub struct Encoder {
    session: Session,
    settings: crate::encoder::Settings,
    gpu: bool,
    vendor: Option<u16>,
}
impl Encoder {
    pub fn new(
        settings: crate::encoder::Settings,
        input: Option<crate::encoder::InputDevice>,
    ) -> Result<Self, CodecError> {
        Self::with_runtime(settings, input, &runtime_path()?)
    }
    /// Explicit runtime path for standalone, isolated performance probes.
    pub fn with_runtime(
        settings: crate::encoder::Settings,
        input: Option<crate::encoder::InputDevice>,
        runtime: &Path,
    ) -> Result<Self, CodecError> {
        validate_encoder_profile(&settings)?;
        if matches!(
            settings.preference,
            crate::encoder::Preference::Software | crate::encoder::Preference::Vaapi
        ) {
            return Err(fail(
                "PyroWave requires a Vulkan GPU; select automatic or hardware encoding",
            ));
        }
        let (device, gpu, vendor) = {
            #[cfg(all(windows, feature = "mf-encode"))]
            {
                use windows::core::Interface;
                (
                    input
                        .as_ref()
                        .map_or(std::ptr::null_mut(), |i| i.device.as_raw()),
                    input.is_some(),
                    input.as_ref().map(|i| i.vendor),
                )
            }
            #[cfg(not(all(windows, feature = "mf-encode")))]
            {
                let _ = input;
                (std::ptr::null_mut(), false, None)
            }
        };
        if settings
            .preference
            .vendor_id()
            .is_some_and(|v| Some(v) != vendor)
        {
            return Err(fail(
                "PyroWave requires the selected vendor to match the capture adapter",
            ));
        }
        let session = Session::open(
            runtime,
            &Config {
                width: settings.width,
                height: settings.height,
                fps: settings.fps,
                kbps: settings.bitrate_kbps,
                encode: 1,
                gpu: u32::from(gpu),
            },
            device,
        )?;
        Ok(Self {
            session,
            settings,
            gpu,
            vendor,
        })
    }
    pub fn performance_stats(&self) -> Result<String, CodecError> {
        self.session.stats()
    }
}
impl VideoEncoder for Encoder {
    fn codec(&self) -> VideoCodec {
        VideoCodec::Pyrowave
    }
    fn is_hardware(&self) -> bool {
        true
    }
    fn gpu_input(&self) -> bool {
        self.gpu
    }
    fn gpu_vendor(&self) -> Option<u16> {
        self.vendor
    }
    fn backend_name(&self) -> &str {
        if self.gpu {
            "pyrowave-vulkan-d3d11"
        } else {
            "pyrowave-vulkan-cpu-upload"
        }
    }
    fn request_keyframe(&mut self) {} // every frame is independently decodable
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError> {
        if !(64..=1_000_000).contains(&kbps) {
            return Err(fail("invalid PyroWave bitrate"));
        }
        let mut error = [0; 512];
        checked(
            unsafe {
                eclipse_pw_set_rate(
                    self.session.0.as_ptr(),
                    kbps,
                    self.settings.fps,
                    error.as_mut_ptr(),
                    error.len(),
                )
            },
            &error,
        )?;
        self.settings.bitrate_kbps = kbps;
        Ok(())
    }
    fn encode(&mut self, frame: &RawFrame) -> Result<Vec<EncodedFrame>, CodecError> {
        if frame.width != self.settings.width
            || frame.height != self.settings.height
            || !frame.is_consistent()
        {
            return Err(fail("PyroWave capture geometry changed"));
        }
        let mut error = [0; 512];
        let mut data = std::ptr::null();
        let mut length = 0;
        let pixels;
        #[cfg(all(windows, feature = "mf-encode", feature = "h264"))]
        let guard;
        let texture = if frame.gpu {
            #[cfg(all(windows, feature = "mf-encode", feature = "h264"))]
            {
                use windows::core::Interface;
                let capture = frame
                    .d3d11
                    .as_ref()
                    .ok_or_else(|| fail("PyroWave requires a D3D11 GPU capture lease"))?;
                guard = capture.capture_read().map_err(fail)?;
                pixels = Vec::new();
                capture.capture_texture().map_err(fail)?.0.as_raw()
            }
            #[cfg(not(all(windows, feature = "mf-encode", feature = "h264")))]
            {
                return Err(fail(
                    "PyroWave GPU capture import is unavailable on this platform",
                ));
            }
        } else {
            #[cfg(all(windows, feature = "mf-encode", feature = "h264"))]
            {
                guard = None;
            }
            pixels = bt709_i420(frame)?;
            std::ptr::null_mut()
        };
        let result = unsafe {
            eclipse_pw_encode(
                self.session.0.as_ptr(),
                pixels.as_ptr().cast(),
                pixels.len(),
                texture,
                &mut data,
                &mut length,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        #[cfg(all(windows, feature = "mf-encode", feature = "h264"))]
        if let Some(guard) = guard {
            guard.finish().map_err(fail)?;
        }
        checked(result, &error)?;
        if data.is_null() || !(8..=MAX_FRAME).contains(&length) {
            return Err(fail("PyroWave returned an invalid bitstream"));
        }
        let data = unsafe { std::slice::from_raw_parts(data, length) };
        if sequence_header(data)? != [self.settings.width, self.settings.height] {
            return Err(fail("PyroWave encoded geometry mismatch"));
        }
        Ok(vec![EncodedFrame {
            data: bytes::Bytes::copy_from_slice(data),
            pts: frame.pts,
            is_keyframe: true,
        }])
    }
}

pub struct Decoder {
    session: Option<Arc<Mutex<Session>>>,
    size: [u32; 2],
    runtime: PathBuf,
    gpu: bool,
}
pub fn probe_decoder() -> Result<(), CodecError> {
    let mut decoder = Decoder::new()?;
    decoder.prepare(64, 64)?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(63u32 | (63 << 14) | (1 << 31)).to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let frame = decoder
        .decode(&EncodedFrame {
            data: bytes.into(),
            pts: crate::Timestamp(0),
            is_keyframe: true,
        })?
        .ok_or_else(|| fail("PyroWave decoder did not produce an image"))?;
    if !frame.is_consistent() {
        return Err(fail("PyroWave decoder returned an invalid image"));
    }
    Ok(())
}
impl Decoder {
    pub fn new() -> Result<Self, CodecError> {
        Ok(Self::with_runtime(
            runtime_path()?,
            cfg!(all(windows, feature = "d3d11va")) && crate::d3d11_frame::rendering(),
        ))
    }
    pub fn with_runtime(runtime: PathBuf, gpu: bool) -> Self {
        Self {
            session: None,
            size: [0, 0],
            runtime,
            gpu,
        }
    }
    pub fn performance_stats(&self) -> Result<String, CodecError> {
        self.session
            .as_ref()
            .ok_or_else(|| fail("PyroWave decoder is unprepared"))?
            .lock()
            .map_err(|_| fail("PyroWave decoder lock poisoned"))?
            .stats()
    }
}
#[derive(Debug)]
struct FrameLease {
    session: Arc<Mutex<Session>>,
    slot: i32,
}
impl Drop for FrameLease {
    fn drop(&mut self) {
        if let Ok(s) = self.session.lock() {
            unsafe {
                eclipse_pw_release(s.0.as_ptr(), self.slot);
            }
        }
    }
}
impl VideoDecoder for Decoder {
    fn codec(&self) -> VideoCodec {
        VideoCodec::Pyrowave
    }
    fn backend_name(&self) -> &str {
        if self.gpu {
            "pyrowave-vulkan-d3d11"
        } else {
            "pyrowave-vulkan-cpu-readback"
        }
    }
    fn prepare(&mut self, width: u32, height: u32) -> Result<(), CodecError> {
        geometry(width, height)?;
        if self.size == [width, height] && self.session.is_some() {
            return Ok(());
        }
        let session = Session::open(
            &self.runtime,
            &Config {
                width,
                height,
                fps: 60,
                kbps: 200_000,
                encode: 0,
                gpu: u32::from(self.gpu),
            },
            std::ptr::null_mut(),
        )?;
        self.session = Some(Arc::new(Mutex::new(session)));
        self.size = [width, height];
        Ok(())
    }
    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<RawFrame>, CodecError> {
        let size = frame_size(&frame.data)?;
        if self.session.is_none() {
            self.prepare(size[0], size[1])?;
        }
        if size != self.size {
            return Err(fail(
                "PyroWave bitstream does not match the negotiated display geometry",
            ));
        }
        let session = self.session.as_ref().unwrap();
        let s = session
            .lock()
            .map_err(|_| fail("PyroWave decoder lock poisoned"))?;
        let mut data = if self.gpu {
            Vec::new()
        } else {
            vec![0; size[0] as usize * size[1] as usize * 3 / 2]
        };
        let mut error = [0; 512];
        let mut slot = -1;
        let mut texture = std::ptr::null_mut();
        checked(
            unsafe {
                eclipse_pw_decode(
                    s.0.as_ptr(),
                    frame.data.as_ptr(),
                    frame.data.len(),
                    data.as_mut_ptr().cast(),
                    data.len(),
                    &mut slot,
                    &mut texture,
                    error.as_mut_ptr(),
                    error.len(),
                )
            },
            &error,
        )?;
        drop(s);
        if self.gpu {
            if slot < 0 {
                return Ok(None);
            }
            let lease = Arc::new(FrameLease {
                session: session.clone(),
                slot,
            });
            #[cfg(all(windows, feature = "d3d11va"))]
            {
                use windows::{core::Interface, Win32::Graphics::Direct3D11::ID3D11Texture2D};
                let texture = unsafe { ID3D11Texture2D::from_raw_borrowed(&texture) }
                    .ok_or_else(|| fail("PyroWave output slot has no texture"))?
                    .clone();
                let device = unsafe { texture.GetDevice() }.map_err(|e| fail(e.to_string()))?;
                let storage = crate::d3d11_frame::Frame::external_rgb(texture, device, lease)
                    .map_err(fail)?;
                return Ok(Some(RawFrame {
                    width: size[0],
                    height: size[1],
                    format: crate::PixelFormat::Bgra8,
                    stride: size[0] * 4,
                    data: bytes::Bytes::new(),
                    pts: frame.pts,
                    gpu: true,
                    dma_buf: None,
                    d3d11: Some(Arc::new(storage)),
                }));
            }
            #[cfg(not(all(windows, feature = "d3d11va")))]
            {
                drop(lease);
                return Err(fail(
                    "PyroWave GPU rendering is unavailable on this platform",
                ));
            }
        }
        Ok(Some(RawFrame::new_i420(size[0], size[1], data, frame.pts)))
    }
}

fn bt709_i420(frame: &RawFrame) -> Result<Vec<u8>, CodecError> {
    crate::convert::validate_cpu_frame(frame)?;
    let rgba = crate::convert::frame_to_rgba(frame);
    let (w, h) = (frame.width as usize, frame.height as usize);
    let mut out = vec![0; w * h * 3 / 2];
    let level = |value: i32| ((value + 32768) >> 16).clamp(0, 255) as u8;
    for y in 0..h {
        for x in 0..w {
            let p = &rgba[(y * w + x) * 4..];
            let (r, g, b) = (i32::from(p[0]), i32::from(p[1]), i32::from(p[2]));
            out[y * w + x] = level(13933 * r + 46871 * g + 4732 * b);
        }
    }
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let (mut r, mut g, mut b) = (0, 0, 0);
            for dy in 0..2 {
                for dx in 0..2 {
                    let p = &rgba[((y + dy) * w + x + dx) * 4..];
                    r += i32::from(p[0]);
                    g += i32::from(p[1]);
                    b += i32::from(p[2]);
                }
            }
            let (r, g, b) = ((r + 2) / 4, (g + 2) / 4, (b + 2) / 4);
            let offset = (y / 2) * (w / 2) + x / 2;
            out[w * h + offset] = level(-7508 * r - 25260 * g + 32768 * b + 128 * 65536);
            out[w * h + w * h / 4 + offset] = level(32768 * r - 29763 * g - 3005 * b + 128 * 65536);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn header(blocks: u32, seq: u32) -> Vec<u8> {
        [31u32 | (31 << 14) | ((seq & 7) << 28) | (1 << 31), blocks]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect()
    }
    fn block(seq: u16) -> Vec<u8> {
        let mut bytes = vec![];
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&(3u16 | ((seq & 7) << 12)).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        bytes
    }
    #[test]
    fn hardware_tuning_does_not_disable_the_unrelated_pyrowave_backend() {
        use crate::encoder::{Settings,Tuning,AmdQuality,LowPower,Preference,RateControl};
        let settings=Settings{codec:VideoCodec::Pyrowave,width:1920,height:1080,fps:165,bitrate_kbps:200000,
            preference:Preference::Hardware,rate_control:RateControl::Cbr,hdr:false,source_hdr:None,
            tuning:Tuning{amd_quality:AmdQuality::Balanced,intel_target_usage:4,intel_low_power:LowPower::Off,nvidia_quality_vs_speed:Some(75)}};
        validate_encoder_profile(&settings).unwrap();
        assert!(validate_encoder_profile(&Settings{hdr:true,..settings}).is_err());
        assert!(validate_encoder_profile(&Settings{rate_control:RateControl::Vbr{peak_kbps:400000},..settings}).is_err());
    }
    #[test]
    fn validates_frames_across_sequence_wrap_and_rejects_truncation() {
        for seq in 0..16 {
            assert_eq!(frame_size(&header(0, seq)).unwrap(), [32, 32]);
            let mut frame = header(1, seq);
            frame.extend(block(seq as u16));
            assert_eq!(frame_size(&frame).unwrap(), [32, 32]);
            for len in 0..frame.len() {
                assert!(frame_size(&frame[..len]).is_err());
            }
            frame[11] ^= 0x10;
            assert!(frame_size(&frame).is_err());
        }
    }
    #[test]
    fn rejects_unsupported_colour_geometry_duplicates_and_payloads() {
        let mut frame = header(1, 0);
        frame.extend(block(0));
        let mut cases = vec![];
        for flag in [26, 27, 28, 29, 30, 31] {
            let mut bad = frame.clone();
            bad[7] |= 1 << (flag - 24);
            cases.push(bad);
        }
        let mut bad = frame.clone();
        bad[14] = 255;
        cases.push(bad); // out-of-range block index
        let mut bad = frame.clone();
        bad[4] = 2;
        bad.extend(block(0));
        cases.push(bad); // duplicate index
        let mut bad = frame.clone();
        bad[18] = 15;
        cases.push(bad); // absent magnitude/sign payload
        let mut bad = frame.clone();
        bad[8] = 0;
        cases.push(bad); // empty ballot
        for bad in cases {
            assert!(frame_size(&bad).is_err());
        }
    }
    #[test]
    fn cpu_rgb_conversion_uses_full_bt709_and_neutral_chroma() {
        for (bgra, y, u, v) in [
            ([0, 0, 0, 255], 0, 128, 128),
            ([255, 255, 255, 255], 255, 128, 128),
            ([0, 0, 255, 255], 54, 99, 255),
        ] {
            let frame = RawFrame {
                width: 2,
                height: 2,
                format: crate::PixelFormat::Bgra8,
                stride: 8,
                data: bgra.repeat(4).into(),
                pts: crate::Timestamp(0),
                gpu: false,
                dma_buf: None,
                d3d11: None,
            };
            assert_eq!(bt709_i420(&frame).unwrap(), [y, y, y, y, u, v]);
        }
    }
}
