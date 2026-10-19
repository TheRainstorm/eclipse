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
    let stats=Arc::new(crate::media::MediaStats::default());
    stats.clipboard.enabled.store(true,std::sync::atomic::Ordering::Release);
    stats.clipboard.active.store(true,std::sync::atomic::Ordering::Release);
    *stats.clipboard.local.lock().unwrap()=Some("local-to-host 中文".into());
    let media = ControllerMedia::new_with_frames(vec![],None,Some(stats.clone())).await.unwrap();
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
    let track_request=crate::screen_control::send_video_track(10001,2);
    let expected_track=track_request.clone();
    let activation=crate::screen_control::activate_screen(10004,42);
    let expected_activation=activation.clone();
    let mut sources=Vec::new();
    for (id,track,name,primary) in [(7,0,"Zako HDR",true),(42,u64::MAX,"S2719",false)] {
        let mut screen=Vec::new();crate::control_wire::int(1,id,&mut screen);crate::control_wire::int(12,track,&mut screen);
        crate::control_wire::int(7,primary as u64,&mut screen);crate::control_wire::bytes(10,name.as_bytes(),&mut screen);
        crate::control_wire::bytes(1,&screen,&mut sources);
    }
    crate::control_wire::int(2,42,&mut sources);
    let inventory=crate::control_wire::envelope(10002,7,&sources);
    let (delivered_tx, mut delivered_rx) = unbounded_channel();
    peer.on_data_channel(Box::new(move |channel| {
        let tx = delivered_tx.clone();
        let expected = expected.clone();
        let expected_track=expected_track.clone();let inventory=inventory.clone();
        let expected_activation=expected_activation.clone();
        Box::pin(async move {
            if channel.label()=="TEXT_DATA_CHANNEL" {
                let weak=Arc::downgrade(&channel);
                channel.on_message(Box::new(move |msg| {
                    let tx=tx.clone();let channel=weak.clone();
                    Box::pin(async move {
                        let received=crate::clipboard::Protocol::default().receive(&msg.data,&std::sync::atomic::AtomicU64::new(1000)).unwrap();
                        if let Some(text)=received.text {
                            assert_eq!(text,"local-to-host 中文");let _=tx.send("clipboard outbound");
                            if let Some(channel)=channel.upgrade() {
                                for reply in received.replies {channel.send(&reply.into()).await.unwrap();}
                                channel.send(&crate::clipboard::text_change(2000,"host-to-local 🙂").unwrap().into()).await.unwrap();
                            }
                        } else if received.acknowledged==Some(2000) {let _=tx.send("clipboard ack");}
                    })
                }));
            } else if channel.label() == "CONTROL_DATA_CHANNEL" {
                let weak=Arc::downgrade(&channel);
                channel.on_open(Box::new(move ||{let channel=weak.clone();Box::pin(async move {
                    if let Some(channel)=channel.upgrade(){channel.send(&crate::control_wire::echo_response_with_clipboard(99,1,true).into()).await.unwrap();}
                })}));
                let track_channel=Arc::downgrade(&channel);
                channel.on_message(Box::new(move |msg| {
                    let tx = tx.clone();let channel=track_channel.clone();let inventory=inventory.clone();
                    let kind = if !msg.is_string && msg.data.as_ref()==expected_track.as_slice(){Some("display track")}
                    else if !msg.is_string && msg.data.as_ref()==expected_activation.as_slice(){Some("display activation")}
                    else if !msg.is_string && msg.data.as_ref() == expected.as_slice() {
                        Some("settings")
                    } else {
                        serde_json::from_slice::<serde_json::Value>(&msg.data).ok().and_then(|v|
                            if v["heartbeat"] == "kbd_heartbeat" { Some("heartbeat") }
                            else if v["action"] == "mouse_move_absolute" { Some("pointer") }
                            else if v["action"] == "kbd_press" { Some("key") } else { None })
                    };
                    Box::pin(async move {
                        if kind==Some("display track") {
                            if let Some(channel)=channel.upgrade() {
                                let mut header=Vec::new();crate::control_wire::int(1,10001,&mut header);
                                let mut rpc=Vec::new();crate::control_wire::bytes(1,&header,&mut rpc);crate::control_wire::bytes(11,&[],&mut rpc);
                                channel.send(&crate::control_wire::envelope(10003,22,&rpc).into()).await.unwrap();
                                channel.send(&inventory.into()).await.unwrap();
                            }
                        }
                        if kind==Some("display activation") {
                            if let Some(channel)=channel.upgrade() {
                                let mut result=Vec::new();crate::control_wire::int(1,10004,&mut result);
                                crate::control_wire::int(2,8,&mut result);
                                channel.send(&crate::control_wire::envelope(10005,19,&result).into()).await.unwrap();
                                // The legacy host changes capture without resending ScreenSources.
                                let mut change=Vec::new();crate::control_wire::int(2,42,&mut change);
                                channel.send(&crate::control_wire::envelope(10006,8,&change).into()).await.unwrap();
                            }
                        }
                        if let Some(kind) = kind { let _ = tx.send(kind); }
                    })
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
            while delivered.len() < 8 { delivered.insert(delivered_rx.recv().await.expect("CONTROL messages")); }
            assert_eq!(delivered, ["settings", "heartbeat", "pointer", "key", "clipboard outbound", "clipboard ack", "display track", "display activation"].into_iter().collect());
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
    input.send(track_request).unwrap();
    input.send(activation).unwrap();
    let (_setting, mut settings_rx) = unbounded_channel();
    tokio::time::timeout(Duration::from_secs(25), drive_session(&mut sio, &media, "test", "ice", &ev,
        &mut input_rx, &mut settings_rx, settings)).await.unwrap().unwrap();
    assert!(events.try_iter().any(|event| matches!(event, ControllerEvent::Status(s) if s == "已请求目标画质/码率")));
    assert_eq!(stats.clipboard.remote.lock().unwrap().as_deref(),Some("host-to-local 🙂"));
    assert!(stats.display_responses.lock().unwrap().contains(&(10001,11,0)));
    assert!(stats.display_responses.lock().unwrap().contains(&(10004,19,0)));
    let screens=stats.remote_screens.lock().unwrap().clone();
    assert_eq!(screens.selected().unwrap().display_name,"S2719");assert_eq!(screens.for_track(0).unwrap().id,7);
    assert_eq!(screens.screens.iter().find(|s|s.id==42).unwrap().assigned_track(),None);
    assert_eq!(*stats.desktop_capture_source.lock().unwrap(),Some(42));
    let effective=screens.with_desktop_capture(*stats.desktop_capture_source.lock().unwrap());
    assert_eq!(effective.for_track(0).unwrap().id,42);
    assert_eq!(effective.screens.iter().find(|s|s.id==7).unwrap().assigned_track(),None);
    stats.clipboard.stopped.store(true,std::sync::atomic::Ordering::Release);
    media.pc.close().await.unwrap();
    // The first close can already reset the remote SCTP association. The
    // library may report that reset while still completing peer shutdown.
    let _=peer.close().await;
    assert_eq!(peer.connection_state(),webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Closed);
    sio.close().await;
    server.await.unwrap();
}
