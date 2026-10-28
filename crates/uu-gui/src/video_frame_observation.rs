//! Immutable image metadata. Never re-query a later decoder input at Present.
use serde::Serialize;
use std::sync::{atomic::Ordering, Arc, Weak};
use uu_core::{
    display_frames::{DecodedObservation, DisplayImage},
    media::MediaStats,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct FrameIdentity {
    pub media_epoch: u64,
    pub track: u32,
    pub actual_pts_us: u64,
    pub token: Option<FrameToken>,
    pub receive: Option<uu_core::pipeline_trace::ImageIdentity>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct FrameToken {
    pub media_epoch: u64,
    pub observation_epoch: u64,
    pub track: u32,
    pub ssrc: u32,
    pub rtp_timestamp: u32,
}
impl From<uu_core::video_e2e::FrameToken> for FrameToken {
    fn from(value: uu_core::video_e2e::FrameToken) -> Self {
        Self {
            media_epoch: value.media_epoch,
            observation_epoch: value.observation_epoch,
            track: value.track,
            ssrc: value.ssrc,
            rtp_timestamp: value.rtp_timestamp,
        }
    }
}
#[derive(Clone)]
pub(crate) struct FrameObservation {
    stats: Weak<MediaStats>,
    identity: FrameIdentity,
}
impl FrameObservation {
    pub(crate) fn new(
        stats: &Arc<MediaStats>,
        image: &DisplayImage,
        frozen: Option<DecodedObservation>,
    ) -> Option<Self> {
        let (track, pts) = match image {
            DisplayImage::Decoded(track, frame, _) => (*track, frame.pts.0),
            DisplayImage::Rgba(frame) => (frame.0, frozen?.pts_us),
        };
        if stats.native_media_retired.load(Ordering::Acquire) {
            return None;
        }
        let epoch = stats.native_media_epoch.load(Ordering::Acquire);
        if frozen.is_some_and(|value| {
            value.media_epoch != epoch || value.track != track || value.pts_us != pts
        }) {
            return None;
        }
        let token = frozen
            .and_then(|value| value.token)
            .filter(|token| token.media_epoch == epoch && token.track == track)
            .map(FrameToken::from);
        let receive = frozen.and_then(|value| value.receive).filter(|value| {
            value.media_epoch == epoch && value.track == track && value.pts_us == pts
        });
        Some(Self {
            stats: Arc::downgrade(stats),
            identity: FrameIdentity {
                media_epoch: epoch,
                track,
                actual_pts_us: pts,
                token,
                receive,
            },
        })
    }
    /// A queued or cached image cannot regain the identity of a retired session.
    pub(crate) fn current_identity(&self) -> Option<FrameIdentity> {
        let stats = self.stats.upgrade()?;
        (stats.native_media_epoch.load(Ordering::Acquire) == self.identity.media_epoch
            && !stats.native_media_retired.load(Ordering::Acquire))
        .then_some(self.identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uu_core::display_frames::{RawFrame, Timestamp, YuvColor};
    fn image() -> DisplayImage {
        DisplayImage::Decoded(
            0,
            RawFrame::new_i420(2, 2, vec![16; 6], Timestamp(0)),
            YuvColor::Limited709,
        )
    }
    #[test]
    fn decoded_identity_is_frozen_and_session_retirement_clears_it() {
        let stats = Arc::new(MediaStats::default());
        stats.native_media_epoch.store(7, Ordering::Release);
        let old = uu_core::video_e2e::FrameToken {
            media_epoch: 7,
            observation_epoch: 1,
            track: 0,
            ssrc: 9,
            rtp_timestamp: 123,
        };
        let frozen = DecodedObservation {
            media_epoch: 7,
            track: 0,
            pts_us: 0,
            token: Some(old),
            receive: None,
        };
        let observation = FrameObservation::new(&stats, &image(), Some(frozen)).unwrap();
        let redraw = observation.clone();
        assert_eq!(redraw.current_identity().unwrap().token.unwrap().ssrc, 9);
        stats.native_media_epoch.store(8, Ordering::Release);
        assert!(observation.current_identity().is_none());
        assert!(FrameObservation::new(&stats, &image(), Some(frozen)).is_none());
    }
    #[test]
    fn missing_metadata_never_reconstructs_a_token_from_a_newer_input() {
        let stats = Arc::new(MediaStats::default());
        let observation = FrameObservation::new(&stats, &image(), None).unwrap();
        let identity = observation.current_identity().unwrap();
        assert_eq!(identity.actual_pts_us, 0);
        assert!(identity.token.is_none() && identity.receive.is_none());
        assert!(
            FrameObservation::new(&stats, &DisplayImage::Rgba((0, 2, 2, vec![0; 16])), None)
                .is_none()
        );
        drop(stats);
        assert!(observation.current_identity().is_none());
    }
    #[test]
    fn converted_rgba_identity_stays_with_its_texture_and_legacy_set_clears_it() {
        let stats = Arc::new(MediaStats::default());
        let ctx = eframe::egui::Context::default();
        let frozen = DecodedObservation {
            media_epoch: 0,
            track: 0,
            pts_us: 123,
            token: None,
            receive: None,
        };
        let image = || DisplayImage::Rgba((0, 2, 2, vec![0; 16]));
        let mut texture =
            super::super::VideoTexture::new_observed(&ctx, None, image(), &stats, Some(frozen));
        assert_eq!(
            texture.fallback_frame_identity().unwrap().actual_pts_us,
            123
        );
        texture.set(&ctx, image());
        assert!(texture.fallback_frame_identity().is_none());
    }
}
