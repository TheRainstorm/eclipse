//! Opt-in, bounded raw Present evidence. JSON and file IO run off the renderer.
use super::frame_observation::{FrameIdentity, FrameObservation};
use serde::Serialize;
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender},
        Arc, Mutex, OnceLock,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use uu_core::display_frames::hdr::RawPresentObservation;

const CAPACITY: usize = 512;
const MAX_BATCH: usize = 256;
const MAX_BYTES: usize = 32 * 1024 * 1024;
const FLUSH: Duration = Duration::from_secs(1);
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Association {
    Matched,
    NoNativeImage,
    MissingDecodedMetadata,
    RetiredSession,
    NativeImageMismatch,
}
#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct Row {
    pub association: Association,
    pub frame: Option<FrameIdentity>,
    pub ui_reused: bool,
    pub pixel_inspection: bool,
    pub raw: RawPresentObservation,
}
impl Row {
    pub(crate) fn new(
        raw: RawPresentObservation,
        offered: Option<(u64, u64)>,
        frame: Option<&FrameObservation>,
        ui_reused: bool,
        pixel_inspection: bool,
    ) -> Self {
        let (association, identity) = match offered {
            None => (Association::NoNativeImage, None),
            Some((id, ready)) if !raw.matches_native_image(id, ready) => {
                (Association::NativeImageMismatch, None)
            }
            Some(_) => match frame {
                None => (Association::MissingDecodedMetadata, None),
                Some(frame) => match frame.current_identity() {
                    Some(identity) => (Association::Matched, Some(identity)),
                    None => (Association::RetiredSession, None),
                },
            },
        };
        Self {
            association,
            frame: identity,
            ui_reused,
            pixel_inspection,
            raw,
        }
    }
}
#[derive(Clone)]
pub(crate) struct Producer {
    tx: SyncSender<Message>,
    dropped: Arc<AtomicU64>,
    attempts: Arc<AtomicU64>,
    active: Arc<AtomicBool>,
}
enum Message {
    Row(Row),
    Shutdown,
}
impl Producer {
    pub(crate) fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
    pub(crate) fn record(&self, row: Row) {
        if self.active() {
            self.attempts.fetch_add(1, Ordering::Relaxed);
            if self.tx.try_send(Message::Row(row)).is_err() {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}
struct Recorder {
    producer: Producer,
    stop: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<io::Result<bool>>>>,
    complete: AtomicBool,
}
impl Recorder {
    fn shutdown(&self, timeout: Duration) -> bool {
        self.producer.active.store(false, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        let _ = self.producer.tx.try_send(Message::Shutdown);
        let Ok(mut slot) = self.worker.try_lock() else {
            return false;
        };
        let Some(worker) = slot.take() else {
            return self.complete.load(Ordering::Acquire);
        };
        let started = Instant::now();
        while !worker.is_finished() && started.elapsed() < timeout {
            thread::sleep(Duration::from_millis(5).min(timeout.saturating_sub(started.elapsed())));
        }
        if !worker.is_finished() {
            *slot = Some(worker);
            return false;
        }
        let complete = matches!(worker.join(), Ok(Ok(true)));
        self.complete.store(complete, Ordering::Release);
        complete
    }
}
static TRACE: OnceLock<Option<Recorder>> = OnceLock::new();
/// The variable is an explicit *new output file path*, not a boolean. Without
/// it no worker/file/statistics query is created. One sink serves all windows.
pub(crate) fn from_env() -> Option<Producer> {
    TRACE
        .get_or_init(|| {
            let path = std::env::var_os("ECLIPSE_NATIVE_PRESENT_TRACE_FILE")?;
            match open(Path::new(&path)) {
                Ok(recorder) => Some(recorder),
                Err(error) => {
                    eprintln!("native Present observations disabled: {error}");
                    None
                }
            }
        })
        .as_ref()
        .map(|recorder| recorder.producer.clone())
}
/// Call once after the native event loop stops, never per frame or viewport.
/// Missing/false terminal records make the observation window incomplete.
pub(crate) fn shutdown() {
    if let Some(Some(recorder)) = TRACE.get() {
        if !recorder.shutdown(Duration::from_secs(2)) {
            eprintln!("native Present trace incomplete: writer did not confirm a complete window within the shutdown budget");
        }
    }
}
fn open(path: &Path) -> io::Result<Recorder> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    let (tx, rx) = sync_channel(CAPACITY);
    let dropped = Arc::new(AtomicU64::new(0));
    let attempts = Arc::new(AtomicU64::new(0));
    let active = Arc::new(AtomicBool::new(true));
    let stop = Arc::new(AtomicBool::new(false));
    let worker_dropped = dropped.clone();
    let worker_attempts = attempts.clone();
    let worker_stop = stop.clone();
    let worker_active = active.clone();
    let worker = thread::Builder::new()
        .name("uu-present-observer".into())
        .spawn(move || {
            let result = write_batches(
                file,
                rx,
                &worker_dropped,
                &worker_attempts,
                &worker_stop,
                MAX_BYTES,
            );
            if let Err(error) = &result {
                eprintln!("native Present observation writer stopped: {error}");
            }
            worker_active.store(false, Ordering::Release);
            result
        })?;
    Ok(Recorder {
        producer: Producer {
            tx,
            dropped,
            attempts,
            active,
        },
        stop,
        worker: Mutex::new(Some(worker)),
        complete: AtomicBool::new(false),
    })
}
#[derive(Serialize)]
struct Batch<'a> {
    event: &'static str,
    version: u32,
    pid: u32,
    endpoint: &'static str,
    dropped_rows_total: u64,
    rows: &'a [Row],
}
fn write_batches(
    mut file: File,
    rx: Receiver<Message>,
    dropped: &AtomicU64,
    attempts: &AtomicU64,
    stop: &AtomicBool,
    limit: usize,
) -> io::Result<bool> {
    let mut rows = Vec::with_capacity(MAX_BATCH);
    let mut bytes = 0;
    let mut written = 0u64;
    let mut started = Instant::now();
    let mut ended = false;
    loop {
        match rx.recv_timeout(FLUSH.saturating_sub(started.elapsed())) {
            Ok(Message::Row(row)) => rows.push(row),
            Ok(Message::Shutdown) => ended = true,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => ended = true,
        }
        if stop.load(Ordering::Acquire) {
            ended = true;
        }
        if ended {
            for message in rx.try_iter() {
                if let Message::Row(row) = message {
                    rows.push(row);
                }
            }
        }
        if rows.len() >= MAX_BATCH || started.elapsed() >= FLUSH || ended {
            if !rows.is_empty() {
                let batch = Batch {
                    event: "native_present_observations",
                    version: 1,
                    pid: std::process::id(),
                    endpoint: "raw_submission_only",
                    dropped_rows_total: dropped.load(Ordering::Relaxed),
                    rows: &rows,
                };
                let mut data = serde_json::to_vec(&batch)?;
                data.push(b'\n');
                if bytes + data.len() > limit {
                    write_terminal(
                        &mut file,
                        false,
                        "file_limit",
                        attempts.load(Ordering::Relaxed),
                        written,
                        attempts.load(Ordering::Relaxed).saturating_sub(written),
                        bytes,
                    )?;
                    return Ok(false);
                }
                file.write_all(&data)?;
                file.flush()?;
                bytes += data.len();
                written += rows.len() as u64;
                rows.clear();
            }
            started = Instant::now();
        }
        if ended {
            let total = attempts.load(Ordering::Acquire);
            let missing = dropped.load(Ordering::Acquire);
            let complete = total == written && missing == 0;
            write_terminal(
                &mut file,
                complete,
                if stop.load(Ordering::Acquire) {
                    "normal_shutdown"
                } else {
                    "producers_closed"
                },
                total,
                written,
                missing,
                bytes,
            )?;
            return Ok(complete);
        }
    }
}
fn write_terminal(
    file: &mut File,
    complete: bool,
    reason: &str,
    total: u64,
    written: u64,
    dropped: u64,
    bytes: usize,
) -> io::Result<()> {
    serde_json::to_writer(
        &mut *file,
        &serde_json::json!({"event":"native_present_trace_end","version":1,"pid":std::process::id(),"endpoint":"raw_submission_only","complete":complete,"reason":reason,"attempted_rows":total,"rows_written":written,"dropped_rows_total":dropped,"batch_bytes":bytes}),
    )?;
    file.write_all(b"\n")?;
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uu_core::{
        display_frames::{DecodedObservation, DisplayImage, RawFrame, Timestamp, YuvColor},
        media::MediaStats,
    };
    fn fixture() -> (Arc<MediaStats>, FrameObservation) {
        let stats = Arc::new(MediaStats::default());
        let image = DisplayImage::Decoded(
            0,
            RawFrame::new_i420(2, 2, vec![16; 6], Timestamp(123)),
            YuvColor::Limited709,
        );
        let observation = FrameObservation::new(
            &stats,
            &image,
            Some(DecodedObservation {
                media_epoch: 0,
                track: 0,
                pts_us: 123,
                token: None,
                receive: None,
            }),
        )
        .unwrap();
        (stats, observation)
    }
    fn raw() -> RawPresentObservation {
        RawPresentObservation {
            version: 1,
            struct_size: 176,
            native_source_id: 9,
            native_source_ready: 4,
            ..Default::default()
        }
    }
    #[test]
    fn rows_match_the_offered_gpu_image_and_clear_retired_sessions() {
        let (stats, frame) = fixture();
        let row = Row::new(raw(), Some((9, 4)), Some(&frame), true, false);
        assert_eq!(row.association, Association::Matched);
        assert_eq!(row.frame.unwrap().actual_pts_us, 123);
        assert_eq!(
            Row::new(raw(), Some((9, 5)), Some(&frame), false, false).association,
            Association::NativeImageMismatch
        );
        stats.native_media_retired.store(true, Ordering::Release);
        assert_eq!(
            Row::new(raw(), Some((9, 4)), Some(&frame), false, false).association,
            Association::RetiredSession
        );
        assert!(Row::new(raw(), None, None, false, false).frame.is_none());
    }
    #[test]
    fn producer_is_bounded_nonblocking_and_writer_batches_metadata() {
        let (tx, rx) = sync_channel(2);
        let dropped = Arc::new(AtomicU64::new(0));
        let attempts = Arc::new(AtomicU64::new(0));
        let producer = Producer {
            tx,
            dropped: dropped.clone(),
            attempts: attempts.clone(),
            active: Arc::new(AtomicBool::new(true)),
        };
        for _ in 0..10 {
            producer.record(Row::new(raw(), None, None, false, false));
        }
        assert_eq!(dropped.load(Ordering::Relaxed), 8);
        drop(producer);
        let path = std::env::temp_dir().join(format!(
            "uu-present-observations-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        assert!(!write_batches(
            file,
            rx,
            &dropped,
            &attempts,
            &AtomicBool::new(false),
            MAX_BYTES
        )
        .unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "two frames must be one background JSON batch plus a terminal"
        );
        let batch: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(batch["endpoint"], "raw_submission_only");
        assert_eq!(batch["rows"].as_array().unwrap().len(), 2);
        assert_eq!(batch["dropped_rows_total"], 8);
        let terminal: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(terminal["complete"], false);
        assert_eq!(terminal["attempted_rows"], 10);
        assert_eq!(terminal["rows_written"], 2);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn explicit_shutdown_drains_static_lifetime_clones_and_confirms_terminal() {
        let path = std::env::temp_dir().join(format!(
            "uu-present-shutdown-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let recorder = open(&path).unwrap();
        let still_owned = recorder.producer.clone();
        for _ in 0..3 {
            still_owned.record(Row::new(raw(), None, None, false, false));
        }
        assert!(
            recorder.shutdown(Duration::from_secs(2)),
            "shutdown cannot depend on every Producer being dropped"
        );
        assert!(!still_owned.active());
        let text = std::fs::read_to_string(&path).unwrap();
        let terminal: serde_json::Value =
            serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(terminal["event"], "native_present_trace_end");
        assert_eq!(terminal["complete"], true);
        assert_eq!(terminal["reason"], "normal_shutdown");
        assert_eq!(terminal["attempted_rows"], 3);
        assert_eq!(terminal["rows_written"], 3);
        assert_eq!(terminal["dropped_rows_total"], 0);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn shutdown_budget_does_not_wait_forever_for_a_stalled_worker() {
        let (tx, _rx) = sync_channel(1);
        let (release, blocked) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            blocked.recv().unwrap();
            Ok(true)
        });
        let recorder = Recorder {
            producer: Producer {
                tx,
                dropped: Arc::new(AtomicU64::new(0)),
                attempts: Arc::new(AtomicU64::new(0)),
                active: Arc::new(AtomicBool::new(true)),
            },
            stop: Arc::new(AtomicBool::new(false)),
            worker: Mutex::new(Some(worker)),
            complete: AtomicBool::new(false),
        };
        let started = Instant::now();
        assert!(!recorder.shutdown(Duration::from_millis(10)));
        assert!(started.elapsed() < Duration::from_millis(500));
        release.send(()).unwrap();
        assert!(recorder.shutdown(Duration::from_secs(2)));
    }
}
