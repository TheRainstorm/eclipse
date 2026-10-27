//! Headless, read-only-input session probe using the same controller as the GUI.
//! cargo run -p uu-compat --example session_probe -- DEVICE [SCREEN_ID ...]
//! Explicit source selections affect capture only. No mouse/keyboard/clipboard
//! data is sent. Always release the session, including after a failed check.
use anyhow::{bail, Context, Result};
use std::{
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};
use uu_compat::{
    controller::{self, ControllerEvent, ControllerHandle},
    media::MediaStats,
    Config,
};

struct Session(ControllerHandle);
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.0.close.send(());
        let until = Instant::now() + Duration::from_secs(5);
        while !self.0.stopped.load(Ordering::Acquire) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        println!(
            "probe cleanup stopped={}",
            self.0.stopped.load(Ordering::Acquire)
        );
    }
}
fn wait(
    session: &mut Session,
    timeout: Duration,
    label: &str,
    ready: impl Fn(&MediaStats) -> bool,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        while let Ok(event) = session.0.events.try_recv() {
            match event {
                ControllerEvent::Status(s) => println!("status: {s}"),
                ControllerEvent::Error(e) => bail!("{e}"),
            }
        }
        while session.0.frames.try_recv().is_ok() {}
        if ready(&session.0.stats) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("probe timeout: {label}");
        }
        if session.0.stopped.load(Ordering::Acquire) {
            bail!("session stopped: {label}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn report(stats: &MediaStats, phase: &str) {
    let screens = stats
        .remote_screens
        .lock()
        .unwrap()
        .clone()
        .with_desktop_capture(*stats.desktop_capture_source.lock().unwrap());
    println!(
        "probe {}",
        serde_json::json!({"phase":phase,
        "control":stats.control_open.load(Ordering::Acquire),
        "video_size":stats.res(),
        "decoded":stats.decoded_total.load(Ordering::Acquire),"errors":stats.errors.load(Ordering::Acquire),
        "settings":stats.settings_response.load(Ordering::Acquire),"current":screens.current_id,
        "replies":*stats.display_responses.lock().unwrap(),
        "screens":screens.screens.iter().map(|s|serde_json::json!({"id":s.id,"track":s.video_track_index,"name":s.display_name,"primary":s.primary})).collect::<Vec<_>>() })
    );
}
fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let device = args
        .next()
        .context("usage: session_probe DEVICE [SCREEN_ID ...]")?;
    let screens: Vec<i32> = args
        .map(|v| {
            v.parse::<i32>()
                .context("source ID must be a nonnegative integer")
        })
        .collect::<Result<_>>()?;
    if screens.iter().any(|s| *s < 0) {
        bail!("source ID must be nonnegative");
    }
    let config = Config::from_env()?;
    let stats = Arc::new(MediaStats::default());
    stats.audio.muted.store(true, Ordering::Release);
    stats.diagnostics.start([std::path::PathBuf::from(
        "artifacts/live-display-validation",
    )]);
    let geo = uu_compat::config::geo_exit_ip();
    let mut session = Session(controller::spawn_with_stats(
        config,
        device,
        geo,
        stats.clone(),
    )?);
    let result = (|| -> Result<()> {
        wait(&mut session, Duration::from_secs(50), "CONTROL open", |s| {
            s.control_open.load(Ordering::Acquire) > 0
        })?;
        wait(
            &mut session,
            Duration::from_secs(10),
            "initial video and settings reply",
            |s| {
                s.decoded_total.load(Ordering::Acquire) > 0
                    && s.settings_response.load(Ordering::Acquire) == 2001
            },
        )?;
        if std::env::var_os("UU_RESOLUTION").is_some() {
            if let uu_compat::control_wire::CaptureResolution::Exact(width, height) =
                uu_compat::config::resolution_from_env()
            {
                wait(&mut session, Duration::from_secs(10), "encoded output bounds", |s| {
                    let (actual_width, actual_height) = s.res();
                    actual_width > 0 && actual_height > 0 && actual_width <= width && actual_height <= height
                })?;
            }
        }
        report(&stats, "initial");
        for (index, id) in screens.into_iter().enumerate() {
            let inventory = stats
                .remote_screens
                .lock()
                .unwrap()
                .clone()
                .with_desktop_capture(*stats.desktop_capture_source.lock().unwrap());
            let screen = inventory
                .screens
                .iter()
                .find(|s| s.id == id)
                .context("source not advertised")?;
            let track = screen.assigned_track();
            let request = 10001 + index as u64;
            let before = stats.decoded_total.load(Ordering::Acquire);
            let command = track
                .map(|t| uu_compat::screen_control::send_video_track(request, t))
                .unwrap_or_else(|| uu_compat::screen_control::activate_screen(request, id));
            session.0.input.send(command)?;
            wait(
                &mut session,
                Duration::from_secs(10),
                "source confirmation",
                |s| {
                    let ack =
                        s.display_responses
                            .lock()
                            .unwrap()
                            .iter()
                            .any(|(seq, case, code)| {
                                *seq == request
                                    && *case == if track.is_some() { 11 } else { 19 }
                                    && *code == 0
                            });
                    let inv = s
                        .remote_screens
                        .lock()
                        .unwrap()
                        .clone()
                        .with_desktop_capture(*s.desktop_capture_source.lock().unwrap());
                    ack && inv
                        .screens
                        .iter()
                        .any(|sc| sc.id == id && sc.assigned_track().is_some())
                },
            )?;
            let inv = stats
                .remote_screens
                .lock()
                .unwrap()
                .clone()
                .with_desktop_capture(*stats.desktop_capture_source.lock().unwrap());
            let selected = inv.screens.iter().find(|s| s.id == id).unwrap();
            stats.select_video_track(selected.assigned_track().unwrap());
            let settings_id = 2001 + index as u64;
            session
                .0
                .input
                .send(uu_compat::control_wire::capture_setting_for_screen(
                    settings_id,
                    uu_compat::control_wire::now_ms(),
                    uu_compat::control_wire::CaptureFrameRate::default(),
                    5,
                    None,
                    uu_compat::control_wire::CaptureResolution::Unchanged,
                    false,
                    id,
                ))?;
            wait(
                &mut session,
                Duration::from_secs(10),
                "source settings and video",
                |s| {
                    s.settings_response.load(Ordering::Acquire) == (settings_id << 1) | 1
                        && s.decoded_total.load(Ordering::Acquire) > before
                },
            )?;
            let after_settings = stats.decoded_total.load(Ordering::Acquire);
            wait(
                &mut session,
                Duration::from_secs(5),
                "continued target video",
                |s| s.decoded_total.load(Ordering::Acquire) >= after_settings + 5,
            )?;
            report(&stats, &format!("source {id}"));
        }
        Ok(())
    })();
    if result.is_err() {
        report(&stats, "failed");
    }
    result
}
