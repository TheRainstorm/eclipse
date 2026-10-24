//! Source identity is independent of output PTS and capture polling frequency.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceTime {
    /// Windows performance-counter ticks, meaningful only on the source host.
    pub qpc_ticks: u64,
    pub frequency_hz: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameInfo {
    pub generation: u64,
    pub frame_id: u64,
    pub duplicate: bool,
    pub accumulated_frames: u32,
    pub present: Option<SourceTime>,
    /// Source-present to observation, computed in the source's own clock.
    pub present_age_us: Option<u64>,
}

pub(crate) struct SourceFrames {
    generation: u64,
    sequence: u64,
    latest: Option<FrameInfo>,
}
impl Default for SourceFrames {
    fn default() -> Self {
        Self {
            generation: 1,
            sequence: 0,
            latest: None,
        }
    }
}
impl SourceFrames {
    pub fn new_frame(&mut self, present: Option<SourceTime>, accumulated_frames: u32) {
        self.sequence += 1;
        self.latest = Some(FrameInfo {
            generation: self.generation,
            frame_id: self.sequence,
            duplicate: false,
            accumulated_frames,
            present,
            present_age_us: None,
        });
    }
    pub fn repeated(&mut self) {
        if let Some(info) = &mut self.latest {
            info.duplicate = true;
            info.accumulated_frames = 0;
        }
    }
    pub fn retire(&mut self) {
        self.generation += 1;
        self.latest = None;
    }
    pub fn snapshot(&self, qpc_now: Option<u64>) -> Option<FrameInfo> {
        let mut info = self.latest?;
        info.present_age_us = info.present.and_then(|source| {
            let elapsed = qpc_now?.checked_sub(source.qpc_ticks)?;
            (source.frequency_hz > 0).then(|| {
                (u128::from(elapsed) * 1_000_000 / u128::from(source.frequency_hz))
                    .min(u128::from(u64::MAX)) as u64
            })
        });
        Some(info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeats_keep_source_identity_and_reset_retires_the_old_desktop() {
        let mut frames = SourceFrames::default();
        assert!(frames.snapshot(None).is_none());
        frames.new_frame(
            Some(SourceTime {
                qpc_ticks: 100,
                frequency_hz: 10_000,
            }),
            3,
        );
        let first = frames.snapshot(Some(120)).unwrap();
        assert_eq!(first.present_age_us, Some(2000));
        frames.repeated();
        let repeated = frames.snapshot(Some(300)).unwrap();
        assert_eq!(
            (repeated.frame_id, repeated.generation),
            (first.frame_id, first.generation)
        );
        assert!(repeated.duplicate);
        assert_eq!(repeated.accumulated_frames, 0);
        assert_eq!(repeated.present_age_us, Some(20_000));
        assert_eq!(frames.snapshot(Some(99)).unwrap().present_age_us, None);
        frames.retire();
        assert!(frames.snapshot(Some(400)).is_none());
        frames.new_frame(None, 1);
        let next = frames.snapshot(None).unwrap();
        assert!(next.frame_id > first.frame_id && next.generation > first.generation);
        assert!(!next.duplicate);
        assert_eq!(next.present_age_us, None);
    }
}
