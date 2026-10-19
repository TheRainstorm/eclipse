//! Regression: delayed trickle must be handled before CONTROL can open.
use super::*;
use futures_util::{SinkExt, StreamExt};
use std::{sync::Arc, time::Duration};
use tokio_tungstenite::tungstenite::Message;
use webrtc::{api::{APIBuilder, media_engine::MediaEngine}, ice_transport::ice_candidate::RTCIceCandidateInit,
    peer_connection::{configuration::RTCConfiguration, sdp::session_description::RTCSessionDescription}};

fn without_candidates(sdp: &str) -> (String, Vec<String>) {
    let mut candidates = Vec::new();
    let lines: Vec<_> = sdp.lines().filter(|line| {
        if let Some(candidate) = line.strip_prefix("a=candidate:") {
            let candidate = format!("candidate:{candidate}");
            if !candidates.contains(&candidate) { candidates.push(candidate); }
            false
        } else { *line != "a=end-of-candidates" }
    }).collect();
    (format!("{}\r\n", lines.join("\r\n")), candidates)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delayed_trickle_opens_control_and_delivers_initial_settings() {
    crate::install_crypto_provider();
    let media = ControllerMedia::new(vec![]).await.unwrap();
    let offer = media.offer().await.unwrap();
    let (offer, client_candidates) = without_candidates(&offer);
    assert!(!client_candidates.is_empty());
    let mut engine = MediaEngine::default();
    engine.register_default_codecs().unwrap();
    let peer = Arc::new(APIBuilder::new().with_media_engine(engine).build()
        .new_peer_connection(RTCConfiguration::default()).await.unwrap());
    let settings = crate::control_wire::capture_setting_request_with_resolution(
        1000, 1, 2, 5, None, crate::control_wire::CaptureResolution::Native);
    let expected = settings.clone();
    let (delivered_tx, mut delivered_rx) = unbounded_channel();
    peer.on_data_channel(Box::new(move |channel| {
        let tx = delivered_tx.clone();
        let expected = expected.clone();
        Box::pin(async move {
            if channel.label() == "CONTROL_DATA_CHANNEL" {
                channel.on_message(Box::new(move |msg| {
                    let tx = tx.clone();
                    let kind = if !msg.is_string && msg.data.as_ref() == expected.as_slice() {
                        Some("settings")
                    } else {
                        serde_json::from_slice::<serde_json::Value>(&msg.data).ok().and_then(|v|
                            if v["heartbeat"] == "kbd_heartbeat" { Some("heartbeat") }
                            else if v["action"] == "mouse_move_absolute" { Some("pointer") }
                            else if v["action"] == "kbd_press" { Some("key") } else { None })
                    };
                    Box::pin(async move { if let Some(kind) = kind { let _ = tx.send(kind); } })
                }));
            }
        })
    }));
    peer.set_remote_description(RTCSessionDescription::offer(offer).unwrap()).await.unwrap();
    let answer = peer.create_answer(None).await.unwrap();
    let mut gathered = peer.gathering_complete_promise().await;
    peer.set_local_description(answer).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), gathered.recv()).await.unwrap();
    let (answer, candidates) = without_candidates(&peer.local_description().await.unwrap().sdp);
    assert!(!candidates.is_empty());
    media.set_answer(&answer).await.unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let remote = peer.clone();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        ws.send(Message::Text("0{}".into())).await.unwrap();
        assert_eq!(ws.next().await.unwrap().unwrap().into_text().unwrap(), "40");
        ws.send(Message::Text("40{}".into())).await.unwrap();
        // Longer than the regressed setup's 3.5s + five 600ms retries.
        tokio::time::sleep(Duration::from_secs(8)).await;
        for candidate in client_candidates {
            remote.add_ice_candidate(RTCIceCandidateInit { candidate, sdp_mid:Some("0".into()), sdp_mline_index:Some(0), ..Default::default() }).await.unwrap();
        }
        for candidate in candidates {
            let event = json!(["soac", {"data":{"type":"candidate", "candidate":{
                "candidate":candidate,"sdpMid":"0","sdpMLineIndex":0}}}]);
            ws.send(Message::Text(format!("42{event}").into())).await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut delivered = std::collections::HashSet::new();
            while delivered.len() < 4 { delivered.insert(delivered_rx.recv().await.expect("CONTROL messages")); }
            assert_eq!(delivered, ["settings", "heartbeat", "pointer", "key"].into_iter().collect());
        }).await.expect("settings, heartbeat and both kinds of input must reach the peer");
        ws.send(Message::Text("42[\"released\",{}]".into())).await.unwrap();
        // Keep the socket alive until the client consumes released.
        while let Some(Ok(message)) = ws.next().await {
            if matches!(message, Message::Close(_)) { break; }
        }
    });
    let mut sio = SocketIo::connect(&format!("ws://{addr}"), "test").await.unwrap();
    let (ev, events) = std::sync::mpsc::channel();
    let (input, mut input_rx) = unbounded_channel();
    input.send(br#"{"action":"mouse_move_absolute","screen_id":42,"abs_x":0.5,"abs_y":0.5}"#.to_vec()).unwrap();
    input.send(br#"{"action":"kbd_press","key":65}"#.to_vec()).unwrap();
    let (_setting, mut settings_rx) = unbounded_channel();
    tokio::time::timeout(Duration::from_secs(25), drive_session(&mut sio, &media, "test", "ice", &ev,
        &mut input_rx, &mut settings_rx, settings)).await.unwrap().unwrap();
    assert!(events.try_iter().any(|event| matches!(event, ControllerEvent::Status(s) if s == "已请求目标画质/码率")));
    media.pc.close().await.unwrap();
    peer.close().await.unwrap();
    sio.close().await;
    server.await.unwrap();
}
