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
    pub frames: crate::media::FrameReceiver,
    /// Raw JSON input frames for `CONTROL_DATA_CHANNEL`.
    pub input: UnboundedSender<Vec<u8>>,
    /// Raw CONTROL messages (e.g. CaptureSettingRequest protobufs) to send.
    pub settings: UnboundedSender<Vec<u8>>,
    pub close: UnboundedSender<()>,
    /// Live media statistics for the HUD.
    pub stats: std::sync::Arc<crate::media::MediaStats>,
}

pub fn spawn(config: Config, device: String, geo_ip: String) -> Result<ControllerHandle> {
    crate::install_crypto_provider();
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    let (frame_tx, frame_rx) = crate::media::frame_channel();
    let (input_tx, input_rx) = unbounded_channel();
    let (settings_tx, settings_rx) = unbounded_channel();
    let (close_tx, close_rx) = unbounded_channel();
    let stats = std::sync::Arc::new(crate::media::MediaStats::default());
    let stats_run = stats.clone();
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
            settings_rx,
            close_rx,
            stats_run,
        )) {
            let _ = ev_tx.send(ControllerEvent::Error(e.to_string()));
        }
    });
    Ok(ControllerHandle {
        events: ev_rx,
        frames: frame_rx,
        input: input_tx,
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
    device: String,
    geo_ip: String,
    ev: std::sync::mpsc::Sender<ControllerEvent>,
    frame_tx: crate::media::FrameSender,
    mut input_rx: UnboundedReceiver<Vec<u8>>,
    mut settings_rx: UnboundedReceiver<Vec<u8>>,
    mut close_rx: UnboundedReceiver<()>,
    stats: std::sync::Arc<crate::media::MediaStats>,
) -> Result<()> {
    let status = |s: &str| {
        let _ = ev.send(ControllerEvent::Status(s.to_string()));
    };
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
                "video_codec":1,"width":3840,"height":2160,
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
    eprintln!("ICE server entries={}", success["iceServers"].as_array().map_or(0,Vec::len));
    let ice = ice_servers_from_json(&success["iceServers"]);
    status("建立 WebRTC…");
    let media = ControllerMedia::new_with_frames(ice, Some(frame_tx), Some(stats.clone())).await?;
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
                media.set_answer(&answer).await?;
                answered = true;
            }
            Some("candidate") => add_trickle(&media, d, &ev).await?,
            _ => {}
        }
    }
    anyhow::ensure!(answered, "no SDP answer");
    status("已连接");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    sio.emit(
        "ice_finished_log",
        json!({"app_control_id": ctrl, "ice_id": ice_id}),
        &[],
    )
    .await?;

    // Ask the controlled end for the target frame rate. Without this the host
    // streams at its idle default (~21 fps); the official client sends the same
    // CaptureSettingRequest. Retry a few times while CONTROL settles.
    let fps_option: u64 = std::env::var("UU_FPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2); // 1=30, 2=60, 3=90, 4=144
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
    // Give CONTROL_DATA_CHANNEL time to open and the SimpleAction echo
    // handshake to settle; the host ignores settings sent before that.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    for i in 0..5u64 {
        let ts = crate::control_wire::now_ms();
        let _ = media
            .send_control(crate::control_wire::capture_setting_request_with_resolution(
                1000 + i,
                ts,
                fps_option,
                quality,
                custom_bps,
                crate::control_wire::CaptureResolution::from_env(),
            ))
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    }
    status("已请求目标画质/码率");

    loop {
        tokio::select! {
            _ = close_rx.recv() => {
                break;
            }
            Some(frame) = input_rx.recv() => {
                // `frame` is the JSON payload for CONTROL_DATA_CHANNEL. Early
                // input can arrive before the channel opens; ignore failures so
                // the status bar is not spammed.
                let _ = media.send_control(frame).await;
            }
            Some(msg) = settings_rx.recv() => {
                // Runtime CaptureSettingRequest (frame rate / quality / bitrate).
                let _ = media.send_control(msg).await;
            }
            recv = sio.recv_event() => {
                let (event, data, _) = recv?;
                if event == "soac" && data["data"]["type"] == "candidate" {
                    add_trickle(&media, &data["data"], &ev).await?;
                }
            }
        }
    }
    // Release the session so the server and the controlled end clear the
    // "connected" state instead of waiting for a timeout.
    let _ = sio
        .emit(
            "release",
            json!({"app_control_id": ctrl, "control_id": ctrl}),
            &[],
        )
        .await;
    let _ = media.pc.close().await;
    sio.close().await;
    status("已断开");
    Ok(())
}
