//! Controller-side WebRTC media plane (webrtc-rs).
//!
//! Mirrors the validated `tools/linux_controller/proto.py` flow: five
//! recv-only video transceivers, one audio transceiver and a data channel.
//! The signalling layer (`socketio.rs`) owns the SDP/ICE exchange; this module
//! only wraps the PeerConnection.

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use webrtc::api::interceptor_registry::{configure_rtcp_reports, configure_twcc_receiver_only};
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
use webrtc::rtcp::payload_feedbacks::full_intra_request::{FirEntry, FullIntraRequest};
use webrtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use webrtc::rtcp::payload_feedbacks::receiver_estimated_maximum_bitrate::ReceiverEstimatedMaximumBitrate;
use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;
use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;
use webrtc::rtp_transceiver::RTCRtpTransceiverInit;

pub use crate::display_frames::{channel as frame_channel, FrameReceiver, FrameSender};

/// Live media statistics shared between the decode task and the UI (HUD).
/// Fixed-point where noted so the hot path stays lock-free.
#[derive(Default)]
pub struct MediaStats {
    pub control_open: AtomicU32,
    /// Snapshot of the user's connection policy: 0 preserve, 1 exclusive UU.
    pub display_mode: AtomicU32,
    pub display_responses: std::sync::Mutex<std::collections::VecDeque<(u64,u32,i32)>>,
    pub exclusive_owned: std::sync::atomic::AtomicBool,
    pub exclusive_result: AtomicU64,
    pub audio: Arc<uu_media::audio::AudioControl>,
    pub clipboard: Arc<crate::clipboard::Clipboard>,
    pub cursor: std::sync::Mutex<Option<(u64,crate::cursor::Shape)>>,
    pub input_messages_sent: AtomicU64,
    pub input_send_errors: AtomicU64,
    pub width: AtomicU32,
    pub height: AtomicU32,
    /// Source (network) frame rate x100.
    pub src_fps_x100: AtomicU32,
    /// Decoded frame rate x100.
    pub dec_fps_x100: AtomicU32,
    /// Average decode time in microseconds.
    pub decode_us: AtomicU32,
    /// Received video bitrate in kbit/s.
    pub bitrate_kbps: AtomicU32,
    /// RTP packets received.
    pub packets: AtomicU64,
    /// RTP packets estimated lost (sequence gaps).
    pub lost: AtomicU64,
    /// Total frames decoded.
    pub decoded_total: AtomicU64,
    /// Decode errors.
    pub errors: AtomicU64,
    /// RTCP round-trip time estimate (ms), when available.
    pub rtt_ms: AtomicU32,
    pub rtt_us: AtomicU32,
    pub rtt_deviation_us: AtomicU32,
    pub rtt_updated_ms: AtomicU64,
    /// Capture-setting response: request_id << 1 | accepted.
    pub settings_response: AtomicU64,
    pub remote_screens: std::sync::Mutex<crate::screen_control::RemoteScreens>,
    /// Unrecovered RTP packet loss in the current receive window, parts/million.
    pub loss_ppm: AtomicU32,
    pub codec: std::sync::Mutex<String>,
    pub nack_requests: AtomicU64,
    pub rtx_packets: AtomicU64,
    pub rtx_drops: AtomicU64,
    pub reordered: AtomicU64,
    pub late_packets: AtomicU64,
    pub damaged_frames: AtomicU64,
    pub queue_drops: AtomicU64,
    pub injected_loss: AtomicU64,
    pub keyframe_requests: AtomicU64,
    pub waiting_for_keyframe: AtomicU32,
    pub backend: std::sync::Mutex<String>,
}

impl MediaStats {
    pub fn set_backend(&self, backend: impl Into<String>) {
        if let Ok(mut b) = self.backend.lock() {
            *b = backend.into();
        }
    }

    pub fn backend(&self) -> String {
        self.backend.lock().map(|b| b.clone()).unwrap_or_default()
    }

    pub fn src_fps(&self) -> f32 {
        self.src_fps_x100.load(Ordering::Relaxed) as f32 / 100.0
    }
    pub fn dec_fps(&self) -> f32 {
        self.dec_fps_x100.load(Ordering::Relaxed) as f32 / 100.0
    }
    pub fn decode_ms(&self) -> f32 {
        self.decode_us.load(Ordering::Relaxed) as f32 / 1000.0
    }
    pub fn res(&self) -> (u32, u32) {
        (
            self.width.load(Ordering::Relaxed),
            self.height.load(Ordering::Relaxed),
        )
    }
}

pub struct ControllerMedia {
    pub pc: Arc<RTCPeerConnection>,
    repairs: Arc<crate::rtx::RepairBus>,
    pub data_channel: Arc<webrtc::data_channel::RTCDataChannel>,
    _text_channel: Option<Arc<webrtc::data_channel::RTCDataChannel>>,
    seq: Arc<AtomicU64>,
    network_probe: Arc<std::sync::Mutex<crate::network_probe::NetworkProbe>>,
    stats: Option<Arc<MediaStats>>,
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
        let mut media = MediaEngine::default();
        register_receive_codecs(&mut media)?;
        let mut registry = Registry::new();
        // NACK is generated from the application's bounded reorder window so
        // successful RTX clears the hole and expired pictures stop requesting.
        registry = configure_rtcp_reports(registry);
        registry = configure_twcc_receiver_only(registry, &mut media)?;
        let repairs = crate::rtx::RepairBus::new(stats.clone());
        registry.add(Box::new(crate::rtx::RepairTap(repairs.clone())));
        let api = APIBuilder::new()
            .with_media_engine(media)
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
                // Our offer has video m-lines 0..4. Packet arrival order is not
                // a screen identity (a later display may start sending first).
                let index = transceiver.mid().and_then(|mid| mid.parse::<u32>().ok())
                    .filter(|index| *index < 5);
                if let Some(index) = index {
                    track_counter.fetch_max(index + 1, Ordering::Relaxed);
                    index
                } else {
                    track_counter.fetch_add(1, Ordering::Relaxed)
                }
            } else {
                u32::MAX
            };
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
            let mut tick=tokio::time::interval(Duration::from_millis(50));tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                if clipboard_stats.as_ref().is_some_and(|s|s.clipboard.stopped.load(Ordering::Acquire)){break;}
                tokio::select! {
                    message=clipboard_rx.recv()=>{let Some(data)=message else{break;};receive_clipboard(&data,&clipboard_stats,&clipboard_seq,&clipboard_channel).await;},
                    _=tick.tick()=>flush_clipboard(&clipboard_stats,&clipboard_seq,&clipboard_channel).await,
                }
            }
        });
        for ch in channels {
            let label=ch.label();
            if label=="TEXT_DATA_CHANNEL" {
                let sender=clipboard_tx.clone();let stats=stats.clone();
                ch.on_message(Box::new(move |message| {
                    queue_clipboard(&message.data,&stats,&sender);
                    Box::pin(async {})
                }));
                text_channel=Some(ch);continue;
            }
            if label != "CONTROL_DATA_CHANNEL" {continue;}
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
            let msg_clipboard=clipboard_tx.clone();
            let msg_ch = Arc::downgrade(&ch);
            let msg_seq = seq.clone();
            let msg_probe = network_probe.clone();
            let msg_stats = stats.clone();
            ch.on_message(Box::new(
                move |msg: webrtc::data_channel::data_channel_message::DataChannelMessage| {
                    let clipboard=msg_clipboard.clone();
                    let ch = msg_ch.clone();
                    let seq = msg_seq.clone();
                    let probe = msg_probe.clone();
                    let stats = msg_stats.clone();
                    Box::pin(async move {
                        let Some(ch) = ch.upgrade() else { return };
                        if !msg.is_string {
                            queue_clipboard(&msg.data,&stats,&clipboard);
                            if let Some(s) = &stats {
                                if crate::control_wire::echo_clipboard_supported(&msg.data){s.clipboard.supported.store(true,Ordering::Release);}
                                if let Some(shape)=crate::cursor::parse(&msg.data) {
                                    let mut cursor=s.cursor.lock().unwrap();
                                    let revision=cursor.as_ref().map_or(1,|(revision,_)|revision+1);
                                    *cursor=Some((revision,shape));
                                }
                                if let Some((id,case,code))=crate::screen_control::response(&msg.data) {
                                    if id==900 && case==22 {
                                        s.exclusive_result.store(if code==0{1}else{2},Ordering::Release);
                                        if code==0{s.exclusive_owned.store(true,Ordering::Release);}
                                    }
                                    let mut replies=s.display_responses.lock().unwrap();
                                    if replies.len()==16{replies.pop_front();}
                                    replies.push_back((id,case,code));
                                }
                                if let Some(screens) = crate::screen_control::screen_sources(&msg.data) {
                                    *s.remote_screens.lock().unwrap() = screens;
                                }
                            }
                            if let Some((id, accepted)) = crate::control_wire::capture_setting_response(&msg.data) {
                                if let Some(s) = &stats {
                                    s.settings_response.fetch_max((id << 1) | accepted as u64, Ordering::Release);
                                }
                            }
                            if let Some((mean, deviation)) = probe.lock().unwrap().reply(&msg.data) {
                                if let Some(s) = &stats {
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
            control_dc = Some(ch);
        }
        let dc = control_dc.expect("CONTROL_DATA_CHANNEL must be created");

        Ok(Self {
            pc,
            repairs,
            data_channel: dc,
            _text_channel:text_channel,
            seq,
            network_probe,
            stats,
        })
    }

    /// Create the local offer and wait until ICE gathering completes so the
    /// SDP carries candidates (the UU peer accepts trickle, but a complete
    /// offer keeps the first exchange simple).
    pub async fn offer(&self) -> Result<String> {
        let offer = self.pc.create_offer(None).await?;
        self.pc.set_local_description(offer).await?;
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

/// Write any decoded layout through the same converter used for presentation.
fn save_ppm(frame: &uu_media::RawFrame, path: &str) {
    let rgba = uu_media::convert::frame_to_rgba(frame);
    let mut out = format!("P6\n{} {}\n255\n", frame.width, frame.height).into_bytes();
    out.extend(rgba.chunks_exact(4).flat_map(|p| p[..3].iter().copied()));
    let _ = std::fs::write(path, out);
}

impl ControllerMedia {
    /// The ICE stats in webrtc-rs 0.11 leave RTT at zero. Use the host's
    /// existing SimpleAction echo protocol and match exactly one pending seq.
    pub async fn probe_network(&self) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.network_probe.lock().unwrap().begin(seq);
        let _ = self.send_control(crate::control_wire::echo_request_with_clipboard(seq,self.stats.as_ref().is_some_and(|s|s.clipboard.enabled.load(Ordering::Acquire)))).await;
    }
    /// Send a raw PCK frame over the WebRTC data channel (control hypothesis).
    pub async fn send_control(&self, data: Vec<u8>) -> Result<()> {
        self.data_channel.send(&bytes::Bytes::from(data)).await?;
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
            stats.clipboard.status("剪贴板队列繁忙，已跳过过期更新");
        }
    }
}
async fn flush_clipboard(stats:&Option<Arc<MediaStats>>,seq:&Arc<AtomicU64>,weak_channel:&std::sync::Weak<webrtc::data_channel::RTCDataChannel>) {
        let Some(stats)=stats else{return;};let clipboard=&stats.clipboard;
        if !clipboard.ready() || clipboard.read_only.load(Ordering::Acquire){return;}
        if !clipboard.supported.load(Ordering::Acquire){clipboard.status("等待远端启用文本剪贴板");return;}
        let Some(channel)=weak_channel.upgrade() else{return;};
        if channel.ready_state()!=webrtc::data_channel::data_channel_state::RTCDataChannelState::Open{return;}
        let value=clipboard.local.lock().unwrap().take();let Some(value)=value else{return;};
        let seq=seq.fetch_add(1,Ordering::Relaxed);
        let Ok(message)=crate::clipboard::text_change(seq,&value) else{clipboard.status("文本超过30 KiB或包含无效字符");return;};
        match tokio::time::timeout(Duration::from_millis(200),channel.send(&bytes::Bytes::from(message))).await {
            Ok(Ok(_))=>{clipboard.last_sent.store(seq,Ordering::Release);clipboard.sent.fetch_add(1,Ordering::Relaxed);clipboard.status("文本已发送，等待远端确认");},
            _=>{let mut pending=clipboard.local.lock().unwrap();if pending.is_none(){*pending=Some(value);}clipboard.status("剪贴板发送失败，将重试");},
        }
    }

async fn receive_clipboard(data:&[u8],stats:&Option<Arc<MediaStats>>,seq:&Arc<AtomicU64>,channel:&std::sync::Weak<webrtc::data_channel::RTCDataChannel>) {
    let Some(stats)=stats else{return;};let clipboard=&stats.clipboard;
    if !clipboard.ready() || !crate::clipboard::is_clipboard(data){return;}
    clipboard.supported.store(true,Ordering::Release);
    let received={clipboard.protocol.lock().unwrap().receive(data,seq)};
    match received {
        Ok(received)=>{
            if let Some(text)=received.text {
                if clipboard.ready(){*clipboard.remote.lock().unwrap()=Some(text);clipboard.received.fetch_add(1,Ordering::Relaxed);}
            }
            if received.acknowledged.is_some_and(|id|id==clipboard.last_sent.load(Ordering::Acquire)){clipboard.status("文本剪贴板已同步");}
            if let Some(channel)=channel.upgrade(){for reply in received.replies{let _=tokio::time::timeout(Duration::from_millis(200),channel.send(&bytes::Bytes::from(reply))).await;}}
        }
        Err(_)=>clipboard.status("已忽略无效或超出范围的剪贴板数据"),
    }
}

async fn receive_audio(track: Arc<webrtc::track::track_remote::TrackRemote>, stats: Option<Arc<MediaStats>>) {
    if !track.codec().capability.mime_type.eq_ignore_ascii_case(MIME_TYPE_OPUS) { return; }
    let control = stats.as_ref().map(|s|s.audio.clone()).unwrap_or_default();
    let tx = uu_media::audio::playback(control.clone());
    while let Ok((packet,_)) = track.read_rtp().await {
        if control.stopped() { break; }
        control.packets.fetch_add(1,Ordering::Relaxed);
        let packet = uu_media::audio::Packet {sequence:packet.header.sequence_number,timestamp:packet.header.timestamp,
            payload:packet.payload.to_vec(),received:std::time::Instant::now()};
        match tx.try_send(packet) {
            Ok(()) => {},
            Err(std::sync::mpsc::TrySendError::Full(_)) => { control.dropped.fetch_add(1,Ordering::Relaxed); },
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => break,
        }
    }
}

async fn receive_video(
    pc: Arc<RTCPeerConnection>,
    track: Arc<webrtc::track::track_remote::TrackRemote>,
    track_idx: u32,
    frame_tx: Option<FrameSender>,
    stats: Option<Arc<MediaStats>>,
    repairs: Arc<crate::rtx::RepairBus>,
) {
    use crate::video_receiver::VideoReceiver;
    use std::time::Instant;
    if track.kind() != RTPCodecType::Video
        || !track
            .codec()
            .capability
            .mime_type
            .eq_ignore_ascii_case(MIME_TYPE_H264)
    {
        return; // Audio and unsupported repair codecs are not H.264 data.
    }
    let debug = std::env::var("UU_MEDIA_DEBUG").is_ok_and(|v| v != "0");
    if let Some(s) = &stats {
        *s.codec.lock().unwrap() = track.codec().capability.mime_type.clone();
    }
    let generation = Arc::new(AtomicU64::new(0));
    let request_generation = Arc::new(crate::decode_gate::RecoverySignal::default());
    let refresh = Arc::new(tokio::sync::Notify::new());
    let (sample_tx, mut sample_rx) = tokio::sync::mpsc::channel::<(bytes::Bytes, u32, u64, u64)>(8);
    let worker_stats = stats.clone();
    let worker_generation = generation.clone();
    let worker_requests = request_generation.clone();
    let worker_refresh = refresh.clone();
    // Decode, colour conversion and driver IPC are blocking CPU/OS work. Keep
    // them off tokio's RTP and RTCP executor threads.
    let worker = tokio::task::spawn_blocking(move || {
        let mut decoder = match uu_media::default_decoder() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("video decoder unavailable: {e}");
                return;
            }
        };
        if let Some(s) = &worker_stats {
            s.set_backend(decoder.backend_name());
        }
        let mut dump = std::env::var("UU_DUMP_H264")
            .ok()
            .and_then(|p| std::fs::File::create(p).ok());
        let mut gate = crate::decode_gate::DecodeGate::default();
        let mut reset_decoder = false;
        let mut recovery = None;
        let mut decoded = 0;
        let mut errors = 0;
        let mut recovery_errors = 0u32;
        let mut window = Instant::now();
        let mut window_frames = 0u64;
        let mut decode_us = 0u64;
        let mut pts_base = None;
        while let Some((data, timestamp, sample_epoch, sample_request)) = sample_rx.blocking_recv()
        {
            if window.elapsed() >= Duration::from_secs(2) {
                if let Some(s) = &worker_stats {
                    s.set_backend(decoder.backend_name());
                    s.dec_fps_x100.store(
                        (window_frames as f64 / window.elapsed().as_secs_f64() * 100.0) as u32,
                        Ordering::Relaxed,
                    );
                    s.decode_us
                        .store((decode_us / window_frames.max(1)) as u32, Ordering::Relaxed);
                    s.waiting_for_keyframe
                        .store(worker_requests.is_waiting() as u32, Ordering::Relaxed);
                }
                if debug {
                    println!("dec frames={decoded} errors={errors} waiting_idr={}", gate.waiting);
                }
                window = Instant::now();
                window_frames = 0;
                decode_us = 0;
            }
            let current_epoch = worker_generation.load(Ordering::Acquire);
            let key = uu_media::EncodedFrame::detect_h264_keyframe(&data);
            if !gate.accepts(sample_epoch, current_epoch, key) { continue; }
            // An IDR resets H.264 references itself. Recreate the decoder only
            // after a decoder/driver error, never for ordinary network loss.
            if key && reset_decoder {
                let next = if recovery_errors >= 3 || decoder.backend_name().starts_with("openh264") {
                    uu_media::software_decoder()
                } else {
                    uu_media::default_decoder()
                };
                match next {
                    Ok(fresh) => decoder = fresh,
                    Err(_) => continue,
                }
                reset_decoder = false;
            }
            if let Some(file) = dump.as_mut() {
                use std::io::Write;
                let _ = file.write_all(&data);
            }
            let base = *pts_base.get_or_insert(timestamp);
            let encoded = uu_media::EncodedFrame {
                data,
                is_keyframe: key,
                pts: uu_media::Timestamp(timestamp.wrapping_sub(base) as u64 * 1_000_000 / 90_000),
            };
            if key {
                recovery = Some((sample_epoch, sample_request, encoded.pts));
            }
            let started = Instant::now();
            match decoder.decode(&encoded) {
                Ok(frame) => {
                    gate.decoded(sample_epoch, worker_generation.load(Ordering::Acquire), key);
                    if let Some(frame) = frame {
                        recovery_errors = 0;
                        if recovery.is_some_and(|(generation, request, pts)| {
                            frame.pts >= pts
                                && generation == worker_generation.load(Ordering::Acquire)
                                && request == worker_requests.requested()
                        }) {
                            worker_requests.complete(recovery.unwrap().1);
                            recovery = None;
                        }
                        decoded += 1;
                        window_frames += 1;
                        decode_us += started.elapsed().as_micros() as u64;
                        if let Some(s) = &worker_stats {
                            s.width.store(frame.width, Ordering::Relaxed);
                            s.height.store(frame.height, Ordering::Relaxed);
                            s.decoded_total.fetch_add(1, Ordering::Relaxed);
                        }
                        if debug && decoded % 60 == 0 {
                            save_ppm(&frame, "/tmp/uu_rust_latest.ppm");
                        }
                        // Decode all reference pictures, but spend RGBA and
                        // upload work only on the newest one after a burst.
                        if let Some(tx) = frame_tx.as_ref().filter(|_| key || sample_rx.is_empty()) {
                            let rgba = uu_media::convert::frame_to_rgba(&frame);
                            let _ = tx.send((track_idx, frame.width, frame.height, rgba));
                        }
                    }
                }
                Err(e) => {
                    errors += 1;
                    // Preserve the fallback budget across decoder recreation;
                    // repeatedly rebuilding a failing driver must not reset it.
                    recovery_errors = recovery_errors.saturating_add(1);
                    gate.failed();
                    reset_decoder = true;
                    recovery = None;
                    worker_requests.request();
                    worker_refresh.notify_one();
                    if let Some(s) = &worker_stats {
                        s.errors.fetch_add(1, Ordering::Relaxed);
                    }
                    if debug {
                        eprintln!("decode error, requesting IDR: {e}");
                    }
                }
            }
        }
        if debug {
            println!("decode done: frames={decoded} errors={errors}");
        }
    });
    let feedback_pc = pc.clone();
    let feedback_waiting = request_generation.clone();
    let feedback_stats = stats.clone();
    let feedback_refresh = refresh.clone();
    let ssrc = track.ssrc();
    let feedback = tokio::spawn(async move {
        let sender_ssrc =
            u32::from_be_bytes(uuid::Uuid::new_v4().as_bytes()[..4].try_into().unwrap());
        let remb_bps = std::env::var("UU_BITRATE")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(55.0)
            * 1_000_000.0;
        let mut ticker = tokio::time::interval(Duration::from_millis(100));
        let mut last_refresh = None;
        let mut last_remb = None;
        let mut fir_seq = 0u8;
        let mut retries = 0u32;
        loop {
            tokio::select! { _ = ticker.tick() => {}, _ = feedback_refresh.notified() => {} }
            let now = Instant::now();
            let mut packets: Vec<Box<dyn webrtc::rtcp::packet::Packet + Send + Sync>> = Vec::new();
            let needs_refresh = feedback_waiting.is_waiting();
            if !needs_refresh {
                retries = 0;
                last_refresh = None;
            }
            if let Some(s) = &feedback_stats {
                s.waiting_for_keyframe
                    .store(needs_refresh as u32, Ordering::Relaxed);
            }
            if needs_refresh
                && last_refresh.map_or(true, |at: Instant| {
                    now.duration_since(at) >= Duration::from_millis(if retries < 2 { 200 } else { 500 })
                })
            {
                packets.push(Box::new(PictureLossIndication {
                    sender_ssrc,
                    media_ssrc: ssrc,
                }));
                retries += 1;
                if retries % 4 == 0 {
                    fir_seq = fir_seq.wrapping_add(1);
                    packets.push(Box::new(FullIntraRequest {
                        sender_ssrc,
                        media_ssrc: 0,
                        fir: vec![FirEntry {
                            ssrc,
                            sequence_number: fir_seq,
                            ..Default::default()
                        }],
                    }));
                }
                last_refresh = Some(now);
                if let Some(s) = &feedback_stats {
                    s.keyframe_requests.fetch_add(1, Ordering::Relaxed);
                }
            }
            // A healthy stream no longer gets forced into a large IDR every
            // two seconds. Periodic REMB is independent of recovery requests.
            if last_remb.map_or(true, |at: Instant| {
                now.duration_since(at) >= Duration::from_secs(2)
            }) {
                packets.push(Box::new(ReceiverEstimatedMaximumBitrate {
                    sender_ssrc,
                    bitrate: remb_bps,
                    ssrcs: vec![ssrc],
                }));
                last_remb = Some(now);
            }
            if !packets.is_empty() && feedback_pc.write_rtcp(&packets).await.is_err() {
                break;
            }
        }
    });
    let wait_ms = std::env::var("UU_JITTER_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(60)
        .clamp(10, 200);
    let mut repair_rx = repairs.subscribe(track.ssrc());
    let nack_sender = u32::from_be_bytes(uuid::Uuid::new_v4().as_bytes()[..4].try_into().unwrap());
    let mut last_nack = Instant::now();
    let mut receiver = VideoReceiver::new(Duration::from_millis(wait_ms));
    let mut ticker = tokio::time::interval(Duration::from_millis(5));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let strict = !std::env::var("UU_STRICT_RECOVERY").is_ok_and(|v| v == "0");
    let recover = |hard: bool| {
        if hard {
            generation.fetch_add(1, Ordering::AcqRel);
        }
        request_generation.request();
        refresh.notify_one();
    };
    let mut receiver_epoch = 0;
    let mut test_loss = crate::test_loss::TestLoss::from_env();
    let mut bytes = 0u64;
    let mut previous_received = 0u64;
    let mut previous_lost = 0u64;
    let mut src = 0u64;
    let mut last_ts = None;
    let mut window = Instant::now();
    loop {
        let packet = tokio::select! {
            result=track.read_rtp()=>{
                let Ok((packet,_))=result else {break};
                Some(packet)
            }
            Some(packet)=repair_rx.recv()=>Some(packet),
            _=ticker.tick()=>None,
        };
        let now = Instant::now();
        if packet.as_ref().is_some_and(|p| test_loss.drop_packet(p.header.sequence_number, now)) {
            if let Some(s) = &stats { s.injected_loss.fetch_add(1, Ordering::Relaxed); }
            continue;
        }
        let units = if let Some(packet) = packet {
            if let Some(s) = &stats {
                s.packets.fetch_add(1, Ordering::Relaxed);
            }
            bytes += packet.payload.len() as u64;
            if last_ts.map_or(true, |last| {
                packet.header.timestamp.wrapping_sub(last) as i32 > 0
            }) {
                last_ts = Some(packet.header.timestamp);
                src += 1;
            }
            receiver.push(packet, now)
        } else {
            receiver.poll(now)
        };
        if last_nack.elapsed() >= Duration::from_millis(20) {
            if let Some(s) = &stats {
                let measured_ms = (s.rtt_us.load(Ordering::Relaxed) as u64
                    + 4 * s.rtt_deviation_us.load(Ordering::Relaxed) as u64) / 1000 + 20;
                receiver.set_repair_wait(Duration::from_millis(wait_ms.max(measured_ms).min(200)));
            }
            let missing = receiver.missing(now);
            if !missing.is_empty() {
                use webrtc::rtcp::transport_feedbacks::transport_layer_nack::{
                    nack_pairs_from_sequence_numbers, TransportLayerNack,
                };
                let nack = TransportLayerNack {
                    sender_ssrc: nack_sender,
                    media_ssrc: track.ssrc(),
                    nacks: nack_pairs_from_sequence_numbers(&missing),
                };
                if pc.write_rtcp(&[Box::new(nack)]).await.is_ok() {
                    if let Some(s) = &stats {
                        s.nack_requests
                            .fetch_add(missing.len() as u64, Ordering::Relaxed);
                    }
                }
            }
            last_nack = now;
        }
        for unit in units {
            if unit.epoch != receiver_epoch {
                receiver_epoch = unit.epoch;
                // With balanced recovery keep decoding intact reference
                // pictures while requesting refresh. Strict IDR-only recovery
                // can freeze indefinitely when large keyframes also lose a packet.
                recover(strict);
            }
            let epoch = generation.load(Ordering::Acquire);
            if sample_tx
                .try_send((
                    unit.data,
                    unit.timestamp,
                    epoch,
                    request_generation.requested(),
                ))
                .is_err()
            {
                recover(true);
                if let Some(s) = &stats {
                    s.queue_drops.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        if receiver.epoch != receiver_epoch {
            receiver_epoch = receiver.epoch;
            recover(strict);
        }
        let _ = receiver.take_refresh();
        if let Some(s) = &stats {
            s.lost.store(receiver.stats.lost, Ordering::Relaxed);
            s.reordered
                .store(receiver.stats.reordered, Ordering::Relaxed);
            s.late_packets.store(receiver.stats.late, Ordering::Relaxed);
            s.damaged_frames
                .store(receiver.stats.damaged, Ordering::Relaxed);
        }
        if window.elapsed() >= Duration::from_secs(2) {
            if let Some(s) = &stats {
                let seconds = window.elapsed().as_secs_f64();
                let lost = receiver.stats.lost.saturating_sub(previous_lost);
                let window_packets = receiver.stats.received.saturating_sub(previous_received);
                s.loss_ppm.store((lost.saturating_mul(1_000_000) / (window_packets + lost).max(1)) as u32, Ordering::Relaxed);
                s.src_fps_x100
                    .store((src as f64 / seconds * 100.0) as u32, Ordering::Relaxed);
                s.bitrate_kbps.store(
                    (bytes as f64 * 8.0 / seconds / 1000.0) as u32,
                    Ordering::Relaxed,
                );
                if debug {
                    println!(
                        "media: kbps={} lost={} bad={} nack={} rtx={} repair_drop={} waiting={}",
                        s.bitrate_kbps.load(Ordering::Relaxed),
                        receiver.stats.lost,
                        receiver.stats.damaged,
                        s.nack_requests.load(Ordering::Relaxed),
                        s.rtx_packets.load(Ordering::Relaxed),
                        s.rtx_drops.load(Ordering::Relaxed),
                        request_generation.is_waiting()
                    );
                }
            }
            src = 0;
            bytes = 0;
            previous_received = receiver.stats.received;
            previous_lost = receiver.stats.lost;
            window = Instant::now();
        }
    }
    repairs.unsubscribe(track.ssrc());
    feedback.abort();
    drop(sample_tx);
    let _ = worker.await;
}

/// Advertise only codecs whose payloads we can actually consume. The default
/// registry also advertises ULPFEC without a receive-side repair implementation.
fn register_receive_codecs(media: &mut MediaEngine) -> Result<()> {
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
