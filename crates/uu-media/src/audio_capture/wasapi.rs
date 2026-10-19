//! CPAL's output-device input stream is documented WASAPI loopback.
use super::StereoRate;
use anyhow::{ensure, Context, Result};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, SampleFormat, SizedSample,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender},
    Arc,
};

pub(super) struct Capture {
    _stream: cpal::Stream,
    receiver: Receiver<(std::time::Instant, Vec<f32>)>,
    failed: Arc<AtomicBool>,
    rate: StereoRate,
}
fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    tx: SyncSender<(std::time::Instant, Vec<f32>)>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels);
    let maximum = config.sample_rate.0 as usize * channels / 5;
    let invalid = failed.clone();
    Ok(device.build_input_stream(
        config,
        move |data: &[T], _| {
            if data.len() > maximum || data.len() % channels != 0 {
                invalid.store(true, Ordering::Release);
                return;
            }
            let mut stereo = Vec::with_capacity(data.len() / channels * 2);
            for frame in data.chunks_exact(channels) {
                stereo.extend(super::stereo_frame(channels, |i| {
                    frame[i].to_sample::<f32>()
                }));
            }
            let _ = tx.try_send((std::time::Instant::now(), stereo));
        },
        move |_| {
            failed.store(true, Ordering::Release);
        },
        None,
    )?)
}
impl Capture {
    pub fn new() -> Result<Self> {
        let device = cpal::default_host()
            .default_output_device()
            .context("no default system-output device")?;
        let supported = device.default_output_config()?;
        ensure!(
            (1..=8).contains(&supported.channels()),
            "unsupported system-output channel count"
        );
        let rate = StereoRate::new(supported.sample_rate().0)?;
        let config = supported.config();
        let (tx, receiver) = mpsc::sync_channel(8);
        let failed = Arc::new(AtomicBool::new(false));
        macro_rules! stream {
            ($ty:ty) => {
                build::<$ty>(&device, &config, tx, failed.clone())?
            };
        }
        let stream = match supported.sample_format() {
            SampleFormat::F32 => stream!(f32),
            SampleFormat::F64 => stream!(f64),
            SampleFormat::I8 => stream!(i8),
            SampleFormat::I16 => stream!(i16),
            SampleFormat::I32 => stream!(i32),
            SampleFormat::I64 => stream!(i64),
            SampleFormat::U8 => stream!(u8),
            SampleFormat::U16 => stream!(u16),
            SampleFormat::U32 => stream!(u32),
            SampleFormat::U64 => stream!(u64),
            _ => anyhow::bail!("unsupported system-output sample format"),
        };
        stream.play()?;
        Ok(Self {
            _stream: stream,
            receiver,
            failed,
            rate,
        })
    }
    pub fn poll(&mut self) -> Result<Vec<Vec<f32>>> {
        ensure!(
            !self.failed.load(Ordering::Acquire),
            "system-output device stopped"
        );
        let mut blocks = Vec::new();
        for _ in 0..8 {
            match self.receiver.try_recv() {
                Ok((when, block)) => {
                    if when.elapsed() <= std::time::Duration::from_millis(100) {
                        blocks.push(self.rate.convert(&block)?);
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    anyhow::bail!("system-output capture stopped")
                }
            }
        }
        Ok(blocks)
    }
}
