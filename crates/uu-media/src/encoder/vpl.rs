//! Intel hardware encoding through a dynamically loaded VPL runtime. The C
//! adapter uses unmodified official ABI declarations; all ownership stays here.
use super::{
    upload::upload_frame,
    Settings,
};
use crate::{EncodedFrame, RawFrame, Timestamp};
use std::{
    collections::VecDeque,
    ffi::{c_char, c_void, CStr},
    ptr::NonNull,
};

extern "C" {
    fn eclipse_vpl_encoder_open(
        codec: u32,
        width: u32,
        height: u32,
        fps: u32,
        target: u32,
        peak: u32,
        vbr: i32,
        target_usage:u32,
        low_power:i32,
        gop_frames:u32,
        vbv_frames:u32,
        d3d11_device: *mut c_void,
        out: *mut *mut c_void,
        error: *mut c_char,
        capacity: usize,
    ) -> i32;
    #[cfg(all(windows, feature="mf-encode"))]
    fn eclipse_vpl_encoder_texture(encoder: *mut c_void, texture: *mut *mut c_void, error: *mut c_char, capacity: usize) -> i32;
    #[cfg(all(windows, feature="mf-encode"))]
    fn eclipse_vpl_encoder_release_texture(encoder: *mut c_void);
    fn eclipse_vpl_encoder_close(encoder: *mut c_void);
    fn eclipse_vpl_encoder_low_power(encoder: *mut c_void) -> i32;
    fn eclipse_vpl_encoder_vbv_kb(encoder: *mut c_void) -> u32;
    fn eclipse_vpl_encoder_input(
        encoder: *mut c_void,
        data: *mut *mut u8,
        len: *mut usize,
        pitch: *mut u32,
        rows: *mut u32,
    ) -> i32;
    fn eclipse_vpl_encoder_encode(
        encoder: *mut c_void,
        timestamp: u64,
        keyframe: i32,
        data: *mut *const u8,
        len: *mut usize,
        pts: *mut u64,
        key: *mut i32,
        error: *mut c_char,
        capacity: usize,
    ) -> i32;
    fn eclipse_vpl_encoder_bitrate(
        encoder: *mut c_void,
        kbps: u32,
        error: *mut c_char,
        capacity: usize,
    ) -> i32;
}
fn message(buffer: &[c_char]) -> String {
    unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}
pub(super) struct Driver {
    encoder: NonNull<c_void>,
    settings: Settings,
    name: String,
    #[cfg(all(windows, feature="mf-encode"))]
    video_input: Option<super::vpl_gpu::Input>,
    pending: VecDeque<(u64, Timestamp)>,
    headers: super::headers::Headers,
    profile: super::profile::Profile,
}
impl Drop for Driver {
    fn drop(&mut self) {
        unsafe {
            eclipse_vpl_encoder_close(self.encoder.as_ptr());
        }
    }
}
impl Driver {
    pub fn open(settings: Settings, input: Option<super::InputDevice>) -> Result<Self, String> {
        #[cfg(all(windows, feature="mf-encode"))]
        if settings.tuning.intel_gpu_input && !settings.hdr && settings.source_hdr.is_none() {
            if let Some(input) = input.filter(|input| input.vendor == 0x8086) {
                match Self::open_texture(settings, input) {
                    Ok(driver) => return Ok(driver),
                    Err(error) => tracing::warn!(%error, "oneVPL D3D11 input unavailable; retrying with system memory"),
                }
            }
        }
        #[cfg(not(all(windows, feature="mf-encode")))]
        let _ = input;
        Self::open_native(settings, std::ptr::null_mut())
    }

    #[cfg(all(windows, feature="mf-encode"))]
    pub(super) fn open_texture(settings: Settings, device: super::InputDevice) -> Result<Self, String> {
        let video = super::vpl_gpu::Input::new(device)?;
        let mut driver = Self::open_native(settings, video.device_ptr())?;
        let raw = driver.acquire_texture()?;
        video.texture(raw, settings)?;
        unsafe { eclipse_vpl_encoder_release_texture(driver.encoder.as_ptr()); }
        driver.video_input = Some(video);
        Ok(driver)
    }

    #[cfg(all(windows, feature="mf-encode"))]
    fn acquire_texture(&self) -> Result<*mut c_void, String> {
        let mut raw = std::ptr::null_mut();
        let mut error = [0; 512];
        let status = unsafe { eclipse_vpl_encoder_texture(self.encoder.as_ptr(), &mut raw, error.as_mut_ptr(), error.len()) };
        if status < 0 { return Err(message(&error)); }
        Ok(raw)
    }

    fn open_native(settings: Settings, device: *mut c_void) -> Result<Self, String> {
        if settings.hdr {return Err("HDR requires the Windows P010 hardware encoder path".into());}
        if cfg!(windows) && settings.tuning.intel_mf_quality_vs_speed.is_some()
            && !settings.tuning.custom_intel()
            && matches!(settings.preference, super::Preference::Auto | super::Preference::Hardware) {
            return Err("Intel MFT quality/speed cannot be applied by oneVPL".into());
        }
        let codec = match settings.codec {
            uu_proto::media::VideoCodec::H264 => 0,
            uu_proto::media::VideoCodec::Hevc => 1,
            uu_proto::media::VideoCodec::Av1 => 2,
            uu_proto::media::VideoCodec::Pyrowave=>return Err("PyroWave uses the Vulkan backend".into()),
        };
        let mut error = [0 as c_char; 512];
        let mut encoder = std::ptr::null_mut();
        let vbr = matches!(settings.rate_control, super::RateControl::Vbr { .. });
        let status = unsafe {
            eclipse_vpl_encoder_open(
                codec,
                settings.width,
                settings.height,
                settings.fps,
                settings.bitrate_kbps,
                settings.rate_control.peak(settings.bitrate_kbps),
                i32::from(vbr),
                u32::from(settings.tuning.intel_target_usage),
                match settings.tuning.intel_low_power{super::LowPower::Auto=>0,super::LowPower::On=>1,super::LowPower::Off=>2},
                settings.tuning.gop_frames(settings.fps).unwrap_or(0),
                u32::from(settings.tuning.intel_vbv_frames.unwrap_or(0)),
                device,
                &mut encoder,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if status < 0 {
            let low_power=match settings.tuning.intel_low_power {
                super::LowPower::Auto=>"自动",super::LowPower::On=>"启用",super::LowPower::Off=>"停用",
            };
            return Err(format!("Intel QSV {:?}（Target Usage={}，低功耗={}{}）：{}",
                settings.codec,settings.tuning.intel_target_usage,low_power, controls(settings),message(&error)));
        }
        let encoder = NonNull::new(encoder).ok_or("Intel runtime returned no encoder")?;
        let low_power = unsafe { eclipse_vpl_encoder_low_power(encoder.as_ptr()) } != 0;
        let mut applied = controls(settings);
        if cfg!(windows) && settings.codec == uu_proto::media::VideoCodec::Av1
            && !vbr && settings.tuning.intel_vbv_frames.is_none()
        {
            let kb = unsafe { eclipse_vpl_encoder_vbv_kb(encoder.as_ptr()) };
            applied.push_str(&format!(", driver VBV={kb} kB"));
        }
        Ok(Self {
            encoder,
            settings,
            name: format!(
                "intel-vpl ({:?} hw, {}, {} engine, TU{}{}, {})",
                settings.codec,
                if vbr { "VBR" } else { "CBR" },
                if low_power { "low power" } else { "general" }, settings.tuning.intel_target_usage, applied,
                if device.is_null() { "system memory" } else { "D3D11 input" }
            ),
            #[cfg(all(windows, feature="mf-encode"))]
            video_input: None,
            pending: VecDeque::new(),
            headers: Default::default(),
            profile: super::profile::Profile::new("intel-vpl"),
        })
    }
    fn prepare_input(&mut self, frame: &RawFrame) -> Result<(), String> {
        #[cfg(all(windows, feature="mf-encode"))]
        if self.video_input.is_some() {
            let raw = self.acquire_texture()?;
            let result = self.video_input.as_mut().unwrap().write(raw, frame, self.settings);
            if result.is_err() {
                unsafe { eclipse_vpl_encoder_release_texture(self.encoder.as_ptr()); }
            }
            return result;
        }
        let (mut pointer, mut size, mut pitch, mut rows) = (std::ptr::null_mut(), 0, 0, 0);
        if unsafe {
            eclipse_vpl_encoder_input(
                self.encoder.as_ptr(),
                &mut pointer,
                &mut size,
                &mut pitch,
                &mut rows,
            )
        } < 0
        {
            return Err(
                "Intel surface remains locked; refusing to overwrite a reference frame".into(),
            );
        }
        if pointer.is_null()
            || size > 64 * 1024 * 1024
            || size != (pitch as usize * rows as usize * 3 / 2)
        {
            return Err("invalid Intel input surface".into());
        }
        let surface = unsafe { std::slice::from_raw_parts_mut(pointer, size) };
        upload_frame(
            frame,
            (pitch, rows),
            [pitch, pitch, 0],
            [0, pitch * rows, 0],
            surface,
        )
    }
}
fn controls(settings: Settings) -> String {
    let mut text = String::new();
    if let Some(frames) = settings.tuning.gop_frames(settings.fps) {
        text.push_str(&format!(", GOP={frames}"));
    }
    if let Some(frames) = settings.tuning.intel_vbv_frames {
        text.push_str(&format!(", VBV={frames}f"));
    }
    text
}
impl super::worker::Driver for Driver {
    #[cfg(all(windows, feature="mf-encode"))]
    fn gpu_input(&self) -> bool { self.video_input.is_some() }
    #[cfg(all(windows, feature="mf-encode"))]
    fn gpu_vendor(&self) -> Option<u16> { self.video_input.as_ref().map(|_| 0x8086) }
    fn backend_name(&self) -> &str {
        &self.name
    }
    fn codec(&self) -> uu_proto::media::VideoCodec {
        self.settings.codec
    }
    fn encode(&mut self, frame: &RawFrame, keyframe: bool) -> Result<Vec<EncodedFrame>, String> {
        let started = self.profile.start();
        if (frame.width, frame.height) != (self.settings.width, self.settings.height) {
            return Err("capture size changed".into());
        }
        if self.pending.len() >= 4 {
            return Err("Intel encoder buffered more than four input frames".into());
        }
        self.prepare_input(frame)?;
        let uploaded = started.map(|start| start.elapsed());
        let tag = frame.pts.0.saturating_mul(90_000) / 1_000_000;
        self.pending.push_back((tag, frame.pts));
        let (mut data, mut len, mut pts, mut key) = (std::ptr::null(), 0, 0, 0);
        let mut error = [0 as c_char; 512];
        let result = unsafe {
            eclipse_vpl_encoder_encode(
                self.encoder.as_ptr(),
                tag,
                i32::from(keyframe),
                &mut data,
                &mut len,
                &mut pts,
                &mut key,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if result < 0 {
            return Err(message(&error));
        }
        if result == 0 {
            return Ok(Vec::new());
        }
        if data.is_null() || len > 64 * 1024 * 1024 {
            return Err("invalid Intel coded buffer".into());
        }
        let index = self
            .pending
            .iter()
            .position(|(tag, _)| *tag == pts)
            .ok_or("Intel encoder returned an unknown input timestamp")?;
        let (_, pts) = self.pending.remove(index).unwrap();
        let mut bytes = unsafe { std::slice::from_raw_parts(data, len) }.to_vec();
        if self.settings.codec == uu_proto::media::VideoCodec::H264 {
            bytes = super::low_delay::declare_no_reordering(&bytes)?;
        }
        let mut frame = EncodedFrame {
            data: bytes.into(),
            pts,
            is_keyframe: key != 0,
        };
        self.headers.prepare(self.settings.codec, &mut frame)?;
        self.profile.record(started, uploaded);
        Ok(vec![frame])
    }
    fn bitrate(&mut self, kbps: u32) -> Result<super::worker::BitrateChange, String> {
        let mut error = [0 as c_char; 512];
        if unsafe {
            eclipse_vpl_encoder_bitrate(
                self.encoder.as_ptr(),
                kbps,
                error.as_mut_ptr(),
                error.len(),
            )
        } < 0
        {
            return Err(message(&error));
        }
        self.settings.bitrate_kbps = kbps;
        Ok(super::worker::BitrateChange::KeyframeRequired)
    }
}
