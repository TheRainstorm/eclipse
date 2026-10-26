//! Bounded CPU wall-clock attribution for each native Glow viewport.
use egui::{Context, Id, ViewportId};
use std::time::{Duration, Instant};

/// A completed sampling window. These are CPU wall-clock durations, including
/// driver stalls, rather than GPU timestamps or physical display latency.
#[derive(Clone, Copy, Debug)]
pub struct ViewportFrameTimings {
    /// Number of completed paints in this window, including UI-only repaints.
    pub samples: u64,
    /// Wall-clock span of the sampling window.
    pub window_ms: f64,
    /// Prepare, initial clear, UI update, paint, viewport commands, present.
    /// The first five sum to the existing CPU frame duration. Present remains
    /// excluded, as do GL context switches already excluded by that timer.
    pub mean_ms: [f64; 6],
    /// Slowest CPU frame in the window, excluding the present phase.
    pub cpu_max_ms: f64,
}

#[derive(Clone)]
struct Window {
    started: Instant,
    samples: u64,
    sum: [f64; 6],
    cpu_max: f64,
    completed: Option<ViewportFrameTimings>,
}
impl Window {
    fn new(now: Instant) -> Self {
        Self { started: now, samples: 0, sum: [0.; 6], cpu_max: 0., completed: None }
    }
    fn record(&mut self, now: Instant, phases_ms: [f64; 6]) {
        self.samples += 1;
        for (sum, value) in self.sum.iter_mut().zip(phases_ms) { *sum += value; }
        self.cpu_max = self.cpu_max.max(phases_ms[..5].iter().sum());
        let elapsed = now.duration_since(self.started);
        if elapsed >= Duration::from_millis(500) {
            self.completed = Some(ViewportFrameTimings {
                samples: self.samples,
                window_ms: elapsed.as_secs_f64() * 1000.,
                mean_ms: self.sum.map(|v| v / self.samples as f64),
                cpu_max_ms: self.cpu_max,
            });
            self.started = now;
            self.samples = 0;
            self.sum = [0.; 6];
            self.cpu_max = 0.;
        }
    }
}
fn key(viewport: ViewportId) -> Id { Id::new(("eframe-viewport-timing", viewport)) }

/// Most recently completed window for the current viewport, never another
/// stream window or the hidden coordinating root.
pub fn viewport_frame_timings(ctx: &Context) -> Option<ViewportFrameTimings> {
    ctx.data(|data| data.get_temp::<Window>(key(ctx.viewport_id())))?.completed
}
pub(crate) fn report(ctx: &Context, viewport: ViewportId, phases_ms: [f64; 6]) {
    let now = Instant::now();
    ctx.data_mut(|data| {
        data.get_temp_mut_or_insert_with(key(viewport), || Window::new(now)).record(now, phases_ms);
    });
}
pub(crate) fn remove(ctx: &Context, viewport: ViewportId) {
    ctx.data_mut(|data| data.remove::<Window>(key(viewport)));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn present_is_separate_and_completed_windows_do_not_mix() {
        let start = Instant::now();
        let mut window = Window::new(start);
        window.record(start + Duration::from_millis(200), [0.2, 0.3, 0.5, 1., 0.1, 14.]);
        assert!(window.completed.is_none());
        window.record(start + Duration::from_millis(500), [0.2, 0.3, 1.5, 3., 0.1, 2.]);
        let result = window.completed.unwrap();
        assert_eq!(result.samples, 2);
        assert_eq!(result.window_ms, 500.);
        assert!((result.mean_ms[..5].iter().sum::<f64>() - 3.6).abs() < 1e-9);
        assert_eq!(result.mean_ms[5], 8.);
        assert!((result.cpu_max_ms - 5.1).abs() < 1e-9);
        window.record(start + Duration::from_millis(1000), [0., 0., 0., 1., 0., 20.]);
        let result = window.completed.unwrap();
        assert_eq!(result.samples, 1);
        assert_eq!(result.mean_ms, [0., 0., 0., 1., 0., 20.]);
        assert_eq!(result.cpu_max_ms, 1.);
    }
    #[test]
    fn viewport_reports_keep_distinct_accumulators() {
        let ctx = Context::default();
        let child = ViewportId::from_hash_of("owned-stream");
        report(&ctx, ViewportId::ROOT, [0.; 6]);
        report(&ctx, child, [1.; 6]);
        ctx.data(|data| {
            assert_eq!(data.get_temp::<Window>(key(ViewportId::ROOT)).unwrap().sum, [0.; 6]);
            assert_eq!(data.get_temp::<Window>(key(child)).unwrap().sum, [1.; 6]);
        });
        remove(&ctx, child);
        ctx.data(|data| {
            assert!(data.get_temp::<Window>(key(child)).is_none());
            assert!(data.get_temp::<Window>(key(ViewportId::ROOT)).is_some());
        });
    }
}
