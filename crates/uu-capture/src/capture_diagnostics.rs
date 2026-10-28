//! Opt-in, owner-thread metadata only. No clocks, locks, textures or IO per row.
use crate::frame_info::FrameInfo;
use serde::Serialize;
use std::collections::VecDeque;

pub(crate) const CAPACITY: usize = 32_768;
pub(crate) fn requested(continuous: Option<&str>, capture: Option<&str>) -> bool {
    continuous == Some("1") && capture == Some("1")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Outcome {
    AcquiredNotReturned,
    Fresh,
    WaitTimeout,
    PointerOnly,
    PoolBusy,
    Retired,
    Error,
}
#[derive(Clone, Copy, Serialize)]
pub(crate) struct Acquisition {
    pub thread_id: u32,
    pub qpc_before: Option<u64>,
    pub qpc_after: Option<u64>,
    pub timeout_ms: u32,
    pub hresult: i32,
    pub frame_info_valid: bool,
    pub last_present_qpc: i64,
    pub last_mouse_update_qpc: i64,
    pub accumulated_frames: u32,
    pub gpu: bool,
    pub overlap: bool,
    pub separate_encode_device: bool,
}
#[derive(Clone, Copy, Serialize)]
struct Returned {
    source: Option<FrameInfo>,
    returned_capture_pts_us: u64,
    cache_replay: bool,
}
#[derive(Serialize)]
struct Row {
    event_sequence: u64,
    attempt_sequence: Option<u64>,
    outcome: Outcome,
    acquisition: Option<Acquisition>,
    returned: Option<Returned>,
    source_before_retire: Option<FrameInfo>,
    retired_at_qpc: Option<u64>,
    retirement_reason: Option<&'static str>,
    source_generation_retired: bool,
}
pub(crate) struct Recorder {
    rows: VecDeque<Row>,
    capacity: usize,
    events: u64,
    attempts: u64,
    returns: u64,
    evicted: u64,
    start_qpc: Option<u64>,
    frequency_hz: u64,
    owner_tid_at_open: u32,
}
impl Recorder {
    pub fn new(
        start_qpc: Option<u64>,
        frequency_hz: u64,
        owner_tid_at_open: u32,
        capacity: usize,
    ) -> Self {
        assert!(capacity > 0 && capacity <= CAPACITY);
        Self {
            rows: VecDeque::with_capacity(capacity),
            capacity,
            events: 0,
            attempts: 0,
            returns: 0,
            evicted: 0,
            start_qpc,
            frequency_hz,
            owner_tid_at_open,
        }
    }
    fn push(&mut self, mut row: Row) {
        self.events += 1;
        row.event_sequence = self.events;
        if self.rows.len() == self.capacity {
            self.rows.pop_front();
            self.evicted += 1;
        }
        self.rows.push_back(row);
    }
    pub fn acquired(&mut self, acquisition: Acquisition, outcome: Outcome) {
        self.attempts += 1;
        self.push(Row {
            event_sequence: 0,
            attempt_sequence: Some(self.attempts),
            outcome,
            acquisition: Some(acquisition),
            returned: None,
            source_before_retire: None,
            retired_at_qpc: None,
            retirement_reason: None,
            source_generation_retired: false,
        });
    }
    pub fn returned(&mut self, source: Option<FrameInfo>, pts_us: u64, replay: bool) {
        // The owner invokes this immediately after the matching acquire result;
        // never attach a returned image to a retirement record or an older call.
        if let Some(row) = self
            .rows
            .back_mut()
            .filter(|row| row.attempt_sequence == Some(self.attempts))
        {
            if row.returned.is_none()
                && row.outcome != Outcome::Retired
                && row.outcome != Outcome::Error
            {
                row.returned = Some(Returned {
                    source,
                    returned_capture_pts_us: pts_us,
                    cache_replay: replay,
                });
                if !replay {
                    row.outcome = Outcome::Fresh;
                }
                self.returns += 1;
            }
        }
    }
    pub fn pool_busy(&mut self) {
        if let Some(row) = self
            .rows
            .back_mut()
            .filter(|row| row.attempt_sequence == Some(self.attempts))
        {
            row.outcome = Outcome::PoolBusy;
        }
    }
    pub fn retired(
        &mut self,
        source: Option<FrameInfo>,
        qpc: Option<u64>,
        reason: &'static str,
        source_generation_retired: bool,
    ) {
        self.push(Row {
            event_sequence: 0,
            attempt_sequence: None,
            outcome: Outcome::Retired,
            acquisition: None,
            returned: None,
            source_before_retire: source,
            retired_at_qpc: qpc,
            retirement_reason: Some(reason),
            source_generation_retired,
        });
    }
    pub fn write(
        &self,
        out: &mut impl std::io::Write,
        pid: u32,
        owner_tid_at_drop: u32,
        unwinding: bool,
    ) -> std::io::Result<()> {
        #[derive(Serialize)]
        struct Report<'a> {
            event: &'static str,
            version: u32,
            terminal: &'static str,
            process_id: u32,
            owner_tid_at_open: u32,
            owner_tid_at_drop: u32,
            start_qpc: Option<u64>,
            frequency_hz: u64,
            unwinding: bool,
            clean_session_proven: bool,
            gpu_completion_or_display_time_claimed: bool,
            events: u64,
            attempts: u64,
            returns: u64,
            evicted: u64,
            capacity: usize,
            rows: &'a VecDeque<Row>,
        }
        serde_json::to_writer(
            &mut *out,
            &Report {
                event: "dxgi_capture_diagnostics",
                version: 1,
                terminal: "capture_owner_drop",
                process_id: pid,
                owner_tid_at_open: self.owner_tid_at_open,
                owner_tid_at_drop,
                start_qpc: self.start_qpc,
                frequency_hz: self.frequency_hz,
                unwinding,
                clean_session_proven: false,
                gpu_completion_or_display_time_claimed: false,
                events: self.events,
                attempts: self.attempts,
                returns: self.returns,
                evicted: self.evicted,
                capacity: self.capacity,
                rows: &self.rows,
            },
        )
        .map_err(std::io::Error::other)?;
        out.write_all(b"\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn acquisition() -> Acquisition {
        Acquisition {
            thread_id: 1,
            qpc_before: Some(10),
            qpc_after: Some(11),
            timeout_ms: 0,
            hresult: 0,
            frame_info_valid: true,
            last_present_qpc: 5,
            last_mouse_update_qpc: 0,
            accumulated_frames: 1,
            gpu: true,
            overlap: false,
            separate_encode_device: false,
        }
    }
    fn source(id: u64) -> FrameInfo {
        FrameInfo {
            generation: 2,
            frame_id: id,
            duplicate: false,
            accumulated_frames: 1,
            present: None,
            present_age_us: None,
        }
    }
    #[test]
    fn both_explicit_switches_are_required() {
        for a in [None, Some("0"), Some("1"), Some("true")] {
            for b in [None, Some("0"), Some("1"), Some("true")] {
                assert_eq!(requested(a, b), a == Some("1") && b == Some("1"));
            }
        }
    }
    #[test]
    fn timeout_pointer_and_pool_replay_preserve_the_real_reason_and_old_source() {
        for outcome in [
            Outcome::WaitTimeout,
            Outcome::PointerOnly,
            Outcome::PoolBusy,
        ] {
            let mut r = Recorder::new(Some(1), 10, 1, 4);
            r.acquired(acquisition(), outcome);
            r.returned(Some(source(7)), 999, true);
            assert_eq!(r.rows[0].outcome, outcome);
            assert_eq!(r.rows[0].returned.unwrap().source.unwrap().frame_id, 7);
            assert!(r.rows[0].returned.unwrap().cache_replay);
        }
    }
    #[test]
    fn a_new_snapshot_gets_its_return_identity_and_no_return_does_not_claim_fresh() {
        let mut r = Recorder::new(None, 0, 1, 4);
        r.acquired(acquisition(), Outcome::AcquiredNotReturned);
        r.returned(Some(source(8)), 1000, false);
        r.acquired(acquisition(), Outcome::AcquiredNotReturned);
        assert_eq!(r.rows[0].outcome, Outcome::Fresh);
        assert_eq!(r.rows[1].outcome, Outcome::AcquiredNotReturned);
        assert!(r.rows[1].returned.is_none());
        r.pool_busy();
        assert_eq!(r.rows[1].outcome, Outcome::PoolBusy);
    }
    #[test]
    fn retire_does_not_inherit_old_return_and_overflow_is_explicit() {
        let mut r = Recorder::new(None, 0, 1, 2);
        r.acquired(acquisition(), Outcome::WaitTimeout);
        r.retired(Some(source(7)), Some(12), "desktop_invalidated", true);
        r.returned(Some(source(8)), 2000, false);
        assert!(r.rows[1].returned.is_none());
        r.acquired(acquisition(), Outcome::Error);
        assert_eq!(
            (r.events, r.attempts, r.evicted, r.rows.len()),
            (3, 2, 1, 2)
        );
        let mut out = Vec::new();
        r.write(&mut out, 5, 2, true).unwrap();
        assert_eq!(out.last(), Some(&b'\n'));
        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["unwinding"], true);
        assert_eq!(value["clean_session_proven"], false);
        assert_eq!(value["rows"][0]["event_sequence"], 2);
    }
}
