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
use webrtc::api::interceptor_registry::register_default_interceptors;
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
use uu_media::VideoDecoder;
use webrtc::media::io::sample_builder::SampleBuilder;
use webrtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use webrtc::rtcp::payload_feedbacks::full_intra_request::{FirEntry, FullIntraRequest};
use webrtc::rtp::codecs::h264::H264Packet;
use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;
use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;
use webrtc::rtp_transceiver::RTCRtpTransceiverInit;

pub type FrameSender = tokio::sync::mpsc::UnboundedSender<(u32, u32, u32, Vec<u8>)>;

pub struct ControllerMedia {
    pub pc: Arc<RTCPeerConnection>,
    pub data_channel: Arc<webrtc::data_channel::RTCDataChannel>,
}

impl ControllerMedia {
    /// Build the PeerConnection with the ICE servers returned by `success`.
    pub async fn new(ice: Vec<RTCIceServer>) -> Result<Self> {
        Self::new_with_frames(ice, None).await
    }

    pub async fn new_with_frames(
        ice: Vec<RTCIceServer>,
        frames: Option<FrameSender>,
    ) -> Result<Self> {
        let mut media = MediaEngine::default();
        media.register_default_codecs()?;
        // Ensure H264/Opus are present even if the default set changes.
        let _ = (MIME_TYPE_H264, MIME_TYPE_OPUS);

        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut media)?;
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

        let pc_track = pc.clone();
        let frames_tx = frames.clone();
        let track_counter = Arc::new(AtomicU32::new(0));
        pc.on_track(Box::new(move |track, _receiver, _transceiver| {
            let pc = pc_track.clone();
            let frame_tx = frames_tx.clone();
            let track_idx = track_counter.fetch_add(1, Ordering::Relaxed);
            Box::pin(async move {
                println!("track: {:?}", track.kind());
                // A 1080p access unit is often dozens of RTP packets. Keep a
                // wider reorder window than the low bitrate default so a few
                // late packets do not permanently lose the reference frame.
                let dbg = std::env::var("UU_MEDIA_DEBUG").map(|v| v != "0").unwrap_or(false);
                let mut dump_file = std::env::var("UU_DUMP_H264")
                    .ok()
                    .and_then(|p| std::fs::File::create(p).ok());
                let mut builder = SampleBuilder::new(512, H264Packet::default(), 90000)
                    .with_max_time_delay(std::time::Duration::from_millis(500));
                let mut decoder = uu_media::H264Decoder::new().ok();
                let mut packets = 0u64;
                let mut frames = 0u64;
                let mut samples = 0u64;
                let mut decode_errors = 0u32;
                let mut pli_started = false;
                let mut last_seq: std::collections::HashMap<u32, u16> = std::collections::HashMap::new();
                let mut lost = 0u64;
                let mut expected = 0u64;
                while let Ok((pkt, _)) = track.read_rtp().await {
                    packets += 1;
                    let ssrc = pkt.header.ssrc;
                    let seq = pkt.header.sequence_number;
                    if let Some(prev) = last_seq.insert(ssrc, seq) {
                        let gap = seq.wrapping_sub(prev).wrapping_sub(1);
                        if gap > 0 && gap < 3000 {
                            lost += gap as u64;
                            expected += gap as u64;
                            if lost <= 40 || packets % 500 == 0 {
                                println!(
                                    "RTP gap ssrc={ssrc} seq {prev}->{seq} missing={gap} total_lost={lost}"
                                );
                            }
                        }
                    }
                    if packets == 1 || packets % 300 == 0 {
                        println!(
                            "rtp ssrc={} seq={} ts={} len={}",
                            pkt.header.ssrc,
                            pkt.header.sequence_number,
                            pkt.header.timestamp,
                            pkt.payload.len()
                        );
                    }
                    if !pli_started {
                        pli_started = true;
                        let pc = pc.clone();
                        let ssrc = pkt.header.ssrc;
                        let sender_ssrc = u32::from_be_bytes(
                            uuid::Uuid::new_v4().as_bytes()[..4].try_into().unwrap(),
                        );
                        tokio::spawn(async move {
                            let mut fir_seq = 1u8;
                            // The first RTP packet is often a delta frame
                            // because the controller joined mid-GOP. Ask for
                            // an IDR immediately, then retry quickly while
                            // the encoder switches to a fresh GOP.
                            loop {
                                let delay = if fir_seq <= 10 {
                                    std::time::Duration::from_millis(300)
                                } else {
                                    std::time::Duration::from_secs(2)
                                };
                                tokio::time::sleep(delay).await;
                                let pkts: Vec<Box<dyn webrtc::rtcp::packet::Packet + Send + Sync>> = vec![
                                    Box::new(PictureLossIndication { sender_ssrc, media_ssrc: ssrc }),
                                    // A few controlled implementations key
                                    // feedback to the source SSRC even though
                                    // RFC 4585 permits an independent sender
                                    // SSRC. Send that interoperable variant as
                                    // well.
                                    Box::new(PictureLossIndication { sender_ssrc: ssrc, media_ssrc: ssrc }),
                                    Box::new(FullIntraRequest {
                                        sender_ssrc,
                                        media_ssrc: ssrc,
                                        fir: vec![FirEntry { ssrc, sequence_number: fir_seq, ..Default::default() }],
                                    }),
                                ];
                                fir_seq = fir_seq.wrapping_add(1);
                                if let Err(e) = pc.write_rtcp(&pkts).await {
                                    eprintln!("RTCP PLI/FIR failed: {e}");
                                }
                            }
                        });
                    }
                    builder.push(pkt);
                    while let Some(sample) = builder.pop() {
                        samples += 1;
                        if let Some(f) = dump_file.as_mut() {
                            use std::io::Write;
                            let _ = f.write_all(&sample.data);
                            let _ = f.flush();
                        }
                        if samples <= 3 {
                            let head: Vec<String> = sample.data.iter().take(12).map(|b| format!("{b:02x}")).collect();
                            println!("sample {samples} len={} head={}", sample.data.len(), head.join(" "));
                        }
                        let nal = uu_media::EncodedFrame::detect_h264_keyframe(&sample.data);
                        if dbg && samples <= 60 {
                            println!(
                                "  S#{} ts={:?} len={} key={} nals={:?}",
                                samples,
                                sample.timestamp,
                                sample.data.len(),
                                nal,
                                h264_nal_types(&sample.data)
                            );
                        }
                        let Some(dec) = decoder.as_mut() else { continue };
                        let ef = uu_media::EncodedFrame {
                            is_keyframe: nal,
                            data: sample.data.clone(),
                            pts: uu_media::Timestamp(0),
                        };
                        match dec.decode(&ef) {
                            Ok(Some(frame)) => {
                                frames += 1;
                                if dbg && frames <= 60 {
                                    println!("  D#{} ok {}x{}", frames, frame.width, frame.height);
                                }
                                if let Some(tx) = &frame_tx {
                                    let rgba = uu_media::convert::i420_to_rgba(
                                        &frame.data, frame.width, frame.height);
                                    let _ = tx.send((track_idx, frame.width, frame.height, rgba));
                                }
                                if frames == 1 {
                                    println!("decoded first frame {}x{}", frame.width, frame.height);
                                    save_ppm(&frame, "/tmp/uu_rust_frame.ppm");
                                } else if frames % 30 == 0 {
                                    println!("decoded {frames} frames");
                                    save_ppm(&frame, "/tmp/uu_rust_latest.ppm");
                                }
                            }
                            Ok(None) => {
                                if dbg && samples <= 60 {
                                    println!("  D none");
                                }
                            }
                            Err(e) => {
                                decode_errors = decode_errors.saturating_add(1);
                                if dbg && decode_errors <= 40 {
                                    println!("  D#ERR {e}");
                                } else if decode_errors == 1 || decode_errors % 30 == 0 {
                                    println!("decode error ({decode_errors}): {e}");
                                }
                            }
                        }
                    }
                }
                println!(
                    "track done: packets={packets} samples={samples} frames={frames} decode_errors={decode_errors} rtp_lost={lost}"
                );
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
        for label in [
            "CONTROL_DATA_CHANNEL",
            "TEXT_DATA_CHANNEL",
            "STREAMER_DATA_CHANNEL",
            "FILE_DATA_CHANNEL",
            "BINARY_DATA_CHANNEL",
        ] {
            let ch = pc.create_data_channel(label, None).await?;
            if label != "CONTROL_DATA_CHANNEL" {
                continue;
            }
            let open_ch = ch.clone();
            let open_seq = seq.clone();
            ch.on_open(Box::new(move || {
                let ch = open_ch.clone();
                let seq = open_seq.clone();
                Box::pin(async move {
                    println!("CONTROL_DATA_CHANNEL open");
                    let s = seq.fetch_add(1, Ordering::Relaxed);
                    let _ = ch.send(&bytes::Bytes::from(crate::control_wire::echo_request(s))).await;
                })
            }));
            ch.on_close(Box::new(|| {
                println!("CONTROL_DATA_CHANNEL closed");
                Box::pin(async {})
            }));
            let msg_ch = ch.clone();
            let msg_seq = seq.clone();
            ch.on_message(Box::new(move |msg: webrtc::data_channel::data_channel_message::DataChannelMessage| {
                let ch = msg_ch.clone();
                let seq = msg_seq.clone();
                Box::pin(async move {
                    if !msg.is_string {
                        let head: Vec<String> = msg.data.iter().take(16).map(|b| format!("{b:02x}")).collect();
                        if let Some(req) = crate::control_wire::echo_request_seq(&msg.data) {
                            let s = seq.fetch_add(1, Ordering::Relaxed);
                            let resp = crate::control_wire::echo_response(s, req);
                            let _ = ch.send(&bytes::Bytes::from(resp)).await;
                            println!("CONTROL echo request seq={req} answered (out seq={s})");
                        } else {
                            println!("CONTROL recv len={} head={}", msg.data.len(), head.join(" "));
                        }
                    }
                })
            }));
            control_dc = Some(ch);
        }
        let dc = control_dc.expect("CONTROL_DATA_CHANNEL must be created");

        Ok(Self { pc, data_channel: dc })
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


/// Enumerate the H.264 NAL unit types in an Annex-B access unit (debug aid).
fn h264_nal_types(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 3 < data.len() {
        let sc = if data[i..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if data[i..].starts_with(&[0, 0, 1]) {
            3
        } else {
            i += 1;
            continue;
        };
        let h = i + sc;
        if h < data.len() {
            out.push(data[h] & 0x1f);
        }
        i = h + 1;
    }
    out
}

/// Write an I420 frame as a binary PPM (P6) so it can be inspected without a
/// UI dependency. Convert with `ffmpeg -i /tmp/uu_rust_frame.ppm out.png`.
fn save_ppm(frame: &uu_media::RawFrame, path: &str) {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let y = &frame.data[..w * h];
    let cw = w / 2;
    let ch = h / 2;
    let u = &frame.data[w * h..w * h + cw * ch];
    let v = &frame.data[w * h + cw * ch..w * h + cw * ch * 2];
    let mut out = Vec::with_capacity(w * h * 3 + 32);
    out.extend_from_slice(format!("P6\n{w} {h}\n255\n").as_bytes());
    let cl = |x: i32| x.clamp(0, 255) as u8;
    for j in 0..h {
        for i in 0..w {
            let yy = y[j * w + i] as i32;
            let uu = u[(j / 2) * cw + i / 2] as i32 - 128;
            let vv = v[(j / 2) * cw + i / 2] as i32 - 128;
            let r = yy + (1.402 * vv as f32) as i32;
            let g = yy - (0.344136 * uu as f32) as i32 - (0.714136 * vv as f32) as i32;
            let b = yy + (1.772 * uu as f32) as i32;
            out.extend_from_slice(&[cl(r), cl(g), cl(b)]);
        }
    }
    let _ = std::fs::write(path, out);
}


impl ControllerMedia {
    /// Send a raw PCK frame over the WebRTC data channel (control hypothesis).
    pub async fn send_control(&self, data: Vec<u8>) -> Result<()> {
        self.data_channel.send(&bytes::Bytes::from(data)).await?;
        Ok(())
    }
}
