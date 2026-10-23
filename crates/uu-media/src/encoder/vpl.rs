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
        out: *mut *mut c_void,
        error: *mut c_char,
        capacity: usize,
    ) -> i32;
    fn eclipse_vpl_encoder_close(encoder: *mut c_void);
    fn eclipse_vpl_encoder_low_power(encoder: *mut c_void) -> i32;
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
    pub fn open(settings: Settings) -> Result<Self, String> {
        let codec = match settings.codec {
            uu_proto::media::VideoCodec::H264 => 0,
            uu_proto::media::VideoCodec::Hevc => 1,
            uu_proto::media::VideoCodec::Av1 => 2,
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
                &mut encoder,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if status < 0 {
            let low_power=match settings.tuning.intel_low_power {
                super::LowPower::Auto=>"自动",super::LowPower::On=>"启用",super::LowPower::Off=>"停用",
            };
            return Err(format!("Intel QSV {:?}（Target Usage={}，低功耗={}）：{}",
                settings.codec,settings.tuning.intel_target_usage,low_power,message(&error)));
        }
        let encoder = NonNull::new(encoder).ok_or("Intel runtime returned no encoder")?;
        let low_power = unsafe { eclipse_vpl_encoder_low_power(encoder.as_ptr()) } != 0;
        Ok(Self {
            encoder,
            settings,
            name: format!(
                "intel-vpl ({:?} hw, {}, {} engine, TU{})",
                settings.codec,
                if vbr { "VBR" } else { "CBR" },
                if low_power { "low power" } else { "general" }, settings.tuning.intel_target_usage
            ),
            pending: VecDeque::new(),
            headers: Default::default(),
            profile: super::profile::Profile::new("intel-vpl"),
        })
    }
}
impl super::worker::Driver for Driver {
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
        )?;
        let uploaded = started.map(|start| start.elapsed());
        let tag = frame.pts.0.saturating_mul(90_000) / 1_000_000;
        if self.pending.len() >= 4 {
            return Err("Intel encoder buffered more than four input frames".into());
        }
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
    fn bitrate(&mut self, kbps: u32) -> Result<(), String> {
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
        Ok(())
    }
}
