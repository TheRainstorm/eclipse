//! One refresh clock, independent of network requests and gallery scans.
use crate::settings::WallpaperRefresh;
use std::time::{Duration, Instant};

pub(super) struct Schedule {
    policy: WallpaperRefresh,
    hours: u32,
    observed: Option<(Instant, Duration)>,
    due: Option<Instant>,
}
impl Default for Schedule {
    fn default() -> Self {
        Self {
            policy: WallpaperRefresh::EveryHours,
            hours: 24,
            observed: None,
            due: None,
        }
    }
}
impl Schedule {
    fn interval(&self) -> Duration {
        Duration::from_secs(u64::from(self.hours) * 3600)
    }
    pub fn configure(&mut self, policy: WallpaperRefresh, hours: u32, now: Instant) {
        if self.policy == policy && self.hours == hours {
            return;
        }
        self.policy = policy;
        self.hours = hours;
        self.due = if policy == WallpaperRefresh::EveryHours {
            self.observed.map(|(time, age)| {
                now + self
                    .interval()
                    .saturating_sub(age.saturating_add(now.saturating_duration_since(time)))
            })
        } else {
            None
        };
    }
    pub fn reset(&mut self) {
        self.observed = None;
        self.due = None;
    }
    pub fn opened(&mut self, age: Option<Duration>, now: Instant) -> bool {
        self.observed = age.map(|age| (now, age));
        let refresh = match (self.policy, age) {
            (_, None) => true,
            (WallpaperRefresh::Never, Some(_)) => false,
            (WallpaperRefresh::OnOpen, Some(_)) => true,
            (WallpaperRefresh::EveryHours, Some(age)) => age >= self.interval(),
        };
        self.due = if !refresh && self.policy == WallpaperRefresh::EveryHours {
            Some(now + self.interval().saturating_sub(age.unwrap_or_default()))
        } else {
            None
        };
        refresh
    }
    pub fn started(&mut self) {
        self.due = None;
    }
    pub fn finished(&mut self, success: bool, now: Instant) {
        if success {
            self.observed = Some((now, Duration::ZERO));
        }
        self.due = if self.policy == WallpaperRefresh::EveryHours {
            Some(
                now + if success {
                    self.interval()
                } else {
                    self.interval().min(Duration::from_secs(300))
                },
            )
        } else {
            None
        };
    }
    pub fn take_due(&mut self, now: Instant) -> bool {
        if self.due.is_some_and(|due| now >= due) {
            self.due = None;
            true
        } else {
            false
        }
    }
    pub fn wake(&self, ctx: &eframe::egui::Context) {
        if let Some(due) = self.due {
            ctx.request_repaint_after(due.saturating_duration_since(Instant::now()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opening_policies_and_interval_changes_use_the_original_cache_age() {
        let now = Instant::now();
        let mut schedule = Schedule::default();
        schedule.configure(WallpaperRefresh::Never, 24, now);
        assert!(!schedule.opened(Some(Duration::from_secs(90000)), now));
        assert!(!schedule.take_due(now + Duration::from_secs(1_000_000)));
        assert!(schedule.opened(None, now));
        schedule.configure(WallpaperRefresh::OnOpen, 24, now);
        assert!(schedule.opened(Some(Duration::ZERO), now));
        schedule.finished(true, now);
        assert!(!schedule.take_due(now + Duration::from_secs(90000)));
        schedule.configure(WallpaperRefresh::EveryHours, 2, now);
        assert!(!schedule.opened(Some(Duration::from_secs(3600)), now));
        assert!(!schedule.take_due(now + Duration::from_secs(3599)));
        assert!(schedule.take_due(now + Duration::from_secs(3600)));
        assert!(!schedule.take_due(now + Duration::from_secs(3601)));
        schedule.opened(Some(Duration::from_secs(3600)), now);
        schedule.configure(
            WallpaperRefresh::EveryHours,
            1,
            now + Duration::from_secs(1),
        );
        assert!(schedule.take_due(now + Duration::from_secs(1)));
    }
    #[test]
    fn failures_back_off_without_changing_never_or_open_policies() {
        let now = Instant::now();
        let mut schedule = Schedule::default();
        schedule.finished(false, now);
        assert!(!schedule.take_due(now + Duration::from_secs(299)));
        assert!(schedule.take_due(now + Duration::from_secs(300)));
        schedule.configure(WallpaperRefresh::Never, 24, now);
        schedule.finished(false, now);
        assert!(!schedule.take_due(now + Duration::from_secs(90000)));
    }
}
