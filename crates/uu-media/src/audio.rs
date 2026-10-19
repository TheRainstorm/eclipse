//! In-process Opus receive/playback. Never block RTP or retain unbounded sound.
#[cfg(any(windows, test))]
use std::collections::VecDeque;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

pub struct AudioControl {
    pub volume: AtomicU32,
    pub muted: AtomicBool,
    pub packets: AtomicU64,
    pub concealed: AtomicU64,
    pub dropped: AtomicU64,
    pub queue_ms: AtomicU32,
    pub packet_ms: AtomicU32,
    pub receive_decode_us: AtomicU32,
    pub output_block_us: AtomicU32,
    pub underruns: AtomicU64,
    pub status: Mutex<String>,
    stopped: AtomicBool,
}
impl Default for AudioControl {
    fn default() -> Self {
        Self {
            volume: AtomicU32::new(100),
            muted: AtomicBool::new(false),
            packets: AtomicU64::new(0),
            concealed: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            queue_ms: AtomicU32::new(0),
            packet_ms: AtomicU32::new(0),
            receive_decode_us: AtomicU32::new(0),
            output_block_us: AtomicU32::new(0),
            underruns: AtomicU64::new(0),
            status: Mutex::new("等待远端音频".into()),
            stopped: AtomicBool::new(false),
        }
    }
}
impl AudioControl {
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
    }
    pub fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
    fn status(&self, text: impl Into<String>) {
        *self.status.lock().unwrap() = text.into();
    }
}

pub struct Packet {
    pub sequence: u16,
    pub timestamp: u32,
    pub payload: Vec<u8>,
    pub received: Instant,
}

/// Small reorder window. A missing packet waits at most 20 ms; wraparound is
/// compared relative to the next expected RTP sequence, never as an integer sort.
#[derive(Default)]
pub struct Reorder {
    packets: Vec<Packet>,
    expected: Option<u16>,
}
impl Reorder {
    pub fn push(&mut self, packet: Packet) {
        if self
            .expected
            .is_some_and(|n| packet.sequence.wrapping_sub(n) >= 0x8000)
            || self.packets.iter().any(|p| p.sequence == packet.sequence)
        {
            return;
        }
        if self.packets.len() == 12 {
            self.packets.remove(0);
        }
        self.packets.push(packet);
    }
    pub fn pop(&mut self, now: Instant) -> Option<Packet> {
        let first = self.packets.first()?;
        let expected = self.expected.unwrap_or(first.sequence);
        let index = self
            .packets
            .iter()
            .enumerate()
            .min_by_key(|(_, p)| p.sequence.wrapping_sub(expected))?
            .0;
        let packet = &self.packets[index];
        if packet.sequence != expected
            && now.duration_since(packet.received) < Duration::from_millis(20)
        {
            return None;
        }
        let packet = self.packets.remove(index);
        self.expected = Some(packet.sequence.wrapping_add(1));
        Some(packet)
    }
}

pub struct OpusReceiver {
    decoder: opus::Decoder,
    next_timestamp: Option<u32>,
}
impl OpusReceiver {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            decoder: opus::Decoder::new(48_000, opus::Channels::Stereo)?,
            next_timestamp: None,
        })
    }
    pub fn decode(
        &mut self,
        timestamp: u32,
        payload: &[u8],
        control: &AudioControl,
    ) -> anyhow::Result<Vec<f32>> {
        let mut output = Vec::new();
        let duration = self.decoder.get_nb_samples(payload)?;
        control
            .packet_ms
            .store((duration * 1000 / 48_000) as u32, Ordering::Relaxed);
        if let Some(expected) = self.next_timestamp {
            let missing = timestamp.wrapping_sub(expected) as usize;
            if missing > 0 && missing <= 5760 && missing % 120 == 0 {
                // libopus recovers in-band FEC if present, otherwise performs
                // PLC. This counter means concealment attempts, not proven FEC.
                let mut recovery = vec![0.0; missing * 2];
                if let Ok(n) = self.decoder.decode_float(payload, &mut recovery, true) {
                    recovery.truncate(n * 2);
                    output.extend(recovery);
                    control.concealed.fetch_add(1, Ordering::Relaxed);
                }
            } else if missing > 5760 {
                self.decoder.reset_state()?;
            }
        }
        let mut pcm = vec![0.0; 5760 * 2];
        let count = self.decoder.decode_float(payload, &mut pcm, false)?;
        pcm.truncate(count * 2);
        output.extend(pcm);
        self.next_timestamp = Some(timestamp.wrapping_add(duration as u32));
        Ok(output)
    }
}

/// RTP task uses try_send; decoder and hardware device run on a dedicated thread.
pub fn playback(control: Arc<AudioControl>) -> std::sync::mpsc::SyncSender<Packet> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<Packet>(12);
    std::thread::Builder::new()
        .name("uu-audio".into())
        .spawn(move || {
            let result = run(rx, &control);
            if let Err(error) = result {
                control.status(format!("音频不可用：{error}"));
            } else {
                control.status("音频已停止");
            }
            control.queue_ms.store(0, Ordering::Relaxed);
        })
        .expect("audio worker");
    tx
}

fn run(rx: std::sync::mpsc::Receiver<Packet>, control: &Arc<AudioControl>) -> anyhow::Result<()> {
    let mut decoder = OpusReceiver::new()?;
    #[cfg(windows)]
    let mut output = windows::Output::new(control.clone())?;
    #[cfg(not(windows))]
    control.status("当前构建未启用系统音频输出");
    let mut reorder = Reorder::default();
    while !control.stopped() {
        match rx.recv_timeout(Duration::from_millis(5)) {
            Ok(packet) => reorder.push(packet),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(_) => {}
        }
        while let Some(packet) = reorder.pop(Instant::now()) {
            if packet.received.elapsed() > Duration::from_millis(120) {
                control.dropped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            control.receive_decode_us.store(
                packet.received.elapsed().as_micros().min(u32::MAX as u128) as u32,
                Ordering::Relaxed,
            );
            match decoder.decode(packet.timestamp, &packet.payload, control) {
                Ok(pcm) => {
                    #[cfg(windows)]
                    output.submit(&pcm, control)?;
                    #[cfg(not(windows))]
                    let _ = pcm;
                }
                Err(_) => {
                    control.dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
    Ok(())
}

#[cfg(any(windows, test))]
#[derive(Default)]
struct PcmQueue {
    samples: VecDeque<f32>,
}
#[cfg(any(windows, test))]
impl PcmQueue {
    fn submit(&mut self, samples: &[f32], rate: u32, channels: usize) {
        // Rejoin live sound after a device stall rather than playing the backlog.
        let max = rate as usize * channels * 30 / 1000; // 30ms; do not accumulate stale sound.
        if self.samples.len() + samples.len() > max {
            self.samples.clear();
        }
        let start = samples.len().saturating_sub(max) / channels * channels;
        self.samples.extend(&samples[start..]);
    }
    fn sample(&mut self) -> f32 {
        self.samples.pop_front().unwrap_or(0.0)
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    pub(super) struct Output {
        _stream: cpal::Stream,
        queue: Arc<Mutex<PcmQueue>>,
        rate: u32,
        channels: usize,
        failed: Arc<AtomicBool>,
        device_name: String,
        checked: Instant,
        phase: f64,
    }
    impl Output {
        pub fn new(control: Arc<AudioControl>) -> anyhow::Result<Self> {
            let device = cpal::default_host()
                .default_output_device()
                .ok_or_else(|| anyhow::anyhow!("没有默认播放设备"))?;
            let supported = device.default_output_config()?;
            let format = supported.sample_format();
            let limits = *supported.buffer_size();
            let mut config: cpal::StreamConfig = supported.into();
            let requested = (config.sample_rate.0 / 100).max(1);
            let requested = match limits {
                cpal::SupportedBufferSize::Range { min, max } => {
                    requested.clamp(min.max(1), max.max(1))
                }
                _ => requested,
            };
            config.buffer_size = cpal::BufferSize::Fixed(requested);
            let queue = Arc::new(Mutex::new(PcmQueue::default()));
            let failed = Arc::new(AtomicBool::new(false));
            let create = |config: &cpal::StreamConfig| match format {
                cpal::SampleFormat::F32 => build::<f32>(
                    &device,
                    config,
                    queue.clone(),
                    control.clone(),
                    failed.clone(),
                ),
                cpal::SampleFormat::I16 => build::<i16>(
                    &device,
                    config,
                    queue.clone(),
                    control.clone(),
                    failed.clone(),
                ),
                cpal::SampleFormat::U16 => build::<u16>(
                    &device,
                    config,
                    queue.clone(),
                    control.clone(),
                    failed.clone(),
                ),
                _ => Err(cpal::BuildStreamError::StreamConfigNotSupported),
            };
            let stream = match create(&config) {
                Ok(stream) => stream,
                Err(_) => {
                    config.buffer_size = cpal::BufferSize::Default;
                    create(&config)?
                }
            };
            stream.play()?;
            control.status("Opus · 48 kHz · 双声道");
            Ok(Self {
                _stream: stream,
                queue,
                rate: config.sample_rate.0,
                channels: config.channels as usize,
                failed,
                device_name: device.name().unwrap_or_default(),
                checked: Instant::now(),
                phase: 0.0,
            })
        }
        pub fn submit(
            &mut self,
            stereo: &[f32],
            control: &Arc<AudioControl>,
        ) -> anyhow::Result<()> {
            if self.failed.load(Ordering::Relaxed)
                || (self.checked.elapsed() > Duration::from_secs(1) && {
                    self.checked = Instant::now();
                    cpal::default_host()
                        .default_output_device()
                        .and_then(|d| d.name().ok())
                        .is_some_and(|name| name != self.device_name)
                })
            {
                *self = Self::new(control.clone())?;
            }
            let frames = stereo.len() / 2;
            let mut samples = Vec::with_capacity(
                (frames as f64 * self.rate as f64 / 48000.0) as usize * self.channels,
            );
            while self.phase < frames as f64 {
                let index = self.phase as usize;
                let next = (index + 1).min(frames - 1);
                let fraction = (self.phase - index as f64) as f32;
                let l = stereo[index * 2] + (stereo[next * 2] - stereo[index * 2]) * fraction;
                let r = stereo[index * 2 + 1]
                    + (stereo[next * 2 + 1] - stereo[index * 2 + 1]) * fraction;
                for channel in 0..self.channels {
                    samples.push(if self.channels == 1 {
                        (l + r) * 0.5
                    } else if channel == 0 {
                        l
                    } else if channel == 1 {
                        r
                    } else {
                        0.0
                    });
                }
                self.phase += 48000.0 / self.rate as f64;
            }
            self.phase -= frames as f64;
            let mut queue = self.queue.lock().unwrap();
            queue.submit(&samples, self.rate, self.channels);
            control.queue_ms.store(
                (queue.samples.len() * 1000 / (self.rate as usize * self.channels)) as u32,
                Ordering::Relaxed,
            );
            Ok(())
        }
    }
    fn build<T: cpal::SizedSample + cpal::FromSample<f32>>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        queue: Arc<Mutex<PcmQueue>>,
        control: Arc<AudioControl>,
        failed: Arc<AtomicBool>,
    ) -> Result<cpal::Stream, cpal::BuildStreamError> {
        let rate = config.sample_rate.0;
        let channels = config.channels as usize;
        device.build_output_stream(
            config,
            move |data: &mut [T], _| {
                control.output_block_us.store(
                    (data.len() * 1_000_000 / (rate as usize * channels)) as u32,
                    Ordering::Relaxed,
                );
                let gain = if control.muted.load(Ordering::Relaxed) {
                    0.0
                } else {
                    control.volume.load(Ordering::Relaxed).min(100) as f32 / 100.0
                };
                // Never wait for decode on the realtime device callback.
                if let Ok(mut q) = queue.try_lock() {
                    if q.samples.len() < data.len() {
                        control.underruns.fetch_add(1, Ordering::Relaxed);
                    }
                    for sample in data {
                        *sample = T::from_sample(q.sample() * gain);
                    }
                    control.queue_ms.store(
                        (q.samples.len() * 1000 / (rate as usize * channels)) as u32,
                        Ordering::Relaxed,
                    );
                } else {
                    for sample in data {
                        *sample = T::from_sample(0.0);
                    }
                }
            },
            move |_| {
                failed.store(true, Ordering::Relaxed);
            },
            None,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opus_roundtrip_and_single_packet_concealment() {
        let mut encoder =
            opus::Encoder::new(48000, opus::Channels::Stereo, opus::Application::Audio).unwrap();
        let mut decoder = OpusReceiver::new().unwrap();
        let c = AudioControl::default();
        let pcm: Vec<f32> = (0..1920)
            .map(|n| ((n / 2) as f32 * 0.06).sin() * 0.2)
            .collect();
        let p = encoder.encode_vec_float(&pcm, 4000).unwrap();
        let first = decoder.decode(u32::MAX - 959, &p, &c).unwrap();
        assert_eq!(first.len(), 1920);
        let _missing = encoder.encode_vec_float(&pcm, 4000).unwrap();
        let p = encoder.encode_vec_float(&pcm, 4000).unwrap();
        let next = decoder.decode(960, &p, &c).unwrap();
        assert_eq!(next.len(), 3840);
        assert_eq!(c.concealed.load(Ordering::Relaxed), 1);
        assert!(next.iter().all(|x| x.is_finite()));
    }
    #[test]
    fn reorder_recovers_late_packet_wraparound_and_limits_gap_wait() {
        let now = Instant::now();
        let mut q = Reorder::default();
        let packet = |n| Packet {
            sequence: n,
            timestamp: 0,
            payload: vec![],
            received: now,
        };
        q.push(packet(65534));
        assert_eq!(q.pop(now).unwrap().sequence, 65534);
        q.push(packet(0));
        assert!(q.pop(now).is_none());
        q.push(packet(65535));
        assert_eq!(q.pop(now).unwrap().sequence, 65535);
        assert_eq!(q.pop(now).unwrap().sequence, 0);
        q.push(packet(2));
        assert!(q.pop(now).is_none());
        assert_eq!(q.pop(now + Duration::from_millis(21)).unwrap().sequence, 2);
        q.push(packet(1));
        assert!(q.pop(now).is_none());
    }
    #[test]
    fn stalled_playback_discards_old_sound_and_underrun_is_silent() {
        let mut q = PcmQueue::default();
        q.submit(&vec![0.1; 1920], 48000, 2);
        q.submit(&vec![0.2; 1920], 48000, 2);
        assert_eq!(q.samples.len(), 1920);
        assert_eq!(q.sample(), 0.2);
        q.samples.clear();
        assert_eq!(q.sample(), 0.0);
    }
}
