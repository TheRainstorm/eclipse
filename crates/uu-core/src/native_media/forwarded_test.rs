//! A forwarded host and an outbound-only client with a rewritten NAT port.
//! Owned loopback UDP sockets only; no firewall, routing or desktop changes.
use super::*;
use uu_media::{RawFrame, Timestamp, VideoEncoder};

fn candidate_port(description: &RTCSessionDescription) -> u16 {
    description.sdp.lines().find_map(|line| {
        line.strip_prefix("a=candidate:")?.split_whitespace().nth(5)?.parse().ok()
    }).unwrap()
}

#[tokio::test(flavor="multi_thread",worker_threads=3)]
async fn outbound_only_client_decodes_forwarded_host_video_despite_nat_port_remapping() {
    let host_ip = "127.0.0.1".parse().unwrap();
    let client_ip = "127.0.0.2".parse().unwrap();
    let nat_ip = "127.0.0.3".parse().unwrap();
    let router_ip = "127.0.0.4".parse().unwrap();
    let stats = Arc::new(MediaStats::default());
    stats.decoder_preference.set(uu_media::DecoderKind::Software).unwrap();
    let mut host = NativePeer::new(host_ip, None, true).await.unwrap();
    let mut client = NativePeer::new_with_advertised(client_ip, nat_ip, Some(stats.clone()), false).await.unwrap();
    let (frames, mut images) = crate::media::frame_channel();
    client.receiver(frames, stats.clone()).await.unwrap();
    let mut relay = None;
    let result = async {
        let offer = client.description(true).await?;
        let client_addr = SocketAddr::new(client_ip, candidate_port(&offer));
        host.remote(offer, nat_ip).await?;
        let mut sender = host.sender_for_color(Arc::new(AtomicBool::new(false)), VideoCodec::H264, false).await?;
        let answer = host.description(false).await?;
        let host_addr = SocketAddr::new(host_ip, candidate_port(&answer));
        // WYM's router forwards its UDP port without changing that port.
        let router = UdpSocket::bind(SocketAddr::new(router_ip, host_addr.port())).await?;
        // B's CGNAT changes B's source port. Nothing listens on its advertised
        // private port, so host-initiated checks cannot create the connection.
        let nat = UdpSocket::bind(SocketAddr::new(nat_ip, 0)).await?;
        ensure!(nat.local_addr()?.port() != client_addr.port(), "fixture must rewrite the NAT port");
        relay = Some(tokio::spawn(async move {
            let mut outbound = [0u8; 65_536]; let mut inbound = [0u8; 65_536];
            let mut opened = false;
            loop {
                tokio::select! {
                    packet = router.recv_from(&mut outbound) => {
                        let (size, from) = packet?;
                        ensure!(from == client_addr, "unexpected fixture client");
                        opened = true;
                        nat.send_to(&outbound[..size], host_addr).await?;
                    }
                    packet = nat.recv_from(&mut inbound) => {
                        let (size, from) = packet?;
                        if opened && from == host_addr {
                            router.send_to(&inbound[..size], client_addr).await?;
                        }
                    }
                }
            }
            #[allow(unreachable_code)] Ok::<(), anyhow::Error>(())
        }));
        // The legacy host advertises its private IP. The authenticated media
        // TCP peer is the public router, so only that router may receive UDP.
        client.remote(answer, router_ip).await?;
        tokio::try_join!(host.connected(), client.connected())?;
        let mut encoder = uu_media::H264Encoder::with_frame_rate(1000, 30)?;
        for index in 0..12u8 {
            let mut pixels = vec![128; 64*64*3/2]; pixels[..64*64].fill(40 + index*8);
            for frame in encoder.encode(&RawFrame::new_i420(64, 64, pixels, Timestamp(u64::from(index)*33_333)))? {
                sender.frame(&frame).await?;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        timeout(Duration::from_secs(4), async {
            loop {
                while images.try_recv_image().is_ok() {}
                if stats.decoded_total.load(Ordering::Acquire) >= 12 { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.context("forwarded encrypted video did not decode")?;
        ensure!(stats.errors.load(Ordering::Acquire) == 0, "forwarded video decode error");
        Ok::<_, anyhow::Error>(())
    }.await;
    host.close().await; client.close().await;
    if let Some(relay) = relay { relay.abort(); let _ = relay.await; }
    result.expect("outbound-only NAT regression");
}
