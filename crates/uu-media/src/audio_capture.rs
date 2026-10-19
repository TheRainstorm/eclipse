//! Explicit system-output capture and bounded, paced Opus audio encoding.
//! Device handles stay on the calling audio worker; no microphone fallback.
use anyhow::{ensure, Result};
use bytes::Bytes;
#[cfg(any(windows, test))]
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
#[cfg(any(windows, test))]
use std::collections::VecDeque;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
mod pulse;
#[cfg(windows)]
mod wasapi;

const RATE: u32 = 48_000;
const FRAMES: usize = 480;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    System,
    Synthetic,
}
pub struct EncodedAudio {
    pub data: Bytes,
    /// 48 kHz source-clock samples, used to preserve gaps after a late worker.
    pub sample_index: u64,
    pub samples: u32,
    pub created: Instant,
}
enum Device {
    Synthetic,
    #[cfg(target_os = "linux")]
    Pulse(pulse::Capture),
    #[cfg(windows)]
    Wasapi(wasapi::Capture),
}
pub struct CaptureSession {
    device: Device,
    queue: crate::audio::PcmQueue,
    encoder: opus::Encoder,
    deadline: Instant,
    sample_index: u64,
    last_source: Instant,
    last_poll: Instant,
}
impl CaptureSession {
    pub fn new(source: Source, stop: &AtomicBool) -> Result<Self> {
        ensure!(!stop.load(Ordering::Acquire), "audio capture stopped");
        let device = match source {
            Source::Synthetic => Device::Synthetic,
            #[cfg(target_os = "linux")]
            Source::System => Device::Pulse(pulse::Capture::new(stop)?),
            #[cfg(windows)]
            Source::System => Device::Wasapi(wasapi::Capture::new()?),
            #[cfg(not(any(windows, target_os = "linux")))]
            Source::System => {
                anyhow::bail!("system-output capture is unavailable on this platform")
            }
        };
        let mut encoder =
            opus::Encoder::new(RATE, opus::Channels::Stereo, opus::Application::Audio)?;
        encoder.set_bitrate(opus::Bitrate::Bits(128_000))?;
        encoder.set_complexity(5)?;
        encoder.set_vbr_constraint(true)?;
        // The decoder can attempt in-band FEC/PLC. Actual FEC inclusion remains
        // an encoder mode decision; this does not imply video FEC support.
        encoder.set_inband_fec(true)?;
        encoder.set_packet_loss_perc(10)?;
        let mut queue = crate::audio::PcmQueue::default();
        queue.submit(&[], RATE, 2);
        Ok(Self {
            device,
            queue,
            encoder,
            deadline: Instant::now() + Duration::from_millis(30),
            sample_index: 0,
            last_source: Instant::now(),
            last_poll: Instant::now(),
        })
    }
    pub fn backend(&self) -> &'static str {
        match self.device {
            Device::Synthetic => "synthetic tone",
            #[cfg(target_os = "linux")]
            Device::Pulse(_) => "PulseAudio output monitor",
            #[cfg(windows)]
            Device::Wasapi(_) => "WASAPI output loopback",
        }
    }
    /// Nonblocking device polling. Caller sleeps briefly when no packet is due.
    pub fn next_packet(&mut self) -> Result<Option<EncodedAudio>> {
        let now = Instant::now();
        let late = now
            .checked_duration_since(self.deadline)
            .unwrap_or_default();
        let worker_late = now.duration_since(self.last_poll);
        self.last_poll = now;
        if worker_late > Duration::from_millis(60) {
            self.queue = crate::audio::PcmQueue::default();
            self.queue.submit(&[], RATE, 2);
        }
        if worker_late > Duration::from_millis(120) {
            self.encoder.reset_state()?;
        }
        let interval = Duration::from_millis(10);
        if (matches!(self.device, Device::Synthetic) && late > interval * 2)
            || worker_late > Duration::from_millis(60)
        {
            let missed = late.as_millis() / 10;
            self.sample_index = self
                .sample_index
                .saturating_add(missed as u64 * FRAMES as u64);
            self.deadline += interval * missed.min(u32::MAX as u128) as u32;
        }
        match &mut self.device {
            Device::Synthetic => {}
            #[cfg(target_os = "linux")]
            Device::Pulse(device) => {
                for block in device.poll()? {
                    if !block.is_empty() {
                        self.last_source = Instant::now();
                        self.queue.submit(&block, RATE, 2);
                    }
                }
            }
            #[cfg(windows)]
            Device::Wasapi(device) => {
                for block in device.poll()? {
                    if !block.is_empty() {
                        self.last_source = Instant::now();
                        self.queue.submit(&block, RATE, 2);
                    }
                }
            }
        }
        let now = Instant::now();
        if now < self.deadline {
            return Ok(None);
        }
        // Wait for a complete driver block instead of turning ordinary device
        // clock jitter into an artificial underrun. Sustained silent loopback
        // still emits paced silence, with the shared fade/refill transitions.
        if !matches!(self.device, Device::Synthetic)
            && self.queue.buffered_frames() < FRAMES
            && now.duration_since(self.last_source) < Duration::from_millis(40)
        {
            return Ok(None);
        }
        let mut pcm = [0.0f32; FRAMES * 2];
        if matches!(self.device, Device::Synthetic) {
            for (i, frame) in pcm.chunks_exact_mut(2).enumerate() {
                let phase = (self.sample_index + i as u64) as f64 * 440.0 * std::f64::consts::TAU
                    / RATE as f64;
                let value = phase.sin() as f32 * 0.05;
                frame.fill(value);
            }
        } else {
            for sample in &mut pcm {
                *sample = self.queue.sample(RATE, 2);
            }
        }
        let mut bytes = [0u8; 1024];
        let size = self.encoder.encode_float(&pcm, &mut bytes)?;
        let packet = EncodedAudio {
            data: Bytes::copy_from_slice(&bytes[..size]),
            sample_index: self.sample_index,
            samples: FRAMES as u32,
            created: now,
        };
        self.sample_index += FRAMES as u64;
        self.deadline += interval;
        Ok(Some(packet))
    }
}

/// Normalized standard WAVE speaker layouts through 7.1, including mono.
#[cfg(any(windows, test))]
fn stereo_frame(channels: usize, mut sample: impl FnMut(usize) -> f32) -> [f32; 2] {
    if channels == 0 || channels > 8 {
        return [0.0; 2];
    }
    let clean = |v: f32| {
        if v.is_finite() {
            v.clamp(-1.0, 1.0)
        } else {
            0.0
        }
    };
    if channels == 1 {
        return [clean(sample(0)); 2];
    }
    if channels == 2 {
        return [clean(sample(0)), clean(sample(1))];
    }
    let mut output = [0.0; 2];
    let mut weight = [0.0; 2];
    let side = std::f32::consts::FRAC_1_SQRT_2;
    for index in 0..channels {
        let coefficients = match (channels, index) {
            (_, 0) => [1.0, 0.0],
            (_, 1) => [0.0, 1.0],
            (4, 2) => [side, 0.0],
            (4, 3) => [0.0, side],
            (_, 2) => [side, side],
            (5, 3) => [side, 0.0],
            (5, 4) => [0.0, side],
            (6..=8, 3) => [0.0, 0.0],
            (7, 4) => [0.5, 0.5],
            (7, 5) => [side, 0.0],
            (7, 6) => [0.0, side],
            (_, 4 | 6) => [side, 0.0],
            (_, 5 | 7) => [0.0, side],
            _ => [0.0, 0.0],
        };
        let value = clean(sample(index));
        for channel in 0..2 {
            output[channel] += value * coefficients[channel];
            weight[channel] += coefficients[channel];
        }
    }
    [output[0] / weight[0], output[1] / weight[1]]
}

/// Standard PCM rate conversion, keeping fractional phase across device blocks.
/// A 48 kHz device bypasses resampling. Other device rates use a windowed sinc.
#[cfg(any(windows, test))]
struct StereoRate {
    resampler: Option<SincFixedIn<f32>>,
    pending: VecDeque<[f32; 2]>,
    chunk: usize,
}
#[cfg(any(windows, test))]
impl StereoRate {
    fn new(rate: u32) -> Result<Self> {
        ensure!(
            (8_000..=192_000).contains(&rate),
            "unsupported system-output sample rate"
        );
        let chunk = (rate as usize / 100).max(1);
        let resampler = if rate == RATE {
            None
        } else {
            Some(SincFixedIn::new(
                RATE as f64 / rate as f64,
                1.1,
                SincInterpolationParameters {
                    sinc_len: 64,
                    f_cutoff: 0.92,
                    interpolation: SincInterpolationType::Linear,
                    oversampling_factor: 128,
                    window: WindowFunction::BlackmanHarris2,
                },
                chunk,
                2,
            )?)
        };
        Ok(Self {
            resampler,
            pending: VecDeque::new(),
            chunk,
        })
    }
    fn convert(&mut self, stereo: &[f32]) -> Result<Vec<f32>> {
        ensure!(
            stereo.len() % 2 == 0 && stereo.len() <= 192_000 * 2,
            "invalid system-output PCM block"
        );
        let Some(resampler) = &mut self.resampler else {
            return Ok(stereo.to_vec());
        };
        self.pending
            .extend(stereo.chunks_exact(2).map(|p| [p[0], p[1]]));
        let mut output = Vec::new();
        while self.pending.len() >= self.chunk {
            let mut planar = vec![
                Vec::with_capacity(self.chunk),
                Vec::with_capacity(self.chunk),
            ];
            for frame in self.pending.drain(..self.chunk) {
                planar[0].push(frame[0]);
                planar[1].push(frame[1]);
            }
            let block = resampler.process(&planar, None)?;
            for (l, r) in block[0].iter().zip(&block[1]) {
                output.extend([*l, *r]);
            }
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn synthetic_packets_are_paced_valid_stereo_opus_and_cancel_before_open() {
        let stop = AtomicBool::new(false);
        let mut capture = CaptureSession::new(Source::Synthetic, &stop).unwrap();
        assert!(capture.next_packet().unwrap().is_none());
        capture.deadline = Instant::now();
        let packet = capture.next_packet().unwrap().unwrap();
        let mut decoder = opus::Decoder::new(RATE, opus::Channels::Stereo).unwrap();
        let mut pcm = [0.0; FRAMES * 2];
        assert_eq!(
            decoder.decode_float(&packet.data, &mut pcm, false).unwrap(),
            FRAMES
        );
        assert!(pcm.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
        assert!(capture.next_packet().unwrap().is_none());
        assert!(CaptureSession::new(Source::System, &AtomicBool::new(true)).is_err());
    }
    #[test]
    fn surround_downmix_keeps_sides_center_and_full_scale_in_bounds() {
        for channels in 1..=8 {
            let full = stereo_frame(channels, |_| 1.0);
            assert!(full
                .iter()
                .all(|v| v.is_finite() && (*v - 1.0).abs() < 0.0001));
        }
        for (channels, left, right) in [(4, 2, 3), (5, 3, 4), (6, 4, 5), (7, 5, 6), (8, 6, 7)] {
            let a = stereo_frame(channels, |i| if i == left { 1.0 } else { 0.0 });
            assert!(a[0] > 0.0 && a[1] == 0.0);
            let b = stereo_frame(channels, |i| if i == right { 1.0 } else { 0.0 });
            assert!(b[1] > 0.0 && b[0] == 0.0);
        }
        assert_eq!(
            stereo_frame(8, |i| if i == 3 { 1.0 } else { 0.0 }),
            [0.0; 2]
        );
        assert_eq!(stereo_frame(2, |_| f32::NAN), [0.0; 2]);
    }
    #[test]
    fn common_device_rates_preserve_duration_stereo_and_finite_waveforms() {
        for rate in [8_000, 22_050, 44_100, 48_000, 96_000, 192_000] {
            let mut converter = StereoRate::new(rate).unwrap();
            let mut samples = Vec::new();
            for i in 0..rate {
                let value = (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.1;
                samples.extend([value, -value]);
            }
            let output = converter.convert(&samples).unwrap();
            assert!(
                (47_500..=48_100).contains(&(output.len() / 2)),
                "{rate}: {}",
                output.len() / 2
            );
            assert!(output
                .chunks_exact(2)
                .all(|p| p.iter().all(|v| v.is_finite() && v.abs() < 0.2)
                    && (p[0] + p[1]).abs() < 0.0001));
            assert!(converter.pending.len() < converter.chunk);
        }
    }
}
