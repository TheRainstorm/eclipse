//! Explicit bounded GPU repaint exercise in owned Windows windows; generated UI only.

#[cfg(windows)]
mod probe {
    use eframe::egui;
    use std::{
        sync::{
            atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering},
            Arc, Mutex,
        },
        time::{Duration, Instant},
    };
    use windows_sys::Win32::{
        System::Threading::GetCurrentProcessId,
        UI::WindowsAndMessaging::{
            GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_MOUSEMOVE,
        },
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    fn viewport(index: usize) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(("owned-repaint-gpu", index))
    }
    struct Shared {
        hwnd: [AtomicIsize; 2],
        visible: [AtomicBool; 2],
        paints: [AtomicU64; 2],
        moves: AtomicU64,
        phase: AtomicU32,
        stop: AtomicBool,
        result: Mutex<Option<serde_json::Value>>,
    }
    impl Shared {
        fn counts(&self) -> [u64; 2] {
            std::array::from_fn(|i| self.paints[i].load(Ordering::Acquire))
        }
    }
    struct App {
        shared: Arc<Shared>,
        worker: Option<std::thread::JoinHandle<()>>,
    }
    impl eframe::App for App {
        fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
            for index in 0..2 {
                let shared = self.shared.clone();
                ctx.show_viewport_deferred(
                    viewport(index),
                    egui::ViewportBuilder::default()
                        .with_title(format!("Owned generated viewport {index}"))
                        .with_active(false)
                        .with_inner_size([360., 240.])
                        .with_position([20. + index as f32 * 380., 20.]),
                    move |ctx, _| {
                        shared.paints[index].fetch_add(1, Ordering::Relaxed);
                        if let Some(window) = eframe::viewport_window(ctx) {
                            if let Ok(handle) = window.window_handle() {
                                if let RawWindowHandle::Win32(handle) = handle.as_raw() {
                                    shared.hwnd[index].store(handle.hwnd.get(), Ordering::Release);
                                    shared.visible[index].store(
                                        unsafe { IsWindowVisible(handle.hwnd.get() as _) } != 0,
                                        Ordering::Release,
                                    );
                                }
                            }
                        }
                        let moves = ctx.input(|input| {
                            input
                                .events
                                .iter()
                                .filter(|e| matches!(e, egui::Event::PointerMoved(_)))
                                .count()
                        });
                        shared.moves.fetch_add(moves as u64, Ordering::Relaxed);
                        if index == 0 && shared.phase.load(Ordering::Acquire) == 2 {
                            // Finite UI work while the worker replenishes posted messages.
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        egui::CentralPanel::default().show(ctx, |ui| {
                            ui.label(
                                "Generated static viewport; no desktop content or remote input",
                            );
                            ui.painter().rect_filled(
                                ui.available_rect_before_wrap(),
                                0.,
                                egui::Color32::from_rgb(40, 90, 130),
                            );
                        });
                    },
                );
            }
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label("Owned GPU repaint fixture");
            });
        }
    }
    impl Drop for App {
        fn drop(&mut self) {
            self.shared.stop.store(true, Ordering::Release);
            if let Some(worker) = self.worker.take() {
                worker.join().unwrap();
            }
        }
    }
    pub fn run(output: &std::path::Path) -> anyhow::Result<()> {
        
        let shared = Arc::new(Shared {
            hwnd: std::array::from_fn(|_| AtomicIsize::new(0)),
            visible: std::array::from_fn(|_| AtomicBool::new(false)),
            paints: std::array::from_fn(|_| AtomicU64::new(0)),
            moves: AtomicU64::new(0),
            phase: AtomicU32::new(0),
            stop: AtomicBool::new(false),
            result: Mutex::new(None),
        });
        let state = shared.clone();
        eframe::run_native("Owned repaint fixture", eframe::NativeOptions { vsync: false, persist_window: false, viewport: egui::ViewportBuilder::default().with_active(false).with_visible(false), ..Default::default() }, Box::new(move |cc| {
            cc.egui_ctx.set_embed_viewports(false);
            let worker_state = state.clone(); let context = cc.egui_ctx.clone();
            let worker = std::thread::spawn(move || {
                let started = Instant::now(); let mut tick = 0u64; let mut phases = Vec::new(); let mut baseline = [0; 2];
                while !worker_state.stop.load(Ordering::Acquire) && started.elapsed() < Duration::from_secs(9) {
                    let elapsed = started.elapsed();
                    let phase = if elapsed < Duration::from_secs(1) { 0 } else if elapsed < Duration::from_millis(2500) { 1 }
                        else if elapsed < Duration::from_millis(4500) { 2 } else if elapsed < Duration::from_millis(5000) { 3 } else { 4 };
                    if worker_state.phase.swap(phase, Ordering::AcqRel) != phase {
                        let counts = worker_state.counts();
                        phases.push(serde_json::json!({"phase":phase,"counts":counts,"delta":std::array::from_fn::<_,2,_>(|i|counts[i]-baseline[i]),"elapsed_ms":elapsed.as_millis()}));
                        baseline = counts;
                    }
                    if phase == 2 {
                        for index in 0..2 { context.request_repaint_after_for(Duration::from_nanos(1), viewport(index)); }
                        let hwnd = worker_state.hwnd[0].load(Ordering::Acquire);
                        if hwnd != 0 {
                            unsafe {
                                let mut owner = 0; GetWindowThreadProcessId(hwnd as _, &mut owner);
                                assert_eq!(owner, GetCurrentProcessId(), "only the fixture HWND may receive messages");
                                let point = (60isize << 16) | (40 + (tick % 160) as isize);
                                for _ in 0..8 { assert_ne!(PostMessageW(hwnd as _, WM_MOUSEMOVE, 0, point), 0); }
                            }
                        }
                    }
                    if elapsed >= Duration::from_millis(6500) {
                        let counts = worker_state.counts();
                        *worker_state.result.lock().unwrap() = Some(serde_json::json!({
                            "phases":phases,"idle_after_counts":std::array::from_fn::<_,2,_>(|i|counts[i]-baseline[i]),
                            "visible":std::array::from_fn::<_,2,_>(|i|worker_state.visible[i].load(Ordering::Acquire)),
                            "pointer_events":worker_state.moves.load(Ordering::Acquire),"total_counts":counts,
                            "desktop_switched":false,"global_input":false,"desktop_capture":false,"owned_visible_windows":true,
                            "sdk":uu_plugin_sdk::SDK_ID,
                        }));
                        context.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
                        break;
                    }
                    tick += 1; std::thread::sleep(Duration::from_millis(2));
                }
            });
            Ok(Box::new(App { shared: state, worker: Some(worker) }))
        })).map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let result = shared
            .result
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| anyhow::anyhow!("fixture did not complete"))?;
        std::fs::write(output, serde_json::to_vec_pretty(&result)?)?;
        anyhow::ensure!(
            result["visible"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v == true),
            "fixture windows were not visible"
        );
        anyhow::ensure!(
            result["idle_after_counts"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v.as_u64().unwrap() <= 20),
            "idle viewports kept repainting"
        );
        let busy = result["phases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["phase"] == 3)
            .unwrap();
        anyhow::ensure!(
            busy["delta"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v.as_u64().unwrap() >= 15),
            "a viewport starved under input load"
        );
        anyhow::ensure!(
            result["pointer_events"].as_u64().unwrap() > 50,
            "owned pointer messages were not delivered"
        );
        Ok(())
    }
}

#[cfg(windows)]
fn main() {
    if std::env::args().nth(2).as_deref() != Some("--owned-visible-windows") {
        eprintln!("Usage: repaint_probe <new-report.json> --owned-visible-windows");
        std::process::exit(2);
    }
    let Some(output) = std::env::args_os().nth(1) else {
        std::process::exit(2)
    };
    let path = std::path::Path::new(&output);
    if path.exists() {
        std::process::exit(2);
    }
    if let Err(error) = probe::run(path) {
        if !path.exists() {
            let _ = std::fs::write(
                path,
                serde_json::json!({"error":error.to_string()}).to_string(),
            );
        }
        std::process::exit(1);
    }
}
#[cfg(not(windows))]
fn main() {
    eprintln!("Windows-only generated GPU fixture; requires --owned-visible-windows.");
}
