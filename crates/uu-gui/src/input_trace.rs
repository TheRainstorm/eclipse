//! Opt-in input metadata. Message intervals include idle time, not input latency.
//! Windows messages and egui events are independent observations, never paired.
use eframe::egui;
use serde::Serialize;
use std::{collections::BTreeMap, sync::{atomic::{AtomicU64, Ordering::Relaxed}, Mutex, OnceLock}, time::Instant};

const MAX_WINDOWS: usize = 32;
const BINS: usize = 1 + 64 * 8;
const GATES: usize = 9;
const GATE_NAMES: [&str; GATES] = ["not_requested", "not_ready", "not_focused_or_active", "view_only", "local_ui", "local_controls", "relative_not_captured", "no_video", "stream_settings"];

pub(super) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| cfg!(windows) && std::env::var("ECLIPSE_INPUT_TRACE").as_deref() == Ok("1"))
}
fn registry() -> &'static Registry {
    static STATE: OnceLock<Registry> = OnceLock::new();
    STATE.get_or_init(Registry::new)
}

/// Fixed-memory quantiles are inclusive bucket bounds, not exact samples.
/// Eight bins per power of two keep all observations without tail sampling.
struct Distribution { bins: [u64; BINS], count: u64, max: u64 }
impl Default for Distribution {
    fn default() -> Self { Self { bins: [0; BINS], count: 0, max: 0 } }
}
impl Distribution {
    fn add(&mut self, value: u64) {
        let index = if value == 0 { 0 } else {
            let exponent = 63 - value.leading_zeros() as usize;
            let part = ((value - (1u64 << exponent)) >> exponent.saturating_sub(3)) as usize;
            1 + exponent * 8 + part
        };
        self.bins[index] = self.bins[index].saturating_add(1);
        self.count = self.count.saturating_add(1);
        self.max = self.max.max(value);
    }
    fn quantile(&self, percent: u64) -> Option<[u64; 2]> {
        if self.count == 0 { return None; }
        let rank = (u128::from(self.count) * u128::from(percent)).div_ceil(100);
        let mut seen = 0u128;
        for (index, count) in self.bins.iter().enumerate() {
            seen += u128::from(*count);
            if seen >= rank {
                if index == 0 { return Some([0, 0]); }
                let exponent = (index - 1) / 8;
                let part = (index - 1) % 8;
                let width = 1u64 << exponent.saturating_sub(3);
                let low = (1u64 << exponent).saturating_add(part as u64 * width);
                return Some([low, low.saturating_add(width - 1)]);
            }
        }
        None
    }
    fn report(&self, unit: &str) -> serde_json::Value {
        serde_json::json!({"unit":unit,"samples":self.count,"p50_bounds":self.quantile(50),
            "p95_bounds":self.quantile(95),"p99_bounds":self.quantile(99),"max":(self.count>0).then_some(self.max)})
    }
}

#[derive(Clone, Copy, Default, Serialize)]
pub(super) struct PointerCounts { absolute_moves: u64, relative_moves: u64, buttons: u64, wheels: u64, gone: u64 }
impl PointerCounts {
    fn from_events(events: &[egui::Event]) -> Self {
        let mut counts = Self::default();
        for event in events {
            match event {
                egui::Event::PointerMoved(_) => counts.absolute_moves += 1,
                egui::Event::MouseMoved(_) => counts.relative_moves += 1,
                egui::Event::PointerButton { .. } => counts.buttons += 1,
                egui::Event::MouseWheel { .. } => counts.wheels += 1,
                egui::Event::PointerGone => counts.gone += 1,
                _ => {}
            }
        }
        counts
    }
    fn add(&mut self, other: Self) {
        self.absolute_moves += other.absolute_moves; self.relative_moves += other.relative_moves;
        self.buttons += other.buttons; self.wheels += other.wheels; self.gone += other.gone;
    }
    fn candidates(self, relative: bool) -> u64 {
        (if relative { self.relative_moves } else { self.absolute_moves }) + self.buttons + self.wheels
    }
    fn any(self) -> bool { self.candidates(false) + self.relative_moves + self.gone != 0 }
}

#[derive(Clone, Copy)]
#[repr(usize)]
pub(super) enum Gate { NotRequested, NotReady, NotFocusedOrActive, ViewOnly, LocalUi, LocalControls, RelativeNotCaptured, NoVideo, StreamSettings }
#[derive(Clone, Copy, Default)]
pub(super) struct Gates(u16);
impl Gates {
    pub(super) fn set(&mut self, gate: Gate, blocked: bool) { if blocked { self.0 |= 1 << gate as usize; } }
}

#[derive(Default)]
struct UiCounts {
    updates: u64, updates_with_pointer: u64, pointer_stages: u64,
    received: PointerCounts, pointer_stage_events: PointerCounts,
    admitted_candidates: u64, blocked_candidates: u64, gate_candidates: [u64; GATES],
    handed_commands: u64, handed_absolute: u64, handed_relative: u64, handed_buttons: u64, handed_wheels: u64,
}
#[derive(Default)]
struct WindowInterval {
    messages: u64, moves: u64, invalid_message_age: u64,
    move_age: Distribution, message_interval: Distribution, update_interval: Distribution,
    update_duration: Distribution, pointer_update_duration: Distribution, update_to_pointer: Distribution,
    pointer_to_dispatch: Distribution, ui: UiCounts,
}
struct Window {
    since: Instant, last_message: Option<Instant>, last_update: Option<Instant>, data: WindowInterval,
}
impl Window { fn new(now: Instant) -> Self { Self { since:now,last_message:None,last_update:None,data:Default::default() } } }
#[derive(Default)]
struct State {
    windows: BTreeMap<isize, Window>, raw_messages: u64, raw_invalid_age: u64, raw_age: Distribution,
}
struct Registry {
    since: Instant, state: Mutex<State>, contended: AtomicU64, untracked_windows: AtomicU64,
}
impl Registry {
    fn new() -> Self { Self { since:Instant::now(),state:Mutex::new(State::default()),contended:AtomicU64::new(0),untracked_windows:AtomicU64::new(0) } }
    fn with_state<T>(&self, f: impl FnOnce(&mut State) -> T) -> Option<T> {
        match self.state.try_lock() {
            Ok(mut state) => Some(f(&mut state)),
            Err(_) => { self.contended.fetch_add(1,Relaxed); None }
        }
    }
    fn register(&self, window: isize, now: Instant) {
        self.with_state(|state| {
            if state.windows.contains_key(&window) || state.windows.len() < MAX_WINDOWS { state.windows.insert(window,Window::new(now)); }
            else { self.untracked_windows.fetch_add(1,Relaxed); }
        });
    }
    fn message(&self, window: isize, raw: bool, moved: bool, age: Option<u32>, now: Instant) {
        self.with_state(|state| {
            // WM_INPUT targets winit's hidden thread window. Do not assign it
            // to a stream HWND or claim that every raw message is mouse input.
            if raw {
                state.raw_messages += 1;
                if let Some(age) = age { state.raw_age.add(u64::from(age)); } else { state.raw_invalid_age += 1; }
            }
            let Some(window) = state.windows.get_mut(&window) else { return; };
            let data = &mut window.data;
            data.messages += 1;
            if let Some(previous) = window.last_message.replace(now) { data.message_interval.add(micros(now.saturating_duration_since(previous))); }
            if moved {
                data.moves += 1;
                if let Some(age) = age { data.move_age.add(u64::from(age)); } else { data.invalid_message_age += 1; }
            }
        });
    }
    fn update(&self, sample: &Update, ended: Instant) {
        self.with_state(|state| {
            let Some(window) = state.windows.get_mut(&sample.window) else { return; };
            let data = &mut window.data;
            if let Some(previous) = window.last_update.replace(sample.started) { data.update_interval.add(micros(sample.started.saturating_duration_since(previous))); }
            let duration = micros(ended.saturating_duration_since(sample.started));
            data.update_duration.add(duration);
            if sample.received.any() { data.pointer_update_duration.add(duration); data.ui.updates_with_pointer += 1; }
            data.ui.updates += 1; data.ui.received.add(sample.received);
            if let Some(pointer) = sample.pointer {
                data.ui.pointer_stages += 1;
                data.update_to_pointer.add(micros(pointer.saturating_duration_since(sample.started)));
                if let Some(dispatch) = sample.dispatch { data.pointer_to_dispatch.add(micros(dispatch.saturating_duration_since(pointer))); }
            }
            data.ui.pointer_stage_events.add(sample.pointer_events);
            data.ui.admitted_candidates += sample.admitted;
            data.ui.blocked_candidates += sample.blocked;
            for (sum, value) in data.ui.gate_candidates.iter_mut().zip(sample.gates) { *sum += value; }
            data.ui.handed_commands += sample.handed[0]; data.ui.handed_absolute += sample.handed[1];
            data.ui.handed_relative += sample.handed[2]; data.ui.handed_buttons += sample.handed[3]; data.ui.handed_wheels += sample.handed[4];
        });
    }
    fn snapshot(&self, window: isize, now: Instant) -> Option<serde_json::Value> {
        self.with_state(|state| {
            let raw = serde_json::json!({"scope":"process_lifetime_all_raw_input_not_only_mouse","window_ms":now.saturating_duration_since(self.since).as_millis(),
                "messages":state.raw_messages,"invalid_age":state.raw_invalid_age,"message_age":state.raw_age.report("ms_coarse")});
            let window = state.windows.get_mut(&window)?;
            let window_ms = now.saturating_duration_since(window.since).as_millis();
            window.since = now;
            // Intervals are only between observations inside this reporting
            // window; silence itself does not generate a latency sample.
            window.last_message = None; window.last_update = None;
            let data = std::mem::take(&mut window.data);
            let gates: BTreeMap<_,_> = GATE_NAMES.into_iter().zip(data.ui.gate_candidates).collect();
            Some(serde_json::json!({"available":true,"window_ms":window_ms,"scope":"native_window_across_active_tabs_ui_samples_at_update_completion",
                "gui_scope":"selected_session_App_update_frame_not_complete_eframe_redraw",
                "association":"independent_message_and_gui_counts_no_same_event_or_hardware_claim",
                "quantiles":"inclusive_log_histogram_bounds_eight_bins_per_power_of_two_max_is_exact",
                "intervals_include_idle_and_are_not_latency":true,"process_raw":raw,
                "process_lifetime_try_lock_drops":self.contended.load(Relaxed),"process_lifetime_untracked_window_registrations":self.untracked_windows.load(Relaxed),
                "messages":{"dispatched":data.messages,"wm_mousemove":data.moves,"invalid_age":data.invalid_message_age,
                    "mouse_move_age":data.move_age.report("ms_coarse"),"inter_dispatch_interval":data.message_interval.report("us")},
                "gui":{"updates":data.ui.updates,"updates_with_pointer":data.ui.updates_with_pointer,"pointer_stages":data.ui.pointer_stages,
                    "received_events":data.ui.received,"pointer_stage_events":data.ui.pointer_stage_events,
                    "admitted_candidates":data.ui.admitted_candidates,"blocked_candidates":data.ui.blocked_candidates,"gate_candidates_nonexclusive":gates,
                    "commands_handed_to_send_input":{"all":data.ui.handed_commands,"absolute":data.ui.handed_absolute,"relative":data.ui.handed_relative,"buttons":data.ui.handed_buttons,"wheels":data.ui.handed_wheels},
                    "command_scope":"includes_generated_releases_not_proof_of_enqueue_send_or_ack",
                    "inter_update_start_interval":data.update_interval.report("us"),"update_duration":data.update_duration.report("us"),
                    "update_duration_with_pointer":data.pointer_update_duration.report("us"),"update_to_pointer_stage":data.update_to_pointer.report("us"),
                    "pointer_stage_to_first_dispatch":data.pointer_to_dispatch.report("us")}}))
        }).flatten()
    }
}
fn micros(duration: std::time::Duration) -> u64 { duration.as_micros().min(u128::from(u64::MAX)) as u64 }
fn message_age(now_tick: u64, posted: u32) -> Option<u32> {
    // MSG.time is DWORD; uptime can already exceed one 49.7-day wrap. Reject
    // ambiguous/stale timestamps, never clamp an invalid age to zero.
    let age = (now_tick as u32).wrapping_sub(posted);
    (age <= 120_000).then_some(age)
}

pub(super) fn register(window: isize) { if enabled() && window != 0 { registry().register(window,Instant::now()); } }
pub(super) fn unregister(window: isize) { if enabled() && window != 0 { registry().with_state(|state| { state.windows.remove(&window); }); } }
pub(super) fn snapshot(window: isize) -> Option<serde_json::Value> {
    if !enabled() { return None; }
    Some(registry().snapshot(window,Instant::now()).unwrap_or_else(||serde_json::json!({"available":false,
        "reason":"unregistered_window_or_try_lock_unavailable","process_lifetime_try_lock_drops":registry().contended.load(Relaxed)})))
}
#[cfg(windows)]
pub(super) fn message_hook(message: *const std::ffi::c_void) -> bool {
    use windows_sys::Win32::{System::SystemInformation::GetTickCount64,UI::WindowsAndMessaging::{MSG,WM_INPUT,WM_MOUSEMOVE}};
    // Winit owns this MSG until the hook returns. Read metadata only, leaving
    // wParam, lParam, coordinates and payload untouched and unrecorded.
    if let Some(message) = unsafe { (message as *const MSG).as_ref() } {
        let now = Instant::now();
        let age = message_age(unsafe { GetTickCount64() },message.time);
        registry().message(message.hwnd as isize,message.message==WM_INPUT,message.message==WM_MOUSEMOVE,age,now);
    }
    false
}

pub(super) struct Update {
    window: isize, started: Instant, received: PointerCounts, pointer: Option<Instant>, dispatch: Option<Instant>,
    pointer_events: PointerCounts, admitted: u64, blocked: u64, gates: [u64; GATES], handed: [u64; 5],
}
impl Update {
    pub(super) fn begin(window: isize, ctx: &egui::Context) -> Option<Self> {
        if !enabled() || window == 0 { return None; }
        let started = Instant::now();
        Some(Self { window,started,received:ctx.input(|input|PointerCounts::from_events(&input.events)),
            pointer:None,dispatch:None,pointer_events:Default::default(),admitted:0,blocked:0,gates:[0;GATES],handed:[0;5] })
    }
    pub(super) fn pointer(&mut self, events: &[egui::Event], relative: bool, allowed: bool, gates: Gates) {
        self.pointer = Some(Instant::now());
        self.pointer_events = PointerCounts::from_events(events);
        self.decision(self.pointer_events.candidates(relative),allowed,gates);
    }
    fn decision(&mut self, candidates: u64, allowed: bool, gates: Gates) {
        if allowed { self.admitted += candidates; } else {
            self.blocked += candidates;
            for (index,count) in self.gates.iter_mut().enumerate() { if gates.0 & (1 << index) != 0 { *count += candidates; } }
        }
    }
    pub(super) fn skip(&mut self, relative: bool, reason: Gate) {
        let mut gates = Gates::default(); gates.set(reason,true);
        self.decision(self.received.candidates(relative),false,gates);
    }
    pub(super) fn handed(&mut self, command: &uu_core::native_input::InputCommand) {
        use uu_core::native_input::InputCommand as I;
        if self.dispatch.is_none() { self.dispatch = Some(Instant::now()); }
        self.handed[0] += 1;
        match command { I::Absolute { .. } => self.handed[1] += 1, I::Relative { .. } => self.handed[2] += 1,
            I::Button { .. } => self.handed[3] += 1, I::Wheel { .. } => self.handed[4] += 1, _ => {} }
    }
}
impl Drop for Update { fn drop(&mut self) { if enabled() { registry().update(self,Instant::now()); } } }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn histogram_retains_outlier_and_bounds_independently_sorted_quantiles() {
        let mut values: Vec<u64> = (0..7000).map(|n| ((n*37)%1537) as u64).collect();
        values.extend([0,1,65_535,65_536,999_999,u64::MAX]);
        let mut distribution = Distribution::default();
        for value in &values { distribution.add(*value); }
        values.sort_unstable();
        for percent in [50,95,99] {
            let expected = values[(values.len()*percent as usize).div_ceil(100)-1];
            let [low,high] = distribution.quantile(percent).unwrap();
            assert!((low..=high).contains(&expected));
        }
        assert_eq!(distribution.count,values.len() as u64); assert_eq!(distribution.max,u64::MAX);
        assert_eq!(Distribution::default().quantile(99),None);
    }
    #[test]
    fn message_age_handles_uptime_wrap_without_inventing_fresh_timestamps() {
        assert_eq!(message_age((1u64<<32)+7,u32::MAX-2),Some(10));
        assert_eq!(message_age(7,8),None); assert_eq!(message_age(500_000,0),None);
    }
    #[test]
    fn windows_are_isolated_raw_is_unattributed_and_idle_creates_no_samples() {
        let registry=Registry::new(); let now=Instant::now();
        registry.register(11,now); registry.register(22,now);
        registry.message(11,false,true,Some(4),now);
        registry.message(900,true,false,Some(7),now);
        let first=registry.snapshot(11,now+std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(first["messages"]["wm_mousemove"],1); assert_eq!(first["messages"]["inter_dispatch_interval"]["samples"],0);
        assert_eq!(first["process_raw"]["messages"],1);
        let other=registry.snapshot(22,now+std::time::Duration::from_secs(30)).unwrap();
        assert_eq!(other["messages"]["wm_mousemove"],0); assert_eq!(other["gui"]["update_duration"]["samples"],0);
        let empty=registry.snapshot(11,now+std::time::Duration::from_secs(30)).unwrap();
        assert_eq!(empty["messages"]["mouse_move_age"]["max"],serde_json::Value::Null);
        registry.register(11,now); // A reused HWND starts a new lifetime.
        assert_eq!(registry.snapshot(11,now).unwrap()["messages"]["dispatched"],0);
    }
    #[test]
    fn overlapping_gates_count_candidates_once_and_contention_never_waits() {
        let registry=Registry::new(); let now=Instant::now(); registry.register(11,now);
        let mut sample=Update {window:11,started:now,received:PointerCounts{absolute_moves:3,relative_moves:7,..Default::default()},
            pointer:Some(now),dispatch:None,pointer_events:Default::default(),admitted:0,blocked:0,gates:[0;GATES],handed:[0;5]};
        let mut gates=Gates::default(); gates.set(Gate::NotReady,true); gates.set(Gate::NotRequested,true);
        sample.decision(sample.received.candidates(false),false,gates);
        registry.update(&sample,now);
        let snapshot=registry.snapshot(11,now).unwrap();
        assert_eq!(snapshot["gui"]["blocked_candidates"],3); assert_eq!(snapshot["gui"]["gate_candidates_nonexclusive"]["not_ready"],3);
        assert_eq!(snapshot["gui"]["received_events"]["relative_moves"],7);
        let locked=registry.state.lock().unwrap();
        registry.message(11,false,true,Some(0),now);
        assert_eq!(registry.contended.load(Relaxed),1); drop(locked);
    }
}
