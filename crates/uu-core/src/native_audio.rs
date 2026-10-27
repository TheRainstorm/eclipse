//! System audio owns its backend on an OS thread, independent of video/input.
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};
use uu_media::audio_capture::{CaptureSession, EncodedAudio, Source};
#[derive(Clone, Debug)]
pub(crate) enum State {
    Starting,
    Active(&'static str),
    Unavailable(String),
}
impl State {
    pub fn label(&self) -> String {
        match self {
            Self::Starting => uu_i18n::text("声音准备中").into(),
            Self::Active(backend) => {
                tracing::debug!(backend, "native output audio active");
                uu_i18n::text("声音已共享").into()
            }
            Self::Unavailable(message) => uu_i18n::format!("声音暂不可用：{message}；正在重试"),
        }
    }
}
pub(crate) struct Worker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub dropped: Arc<AtomicU64>,
    pub errors: Arc<AtomicU64>,
}
impl Worker {
    pub fn start(
        source: Source,
    ) -> anyhow::Result<(Self, mpsc::Receiver<EncodedAudio>, watch::Receiver<State>)> {
        let stop = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicU64::new(0));
        let errors = Arc::new(AtomicU64::new(0));
        let (output, frames) = mpsc::channel(8);
        let (status, state) = watch::channel(State::Starting);
        let (worker_stop, worker_dropped, worker_errors) =
            (stop.clone(), dropped.clone(), errors.clone());
        let thread = std::thread::Builder::new()
            .name("native-audio-capture".into())
            .spawn(move || {
                let started = Instant::now();
                while !worker_stop.load(Ordering::Acquire) {
                    let result = (|| -> anyhow::Result<()> {
                        let mut capture = CaptureSession::new(source, &worker_stop)?;
                        let epoch = (started.elapsed().as_micros() * 48_000 / 1_000_000)
                            .min(u64::MAX as u128) as u64;
                        status.send_replace(State::Active(capture.backend()));
                        while !worker_stop.load(Ordering::Acquire) {
                            if let Some(mut frame) = capture.next_packet()? {
                                frame.sample_index = frame.sample_index.saturating_add(epoch);
                                match output.try_send(frame) {
                                    Ok(()) => {}
                                    Err(mpsc::error::TrySendError::Full(_)) => {
                                        worker_dropped.fetch_add(1, Ordering::Relaxed);
                                    }
                                    Err(mpsc::error::TrySendError::Closed(_)) => return Ok(()),
                                }
                            } else {
                                std::thread::sleep(Duration::from_millis(2));
                            }
                        }
                        Ok(())
                    })();
                    if worker_stop.load(Ordering::Acquire) || output.is_closed() {
                        break;
                    }
                    if let Err(error) = result {
                        worker_errors.fetch_add(1, Ordering::Relaxed);
                        status.send_replace(State::Unavailable(error.to_string()));
                    } else {
                        break;
                    }
                    if source == Source::Synthetic {
                        break;
                    }
                    let retry = Instant::now() + Duration::from_secs(2);
                    while !worker_stop.load(Ordering::Acquire) && Instant::now() < retry {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            })?;
        Ok((
            Self {
                stop,
                thread: Some(thread),
                dropped,
                errors,
            },
            frames,
            state,
        ))
    }
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
    pub async fn stop(&mut self) {
        self.request_stop();
        if let Some(thread) = self.thread.take() {
            if !matches!(
                tokio::task::spawn_blocking(move || thread.join()).await,
                Ok(Ok(()))
            ) {
                self.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.request_stop();
    }
}
