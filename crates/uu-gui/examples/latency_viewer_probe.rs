//! Bounded, receive-only native session through the production video renderer.
//! No input injection, account access, audio playback, snapshots or display changes.
#[allow(dead_code)]
#[path = "../src/video_texture.rs"]
mod video_texture;
#[cfg(windows)]
#[path = "../src/wgl_interop.rs"]
mod wgl_interop;
use anyhow::{ensure, Context, Result};
use eframe::egui;
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::{atomic::Ordering, Arc, Mutex},
    time::{Duration, Instant},
};
use uu_core::{display_frames::FrameReceiver, media::MediaStats, native, native_quality};

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Config {
    invitation: PathBuf,
    report: PathBuf,
    seconds: u64,
    warmup_seconds: u64,
    fps: u32,
    bounds: [u32; 2],
    bitrate_kbps: u32,
    codec: uu_proto::media::VideoCodec,
    vsync: bool,
}
// Exact samples, bounded by the probe's maximum duration and supported cadence.
// A full buffer fails the probe instead of silently truncating its tail.
#[derive(Default)]
struct Samples {
    values: Vec<f64>,
    overflow: bool,
}
impl Samples {
    fn add(&mut self, value: f64) {
        if self.values.len() < 900_000 {
            self.values.push(value);
        } else {
            self.overflow = true;
        }
    }
    fn report(&self, period_ms: f64) -> serde_json::Value {
        let mut values = self.values.clone();
        values.sort_by(f64::total_cmp);
        if values.is_empty() {
            return serde_json::Value::Null;
        }
        let n = values.len();
        let rank = |p: usize, d: usize| values[(n * p).div_ceil(d).saturating_sub(1).min(n - 1)];
        serde_json::json!({"samples":n,"p50":rank(50,100),"p95":rank(95,100),"p99":rank(99,100),
            "p999":rank(999,1000),"max":values[n-1],"overflow":self.overflow,
            "over_2t":values.iter().filter(|v|**v>2.*period_ms).count(),
            "over_50ms":values.iter().filter(|v|**v>50.).count(),
            "over_100ms":values.iter().filter(|v|**v>100.).count(),
            "over_250ms":values.iter().filter(|v|**v>250.).count()})
    }
}
#[derive(Default)]
struct Draws {
    last_serial: u64,
    first: Option<Instant>,
    last: Option<Instant>,
    count: u64,
    gap: Samples,
    mailbox_to_draw: Samples,
}
struct Probe {
    gl: Arc<eframe::glow::Context>,
    images: FrameReceiver,
    stats: Arc<MediaStats>,
    started: Instant,
    first: Arc<Mutex<Option<Instant>>>,
    warmup: Duration,
    duration: Duration,
    texture: Option<video_texture::VideoTexture>,
    pending: Option<(u64, Instant)>,
    serial: u64,
    draws: Arc<Mutex<Draws>>,
    paths: Arc<Mutex<BTreeSet<String>>>,
    samples: Arc<Mutex<Vec<serde_json::Value>>>,
    next_sample: Instant,
    ended: Arc<std::sync::atomic::AtomicBool>,
    duration_completed: Arc<std::sync::atomic::AtomicBool>,
    mailbox_counts: Arc<Mutex<[u64; 3]>>,
}
impl eframe::App for Probe {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        let first = *self.first.lock().unwrap();
        let complete = first.is_some_and(|t| t.elapsed() >= self.warmup + self.duration);
        if complete {
            self.duration_completed.store(true, Ordering::Release);
        }
        *self.mailbox_counts.lock().unwrap() = self.images.delivery_counts(0);
        if complete
            || self.started.elapsed() > self.warmup + self.duration + Duration::from_secs(30)
            || self.ended.load(Ordering::Acquire)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        while let Ok((image, published)) = self.images.try_recv_timed_image() {
            self.first.lock().unwrap().get_or_insert(published);
            self.serial += 1;
            self.pending = Some((self.serial, published));
            match &mut self.texture {
                Some(texture) => texture.set(ctx, image),
                None => {
                    self.texture =
                        Some(video_texture::VideoTexture::new(ctx, Some(&self.gl), image))
                }
            }
        }
        if let Some(texture) = &self.texture {
            egui::CentralPanel::default()
                .frame(egui::Frame::none())
                .show(ctx, |ui| {
                    texture.paint(ui, ui.max_rect());
                    if let Some((serial, published)) = self.pending {
                        let draws = self.draws.clone();
                        let measure_from = self.first.lock().unwrap().map(|t| t + self.warmup);
                        ui.painter().add(egui::PaintCallback {
                            rect: ui.max_rect(),
                            callback: Arc::new(eframe::egui_glow::CallbackFn::new(move |_, _| {
                                let now = Instant::now();
                                let mut draws = draws.lock().unwrap();
                                if serial <= draws.last_serial {
                                    return;
                                }
                                draws.last_serial = serial;
                                if measure_from.is_some_and(|t| published >= t) {
                                    if let Some(last) = draws.last {
                                        draws
                                            .gap
                                            .add(now.duration_since(last).as_secs_f64() * 1000.);
                                    }
                                    draws.first.get_or_insert(now);
                                    draws.last = Some(now);
                                    draws.count += 1;
                                    draws
                                        .mailbox_to_draw
                                        .add(now.duration_since(published).as_secs_f64() * 1000.);
                                }
                            })),
                        });
                    }
                });
            if self.draws.lock().unwrap().last_serial > 0 {
                self.paths
                    .lock()
                    .unwrap()
                    .insert(texture.render_path().into());
            }
        }
        if Instant::now() >= self.next_sample {
            self.next_sample = Instant::now() + Duration::from_secs(1);
            let (w, h) = self.stats.res();
            self.samples.lock().unwrap().push(serde_json::json!({
                "at_ms":self.started.elapsed().as_millis(),"decoded":self.stats.decoded_total.load(Ordering::Relaxed),
                "width":w,"height":h,"backend":self.stats.backend(),
                "decode_ms_window_mean":self.stats.decode_ms(),
                "errors":self.stats.errors.load(Ordering::Relaxed),
                "queue_drops":self.stats.queue_drops.load(Ordering::Relaxed),
                "rtx_packets":self.stats.rtx_packets.load(Ordering::Relaxed),
                "rtt_us":self.stats.rtt_us.load(Ordering::Relaxed),
                "cadence":self.stats.frame_cadence.lock().unwrap().clone(),
                "host_frame_trace":self.stats.native_frame_trace.lock().unwrap().clone(),
            }));
        }
        // Wake on decoder publication, plus a bounded watchdog if the stream dies.
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}
fn main() -> Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .context("provide a private probe configuration JSON")?;
    let config: Config = serde_json::from_slice(&std::fs::read(path)?)?;
    ensure!(
        (5..=3600).contains(&config.seconds) && (1..=30).contains(&config.warmup_seconds),
        "invalid duration"
    );
    let quality = native_quality::Request {
        fps: config.fps,
        bounds: Some(config.bounds),
        bitrate_kbps: Some(config.bitrate_kbps),
        codec: Some(config.codec),
    };
    quality.validate()?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut report = options
        .open(&config.report)
        .context("report must be a new private file")?;
    let invitation = uu_core::native_pairing::Invitation::load(&config.invitation)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let stats = Arc::new(MediaStats::default());
    stats.audio.output_enabled.store(false, Ordering::Release);
    stats.native_quality_request.set(quality).unwrap();
    let (frames, images) = uu_core::media::frame_channel();
    images.enable_planar();
    let (stop, rx) = tokio::sync::watch::channel(false);
    let draws = Arc::new(Mutex::new(Draws::default()));
    let paths = Arc::new(Mutex::new(BTreeSet::new()));
    let samples = Arc::new(Mutex::new(Vec::new()));
    let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let first = Arc::new(Mutex::new(None));
    let duration_completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mailbox_counts = Arc::new(Mutex::new([0; 3]));
    let started = Instant::now();
    let worker_stats = stats.clone();
    let worker_ended = ended.clone();
    let mut worker = runtime.spawn(async move {
        let result = native::view(&invitation, frames, worker_stats, rx).await;
        worker_ended.store(true, Ordering::Release);
        result
    });
    let ui_stats = stats.clone();
    let ui_draws = draws.clone();
    let ui_paths = paths.clone();
    let ui_samples = samples.clone();
    let ui_first = first.clone();
    let ui_complete = duration_completed.clone();
    let ui_counts = mailbox_counts.clone();
    let duration = Duration::from_secs(config.seconds);
    let warmup = Duration::from_secs(config.warmup_seconds);
    let gui = eframe::run_native(
        "Eclipse latency test (receive only)",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Glow,
            vsync: config.vsync,
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([960., 540.])
                .with_active(false),
            ..Default::default()
        },
        Box::new(move |cc| {
            let gl = cc.gl.clone().ok_or("missing OpenGL context")?;
            video_texture::enable_gpu_video(&gl);
            let ctx = cc.egui_ctx.clone();
            images.set_waker(move || ctx.request_repaint());
            Ok(Box::new(Probe {
                gl,
                images,
                stats: ui_stats,
                started,
                first: ui_first,
                warmup,
                duration,
                texture: None,
                pending: None,
                serial: 0,
                draws: ui_draws,
                paths: ui_paths,
                samples: ui_samples,
                next_sample: started,
                ended,
                duration_completed: ui_complete,
                mailbox_counts: ui_counts,
            }))
        }),
    );
    let _ = stop.send(true);
    let closed = runtime
        .block_on(async { tokio::time::timeout(Duration::from_secs(10), &mut worker).await });
    if closed.is_err() {
        worker.abort();
    }
    let close_ok = matches!(closed, Ok(Ok(Ok(()))));
    let close_error = if close_ok {
        None
    } else {
        Some(format!("{closed:?}"))
    };
    let draws = draws.lock().unwrap();
    let period = 1000. / config.fps as f64;
    let covered_ms = draws
        .first
        .zip(draws.last)
        .map_or(0., |(a, b)| b.duration_since(a).as_secs_f64() * 1000.);
    let full_duration = duration_completed.load(Ordering::Acquire);
    let completed = gui.is_ok()
        && close_ok
        && full_duration
        && draws.count > 1
        && stats.errors.load(Ordering::Relaxed) == 0;
    let value = serde_json::json!({"version":1,"completed":completed,
        "scope":"native desktop or generated source; production video texture in owned test window; paint callback API, not scanout",
        "configuration":config,"first_frame_ms":first.lock().unwrap().map(|t: Instant|t.duration_since(started).as_secs_f64()*1000.),
        "coverage_ms":covered_ms,"painted":draws.count,"paint_fps":(draws.count.saturating_sub(1)) as f64*1000./covered_ms.max(1.),
        "paint_gap_ms":draws.gap.report(period),"mailbox_to_draw_ms":draws.mailbox_to_draw.report(period),
        "decoder_backend":stats.backend(),"render_paths":*paths.lock().unwrap(),
        "decoded":stats.decoded_total.load(Ordering::Relaxed),"errors":stats.errors.load(Ordering::Relaxed),
        "decode_display_skips":stats.decoded_display_skips.load(Ordering::Relaxed),
        "mailbox_counts":*mailbox_counts.lock().unwrap(),
        "samples":*samples.lock().unwrap(),"host_frame_trace":stats.native_frame_trace.lock().unwrap().clone(),
        "close_error":close_error,"gui_error":gui.as_ref().err().map(ToString::to_string)});
    report.write_all(serde_json::to_string_pretty(&value)?.as_bytes())?;
    report.sync_all()?;
    ensure!(
        completed,
        "native renderer probe failed; inspect its private report"
    );
    Ok(())
}
