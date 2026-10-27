use super::*;
use std::time::Duration;
use tokio::time::{timeout, Instant};

async fn server_socket() -> (String, tokio::net::TcpListener) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    (format!("ws://{}", listener.local_addr().unwrap()), listener)
}
async fn accept(listener: tokio::net::TcpListener, open: &str) -> WebSocketStream<TcpStream> {
    let (tcp, _) = listener.accept().await.unwrap();
    tcp.set_nodelay(true).unwrap();
    let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
    socket
        .send(Message::Text(format!("0{open}").into()))
        .await
        .unwrap();
    assert_eq!(
        socket.next().await.unwrap().unwrap().into_text().unwrap(),
        "40"
    );
    socket.send(Message::Text("40{}".into())).await.unwrap();
    socket
}
fn refresh_id(text: &str) -> u64 {
    let text = text.strip_prefix("42").expect("Socket.IO event");
    let at = text.find('[').unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&text[at..]).unwrap(),
        serde_json::json!(["refresh_reconnect_key"]),
        "refresh has no invented payload argument"
    );
    text[..at].parse().unwrap()
}
fn ack(id: u64) -> Message {
    Message::Text(
        format!("43{id}[\"success\",{{\"reconnect_key\":\"generated-room-key\"}}]").into(),
    )
}

#[tokio::test]
async fn host_room_renews_despite_frequent_receive_cancellation_and_active_events() {
    room(
        Duration::from_millis(1500),
        Some((Duration::from_millis(80), Duration::from_millis(200))),
    )
    .await;
}
async fn room(duration: Duration, scaled: Option<(Duration, Duration)>) {
    let (url, listener) = server_socket().await;
    let lease = if scaled.is_some() {
        Duration::from_millis(650)
    } else {
        Duration::from_secs(900)
    };
    let server = tokio::spawn(async move {
        let mut socket = accept(listener, r#"{"pingInterval":1000,"pingTimeout":1000}"#).await;
        let mut expiry = Instant::now() + lease;
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let mut renewals = 0;
        loop {
            tokio::select! {
                _=tokio::time::sleep_until(expiry)=>panic!("room expired without refresh"),
                _=tick.tick()=>{
                    socket.send(Message::Text("2".into())).await.unwrap();
                    socket.send(Message::Text("42[\"fixture_tick\",{}]".into())).await.unwrap();
                },
                message=socket.next()=>match message {
                    Some(Ok(Message::Text(t))) if t=="3"=>{},
                    Some(Ok(Message::Text(t))) if t=="41"=>{},
                    Some(Ok(Message::Text(t)))=>{
                        let id=refresh_id(&t);renewals+=1;expiry=Instant::now()+lease;
                        socket.send(ack(id)).await.unwrap();
                    },
                    Some(Ok(Message::Close(_)))|None=>break,
                    other=>panic!("unexpected room message: {other:?}"),
                }
            }
        }
        renewals
    });
    let mut socket = SocketIo::connect(&url, "generated-only").await.unwrap();
    socket.keep_host_online();
    if let Some((retry, renew)) = scaled {
        socket.timers.refresh = Some(lifecycle::Refresh::new(Instant::now(), retry, renew));
    }
    let until = Instant::now() + duration;
    let mut events = 0;
    while Instant::now() < until {
        // Both a GUI timer and outbound route events cancel this future often.
        if let Ok(event) = timeout(Duration::from_millis(7), socket.recv_event()).await {
            let (name, _, _) = event.unwrap();
            assert_eq!(name, "fixture_tick");
            events += 1;
        }
    }
    socket.close().await;
    let renewals = timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
    assert!(
        renewals >= if scaled.is_some() { 5 } else { 7 },
        "host room must renew repeatedly"
    );
    assert!(events >= 10);
    println!(
        "room lifetime verified: elapsed_s={} renewals={renewals} events={events}",
        duration.as_secs()
    );
}

#[tokio::test]
async fn keepalive_ack_can_arrive_during_binary_assembly_without_losing_attachments() {
    let (url, listener) = server_socket().await;
    let server = tokio::spawn(async move {
        let mut socket = accept(listener, "{}").await;
        socket
            .send(Message::Text("452-[\"soac\",{}]".into()))
            .await
            .unwrap();
        socket
            .send(Message::Binary(vec![4, 10].into()))
            .await
            .unwrap();
        let id = refresh_id(&socket.next().await.unwrap().unwrap().into_text().unwrap());
        socket.send(ack(id)).await.unwrap();
        socket
            .send(Message::Binary(vec![4, 20].into()))
            .await
            .unwrap();
        while let Some(Ok(msg)) = socket.next().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
        }
    });
    let mut socket = SocketIo::connect(&url, "generated-only").await.unwrap();
    socket.timers.refresh = Some(lifecycle::Refresh::new(
        Instant::now(),
        Duration::from_millis(60),
        Duration::from_secs(300),
    ));
    let result = timeout(Duration::from_secs(2), socket.recv_event())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.0, "soac");
    assert_eq!(result.2, vec![vec![4, 10], vec![4, 20]]);
    assert!(
        socket.timers.refresh.as_ref().unwrap().next > Instant::now() + Duration::from_secs(290)
    );
    socket.close().await;
    timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}
#[tokio::test]
async fn silent_half_open_and_namespace_disconnect_end_the_room() {
    for silent in [false, true] {
        let (url, listener) = server_socket().await;
        let server = tokio::spawn(async move {
            let mut socket = accept(listener, r#"{"pingInterval":40,"pingTimeout":60}"#).await;
            if !silent {
                socket.send(Message::Text("41".into())).await.unwrap();
            }
            while let Some(Ok(msg)) = socket.next().await {
                if matches!(msg, Message::Close(_)) {
                    break;
                }
            }
        });
        let mut socket = SocketIo::connect(&url, "generated-only").await.unwrap();
        assert!(
            socket.timers.refresh.is_none(),
            "a viewer never renews a host room"
        );
        let error = timeout(Duration::from_secs(2), socket.recv_event())
            .await
            .unwrap()
            .unwrap_err()
            .to_string();
        assert!(if silent {
            error.contains("heartbeat")
        } else {
            error.contains("disconnected")
        });
        socket.close().await;
        timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }
}
#[tokio::test]
#[ignore = "35-minute real-clock room lifetime check; only an owned loopback fixture"]
async fn host_room_lives_past_thirty_minutes_with_production_refresh_intervals() {
    room(Duration::from_secs(35 * 60), None).await;
}
