//! Optional v1 channel. Existing strict control/trace schemas remain unchanged.
use super::{FourTimestamp, Host, SourceFrame, Stamp, WireClockStamp};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    future::Future,
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
/// True means the full message was accepted by the local SCTP send API. It is
/// not a peer acknowledgement; timeout/cancellation can have an unknown result.
async fn send(channel: &RTCDataChannel, packet: Packet) -> bool {
    let Ok(data) = serde_json::to_vec(&packet) else {
        return false;
    };
    if data.len() > MAX_JSON {
        return false;
    }
    let len = data.len();
    tokio::time::timeout(Duration::from_millis(20), async {
        if channel.ready_state() != RTCDataChannelState::Open || channel.buffered_amount().await > 0
        {
            return false;
        }
        matches!(channel.send(&data.into()).await, Ok(written) if written == len)
    })
    .await
    .unwrap_or(false)
}
fn same_frame(a: &SourceFrame, b: &SourceFrame) -> bool {
    a.ssrc == b.ssrc
        && a.rtp_timestamp == b.rtp_timestamp
        && match (a.source, b.source) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                a.generation == b.generation
                    && a.frame_id == b.frame_id
                    && a.duplicate == b.duplicate
                    && a.presented == b.presented
            }
            _ => false,
        }
}
/// Copy without draining, then remove only the exact accepted descriptors.
/// No queue guard crosses the send await. A busy cleanup lock leaves harmless
/// duplicates queued; accepting a retry never proves delivery of either copy.
async fn send_frames<F, W>(host: &Host, write: F) -> bool
where
    F: FnOnce(Packet) -> W,
    W: Future<Output = bool>,
{
    let frames = {
        let Ok(frames) = host.frames.try_lock() else {
            return false;
        };
        frames.iter().take(MAX_FRAMES).copied().collect::<Vec<_>>()
    };
    if frames.is_empty() {
        return false;
    }
    if !write(Packet::Frames {
        host_epoch: host.clock.epoch,
        frames: frames.clone(),
    })
    .await
    {
        return false;
    }
    if let Ok(mut pending) = host.frames.try_lock() {
        for accepted in &frames {
            if let Some(index) = pending
                .iter()
                .position(|queued| same_frame(queued, accepted))
            {
                pending.remove(index);
            }
        }
    }
    true
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
                    send_frames(&host, |packet| send(&channel, packet)).await;
                }
            });
        })
    }));
}
pub(crate) async fn viewer_channel(
    pc: &RTCPeerConnection,
    stats: Arc<crate::media::MediaStats>,
) -> anyhow::Result<Arc<RTCDataChannel>> {
    viewer_channel_observed(
        pc,
        stats,
        std::env::var("ECLIPSE_CONTINUOUS_TRACE").as_deref() == Ok("1"),
    )
    .await
}

#[derive(Serialize)]
struct ClockExchange {
    #[serde(with = "Stamp")]
    client_send: WireClockStamp,
    #[serde(with = "Stamp")]
    host_receive: WireClockStamp,
    #[serde(with = "Stamp")]
    host_send: WireClockStamp,
    #[serde(with = "Stamp")]
    client_receive: WireClockStamp,
}
impl From<FourTimestamp> for ClockExchange {
    fn from(sample: FourTimestamp) -> Self {
        Self {
            client_send: sample.client_send,
            host_receive: sample.host_receive,
            host_send: sample.host_send,
            client_receive: sample.client_receive,
        }
    }
}

async fn viewer_channel_observed(
    pc: &RTCPeerConnection,
    stats: Arc<crate::media::MediaStats>,
    trace: bool,
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
                            let sample = FourTimestamp {
                                client_send,
                                host_receive,
                                host_send,
                                client_receive,
                            };
                            if received_stats.video_e2e.clock_sample(epoch, sample) && trace {
                                received_stats.diagnostics.record(serde_json::json!({
                                    "event": "video_e2e_clock", "media_epoch": epoch,
                                    "sample": ClockExchange::from(sample),
                                }));
                            }
                        }
                    }
                }
                Some(Packet::Frames { host_epoch, frames }) => {
                    if received_stats
                        .video_e2e
                        .source_frames(epoch, host_epoch, &frames)
                        && trace
                    {
                        // Source batches include every available descriptor,
                        // unlike the older recent/slow-frame HostTrace subset.
                        // The existing bounded diagnostic sink may drop or
                        // rotate observations; absence never proves no frame.
                        received_stats.diagnostics.record(serde_json::json!({
                            "event": "video_e2e_sources", "media_epoch": epoch,
                            "host_epoch": host_epoch, "frames": frames,
                        }));
                    }
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
    use super::super::{Client, DisplayMethod, DisplayObservation, SourceEndpoint};
    use super::*;

    fn queued(host: &Host) -> Vec<SourceFrame> {
        host.frames.lock().unwrap().iter().copied().collect()
    }
    fn queue(host: &Host, n: u32) {
        host.sent(
            Some(SourceEndpoint {
                generation: 1,
                frame_id: u64::from(n),
                duplicate: false,
                presented: WireClockStamp {
                    epoch: host.clock.epoch,
                    ticks: u64::from(n) + 1,
                    frequency_hz: 1_000_000_000,
                },
            }),
            17,
            n,
        );
    }
    fn host() -> Host {
        let host = Host::default();
        host.enable(true);
        host
    }
    #[tokio::test]
    async fn refused_or_uncertain_batch_keeps_every_descriptor_queued() {
        let host = host();
        for n in 0..6 {
            queue(&host, n);
        }
        let before = serde_json::to_value(queued(&host)).unwrap();
        for _ in 0..3 {
            let accepted = send_frames(&host, |packet| {
                let Packet::Frames { frames, .. } = packet else {
                    panic!("not a frame batch");
                };
                assert_eq!(frames.len(), 6);
                assert_eq!(
                    serde_json::to_value(queued(&host)).unwrap(),
                    before,
                    "the batch must remain queued while the local send is unresolved"
                );
                async { false }
            })
            .await;
            assert!(!accepted);
            assert_eq!(serde_json::to_value(queued(&host)).unwrap(), before);
        }
    }
    #[tokio::test]
    async fn accepted_batch_removes_exact_items_and_keeps_concurrent_new_frames() {
        let host = host();
        queue(&host, 1);
        queue(&host, 2);
        let accepted = send_frames(&host, |packet| {
            let Packet::Frames { frames, .. } = packet else {
                panic!("not a frame batch");
            };
            assert_eq!(frames.len(), 2);
            assert_eq!(queued(&host).len(), 2);
            queue(&host, 3);
            let mut replacement = frames[0];
            replacement.source.as_mut().unwrap().generation += 1;
            host.sent(
                replacement.source,
                replacement.ssrc,
                replacement.rtp_timestamp,
            );
            async { true }
        })
        .await;
        assert!(accepted);
        let left = queued(&host);
        assert_eq!(left.len(), 2);
        assert_eq!(left[0].rtp_timestamp, 3);
        assert_eq!(left[1].rtp_timestamp, 1);
        assert_eq!(left[1].source.unwrap().generation, 2);
    }
    #[tokio::test]
    async fn accepted_cleanup_never_deletes_a_new_identity_reusing_ssrc_or_rtp() {
        for field in 0..9 {
            let host = host();
            queue(&host, 7);
            let mut replacement = queued(&host)[0];
            match field {
                0 => replacement.ssrc += 1,
                1 => replacement.rtp_timestamp += 1,
                2 => replacement.source.as_mut().unwrap().generation += 1,
                3 => replacement.source.as_mut().unwrap().frame_id += 1,
                4 => replacement.source.as_mut().unwrap().duplicate = true,
                5 => replacement.source.as_mut().unwrap().presented.epoch += 1,
                6 => replacement.source.as_mut().unwrap().presented.ticks += 1,
                7 => replacement.source.as_mut().unwrap().presented.frequency_hz += 1,
                _ => replacement.source = None,
            }
            assert!(
                send_frames(&host, |_| {
                    let mut pending = host.frames.lock().unwrap();
                    pending.clear();
                    pending.push_back(replacement);
                    async { true }
                })
                .await
            );
            assert_eq!(
                serde_json::to_value(queued(&host)).unwrap(),
                serde_json::to_value(vec![replacement]).unwrap(),
                "identity field {field}"
            );
        }
    }
    #[tokio::test]
    async fn bounded_overflow_during_send_does_not_remove_unrelated_new_tail() {
        let host = host();
        for n in 0..super::super::LIMIT as u32 {
            queue(&host, n);
        }
        assert!(
            send_frames(&host, |packet| {
                let Packet::Frames { frames, .. } = packet else {
                    panic!("not a frame batch");
                };
                assert_eq!(frames.len(), MAX_FRAMES);
                for n in super::super::LIMIT as u32..super::super::LIMIT as u32 + 10 {
                    queue(&host, n);
                }
                assert_eq!(queued(&host).len(), super::super::LIMIT);
                async { true }
            })
            .await
        );
        let left = queued(&host);
        assert_eq!(
            left.iter().map(|f| f.rtp_timestamp).collect::<Vec<_>>(),
            (MAX_FRAMES as u32..super::super::LIMIT as u32 + 10).collect::<Vec<_>>()
        );
        for n in 1000..2000 {
            queue(&host, n);
        }
        assert!(!send_frames(&host, |_| async { false }).await);
        assert_eq!(queued(&host).len(), super::super::LIMIT);
    }
    #[tokio::test]
    async fn busy_cleanup_retains_accepted_items_for_idempotent_retry() {
        let host = host();
        queue(&host, 1);
        let mut held = None;
        assert!(
            send_frames(&host, |_| {
                held = Some(host.frames.lock().unwrap());
                async { true }
            })
            .await
        );
        drop(held);
        assert_eq!(queued(&host).len(), 1);
        assert!(send_frames(&host, |_| async { true }).await);
        assert!(queued(&host).is_empty());
    }
    #[test]
    fn duplicate_source_metadata_does_not_duplicate_same_frame_latency() {
        let host = host();
        let client = Client::default();
        client.begin(7, true);
        assert!(client.set_display_observer(7, 1, DisplayMethod::PlatformDisplayed));
        let client_send = client.clock_stamp().unwrap();
        let source = host.clock.stamp().unwrap();
        let host_send = host.clock.stamp().unwrap();
        let client_receive = client.clock_stamp().unwrap();
        assert!(client.clock_sample(
            7,
            FourTimestamp {
                client_send,
                host_receive: source,
                host_send,
                client_receive
            }
        ));
        client.decode_input(7, 0, 17, 1, 100);
        host.sent(
            Some(SourceEndpoint {
                generation: 1,
                frame_id: 1,
                duplicate: false,
                presented: source,
            }),
            17,
            1,
        );
        let frames = queued(&host);
        assert!(client.source_frames(7, host.clock.epoch, &frames));
        let at = client.clock_stamp().unwrap();
        assert!(client.displayed(
            client.frame_for_pts(7, 0, 100).unwrap(),
            DisplayObservation {
                surface_epoch: 1,
                present_id: 1,
                earliest: at,
                latest: at,
                method: DisplayMethod::PlatformDisplayed
            }
        ));
        let original = serde_json::to_value(client.snapshot().measurement.unwrap()).unwrap();
        assert!(client.source_frames(7, host.clock.epoch, &frames));
        assert_eq!(
            serde_json::to_value(client.snapshot().measurement.unwrap()).unwrap(),
            original
        );
    }
    #[tokio::test]
    async fn channel_not_open_refuses_without_removing_the_batch() {
        let peer = crate::native_media::NativePeer::new("127.0.0.1".parse().unwrap(), None, false)
            .await
            .unwrap();
        let channel = peer.pc.create_data_channel(LABEL, None).await.unwrap();
        let host = host();
        queue(&host, 1);
        assert!(!send_frames(&host, |packet| send(&channel, packet)).await);
        assert_eq!(queued(&host).len(), 1);
        peer.close().await;
    }
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
        let diagnostics = stats.diagnostics.capture().unwrap();
        let _channel = viewer_channel_observed(&viewer.pc, stats.clone(), true)
            .await
            .unwrap();
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
            let mut events = Vec::new();
            loop {
                let mapped = {
                    let state = stats.video_e2e.state.lock().unwrap();
                    state.source_frames == 1
                        && state
                            .clock
                            .map_host(source, stats.video_e2e.clock_stamp().unwrap())
                            .is_some()
                };
                events.extend(diagnostics.try_iter());
                if mapped
                    && events
                        .iter()
                        .any(|event| event["event"] == "video_e2e_sources")
                    && events
                        .iter()
                        .any(|event| event["event"] == "video_e2e_clock")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let sources = events
                .iter()
                .find(|event| event["event"] == "video_e2e_sources")
                .unwrap();
            let frames: Vec<SourceFrame> =
                serde_json::from_value(sources["frames"].clone()).unwrap();
            assert_eq!(sources["media_epoch"], 7);
            assert_eq!(frames.len(), 1);
            assert_eq!(frames[0].ssrc, 42);
            assert_eq!(frames[0].source.unwrap().presented, source);
            // A local retry can deliver an identical descriptor twice. It must
            // stay the same source image and must not create a display endpoint.
            host.sent(frames[0].source, frames[0].ssrc, frames[0].rtp_timestamp);
            loop {
                events.extend(diagnostics.try_iter());
                if events
                    .iter()
                    .filter(|event| event["event"] == "video_e2e_sources")
                    .count()
                    >= 2
                    && host.frames.lock().unwrap().is_empty()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let exchange = events
                .iter()
                .find(|event| event["event"] == "video_e2e_clock")
                .unwrap();
            assert_eq!(exchange["sample"]["host_receive"]["epoch"], source.epoch);
            assert!(
                exchange["sample"]["client_receive"]["ticks"]
                    .as_u64()
                    .unwrap()
                    > 0
            );
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
