//! Deterministic synthetic capture source.
//!
//! Purpose: make the whole host pipeline (capture -> encode -> packetize ->
//! send) exercisable on a headless machine with no compositor, so CI and the
//! self-loop test never depend on a display server.
//!
//! The pattern deliberately contains *motion* and *colour*, because a static
//! black frame would let real bugs (wrong plane, frozen stream, dropped
//! chunks) pass unnoticed.

use std::time::{Duration, Instant};

use uu_media::frame::{PixelFormat, RawFrame};
use uu_media::Timestamp;

use crate::backend::{Capture, CaptureError};

pub struct SyntheticCapture {
    width: u32,
    height: u32,
    frame_index: u64,
    started: Instant,
    fps: u32,
}

impl SyntheticCapture {
    pub fn new(width: u32, height: u32, fps: u32) -> Self {
        // H.264 4:2:0 needs even dimensions on both axes.
        let width = width & !1;
        let height = height & !1;
        Self {
            width,
            height,
            frame_index: 0,
            started: Instant::now(),
            fps: fps.max(1),
        }
    }

    /// Build frame `n` of the pattern. Exposed separately so tests can
    /// compare a specific frame without driving the clock.
    pub fn pattern(&self, n: u64) -> RawFrame {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut data = vec![0u8; w * h * 3 / 2];
        let t = n as usize;

        // Moving 32x32 white box: guarantees inter-frame difference.
        let box_size = 32usize.min(w).min(h);
        let bx = t.wrapping_mul(3) % w.saturating_sub(box_size).max(1);
        let by = t.wrapping_mul(2) % h.saturating_sub(box_size).max(1);

        for y in 0..h {
            for x in 0..w {
                let gradient = ((x.wrapping_add(t.wrapping_mul(2))) % 256) as u8;
                let in_box = x >= bx && x < bx + box_size && y >= by && y < by + box_size;
                let stripes = if (y / 16) % 2 == 0 { 40 } else { 0 } as u8;
                data[y * w + x] = if in_box {
                    235
                } else {
                    (gradient / 2).max(1) + stripes
                };
            }
        }

        // Chroma: U follows the vertical axis, V the horizontal axis, so a
        // plane swap would be immediately visible as a colour shift.
        let (cw, ch) = (w / 2, h / 2);
        let u_off = w * h;
        let v_off = u_off + cw * ch;
        for y in 0..ch {
            for x in 0..cw {
                data[u_off + y * cw + x] = (64 + (y * 255 / ch.max(1))) as u8;
                data[v_off + y * cw + x] = (64 + (x * 255 / cw.max(1))) as u8;
            }
        }

        let pts =
            Timestamp(((n as u128 * 1_000_000) / self.fps as u128).min(u64::MAX as u128) as u64);
        RawFrame {
            width: self.width,
            height: self.height,
            format: PixelFormat::I420,
            stride: self.width,
            data: data.into(),
            pts,
            gpu: false,
            dma_buf: None,
        }
    }
}

impl Capture for SyntheticCapture {
    fn resolution(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn next_frame(&mut self) -> Result<RawFrame, CaptureError> {
        // Pace to the requested rate, then advance the pattern.
        let elapsed = ((self.frame_index as u128 + 1) * 1_000_000_000 / self.fps as u128)
            .min(u64::MAX as u128) as u64;
        let target = self.started + Duration::from_nanos(elapsed);
        let now = Instant::now();
        if target > now {
            std::thread::sleep(target - now);
        }
        let frame = self.pattern(self.frame_index);
        self.frame_index += 1;
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odd_dimensions_are_rounded_down_for_420() {
        let c = SyntheticCapture::new(641, 361, 30);
        assert_eq!(c.resolution(), (640, 360));
    }

    #[test]
    fn frames_are_well_formed_and_change_over_time() {
        let c = SyntheticCapture::new(320, 240, 30);
        let a = c.pattern(0);
        let b = c.pattern(7);
        assert!(a.is_consistent());
        assert_eq!(a.data.len(), (320 * 240 * 3 / 2) as usize);
        assert_ne!(
            a.data[..320 * 240],
            b.data[..320 * 240],
            "pattern must move"
        );
        assert_ne!(a.pts, b.pts);
    }

    #[test]
    fn chroma_planes_are_not_flat() {
        let c = SyntheticCapture::new(64, 64, 30);
        let f = c.pattern(3);
        let (w, h) = (64usize, 64usize);
        let u = &f.data[w * h..w * h + (w / 2) * (h / 2)];
        assert!(u.iter().max() != u.iter().min(), "U plane should vary");
    }
    #[test]
    fn timestamps_match_requested_frame_rate_without_long_term_rounding_drift() {
        for fps in [24, 30, 60, 120, 144] {
            let capture = SyntheticCapture::new(8, 8, fps);
            assert_eq!(capture.pattern(fps as u64).pts, Timestamp(1_000_000));
            assert_eq!(capture.pattern(1).pts, Timestamp(1_000_000 / fps as u64));
            assert!(capture.pattern(u64::MAX).pts.0 > 0);
        }
    }
}
