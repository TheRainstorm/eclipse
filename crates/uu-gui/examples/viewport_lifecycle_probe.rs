//! Native generated-window lifecycle check, never target a real desktop.
#[cfg(target_os = "linux")]
#[path = "../../../tools/native/owned_display.rs"]
mod owned_display;
#[path = "../src/window_context.rs"]
mod window_context;
use eframe::egui;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

fn source() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("generated-source")
}
fn destination() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("generated-destination")
}
#[derive(Default)]
struct Model {
    route: Option<window_context::Context>,
    captured: Option<window_context::Context>,
    a_started: bool,
    b_started: bool,
    transferring: bool,
    bound: bool,
    late_sent: bool,
    first_native: Option<String>,
    last_native: Option<String>,
    phantom_creations: usize,
}
struct Probe {
    model: Arc<Mutex<Model>>,
    legacy: bool,
    started: Instant,
    stop: Arc<AtomicBool>,
    events: Arc<AtomicUsize>,
    producer: Option<std::thread::JoinHandle<()>>,
}
impl eframe::App for Probe {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        let native =
            eframe::viewport_window_for(ctx, source()).map(|window| format!("{:?}", window.id()));
        {
            let mut m = self.model.lock().unwrap();
            if !m.transferring {
                if native.is_some() {
                    m.first_native = native.clone();
                }
            } else if let Some(native) = native.filter(|id| {
                Some(id) != m.first_native.as_ref() && Some(id) != m.last_native.as_ref()
            }) {
                m.phantom_creations += 1;
                m.last_native = Some(native);
            }
            if m.a_started
                && m.b_started
                && self.started.elapsed() > Duration::from_millis(500)
                && !m.transferring
            {
                if !self.legacy {
                    m.route.as_ref().unwrap().suspend();
                }
                m.transferring = true;
            }
        }
        if self.producer.is_none() {
            if let Some(callback) = self.model.lock().unwrap().captured.clone() {
                let stop = self.stop.clone();
                let events = self.events.clone();
                self.producer = Some(std::thread::spawn(move || {
                    while !stop.load(Ordering::Acquire) {
                        callback.request_repaint();
                        callback.request_video_frame();
                        events.fetch_add(1, Ordering::Relaxed);
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }));
            }
        }
        if !self.model.lock().unwrap().transferring {
            let state = self.model.clone();
            ctx.show_viewport_deferred(
                source(),
                egui::ViewportBuilder::default()
                    .with_title("Generated source")
                    .with_inner_size([320., 220.]),
                move |ctx, _| {
                    let mut m = state.lock().unwrap();
                    m.a_started = true;
                    if m.route.is_none() {
                        let route = window_context::Context::new(ctx);
                        m.captured = Some(route.clone());
                        m.route = Some(route);
                    }
                    drop(m);
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.label("Generated source: one session moves out");
                    });
                },
            );
        }
        let state = self.model.clone();
        let legacy = self.legacy;
        ctx.show_viewport_deferred(
            destination(),
            egui::ViewportBuilder::default()
                .with_title("Generated destination")
                .with_inner_size([340., 240.]),
            move |ctx, _| {
                let mut m = state.lock().unwrap();
                m.b_started = true;
                if m.transferring && !m.bound {
                    if legacy {
                        m.route = Some(window_context::Context::new(ctx));
                    } else {
                        m.route.as_ref().unwrap().bind(ctx);
                    }
                    m.bound = true;
                }
                // Deterministically emulate a producer which read source's id just
                // before suspension and entered egui just after destination bind.
                let late = m.bound && !m.late_sent;
                m.late_sent |= late;
                drop(m);
                if late {
                    ctx.request_repaint_of(source());
                }
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.label("Generated destination: same session and callbacks");
                });
                ctx.request_repaint_after(Duration::from_millis(100));
            },
        );
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label("Generated lifecycle test · no host connection, input or desktop capture");
        });
        if self.started.elapsed() > Duration::from_secs(3) {
            self.stop.store(true, Ordering::Release);
            let m = self.model.lock().unwrap();
            if let Some(route) = &m.route {
                route.retire();
            }
            if let Some(route) = &m.captured {
                route.retire();
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(producer) = self.producer.take() {
            producer.join().unwrap();
        }
    }
}
fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    #[cfg(target_os = "linux")]
    {
        let index = args
            .iter()
            .position(|arg| arg == "--x11-display")
            .ok_or_else(|| anyhow::anyhow!("owned Xvfb required"))?;
        owned_display::check(
            args.get(index + 1)
                .ok_or_else(|| anyhow::anyhow!("display required"))?,
        )?;
    }
    #[cfg(not(target_os = "linux"))]
    anyhow::bail!("run this fixture only with an owned Xvfb");
    let legacy = args.iter().any(|arg| arg == "--legacy-snapshot");
    let model = Arc::new(Mutex::new(Model::default()));
    let result = model.clone();
    let events = Arc::new(AtomicUsize::new(0));
    let counts = events.clone();
    eframe::run_native(
        "Generated viewport lifecycle",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([400., 180.]),
            vsync: false,
            ..Default::default()
        },
        Box::new(move |_| {
            Ok(Box::new(Probe {
                model,
                legacy,
                started: Instant::now(),
                stop: Arc::new(AtomicBool::new(false)),
                events,
                producer: None,
            }))
        }),
    )
    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let m = result.lock().unwrap();
    let count = counts.load(Ordering::Relaxed);
    anyhow::ensure!(
        m.a_started && m.b_started && m.bound && m.late_sent && count >= 20,
        "fixture did not exercise two native windows and concurrent late wake"
    );
    if legacy {
        anyhow::ensure!(m.phantom_creations >= 2, "legacy defect was not reproduced");
    } else {
        anyhow::ensure!(
            m.phantom_creations == 0,
            "retired viewport created {} native windows",
            m.phantom_creations
        );
    }
    println!("{{\"window\":\"passed\",\"generated\":true,\"legacy_snapshot\":{legacy},\"cursor_frame_events\":{count},\"phantom_native_creations\":{}}}",m.phantom_creations);
    Ok(())
}
