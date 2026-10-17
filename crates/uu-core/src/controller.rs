//! High-level controller session used by both the CLI and the GUI.
//!
//! Runs the recovered UU flow (room/join -> Socket.IO -> control -> WebRTC ->
//! PCK input) on a background thread and exposes frames, status and input
//! channels to the UI.

use anyhow::{Context, Result};
use serde_json::json;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::api::UuApi;
use crate::config::Config;
use crate::media::{ice_servers_from_json, ControllerMedia};
use crate::signal::SignalClient;
use crate::socketio::{encode_app_data, gunzip_sdp04, gzip_sdp04, SocketIo};

pub enum ControllerEvent {
    Status(String),
    Error(String),
}

pub struct ControllerHandle {
    pub events: std::sync::mpsc::Receiver<ControllerEvent>,
    pub frames: UnboundedReceiver<(u32, u32, u32, Vec<u8>)>,
    pub input: UnboundedSender<Vec<u8>>,
    pub close: UnboundedSender<()>,
}

pub fn spawn(config: Config, device: String, geo_ip: String) -> Result<ControllerHandle> {
    crate::install_crypto_provider();
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    let (frame_tx, frame_rx) = unbounded_channel();
    let (input_tx, input_rx) = unbounded_channel();
    let (close_tx, close_rx) = unbounded_channel();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                let _ = ev_tx.send(ControllerEvent::Error(e.to_string()));
                return;
            }
        };
        if let Err(e) = rt.block_on(run(
            config,
            device,
            geo_ip,
            ev_tx.clone(),
            frame_tx,
            input_rx,
            close_rx,
        )) {
            let _ = ev_tx.send(ControllerEvent::Error(e.to_string()));
        }
    });
    Ok(ControllerHandle {
        events: ev_rx,
        frames: frame_rx,
        input: input_tx,
        close: close_tx,
    })
}

async fn pck_session<R, W>(
    mut rd: R,
    mut wr: W,
    ev: std::sync::mpsc::Sender<ControllerEvent>,
    ready: Arc<AtomicBool>,
    mut pck_rx: UnboundedReceiver<Vec<u8>>,
    label: &'static str,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    ready.store(true, Ordering::Relaxed);
    let framing = std::env::var("UU_PCK_FRAME").unwrap_or_default();
    let _ = ev.send(ControllerEvent::Status(format!(
        "键盘通道已就绪 ({label}, frame={})",
        if framing.is_empty() { "bare" } else { &framing }
    )));
    let wrap = |f: Vec<u8>| -> Vec<u8> {
        match framing.as_str() {
            "be16" => {
                let mut v = Vec::with_capacity(f.len() + 2);
                v.extend_from_slice(&(f.len() as u16).to_be_bytes());
                v.extend_from_slice(&f);
                v
            }
            "le16" => {
                let mut v = Vec::with_capacity(f.len() + 2);
                v.extend_from_slice(&(f.len() as u16).to_le_bytes());
                v.extend_from_slice(&f);
                v
            }
            _ => f,
        }
    };
    let hello = hex::decode("1a18221608061003180320022801300238024003480150015801").unwrap();
    let _ = wr.write_all(&wrap(pck_heartbeat(1))).await;
    let f = uu_proto::pck::encode_control_ch(&hello, 2, 1, 0x11223344, 0);
    let _ = wr.write_all(&wrap(f)).await;

    let evr = ev.clone();
    tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        loop {
            match tokio::io::AsyncReadExt::read(&mut rd, &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if n >= 28 && &buf[..3] == b"PCK" {
                        let k = u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
                        if k == 0 || k == 3 || k == 8 {
                            let _ = evr.send(ControllerEvent::Status(format!(
                                "PCK 收到 kind={k} len={n}"
                            )));
                        }
                    } else {
                        let _ = evr.send(ControllerEvent::Status(format!(
                            "PCK 原始回包 len={n} hex={}",
                            hex::encode(&buf[..n.min(32)])
                        )));
                    }
                }
            }
        }
    });

    let mut seq = 2u32;
    let hb = json!({"heartbeat":"kbd_heartbeat"}).to_string();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(200));
    loop {
        tokio::select! {
            Some(frame) = pck_rx.recv() => {
                if wr.write_all(&wrap(frame)).await.is_err() { break; }
            }
            _ = tick.tick() => {
                let ts = 0x1122_0000u32.wrapping_add(seq);
                let f = uu_proto::pck::encode_control_ch(hb.as_bytes(), 8, seq, ts, 0);
                seq = seq.wrapping_add(1);
                if wr.write_all(&wrap(f)).await.is_err() { break; }
            }
        }
    }
    ready.store(false, Ordering::Relaxed);
}

async fn turn_pck_session(
    mut turn: crate::turn::TurnClient,
    ev: std::sync::mpsc::Sender<ControllerEvent>,
    ready: Arc<AtomicBool>,
    mut pck_rx: UnboundedReceiver<Vec<u8>>,
) {
    ready.store(true, Ordering::Relaxed);
    let _ = ev.send(ControllerEvent::Status("键盘通道已就绪 (TURN/PCK)".into()));
    let capability = hex::decode("1a18221608061003180320022801300238024003480150015801").unwrap();
    if let Err(e) = turn.send_to_permission(&pck_heartbeat(1)).await {
        let _ = ev.send(ControllerEvent::Status(format!(
            "TURN heartbeat 发送失败: {e:#}"
        )));
        ready.store(false, Ordering::Relaxed);
        return;
    }
    let capability = uu_proto::pck::encode_control_ch(&capability, 2, 1, 0x11223344, 0);
    if let Err(e) = turn.send_to_permission(&capability).await {
        let _ = ev.send(ControllerEvent::Status(format!(
            "TURN capability 发送失败: {e:#}"
        )));
        ready.store(false, Ordering::Relaxed);
        return;
    }
    let hb = json!({"heartbeat":"kbd_heartbeat"}).to_string();
    let mut seq = 2u32;
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(200));
    loop {
        tokio::select! {
            Some(frame) = pck_rx.recv() => {
                if let Err(e) = turn.send_to_permission(&frame).await {
                    let _ = ev.send(ControllerEvent::Status(format!("TURN PCK 发送失败: {e:#}")));
                    break;
                }
            }
            _ = tick.tick() => {
                let frame = uu_proto::pck::encode_control_ch(hb.as_bytes(), 8, seq, 0x1122_0000u32.wrapping_add(seq), 0);
                seq = seq.wrapping_add(1);
                if let Err(e) = turn.send_to_permission(&frame).await {
                    let _ = ev.send(ControllerEvent::Status(format!("TURN heartbeat 发送失败: {e:#}")));
                    break;
                }
            }
            incoming = turn.recv() => {
                match incoming {
                    Ok(Some((peer, data))) => {
                        if let Ok(frame) = crate::pck::Frame::decode(&data) {
                            let _ = ev.send(ControllerEvent::Status(format!("TURN 收到 PCK peer={peer} kind={} len={}", frame.kind, frame.payload.len())));
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        let _ = ev.send(ControllerEvent::Status(format!("TURN 接收失败: {e:#}")));
                        break;
                    }
                }
            }
        }
    }
    ready.store(false, Ordering::Relaxed);
}

/// Strip the PCK framing from a locally-built input frame, returning the JSON
/// payload the controlled end expects on CONTROL_DATA_CHANNEL.
fn pck_payload_json(frame: &[u8]) -> Option<Vec<u8>> {
    const TRAILER: [u8; 4] = [0x03, 0x00, 0x01, 0x00];
    let f = crate::pck::Frame::decode(frame).ok()?;
    let mut payload = f.payload;
    if payload.len() >= 4 && payload[payload.len() - 4..] == TRAILER {
        payload.truncate(payload.len() - 4);
    }
    Some(payload)
}

fn pck_heartbeat(seq: u32) -> Vec<u8> {
    let mut out = vec![0u8; 37];
    out[..3].copy_from_slice(b"PCK");
    out[3] = 0x02;
    out[4..6].copy_from_slice(b"V3");
    out[6..8].copy_from_slice(&[0x02, 0x00]);
    out[8] = 0x57;
    out[9..13].copy_from_slice(&seq.to_be_bytes());
    out
}

async fn run(
    config: Config,
    device: String,
    geo_ip: String,
    ev: std::sync::mpsc::Sender<ControllerEvent>,
    frame_tx: crate::media::FrameSender,
    mut input_rx: UnboundedReceiver<Vec<u8>>,
    mut close_rx: UnboundedReceiver<()>,
) -> Result<()> {
    let status = |s: &str| {
        let _ = ev.send(ControllerEvent::Status(s.to_string()));
    };
    let (_pck_tx, pck_rx) = unbounded_channel::<Vec<u8>>();
    let pck_ready = Arc::new(AtomicBool::new(false));
    let (cand_tx, cand_rx) = tokio::sync::watch::channel(Vec::<SocketAddr>::new());
    let (peer_tx, peer_rx) = tokio::sync::watch::channel(Vec::<SocketAddr>::new());
    status("正在加入房间…");
    let client = SignalClient::new(&config)?;
    let api = UuApi::new(client, config.at("state.json"));
    // A controller needs a fresh controlled-side media session. Reusing an
    // existing official session can start us halfway through an H264 GOP,
    // leaving the decoder without SPS/PPS and showing a frozen first frame.
    let force = std::env::var("UU_FORCE_JOIN")
        .map(|v| v != "0")
        .unwrap_or(true);
    let room = api.room_join(&device, force).await?;
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
    status("连接信令服务器…");
    let mut sio = SocketIo::connect(&url, &token).await?;

    let ctrl = uuid::Uuid::new_v4().to_string();
    let dev_id = api.state.device_id.clone().unwrap_or_default();
    let app = encode_app_data(&dev_id);
    let streamer = json!({
        "control_id": ctrl,
        "device_capability": {
            "display_info": [{"id":0,"fps":60,"type":0,"hdr":-2}],
            "video_codec_capability": [{
                "video_codec":1,"width":1920,"height":1080,
                "chroma_sampling":1,"bit_depth":8,"codec_impl":32
            }],
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

    status("等待被控端…");
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
    eprintln!("iceServers={}", success["iceServers"]);
    let ice = ice_servers_from_json(&success["iceServers"]);
    let turn_servers = crate::turn::servers_from_json(&success["iceServers"]);
    status("建立 WebRTC…");
    let media = ControllerMedia::new_with_frames(ice, Some(frame_tx)).await?;
    let offer_sdp = media.offer().await?;
    let _ = std::fs::write("/tmp/offer_rust.sdp", &offer_sdp);
    let gz = gzip_sdp04(&offer_sdp)?;
    let ice_id = success["ice_id"].as_str().unwrap_or_default().to_string();
    let target = success["client_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    sio.emit(
        "soac",
        json!({
            "client_id": target,
            "data": {
                "app_control_id": ctrl,
                "gzip_sdp": {"_placeholder": true, "num": 0},
                "ice_id": ice_id,
                "ice_network_type": 0,
                "sdp": "",
                "type": "offer"
            }
        }),
        &[gz],
    )
    .await?;

    status("等待 SDP answer…");
    let mut answered = false;
    let mut transport: Option<crate::sdp::IceTransport> = None;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(45);
    while tokio::time::Instant::now() < deadline && !answered {
        let (event, data, atts) =
            tokio::time::timeout(std::time::Duration::from_secs(10), sio.recv_event())
                .await
                .context("signalling timeout")??;
        if event != "soac" {
            continue;
        }
        let d = &data["data"];
        match d["type"].as_str() {
            Some("answer") if !atts.is_empty() => {
                let answer = gunzip_sdp04(&atts[0])?;
                let _ = std::fs::write("/tmp/answer_live.sdp", &answer);
                transport = crate::sdp::parse_answer(&offer_sdp, &answer);
                if let Some(t) = &transport {
                    let _ = peer_tx.send(t.udp_candidates.clone());
                }
                media.set_answer(&answer).await?;
                answered = true;
            }
            Some("candidate") => {
                if let Some(c) = d.get("candidate") {
                    let cand = c["candidate"].as_str().unwrap_or_default().to_string();
                    let mid = c["sdpMid"].as_str().unwrap_or("0").to_string();
                    let idx = c["sdpMLineIndex"].as_u64().unwrap_or(0) as u16;
                    if let Some(addr) = crate::sdp::parse_tcp_candidate(&cand) {
                        let mut v = cand_tx.borrow().clone();
                        if !v.contains(&addr) {
                            v.push(addr);
                            let _ = cand_tx.send(v);
                        }
                    }
                    if let Some(addr) = crate::sdp::parse_relay_candidate(&cand) {
                        let mut v = peer_tx.borrow().clone();
                        if !v.contains(&addr) {
                            v.push(addr);
                            let _ = peer_tx.send(v);
                        }
                    }
                    if let Some(addr) = crate::sdp::parse_udp_candidate(&cand) {
                        let mut v = peer_tx.borrow().clone();
                        if !v.contains(&addr) {
                            v.push(addr);
                            let _ = peer_tx.send(v);
                        }
                    }
                    if !cand.is_empty() {
                        eprintln!("trickle {cand}");
                        let parts: Vec<&str> = cand.split_whitespace().collect();
                        if parts.len() >= 6 && parts.get(6) == Some(&"host") {
                            let _ = ev.send(ControllerEvent::Status(format!(
                                "host candidate {}:{}",
                                parts[4], parts[5]
                            )));
                        }
                        media.add_candidate(cand, mid, idx).await?;
                    }
                }
            }
            _ => {}
        }
    }
    anyhow::ensure!(answered, "no SDP answer");
    if let Some(t) = &transport {
        eprintln!(
            "ICE creds local={} remote={} pwd={} tcp={:?} udp={:?}",
            t.local_ufrag, t.remote_ufrag, t.remote_pwd, t.tcp_candidates, t.udp_candidates
        );
        let mut v = cand_tx.borrow().clone();
        for a in &t.tcp_candidates {
            if !v.contains(a) {
                v.push(*a);
            }
        }
        let _ = cand_tx.send(v);
        let mut peers = peer_tx.borrow().clone();
        for a in &t.udp_candidates {
            if !peers.contains(a) {
                peers.push(*a);
            }
        }
        let _ = peer_tx.send(peers);
    }
    status("已连接");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    sio.emit(
        "ice_finished_log",
        json!({"app_control_id": ctrl, "ice_id": ice_id}),
        &[],
    )
    .await?;

    // The controlled end drives its own CONTROL initialization once the
    // CONTROL_DATA_CHANNEL is open; no client-side capability hello is needed.

    if let Some(t) = transport.clone() {
        let ev2 = ev.clone();
        let ready2 = pck_ready.clone();
        let cand_rx2 = cand_rx.clone();
        let mut peer_rx2 = peer_rx.clone();
        let mut pck_rx2 = Some(pck_rx);
        let mode = std::env::var("UU_PCK_MODE").unwrap_or_default();
        if mode == "off" {
            let _ = ev2.send(ControllerEvent::Status("键盘通道已就绪 (dc)".into()));
        } else {
            tokio::spawn(async move {
                let creds = crate::stun::IceCredentials {
                    remote_ufrag: t.remote_ufrag.clone(),
                    remote_pwd: t.remote_pwd.clone(),
                    local_ufrag: t.local_ufrag.clone(),
                    local_pwd: t.local_pwd.clone(),
                };
                // The default path is the recovered official transport: TURN over
                // TLS, with the peer's relay candidate as the permission target.
                // Keep the old ICE-TCP probes available through UU_PCK_MODE for
                // diagnostics and as a fallback when a session has no relay.
                if (mode.is_empty() || mode == "turn" || mode == "turntls")
                    && !turn_servers.is_empty()
                {
                    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
                    // Trickle candidates arrive shortly after the SDP
                    // answer. Give the signaling loop a moment to collect
                    // the srflx and relay addresses before allocating.
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    loop {
                        let peers = peer_rx2.borrow().clone();
                        if !peers.is_empty() {
                            for server in &turn_servers {
                                let _ = ev2.send(ControllerEvent::Status(format!(
                                    "尝试 TURN/PCK {} -> {} 个候选",
                                    server.host,
                                    peers.len()
                                )));
                                let result = tokio::time::timeout(
                                    std::time::Duration::from_secs(10),
                                    async {
                                        let mut turn =
                                            crate::turn::TurnClient::connect(server.clone())
                                                .await?;
                                        let allocation = turn.allocate().await?;
                                        for peer in &peers {
                                            turn.create_permission(*peer).await?;
                                        }
                                        // Complete the ICE check through the
                                        // TURN allocation before sending PCK.
                                        let u = uuid::Uuid::new_v4();
                                        let mut txid = [0u8; 12];
                                        txid.copy_from_slice(&u.as_bytes()[..12]);
                                        let tiebreaker = u64::from_be_bytes(
                                            uuid::Uuid::new_v4().as_bytes()[..8]
                                                .try_into()
                                                .unwrap(),
                                        );
                                        let check = crate::stun::binding_request(
                                            &creds,
                                            0x6e00_20ff,
                                            true,
                                            tiebreaker,
                                            txid,
                                        );
                                        turn.send_to_permission(&check[2..]).await?;
                                        Ok::<_, anyhow::Error>((turn, allocation))
                                    },
                                )
                                .await;
                                match result {
                                    Ok(Ok((turn, allocation))) => {
                                        let _ = ev2.send(ControllerEvent::Status(format!(
                                            "TURN Allocate 成功 relay={:?} lifetime={:?}",
                                            allocation.relayed_addr, allocation.lifetime
                                        )));
                                        if mode == "turntls" {
                                            let raw = tokio::time::timeout(
                                                std::time::Duration::from_secs(8),
                                                crate::pck::PckTls::connect_with_alpn(
                                                    &server.host,
                                                    server.port,
                                                    &server.host,
                                                    None,
                                                ),
                                            )
                                            .await;
                                            match raw {
                                                Ok(Ok(tls)) => {
                                                    let _ = ev2.send(ControllerEvent::Status(
                                                        "TURN 原始 TLS/PCK 已连接".into(),
                                                    ));
                                                    tokio::spawn(async move {
                                                        tokio::time::sleep(
                                                            std::time::Duration::from_secs(20),
                                                        )
                                                        .await;
                                                        drop(turn);
                                                    });
                                                    if let Some(rx) = pck_rx2.take() {
                                                        let (rd, wr) = tls.into_split();
                                                        pck_session(
                                                            rd,
                                                            wr,
                                                            ev2.clone(),
                                                            ready2.clone(),
                                                            rx,
                                                            "turn-tls-raw",
                                                        )
                                                        .await;
                                                    }
                                                    return;
                                                }
                                                Ok(Err(e)) => {
                                                    let _ = ev2.send(ControllerEvent::Status(
                                                        format!("TURN 原始 TLS/PCK 失败: {e:#}"),
                                                    ));
                                                }
                                                Err(_) => {
                                                    let _ = ev2.send(ControllerEvent::Status(
                                                        "TURN 原始 TLS/PCK 超时".into(),
                                                    ));
                                                }
                                            }
                                        }
                                        if let Some(rx) = pck_rx2.take() {
                                            turn_pck_session(turn, ev2.clone(), ready2.clone(), rx)
                                                .await;
                                        }
                                        return;
                                    }
                                    Ok(Err(e)) => {
                                        let _ = ev2.send(ControllerEvent::Status(format!(
                                            "TURN {} 失败: {e:#}",
                                            server.host
                                        )));
                                    }
                                    Err(_) => {
                                        let _ = ev2.send(ControllerEvent::Status(format!(
                                            "TURN {} 超时",
                                            server.host
                                        )));
                                    }
                                }
                            }
                        }
                        if tokio::time::Instant::now() >= deadline {
                            break;
                        }
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_millis(500),
                            peer_rx2.changed(),
                        )
                        .await;
                    }
                    if mode == "turn" {
                        let _ = ev2.send(ControllerEvent::Status("TURN/PCK 键盘通道未建立".into()));
                        return;
                    }
                }
                let Some(pck_rx2) = pck_rx2.take() else {
                    return;
                };
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
                let mut tried: HashSet<SocketAddr> = HashSet::new();
                loop {
                    let list = cand_rx2.borrow().clone();
                    for addr in list {
                        if !tried.insert(addr) {
                            continue;
                        }
                        let _ = ev2.send(ControllerEvent::Status(format!(
                            "尝试键盘通道 {addr} ({mode})"
                        )));
                        let raw = mode == "raw";
                        if mode == "probe" {
                            match tokio::time::timeout(
                                std::time::Duration::from_secs(10),
                                crate::pck::PckTls::probe_after_stun(addr, &creds, ""),
                            )
                            .await
                            {
                                Ok(Ok(r)) => {
                                    let _ = ev2.send(ControllerEvent::Status(format!(
                                        "PCK 探针 {addr}: {r}"
                                    )));
                                }
                                Ok(Err(e)) => {
                                    let _ = ev2.send(ControllerEvent::Status(format!(
                                        "PCK 探针 {addr} 失败: {e:#}"
                                    )));
                                }
                                Err(_) => {
                                    let _ = ev2.send(ControllerEvent::Status(format!(
                                        "PCK 探针 {addr} 超时"
                                    )));
                                }
                            }
                            continue;
                        }
                        let conn = tokio::time::timeout(std::time::Duration::from_secs(8), async {
                            if raw {
                                let tcp =
                                    crate::pck::PckTls::dial_ice_tcp_raw(addr, &creds).await?;
                                let (r, w) = tokio::io::split(tcp);
                                Ok::<_, anyhow::Error>((
                                    Box::new(r) as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
                                    Box::new(w) as Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
                                ))
                            } else {
                                let tls =
                                    crate::pck::PckTls::dial_ice_tcp(addr, &creds, "").await?;
                                let (r, w) = tls.into_split();
                                Ok((
                                    Box::new(r) as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
                                    Box::new(w) as Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
                                ))
                            }
                        })
                        .await;
                        match conn {
                            Ok(Ok((rd, wr))) => {
                                pck_session(
                                    rd,
                                    wr,
                                    ev2.clone(),
                                    ready2.clone(),
                                    pck_rx2,
                                    if raw { "raw" } else { "tls" },
                                )
                                .await;
                                return;
                            }
                            Ok(Err(e)) => {
                                let _ = ev2.send(ControllerEvent::Status(format!(
                                    "键盘通道 {addr} 失败: {e:#}"
                                )));
                            }
                            Err(_) => {
                                let _ = ev2
                                    .send(ControllerEvent::Status(format!("键盘通道 {addr} 超时")));
                            }
                        }
                    }
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                let _ = ev2.send(ControllerEvent::Status("键盘通道未建立".into()));
            });
        }
    }

    loop {
        tokio::select! {
            _ = close_rx.recv() => {
                status("已断开");
                break;
            }
            Some(frame) = input_rx.recv() => {
                // The recovered input wire format is raw JSON on the
                // CONTROL_DATA_CHANNEL data channel
                // (`{"action":"mouse_move_absolute",...}` / `{"action":"kbd_press",...}`).
                // The GUI/CLI still hand us PCK-framed bytes, so unwrap the
                // PCK header and the `03 00 01 00` trailer and forward the JSON.
                if let Some(json) = pck_payload_json(&frame) {
                    if let Err(e) = media.send_control(json).await {
                        status(&format!("输入发送失败: {e}"));
                    }
                } else if let Err(e) = media.send_control(frame).await {
                    status(&format!("输入发送失败: {e}"));
                }
            }
            ev = sio.recv_event() => {
                let (event, data, _) = ev?;
                if event == "soac" && data["data"]["type"] == "candidate" {
                    if let Some(c) = data["data"].get("candidate") {
                        let cand = c["candidate"].as_str().unwrap_or_default().to_string();
                        let mid = c["sdpMid"].as_str().unwrap_or("0").to_string();
                        let idx = c["sdpMLineIndex"].as_u64().unwrap_or(0) as u16;
                        if let Some(addr) = crate::sdp::parse_tcp_candidate(&cand) {
                            let mut v = cand_tx.borrow().clone();
                            if !v.contains(&addr) { v.push(addr); let _ = cand_tx.send(v); }
                        }
                        if let Some(addr) = crate::sdp::parse_relay_candidate(&cand) {
                            let mut v = peer_tx.borrow().clone();
                            if !v.contains(&addr) { v.push(addr); let _ = peer_tx.send(v); }
                        }
                        if let Some(addr) = crate::sdp::parse_udp_candidate(&cand) {
                            let mut v = peer_tx.borrow().clone();
                            if !v.contains(&addr) { v.push(addr); let _ = peer_tx.send(v); }
                        }
                        if !cand.is_empty() {
                            media.add_candidate(cand, mid, idx).await?;
                        }
                    }
                }
            }
        }
    }
    let _ = media.pc.close().await;
    Ok(())
}
