//! Rolling cadence of new-image submissions while the caller marks them active.
//! This measures delivery to the renderer, not scanout or motion in the content.
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const WINDOW: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub long_gaps: usize,
    pub max_gap_ms: f64,
    pub threshold_ms: f64,
    pub ongoing_gap_ms: Option<f64>,
    pub observed_seconds: f64,
}

struct Gap {
    ended: Instant,
    duration: Duration,
}
struct Bucket { ended: Instant, long_gaps: usize, maximum: Duration }

#[derive(Default)]
pub struct Continuity {
    active: bool,
    fps: u32,
    started: Option<Instant>,
    previous: Option<Instant>,
    intervals: usize,
    // Latest foreground observation of an unfinished stall. Pausing preserves
    // that observation without adding time spent inactive to the interval.
    ongoing: Option<Gap>,
    // Only long intervals are retained: at least 20 ms each, at most 3001/min.
    gaps: VecDeque<Gap>,
    // Monotonic maximum queue: dominated intervals leave immediately; every
    // surviving interval expires at its exact end time, without second buckets.
    maxima: VecDeque<(Instant, Duration)>,
    // Long-window statistics at one-second resolution; hot-path work is O(1).
    buckets: VecDeque<Bucket>,
}

impl Continuity {
    fn threshold(&self) -> Duration {
        Duration::from_secs_f64(3. / f64::from(self.fps.max(1))).max(Duration::from_millis(20))
    }
    pub fn set_active(&mut self, active: bool, fps: u32) {
        let active = active && fps > 0;
        if self.active != active || self.fps != fps {
            if let Some(observed) = self.ongoing.take() {
                self.remember(observed.ended, observed.duration, true);
            }
            self.previous = None;
            self.started = None;
            self.intervals = 0;
        }
        self.active = active;
        self.fps = fps;
    }
    fn remember(&mut self, ended: Instant, duration: Duration, long: bool) {
        while self.buckets.front().is_some_and(|b|ended.saturating_duration_since(b.ended)>=Duration::from_secs(3601)) { self.buckets.pop_front(); }
        if let Some(bucket)=self.buckets.back_mut().filter(|b|ended.saturating_duration_since(b.ended)<Duration::from_secs(1)) {
            bucket.long_gaps+=usize::from(long);bucket.maximum=bucket.maximum.max(duration);
        } else { self.buckets.push_back(Bucket{ended,long_gaps:usize::from(long),maximum:duration}); }
        if long {
            self.gaps.push_back(Gap { ended, duration });
        }
        while self
            .maxima
            .back()
            .is_some_and(|(_, maximum)| *maximum <= duration)
        {
            self.maxima.pop_back();
        }
        self.maxima.push_back((ended, duration));
    }
    fn expire(&mut self, now: Instant) {
        while self
            .gaps
            .front()
            .is_some_and(|g| now.saturating_duration_since(g.ended) >= WINDOW)
        {
            self.gaps.pop_front();
        }
        while self
            .maxima
            .front()
            .is_some_and(|g| now.saturating_duration_since(g.0) >= WINDOW)
        {
            self.maxima.pop_front();
        }
    }
    pub fn submit(&mut self, now: Instant) {
        self.expire(now);
        if !self.active {
            return;
        }
        // A completed interval replaces its projected observation; count once.
        self.ongoing = None;
        if let Some(previous) = self.previous {
            let gap = now.saturating_duration_since(previous);
            self.intervals = self.intervals.saturating_add(1);
            self.remember(now, gap, gap > self.threshold());
        } else {
            self.started = Some(now);
        }
        self.previous = Some(now);
    }
    pub fn snapshot(&mut self, now: Instant) -> Option<Snapshot> {
        self.expire(now);
        if !self.active || self.intervals == 0 {
            return None;
        }
        let current = now.saturating_duration_since(self.previous?);
        let ongoing = (current > self.threshold()).then_some(current);
        self.ongoing = ongoing.map(|duration| Gap {
            ended: now,
            duration,
        });
        let maximum = self
            .maxima
            .iter()
            .map(|(_, gap)| *gap)
            .chain(self.gaps.iter().map(|g| g.duration))
            .chain(ongoing)
            .max()
            .unwrap_or_default();
        Some(Snapshot {
            long_gaps: self.gaps.len() + usize::from(ongoing.is_some()),
            max_gap_ms: maximum.as_secs_f64() * 1000.,
            threshold_ms: self.threshold().as_secs_f64() * 1000.,
            ongoing_gap_ms: ongoing.map(|gap| gap.as_secs_f64() * 1000.),
            observed_seconds: now
                .saturating_duration_since(self.started?)
                .min(WINDOW)
                .as_secs_f64(),
        })
    }
    /// 3/10/60 minute views share the same observations. Completed intervals
    /// are bucketed to one second; the legacy 60-second diagnostic stays exact.
    pub fn snapshot_window(&mut self, now:Instant, seconds:u64)->Option<Snapshot> {
        let mut snapshot=self.snapshot(now)?;
        let window=Duration::from_secs(seconds.clamp(1,3600));
        let eligible=self.buckets.iter().filter(|b|now.saturating_duration_since(b.ended)<window);
        snapshot.long_gaps=usize::from(snapshot.ongoing_gap_ms.is_some());
        snapshot.max_gap_ms=snapshot.ongoing_gap_ms.unwrap_or(0.);
        for b in eligible {snapshot.long_gaps+=b.long_gaps;snapshot.max_gap_ms=snapshot.max_gap_ms.max(b.maximum.as_secs_f64()*1000.);}
        snapshot.observed_seconds=now.saturating_duration_since(self.started?).min(window).as_secs_f64();
        Some(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_windows_keep_the_same_observed_stall_and_expire_independently() {
        let start=Instant::now();let mut c=Continuity::default();c.set_active(true,60);
        c.submit(start);c.submit(start+Duration::from_millis(17));
        assert_eq!(c.snapshot(start+Duration::from_millis(100)).unwrap().long_gaps,1);
        c.set_active(false,60);c.set_active(true,60);
        c.submit(start+Duration::from_secs(200));c.submit(start+Duration::from_secs(200)+Duration::from_millis(17));
        let at=start+Duration::from_secs(201);
        // Observe at the last submission so no new ongoing stall is invented.
        c.submit(at);
        c.submit(at+Duration::from_millis(17));
        let at=at+Duration::from_millis(17);
        assert_eq!(c.snapshot_window(at,180).unwrap().long_gaps,1);
        assert_eq!(c.snapshot_window(at,600).unwrap().long_gaps,2);
        assert_eq!(c.snapshot_window(at,3600).unwrap().long_gaps,2);
        c.set_active(false,60);c.set_active(true,60);
        c.submit(start+Duration::from_secs(4000));c.submit(start+Duration::from_secs(4000)+Duration::from_millis(17));
        assert_eq!(c.snapshot_window(start+Duration::from_secs(4000)+Duration::from_millis(17),3600).unwrap().long_gaps,0);
        assert!(c.buckets.len()<=3602);
    }
    #[test]
    fn observed_stall_survives_pause_and_resumes_without_double_counting() {
        let start = Instant::now();
        let mut c = Continuity::default();
        c.set_active(true, 165);
        c.submit(start);
        c.submit(start + Duration::from_millis(6));
        let observed = start + Duration::from_millis(40);
        assert_eq!(c.snapshot(observed).unwrap().long_gaps, 1);
        c.set_active(false, 165);
        assert!(c.snapshot(start + Duration::from_secs(1)).is_none());
        c.set_active(true, 165);
        c.submit(start + Duration::from_secs(2));
        c.submit(start + Duration::from_secs(2) + Duration::from_millis(6));
        let s = c
            .snapshot(start + Duration::from_secs(2) + Duration::from_millis(6))
            .unwrap();
        assert_eq!(s.long_gaps, 1);
        assert_eq!(s.max_gap_ms, 34.);
        assert!(s.ongoing_gap_ms.is_none());
        c.set_active(false, 165);
        c.set_active(true, 165);
        c.submit(start + Duration::from_secs(61));
        c.submit(start + Duration::from_secs(61) + Duration::from_millis(6));
        assert_eq!(
            c.snapshot(start + Duration::from_secs(61) + Duration::from_millis(6))
                .unwrap()
                .long_gaps,
            0
        );
    }
    #[test]
    fn fps_change_preserves_only_previously_observed_stalls() {
        let start = Instant::now();
        let mut c = Continuity::default();
        c.set_active(true, 165);
        c.submit(start);
        c.submit(start + Duration::from_millis(6));
        assert_eq!(
            c.snapshot(start + Duration::from_millis(40))
                .unwrap()
                .long_gaps,
            1
        );
        c.set_active(true, 24);
        c.submit(start + Duration::from_secs(2));
        c.submit(start + Duration::from_secs(2) + Duration::from_millis(42));
        assert_eq!(
            c.snapshot(start + Duration::from_secs(2) + Duration::from_millis(42))
                .unwrap()
                .long_gaps,
            1
        );
        let mut unseen = Continuity::default();
        unseen.set_active(true, 165);
        unseen.submit(start);
        unseen.submit(start + Duration::from_millis(6));
        unseen.set_active(false, 165);
        unseen.set_active(true, 24);
        unseen.submit(start + Duration::from_secs(5));
        unseen.submit(start + Duration::from_secs(5) + Duration::from_millis(42));
        assert_eq!(
            unseen
                .snapshot(start + Duration::from_secs(5) + Duration::from_millis(42))
                .unwrap()
                .long_gaps,
            0
        );
    }
    #[test]
    fn one_freeze_counts_once_during_and_after_the_gap_then_expires() {
        let start = Instant::now();
        let mut c = Continuity::default();
        c.set_active(true, 165);
        assert!(c.snapshot(start).is_none());
        c.submit(start);
        assert!(c.snapshot(start).is_none());
        c.submit(start + Duration::from_millis(6));
        assert_eq!(
            c.snapshot(start + Duration::from_millis(10))
                .unwrap()
                .long_gaps,
            0
        );
        for ms in [30, 100, 200] {
            assert_eq!(
                c.snapshot(start + Duration::from_millis(ms))
                    .unwrap()
                    .long_gaps,
                1
            );
        }
        c.submit(start + Duration::from_millis(206));
        let s = c.snapshot(start + Duration::from_millis(206)).unwrap();
        assert_eq!(s.long_gaps, 1);
        assert_eq!(s.max_gap_ms, 200.);
        assert!(s.ongoing_gap_ms.is_none());
        c.set_active(false, 165);
        c.set_active(true, 165);
        c.submit(start + Duration::from_secs(61));
        c.submit(start + Duration::from_secs(61) + Duration::from_millis(6));
        assert_eq!(
            c.snapshot(start + Duration::from_secs(61) + Duration::from_millis(6))
                .unwrap()
                .long_gaps,
            0
        );
    }
    #[test]
    fn pause_fps_change_and_warmup_never_create_a_false_stall() {
        let start = Instant::now();
        let mut c = Continuity::default();
        c.set_active(true, 60);
        c.submit(start);
        c.submit(start + Duration::from_millis(17));
        c.set_active(false, 60);
        assert!(c.snapshot(start + Duration::from_secs(5)).is_none());
        c.set_active(true, 60);
        c.submit(start + Duration::from_secs(6));
        assert!(c.snapshot(start + Duration::from_secs(6)).is_none());
        c.submit(start + Duration::from_secs(6) + Duration::from_millis(17));
        assert_eq!(
            c.snapshot(start + Duration::from_secs(6) + Duration::from_millis(17))
                .unwrap()
                .long_gaps,
            0
        );
        c.set_active(true, 24);
        c.submit(start + Duration::from_secs(7));
        c.submit(start + Duration::from_secs(7) + Duration::from_millis(42));
        let s = c
            .snapshot(start + Duration::from_secs(7) + Duration::from_millis(42))
            .unwrap();
        assert_eq!(s.long_gaps, 0);
        assert_eq!(s.threshold_ms, 125.);
    }
    #[test]
    fn long_run_storage_and_maximum_expire_with_the_window() {
        let start = Instant::now();
        let mut c = Continuity::default();
        c.set_active(true, 240);
        for frame in 0..100_000 {
            c.submit(start + Duration::from_millis(frame * 5));
        }
        assert!(c.maxima.len() <= 61);
        assert!(c.gaps.is_empty());
        let now = start + Duration::from_millis(499_995);
        let s = c.snapshot(now).unwrap();
        assert_eq!(s.long_gaps, 0);
        assert_eq!(s.max_gap_ms, 5.);
        assert_eq!(s.observed_seconds, 60.);
    }
}
