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
        Arc, Mutex,
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
    track::track_local::{track_local_static_sample::TrackLocalStaticSample,track_local_static_rtp::TrackLocalStaticRTP,TrackLocalWriter, TrackLocal},
    util::vnet::net::Net,
};

fn opus_capability(red:bool)->RTCRtpCodecCapability {
    RTCRtpCodecCapability {mime_type:if red{"audio/red"}else{"audio/opus"}.into(),clock_rate:48_000,channels:2,
        sdp_fmtp_line:if red{"111/111"}else{"minptime=10;useinbandfec=1;stereo=1;sprop-stereo=1"}.into(),..Default::default()}
}
/// Only an active receiving audio section can request system-output access.
pub(crate) fn offered_audio(description:&RTCSessionDescription)->Result<Option<bool>> {
    let sdp=description.unmarshal()?;
    for media in &sdp.media_descriptions {
        let direction=media.attributes.iter().chain(&sdp.attributes).find(|a|matches!(a.key.as_str(),"sendonly"|"recvonly"|"sendrecv"|"inactive"));
        if media.media_name.media!="audio" || media.media_name.port.value==0
            ||direction.is_some_and(|a|matches!(a.key.as_str(),"sendonly"|"inactive")){continue;}
        let codec=|pt:&str,name:&str|media.media_name.formats.iter().any(|f|f==pt)
            &&media.attributes.iter().any(|a|a.key=="rtpmap"&&a.value.as_ref().is_some_and(|v|v.eq_ignore_ascii_case(&format!("{pt} {name}/48000/2"))));
        if codec("111","opus") {return Ok(Some(codec("112","red")));}
    }
    Ok(None)
}
pub(crate) struct AudioSender {
    track:Arc<TrackLocalStaticRTP>,red:bool,sequence:u16,base:u32,previous:Option<(u32,bytes::Bytes)>,
}
impl AudioSender {
    pub async fn send(&mut self,frame:uu_media::audio_capture::EncodedAudio)->Result<usize> {
        let timestamp=self.base.wrapping_add(frame.sample_index as u32);
        let payload=if self.red {
            let previous=self.previous.as_ref().and_then(|(old,data)|{
                let offset=timestamp.wrapping_sub(*old);(offset>0&&offset<16384).then_some((offset as u16,data.as_ref()))
            });uu_media::audio_red::pack(111,&frame.data,previous)?
        }else{frame.data.clone()};
        ensure!(payload.len()<=uu_media::audio_red::MAX_PAYLOAD,"native audio exceeds datagram budget");
        let size=payload.len();
        let written=self.track.write_rtp(&webrtc::rtp::packet::Packet {header:webrtc::rtp::header::Header {
            version:2,payload_type:if self.red{112}else{111},sequence_number:self.sequence,timestamp,marker:self.previous.as_ref().map_or(true,|(old,_)|timestamp.wrapping_sub(*old)>frame.samples),..Default::default()
        },payload}).await?;
        ensure!(written>0,"audio track has no negotiated active binding");
        self.sequence=self.sequence.wrapping_add(1);self.previous=Some((timestamp,frame.data));Ok(size)
    }
}

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
    pending_decoder: Arc<Mutex<Option<Box<dyn uu_media::VideoDecoder>>>>,
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
        settings.set_ice_multicast_dns_mode(webrtc::ice::mdns::MulticastDnsMode::Disabled);
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
                capability: opus_capability(false), payload_type: 111, ..Default::default()
            }, RTPCodecType::Audio,
        )?;
        media.register_codec(webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
            capability:opus_capability(true),payload_type:112,..Default::default()
        },RTPCodecType::Audio)?;
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
            pending_decoder: Arc::new(Mutex::new(None)),
        })
    }
    pub async fn receiver(&self, frames: FrameSender, stats: Arc<MediaStats>) -> Result<()> {
        // Driver startup must finish before the peer can send video, otherwise
        // the bounded AU queue can overflow while the first decoder boots.
        // Hardware handles keep their COM/VA-API state on their own OS worker;
        // moving this Send handle does not move any driver objects.
        let decoder = tokio::task::spawn_blocking(uu_media::default_decoder)
            .await
            .context("native decoder initialization task failed")??;
        stats.set_backend(decoder.backend_name());
        *self.pending_decoder.lock().unwrap() = Some(decoder);
        let prepared = self.pending_decoder.clone();
        let audio_claimed=Arc::new(AtomicBool::new(false));
        let weak = Arc::downgrade(&self.pc);
        let repairs = self.repairs.clone();
        self.pc.on_track(Box::new(move |track, _, _| {
            let weak = weak.clone();
            let repairs = repairs.clone();
            let frames = frames.clone();
            let stats = stats.clone();
            let prepared = prepared.clone();
            let audio_claimed=audio_claimed.clone();
            Box::pin(async move {
                if track.kind() == RTPCodecType::Video {
                    if let Some(pc) = weak.upgrade() {
                        // Exactly one native video track owns the prepared
                        // decoder. Ignore duplicate/unexpected track callbacks.
                        let decoder = prepared.lock().unwrap().take();
                        let Some(decoder) = decoder else { return; };
                        tokio::spawn(crate::media::receive_video(
                            pc,
                            track,
                            0,
                            Some(frames),
                            Some(stats),
                            repairs,
                            Some(decoder),
                        ));
                    }
                } else if track.kind()==RTPCodecType::Audio && !audio_claimed.swap(true,Ordering::AcqRel) {
                    stats.capabilities.enable(crate::capabilities::Capability::Audio,true);
                    tokio::spawn(crate::media::receive_audio(track,Some(stats)));
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
        self.pc.add_transceiver_from_kind(RTPCodecType::Audio,Some(RTCRtpTransceiverInit {
            direction:RTCRtpTransceiverDirection::Recvonly,send_encodings:vec![],
        })).await?;
        Ok(())
    }
    pub async fn prepare_video(&self, width: u32, height: u32, stats: &MediaStats) -> Result<()> {
        let mut decoder = self.pending_decoder.lock().unwrap().take()
            .context("native video decoder was already assigned")?;
        let decoder = tokio::task::spawn_blocking(move || {
            decoder.prepare(width, height)?;
            Ok::<_, uu_media::CodecError>(decoder)
        }).await.context("native decoder preparation task failed")??;
        stats.set_backend(decoder.backend_name());
        *self.pending_decoder.lock().unwrap() = Some(decoder);
        Ok(())
    }
    pub async fn audio_sender(&self,red:bool)->Result<AudioSender> {
        let track=Arc::new(TrackLocalStaticRTP::new(opus_capability(red),"audio_0".into(),"eclipse-native".into()));
        let sender=self.pc.add_track(track.clone() as Arc<dyn TrackLocal+Send+Sync>).await?;
        tokio::spawn(async move{while sender.read_rtcp().await.is_ok(){}});
        Ok(AudioSender {track,red,sequence:rand::random(),base:rand::random(),previous:None})
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
        self.pending_decoder.lock().unwrap().take();
        if !self.closed.load(Ordering::Acquire) {
            let _ = self.pc.close().await;
            let _ = self.mux.close().await;
            self.closed.store(true, Ordering::Release);
        }
    }
}
impl Drop for NativePeer {
    fn drop(&mut self) {
        self.pending_decoder.lock().unwrap().take();
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
        let stats = Arc::new(MediaStats::default());
        peer.receiver(frames, stats.clone())
            .await
            .unwrap();
        assert!(!stats.backend.lock().unwrap().is_empty(),
            "decoder must be ready before producing the SDP offer");
        let offer = peer.description(true).await.unwrap();
        assert_eq!(
            offer
                .sdp
                .lines()
                .filter(|line| line.starts_with("m=video "))
                .count(),
            1
        );
        assert_eq!(offer.sdp.lines().filter(|line|line.starts_with("m=audio ")).count(),1);
        assert_eq!(offered_audio(&offer).unwrap(),Some(true));
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

#[cfg(test)]
mod srtp_rollover_regression {
    use webrtc::{rtp::{header::Header,packet::Packet},srtp::{context::Context,option::srtp_replay_protection,protection_profile::ProtectionProfile},util::marshal::Marshal};
    fn pair(profile:ProtectionProfile)->(Context,Context) {
        let key=[0x17;16];let salt=vec![0x2a;if matches!(profile,ProtectionProfile::Aes128CmHmacSha1_80) {14}else{12}];
        (Context::new(&key,&salt,profile,None,None).unwrap(),Context::new(&key,&salt,profile,Some(srtp_replay_protection(64)),None).unwrap())
    }
    fn packet(seq:u16)->bytes::Bytes {
        Packet {header:Header {version:2,payload_type:102,ssrc:42,sequence_number:seq,..Default::default()},
            payload:bytes::Bytes::from_static(b"generated SRTP regression"),..Default::default()}.marshal().unwrap()
    }
    #[test]
    fn late_retransmissions_keep_original_ciphertext_and_new_packets_decodable() {
        for profile in [ProtectionProfile::Aes128CmHmacSha1_80,ProtectionProfile::AeadAes128Gcm] {
            let (mut sender,mut receiver)=pair(profile);let mut cached=Vec::new();
            for seq in (65500..=65535).chain(0..=200) {
                let plain=packet(seq);let wire=sender.encrypt_rtp(&plain).unwrap();
                assert_eq!(receiver.decrypt_rtp(&wire).unwrap(),plain);
                if seq==65530||seq==40 {cached.push((seq,wire));}
            }
            for (seq,original) in cached {
                let wire=sender.encrypt_rtp(&packet(seq)).unwrap();assert_eq!(wire,original,"retransmission must retain its original packet index");
                assert!(receiver.decrypt_rtp(&wire).is_err(),"replay protection must remain active");
            }
            let plain=packet(201);let wire=sender.encrypt_rtp(&plain).unwrap();
            assert_eq!(receiver.decrypt_rtp(&wire).unwrap(),plain);
        }
    }
    #[test]
    fn loss_of_more_than_one_hundred_packets_across_rollover_recovers_with_authentication() {
        for profile in [ProtectionProfile::Aes128CmHmacSha1_80,ProtectionProfile::AeadAes128Gcm] {
            let (mut sender,mut receiver)=pair(profile);
            for seq in (65500..=65535).chain(0..=200) {
                let plain=packet(seq);let wire=sender.encrypt_rtp(&plain).unwrap();
                if seq>=65500 {assert_eq!(receiver.decrypt_rtp(&wire).unwrap(),plain);}
            }
            let plain=packet(201);let wire=sender.encrypt_rtp(&plain).unwrap();
            let mut tampered=wire.to_vec();*tampered.last_mut().unwrap()^=1;
            assert!(receiver.decrypt_rtp(&tampered).is_err(),"forged packets must not move receiver state");
            assert_eq!(receiver.decrypt_rtp(&wire).unwrap(),plain);
            assert!(receiver.decrypt_rtp(&wire).is_err(),"duplicate packets must still be rejected");
        }
    }
}
