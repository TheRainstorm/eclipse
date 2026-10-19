//! Native single-track WebRTC. Shares the receive pipeline, not UU control channels.
use crate::{
    media::{FrameSender, MediaStats},
    rtx::RepairBus,
};
use anyhow::{ensure, Context, Result};
use std::{
    net::{IpAddr, SocketAddr},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{net::UdpSocket, sync::watch, time::timeout};
use webrtc::{
    api::{
        interceptor_registry::register_default_interceptors, media_engine::MediaEngine,
        setting_engine::SettingEngine, APIBuilder,
    },
    ice::{
        network_type::NetworkType,
        udp_mux::{UDPMux, UDPMuxDefault, UDPMuxParams},
        udp_network::UDPNetwork,
    },
    ice_transport::ice_candidate_type::RTCIceCandidateType,
    interceptor::registry::Registry,
    peer_connection::{
        configuration::RTCConfiguration, peer_connection_state::RTCPeerConnectionState,
        sdp::session_description::RTCSessionDescription, RTCPeerConnection,
    },
    rtp_transceiver::{
        rtp_codec::{RTCRtpCodecCapability, RTPCodecType},
        rtp_transceiver_direction::RTCRtpTransceiverDirection,
        RTCRtpTransceiverInit,
    },
    track::track_local::{track_local_static_sample::TrackLocalStaticSample, TrackLocal},
    util::vnet::net::Net,
};

fn validate_candidates(sdp: &str, peer_ip: IpAddr) -> Result<()> {
    let mut candidates = 0usize;
    for line in sdp.lines().filter(|line| line.starts_with("a=candidate:")) {
        let words: Vec<_> = line.split_whitespace().collect();
        ensure!(
            words.len() >= 8
                && words[2].eq_ignore_ascii_case("udp")
                && words[6] == "typ"
                && words[7] == "host",
            "native LAN mode accepts UDP host candidates only"
        );
        let address: IpAddr = words[4]
            .parse()
            .context("native LAN candidate needs a numeric address")?;
        ensure!(
            address == peer_ip,
            "native LAN candidate must match the authenticated signaling peer"
        );
        candidates += 1;
    }
    ensure!(
        candidates > 0,
        "native LAN peer did not provide a media candidate"
    );
    Ok(())
}

fn h264_capability() -> RTCRtpCodecCapability {
    use webrtc::rtp_transceiver::RTCPFeedback;
    RTCRtpCodecCapability {
        mime_type: "video/H264".into(),
        clock_rate: 90_000,
        // Baseline receive capability through level 5.2 (including 4K60).
        sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=420034"
            .into(),
        rtcp_feedback: vec![
            RTCPFeedback {
                typ: "nack".into(),
                parameter: String::new(),
            },
            RTCPFeedback {
                typ: "nack".into(),
                parameter: "pli".into(),
            },
            RTCPFeedback {
                typ: "ccm".into(),
                parameter: "fir".into(),
            },
        ],
        ..Default::default()
    }
}

pub(crate) struct NativePeer {
    pub pc: Arc<RTCPeerConnection>,
    mux: Arc<UDPMuxDefault>,
    pub repairs: Arc<RepairBus>,
    pub states: watch::Receiver<RTCPeerConnectionState>,
    closed: AtomicBool,
}
impl NativePeer {
    pub async fn new(
        local_ip: IpAddr,
        stats: Option<Arc<MediaStats>>,
        sender: bool,
    ) -> Result<Self> {
        crate::install_crypto_provider();
        ensure!(
            !local_ip.is_unspecified() && !local_ip.is_multicast(),
            "native media requires a concrete local address"
        );
        // One socket, explicitly bound to the authenticated signaling interface.
        // The older ICE library excludes loopback while enumerating interfaces;
        // its supported UDP mux + 1:1 mapping keeps localhost tests on loopback.
        let gather_ip = if local_ip.is_loopback() {
            Net::new(None)
                .get_interfaces()
                .await
                .into_iter()
                .flat_map(|i| i.addrs().to_vec())
                .map(|a| a.addr())
                .find(|a| !a.is_loopback() && a.is_ipv4() == local_ip.is_ipv4())
                .context(
                    "native localhost media needs an active network interface for ICE enumeration",
                )?
        } else {
            local_ip
        };
        let socket = UdpSocket::bind(SocketAddr::new(local_ip, 0)).await?;
        let mux = UDPMuxDefault::new(UDPMuxParams::new(socket));
        let mut settings = SettingEngine::default();
        settings.set_network_types(vec![if local_ip.is_ipv4() {
            NetworkType::Udp4
        } else {
            NetworkType::Udp6
        }]);
        settings.set_ip_filter(Box::new(move |ip| ip == gather_ip));
        settings.set_nat_1to1_ips(vec![local_ip.to_string()], RTCIceCandidateType::Host);
        settings.set_udp_network(UDPNetwork::Muxed(mux.clone()));
        let mut media = MediaEngine::default();
        media.register_codec(
            webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
                capability: h264_capability(),
                payload_type: 102,
                ..Default::default()
            },
            RTPCodecType::Video,
        )?;
        media.register_codec(
            webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: "video/rtx".into(),
                    clock_rate: 90_000,
                    sdp_fmtp_line: "apt=102".into(),
                    ..Default::default()
                },
                payload_type: 103,
                ..Default::default()
            },
            RTPCodecType::Video,
        )?;
        let repairs = RepairBus::new(stats);
        let mut registry = if sender {
            register_default_interceptors(Registry::new(), &mut media)?
        } else {
            let registry =
                webrtc::api::interceptor_registry::configure_rtcp_reports(Registry::new());
            webrtc::api::interceptor_registry::configure_twcc_receiver_only(registry, &mut media)?
        };
        registry.add(Box::new(crate::rtx::RepairTap(repairs.clone())));
        let api = APIBuilder::new()
            .with_media_engine(media)
            .with_setting_engine(settings)
            .with_interceptor_registry(registry)
            .build();
        let pc = match api.new_peer_connection(RTCConfiguration::default()).await {
            Ok(pc) => Arc::new(pc),
            Err(e) => {
                let _ = mux.close().await;
                return Err(e.into());
            }
        };
        let (tx, states) = watch::channel(RTCPeerConnectionState::New);
        pc.on_peer_connection_state_change(Box::new(move |state| {
            let _ = tx.send(state);
            Box::pin(async {})
        }));
        Ok(Self {
            pc,
            mux,
            repairs,
            states,
            closed: AtomicBool::new(false),
        })
    }
    pub async fn receiver(&self, frames: FrameSender, stats: Arc<MediaStats>) -> Result<()> {
        let weak = Arc::downgrade(&self.pc);
        let repairs = self.repairs.clone();
        self.pc.on_track(Box::new(move |track, _, _| {
            let weak = weak.clone();
            let repairs = repairs.clone();
            let frames = frames.clone();
            let stats = stats.clone();
            Box::pin(async move {
                if track.kind() == RTPCodecType::Video {
                    if let Some(pc) = weak.upgrade() {
                        tokio::spawn(crate::media::receive_video(
                            pc,
                            track,
                            0,
                            Some(frames),
                            Some(stats),
                            repairs,
                        ));
                    }
                }
            })
        }));
        self.pc
            .add_transceiver_from_kind(
                RTPCodecType::Video,
                Some(RTCRtpTransceiverInit {
                    direction: RTCRtpTransceiverDirection::Recvonly,
                    send_encodings: vec![],
                }),
            )
            .await?;
        Ok(())
    }
    pub async fn sender(&self, refresh: Arc<AtomicBool>) -> Result<Arc<TrackLocalStaticSample>> {
        let track = Arc::new(TrackLocalStaticSample::new(
            h264_capability(),
            "video_0".into(),
            "eclipse-native".into(),
        ));
        let sender = self
            .pc
            .add_track(track.clone() as Arc<dyn TrackLocal + Send + Sync>)
            .await?;
        tokio::spawn(async move {
            use webrtc::rtcp::payload_feedbacks::{
                full_intra_request::FullIntraRequest,
                picture_loss_indication::PictureLossIndication,
            };
            while let Ok((packets, _)) = sender.read_rtcp().await {
                if packets.iter().any(|p| {
                    p.as_any().is::<PictureLossIndication>() || p.as_any().is::<FullIntraRequest>()
                }) {
                    refresh.store(true, Ordering::Release);
                }
            }
        });
        Ok(track)
    }
    pub async fn description(&self, offer: bool) -> Result<RTCSessionDescription> {
        let description = if offer {
            self.pc.create_offer(None).await?
        } else {
            self.pc.create_answer(None).await?
        };
        let mut gathered = self.pc.gathering_complete_promise().await;
        self.pc.set_local_description(description).await?;
        timeout(Duration::from_secs(10), gathered.recv())
            .await
            .context("native ICE gathering timed out")?;
        self.pc
            .local_description()
            .await
            .context("native local SDP unavailable")
    }
    pub async fn remote(&self, description: RTCSessionDescription, peer_ip: IpAddr) -> Result<()> {
        validate_candidates(&description.sdp, peer_ip)?;
        self.repairs.configure(&description.sdp);
        self.pc.set_remote_description(description).await?;
        Ok(())
    }
    pub async fn connected(&mut self) -> Result<()> {
        timeout(Duration::from_secs(15), async {
            loop {
                let state = *self.states.borrow_and_update();
                match state {
                    RTCPeerConnectionState::Connected => return Ok(()),
                    RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => {
                        anyhow::bail!("native WebRTC connection failed")
                    }
                    _ => self
                        .states
                        .changed()
                        .await
                        .context("native connection state closed")?,
                }
            }
        })
        .await
        .context("native WebRTC connection timed out")?
    }
    pub async fn close(&self) {
        if !self.closed.load(Ordering::Acquire) {
            let _ = self.pc.close().await;
            let _ = self.mux.close().await;
            self.closed.store(true, Ordering::Release);
        }
    }
}
impl Drop for NativePeer {
    fn drop(&mut self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let pc = self.pc.clone();
                let mux = self.mux.clone();
                runtime.spawn(async move {
                    let _ = pc.close().await;
                    let _ = mux.close().await;
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_candidates_cannot_redirect_a_paired_session_to_another_host() {
        let peer = "127.0.0.1".parse().unwrap();
        let prefix = "a=candidate:1 1 udp 1 ";
        assert!(
            validate_candidates(&format!("{prefix}127.0.0.1 12345 typ host\r\n"), peer).is_ok()
        );
        for candidate in [
            "127.0.0.2 12345 typ host",
            "localhost 12345 typ host",
            "127.0.0.1 12345 typ relay",
        ] {
            assert!(validate_candidates(&format!("{prefix}{candidate}\r\n"), peer).is_err());
        }
        assert!(validate_candidates("v=0\r\n", peer).is_err());
    }

    #[tokio::test]
    async fn localhost_media_advertises_only_loopback_and_releases_its_udp_port() {
        let peer = NativePeer::new("127.0.0.1".parse().unwrap(), None, false)
            .await
            .unwrap();
        let (frames, _) = crate::media::frame_channel();
        peer.receiver(frames, Arc::new(MediaStats::default()))
            .await
            .unwrap();
        let offer = peer.description(true).await.unwrap();
        assert_eq!(
            offer
                .sdp
                .lines()
                .filter(|line| line.starts_with("m=video "))
                .count(),
            1
        );
        assert!(!offer.sdp.lines().any(|line| line.starts_with("m=audio ")));
        assert!(offer.sdp.contains("profile-level-id=420034"));
        let candidates: Vec<_> = offer
            .sdp
            .lines()
            .filter(|line| line.starts_with("a=candidate:"))
            .collect();
        assert!(!candidates.is_empty());
        assert!(
            candidates
                .iter()
                .all(|line| line.split_whitespace().nth(4) == Some("127.0.0.1")),
            "localhost media leaked a different interface"
        );
        let words: Vec<_> = candidates[0].split_whitespace().collect();
        assert!(
            words.get(4) == Some(&"127.0.0.1"),
            "localhost media leaked a different interface"
        );
        let port: u16 = words[5].parse().unwrap();
        peer.close().await;
        drop(peer);
        timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(socket) = UdpSocket::bind(("127.0.0.1", port)).await {
                    drop(socket);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("native UDP socket remained open after shutdown");
    }
}
