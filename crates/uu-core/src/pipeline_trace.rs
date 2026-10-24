//! Opt-in bounded metadata only. One client monotonic clock; never wire latency.
use serde::Serialize;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
    time::Instant,
};
const LIMIT: usize = 8192;
pub(crate) fn diagnostic_limit()->usize {if std::env::var("ECLIPSE_SOCKET_TRACE").as_deref()==Ok("1"){32768}else{LIMIT}}
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Stage {
    CompleteAu = 2,
    DecodeStart = 3,
    DecodeReturn = 4,
    Publish = 5,
    MailboxTake = 6,
    Draw = 7,
    OutputAvailable = 8,
}
type Key = (u64, u32, u32, u32); // media epoch, track, SSRC, RTP timestamp
#[derive(Clone, Serialize)]
struct Row {
    id: u64,
    epoch: u64,
    track: u32,
    ssrc: u32,
    rtp: u32,
    pts_us: Option<u64>,
    generation: Option<u64>,
    at_us: [Option<u64>; 9],
    primary_packets: u32,
    repair_packets: u32,
}
struct State {
    rows: VecDeque<Row>,
    keys: BTreeMap<Key, u64>,
    pts: BTreeMap<(u64, u32, u64), u64>,
    newest: BTreeMap<(u64, u32, u32), u32>,
    total: u64,
    evicted: u64,
    missing: u64,
}
pub struct Trace {
    enabled: bool,
    limit: usize,
    started: Instant,
    state: Mutex<State>,
}
impl Default for Trace {
    fn default() -> Self {
        Self::new(std::env::var("ECLIPSE_CONTINUOUS_TRACE").as_deref() == Ok("1"))
    }
}
impl Trace {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            limit: diagnostic_limit(),
            started: Instant::now(),
            state: Mutex::new(State {
                rows: VecDeque::new(),
                keys: BTreeMap::new(),
                pts: BTreeMap::new(),
                newest: BTreeMap::new(),
                total: 0,
                evicted: 0,
                missing: 0,
            }),
        }
    }
    pub(crate) fn clock_offset(&self,at:Instant)->u64{self.offset(at)}
    fn offset(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.started)
            .as_micros()
            .min(u64::MAX as u128) as u64
    }
    fn index(s: &State, id: u64) -> Option<usize> {
        let i = id.checked_sub(s.rows.front()?.id)? as usize;
        (i < s.rows.len()).then_some(i)
    }
    pub fn packet(&self, epoch: u64, track: u32, ssrc: u32, rtp: u32, primary: bool, at: Instant) {
        if !self.enabled {
            return;
        }
        let at = self.offset(at);
        let mut s = self.state.lock().unwrap();
        let key = (epoch, track, ssrc, rtp);
        let id = if let Some(&id) = s.keys.get(&key) {
            id
        } else {
            let stream = (epoch, track, ssrc);
            if s.newest
                .get(&stream)
                .is_some_and(|old| rtp.wrapping_sub(*old) as i32 <= 0)
                || (!s.newest.contains_key(&stream) && s.newest.len() >= 32)
            {
                s.missing += 1;
                return;
            }
            s.newest.insert(stream, rtp);
            if s.rows.len() == self.limit {
                let old = s.rows.pop_front().unwrap();
                s.keys.remove(&(old.epoch, old.track, old.ssrc, old.rtp));
                if let Some(pts) = old.pts_us {
                    let key = (old.epoch, old.track, pts);
                    if s.pts.get(&key) == Some(&old.id) {
                        s.pts.remove(&key);
                    }
                }
                s.evicted += 1;
            }
            s.total += 1;
            let id = s.total;
            s.rows.push_back(Row {
                id,
                epoch,
                track,
                ssrc,
                rtp,
                pts_us: None,
                generation: None,
                at_us: [Some(at), Some(at), None, None, None, None, None, None, None],
                primary_packets: 0,
                repair_packets: 0,
            });
            s.keys.insert(key, id);
            id
        };
        if let Some(i) = Self::index(&s, id) {
            let r = &mut s.rows[i];
            r.at_us[1] = Some(at);
            if primary {
                r.primary_packets += 1
            } else {
                r.repair_packets += 1
            }
        }
    }
    pub fn frame(
        &self,
        epoch: u64,
        track: u32,
        ssrc: u32,
        rtp: u32,
        stage: Stage,
        decode: Option<(u64, u64)>,
    ) {
        if !self.enabled {
            return;
        }
        let at = self.offset(Instant::now());
        let mut s = self.state.lock().unwrap();
        let Some(id) = s.keys.get(&(epoch, track, ssrc, rtp)).copied() else {
            s.missing += 1;
            return;
        };
        if let Some(i) = Self::index(&s, id) {
            s.rows[i].at_us[stage as usize].get_or_insert(at);
            if let Some((pts, generation)) = decode {
                s.rows[i].pts_us = Some(pts);
                s.rows[i].generation = Some(generation);
                s.pts.insert((epoch, track, pts), id);
            }
        }
    }
    pub fn image(&self, epoch: u64, track: u32, pts: u64, stage: Stage) {
        self.image_at(epoch,track,pts,stage,Instant::now());
    }
    pub fn image_at(&self, epoch:u64, track:u32, pts:u64, stage:Stage, observed:Instant) {
        if !self.enabled {return;}
        let at=self.offset(observed);
        let mut s = self.state.lock().unwrap();
        let Some(id) = s.pts.get(&(epoch, track, pts)).copied() else {
            s.missing += 1;
            return;
        };
        if let Some(i) = Self::index(&s, id) {
            s.rows[i].at_us[stage as usize].get_or_insert(at);
        }
    }
    pub fn report(&self) -> Option<serde_json::Value> {
        if !self.enabled {
            return None;
        }
        let s = self.state.lock().unwrap();
        let (total,evicted,missing,rows)=(s.total,s.evicted,s.missing,s.rows.clone());
        drop(s);
        Some(
            serde_json::json!({"clock":"client_monotonic","stages":["first_rtp_read","last_rtp_read","complete_au","decode_start","decode_return","publish_call","mailbox_take","draw_callback","output_available_by_pts"],"limit":self.limit,"total_frames":total,"evicted_frames":evicted,"unmatched_events":missing,"frames":rows}),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wrap_duplicates_reorder_and_epoch_are_distinct() {
        let t = Trace::new(true);
        t.packet(1, 0, 9, u32::MAX, true, Instant::now());
        t.packet(1, 0, 9, 5, true, Instant::now());
        t.packet(1, 0, 9, u32::MAX, false, Instant::now());
        t.packet(1, 0, 9, 3, true, Instant::now());
        t.packet(2, 0, 9, 5, true, Instant::now());
        let r = t.report().unwrap();
        assert_eq!(r["total_frames"], 3);
        assert_eq!(r["unmatched_events"], 1);
        assert_eq!(r["frames"][0]["repair_packets"], 1);
    }
    #[test]
    fn bounded_ring_and_missing_identity_are_reported() {
        let t = Trace::new(true);
        for n in 0..LIMIT + 2 {
            t.packet(1, 0, 9, n as u32, true, Instant::now());
        }
        t.frame(1, 0, 9, 0, Stage::CompleteAu, None);
        t.image(2, 0, 0, Stage::Draw);
        let r = t.report().unwrap();
        assert_eq!(r["frames"].as_array().unwrap().len(), LIMIT);
        assert_eq!(r["evicted_frames"], 2);
        assert_eq!(r["unmatched_events"], 2);
    }
    #[test]
    fn decode_pts_mapping_does_not_cross_media_epochs() {
        let t = Trace::new(true);
        t.packet(1, 0, 9, 1, true, Instant::now());
        t.frame(1, 0, 9, 1, Stage::DecodeStart, Some((0, 7)));
        t.image(2, 0, 0, Stage::Draw);
        t.image(1, 0, 0, Stage::Publish);
        let r = t.report().unwrap();
        assert!(r["frames"][0]["at_us"][5].is_number());
        assert!(r["frames"][0]["at_us"][7].is_null());
        assert_eq!(r["unmatched_events"], 1);
    }
    #[test]
    fn retiring_an_old_ssrc_does_not_delete_the_rebound_pts_mapping() {
        let t = Trace::new(true);
        for ssrc in [9, 10] {
            t.packet(1, 0, ssrc, 1, true, Instant::now());
            t.frame(1, 0, ssrc, 1, Stage::DecodeStart, Some((0, ssrc as u64)));
        }
        for n in 2..LIMIT + 1 {
            t.packet(1, 0, 10, n as u32, true, Instant::now());
        }
        t.image(1, 0, 0, Stage::Draw);
        let r = t.report().unwrap();
        assert_eq!(r["evicted_frames"], 1);
        assert_eq!(r["frames"][0]["ssrc"], 10);
        assert!(r["frames"][0]["at_us"][7].is_number());
        assert_eq!(r["unmatched_events"], 0);
    }
    #[test]
    fn delayed_output_is_attached_to_output_pts_not_current_input() {
        let t=Trace::new(true);
        for (rtp,pts) in [(1,0),(2,16667)] {
            t.packet(1,0,9,rtp,true,Instant::now());
            t.frame(1,0,9,rtp,Stage::DecodeStart,Some((pts,1)));
            t.frame(1,0,9,rtp,Stage::DecodeReturn,None);
        }
        let observed=Instant::now();
        t.image_at(1,0,0,Stage::OutputAvailable,observed);
        let r=t.report().unwrap();
        assert!(r["frames"][0]["at_us"][8].is_number());
        assert!(r["frames"][1]["at_us"][8].is_null());
    }
    #[test]
    fn disabled_has_no_observations() {
        let t = Trace::new(false);
        t.packet(1, 0, 9, 1, true, Instant::now());
        assert!(t.report().is_none());
    }
}
