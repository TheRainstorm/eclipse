//! Same-image DXGI count receipts. This is an empirical interval, not a precise
//! display timestamp or a platform guarantee for multiple-monitor statistics.
use super::frame_observation::{FrameIdentity, FrameObservation};
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicU64, Ordering},
};
use uu_core::{
    display_frames::hdr::RawPresentObservation,
    video_e2e::{DisplayMethod, DisplayObservation, FrameToken},
};

const LIMIT: usize = 512;
static NEXT_OBSERVER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Image {
    token: FrameToken,
    pts: u64,
}
impl Image {
    fn new(identity: FrameIdentity) -> Option<Self> {
        let token = identity.token?;
        Some(Self {
            token: FrameToken {
                media_epoch: token.media_epoch,
                observation_epoch: token.observation_epoch,
                track: token.track,
                ssrc: token.ssrc,
                rtp_timestamp: token.rtp_timestamp,
            },
            pts: identity.actual_pts_us,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Scope {
    chain: u64,
    address: u64,
    surface: u64,
    frequency: u64,
}
#[derive(Clone, Copy)]
struct Submitted {
    count: u32,
    image: Image,
}
#[derive(Clone, Copy)]
struct First {
    image: Image,
    before: u64,
    received: bool,
}
#[derive(Clone, Copy, Debug)]
struct Receipt {
    image: Image,
    count: u32,
    earliest: u64,
    latest: u64,
    frequency: u64,
}
#[derive(Default)]
struct Ledger {
    scope: Option<Scope>,
    serial: u64,
    count: u32,
    stats_count: u32,
    received: u32,
    last_after: i64,
    pending: VecDeque<Submitted>,
    first: VecDeque<First>,
    last_image: Option<Image>,
    // A gap cannot turn a cached old image's next submission into its first.
    blocked: Option<Image>,
}
enum Update {
    Invalid,
    Current {
        new_scope: bool,
        receipt: Option<Receipt>,
    },
}
impl Ledger {
    fn reset(&mut self, blocked: Option<Image>) {
        *self = Self {
            blocked: blocked.or(self.last_image).or(self.blocked),
            ..Self::default()
        };
    }
    fn observe(&mut self, r: RawPresentObservation, image: Image) -> Update {
        let times = [
            r.present_before_qpc,
            r.present_after_qpc,
            r.last_count_query_before_qpc,
            r.last_count_query_after_qpc,
            r.stats_query_before_qpc,
            r.stats_query_after_qpc,
        ];
        // Positive DXGI status codes (including OCCLUDED) are not receipts.
        if !r.valid_layout()
            || r.present_hresult != 0
            || r.last_present_count_hresult != 0
            || r.stats_hresult != 0
            || r.last_present_count == 0
            || r.stats_present_count == 0
            || r.serial == 0
            || r.chain_id == 0
            || r.chain_address == 0
            || r.surface_epoch == 0
            || r.surface_epoch_after_query != r.surface_epoch
            || r.qpc_frequency == 0
            || r.stats_present_count > r.last_present_count
            || r.present_flags & 5 != 0
            || times[0] <= 0
            || times.windows(2).any(|v| v[0] > v[1])
        {
            self.reset(Some(image));
            return Update::Invalid;
        }
        let scope = Scope {
            chain: r.chain_id,
            address: r.chain_address,
            surface: r.surface_epoch,
            frequency: r.qpc_frequency,
        };
        let new_scope = self.scope != Some(scope);
        if new_scope && self.scope.is_some() {
            self.reset(None);
        }
        if self.blocked == Some(image) {
            return Update::Invalid;
        }
        if !new_scope
            && (self.serial.checked_add(1) != Some(r.serial)
                || self.count.checked_add(1) != Some(r.last_present_count)
                || r.stats_present_count < self.stats_count
                || r.present_before_qpc < self.last_after)
        {
            self.reset(Some(image));
            return Update::Invalid;
        }
        if self.pending.len() == LIMIT || self.first.len() == LIMIT {
            self.reset(Some(image));
            return Update::Invalid;
        }
        self.scope = Some(scope);
        self.blocked = None;
        self.serial = r.serial;
        self.count = r.last_present_count;
        self.stats_count = r.stats_present_count;
        self.last_after = r.stats_query_after_qpc;
        self.last_image = Some(image);
        self.pending.push_back(Submitted {
            count: r.last_present_count,
            image,
        });
        if !self.first.iter().any(|first| first.image == image) {
            self.first.push_back(First {
                image,
                before: r.present_before_qpc as u64,
                received: false,
            });
        }
        let count = r.stats_present_count;
        let receipt = if count <= self.received {
            None
        } else {
            self.pending
                .iter()
                .find(|p| p.count == count)
                .and_then(|p| {
                    let first = self.first.iter_mut().find(|first| first.image == p.image)?;
                    first.received = true;
                    Some(Receipt {
                        image: p.image,
                        count,
                        earliest: first.before,
                        latest: r.stats_query_after_qpc as u64,
                        frequency: r.qpc_frequency,
                    })
                })
        };
        if receipt.is_some() {
            self.received = count;
            // A skipped Count is unobserved, not proven dropped. Keep its image's
            // earliest submission until it receives a receipt or the scope retires.
            self.pending.retain(|p| p.count > count);
            self.first.retain(|f| {
                !f.received || f.image == image || self.pending.iter().any(|p| p.image == f.image)
            });
        }
        Update::Current { new_scope, receipt }
    }
}

#[derive(Default)]
pub(crate) struct Bridge {
    ledger: Ledger,
    owner: Option<FrameObservation>,
    observer: u64,
    // Clear atomically invalidates immediately; a contended metadata lock is
    // retried without waiting and cannot clear a replacement window's observer.
    retiring: VecDeque<(FrameObservation, u64)>,
}
impl Bridge {
    pub(crate) fn wanted(frame: Option<&FrameObservation>) -> bool {
        frame
            .and_then(FrameObservation::current_stats)
            .is_some_and(|stats| stats.video_e2e.supported())
    }
    fn drain_retiring(&mut self) {
        self.retiring
            .retain(|(owner, observer)| !owner.clear_display_observer(*observer));
    }
    fn retire_owner(&mut self) {
        if let Some(owner) = self.owner.take() {
            if self.observer != 0 && !owner.clear_display_observer(self.observer) {
                // All retained owners are weak. Their active receipt has already
                // been invalidated, even when metadata cleanup could not lock.
                if self.retiring.len() == 16 {
                    self.retiring.pop_front();
                }
                self.retiring.push_back((owner, self.observer));
            }
        }
        self.observer = 0;
    }
    pub(crate) fn reset(&mut self) {
        self.drain_retiring();
        self.retire_owner();
        self.ledger.reset(None);
    }
    pub(crate) fn record(
        &mut self,
        raw: RawPresentObservation,
        offered: Option<(u64, u64)>,
        frame: Option<&FrameObservation>,
        pixel_inspection: bool,
    ) {
        self.drain_retiring();
        let Some(frame) = frame else {
            self.reset();
            return;
        };
        let identity = frame.current_identity();
        let image = identity.and_then(Image::new);
        let valid_offer =
            offered.is_some_and(|(id, ready)| ready != 0 && raw.matches_native_image(id, ready));
        if pixel_inspection || !valid_offer || image.is_none() || !Self::wanted(Some(frame)) {
            self.retire_owner();
            self.ledger.reset(image);
            return;
        }
        let image = image.unwrap();
        if self
            .owner
            .as_ref()
            .is_some_and(|owner| !owner.same_display_owner(frame))
        {
            self.reset();
            // Equal numeric tokens from different weak owners are unrelated.
            self.ledger = Ledger::default();
        }
        let update = self.ledger.observe(raw, image);
        let Update::Current { new_scope, receipt } = update else {
            self.retire_owner();
            return;
        };
        if new_scope || self.owner.is_none() {
            self.retire_owner();
            // Independent from the native epoch: error/gap recovery must not
            // revive an observer ID that Core has already retired.
            let epoch = NEXT_OBSERVER
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                    next.checked_add(1)
                })
                .unwrap_or(0);
            if epoch == 0 || !frame.set_display_observer(epoch) {
                self.ledger.reset(Some(image));
                return;
            }
            self.owner = Some(frame.clone());
            self.observer = epoch;
        }
        if let Some(receipt) = receipt {
            let Some(stats) = frame.current_stats() else {
                self.reset();
                return;
            };
            let Some(now) = stats.video_e2e.clock_stamp() else {
                self.reset();
                return;
            };
            if now.frequency_hz != receipt.frequency
                || receipt.latest > now.ticks
                || !stats.video_e2e.current_token(receipt.image.token)
            {
                self.reset();
                return;
            }
            let mut earliest = now;
            earliest.ticks = receipt.earliest;
            let mut latest = now;
            latest.ticks = receipt.latest;
            // `false` can also mean a duplicate/no source metadata; Core retires
            // observation_epoch on actual metadata loss, checked next frame.
            stats.video_e2e.displayed(
                receipt.image.token,
                DisplayObservation {
                    surface_epoch: self.observer,
                    present_id: u64::from(receipt.count),
                    earliest,
                    latest,
                    method: DisplayMethod::DxgiReceiptInterval,
                },
            );
        }
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn image(n: u32) -> Image {
        Image {
            token: FrameToken {
                media_epoch: 1,
                observation_epoch: 2,
                track: 0,
                ssrc: 3,
                rtp_timestamp: n,
            },
            pts: u64::from(n),
        }
    }
    fn raw(serial: u64, stats: u32) -> RawPresentObservation {
        let before = serial as i64 * 1000;
        RawPresentObservation {
            version: 1,
            struct_size: 176,
            serial,
            chain_id: 7,
            chain_address: 8,
            surface_epoch: 9,
            surface_epoch_after_query: 9,
            qpc_frequency: 10_000_000,
            present_before_qpc: before,
            present_after_qpc: before + 10,
            last_count_query_before_qpc: before + 10,
            last_count_query_after_qpc: before + 11,
            stats_query_before_qpc: before + 11,
            stats_query_after_qpc: before + 12,
            last_present_count: serial as u32,
            stats_present_count: stats,
            ..Default::default()
        }
    }
    fn receipt(update: Update) -> Option<Receipt> {
        match update {
            Update::Current { receipt, .. } => receipt,
            _ => None,
        }
    }
    #[test]
    fn delayed_count_uses_its_own_frame_and_zero_refresh_fields_are_valid() {
        let mut l = Ledger::default();
        let first = receipt(l.observe(raw(1, 1), image(11))).unwrap();
        assert_eq!(first.earliest, 1000);
        assert!(receipt(l.observe(raw(2, 1), image(12))).is_none());
        let r = receipt(l.observe(raw(3, 2), image(13))).unwrap();
        assert_eq!(r.image, image(12));
        assert_eq!((r.count, r.earliest, r.latest), (2, 2000, 3012));
    }
    #[test]
    fn repeated_image_keeps_earliest_submission_and_does_not_invent_skipped_receipts() {
        let mut l = Ledger::default();
        l.observe(raw(1, 1), image(10));
        l.observe(raw(2, 1), image(20));
        l.observe(raw(3, 1), image(20));
        let r = receipt(l.observe(raw(4, 3), image(30))).unwrap();
        assert_eq!(r.image, image(20));
        assert_eq!(r.earliest, 2000);
        assert_eq!(r.count, 3);
        assert!(receipt(l.observe(raw(5, 3), image(30))).is_none());
    }
    #[test]
    fn first_query_may_refer_to_a_present_before_observation_began() {
        let mut l = Ledger::default();
        assert!(receipt(l.observe(raw(50, 49), image(1))).is_none());
        let r = receipt(l.observe(raw(51, 50), image(2))).unwrap();
        assert_eq!(r.image, image(1));
        assert_eq!(r.earliest, 50_000);
    }
    #[test]
    fn error_zero_or_gap_blocks_cached_frame_until_a_new_token() {
        let mut invalids = vec![raw(2, 0), raw(4, 3), raw(2, 3)];
        let mut occluded = raw(2, 1);
        occluded.present_hresult = 0x087a0001;
        invalids.push(occluded);
        let mut failed = raw(2, 1);
        failed.last_present_count_hresult = -1;
        invalids.push(failed);
        let mut disjoint = raw(2, 1);
        disjoint.stats_hresult = -2005270517;
        invalids.push(disjoint);
        let mut changed = raw(2, 1);
        changed.surface_epoch_after_query += 1;
        invalids.push(changed);
        for bad in invalids {
            let mut l = Ledger::default();
            l.observe(raw(1, 1), image(1));
            assert!(matches!(l.observe(bad, image(2)), Update::Invalid));
            assert!(matches!(l.observe(raw(10, 9), image(2)), Update::Invalid));
            assert!(matches!(
                l.observe(raw(11, 10), image(3)),
                Update::Current { .. }
            ));
            assert!(receipt(l.observe(raw(12, 11), image(4))).is_some());
        }
    }
    #[test]
    fn surface_or_frequency_change_cannot_reassign_cached_image_first_submission() {
        for mutate in [0, 1, 2] {
            let mut l = Ledger::default();
            l.observe(raw(1, 1), image(1));
            let mut r = raw(2, 1);
            match mutate {
                0 => {
                    r.surface_epoch += 1;
                    r.surface_epoch_after_query += 1;
                }
                1 => {
                    r.chain_id += 1;
                }
                _ => {
                    r.qpc_frequency += 1;
                }
            }
            assert!(matches!(l.observe(r, image(1)), Update::Invalid));
        }
    }
    #[test]
    fn stats_regression_and_present_count_wrap_retire_the_scope() {
        let mut l = Ledger::default();
        l.observe(raw(1, 1), image(1));
        l.observe(raw(2, 2), image(2));
        assert!(matches!(l.observe(raw(3, 1), image(3)), Update::Invalid));
        let mut l = Ledger::default();
        let mut r = raw(1, u32::MAX);
        r.last_present_count = u32::MAX;
        l.observe(r, image(1));
        assert!(matches!(l.observe(raw(2, 1), image(2)), Update::Invalid));
    }
    #[test]
    fn unobserved_images_are_bounded_and_survive_count_jumps() {
        let mut l = Ledger::default();
        l.observe(raw(1, 1), image(1));
        l.observe(raw(2, 1), image(2));
        l.observe(raw(3, 3), image(3));
        let r = receipt(l.observe(raw(4, 4), image(2))).unwrap();
        assert_eq!(
            r.earliest, 2000,
            "unobserved Count 2 could have displayed earlier"
        );
        let mut l = Ledger::default();
        for n in 1..=(LIMIT as u32) {
            l.observe(raw(u64::from(n), 1), image(n));
        }
        assert!(matches!(
            l.observe(raw(LIMIT as u64 + 1, 1), image(900)),
            Update::Invalid
        ));
        assert!(l.pending.is_empty() && l.first.is_empty());
    }
}
