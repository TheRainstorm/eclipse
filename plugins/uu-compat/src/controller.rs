//! High-level controller session used by both the CLI and the GUI.
//!
//! Runs the recovered UU flow (room/join -> Socket.IO -> control -> WebRTC ->
//! CONTROL input) on a background thread and exposes frames, status and input
//! channels to the UI.
//!
//! Input is plain JSON sent on the `CONTROL_DATA_CHANNEL` data channel (see
//! `control_wire`); the GUI and CLI hand us finished JSON payloads.

use anyhow::{Context, Result};
use serde_json::json;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use crate::api::UuApi;
use crate::config::Config;
use crate::media::{ice_servers_from_json, ControllerMedia};
use crate::signal::SignalClient;
use crate::socketio::{gunzip_sdp04, gzip_sdp04, SocketIo};

pub enum Target { Device(String), Assistance(crate::assistance::Request) }
pub use uu_core::controller::{ControllerEvent,ControllerHandle};
pub fn spawn(config: Config, device: String, geo_ip: String) -> Result<ControllerHandle> {
    spawn_with_stats(config,device,geo_ip,std::sync::Arc::new(crate::media::MediaStats::default()))
}

pub fn spawn_with_stats(config:Config,device:String,geo_ip:String,stats:std::sync::Arc<crate::media::MediaStats>)->Result<ControllerHandle> {
    spawn_target(config,Target::Device(device),geo_ip,stats)
}

pub fn spawn_target(config:Config,target:Target,geo_ip:String,stats:std::sync::Arc<crate::media::MediaStats>)->Result<ControllerHandle> {
    crate::require_enabled()?;
    crate::install_crypto_provider();
    stats.capabilities.set(crate::capabilities::Capabilities::ALL);
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    let (frame_tx, frame_rx) = crate::media::frame_channel();
    let (input_tx, input_rx) = unbounded_channel();
    let (settings_tx, settings_rx) = unbounded_channel();
    let (close_tx, close_rx) = unbounded_channel();
    let stats_run = stats.clone();
    let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped_run = stopped.clone();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                let _ = ev_tx.send(ControllerEvent::Error(e.to_string()));
                stats_run.audio.stop();stats_run.clipboard.stopped.store(true,std::sync::atomic::Ordering::Release);
                stopped_run.store(true, std::sync::atomic::Ordering::Release);
                return;
            }
        };
        if let Err(e) = rt.block_on(run(
            config,
            target,
            geo_ip,
            ev_tx.clone(),
            frame_tx,
            input_rx,
            settings_rx,
            close_rx,
            stats_run,
        )) {
            let _ = ev_tx.send(ControllerEvent::Error(e.to_string()));
        }
        rt.shutdown_timeout(std::time::Duration::from_secs(1));
        stopped_run.store(true, std::sync::atomic::Ordering::Release);
    });
    Ok(ControllerHandle {
        stopped,
        events: ev_rx,
        frames: frame_rx,
        input: input_tx,
        native_input:None,
        settings: settings_tx,
        close: close_tx,
        stats,
    })
}

/// Feed one candidate carried in a `soac` event to the peer connection.
async fn add_trickle(
    media: &ControllerMedia,
    d: &serde_json::Value,
    ev: &std::sync::mpsc::Sender<ControllerEvent>,
) -> Result<()> {
    if let Some(c) = d.get("candidate") {
        let cand = c["candidate"].as_str().unwrap_or_default().to_string();
        let mid = c["sdpMid"].as_str().unwrap_or("0").to_string();
        let idx = c["sdpMLineIndex"].as_u64().unwrap_or(0) as u16;
        if cand.is_empty() {
            return Ok(());
        }
        let parts: Vec<&str> = cand.split_whitespace().collect();
        if parts.len() >= 6 && parts.get(6) == Some(&"host") {
            let _ = ev.send(ControllerEvent::Status(format!(
                "host candidate {}:{}",
                parts[4], parts[5]
            )));
        }
        media.add_candidate(cand, mid, idx).await?;
    }
    Ok(())
}

async fn run(
    config: Config,
    target: Target,
    geo_ip: String,
    ev: std::sync::mpsc::Sender<ControllerEvent>,
    frame_tx: crate::media::FrameSender,
    mut input_rx: UnboundedReceiver<Vec<u8>>,
    mut settings_rx: UnboundedReceiver<Vec<u8>>,
    mut close_rx: UnboundedReceiver<()>,
    stats: std::sync::Arc<crate::media::MediaStats>,
) -> Result<()> {
    let phase_started=std::time::Instant::now();
    let status = |s: &str| {
        stats.diagnostics.record(serde_json::json!({
            "event":"connection_phase","phase":s,
            "elapsed_ms":phase_started.elapsed().as_millis() as u64
        }));
        let _ = ev.send(ControllerEvent::Status(s.to_string()));
    };
    let ctrl = uuid::Uuid::new_v4().to_string();
    // Keep ownership outside the cancellable attempt: every exit path runs the
    // same cleanup, including failures and cancellation during negotiation.
    let mut socket = None;
    let mut peer = None;
    let attempt = async {
    let hevc = if stats.prefer_hevc.load(std::sync::atomic::Ordering::Acquire) {
        status(uu_i18n::text("正在检查 H.265 硬件解码…"));
        let probe = tokio::task::spawn_blocking(uu_media::hevc::probe_decoder).await;
        let result = match probe { Ok(result) => result.map_err(|e|e.to_string()), Err(e) => Err(e.to_string()) };
        stats.diagnostics.record(json!({"event":"hevc_decoder_probe","available":result.is_ok(),"reason":result.as_ref().err()}));
        if result.is_err() { status(uu_i18n::text("H.265 硬件解码不可用，使用 H.264")); }
        result.is_ok()
    } else { false };
    let _ = stats.hevc_enabled.set(hevc);
    status(uu_i18n::text("正在加入房间…"));
    let client = SignalClient::new(&config)?;
    let api = UuApi::new(client, config.at("state.json"))?;
    // A controller needs a fresh controlled-side media session. Reusing an
    // existing official session can start us halfway through an H264 GOP,
    // leaving the decoder without SPS/PPS and showing a frozen first frame.
    let force = std::env::var("UU_FORCE_JOIN")
        .map(|v| v != "0")
        .unwrap_or(true);
    let room = match &target {
        Target::Device(device)=>api.room_join(device,force).await?,
        Target::Assistance(request)=>api.room_join_assistance(request).await?,
    };
    let data = room.get("data").cloned().unwrap_or_default();
    let token = data["token"]
        .as_str()
        .context("room/join: missing token")?
        .to_string();
    let server = data["signaling_server"]
        .as_str()
        .context("room/join: missing signaling_server")?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let url = format!(
        "{}/socket.io/?EIO=4&transport=websocket&t={ts}",
        server.trim_end_matches('/')
    );
    status(uu_i18n::text("连接信令服务器…"));
    socket = Some(SocketIo::connect(&url, &token).await?);
    let sio = socket.as_mut().unwrap();
    let dev_id = api.state.device_id.clone().unwrap_or_default();
    let display_mode=if stats.display_mode.load(std::sync::atomic::Ordering::Acquire)==1 {crate::screen_control::DisplayMode::ExclusiveVirtual}else{crate::screen_control::DisplayMode::Preserve};
    let resolution = *stats.output_resolution.get_or_init(|| {
        if std::env::var_os("UU_RESOLUTION").is_some() { crate::config::resolution_from_env() }
        else { crate::control_wire::CaptureResolution::Unchanged }
    });
    let output = resolution.output_bounds();
    let app = crate::socketio::encode_app_data_with_codecs(&dev_id,stats.clipboard.enabled.load(std::sync::atomic::Ordering::Acquire),display_mode,output,hevc);
    stats.diagnostics.record(json!({"event":"output_negotiation","width":output.0,"height":output.1,"hevc":hevc}));
    let codecs: Vec<_> = crate::socketio::decoder_caps(output, hevc).into_iter().map(|(codec,width,height)|json!({
        "video_codec":codec,"width":width,"height":height,
        "chroma_sampling":1,"bit_depth":8,"codec_impl":32
    })).collect();
    let streamer = json!({
        "control_id": ctrl,
        "device_capability": {
            "display_info": [{"id":0,"fps":crate::control_wire::CaptureFrameRate::MAX,"type":0,"hdr":-2}],
            "video_codec_capability": codecs,
            "ice_id": ""
        }
    })
    .to_string();
    sio.emit(
        "control",
        json!({
            "app_control_id": ctrl,
            "app_data": {"_placeholder": true, "num": 0},
            "geo_exit_ip": geo_ip,
            "streamer_data": streamer,
        }),
        &[app],
    )
    .await?;

    status(uu_i18n::text("等待被控端…"));
    let mut success = None;
    let mut forwarded = 0;
    while success.is_none() || forwarded < 2 {
        let (event, data, _) = sio.recv_event().await?;
        match event.as_str() {
            "success" => success = Some(data),
            "forward_setting" => forwarded += 1,
            "fail" => anyhow::bail!("control failed: {data}"),
            _ => {}
        }
    }
    let success = success.context("missing success event")?;
    status(uu_i18n::text("收到被控端参数"));
    eprintln!("ICE server entries={}", success["iceServers"].as_array().map_or(0,Vec::len));
    let ice = ice_servers_from_json(&success["iceServers"]);
    stats.diagnostics.record(serde_json::json!({"event":"ice_servers","count":ice.len()}));
    status(uu_i18n::text("建立 WebRTC…"));
    peer = Some(ControllerMedia::new_with_frames(ice, Some(frame_tx), Some(stats.clone())).await?);
    let media = peer.as_ref().unwrap();
    let ice_id = success["ice_id"].as_str().unwrap_or_default().to_string();
    let target = success["client_id"].as_str().unwrap_or_default().to_string();
    let mut local_ice = offer_and_answer(sio, media, &ctrl, &target, &ice_id, &ev, &status).await?;
    status(uu_i18n::text("等待 WebRTC 控制通道…"));
    // Ask the controlled end for the target frame rate. Without this the host
    // streams at its idle default (~21 fps); the official client sends the same
    // CaptureSettingRequest. Retry a few times while CONTROL settles.
    let frame_rate = crate::control_wire::CaptureFrameRate::from_env();
    let bitrate_mbps: Option<u64> = std::env::var("UU_BITRATE")
        .ok()
        .and_then(|v| v.parse().ok());
    // Quality 6 is the custom-bitrate preset; the official host only honours
    // field 8 when it is paired with quality 6.
    let quality: u64 = std::env::var("UU_QUALITY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(if bitrate_mbps.is_some() { 6 } else { 5 });
    let custom_bps = if quality == 6 {
        Some(bitrate_mbps.unwrap_or(30).saturating_mul(1_000_000))
    } else {
        None
    };
    // Do not derive a display-layout operation from an automatic source or a
    // FOLLOW_REMOTE resolution request. Preserve the host's current capture.
    let initial_settings = crate::control_wire::capture_setting_for_screen(
        1000, crate::control_wire::now_ms(), frame_rate, quality, custom_bps,
        resolution,false,-1,
    );
    drive_session(sio, media, &ctrl, &ice_id, &ev, &mut input_rx, &mut settings_rx, initial_settings, &target, &mut local_ice).await?;
    Ok::<_, anyhow::Error>(())
    };
    let result = tokio::select! {
        biased;
        _ = close_rx.recv() => Ok(()),
        result = attempt => result,
    };
    stats.audio.stop();
    stats.clipboard.stopped.store(true,std::sync::atomic::Ordering::Release);
    stats.clipboard.active.store(false,std::sync::atomic::Ordering::Release);
    status(uu_i18n::text("正在释放连接…"));
    // Flush pending key/button releases before tearing down CONTROL. Bound
    // every cleanup operation so a dead network cannot trap the UI on exit.
    if let Some(media) = &peer {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(250), async {
            while let Ok(frame) = input_rx.try_recv() {
                let _ = media.send_input(frame).await;
            }
        }).await;
    }
    if stats.exclusive_owned.load(std::sync::atomic::Ordering::Acquire) {
        if let Some(media)=&peer {
            let _=tokio::time::timeout(std::time::Duration::from_millis(500),media.send_control(crate::screen_control::quit_exclusive(901))).await;
        }
    }
    close_session(socket.as_mut(), peer.as_ref(), &ctrl).await;
    status(uu_i18n::text("已断开"));
    result
}

type LocalIceCandidates = UnboundedReceiver<Option<webrtc::ice_transport::ice_candidate::RTCIceCandidateInit>>;

/// Offer first; candidate callbacks are queued until Socket.IO has sent it.
/// Keep receiving remote candidates while waiting for the answer. The ICE API
/// requires a remote description, so hold early candidates until it is set.
async fn offer_and_answer(
    sio: &mut SocketIo, media: &ControllerMedia, ctrl: &str, target: &str,
    ice_id: &str, ev: &std::sync::mpsc::Sender<ControllerEvent>, status: &impl Fn(&str),
) -> Result<LocalIceCandidates> {
    let (offer, mut local_ice) = media.trickle_offer().await?;
    sio.emit("soac", json!({"client_id": target, "data": {
        "app_control_id": ctrl, "gzip_sdp": {"_placeholder": true, "num": 0},
        "ice_id": ice_id, "ice_network_type": 0, "sdp": "", "type": "offer"
    }}), &[gzip_sdp04(&offer)?]).await?;
    status(uu_i18n::text("已发送SDP offer"));
    status(uu_i18n::text("等待 SDP answer…"));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(45);
    let mut early_candidates = Vec::new();
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => anyhow::bail!("signalling timeout: no SDP answer"),
            Some(candidate) = local_ice.recv() => {
                send_local_candidate(sio, candidate, ctrl, target, ice_id, status).await?;
            }
            received = sio.recv_event() => {
                let (event, data, attachments) = received?;
                if event == "released" || event == "fail" { anyhow::bail!("被控端结束连接"); }
                if event != "soac" { continue; }
                let body = &data["data"];
                match body["type"].as_str() {
                    Some("answer") if !attachments.is_empty() => {
                        media.set_answer(&gunzip_sdp04(&attachments[0])?).await?;
                        for candidate in early_candidates { add_trickle(media, &candidate, ev).await?; }
                        status("收到SDP answer");
                        return Ok(local_ice);
                    }
                    Some("candidate") => {
                        anyhow::ensure!(early_candidates.len() < 256, "too many ICE candidates before answer");
                        early_candidates.push(body.clone());
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn send_local_candidate(
    sio: &mut SocketIo, candidate: Option<webrtc::ice_transport::ice_candidate::RTCIceCandidateInit>,
    ctrl: &str, target: &str, ice_id: &str, status: &impl Fn(&str),
) -> Result<()> {
    if let Some(candidate) = candidate {
        sio.emit("soac", json!({"client_id": target, "data": {
            "app_control_id": ctrl, "ice_id": ice_id, "type": "candidate", "candidate": candidate
        }}), &[]).await?;
    } else {
        // UU's candidate event has no negotiated end-of-candidates extension.
        // Completion is local progress only; do not invent a wire message.
        status(uu_i18n::text("ICE收集完成"));
    }
    Ok(())
}

/// Keep consuming trickle candidates while ICE/DTLS/SCTP are connecting.
/// Capture settings are a deferred operation, never a prerequisite for driving
/// signalling: otherwise a slow connection deadlocks waiting for its own ICE.
async fn drive_session(
    sio: &mut SocketIo,
    media: &ControllerMedia,
    ctrl: &str,
    ice_id: &str,
    ev: &std::sync::mpsc::Sender<ControllerEvent>,
    input_rx: &mut UnboundedReceiver<Vec<u8>>,
    settings_rx: &mut UnboundedReceiver<Vec<u8>>,
    initial_settings: Vec<u8>,
    target: &str,
    local_ice: &mut LocalIceCandidates,
) -> Result<()> {
    use std::time::Duration;
    use webrtc::data_channel::data_channel_state::RTCDataChannelState;
    use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
    let status = |message: &str| { let _ = ev.send(ControllerEvent::Status(message.into())); };
    let started = tokio::time::Instant::now();
    let mut opened_at = None;
    let mut pending_initial = true;
    let exclusive=media.wants_exclusive_display();
    let mut policy_done=!exclusive;
    let mut policy_sent=None;
    let mut policy_started=None;
    let mut pending_setting: Option<Vec<u8>> = None;
    let mut last_attempt = None;
    let mut last_transport = String::new();
    let mut setup_timer = tokio::time::interval(Duration::from_millis(250));
    setup_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut probe_timer = tokio::time::interval(Duration::from_secs(1));
    // Official CONTROL captures include this independent of SimpleAction RTT.
    let mut input_heartbeat = tokio::time::interval(Duration::from_millis(200));
    input_heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    probe_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            Some(candidate) = local_ice.recv() => {
                send_local_candidate(sio, candidate, ctrl, target, ice_id, &status).await?;
            }
            _ = setup_timer.tick() => {
                media.trace_transport(&mut last_transport);
                let now = tokio::time::Instant::now();
                match media.pc.connection_state() {
                    RTCPeerConnectionState::Failed => anyhow::bail!("WebRTC 连接失败（ICE/DTLS）"),
                    RTCPeerConnectionState::Closed => break,
                    _ => {}
                }
                if media.data_channel.ready_state() == RTCDataChannelState::Open {
                    if opened_at.is_none() {
                        opened_at = Some(now);
                        status("已连接");
                        sio.emit("ice_finished_log", json!({"app_control_id":ctrl,"ice_id":ice_id}), &[]).await?;
                    }
                    // Allow the host's echo/initialization exchange to settle.
                    // These timers never block receiving trickle candidates.
                    if now.duration_since(opened_at.unwrap()) >= Duration::from_millis(1500)
                        && last_attempt.map_or(true, |at| now.duration_since(at) >= Duration::from_millis(500))
                    {
                        last_attempt = Some(now);
                        if !policy_done {
                            let waiting_since=*policy_started.get_or_insert(now);
                            if policy_sent.is_none() {
                                if media.has_super_screen() {policy_done=true;}
                                else if let Some(request)=media.exclusive_request(900) {
                                    if media.send_control(request).await.is_ok() {policy_sent=Some(now);status("正在请求UU虚拟屏独占…");}
                                }
                                if !policy_done && policy_sent.is_none() && now.duration_since(waiting_since)>=Duration::from_secs(8) {
                                    policy_done=true;status("无法确认远端显示器参数，未申请独占");
                                }
                            }
                            if let Some(sent)=policy_sent {
                                match media.exclusive_result() {
                                    1=>{policy_done=true;status("远端已确认UU虚拟屏独占");},
                                    2=>{policy_done=true;status("远端拒绝显示器独占，保留当前布局");},
                                    _ if now.duration_since(sent)>=Duration::from_secs(8)=>{policy_done=true;status("显示器独占请求未确认，保留当前采集");},
                                    _=>{}
                                }
                            }
                            if !policy_done {continue;}
                        }
                        if pending_initial && media.send_control(media.bind_initial_capture(&initial_settings)).await.is_ok() {
                            pending_initial = false;
                            status("已请求目标画质/码率");
                        }
                        if !pending_initial {
                            if let Some(msg) = pending_setting.take() {
                                if media.send_control(msg.clone()).await.is_err() { pending_setting = Some(msg); }
                            }
                        }
                    }
                } else if opened_at.is_none() && now.duration_since(started) >= Duration::from_secs(45) {
                    let sctp = media.pc.sctp();
                    anyhow::bail!("输入控制通道未建立（45秒）：ICE={}，DTLS={}，SCTP={}，CONTROL={}。视频与输入分别建立，请重新连接。",
                        media.pc.ice_connection_state(), sctp.transport().state(), sctp.state(), media.data_channel.ready_state());
                }
            }
            _ = probe_timer.tick(), if opened_at.is_some() => media.probe_network().await,
            _ = input_heartbeat.tick(), if opened_at.is_some() => {
                let _ = media.send_control(br#"{"heartbeat":"kbd_heartbeat"}"#.to_vec()).await;
            }
            Some(frame) = input_rx.recv(), if opened_at.is_some() => {
                if media.send_input(frame).await.is_err() {
                    status("输入通道发送失败，请重新连接");
                }
            }
            Some(msg) = settings_rx.recv() => {
                pending_setting = Some(msg); // Keep the newest selection until CONTROL is ready.
            }
            recv = sio.recv_event() => {
                let (event, data, _) = recv?;
                if event == "released" { break; }
                if event == "soac" && data["data"]["type"] == "candidate" {
                    add_trickle(media, &data["data"], ev).await?;
                }
            }
        }
    }
    Ok(())
}

/// The same finalizer runs after cancellation, errors and a host release.
async fn close_session(mut socket: Option<&mut SocketIo>, peer: Option<&ControllerMedia>, ctrl: &str) {
    if let Some(sio) = socket.as_deref_mut() {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1),
            sio.emit("release", json!({"app_control_id": ctrl, "control_id": ctrl}), &[])).await;
    }
    if let Some(media) = peer {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), media.pc.close()).await;
    }
    if let Some(sio) = socket {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), sio.close()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    #[tokio::test]
    async fn partial_session_cleanup_releases_the_correct_control_before_socket_disconnect() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            ws.send(Message::Text("0{}".into())).await.unwrap();
            assert_eq!(ws.next().await.unwrap().unwrap().into_text().unwrap(), "40");
            ws.send(Message::Text("40{}".into())).await.unwrap();
            let release = ws.next().await.unwrap().unwrap().into_text().unwrap();
            let event: serde_json::Value = serde_json::from_str(&release[2..]).unwrap();
            assert_eq!(event, json!(["release", {"app_control_id":"cancelled-attempt", "control_id":"cancelled-attempt"}]));
            assert_eq!(ws.next().await.unwrap().unwrap().into_text().unwrap(), "41");
            assert!(matches!(ws.next().await.unwrap().unwrap(), Message::Close(_)));
        });
        let mut sio = SocketIo::connect(&format!("ws://{addr}"), "test").await.unwrap();
        // No peer yet: cancellation while waiting for the host must still release.
        close_session(Some(&mut sio), None, "cancelled-attempt").await;
        server.await.unwrap();
        close_session(None, None, "never-connected").await;
    }
}

#[cfg(test)]
#[path = "controller_startup_tests.rs"]
mod startup_tests;
