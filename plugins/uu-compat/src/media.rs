//! Controller-side WebRTC media plane (webrtc-rs).
//!
//! Mirrors the validated `tools/linux_controller/proto.py` flow: five
//! recv-only video transceivers, one audio transceiver and a data channel.
//! The signalling layer (`socketio.rs`) owns the SDP/ICE exchange; this module
//! only wraps the PeerConnection.

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use webrtc::api::interceptor_registry::configure_rtcp_reports;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_H264, MIME_TYPE_OPUS};
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_credential_type::RTCIceCredentialType;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;
use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;
use webrtc::rtp_transceiver::RTCRtpTransceiverInit;

pub use crate::display_frames::{channel as frame_channel, FrameReceiver, FrameSender};
pub use uu_media::DecoderKind;

pub use uu_core::media::MediaStats;
use uu_core::plugin_backend::{receive_audio,receive_video};
pub struct ControllerMedia {
    _connection_monitor:Option<crate::connection_info::Monitor>,
    pub pc: Arc<RTCPeerConnection>,
    repairs: Arc<crate::rtx::RepairBus>,
    pub data_channel: Arc<webrtc::data_channel::RTCDataChannel>,
    _text_channel: Option<Arc<webrtc::data_channel::RTCDataChannel>>,
    seq: Arc<AtomicU64>,
    network_probe: Arc<std::sync::Mutex<crate::network_probe::NetworkProbe>>,
    stats: Option<Arc<MediaStats>>,
    wait_for_ice_gather: bool,
    hevc_negotiated: std::sync::atomic::AtomicBool,
}

impl ControllerMedia {
    pub(crate) fn bind_initial_capture(&self,data:&[u8])->Vec<u8> {
        self.stats.as_ref().and_then(|s| {
            let screens=s.remote_screens.lock().unwrap();
            let source=screens.selected().or_else(||screens.screens.iter().find(|s|s.primary)).or_else(||screens.screens.first())?;
            crate::control_wire::bind_current_capture(data,source.id)
        }).unwrap_or_else(||data.to_vec())
    }
    pub(crate) fn wants_exclusive_display(&self)->bool {
        self.stats.as_ref().is_some_and(|s|s.display_mode.load(Ordering::Acquire)==1)
    }
    pub(crate) fn has_super_screen(&self)->bool {
        self.stats.as_ref().is_some_and(|s|{let screens=s.remote_screens.lock().unwrap();screens.screens.len()==1 && screens.selected().is_some_and(|v|v.kind==2)})
    }
    pub(crate) fn exclusive_request(&self,seq:u64)->Option<Vec<u8>> {
        let stats=self.stats.as_ref()?;
        let screens=stats.remote_screens.lock().unwrap();
        let source=screens.screens.iter().find(|s|s.kind!=0)
            .or_else(||screens.selected()).or_else(||screens.screens.iter().find(|s|s.primary)).or_else(||screens.screens.first());
        let (w,h,dpi)=source.map(|s|(s.width,s.height,s.dpi_scale))
            .filter(|(w,h,_)|*w>0&&*h>0).unwrap_or_else(||{let (w,h)=stats.res();(w,h,None)});
        crate::screen_control::enter_exclusive(seq,w,h,dpi)
    }
    pub(crate) fn exclusive_result(&self)->u64 {
        self.stats.as_ref().map_or(0,|s|s.exclusive_result.load(Ordering::Acquire))
    }

    /// Build the PeerConnection with the ICE servers returned by `success`.
    pub async fn new(ice: Vec<RTCIceServer>) -> Result<Self> {
        Self::new_with_frames(ice, None, None).await
    }

    pub async fn new_with_frames(
        ice: Vec<RTCIceServer>,
        frames: Option<FrameSender>,
        stats: Option<Arc<MediaStats>>,
    ) -> Result<Self> {
        // With no usable STUN/TURN servers, webrtc-ice still waits its fixed
        // STUN gather timeout. Host candidates are available immediately, so
        // avoid that pointless delay and only fall back to the full wait if no
        // candidate appears in the local SDP.
        let wait_for_ice_gather = !ice.is_empty();
        let mut media = MediaEngine::default();
        register_receive_codecs(&mut media, stats.as_ref().is_some_and(|s| s.hevc_enabled.get() == Some(&true)))?;
        let mut registry = Registry::new();
        // NACK is generated from the application's bounded reorder window so
        // successful RTX clears the hole and expired pictures stop requesting.
        registry = configure_rtcp_reports(registry);
        registry = crate::congestion::configure(registry, &mut media, stats.as_ref().map(|s|s.twcc_metadata_drops.clone()).unwrap_or_default(), stats.as_ref().map(|s|s.twcc_received_packets.clone()).unwrap_or_default())?;
        let repairs = crate::rtx::RepairBus::new(stats.clone());
        registry.add(Box::new(crate::rtx::RepairTap(repairs.clone())));
        let mut settings=webrtc::api::setting_engine::SettingEngine::default();
        crate::srtp_policy::configure(&mut settings);
        let api = APIBuilder::new()
            .with_media_engine(media)
            .with_setting_engine(settings)
            .with_interceptor_registry(registry)
            .build();

        let pc = Arc::new(
            api.new_peer_connection(RTCConfiguration {
                ice_servers: ice,
                ..Default::default()
            })
            .await?,
        );

        pc.on_peer_connection_state_change(Box::new(|s: RTCPeerConnectionState| {
            println!("peer state: {s}");
            Box::pin(async {})
        }));

        let network_probe = Arc::new(std::sync::Mutex::new(crate::network_probe::NetworkProbe::default()));
        let pc_track = Arc::downgrade(&pc);
        let repair_track = repairs.clone();
        let frames_tx = frames.clone();
        let stats_track = stats.clone();
        let track_counter = Arc::new(AtomicU32::new(0));
        pc.on_track(Box::new(move |track, _receiver, transceiver| {
            let pc = pc_track.clone();
            let repairs = repair_track.clone();
            let frames = frames_tx.clone();
            let stats = stats_track.clone();
            let index = if track.kind() == RTPCodecType::Video {
                // Track identity comes from the negotiated video_N label, with
                // mid as a fallback, never from packet arrival order.
                let id = track.id();
                let mid = transceiver.mid();
                let index = video_track_identity(&id, mid.as_deref());
                if let Some(index) = index {
                    track_counter.fetch_max(index + 1, Ordering::Relaxed);
                    index
                } else {
                    track_counter.fetch_add(1, Ordering::Relaxed)
                }
            } else {
                u32::MAX
            };
            if let Some(s) = &stats {
                s.diagnostics.record(serde_json::json!({"event":"track", "kind":track.kind().to_string(), "index":index, "id":track.id(), "mid":transceiver.mid()}));
            }
            Box::pin(async move {
                if let Some(pc) = pc.upgrade() {
                    if track.kind() == RTPCodecType::Audio {
                        tokio::spawn(receive_audio(track, stats));
                    } else {
                        tokio::spawn(receive_video(pc, track, index, frames, stats, repairs));
                    }
                }
            })
        }));

        // 5 displays (the controlled machine may only send on one).
        for _ in 0..5 {
            pc.add_transceiver_from_kind(
                RTPCodecType::Video,
                Some(RTCRtpTransceiverInit {
                    direction: RTCRtpTransceiverDirection::Recvonly,
                    send_encodings: vec![],
                }),
            )
            .await?;
        }
        pc.add_transceiver_from_kind(
            RTPCodecType::Audio,
            Some(RTCRtpTransceiverInit {
                direction: RTCRtpTransceiverDirection::Recvonly,
                send_encodings: vec![],
            }),
        )
        .await?;
        // The official controller opens five data channels; input handling is
        // wired to `CONTROL_DATA_CHANNEL`, and input frames are raw JSON (no
        // PCK framing). After the channels open the host repeats a SimpleAction
        // echo request until it is answered, so we send our own echo request
        // and answer theirs before input is accepted.
        let seq = Arc::new(AtomicU64::new(1));
        let mut control_dc = None;
        let mut text_channel=None;
        let mut channels=Vec::new();
        for label in ["CONTROL_DATA_CHANNEL","TEXT_DATA_CHANNEL","STREAMER_DATA_CHANNEL","FILE_DATA_CHANNEL","BINARY_DATA_CHANNEL"] {
            channels.push(pc.create_data_channel(label,None).await?);
        }
        let text_for_control=Arc::downgrade(&channels[1]);
        let (clipboard_tx,mut clipboard_rx)=tokio::sync::mpsc::channel::<Vec<u8>>(16);
        let clipboard_stats=stats.clone();let clipboard_channel=text_for_control.clone();let clipboard_seq=seq.clone();
        tokio::spawn(async move {
            let mut protocol=crate::clipboard::Protocol::default();
            let mut generation=0;
            let mut tick=tokio::time::interval(Duration::from_millis(50));tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                if clipboard_stats.as_ref().is_some_and(|s|s.clipboard.stopped.load(Ordering::Acquire)){break;}
                if let Some(stats)=&clipboard_stats {let current=stats.clipboard.reset_generation.load(Ordering::Acquire);if generation!=current {protocol.cancel_pending();generation=current;}}
                tokio::select! {
                    message=clipboard_rx.recv()=>{let Some(data)=message else{break;};receive_clipboard(&data,&clipboard_stats,&clipboard_seq,&clipboard_channel,&mut protocol).await;},
                    _=tick.tick()=>flush_clipboard(&clipboard_stats,&clipboard_seq,&clipboard_channel).await,
                }
            }
        });
        for ch in channels {
            let label=ch.label();
            if label == "TEXT_DATA_CHANNEL" {
                text_channel = Some(ch.clone());
            }
            if label != "CONTROL_DATA_CHANNEL" && label != "TEXT_DATA_CHANNEL" { continue; }
            // Official hosts send CaptureChange on TEXT and may send screen
            // inventories / RPC replies there too. Both channels carry the same
            // Message protobuf; TEXT is also used for clipboard traffic.
            if label == "CONTROL_DATA_CHANNEL" {
                let open_ch = Arc::downgrade(&ch);
                let open_seq = seq.clone();
                let open_stats = stats.clone();
                ch.on_open(Box::new(move || {
                    let ch = open_ch.clone();
                    let seq = open_seq.clone();
                    let stats = open_stats.clone();
                    Box::pin(async move {
                        let Some(ch) = ch.upgrade() else { return };
                        println!("CONTROL_DATA_CHANNEL open");
                        if let Some(s) = &stats { s.control_open.store(1, Ordering::Release); }
                        let s = seq.fetch_add(1, Ordering::Relaxed);
                        let _ = ch
                            .send(&bytes::Bytes::from(crate::control_wire::echo_request_with_clipboard(s,stats.as_ref().is_some_and(|s|s.clipboard.enabled.load(Ordering::Acquire)))))
                            .await;
                    })
                }));
                let close_stats = stats.clone();
                ch.on_close(Box::new(move || {
                    if let Some(s) = &close_stats { s.control_open.store(0, Ordering::Release); }
                    println!("CONTROL_DATA_CHANNEL closed");
                    Box::pin(async {})
                }));
                control_dc = Some(ch.clone());
            }
            let msg_clipboard=clipboard_tx.clone();
            let msg_ch = Arc::downgrade(&ch);
            let msg_seq = seq.clone();
            let msg_probe = network_probe.clone();
            let msg_stats = stats.clone();
            let msg_channel = label.to_string();
            ch.on_message(Box::new(
                move |msg: webrtc::data_channel::data_channel_message::DataChannelMessage| {
                    let clipboard=msg_clipboard.clone();
                    let ch = msg_ch.clone();
                    let seq = msg_seq.clone();
                    let probe = msg_probe.clone();
                    let stats = msg_stats.clone();
                    let channel = msg_channel.clone();
                    Box::pin(async move {
                        let received_at = Instant::now();
                        let Some(ch) = ch.upgrade() else { return };
                        {
                            queue_clipboard(&msg.data,&stats,&clipboard);
                            if let Some(s) = &stats {
                                if crate::control_wire::echo_clipboard_supported(&msg.data){s.clipboard.supported.store(true,Ordering::Release);}
                                if let Some(shape)=crate::cursor::parse(&msg.data) {
                                    let mut cursor=s.cursor.lock().unwrap();
                                    let revision=cursor.as_ref().map_or(1,|(revision,_)|revision+1);
                                    *cursor=Some((revision,shape));
                                }
                                if let Some((id,case,code))=crate::screen_control::response(&msg.data) {
                                    s.diagnostics.record(serde_json::json!({"event":"display_reply","channel":channel,"request":id,"case":case,"code":code}));
                                    if id==900 && case==22 {
                                        s.exclusive_result.store(if code==0{1}else{2},Ordering::Release);
                                        if code==0{s.exclusive_owned.store(true,Ordering::Release);}
                                    }
                                    let mut replies=s.display_responses.lock().unwrap();
                                    if replies.len()==16{replies.pop_front();}
                                    replies.push_back((id,case,code));
                                }
                                if let Some(screens) = crate::screen_control::screen_sources(&msg.data) {
                                    s.diagnostics.record(serde_json::json!({"event":"screens","channel":channel,"current":screens.current_id,"sources":screens.screens.iter().map(|v|serde_json::json!({"id":v.id,"track":v.video_track_index,"name":v.display_name,"kind":v.kind,"primary":v.primary,"width":v.width,"height":v.height})).collect::<Vec<_>>()}));
                                    *s.remote_screens.lock().unwrap() = screens;
                                    s.remote_screens_revision.fetch_add(1,Ordering::Release);
                                }
                                if let Some(id) = crate::screen_control::desktop_capture_change(&msg.data) {
                                    s.diagnostics.record(serde_json::json!({"event":"capture_change","channel":channel,"source":id}));
                                    *s.desktop_capture_source.lock().unwrap() = Some(id);
                                }
                            }
                            if let Some((id, accepted)) = crate::control_wire::capture_setting_response(&msg.data) {
                                if let Some(s) = &stats {
                                    s.diagnostics.record(serde_json::json!({"event":"settings_reply","channel":channel,"request":id,"accepted":accepted}));
                                    s.settings_response.fetch_max((id << 1) | accepted as u64, Ordering::Release);
                                }
                            }
                            let reading = {
                                let mut probe = probe.lock().unwrap();
                                probe.reply_at(&msg.data, received_at).map(|values| (values, probe.distribution_us()))
                            };
                            if let Some(((mean, deviation), distribution)) = reading {
                                if let Some(s) = &stats {
                                    *s.rtt_distribution_us.lock().unwrap() = distribution;
                                    let send_us = s.control_probe_send_us.load(Ordering::Relaxed);
                                    s.diagnostics.record(serde_json::json!({"event":"control_rtt",
                                        "mean_us":mean,"stddev_us":deviation,
                                        "last_min_p50_p95_max_us":distribution,
                                        "send_call_us":(send_us>0).then_some(send_us)}));
                                    s.rtt_ms.store(mean / 1000, Ordering::Relaxed);
                                    s.rtt_us.store(mean, Ordering::Relaxed);
                                    s.rtt_deviation_us.store(deviation, Ordering::Relaxed);
                                    s.rtt_updated_ms.store(crate::control_wire::now_ms(), Ordering::Release);
                                }
                            }
                            if let Some(req) = crate::control_wire::echo_request_seq(&msg.data) {
                                let s = seq.fetch_add(1, Ordering::Relaxed);
                                let resp = crate::control_wire::echo_response_with_clipboard(s,req,stats.as_ref().is_some_and(|s|s.clipboard.enabled.load(Ordering::Acquire)));
                                let _ = ch.send(&bytes::Bytes::from(resp)).await;
                                println!("CONTROL echo request seq={req} answered (out seq={s})");

                            }
                        }
                    })
                },
            ));
        }
        let dc = control_dc.expect("CONTROL_DATA_CHANNEL must be created");

        Ok(Self {
            _connection_monitor:stats.as_ref().map(|stats|{stats.connection.set_service(uu_i18n::text("UU 兼容连接"));crate::connection_info::Monitor::start(&pc,stats)}),
            pc,
            repairs,
            data_channel: dc,
            _text_channel:text_channel,
            seq,
            network_probe,
            stats,
            wait_for_ice_gather,
            hevc_negotiated: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Register before set_local_description so even the first host candidate
    /// is queued. Signalling must send the offer before draining this receiver,
    /// and keep draining it after the answer (STUN/TURN may finish later).
    pub async fn trickle_offer(&self) -> Result<(String, tokio::sync::mpsc::UnboundedReceiver<Option<RTCIceCandidateInit>>)> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.pc.on_ice_candidate(Box::new(move |candidate| {
            let tx = tx.clone();
            Box::pin(async move {
                let value = match candidate {
                    Some(candidate) => match candidate.to_json() {
                        Ok(mut value) => {
                            // All transceivers use the first BUNDLE transport.
                            // webrtc-rs 0.11 returns an empty MID from to_json.
                            value.sdp_mid = Some("0".into());
                            value.sdp_mline_index = Some(0);
                            Some(value)
                        }
                        Err(error) => {
                            tracing::warn!(%error, "cannot serialize local ICE candidate");
                            return;
                        }
                    },
                    None => None,
                };
                let _ = tx.send(value);
            })
        }));
        let offer = self.pc.create_offer(None).await?;
        let sdp = offer.sdp.clone();
        self.pc.set_local_description(offer).await?;
        Ok((sdp, rx))
    }

    /// Gather a self-contained offer for offline probes and direct peer tests.
    /// Interactive Socket.IO sessions use trickle_offer to avoid this wait.
    pub async fn offer(&self) -> Result<String> {
        let offer = self.pc.create_offer(None).await?;
        self.pc.set_local_description(offer).await?;
        if !self.wait_for_ice_gather {
            let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
            loop {
                let local = self
                    .pc
                    .local_description()
                    .await
                    .context("no local description after gathering")?;
                if local.sdp.contains("a=candidate") {
                    return Ok(local.sdp);
                }
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        let mut gather = self.pc.gathering_complete_promise().await;
        let _ = tokio::time::timeout(Duration::from_secs(8), gather.recv()).await;
        let local = self
            .pc
            .local_description()
            .await
            .context("no local description after gathering")?;
        Ok(local.sdp)
    }

    pub async fn set_answer(&self, sdp: &str) -> Result<()> {
        self.repairs.configure(sdp);
        self.pc
            .set_remote_description(RTCSessionDescription::answer(sdp.to_string())?)
            .await?;
        self.hevc_negotiated.store(sdp.split("m=").skip(1).any(|section| {
            let mut lines = section.lines();
            let Some(header) = lines.next() else { return false; };
            let fields: Vec<_> = header.split_whitespace().collect();
            fields.first() == Some(&"video") && fields.get(1) != Some(&"0") && lines.any(|l| {
                l.trim().strip_prefix("a=rtpmap:").and_then(|l|l.split_once(' '))
                    .is_some_and(|(pt,codec)| fields.get(3..).unwrap_or(&[]).contains(&pt) && codec.eq_ignore_ascii_case("H265/90000"))
            })
        }), Ordering::Release);
        Ok(())
    }

    pub async fn add_candidate(&self, candidate: String, mid: String, index: u16) -> Result<()> {
        self.pc
            .add_ice_candidate(RTCIceCandidateInit {
                candidate,
                sdp_mid: Some(mid),
                sdp_mline_index: Some(index),
                username_fragment: None,
            })
            .await?;
        Ok(())
    }
}

/// Convert the `iceServers` array from the `success` event.
pub fn ice_servers_from_json(v: &serde_json::Value) -> Vec<RTCIceServer> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    let url = s["urls"].as_str().unwrap_or_default();
                    // webrtc-rs 0.11 rejects the `turns:` credential form used
                    // by UU; STUN + TURN/UDP is enough, including on the LAN.
                    if url.starts_with("turns:") {
                        return None;
                    }
                    Some(RTCIceServer {
                        urls: vec![url.to_string()],
                        username: s["username"].as_str().unwrap_or_default().to_string(),
                        credential: s["credential"].as_str().unwrap_or_default().to_string(),
                        credential_type: RTCIceCredentialType::Password,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

impl ControllerMedia {
    pub(crate) fn trace_transport(&self, previous: &mut String) {
        let sctp = self.pc.sctp();
        let state = serde_json::json!({"event":"transport", "peer":self.pc.connection_state().to_string(),
            "ice":self.pc.ice_connection_state().to_string(),"dtls":sctp.transport().state().to_string(),
            "sctp":sctp.state().to_string(),"control":self.data_channel.ready_state().to_string()});
        let current = state.to_string();
        if &current != previous {
            *previous = current;
            if let Some(stats) = &self.stats { stats.diagnostics.record(state); }
        }
    }
    /// The ICE stats in webrtc-rs 0.11 leave RTT at zero. Use the host's
    /// existing SimpleAction echo protocol and match exactly one pending seq.
    pub async fn probe_network(&self) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        if let Some(s) = &self.stats {s.control_probe_send_us.store(0,Ordering::Relaxed);}
        let started = Instant::now();
        self.network_probe.lock().unwrap().begin(seq);
        let _ = self.send_control(crate::control_wire::echo_request_with_clipboard(seq,self.stats.as_ref().is_some_and(|s|s.clipboard.enabled.load(Ordering::Acquire)))).await;
        if let Some(s) = &self.stats {s.control_probe_send_us.store(started.elapsed().as_micros().min(u32::MAX as u128) as u32,Ordering::Relaxed);}
    }
    /// Send a raw PCK frame over the WebRTC data channel (control hypothesis).
    pub async fn send_control(&self, data: Vec<u8>) -> Result<()> {
        let hevc = self.hevc_negotiated.load(Ordering::Acquire);
        let data = crate::control_wire::capture_codec(&data, hevc).unwrap_or(data);
        let metadata = self.stats.as_ref().and_then(|_| crate::control_wire::capture_setting_metadata(&data));
        self.data_channel.send(&bytes::Bytes::from(data)).await?;
        if let (Some(stats), Some(metadata)) = (&self.stats, metadata) { stats.diagnostics.record(metadata); }
        Ok(())
    }
    pub async fn send_input(&self, data: Vec<u8>) -> Result<()> {
        let result = self.send_control(data).await;
        if let Some(s) = &self.stats {
            if result.is_ok() { s.input_messages_sent.fetch_add(1, Ordering::Relaxed); }
            else { s.input_send_errors.fetch_add(1, Ordering::Relaxed); }
        }
        result
    }
}

fn queue_clipboard(data:&[u8],stats:&Option<Arc<MediaStats>>,sender:&tokio::sync::mpsc::Sender<Vec<u8>>) {
    if let Some(stats)=stats {
        if stats.clipboard.ready() && crate::clipboard::is_clipboard(data) && sender.try_send(data.to_vec()).is_err() {
            stats.clipboard.status(uu_i18n::text("剪贴板队列繁忙，已跳过过期更新"));
        }
    }
}
async fn flush_clipboard(stats:&Option<Arc<MediaStats>>,seq:&Arc<AtomicU64>,weak_channel:&std::sync::Weak<webrtc::data_channel::RTCDataChannel>) {
        let Some(stats)=stats else{return;};let clipboard=&stats.clipboard;
        if !clipboard.ready() || clipboard.read_only.load(Ordering::Acquire){return;}
        if !clipboard.supported.load(Ordering::Acquire){clipboard.status(uu_i18n::text("等待远端启用文本剪贴板"));return;}
        let Some(channel)=weak_channel.upgrade() else{return;};
        if channel.ready_state()!=webrtc::data_channel::data_channel_state::RTCDataChannelState::Open{return;}
        let value=clipboard.local.lock().unwrap().take();let Some(value)=value else{return;};
        let Some(text)=value.as_text() else{clipboard.status(uu_i18n::text("此兼容连接只支持文本剪贴板"));return;};
        let seq=seq.fetch_add(1,Ordering::Relaxed);
        let Ok(message)=crate::clipboard::text_change(seq,text) else{clipboard.status(uu_i18n::text("文本超过30 KiB或包含无效字符"));return;};
        match tokio::time::timeout(Duration::from_millis(200),channel.send(&bytes::Bytes::from(message))).await {
            Ok(Ok(_))=>{clipboard.last_sent.store(seq,Ordering::Release);clipboard.sent.fetch_add(1,Ordering::Relaxed);clipboard.status(uu_i18n::text("文本已发送，等待远端确认"));},
            _=>{let mut pending=clipboard.local.lock().unwrap();if pending.is_none(){*pending=Some(value);}clipboard.status(uu_i18n::text("剪贴板发送失败，将重试"));},
        }
    }

async fn receive_clipboard(data:&[u8],stats:&Option<Arc<MediaStats>>,seq:&Arc<AtomicU64>,channel:&std::sync::Weak<webrtc::data_channel::RTCDataChannel>,protocol:&mut crate::clipboard::Protocol) {
    let Some(stats)=stats else{return;};let clipboard=&stats.clipboard;
    if !clipboard.ready() || !crate::clipboard::is_clipboard(data){return;}
    clipboard.supported.store(true,Ordering::Release);
    let received=protocol.receive(data,seq);
    match received {
        Ok(received)=>{
            if let Some(text)=received.text {
                if clipboard.ready(){*clipboard.remote.lock().unwrap()=Some(text.into());clipboard.received.fetch_add(1,Ordering::Relaxed);}
            }
            if received.acknowledged.is_some_and(|id|id==clipboard.last_sent.load(Ordering::Acquire)){clipboard.status(uu_i18n::text("文本剪贴板已同步"));}
            if let Some(channel)=channel.upgrade(){for reply in received.replies{let _=tokio::time::timeout(Duration::from_millis(200),channel.send(&bytes::Bytes::from(reply))).await;}}
        }
        Err(_)=>clipboard.status(uu_i18n::text("已忽略无效或超出范围的剪贴板数据")),
    }
}

/// Official SDP names tracks video_N. N is an identity, not a slot count.
fn video_track_identity(id: &str, mid: Option<&str>) -> Option<u32> {
    id.strip_prefix("video_")
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v <= i32::MAX as u32)
        .or_else(|| mid.and_then(|v| v.parse::<u32>().ok()))
}
#[cfg(test)]
mod display_track_tests {
    #[test]
    fn negotiated_track_identity_does_not_assume_dense_or_five_bounded_ids() {
        assert_eq!(super::video_track_identity("video_7",Some("0")),Some(7));
        assert_eq!(super::video_track_identity("video_2",Some("2")),Some(2));
        assert_eq!(super::video_track_identity("other",Some("3")),Some(3));
        assert_eq!(super::video_track_identity("video_-1",None),None);
    }
}
/// Advertise only codecs whose payloads we can actually consume. The default
/// registry also advertises ULPFEC without a receive-side repair implementation.
fn register_receive_codecs(media: &mut MediaEngine, hevc: bool) -> Result<()> {
    use webrtc::rtp_transceiver::rtp_codec::{RTCRtpCodecCapability, RTCRtpCodecParameters};
    use webrtc::rtp_transceiver::RTCPFeedback;
    let feedback = vec![
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
        RTCPFeedback {
            typ: "goog-remb".into(),
            parameter: String::new(),
        },
    ];
    if hevc {
        for (payload_type, mime, fmtp) in [(104, "video/H265", ""), (105, "video/rtx", "apt=104")] {
            media.register_codec(RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability { mime_type: mime.into(), clock_rate: 90_000,
                    sdp_fmtp_line: fmtp.into(), rtcp_feedback: if payload_type == 104 { feedback.clone() } else { vec![] },
                    ..Default::default() }, payload_type, ..Default::default()
            }, RTPCodecType::Video)?;
        }
    }
    for (payload_type, repair_type, profile, mode) in [
        (102, 103, "42001f", 1),
        (127, 126, "42001f", 0),
        (125, 124, "42e01f", 1),
        (108, 109, "42e01f", 0),
        (123, 122, "640032", 1),
    ] {
        media.register_codec(RTCRtpCodecParameters {
            capability:RTCRtpCodecCapability {mime_type:MIME_TYPE_H264.into(),clock_rate:90_000,
                sdp_fmtp_line:format!("level-asymmetry-allowed=1;packetization-mode={mode};profile-level-id={profile}"),
                rtcp_feedback:feedback.clone(),..Default::default()},payload_type,..Default::default()
        },RTPCodecType::Video)?;
        media.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: "video/rtx".into(),
                    clock_rate: 90_000,
                    sdp_fmtp_line: format!("apt={payload_type}"),
                    ..Default::default()
                },
                payload_type: repair_type,
                ..Default::default()
            },
            RTPCodecType::Video,
        )?;
    }
    media.register_codec(
        RTCRtpCodecParameters {
            capability: RTCRtpCodecCapability {
                mime_type: MIME_TYPE_OPUS.into(),
                clock_rate: 48_000,
                channels: 2,
                sdp_fmtp_line: "minptime=10;useinbandfec=1;stereo=1".into(),
                ..Default::default()
            },
            payload_type: 111,
            ..Default::default()
        },
        RTPCodecType::Audio,
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "media_channel_tests.rs"]
mod channel_tests;

#[cfg(test)]
#[path = "media_hevc_tests.rs"]
mod hevc_tests;
