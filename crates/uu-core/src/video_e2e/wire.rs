//! Optional v1 channel. Existing strict control/trace schemas remain unchanged.
use super::{FourTimestamp, Host, SourceFrame, Stamp, WireClockStamp};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::{atomic::Ordering, Arc, Mutex},
    time::{Duration, Instant},
};
use webrtc::{
    data_channel::{
        data_channel_init::RTCDataChannelInit, data_channel_state::RTCDataChannelState,
        RTCDataChannel,
    },
    peer_connection::{sdp::session_description::RTCSessionDescription, RTCPeerConnection},
};

const ATTRIBUTE: &str = "a=x-eclipse-video-e2e:1";
const LABEL: &str = "eclipse-video-e2e-v1";
const LIFETIME_MS: u16 = 100;
const MAX_JSON: usize = 32 * 1024;
const MAX_FRAMES: usize = 64;

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Packet {
    ClockRequest {
        sequence: u64,
        #[serde(with = "Stamp")]
        client_send: WireClockStamp,
    },
    ClockResponse {
        sequence: u64,
        #[serde(with = "Stamp")]
        client_send: WireClockStamp,
        #[serde(with = "Stamp")]
        host_receive: WireClockStamp,
        #[serde(with = "Stamp")]
        host_send: WireClockStamp,
    },
    Frames {
        host_epoch: u64,
        frames: Vec<SourceFrame>,
    },
}
fn parse(data: &[u8]) -> Option<Packet> {
    if data.len() > MAX_JSON {
        return None;
    }
    let packet: Packet = serde_json::from_slice(data).ok()?;
    match &packet {
        Packet::ClockRequest { client_send, .. }
            if client_send.epoch == 0 || client_send.interval_ns().is_none() =>
        {
            None
        }
        Packet::ClockResponse {
            client_send,
            host_receive,
            host_send,
            ..
        } if client_send.epoch == 0
            || host_receive.epoch == 0
            || host_receive.epoch != host_send.epoch
            || host_receive.frequency_hz != host_send.frequency_hz
            || host_receive.ticks > host_send.ticks
            || [client_send, host_receive, host_send]
                .iter()
                .any(|stamp| stamp.interval_ns().is_none()) =>
        {
            None
        }
        Packet::Frames { host_epoch, frames }
            if *host_epoch == 0 || frames.is_empty() || frames.len() > MAX_FRAMES =>
        {
            None
        }
        _ => Some(packet),
    }
}
async fn send(channel: &RTCDataChannel, packet: Packet) {
    if channel.ready_state() != RTCDataChannelState::Open || channel.buffered_amount().await > 0 {
        return;
    }
    let Ok(data) = serde_json::to_vec(&packet) else {
        return;
    };
    if data.len() > MAX_JSON {
        return;
    }
    let _ = tokio::time::timeout(Duration::from_millis(20), channel.send(&data.into())).await;
}
pub(crate) fn offered(description: &RTCSessionDescription) -> bool {
    let mut attributes = description
        .sdp
        .lines()
        .filter(|line| line.starts_with("a=x-eclipse-video-e2e:"));
    attributes.next() == Some(ATTRIBUTE) && attributes.next().is_none()
}
pub(crate) fn advertise(description: &mut RTCSessionDescription) {
    description.sdp.push_str(ATTRIBUTE);
    description.sdp.push_str("\r\n");
}
pub(crate) fn accepts_channel(channel: &RTCDataChannel) -> bool {
    channel.label() == LABEL
        && !channel.ordered()
        && channel.max_packet_lifetime() == LIFETIME_MS
        && channel.max_retransmits() == 0
}
pub(crate) fn host_channel(channel: Arc<RTCDataChannel>, host: Arc<Host>) {
    let weak = Arc::downgrade(&channel);
    let handler = host.clone();
    let last_request = Arc::new(Mutex::new(None::<Instant>));
    channel.on_message(Box::new(move |message| {
        let received = handler.clock.stamp();
        let host = handler.clone();
        let weak = weak.clone();
        let accepted = if let Some(Packet::ClockRequest {
            sequence,
            client_send,
        }) = parse(&message.data)
        {
            let mut last = last_request.lock().unwrap();
            if last.is_some_and(|at| at.elapsed() < Duration::from_millis(100)) {
                None
            } else {
                *last = Some(Instant::now());
                Some((sequence, client_send))
            }
        } else {
            None
        };
        Box::pin(async move {
            let (Some((sequence, client_send)), Some(host_receive), Some(channel)) =
                (accepted, received, weak.upgrade())
            else {
                return;
            };
            let Some(host_send) = host.clock.stamp() else {
                return;
            };
            send(
                &channel,
                Packet::ClockResponse {
                    sequence,
                    client_send,
                    host_receive,
                    host_send,
                },
            )
            .await;
        })
    }));
    let weak = Arc::downgrade(&channel);
    channel.on_open(Box::new(move || {
        let weak = weak.clone();
        let host = host.clone();
        Box::pin(async move {
            tokio::spawn(async move {
                let mut timer = tokio::time::interval(Duration::from_millis(100));
                timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    timer.tick().await;
                    let Some(channel) = weak.upgrade() else {
                        break;
                    };
                    if channel.ready_state() != RTCDataChannelState::Open {
                        break;
                    }
                    if channel.buffered_amount().await > 0 {
                        continue;
                    }
                    let frames = {
                        let mut frames = host.frames.lock().unwrap();
                        let count = frames.len().min(MAX_FRAMES);
                        frames.drain(..count).collect::<Vec<_>>()
                    };
                    if !frames.is_empty() {
                        send(
                            &channel,
                            Packet::Frames {
                                host_epoch: host.clock.epoch,
                                frames,
                            },
                        )
                        .await;
                    }
                }
            });
        })
    }));
}
pub(crate) async fn viewer_channel(
    pc: &RTCPeerConnection,
    stats: Arc<crate::media::MediaStats>,
) -> anyhow::Result<Arc<RTCDataChannel>> {
    let channel = pc
        .create_data_channel(
            LABEL,
            Some(RTCDataChannelInit {
                ordered: Some(false),
                max_packet_life_time: Some(LIFETIME_MS),
                ..Default::default()
            }),
        )
        .await?;
    let epoch = stats.native_media_epoch.load(Ordering::Acquire);
    let pending = Arc::new(Mutex::new(VecDeque::<(u64, WireClockStamp, Instant)>::new()));
    let received_stats = stats.clone();
    let received_pending = pending.clone();
    channel.on_message(Box::new(move |message| {
        let now = received_stats.video_e2e.clock_stamp();
        if received_stats.native_media_epoch.load(Ordering::Acquire) == epoch {
            match parse(&message.data) {
                Some(Packet::ClockResponse {
                    sequence,
                    client_send,
                    host_receive,
                    host_send,
                }) => {
                    let expected = {
                        let mut pending = received_pending.lock().unwrap();
                        pending
                            .iter()
                            .position(|(id, _, _)| *id == sequence)
                            .and_then(|index| pending.remove(index))
                    };
                    if let (Some((_, expected, sent)), Some(client_receive)) = (expected, now) {
                        if expected == client_send && sent.elapsed() <= Duration::from_secs(2) {
                            received_stats.video_e2e.clock_sample(
                                epoch,
                                FourTimestamp {
                                    client_send,
                                    host_receive,
                                    host_send,
                                    client_receive,
                                },
                            );
                        }
                    }
                }
                Some(Packet::Frames { host_epoch, frames }) => {
                    received_stats
                        .video_e2e
                        .source_frames(epoch, host_epoch, &frames);
                }
                _ => {}
            }
        }
        Box::pin(async {})
    }));
    let weak = Arc::downgrade(&channel);
    channel.on_open(Box::new(move || {
        let weak = weak.clone();
        let stats = stats.clone();
        let pending = pending.clone();
        Box::pin(async move {
            tokio::spawn(async move {
                let mut sequence = 0u64;
                let mut timer = tokio::time::interval(Duration::from_millis(250));
                timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    timer.tick().await;
                    let Some(channel) = weak.upgrade() else {
                        break;
                    };
                    if channel.ready_state() != RTCDataChannelState::Open
                        || stats.native_media_epoch.load(Ordering::Acquire) != epoch
                    {
                        break;
                    }
                    if channel.buffered_amount().await > 0 {
                        continue;
                    }
                    let Some(client_send) = stats.video_e2e.clock_stamp() else {
                        continue;
                    };
                    sequence = sequence.wrapping_add(1);
                    {
                        let mut pending = pending.lock().unwrap();
                        while pending
                            .front()
                            .is_some_and(|(_, _, sent)| sent.elapsed() > Duration::from_secs(2))
                        {
                            pending.pop_front();
                        }
                        if pending.len() == 8 {
                            pending.pop_front();
                        }
                        pending.push_back((sequence, client_send, Instant::now()));
                    }
                    send(
                        &channel,
                        Packet::ClockRequest {
                            sequence,
                            client_send,
                        },
                    )
                    .await;
                }
            });
        })
    }));
    Ok(channel)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn optional_channel_exchanges_clocks_and_sources_without_a_display_endpoint() {
        use crate::{media::MediaStats, native_media::NativePeer};
        let ip = "127.0.0.1".parse().unwrap();
        let viewer = NativePeer::new(ip, None, false).await.unwrap();
        let host_peer = NativePeer::new(ip, None, true).await.unwrap();
        let host = Arc::new(Host::default());
        host.enable(true);
        let handler = host.clone();
        host_peer.pc.on_data_channel(Box::new(move |channel| {
            let host = handler.clone();
            Box::pin(async move {
                assert!(accepts_channel(&channel));
                host_channel(channel, host);
            })
        }));
        let stats = Arc::new(MediaStats::default());
        stats.native_media_epoch.store(7, Ordering::Release);
        stats.video_e2e.begin(7, true);
        let _channel = viewer_channel(&viewer.pc, stats.clone()).await.unwrap();
        let source = host.clock.stamp().unwrap();
        host.sent(
            Some(super::super::SourceEndpoint {
                generation: 1,
                frame_id: 1,
                duplicate: false,
                presented: source,
            }),
            42,
            100,
        );
        let result = tokio::time::timeout(Duration::from_secs(8), async {
            let offer = viewer.description(true).await.unwrap();
            host_peer.remote(offer, ip).await.unwrap();
            let answer = host_peer.description(false).await.unwrap();
            viewer.remote(answer, ip).await.unwrap();
            loop {
                let mapped = {
                    let state = stats.video_e2e.state.lock().unwrap();
                    state.source_frames == 1
                        && state
                            .clock
                            .map_host(source, stats.video_e2e.clock_stamp().unwrap())
                            .is_some()
                };
                if mapped {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(
                stats.video_e2e.snapshot().measurement.is_none(),
                "clock + source must never stand in for display"
            );
            assert_eq!(
                stats.video_e2e.snapshot().unknown,
                Some(super::super::UnknownReason::DisplayTimestampUnavailable)
            );
            stats.native_media_epoch.store(8, Ordering::Release);
            stats.video_e2e.begin(8, true);
            host.sent(None, 42, 101);
            tokio::time::sleep(Duration::from_millis(150)).await;
            assert_eq!(
                stats.video_e2e.snapshot().source_frames,
                0,
                "retired channel callbacks cannot update a new media epoch"
            );
        })
        .await;
        viewer.close().await;
        host_peer.close().await;
        assert!(
            result.is_ok(),
            "bounded local SCTP exchange timed out: {result:?}"
        );
    }
    #[test]
    fn independent_capability_requires_one_exact_supported_version() {
        let description = |sdp: &str| {
            let mut value = RTCSessionDescription::default();
            value.sdp = sdp.into();
            value
        };
        assert!(offered(&description("v=0\r\na=x-eclipse-video-e2e:1\r\n")));
        for sdp in [
            "v=0\r\n",
            "a=x-eclipse-video-e2e:2\r\n",
            "a=x-eclipse-video-e2e:1\r\na=x-eclipse-video-e2e:1\r\n",
        ] {
            assert!(!offered(&description(sdp)));
        }
    }
    #[test]
    fn wire_rejects_unknown_fields_invalid_clocks_and_oversized_batches() {
        assert!(parse(br#"{"type":"clock_request","sequence":1,"client_send":{"epoch":1,"ticks":1,"frequency_hz":1000}}"#).is_some());
        assert!(parse(br#"{"type":"clock_request","sequence":1,"client_send":{"epoch":1,"ticks":1,"frequency_hz":0}}"#).is_none());
        assert!(parse(br#"{"type":"clock_request","sequence":1,"client_send":{"epoch":1,"ticks":1,"frequency_hz":1000},"surprise":0}"#).is_none());
        assert!(parse(br#"{"type":"frames","host_epoch":1,"frames":[]}"#).is_none());
        assert!(parse(&vec![b' '; MAX_JSON + 1]).is_none());
        let frames = vec![
            SourceFrame {
                ssrc: 1,
                rtp_timestamp: 1,
                source: None
            };
            MAX_FRAMES + 1
        ];
        assert!(parse(
            &serde_json::to_vec(&Packet::Frames {
                host_epoch: 1,
                frames
            })
            .unwrap()
        )
        .is_none());
    }
}
