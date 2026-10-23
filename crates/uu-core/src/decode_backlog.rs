//! Bound pending reference pictures by memory and measured decode work.
//! A fast decoder can absorb a network burst without dropping its reference
//! chain. Slow decoders retain eight queued pictures plus the current decode.
use std::{sync::{Arc,atomic::{AtomicU64,AtomicUsize,Ordering}},time::Duration};

pub(crate) const MAX_FRAMES:usize=64;
const MIN_FRAMES:usize=9;
const MAX_BYTES:usize=64*1024*1024;
const WORK_US:u64=20_000;
pub(crate) struct Budget {
    frames:AtomicUsize,
    bytes:AtomicUsize,
    decode_us:AtomicU64,
}
impl Default for Budget {
    fn default()->Self {
        Self {frames:0.into(),bytes:0.into(),decode_us:2_500.into()}
    }
}
impl Budget {
    /// Exactly one RTP assembler produces samples; the decoder only releases
    /// permits. A concurrent release can make this admission conservative.
    pub fn reserve(self:&Arc<Self>,bytes:usize)->Option<Permit> {
        let limit=(WORK_US/self.decode_us.load(Ordering::Relaxed).max(1))
            .clamp(MIN_FRAMES as u64,MAX_FRAMES as u64) as usize;
        if self.frames.load(Ordering::Acquire)>=limit
            || self.bytes.load(Ordering::Acquire).checked_add(bytes)?>MAX_BYTES {return None;}
        self.bytes.fetch_add(bytes,Ordering::AcqRel);
        self.frames.fetch_add(1,Ordering::AcqRel);
        Some(Permit {budget:self.clone(),bytes})
    }
    pub fn observe(&self,elapsed:Duration) {
        let measured=(elapsed.as_micros().min(100_000) as u64).max(50);
        let previous=self.decode_us.load(Ordering::Relaxed);
        self.decode_us.store((previous*7+measured)/8,Ordering::Relaxed);
    }
    pub fn reset_estimate(&self) {self.decode_us.store(2_500,Ordering::Relaxed);}
}
pub(crate) struct Permit {budget:Arc<Budget>,bytes:usize}
impl Drop for Permit {
    fn drop(&mut self) {
        let before=self.budget.bytes.fetch_sub(self.bytes,Ordering::AcqRel);
        debug_assert!(before>=self.bytes);
        let before=self.budget.frames.fetch_sub(1,Ordering::AcqRel);
        debug_assert!(before>0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn memory_and_slow_decode_pressure_remain_bounded_and_released_permits_are_reusable() {
        let budget=Arc::new(Budget::default());
        assert!(budget.reserve(MAX_BYTES+1).is_none());
        let mut held=Vec::new();
        for _ in 0..4 {held.push(budget.reserve(MAX_BYTES/4).unwrap());}
        assert!(budget.reserve(1).is_none());
        held.pop();assert!(budget.reserve(MAX_BYTES/4).is_some());drop(held);
        for _ in 0..32 {budget.observe(Duration::from_millis(20));}
        let held:Vec<_>=(0..9).map(|_|budget.reserve(4096).unwrap()).collect();
        assert!(budget.reserve(4096).is_none());drop(held);
        assert_eq!(budget.frames.load(Ordering::Acquire),0);
        assert_eq!(budget.bytes.load(Ordering::Acquire),0);
    }
    #[test]
    fn fast_decode_keeps_a_twelve_picture_network_burst_independently_decodable() {
        use uu_media::{RawFrame,Timestamp,VideoEncoder};
        let budget=Arc::new(Budget::default());
        // Actual receiver traces include ~120 ms bursts, while hardware
        // decoding takes ~0.3 ms. Model that measured throughput here.
        for _ in 0..32 {budget.observe(Duration::from_micros(300));}
        let mut encoder=uu_media::H264Encoder::with_frame_rate(1000,60).unwrap();
        let mut pending=Vec::new();
        for index in 0..12 {
            let mut pixels=vec![128;64*64*3/2];pixels[..64*64].fill(32+index*3);
            let raw=RawFrame::new_i420(64,64,pixels,Timestamp(u64::from(index)*16_666));
            for frame in encoder.encode(&raw).unwrap() {
                let permit=budget.reserve(frame.data.len()).expect("fast decoder must preserve references through a bounded network burst");
                pending.push((frame,permit));
            }
        }
        assert_eq!(pending.len(),12);
        let mut decoder=uu_media::software_decoder().unwrap();let mut count=0u8;
        for (frame,_permit) in pending {
            let decoded=decoder.decode(&frame).unwrap().unwrap();count+=1;
            assert_eq!(decoded.pts,frame.pts);
            assert!((i16::from(decoded.data[0])-i16::from(32+(count-1)*3)).abs()<=3);
        }
        assert_eq!(count,12);
        assert_eq!(budget.frames.load(Ordering::Acquire),0);
        assert_eq!(budget.bytes.load(Ordering::Acquire),0);
    }
}
