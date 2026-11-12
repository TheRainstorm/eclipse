//! Owned network startup regressions; no desktop, input or production service.
use super::*;
use std::time::Instant;
use webrtc::stun::{agent::TransactionId, message::{Message, BINDING_REQUEST, BINDING_SUCCESS}, xoraddr::XorMappedAddress};

fn binding_response(request: &Message, port: u16) -> Message {
    let mut response = Message::new();
    response.build(&[
        Box::new(BINDING_SUCCESS),
        Box::new(request.transaction_id),
        Box::new(XorMappedAddress { ip: "192.0.2.7".parse().unwrap(), port }),
    ]).unwrap();
    response
}

#[tokio::test]
async fn stun_retries_lost_request_and_matches_response() {
    let conn = Net::new(None).bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = server.local_addr().unwrap();
    let mut responder = tokio::spawn(async move {
        let mut buf = [0; 1280];
        let (n, client) = server.recv_from(&mut buf).await.unwrap();
        let first = buf[..n].to_vec(); // Deliberately lose the first request.
        let (n, retry_client) = server.recv_from(&mut buf).await.unwrap();
        assert_eq!(client, retry_client);
        assert_eq!(first, buf[..n]); // Reuse the transaction ID and bytes.
        let mut request = Message::new();
        request.raw = first;
        request.decode().unwrap();
        let wrong_mapping = binding_response(&request, 40000);
        let foreign = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        foreign.send_to(&wrong_mapping.raw, client).await.unwrap();
        server.send_to(b"invalid STUN", client).await.unwrap();
        let mut unrelated = Message::new();
        unrelated.build(&[Box::new(BINDING_REQUEST), Box::new(TransactionId::new())]).unwrap();
        server.send_to(&binding_response(&unrelated, 40001).raw, client).await.unwrap();
        server.send_to(&request.raw, client).await.unwrap(); // Requests are not responses.
        tokio::time::sleep(Duration::from_millis(20)).await;
        server.send_to(&binding_response(&request, 50000).raw, client).await.unwrap();
    });
    let result = webrtc::ice::util::get_xormapped_addr(&conn, addr, Duration::from_secs(2)).await;
    conn.close().await.unwrap();
    let completed = timeout(Duration::from_secs(1), &mut responder).await;
    if completed.is_err() { responder.abort(); let _ = responder.await; }
    let mapped = result.unwrap();
    assert_eq!(mapped.ip, "192.0.2.7".parse::<IpAddr>().unwrap());
    assert_eq!(mapped.port, 50000);
    completed.unwrap().unwrap();
}

#[tokio::test]
async fn stun_retries_keep_the_original_deadline() {
    let conn = Net::new(None).bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let started = Instant::now();
    let result = webrtc::ice::util::stun_request(&conn, server.local_addr().unwrap(), Duration::from_millis(650)).await;
    conn.close().await.unwrap();
    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(2));
    let mut buf = [0; 1280];
    let (n, _) = server.try_recv_from(&mut buf).unwrap();
    let first = buf[..n].to_vec();
    let (n, _) = server.try_recv_from(&mut buf).unwrap();
    assert_eq!(first, buf[..n]);
    assert!(server.try_recv_from(&mut buf).is_err());
}

#[tokio::test]
async fn internet_gathers_before_sdp_and_reuses_candidates() {
    let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let ice = crate::direct_ice::InternetIce::new(vec![format!("stun:{}", server.local_addr().unwrap())]).unwrap();
    let peer = NativePeer::new_direct(&ice, None, true).await.unwrap();
    let result = async {
        assert!(peer.pc.local_description().await.is_none());
        assert!(peer.pc.remote_description().await.is_none());
        assert!(peer.pc.get_transceivers().await.is_empty());
        let mut complete = peer.pc.gathering_complete_promise().await;
        let mut buf = [0; 1280];
        let (n, client) = timeout(Duration::from_secs(1), server.recv_from(&mut buf)).await??;
        let mut request = Message::new(); request.raw = buf[..n].to_vec(); request.decode()?;
        server.send_to(&binding_response(&request, 50000).raw, client).await?;
        timeout(Duration::from_secs(2), complete.recv()).await?;
        peer.pc.gather_ice_candidates().await?; // A repeated start is harmless.
        peer.pc.create_data_channel("late-control", None).await?;
        let offer = peer.description(true).await?;
        ensure!(offer.sdp.contains("192.0.2.7 50000 typ srflx"), "early STUN candidate missing from the complete SDP");
        ensure!(offer.sdp.contains("m=application"), "late data channel missing");
        Ok::<_,anyhow::Error>(())
    }.await;
    peer.close().await;
    assert!(peer.pc.gather_ice_candidates().await.is_err());
    result.unwrap();
}
