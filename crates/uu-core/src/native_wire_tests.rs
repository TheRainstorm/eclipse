//! Own localhost UDP relay: drop encrypted video before SRTP authentication.
//! No account, capture, audio, desktop input, or system network changes.
use super::*;
use anyhow::{bail, Context};
use std::sync::atomic::AtomicUsize;
use tokio::sync::mpsc;
use webrtc::{
    rtcp::transport_feedbacks::transport_layer_nack::{NackPair, TransportLayerNack},
    rtp::{header::Header, packet::Packet},
};

fn media_port(description: &RTCSessionDescription) -> Result<u16> {
    description
        .sdp
        .lines()
        .find(|line| line.starts_with("a=candidate:"))
        .and_then(|line| line.split_whitespace().nth(5))
        .context("missing localhost candidate")?
        .parse()
        .context("invalid localhost candidate port")
}
fn via_relay(mut description: RTCSessionDescription, port: u16) -> RTCSessionDescription {
    description.sdp = description
        .sdp
        .lines()
        .map(|line| {
            if line.starts_with("a=candidate:") {
                let mut words: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
                words[5] = port.to_string();
                words.join(" ")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n";
    description
}

#[tokio::test(flavor="multi_thread",worker_threads=3)]
async fn encrypted_twelve_frame_burst_preserves_references_while_a_fast_decoder_is_briefly_busy() {run_encrypted_twelve_frame_burst_preserves_references_while_a_fast_decoder_is_briefly_busy(false).await;}
#[tokio::test(flavor="multi_thread",worker_threads=3)]
async fn dedicated_encrypted_twelve_frame_burst_preserves_references_while_a_fast_decoder_is_briefly_busy() {run_encrypted_twelve_frame_burst_preserves_references_while_a_fast_decoder_is_briefly_busy(true).await;}
async fn run_encrypted_twelve_frame_burst_preserves_references_while_a_fast_decoder_is_briefly_busy(dedicated:bool) {
    use uu_media::{EncodedFrame,RawFrame,Timestamp,VideoDecoder,VideoEncoder};
    struct Pause {
        entered:AtomicBool,
        released:std::sync::Mutex<bool>,
        ready:std::sync::Condvar,
    }
    struct Release(Arc<Pause>);
    impl Drop for Release {
        fn drop(&mut self) {
            *self.0.released.lock().unwrap()=true;self.0.ready.notify_all();
        }
    }
    struct Decoder {inner:Box<dyn VideoDecoder>,count:u32,pause:Arc<Pause>}
    impl VideoDecoder for Decoder {
        fn codec(&self)->VideoCodec {self.inner.codec()}
        fn prepare(&mut self,w:u32,h:u32)->std::result::Result<(),uu_media::CodecError> {self.inner.prepare(w,h)}
        fn backend_name(&self)->&str {self.inner.backend_name()}
        fn decode(&mut self,frame:&EncodedFrame)->std::result::Result<Option<RawFrame>,uu_media::CodecError> {
            if self.count==32 {
                self.pause.entered.store(true,Ordering::Release);
                let released=self.pause.released.lock().unwrap();
                let (released,_)=self.pause.ready.wait_timeout_while(released,Duration::from_secs(5),|ready|!*ready).unwrap();
                if !*released {return Err(uu_media::CodecError::Backend("owned burst fixture was not released".into()));}
            }
            self.count+=1;self.inner.decode(frame)
        }
    }
    let ip:IpAddr="127.0.0.1".parse().unwrap();
    let stats=Arc::new(MediaStats::default());let _=stats.decoder_preference.set(uu_media::DecoderKind::Software);
    let mut host=NativePeer::new(ip,None,true).await.unwrap();
    let mut viewer=NativePeer::new_lan_io(ip,ip,Some(stats.clone()),false,None,dedicated).await.unwrap();
    let pause=Arc::new(Pause{entered:false.into(),released:std::sync::Mutex::new(false),ready:Default::default()});
    let release=Release(pause.clone());
    let (frames,mut images)=crate::media::frame_channel();
    viewer.receiver(frames,stats.clone()).await.unwrap();
    let decoder=viewer.pending_decoder.lock().unwrap().take().unwrap();
    *viewer.pending_decoder.lock().unwrap()=Some(crate::media::PreparedVideoDecoder{
        decoder:Box::new(Decoder{inner:decoder.decoder,count:0,pause:pause.clone()}),visible:decoder.visible});
    let refresh=Arc::new(AtomicBool::new(false));
    let mut sender=host.sender_for_color(refresh,VideoCodec::H264,false).await.unwrap();
    let result=async {
        let offer=viewer.description(true).await?;host.remote(offer,ip).await?;
        let answer=host.description(false).await?;viewer.remote(answer,ip).await?;
        tokio::try_join!(host.connected(),viewer.connected())?;
        let mut encoder=uu_media::H264Encoder::with_frame_rate(1000,60)?;
        let mut encoded=Vec::new();
        for index in 0..44u8 {
            let mut pixels=vec![128;64*64*3/2];pixels[..64*64].fill(32+index*2);
            encoded.extend(encoder.encode(&RawFrame::new_i420(64,64,pixels,Timestamp(u64::from(index)*16_666)))?);
        }
        ensure!(encoded.len()==44,"fixture needs one low-delay picture per input");
        for frame in &encoded[..32] {
            sender.frame(frame).await?;
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        timeout(Duration::from_secs(4),async {
            while stats.decoded_total.load(Ordering::Acquire)<32 {
                while images.try_recv_image().is_ok(){}
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }).await.context("warm-up pictures were not decoded")?;
        let packets=stats.packets.load(Ordering::Acquire);
        for frame in &encoded[32..] {sender.frame(frame).await?;}
        timeout(Duration::from_secs(3),async {
            while !pause.entered.load(Ordering::Acquire)||stats.packets.load(Ordering::Acquire)<packets+12 {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }).await.context("encrypted burst did not reach the paused decoder")?;
        // The last packet counter is published just before AU assembly.
        tokio::time::sleep(Duration::from_millis(10)).await;
        ensure!(stats.queue_drops.load(Ordering::Acquire)==0,"bounded fast-decode burst unnecessarily broke references");
        drop(release);
        timeout(Duration::from_secs(4),async {
            while stats.decoded_total.load(Ordering::Acquire)<44 {
                while images.try_recv_image().is_ok(){}
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }).await.context("burst references did not recover without a new IDR")?;
        ensure!(stats.errors.load(Ordering::Acquire)==0 && stats.recovery_decode_queue.load(Ordering::Acquire)==0,"burst caused decoder recovery");
        Ok::<_,anyhow::Error>(())
    }.await;
    // Also release on an assertion/error before closing the decoder's peer.
    *pause.released.lock().unwrap()=true;pause.ready.notify_all();
    host.close().await;viewer.close().await;
    result.expect("encrypted reference-preserving burst regression");
}

#[tokio::test(flavor="multi_thread",worker_threads=3)]
async fn encrypted_wire_loss_is_repaired_once_after_three_hundred_new_packets() {run_encrypted_wire_loss_is_repaired_once_after_three_hundred_new_packets(false).await;}
#[tokio::test(flavor="multi_thread",worker_threads=3)]
async fn dedicated_encrypted_wire_loss_is_repaired_once_after_three_hundred_new_packets() {run_encrypted_wire_loss_is_repaired_once_after_three_hundred_new_packets(true).await;}
async fn run_encrypted_wire_loss_is_repaired_once_after_three_hundred_new_packets(dedicated:bool) {
    const FIRST: u16 = 7000;
    const MISSING: u16 = FIRST + 100;
    const LAST: u16 = FIRST + 400;
    let ip: IpAddr = "127.0.0.1".parse().unwrap();
    let mut host = NativePeer::new(ip, None, true).await.unwrap();
    let mut viewer = NativePeer::new_lan_io(ip,ip,None,false,None,dedicated).await.unwrap();
    let relay = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
    let relay_port = relay.local_addr().unwrap().port();
    let retransmitted = Arc::new(AtomicUsize::new(0));
    let forwarded = retransmitted.clone();
    let (received_tx, mut received) = mpsc::channel(512);
    viewer.pc.on_track(Box::new(move |track, _, _| {
        let tx = received_tx.clone();
        Box::pin(async move {
            tokio::spawn(async move {
                while let Ok((packet, _)) = track.read_rtp().await {
                    if tx
                        .send((packet.header.sequence_number, packet.header.ssrc))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
        })
    }));
    let mut relay_task = None;
    let mut feedback_task = None;
    let result = async {
        viewer
            .pc
            .add_transceiver_from_kind(
                RTPCodecType::Video,
                Some(RTCRtpTransceiverInit {
                    direction: RTCRtpTransceiverDirection::Recvonly,
                    send_encodings: vec![],
                }),
            )
            .await?;
        let track = Arc::new(TrackLocalStaticRTP::new(
            h264_capability(),
            "wire-video".into(),
            "synthetic".into(),
        ));
        let sender = host
            .pc
            .add_track(track.clone() as Arc<dyn TrackLocal + Send + Sync>)
            .await?;
        feedback_task = Some(tokio::spawn(async move {
            while sender.read_rtcp().await.is_ok() {}
        }));
        let offer = viewer.description(true).await?;
        let viewer_address = SocketAddr::new(ip, media_port(&offer)?);
        host.remote(via_relay(offer, relay_port), ip).await?;
        let answer = host.description(false).await?;
        let host_address = SocketAddr::new(ip, media_port(&answer)?);
        relay_task = Some(tokio::spawn(async move {
            let mut buffer = vec![0u8; 65536];
            let mut lost: Option<Vec<u8>> = None;
            loop {
                let (length, source) = relay.recv_from(&mut buffer).await?;
                let target = if source == host_address {
                    viewer_address
                } else if source == viewer_address {
                    host_address
                } else {
                    continue;
                };
                let data = &mut buffer[..length];
                if source == host_address
                    && length >= 12
                    && data[0] >> 6 == 2
                    && data[1] & 0x7f == 102
                    && u16::from_be_bytes([data[2], data[3]]) == MISSING
                {
                    match &lost {
                        None => {
                            lost = Some(data.to_vec());
                            continue;
                        }
                        Some(original) => {
                            ensure!(
                                data == original.as_slice(),
                                "late retransmission changed its encrypted packet index"
                            );
                            let attempt = forwarded.load(Ordering::Relaxed);
                            if attempt == 0 {
                                // A forged repair must not consume the missing index.
                                data[length - 1] ^= 1;
                            }
                        }
                    }
                }
                relay.send_to(data, target).await?;
                if source == host_address
                    && length >= 12
                    && data[0] >> 6 == 2
                    && data[1] & 0x7f == 102
                    && u16::from_be_bytes([data[2], data[3]]) == MISSING
                {
                    // Publish the barrier after the UDP send, so a negative
                    // receive assertion cannot pass before forwarding occurs.
                    forwarded.fetch_add(1, Ordering::Release);
                }
            }
            #[allow(unreachable_code)]
            Ok::<_, anyhow::Error>(())
        }));
        viewer.remote(via_relay(answer, relay_port), ip).await?;
        tokio::try_join!(host.connected(), viewer.connected())?;
        let packet = |seq| Packet {
            header: Header {
                version: 2,
                payload_type: 102,
                sequence_number: seq,
                marker: true,
                ..Default::default()
            },
            payload: bytes::Bytes::from_static(b"\x65generated wire regression"),
            ..Default::default()
        };
        // Connected can precede the receive task becoming ready. Bootstrap
        // with identical copies, then pace the fixture so only our deliberate
        // drop is lost, even while the workspace tests run in parallel.
        timeout(Duration::from_secs(5), async {
            loop {
                track.write_rtp(&packet(FIRST)).await?;
                if let Ok(received) = timeout(Duration::from_millis(10), received.recv()).await {
                    let (seq, _) = received.context("bootstrap track closed")?;
                    ensure!(seq == FIRST, "unexpected bootstrap sequence");
                    return Ok::<_, anyhow::Error>(());
                }
            }
        })
        .await
        .context("initial encrypted track did not become ready")??;
        for seq in FIRST + 1..=LAST {
            track.write_rtp(&packet(seq)).await?;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let mut count = 1;
        let media_ssrc = timeout(Duration::from_secs(5), async {
            loop {
                let (seq, ssrc) = received.recv().await.context("viewing track closed")?;
                ensure!(seq != MISSING, "original lost packet reached SRTP");
                count += 1;
                if seq == LAST {
                    return Ok::<_, anyhow::Error>(ssrc);
                }
            }
        })
        .await
        .context("initial encrypted packets were not delivered")??;
        ensure!(
            count == 400,
            "unexpected packet loss in localhost fixture: {count}"
        );
        for attempt in 0..3 {
            viewer
                .pc
                .write_rtcp(&[Box::new(TransportLayerNack {
                    sender_ssrc: 123,
                    media_ssrc,
                    nacks: vec![NackPair {
                        packet_id: MISSING,
                        lost_packets: 0,
                    }],
                })])
                .await?;
            timeout(Duration::from_secs(3), async {
                while retransmitted.load(Ordering::Relaxed) <= attempt {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .context("requested NACK repair never crossed the owned relay")?;
            if attempt == 1 {
                let (seq, _) = timeout(Duration::from_secs(3), received.recv())
                    .await
                    .context("authenticated NACK repair was rejected after 300 newer packets")?
                    .context("repair track closed")?;
                ensure!(seq == MISSING, "unexpected repair sequence");
            } else {
                ensure!(
                    timeout(Duration::from_millis(200), received.recv())
                        .await
                        .is_err(),
                    "forged or duplicate repair reached the application"
                );
            }
        }
        ensure!(
            retransmitted.load(Ordering::Relaxed) >= 3,
            "fixture did not forward all NACK repairs"
        );
        if let Some(task) = &relay_task {
            if task.is_finished() {
                bail!("owned UDP relay ended unexpectedly");
            }
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    host.close().await;
    viewer.close().await;
    if let Some(task) = relay_task {
        task.abort();
        let _ = task.await;
    }
    if let Some(task) = feedback_task {
        timeout(Duration::from_secs(2), task)
            .await
            .expect("RTCP reader did not stop")
            .unwrap();
    }
    result.expect("encrypted localhost retransmission regression");
}

#[tokio::test(flavor="multi_thread",worker_threads=3)]
async fn encrypted_fec_recovers_a_lost_keyframe_fragment_when_every_retransmission_is_dropped() {run_encrypted_fec_recovers_a_lost_keyframe_fragment_when_every_retransmission_is_dropped(false).await;}
#[tokio::test(flavor="multi_thread",worker_threads=3)]
async fn dedicated_encrypted_fec_recovers_a_lost_keyframe_fragment_when_every_retransmission_is_dropped() {run_encrypted_fec_recovers_a_lost_keyframe_fragment_when_every_retransmission_is_dropped(true).await;}
async fn run_encrypted_fec_recovers_a_lost_keyframe_fragment_when_every_retransmission_is_dropped(dedicated:bool) {
    use uu_media::{RawFrame,Timestamp,VideoEncoder};
    let ip:IpAddr="127.0.0.1".parse().unwrap();
    let stats=Arc::new(MediaStats::default());let _=stats.decoder_preference.set(uu_media::DecoderKind::Software);
    let mut host=NativePeer::new(ip,None,true).await.unwrap();
    let mut viewer=NativePeer::new_lan_io(ip,ip,Some(stats.clone()),false,None,dedicated).await.unwrap();
    let(frames,mut images)=crate::media::frame_channel();viewer.receiver(frames,stats.clone()).await.unwrap();
    let mut sender=host.sender_for_color(Arc::new(AtomicBool::new(false)),VideoCodec::H264,false).await.unwrap();
    let relay=UdpSocket::bind((ip,0)).await.unwrap();let port=relay.local_addr().unwrap().port();
    let dropped=Arc::new(AtomicUsize::new(0));let losses=dropped.clone();let mut relay_task=None;
    let result=async {
        let offer=viewer.description(true).await?;let viewer_address=SocketAddr::new(ip,media_port(&offer)?);
        host.remote(via_relay(offer,port),ip).await?;
        let answer=host.description(false).await?;let host_address=SocketAddr::new(ip,media_port(&answer)?);
        relay_task=Some(tokio::spawn(async move {
            let mut buffer=vec![0;65536];let mut missing=None;let mut originals=std::collections::HashSet::new();
            loop {
                let(length,source)=relay.recv_from(&mut buffer).await?;
                let target=if source==host_address{viewer_address}else if source==viewer_address{host_address}else{continue;};
                let data=&buffer[..length];
                if source==host_address&&length>=12&&data[0]>>6==2 {
                    let pt=data[1]&127;
                    if pt==103 {losses.fetch_add(1,Ordering::Release);continue;}
                    if pt==102 {
                        let seq=u16::from_be_bytes([data[2],data[3]]);
                        if originals.insert(seq)&&originals.len()==3{missing=Some(seq);}
                        if missing==Some(seq){losses.fetch_add(1,Ordering::Release);continue;}
                    }
                }
                relay.send_to(data,target).await?;
            }
            #[allow(unreachable_code)] Ok::<_,anyhow::Error>(())
        }));
        viewer.remote(via_relay(answer,port),ip).await?;
        tokio::try_join!(host.connected(),viewer.connected())?;
        timeout(Duration::from_secs(3),async {
            loop {
                let open=host.fec.ready();
                if open{break;}
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }).await.context("negotiated FEC channel did not open")?;
        let mut encoder=uu_media::H264Encoder::with_frame_rate(3000,30)?;
        for index in 0..12u32 {
            let mut pixels=vec![128;320*180*3/2];let mut random=0x12345678u32;
            for pixel in &mut pixels[..320*180]{random=random.wrapping_mul(1664525).wrapping_add(1013904223);*pixel=((random>>24) as u8).wrapping_add(index as u8);}
            let encoded=encoder.encode(&RawFrame::new_i420(320,180,pixels,Timestamp(u64::from(index)*33_333)))?;
            ensure!(encoded.len()==1,"fixture needs one coded frame per input");
            sender.frame(&encoded[0]).await?;
            timeout(Duration::from_secs(3),async {
                while stats.decoded_total.load(Ordering::Acquire)<u64::from(index)+1 {
                    while images.try_recv_image().is_ok(){}
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            }).await.context("keyframe/reference did not decode without retransmission")?;
        }
        ensure!(dropped.load(Ordering::Acquire)>0,"fixture did not drop encrypted RTP");
        ensure!(stats.fec_enabled.load(Ordering::Acquire)&&stats.fec_packets.load(Ordering::Acquire)>0,"no negotiated protection arrived");
        ensure!(stats.fec_recovered.load(Ordering::Acquire)>=1,"lost fragment was not reconstructed");
        ensure!(stats.decoded_total.load(Ordering::Acquire)==12&&stats.errors.load(Ordering::Acquire)==0,"FEC damaged the video references");
        Ok::<_,anyhow::Error>(())
    }.await;
    host.close().await;viewer.close().await;
    if let Some(task)=relay_task{task.abort();let _=task.await;}
    result.expect("encrypted FEC regression");
}

/// Same session statistics, three actual encrypted UDP receiver generations.
/// This exercises on->on and on->off without process-global environment races.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn encrypted_receiver_generations_keep_connection_owned_observations() {
    use std::{collections::HashSet, path::PathBuf};
    use uu_media::VideoEncoder;
    let ip: IpAddr = "127.0.0.1".parse().unwrap();
    let stats=Arc::new(MediaStats::default());
    stats.decoder_preference.set(uu_media::DecoderKind::Software).unwrap();
    stats.native_codec_capabilities.set(vec![VideoCodec::H264]).unwrap();
    let retained=std::env::var_os("UU_WIRE_TEST_EVIDENCE_DIR");
    let root=retained.as_ref().map(PathBuf::from).unwrap_or_else(||std::env::temp_dir().join(format!("uu-wire-generations-{}",uuid::Uuid::new_v4())));
    std::fs::create_dir(&root).unwrap();
    let mut previous:Vec<(PathBuf,Vec<u8>,std::sync::Weak<crate::socket_trace::Observation>)>=Vec::new();
    let mut instances=HashSet::new();
    for generation in 0..3u64 {
        stats.native_media_epoch.store(generation,Ordering::Release);
        let path=root.join(format!("generation-{generation}.json"));
        let trace=(generation<2).then(||Arc::new(crate::socket_trace::Observation::new(path.clone())));
        let weak=trace.as_ref().map(Arc::downgrade);
        let mut host=NativePeer::new_lan(ip,ip,None,true,None).await.unwrap();
        let mut viewer=NativePeer::new_lan_io(ip,ip,Some(stats.clone()),false,trace,generation<2).await.unwrap();
        let (frames,mut images)=crate::media::frame_channel();
        viewer.receiver(frames,stats.clone()).await.unwrap();
        let mut sender=host.sender_for_color(Arc::new(AtomicBool::new(false)),VideoCodec::H264,false).await.unwrap();
        let before=stats.decoded_total.load(Ordering::Acquire);
        let result=async {
            let offer=viewer.description(true).await?;host.remote(offer,ip).await?;
            let answer=host.description(false).await?;viewer.remote(answer,ip).await?;
            tokio::try_join!(host.connected(),viewer.connected())?;
            let mut encoder=uu_media::H264Encoder::with_frame_rate(1000,60)?;
            for index in 0..4u64 {
                let pixels=vec![128;64*64*3/2];
                for frame in encoder.encode(&uu_media::RawFrame::new_i420(64,64,pixels,uu_media::Timestamp(index*16_666)))? {
                    sender.frame(&frame).await?;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            timeout(Duration::from_secs(4),async {
                while stats.decoded_total.load(Ordering::Acquire)<before+4 {
                    while images.try_recv_image().is_ok(){}
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            }).await.context("generation did not decode")?;
            Ok::<_,anyhow::Error>(())
        }.await;
        host.close().await;
        timeout(Duration::from_secs(3),viewer.close()).await.expect("bounded diagnostic close");
        if let Some(io)=&viewer.io {io.stop().await.unwrap();}
        viewer.close().await;
        result.unwrap();
        drop(sender);drop(viewer);drop(host);
        for (old,bytes,_) in &previous {assert_eq!(std::fs::read(old).unwrap(),*bytes);}
        if let Some(weak)=weak {
            let bytes=std::fs::read(&path).unwrap();
            let data:serde_json::Value=serde_json::from_slice(&bytes).unwrap();
            assert!(instances.insert(data["instance"].as_str().unwrap().to_owned()));
            let primary=data["primary"].as_array().unwrap();assert!(!primary.is_empty());
            for row in primary {
                assert_eq!(row["epoch"],generation);
                let matches=data["wire"].as_array().unwrap().iter().filter(|w|w["direction"]==0&&w["outcome"]==0&&w["header"]==row["header"]).count();
                assert_eq!(matches,1,"authenticated identity needs its own unique socket read");
            }
            timeout(Duration::from_secs(2),async{while weak.upgrade().is_some(){tokio::task::yield_now().await;}}).await.expect("old connection must release observer");
            previous.push((path,bytes,weak));
        } else {assert!(!path.exists());}
    }
    assert_eq!(instances.len(),2);
    for (_,_,weak) in previous {assert!(weak.upgrade().is_none());}
    if retained.is_none(){std::fs::remove_dir_all(root).unwrap();}
}
