//! Bounded capture/encode ownership shared by native and plugin protocols.
//! Authorization and signaling stay with the caller. The worker owns capture,
//! encoder and output leases; stop closes output before awaiting the producer.
use anyhow::{bail, ensure, Context, Result};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};
use uu_media::encoder::Preference as EncoderPreference;

#[derive(Default)]
struct ProducerTimings {
    capture_us: AtomicU64,
    encode_us: AtomicU64,
    backpressure_us: AtomicU64,
}

/// One owner for the blocking producer. Control/observation ports are shared
/// with the session; cancellation and joining remain private to this owner.
pub(super) struct Worker {
    pub(super) progress: Arc<crate::native_diagnostics::ProducerProgress>,
    pub(super) trace: Arc<crate::frame_trace::HostTrace>,
    pub(super) video_e2e: Arc<crate::video_e2e::Host>,
    pub(super) quality: crate::native_quality::Runtime,
    pub(super) quality_state: watch::Receiver<uu_media::encoder::Settings>,
    started: Instant,
    timings: Arc<ProducerTimings>,
    stopped: Arc<AtomicBool>,
    pub(super) captured: Arc<AtomicU64>,
    pub(super) retired_outputs: AtomicU64,
    pub(super) frames: mpsc::Receiver<Result<Sample>>,
    pub(super) encoder: watch::Receiver<String>,
    task: Option<tokio::task::JoinHandle<()>>,
}
pub(super) struct Sample {
    pub(super) frame: uu_media::EncodedFrame,
    pub(super) trace: Option<crate::frame_trace::Token>,
    pub(super) source_e2e: Option<crate::video_e2e::SourceEndpoint>,
    capture_lease: Option<crate::latest_capture::Lease>,
}
impl Sample {
    pub(super) fn current(&self) -> bool {
        self.capture_lease
            .as_ref()
            .is_none_or(|lease| lease.valid())
    }
}

/// Observed producer work; contains no transport or protocol counters.
pub(super) struct Statistics {
    pub encoder: String,
    pub captured: u64,
    pub producer_ms: u64,
    pub capture_us: u64,
    pub encode_us: u64,
    pub backpressure_us: u64,
}

impl std::ops::Deref for Sample {
    type Target = uu_media::EncodedFrame;
    fn deref(&self) -> &Self::Target {
        &self.frame
    }
}
impl Worker {
    pub(super) fn start(
        capture: Box<dyn uu_capture::Capture>,
        settings: uu_media::encoder::Settings,
        refresh: Arc<AtomicBool>,
        encoder: Box<dyn uu_media::VideoEncoder>,
        live_timing: Arc<crate::native_timing::HostTiming>,
    ) -> Self {
        Self::start_mode(capture, settings, refresh, encoder, live_timing, None)
    }
    fn start_mode(
        capture: Box<dyn uu_capture::Capture>,
        settings: uu_media::encoder::Settings,
        refresh: Arc<AtomicBool>,
        encoder: Box<dyn uu_media::VideoEncoder>,
        live_timing: Arc<crate::native_timing::HostTiming>,
        mode: Option<bool>,
    ) -> Self {
        let trace = live_timing.trace.clone();
        let video_e2e = live_timing.video_e2e.clone();
        trace.configure(settings, encoder.backend_name());
        let stopped = Arc::new(AtomicBool::new(false));
        let captured = Arc::new(AtomicU64::new(0));
        let (tx, frames) = mpsc::channel(2);
        let worker_stop = stopped.clone();
        let counter = captured.clone();
        let timings = Arc::new(ProducerTimings::default());
        let worker_timings = timings.clone();
        let started = Instant::now();
        let (encoder_status, encoder_state) = watch::channel(encoder.backend_name().to_owned());
        let (quality, changes) = crate::native_quality::Runtime::channel();
        let (quality_status, quality_state) = watch::channel(settings);
        let capture_control = quality.clone();
        let progress = Arc::new(crate::native_diagnostics::ProducerProgress::default());
        let worker_progress = progress.clone();
        let task = tokio::task::spawn_blocking(move || {
            let result = encode_source(
                capture,
                settings,
                encoder,
                refresh,
                worker_stop,
                counter,
                worker_timings,
                live_timing,
                encoder_status,
                changes,
                quality_status,
                &tx,
                mode,
                capture_control,
                worker_progress,
            );
            if let Err(error) = result {
                let _ = tx.blocking_send(Err(error));
            }
        });
        Self {
            progress,
            trace,
            video_e2e,
            quality,
            quality_state,
            started,
            timings,
            stopped,
            captured,
            retired_outputs: AtomicU64::default(),
            frames,
            encoder: encoder_state,
            task: Some(task),
        }
    }
    pub(super) fn statistics(&self) -> Statistics {
        Statistics {
            encoder: self.encoder.borrow().clone(),
            captured: self.captured.load(Ordering::Relaxed),
            producer_ms: self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            capture_us: self.timings.capture_us.load(Ordering::Relaxed),
            encode_us: self.timings.encode_us.load(Ordering::Relaxed),
            backpressure_us: self.timings.backpressure_us.load(Ordering::Relaxed),
        }
    }

    pub(super) async fn stop(&mut self) -> Result<()> {
        self.stopped.store(true, Ordering::Release);
        self.quality.wake();
        self.frames.close();
        if let Some(task) = self.task.take() {
            task.await.context("native capture worker failed")?;
        }
        Ok(())
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.quality.wake();
    }
}

fn apply_refresh_request(refresh: &AtomicBool, encoder: &mut dyn uu_media::VideoEncoder) {
    if refresh.swap(false, Ordering::AcqRel) {
        encoder.request_keyframe();
    }
}

fn encode_source(
    capture: Box<dyn uu_capture::Capture>,
    mut settings: uu_media::encoder::Settings,
    mut encoder: Box<dyn uu_media::VideoEncoder>,
    refresh: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    captured: Arc<AtomicU64>,
    timings: Arc<ProducerTimings>,
    live_timing: Arc<crate::native_timing::HostTiming>,
    encoder_status: watch::Sender<String>,
    changes: std::sync::mpsc::Receiver<crate::native_quality::Change>,
    quality_status: watch::Sender<uu_media::encoder::Settings>,
    output: &mpsc::Sender<Result<Sample>>,
    mode: Option<bool>,
    capture_control: crate::native_quality::Runtime,
    progress: Arc<crate::native_diagnostics::ProducerProgress>,
) -> Result<()> {
    use crate::native_diagnostics::ProducerPhase;
    let overlap = match mode {
        Some(enabled) => enabled,
        None => crate::capture_overlap::requested_from_env()?,
    };
    let mut capture = crate::capture_overlap::Input::new(
        capture,
        overlap,
        settings.fps,
        encoder.gpu_input(),
        encoder.gpu_vendor(),
    )?;
    capture_control.set_wake(capture.interrupt_handle());
    let result = (|| -> Result<()> {
        let mut backend = encoder.backend_name().to_owned();
        let mut interval = Duration::from_secs_f64(1.0 / settings.fps as f64);
        let mut next = Instant::now();
        encoder.request_keyframe();
        while !stopped.load(Ordering::Acquire) {
            progress.set(ProducerPhase::Pacing);
            if !capture.owner_pacing() {
                std::thread::sleep(next.saturating_duration_since(Instant::now()));
            }
            if stopped.load(Ordering::Acquire) {
                break;
            }
            if let Ok(change) = changes.try_recv() {
                progress.set(ProducerPhase::Reconfiguring);
                let result = (|| -> Result<u32> {
                    ensure!(
                        !change.reply.is_closed() && !stopped.load(Ordering::Acquire),
                        "{}",
                        uu_i18n::format!("画质请求已取消")
                    );
                    change.settings.validate()?;
                    let bitrate_kbps = change.settings.bitrate_for(
                        settings.width,
                        settings.height,
                        settings.codec,
                    );
                    let rate_control = change
                        .settings
                        .rate_control
                        .unwrap_or_else(|| {
                            crate::native_quality::RateMode::from_encoder(settings.rate_control)
                        })
                        .encoder(bitrate_kbps);
                    let updated = uu_media::encoder::Settings {
                        fps: change.settings.fps,
                        bitrate_kbps,
                        rate_control,
                        ..settings
                    };
                    updated.validate()?;
                    if updated.fps != settings.fps || updated.rate_control != settings.rate_control
                    {
                        // Driver contexts stay on their own threads. Build first,
                        // then start a fresh reference chain on the same transport.
                        let reopen = if encoder.is_hardware()
                            && updated.preference == EncoderPreference::Auto
                        {
                            uu_media::encoder::Settings {
                                preference: EncoderPreference::Hardware,
                                ..updated
                            }
                        } else {
                            updated
                        };
                        let replacement = uu_media::encoder::create_with_device(
                            reopen,
                            capture.encoder_input_device(),
                        )?;
                        ensure!(
                            !change.reply.is_closed() && !stopped.load(Ordering::Acquire),
                            "{}",
                            uu_i18n::format!("画质请求已取消")
                        );
                        capture.set_frame_rate(updated.fps)?;
                        if change.reply.is_closed() || stopped.load(Ordering::Acquire) {
                            capture.set_frame_rate(settings.fps)?;
                            bail!("{}", uu_i18n::format!("画质请求已取消"));
                        }
                        encoder = replacement;
                        if encoder.backend_name() != backend {
                            backend = encoder.backend_name().to_owned();
                            encoder_status.send_replace(backend.clone());
                        }
                        encoder.request_keyframe();
                        interval = Duration::from_secs_f64(1.0 / updated.fps as f64);
                        next = Instant::now();
                    } else {
                        encoder.set_bitrate(bitrate_kbps)?;
                    }
                    // The encoder owns whether a bitrate update invalidated its
                    // references; replacement encoders already start a new chain.
                    settings = updated;
                    live_timing
                        .trace
                        .configure(settings, encoder.backend_name());
                    live_timing.video_e2e.retire();
                    quality_status.send_replace(settings);
                    Ok(bitrate_kbps)
                })()
                .map_err(|error| error.to_string());
                let _ = change.reply.send(result);
            }
            progress.set(ProducerPhase::Capturing);
            capture.prefer_gpu(encoder.gpu_input(), encoder.gpu_vendor())?;
            let observed = match capture
                .next(live_timing.trace.enabled() || live_timing.video_e2e.enabled())
            {
                Ok(frame) => frame,
                Err(uu_capture::CaptureError::Suspended(_)) => {
                    // Desktop transitions must not terminate an authenticated
                    // transport or transmit a cached image of the old desktop.
                    refresh.store(true, Ordering::Release);
                    next = Instant::now() + Duration::from_millis(25);
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            if observed.retired {
                live_timing
                    .trace
                    .configure(settings, encoder.backend_name());
                live_timing.video_e2e.retire();
                refresh.store(true, Ordering::Release);
            }
            let capture_ready = observed.ready;
            let capture_started = observed.started;
            let source_info = observed.source;
            let frame = &observed.frame;
            let capture_us = capture_ready
                .duration_since(capture_started)
                .as_micros()
                .min(u128::from(u64::MAX)) as u64;
            timings.capture_us.fetch_add(capture_us, Ordering::Relaxed);
            captured.fetch_add(1, Ordering::Relaxed);
            // RTCP already coalesces and limits recovery requests. Once requested,
            // renew references on the next frame, even just after a previous IDR.
            // Swap also preserves a request arriving while this frame is encoded.
            apply_refresh_request(&refresh, encoder.as_mut());
            let started = Instant::now();
            let frame_token = live_timing.trace.captured(
                frame.pts.0,
                source_info,
                frame.gpu,
                capture_started,
                capture_ready,
                started,
            );
            live_timing.video_e2e.captured(frame.pts.0, source_info);
            progress.set(ProducerPhase::Encoding);
            let encoded_frames = encoder.encode(frame)?;
            if !observed.current() {
                capture.retired_output();
                refresh.store(true, Ordering::Release);
                continue;
            }
            let output_ready = Instant::now();
            live_timing.trace.mark(
                frame_token,
                crate::frame_trace::Event::EncodeReturn,
                output_ready,
            );
            if encoder.backend_name() != backend {
                backend = encoder.backend_name().to_owned();
                encoder_status.send_replace(backend.clone());
                // A runtime fallback is a new measurement generation. Do not
                // attribute its first output to the retired backend's input.
                live_timing.trace.configure(settings, &backend);
                live_timing.video_e2e.retire();
            }
            let encode_us = output_ready
                .duration_since(started)
                .as_micros()
                .min(u128::from(u64::MAX)) as u64;
            timings.encode_us.fetch_add(encode_us, Ordering::Relaxed);
            let started = Instant::now();
            progress.set(ProducerPhase::Enqueueing);
            for encoded in encoded_frames {
                let token =
                    live_timing
                        .trace
                        .output(encoded.pts.0, encoded.is_keyframe, output_ready);
                let source_e2e = live_timing.video_e2e.output(encoded.pts.0);
                if output
                    .blocking_send(Ok(Sample {
                        frame: encoded,
                        trace: token,
                        source_e2e,
                        capture_lease: observed.lease.clone(),
                    }))
                    .is_err()
                {
                    return Ok(());
                }
                live_timing.trace.mark(
                    token,
                    crate::frame_trace::Event::EnqueueReturn,
                    Instant::now(),
                );
            }
            let wait_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
            timings
                .backpressure_us
                .fetch_add(wait_us, Ordering::Relaxed);
            live_timing.video(capture_us, encode_us, wait_us);
            next = crate::video_pacing::next_deadline(next, interval, Instant::now());
        }
        Ok(())
    })();
    progress.set(ProducerPhase::Stopping);
    let joined = capture.finish();
    capture_control.set_wake(None);
    progress.set(ProducerPhase::Stopped);
    result.and(joined)
}

#[cfg(test)]
mod tests;
