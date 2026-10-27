//! CPU time inside the video paint callback, including any driver waits.
//! Keep new images separate from UI repaints of the same image.
use std::time::{Duration, Instant};

#[derive(Clone, Default)]
struct Group {
    samples: u64,
    sum: [f64; 5],
    max_ms: f64,
}
impl Group {
    fn record(&mut self, phases_ms: [f64; 5]) {
        self.samples += 1;
        for (sum, value) in self.sum.iter_mut().zip(phases_ms) { *sum += value; }
        self.max_ms = self.max_ms.max(phases_ms.iter().sum());
    }
    fn report(&self) -> serde_json::Value {
        serde_json::json!({
            "samples":self.samples,
            "mean_upload_bind_uniform_draw_post_ms":(self.samples>0).then(||self.sum.map(|v|v/self.samples as f64)),
            "max_callback_ms":(self.samples>0).then_some(self.max_ms),
        })
    }
}
pub(super) struct Timings {
    started: Instant,
    groups: [Group; 2],
    completed: Option<(Instant, f64, [Group; 2])>,
}
impl Default for Timings {
    fn default() -> Self { Self::new(Instant::now()) }
}
impl Timings {
    fn new(now: Instant) -> Self {
        Self { started: now, groups: Default::default(), completed: None }
    }
    pub(super) fn record(&mut self, new_image: bool, phases_ms: [f64; 5]) {
        self.record_at(Instant::now(), new_image, phases_ms);
    }
    fn record_at(&mut self, now: Instant, new_image: bool, phases_ms: [f64; 5]) {
        self.groups[usize::from(!new_image)].record(phases_ms);
        let elapsed = now.duration_since(self.started);
        if elapsed >= Duration::from_millis(500) {
            self.completed = Some((now, elapsed.as_secs_f64()*1000., std::mem::take(&mut self.groups)));
            self.started = now;
        }
    }
    pub(super) fn report(&self) -> Option<serde_json::Value> { self.report_at(Instant::now()) }
    fn report_at(&self, now: Instant) -> Option<serde_json::Value> {
        let (at, window_ms, groups) = self.completed.as_ref()?;
        if now.duration_since(*at) > Duration::from_secs(2) { return None; }
        Some(serde_json::json!({
            "scope":"video callback CPU wall time, including driver waits; excludes other UI meshes and Present; not GPU execution or scanout",
            "window_ms":window_ms,"new_image":groups[0].report(),"same_image_repaint":groups[1].report(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_images_and_ui_repaints_keep_distinct_counts_and_costs() {
        let start=Instant::now(); let mut t=Timings::new(start);
        t.record_at(start+Duration::from_millis(100),true,[1.,2.,0.,3.,0.]);
        t.record_at(start+Duration::from_millis(200),false,[0.,0.,0.,9.,0.]);
        t.record_at(start+Duration::from_millis(500),true,[3.,4.,0.,5.,0.]);
        let r=t.report_at(start+Duration::from_millis(500)).unwrap();
        assert_eq!(r["new_image"]["samples"],2);
        assert_eq!(r["new_image"]["mean_upload_bind_uniform_draw_post_ms"],serde_json::json!([2.,3.,0.,4.,0.]));
        assert_eq!(r["same_image_repaint"]["samples"],1);
        assert_eq!(r["same_image_repaint"]["max_callback_ms"],9.);
    }
    #[test]
    fn completed_windows_reset_and_idle_data_expires() {
        let start=Instant::now(); let mut t=Timings::new(start);
        assert!(t.report_at(start).is_none());
        t.record_at(start+Duration::from_millis(500),true,[1.;5]);
        t.record_at(start+Duration::from_millis(1000),false,[2.;5]);
        let r=t.report_at(start+Duration::from_millis(1000)).unwrap();
        assert_eq!(r["new_image"]["samples"],0);
        assert!(r["new_image"]["mean_upload_bind_uniform_draw_post_ms"].is_null());
        assert_eq!(r["same_image_repaint"]["samples"],1);
        assert!(t.report_at(start+Duration::from_secs(4)).is_none());
    }
}
