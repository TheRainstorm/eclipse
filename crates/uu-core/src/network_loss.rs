//! Original RTP delivery before FEC/RTX, independently of access-unit repair.
//! A packet is missing only after a reorder grace period; observing a repaired
//! packet must never erase that event. This measures receiver-observed delivery,
//! not the physical link alone. Sequence discontinuities invalidate a window.
use serde::Serialize;
use std::{collections::VecDeque,time::{Duration,Instant}};
const CAPACITY:usize=32768;
#[derive(Clone,Debug,Serialize)]
pub struct Snapshot {
    pub loss_ppm:Option<u32>,
    pub window_expected:u64,
    pub window_lost:u64,
    pub original_received:u64,
    pub original_lost:u64,
    pub reordered:u64,
    pub duplicates:u64,
    pub late:u64,
    pub resyncs:u64,
    pub grace_ms:u32,
    #[serde(skip)]
    pub sampled_at:Instant,
}
impl Snapshot {pub fn fresh(&self)->bool {self.sampled_at.elapsed()<Duration::from_millis(1500)}}
pub(crate) struct Observer {
    grace:Duration,
    seen:Vec<i64>,
    boundaries:VecDeque<(i64,Instant)>,
    highest:Option<i64>,
    next:i64,
    last_new:Option<Instant>,
    restart:Option<u16>,
    expected:u64,lost:u64,received:u64,total_lost:u64,reordered:u64,duplicates:u64,late:u64,resyncs:u64,
    valid:bool,
}
impl Observer {
    pub fn new(grace:Duration)->Self {
        Self{grace,seen:vec![i64::MIN;CAPACITY],boundaries:VecDeque::new(),highest:None,next:0,last_new:None,restart:None,
            expected:0,lost:0,received:0,total_lost:0,reordered:0,duplicates:0,late:0,resyncs:0,valid:true}
    }
    fn anchor(&mut self,seq:u16,now:Instant) {
        self.seen.fill(i64::MIN);self.boundaries.clear();self.highest=Some(i64::from(seq));self.next=i64::from(seq);self.last_new=Some(now);self.restart=None;
    }
    pub fn observe(&mut self,seq:u16,now:Instant) {
        self.advance(now);
        if self.highest.is_none(){self.anchor(seq,now);}
        let high=self.highest.unwrap();
        let delta=i64::from(seq.wrapping_sub(high as u16) as i16);
        // After a long gap, two consecutive packets establish a new anchor.
        // The number of wraps during the gap is unknowable, not zero loss.
        if delta<=0&&self.last_new.is_some_and(|at|now.saturating_duration_since(at)>Duration::from_secs(1)) {
            if self.restart==Some(seq) {self.anchor(seq,now);self.valid=false;self.resyncs+=1;}
            else{self.restart=Some(seq.wrapping_add(1));return;}
        }
        let high=self.highest.unwrap();let ext=high+i64::from(seq.wrapping_sub(high as u16) as i16);
        if ext<self.next-CAPACITY as i64 {return;}
        let slot=ext.rem_euclid(CAPACITY as i64) as usize;
        if self.seen[slot]==ext {self.duplicates+=1;return;}
        if ext<self.next {self.late+=1;self.received+=1;self.seen[slot]=ext;return;}
        if ext-self.next>=CAPACITY as i64 {
            // Keep memory bounded without inventing a valid loss percentage.
            self.valid=false;self.resyncs+=1;self.anchor(seq,now);
        }
        let high=self.highest.unwrap();let ext=high+i64::from(seq.wrapping_sub(high as u16) as i16);
        self.seen[ext.rem_euclid(CAPACITY as i64) as usize]=ext;self.received+=1;
        if ext<high {self.reordered+=1;}
        if ext>=high {
            self.highest=Some(ext);self.last_new=Some(now);self.restart=None;
            self.boundaries.push_back((ext,now));
        }
    }
    pub fn advance(&mut self,now:Instant) {
        let mut end=None;
        while self.boundaries.front().is_some_and(|(_,at)|now.saturating_duration_since(*at)>=self.grace) {
            end=self.boundaries.pop_front().map(|v|v.0);
        }
        if let Some(end)=end {
            // Monotonic and bounded by the ring; work is amortized per packet.
            while self.next<=end {
                self.expected+=1;
                if self.seen[self.next.rem_euclid(CAPACITY as i64) as usize]!=self.next {self.lost+=1;self.total_lost+=1;}
                self.next+=1;
            }
        }
    }
    pub fn report(&mut self,now:Instant)->Snapshot {
        self.advance(now);
        let expected=std::mem::take(&mut self.expected);let lost=std::mem::take(&mut self.lost);
        let known=std::mem::replace(&mut self.valid,true)&&expected>0;
        Snapshot{loss_ppm:known.then(||(lost.saturating_mul(1_000_000)/expected) as u32),window_expected:expected,window_lost:lost,
            original_received:self.received,original_lost:self.total_lost,reordered:self.reordered,duplicates:self.duplicates,late:self.late,
            resyncs:self.resyncs,grace_ms:self.grace.as_millis().min(u128::from(u32::MAX)) as u32,sampled_at:now}
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovered_packets_do_not_erase_original_network_loss() {
        let t=Instant::now();let mut o=Observer::new(Duration::from_millis(60));
        for seq in [100,101,103,104,105,106,107]{o.observe(seq,t);}
        // A repair for 102 is delivered to the decoder, never to this observer.
        let s=o.report(t+Duration::from_millis(70));
        assert_eq!(s.loss_ppm,Some(125_000));assert_eq!(s.original_lost,1);assert_eq!(s.window_expected,8);
    }
    #[test]
    fn reordering_and_duplicates_are_not_counted_as_loss_or_extra_delivery() {
        let t=Instant::now();let mut o=Observer::new(Duration::from_millis(60));
        for seq in [65534,0,65535,0,1]{o.observe(seq,t);}
        let s=o.report(t+Duration::from_millis(70));
        assert_eq!(s.loss_ppm,Some(0));assert_eq!(s.original_received,4);assert_eq!(s.reordered,1);assert_eq!(s.duplicates,1);
    }
    #[test]
    fn late_original_is_not_a_current_window_repair() {
        let t=Instant::now();let mut o=Observer::new(Duration::from_millis(60));
        o.observe(1,t);o.observe(3,t);let s=o.report(t+Duration::from_millis(70));assert_eq!(s.window_lost,1);
        o.observe(2,t+Duration::from_millis(80));let s=o.report(t+Duration::from_millis(90));
        assert_eq!(s.loss_ppm,None);assert_eq!(s.original_lost,1);assert_eq!(s.late,1);
    }
    #[test]
    fn discontinuity_is_reported_as_unknown_and_memory_is_bounded() {
        let t=Instant::now();let mut o=Observer::new(Duration::from_millis(60));o.observe(10,t);
        o.report(t+Duration::from_millis(70));o.observe(40000,t+Duration::from_secs(2));o.observe(40001,t+Duration::from_secs(2));
        let s=o.report(t+Duration::from_millis(2070));assert_eq!(s.loss_ppm,None);assert_eq!(s.resyncs,1);
        for i in 0..100_000u32 {o.observe(i as u16,t+Duration::from_secs(3)+Duration::from_micros(u64::from(i)));}
        assert!(o.boundaries.len()<=CAPACITY);assert_eq!(o.seen.len(),CAPACITY);
    }
}
