//! Native single-track WebRTC. Shares the receive pipeline, not UU control channels.
use crate::{
    media::{FrameSender, MediaStats},
    rtx::RepairBus,
};
use anyhow::{ensure, Context, Result};
use uu_proto::media::VideoCodec;
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
    track::track_local::{track_local_static_rtp::TrackLocalStaticRTP,TrackLocalWriter, TrackLocal},
    util::vnet::net::Net,
};

// ICMP port-unreachable replies during ICE probing are not a teardown of this
// connectionless socket. Otherwise Windows reports 10054 to the mux, whose
// reader exits before authenticated peer-reflexive checks can arrive.
#[cfg(windows)]
fn configure_udp_probing(socket: &UdpSocket) -> std::io::Result<()> {
    use std::{os::windows::io::AsRawSocket, ptr::{null_mut}, mem::size_of};
    use windows_sys::Win32::Networking::WinSock::{WSAIoctl, WSAGetLastError, SIO_UDP_CONNRESET, SOCKET_ERROR};
    let report_reset: u32 = 0;
    let mut returned = 0;
    let result = unsafe { WSAIoctl(socket.as_raw_socket() as _, SIO_UDP_CONNRESET,
        (&report_reset as *const u32).cast(), size_of::<u32>() as u32,
        null_mut(), 0, &mut returned, null_mut(), None) };
    if result == SOCKET_ERROR { return Err(std::io::Error::from_raw_os_error(unsafe { WSAGetLastError() })); }
    Ok(())
}

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

fn video_capability(codec:VideoCodec)->RTCRtpCodecCapability {
    let mut capability=h264_capability();
    match codec {
        VideoCodec::H264=>{},
        VideoCodec::Hevc=>{capability.mime_type="video/H265".into();capability.sdp_fmtp_line="profile-id=1;tier-flag=1;level-id=156;sprop-max-don-diff=0".into();},
        VideoCodec::Av1=>{capability.mime_type="video/AV1".into();capability.sdp_fmtp_line="profile=0;level-idx=14;tier=1".into();},
    }capability
}
fn video_color_capability(codec:VideoCodec,hdr:bool)->RTCRtpCodecCapability {
    let mut capability=video_capability(codec);
    if hdr && codec==VideoCodec::Hevc {capability.sdp_fmtp_line=capability.sdp_fmtp_line.replace("profile-id=1;","profile-id=2;");}
    capability
}
pub(crate) fn offers_codec(description:&RTCSessionDescription,codec:VideoCodec)->bool{
    let name=match codec {VideoCodec::H264=>"H264/90000",VideoCodec::Hevc=>"H265/90000",VideoCodec::Av1=>"AV1/90000"};
    let mut video=false;let mut payloads=Vec::new();
    for line in description.sdp.lines().map(str::trim){
        if let Some(media)=line.strip_prefix("m="){
            video=media.starts_with("video ");payloads.clear();
            if video {payloads.extend(media.split_whitespace().skip(3).filter_map(|v|v.parse::<u8>().ok()));}
        }
        if video {if let Some(value)=line.strip_prefix("a=rtpmap:"){
            if let Some((pt,format))=value.split_once(' ') {
                if format.eq_ignore_ascii_case(name)&&pt.parse::<u8>().ok().is_some_and(|pt|payloads.contains(&pt)){return true;}
            }
        }}
    }false
}
/// A 10-bit claim must also match the negotiated RTP profile.
pub(crate) fn offers_hdr_codec(description:&RTCSessionDescription,codec:VideoCodec)->bool {
    let (mime,name,profile)=match codec {VideoCodec::Hevc=>("H265/90000","profile-id","2"),VideoCodec::Av1=>("AV1/90000","profile","0"),VideoCodec::H264=>return false};
    let Ok(sdp)=description.unmarshal() else{return false};
    sdp.media_descriptions.iter().filter(|m|m.media_name.media=="video"&&m.media_name.port.value!=0).any(|media| {
        media.media_name.formats.iter().any(|pt| {
            let mapped=media.attributes.iter().any(|a|a.key=="rtpmap"&&a.value.as_ref().is_some_and(|v|v.split_once(' ').is_some_and(|(p,m)|p==pt&&m.eq_ignore_ascii_case(mime))));
            mapped&&media.attributes.iter().any(|a|a.key=="fmtp"&&a.value.as_ref().is_some_and(|v|v.split_once(' ').is_some_and(|(p,params)|p==pt&&params.split(';').any(|v|v.trim().split_once('=').is_some_and(|(n,v)|n.eq_ignore_ascii_case(name)&&v.trim()==profile)))))
        })
    })
}
pub(crate) async fn receive_codecs(stats:&MediaStats)->Vec<VideoCodec>{
    if let Some(codecs)=stats.native_codec_capabilities.get(){return codecs.clone();}
    let mut codecs=vec![VideoCodec::H264];
    let kind=stats.decoder_kind();
    // Each probe owns its decoder/device. Prepare the independent formats in
    // parallel so unavailable drivers cannot add two five-second waits before
    // the first offer. Only formats producing valid images are advertised.
    let probe=|codec|async move {
        let started=std::time::Instant::now();
        let result=timeout(Duration::from_secs(5),tokio::task::spawn_blocking(move||match codec {
            VideoCodec::Hevc=>uu_media::hevc::probe_decoder_with_kind(kind),VideoCodec::Av1=>uu_media::av1::probe_decoder_with_kind(kind),VideoCodec::H264=>unreachable!()
        })).await;
        let (available,reason)=match result {
            Ok(Ok(Ok(())))=>(true,String::new()),
            Ok(Ok(Err(error)))=>(false,error.to_string().chars().take(384).collect()),
            Ok(Err(_))=>(false,"decoder probe worker failed".into()),
            Err(_)=>(false,"decoder probe exceeded 5 seconds".into()),
        };
        stats.diagnostics.record(serde_json::json!({"event":"native_decoder_probe","codec":codec,"available":available,"reason":reason,"elapsed_us":started.elapsed().as_micros().min(u64::MAX as u128) as u64}));
        (codec,available)
    };
    let (hevc,av1)=tokio::join!(probe(VideoCodec::Hevc),probe(VideoCodec::Av1));
    for (codec,available) in [hevc,av1] {if available {codecs.push(codec);}}
    let _=stats.native_codec_capabilities.set(codecs.clone());codecs
}

pub(crate) struct NativePeer {
    _connection_monitor:Option<crate::connection_info::Monitor>,
    pub pc: Arc<RTCPeerConnection>,
    mux: Option<Arc<UDPMuxDefault>>,
    pub repairs: Arc<RepairBus>,
    fec: Arc<crate::native_fec::Transport>,
    pub states: watch::Receiver<RTCPeerConnectionState>,
    closed: AtomicBool,
    development_relay: bool,
    pending_decoder: Arc<Mutex<Option<crate::media::PreparedVideoDecoder>>>,
    wlan: Mutex<Option<crate::wlan_streaming::Guard>>,
}
impl NativePeer {
    pub async fn new(
        local_ip: IpAddr,
        stats: Option<Arc<MediaStats>>,
        sender: bool,
    ) -> Result<Self> {
        Self::new_with_advertised(local_ip, local_ip, stats, sender).await
    }
    /// The optional address comes from the authenticated broker's observation
    /// of this client's TCP connection. UDP stays bound to the local interface;
    /// remote candidates still must match the authenticated signaling peer.
    pub async fn new_with_advertised(
        local_ip: IpAddr,
        advertised_ip: IpAddr,
        stats: Option<Arc<MediaStats>>,
        sender: bool,
    ) -> Result<Self> {
        crate::install_crypto_provider();
        ensure!(
            !local_ip.is_unspecified() && !local_ip.is_multicast(),
            "native media requires a concrete local address"
        );
        ensure!(!advertised_ip.is_unspecified() && !advertised_ip.is_multicast()
            && local_ip.is_ipv4() == advertised_ip.is_ipv4(), "invalid native advertised address");
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
        #[cfg(windows)]
        configure_udp_probing(&socket).context("cannot configure native UDP probing")?;
        let mux = UDPMuxDefault::new(UDPMuxParams::new(socket));
        let mut settings = SettingEngine::default();
        crate::srtp_policy::configure(&mut settings);
        settings.set_ice_multicast_dns_mode(webrtc::ice::mdns::MulticastDnsMode::Disabled);
        settings.set_network_types(vec![if local_ip.is_ipv4() {
            NetworkType::Udp4
        } else {
            NetworkType::Udp6
        }]);
        settings.set_ip_filter(Box::new(move |ip| ip == gather_ip));
        settings.set_nat_1to1_ips(vec![advertised_ip.to_string()], RTCIceCandidateType::Host);
        settings.set_udp_network(UDPNetwork::Muxed(mux.clone()));
        Self::build(settings, Some(mux), RTCConfiguration::default(), stats, sender).await
    }

    /// Internet mode enumerates interfaces and obtains mapped addresses using
    /// STUN. Unlike LAN mode it does not pin media to a signaling server's IP.
    pub async fn new_direct(ice: &crate::direct_ice::InternetIce, stats: Option<Arc<MediaStats>>, sender: bool) -> Result<Self> {
        crate::install_crypto_provider();
        let mut settings = SettingEngine::default();
        crate::srtp_policy::configure(&mut settings);
        settings.set_ice_multicast_dns_mode(webrtc::ice::mdns::MulticastDnsMode::Disabled);
        settings.set_network_types(vec![NetworkType::Udp4, NetworkType::Udp6]);
        let mut peer=Self::build(settings, None, ice.configuration(), stats, sender).await?;
        peer.development_relay=ice.development_relay();
        Ok(peer)
    }

    async fn build(settings: SettingEngine, mux: Option<Arc<UDPMuxDefault>>, configuration: RTCConfiguration,
        stats: Option<Arc<MediaStats>>, sender: bool) -> Result<Self> {
        let mut media = MediaEngine::default();
        let codecs=match &stats {Some(stats) if !sender=>receive_codecs(stats).await,_=>vec![VideoCodec::H264,VideoCodec::Hevc,VideoCodec::Av1]};
        for codec in codecs {
            let pt=match codec {VideoCodec::H264=>102,VideoCodec::Hevc=>104,VideoCodec::Av1=>106};
            media.register_codec(webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
                capability:video_capability(codec),payload_type:pt,..Default::default()
            },RTPCodecType::Video)?;
            media.register_codec(webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
                capability:RTCRtpCodecCapability {mime_type:"video/rtx".into(),clock_rate:90_000,sdp_fmtp_line:format!("apt={pt}"),..Default::default()},payload_type:pt+1,..Default::default()
            },RTPCodecType::Video)?;
        }
        let hdr_codecs=if sender {vec![VideoCodec::Hevc,VideoCodec::Av1]}else if let Some(stats)=&stats {
            if stats.native_hdr_capable.load(Ordering::Acquire) {
                if let Some(cached)=stats.native_hdr_codecs.get(){cached.clone()}else{
                    let probe=|codec|async move {
                        let kind=stats.decoder_kind();
                        let result=timeout(Duration::from_secs(5),tokio::task::spawn_blocking(move||uu_media::hdr::probe_decoder(codec,kind))).await;
                        let available=matches!(result,Ok(Ok(Ok(()))));
                        stats.diagnostics.record(serde_json::json!({"event":"native_hdr_decoder_probe","codec":codec,"available":available}));
                        (codec,available)
                    };
                    let (hevc,av1)=tokio::join!(probe(VideoCodec::Hevc),probe(VideoCodec::Av1));
                    let codecs=vec![hevc,av1].into_iter().filter_map(|(c,ok)|ok.then_some(c)).collect::<Vec<_>>();
                    let _=stats.native_hdr_codecs.set(codecs.clone());codecs
                }
            }else{Vec::new()}
        }else{Vec::new()};
        if hdr_codecs.contains(&VideoCodec::Hevc) {
            media.register_codec(webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters{capability:video_color_capability(VideoCodec::Hevc,true),payload_type:108,..Default::default()},RTPCodecType::Video)?;
            media.register_codec(webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters{capability:RTCRtpCodecCapability{mime_type:"video/rtx".into(),clock_rate:90_000,sdp_fmtp_line:"apt=108".into(),..Default::default()},payload_type:109,..Default::default()},RTPCodecType::Video)?;
        }
        media.register_codec(
            webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
                capability: opus_capability(false), payload_type: 111, ..Default::default()
            }, RTPCodecType::Audio,
        )?;
        media.register_codec(webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters {
            capability:opus_capability(true),payload_type:112,..Default::default()
        },RTPCodecType::Audio)?;
        let repairs = RepairBus::new(stats.clone());
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
        // Each connection owns an independent request. Opening is a low-rate
        // blocking boundary; cancellation drops the unclaimed task result.
        let wlan=if let Some(requested)=crate::wlan_streaming::experiment_request() {
            tokio::task::spawn_blocking(move||crate::wlan_streaming::Guard::acquire(uuid::Uuid::new_v4().to_string(),requested)).await.ok()
        }else{None};
        let pc = match api.new_peer_connection(configuration).await {
            Ok(pc) => Arc::new(pc),
            Err(e) => {
                if let Some(mux) = &mux { let _ = mux.close().await; }
                return Err(e.into());
            }
        };
        let (tx, states) = watch::channel(RTCPeerConnectionState::New);
        pc.on_peer_connection_state_change(Box::new(move |state| {
            let _ = tx.send(state);
            Box::pin(async {})
        }));
        Ok(Self {
            _connection_monitor:stats.as_ref().map(|stats|crate::connection_info::Monitor::start(&pc,stats)),
            pc,
            mux,
            repairs,
            fec: crate::native_fec::Transport::new(sender, stats.clone()),
            states,
            closed: AtomicBool::new(false),
            development_relay:false,
            pending_decoder: Arc::new(Mutex::new(None)),
            wlan: Mutex::new(wlan),
        })
    }
    pub async fn receiver(&self, frames: FrameSender, stats: Arc<MediaStats>) -> Result<()> {
        self.fec.receiver(&self.pc).await?;
        // Driver startup must finish before the peer can send video, otherwise
        // the bounded AU queue can overflow while the first decoder boots.
        // Hardware handles keep their COM/VA-API state on their own OS worker;
        // moving this Send handle does not move any driver objects.
        let kind=stats.decoder_kind();
        let decoder = tokio::task::spawn_blocking(move ||uu_media::default_decoder_with_kind(kind))
            .await
            .context("native decoder initialization task failed")??;
        stats.set_backend(decoder.backend_name());
        *self.pending_decoder.lock().unwrap() = Some(crate::media::PreparedVideoDecoder{decoder,visible:None});
        let prepared = self.pending_decoder.clone();
        let audio_claimed=Arc::new(AtomicBool::new(false));
        let weak = Arc::downgrade(&self.pc);
        let repairs = self.repairs.clone();
        let fec = self.fec.receive.clone();
        self.pc.on_track(Box::new(move |track, _, _| {
            let weak = weak.clone();
            let repairs = repairs.clone();
            let fec = fec.clone();
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
                            Some(fec),
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
    pub async fn prepare_video(&self, width: u32, height: u32, codec:VideoCodec,stats: &MediaStats) -> Result<()> {
        let prepared = self.pending_decoder.lock().unwrap().take()
            .context("native video decoder was already assigned")?;
        let mut decoder=prepared.decoder;
        let decoder = tokio::task::spawn_blocking(move || {
            if decoder.codec()!=codec{decoder=uu_media::decoder_for_codec(codec)?;}
            decoder.prepare(width, height)?;
            Ok::<_, uu_media::CodecError>(decoder)
        }).await.context("native decoder preparation task failed")??;
        stats.set_backend(decoder.backend_name());
        *self.pending_decoder.lock().unwrap() = Some(crate::media::PreparedVideoDecoder{decoder,visible:Some([width,height])});
        Ok(())
    }
    /// Reuse the offered media section when encoder selection follows consent.
    /// webrtc-rs 0.11 add_track only reuses a sender with an identical generated
    /// track id; calling it here would append an unoffered m= section.
    async fn attach_sender(&self,track:Arc<dyn TrackLocal+Send+Sync>)->Result<Arc<webrtc::rtp_transceiver::rtp_sender::RTCRtpSender>> {
        if self.pc.remote_description().await.is_some() {
            for transceiver in self.pc.get_transceivers().await {
                if transceiver.kind()==track.kind() && transceiver.mid().is_some() {
                    let sender=transceiver.sender().await;
                    if sender.track().await.is_none() {
                        sender.replace_track(Some(track)).await?;
                        transceiver.set_direction(RTCRtpTransceiverDirection::Sendonly).await;
                        return Ok(sender);
                    }
                }
            }
            anyhow::bail!("控制端没有可用的媒体接收通道");
        }
        Ok(self.pc.add_track(track).await?)
    }
    pub async fn audio_sender(&self,red:bool)->Result<AudioSender> {
        let track=Arc::new(TrackLocalStaticRTP::new(opus_capability(red),"audio_0".into(),"eclipse-native".into()));
        let sender=self.attach_sender(track.clone() as Arc<dyn TrackLocal+Send+Sync>).await?;
        tokio::spawn(async move{while sender.read_rtcp().await.is_ok(){}});
        Ok(AudioSender {track,red,sequence:rand::random(),base:rand::random(),previous:None})
    }
    pub async fn sender_for_color(&self,refresh:Arc<AtomicBool>,codec:VideoCodec,hdr:bool)->Result<crate::native_video::Sender>{
        let track=Arc::new(TrackLocalStaticRTP::new(video_color_capability(codec,hdr),"video_0".into(),"eclipse-native".into()));
        let sender = self.attach_sender(track.clone() as Arc<dyn TrackLocal + Send + Sync>).await?;
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
        Ok(crate::native_video::Sender::with_fec(track,codec,self.fec.clone()))
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
        let mut description = self.pc
            .local_description()
            .await
            .context("native local SDP unavailable")?;
        self.fec.description(&mut description);
        Ok(description)
    }
    pub async fn remote(&self, description: RTCSessionDescription, peer_ip: IpAddr) -> Result<()> {
        validate_candidates(&description.sdp, peer_ip)?;
        self.repairs.configure(&description.sdp);
        self.fec.remote(&self.pc, &description).await?;
        self.pc.set_remote_description(description).await?;
        Ok(())
    }
    pub async fn remote_direct(&self, description: RTCSessionDescription) -> Result<()> {
        crate::direct_ice::validate_sdp(&description.sdp,self.development_relay)?;
        self.repairs.configure(&description.sdp);
        self.fec.remote(&self.pc, &description).await?;
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
    pub async fn direct_path(&self) -> Result<serde_json::Value> {
        let pair=self.pc.sctp().transport().ice_transport().get_selected_candidate_pair().await
            .context("cannot confirm selected native ICE path")?;
        let local_type=pair.local().typ.to_string();let remote_type=pair.remote().typ.to_string();
        ensure!(self.development_relay || (pair.local().typ!=RTCIceCandidateType::Relay && pair.remote().typ!=RTCIceCandidateType::Relay),"direct-only session selected a forbidden relay");
        tracing::info!(local_type,remote_type,"native UDP path selected");
        Ok(serde_json::json!({"event":"native_path","direct":pair.local().typ!=RTCIceCandidateType::Relay&&pair.remote().typ!=RTCIceCandidateType::Relay,
            "development_relay":self.development_relay,"local_type":local_type,"remote_type":remote_type,"transport":"udp"}))
    }
    pub async fn close(&self) {
        self.fec.stop();
        self.pending_decoder.lock().unwrap().take();
        if !self.closed.load(Ordering::Acquire) {
            let _ = self.pc.close().await;
            if let Some(mux) = &self.mux { let _ = mux.close().await; }
            self.closed.store(true, Ordering::Release);
        }
        let wlan=self.wlan.lock().unwrap().take();
        if let Some(wlan)=wlan {let _=tokio::task::spawn_blocking(move||drop(wlan)).await;}
    }
}
impl Drop for NativePeer {
    fn drop(&mut self) {
        self.fec.stop();
        self.pending_decoder.lock().unwrap().take();
        if let Some(wlan)=self.wlan.get_mut().unwrap().take() {
            if let Ok(runtime)=tokio::runtime::Handle::try_current(){runtime.spawn_blocking(move||drop(wlan));}
            else {drop(wlan);}
        }
        if !self.closed.swap(true, Ordering::AcqRel) {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let pc = self.pc.clone();
                let mux = self.mux.clone();
                runtime.spawn(async move {
                    let _ = pc.close().await;
                    if let Some(mux) = mux { let _ = mux.close().await; }
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn format_support_requires_a_video_payload_in_the_offer(){
        let description=|sdp:&str|serde_json::from_value(serde_json::json!({"type":"offer","sdp":sdp})).unwrap();
        let legacy=description("m=video 9 UDP/TLS/RTP/SAVPF 102\r\na=rtpmap:102 H264/90000\r\na=rtpmap:106 AV1/90000\r\nm=audio 9 UDP/TLS/RTP/SAVPF 104\r\na=rtpmap:104 H265/90000\r\n");
        assert!(offers_codec(&legacy,VideoCodec::H264));assert!(!offers_codec(&legacy,VideoCodec::Av1));assert!(!offers_codec(&legacy,VideoCodec::Hevc));
        let extended=description("m=video 9 UDP/TLS/RTP/SAVPF 104 106\r\na=rtpmap:104 H265/90000\r\na=rtpmap:106 AV1/90000\r\n");
        assert!(offers_codec(&extended,VideoCodec::Hevc));assert!(offers_codec(&extended,VideoCodec::Av1));
    }
    #[test]
    fn hdr_profile_must_belong_to_an_active_video_payload() {
        let base="v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\n";
        for (media,expected) in [
            ("m=video 9 UDP/TLS/RTP/SAVPF 108\r\na=rtpmap:108 H265/90000\r\na=fmtp:108 profile-id=2;tier-flag=1\r\n",true),
            ("m=video 9 UDP/TLS/RTP/SAVPF 104\r\na=rtpmap:104 H265/90000\r\na=fmtp:104 profile-id=1\r\n",false),
            ("m=video 0 UDP/TLS/RTP/SAVPF 108\r\na=rtpmap:108 H265/90000\r\na=fmtp:108 profile-id=2\r\n",false),
            ("m=video 9 UDP/TLS/RTP/SAVPF 104\r\na=rtpmap:108 H265/90000\r\na=fmtp:108 profile-id=2\r\n",false),
        ] {
            let sdp=RTCSessionDescription::offer(format!("{base}{media}")).unwrap();
            assert_eq!(offers_hdr_codec(&sdp,VideoCodec::Hevc),expected);
        }
    }
    #[tokio::test]
    async fn hdr_main10_profile_is_used_on_the_actual_encrypted_rtp_track() {
        let local="127.0.0.1".parse().unwrap();let stats=Arc::new(MediaStats::default());
        stats.native_codec_capabilities.set(vec![VideoCodec::Hevc]).unwrap();
        stats.native_hdr_capable.store(true,Ordering::Release);
        stats.native_hdr_codecs.set(vec![VideoCodec::Hevc]).unwrap();
        let mut viewer=NativePeer::new(local,Some(stats),false).await.unwrap();
        let mut host=NativePeer::new(local,None,true).await.unwrap();
        let(tx,mut rx)=tokio::sync::mpsc::channel(1);
        viewer.pc.on_track(Box::new(move|track,_,_|{let tx=tx.clone();Box::pin(async move{
            let codec=track.codec();let packet=track.read_rtp().await.unwrap().0;let _=tx.send((codec,packet)).await;
        })}));
        viewer.pc.add_transceiver_from_kind(RTPCodecType::Video,Some(RTCRtpTransceiverInit{direction:RTCRtpTransceiverDirection::Recvonly,send_encodings:vec![]})).await.unwrap();
        let offer=viewer.description(true).await.unwrap();assert!(offers_hdr_codec(&offer,VideoCodec::Hevc));
        host.remote(offer,local).await.unwrap();
        let mut sender=host.sender_for_color(Arc::new(AtomicBool::new(false)),VideoCodec::Hevc,true).await.unwrap();
        let answer=host.description(false).await.unwrap();assert!(offers_hdr_codec(&answer,VideoCodec::Hevc));
        viewer.remote(answer,local).await.unwrap();
        let (a,b)=tokio::join!(host.connected(),viewer.connected());a.unwrap();b.unwrap();
        let data=uu_media::hdr::HEVC_PROBE;let end=data.windows(5).enumerate().skip(1).find(|(_,v)|*v==[0,0,0,1,70]).map_or(data.len(),|(i,_)|i);
        sender.frame(&uu_media::EncodedFrame{data:bytes::Bytes::copy_from_slice(&data[..end]),pts:uu_media::Timestamp(0),is_keyframe:true}).await.unwrap();
        let (codec,packet)=timeout(Duration::from_secs(3),rx.recv()).await.unwrap().unwrap();
        assert_eq!(codec.payload_type,108);assert_eq!(packet.header.payload_type,108);assert!(codec.capability.sdp_fmtp_line.contains("profile-id=2"));assert!(!packet.payload.is_empty());
        host.close().await;viewer.close().await;
    }
    #[tokio::test]
    async fn authenticated_observed_address_changes_advertisement_without_relaxing_peer_validation() {
        let local = "127.0.0.1".parse().unwrap();
        let observed = "127.0.0.2".parse().unwrap();
        let peer = NativePeer::new_with_advertised(local, observed, None, false).await.unwrap();
        let (frames, _receiver) = crate::media::frame_channel();
        peer.receiver(frames, Arc::new(MediaStats::default())).await.unwrap();
        let offer = peer.description(true).await.unwrap();
        assert!(validate_candidates(&offer.sdp, observed).is_ok());
        assert!(validate_candidates(&offer.sdp, local).is_err());
        peer.close().await;
        assert!(NativePeer::new_with_advertised(local, "0.0.0.0".parse().unwrap(), None, false).await.is_err());
        assert!(NativePeer::new_with_advertised(local, "224.0.0.1".parse().unwrap(), None, false).await.is_err());
        assert!(NativePeer::new_with_advertised(local, "::1".parse().unwrap(), None, false).await.is_err());
    }
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
    use webrtc::{
        rtp::{header::Header, packet::Packet},
        srtp::{
            context::Context, option::srtp_replay_protection, protection_profile::ProtectionProfile,
        },
        util::marshal::Marshal,
    };
    fn pair(profile: ProtectionProfile) -> (Context, Context) {
        let key = [0x17; 16];
        let salt = vec![
            0x2a;
            if matches!(profile, ProtectionProfile::Aes128CmHmacSha1_80) {
                14
            } else {
                12
            }
        ];
        (
            Context::new(&key, &salt, profile, None, None).unwrap(),
            Context::new(&key, &salt, profile, Some(srtp_replay_protection(64)), None).unwrap(),
        )
    }
    fn packet(seq: u16) -> bytes::Bytes {
        Packet {
            header: Header {
                version: 2,
                payload_type: 102,
                ssrc: 42,
                sequence_number: seq,
                ..Default::default()
            },
            payload: bytes::Bytes::from_static(b"generated SRTP regression"),
            ..Default::default()
        }
        .marshal()
        .unwrap()
    }
    #[test]
    fn late_retransmissions_keep_original_ciphertext_and_new_packets_decodable() {
        for profile in [
            ProtectionProfile::Aes128CmHmacSha1_80,
            ProtectionProfile::AeadAes128Gcm,
        ] {
            let (mut sender, mut receiver) = pair(profile);
            let mut cached = Vec::new();
            for seq in (65500..=65535).chain(0..=200) {
                let plain = packet(seq);
                let wire = sender.encrypt_rtp(&plain).unwrap();
                assert_eq!(receiver.decrypt_rtp(&wire).unwrap(), plain);
                if seq == 65530 || seq == 40 {
                    cached.push((seq, wire));
                }
            }
            for (seq, original) in cached {
                let wire = sender.encrypt_rtp(&packet(seq)).unwrap();
                assert_eq!(
                    wire, original,
                    "retransmission must retain its original packet index"
                );
                assert!(
                    receiver.decrypt_rtp(&wire).is_err(),
                    "replay protection must remain active"
                );
            }
            let plain = packet(201);
            let wire = sender.encrypt_rtp(&plain).unwrap();
            assert_eq!(receiver.decrypt_rtp(&wire).unwrap(), plain);
        }
    }
    #[test]
    fn loss_of_more_than_one_hundred_packets_across_rollover_recovers_with_authentication() {
        for profile in [
            ProtectionProfile::Aes128CmHmacSha1_80,
            ProtectionProfile::AeadAes128Gcm,
        ] {
            let (mut sender, mut receiver) = pair(profile);
            for seq in (65500..=65535).chain(0..=200) {
                let plain = packet(seq);
                let wire = sender.encrypt_rtp(&plain).unwrap();
                if seq >= 65500 {
                    assert_eq!(receiver.decrypt_rtp(&wire).unwrap(), plain);
                }
            }
            let plain = packet(201);
            let wire = sender.encrypt_rtp(&plain).unwrap();
            let mut tampered = wire.to_vec();
            *tampered.last_mut().unwrap() ^= 1;
            assert!(
                receiver.decrypt_rtp(&tampered).is_err(),
                "forged packets must not move receiver state"
            );
            assert_eq!(receiver.decrypt_rtp(&wire).unwrap(), plain);
            assert!(
                receiver.decrypt_rtp(&wire).is_err(),
                "duplicate packets must still be rejected"
            );
        }
    }
}

#[cfg(test)]
#[path="native_wire_tests.rs"]
mod wire_tests;
