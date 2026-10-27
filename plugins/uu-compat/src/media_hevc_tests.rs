//! Real SDP/DTLS/SRTP codec negotiation and generated-video switching tests.
use super::*;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters;
use webrtc::track::track_local::{TrackLocal, TrackLocalContext};

#[derive(Default)]
struct Source(std::sync::Mutex<Option<TrackLocalContext>>);
#[async_trait::async_trait]
impl TrackLocal for Source {
    async fn bind(
        &self,
        context: &TrackLocalContext,
    ) -> webrtc::error::Result<RTCRtpCodecParameters> {
        let codec = context
            .codec_parameters()
            .iter()
            .find(|p| p.capability.mime_type.eq_ignore_ascii_case(MIME_TYPE_H264))
            .or_else(|| context.codec_parameters().first())
            .unwrap()
            .clone();
        *self.0.lock().unwrap() = Some(context.clone());
        Ok(codec)
    }
    async fn unbind(&self, _: &TrackLocalContext) -> webrtc::error::Result<()> {
        self.0.lock().unwrap().take();
        Ok(())
    }
    fn id(&self) -> &str {
        "video_0"
    }
    fn stream_id(&self) -> &str {
        "generated"
    }
    fn kind(&self) -> RTPCodecType {
        RTPCodecType::Video
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

async fn connect(
    local_hevc: bool,
    host_hevc: bool,
) -> (
    ControllerMedia,
    RTCPeerConnection,
    Arc<Source>,
    Arc<MediaStats>,
    FrameReceiver,
) {
    crate::install_crypto_provider();
    let stats = Arc::new(MediaStats::default());
    stats.hevc_enabled.set(local_hevc).unwrap();
    let (tx, rx) = frame_channel();
    let media = ControllerMedia::new_with_frames(vec![], Some(tx), Some(stats.clone()))
        .await
        .unwrap();
    let offer = media.offer().await.unwrap();
    assert_eq!(offer.contains("H265/90000"), local_hevc);
    let mut engine = MediaEngine::default();
    register_receive_codecs(&mut engine, host_hevc).unwrap();
    let host = APIBuilder::new()
        .with_media_engine(engine)
        .build()
        .new_peer_connection(RTCConfiguration::default())
        .await
        .unwrap();
    let source = Arc::new(Source::default());
    host.add_track(source.clone()).await.unwrap();
    host.set_remote_description(RTCSessionDescription::offer(offer).unwrap())
        .await
        .unwrap();
    let mut gathered = host.gathering_complete_promise().await;
    host.set_local_description(host.create_answer(None).await.unwrap())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), gathered.recv())
        .await
        .unwrap();
    media
        .set_answer(&host.local_description().await.unwrap().sdp)
        .await
        .unwrap();
    (media, host, source, stats, rx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hevc_offer_requires_verified_local_support_and_h264_only_peer_falls_back() {
    for (local, remote) in [(false, true), (true, false), (true, true)] {
        let (media, host, _, _, _) = connect(local, remote).await;
        assert_eq!(
            media.hevc_negotiated.load(Ordering::Acquire),
            local && remote
        );
        let parameters = host.get_senders().await[0].get_parameters().await;
        assert!(parameters
            .rtp_parameters
            .codecs
            .iter()
            .any(|p| p.capability.mime_type.eq_ignore_ascii_case(MIME_TYPE_H264)));
        host.close().await.unwrap();
        media.pc.close().await.unwrap();
    }
}

#[test]
fn video_formats_reject_interleaving_and_unsupported_av1_profiles() {
    use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
    for (mime, fmtp, valid) in [
        ("video/H265", "", true),
        ("video/h265", "sprop-max-don-diff=0", true),
        ("video/H265", "sprop-max-don-diff=1", false),
        ("video/AV1", "", true),
        ("video/AV1", "profile=0;level-idx=14;tier=1", true),
        ("video/AV1", "profile=1", false),
        ("video/rtx", "", false),
    ] {
        assert_eq!(
            uu_core::plugin_backend::receive_video_codec(&RTCRtpCodecCapability {
                mime_type: mime.into(),
                sdp_fmtp_line: fmtp.into(),
                ..Default::default()
            })
            .is_some(),
            valid
        );
    }
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Windows HEVC MFT; uses only generated video over loopback"]
async fn windows_generated_srtp_switches_h264_hevc_h264_on_one_track() {
    use uu_media::{RawFrame, Timestamp, VideoEncoder};
    use webrtc::rtp::{header::Header, packet::Packet, packetizer::Payloader};
    uu_media::hevc::probe_decoder().unwrap();
    uu_media::d3d11_frame::set_rendering(true);
    let (media, host, source, stats, _frames) = connect(true, true).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while host.connection_state() != RTCPeerConnectionState::Connected {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let context = source.0.lock().unwrap().clone().unwrap();
    let writer = context.write_stream().unwrap();
    let mut sequence = 65_510u16;
    let mut timestamp = 9_000_000u32;
    for mime in ["video/H264", "video/H265", "video/H264"] {
        let payload_type = context
            .codec_parameters()
            .iter()
            .find(|p| p.capability.mime_type.eq_ignore_ascii_case(mime))
            .unwrap()
            .payload_type;
        let mut units = Vec::new();
        if mime == "video/H264" {
            let mut encoder = uu_media::H264Encoder::new(500).unwrap();
            for i in 0..24 {
                units.extend(
                    encoder
                        .encode(&RawFrame::new_i420(
                            128,
                            128,
                            vec![128; 128 * 128 * 3 / 2],
                            Timestamp(i * 16_667),
                        ))
                        .unwrap()
                        .into_iter()
                        .map(|f| f.data),
                );
            }
        } else {
            let data = uu_media::hevc::PROBE;
            let mut starts: Vec<_> = (0..data.len().saturating_sub(4))
                .filter(|&i| data[i..].starts_with(&[0, 0, 0, 1, 70]))
                .collect();
            starts.push(data.len());
            for _ in 0..6 {
                for span in starts.windows(2) {
                    units.push(bytes::Bytes::copy_from_slice(&data[span[0]..span[1]]));
                }
            }
        }
        let before = stats.decoded_total.load(Ordering::Relaxed);
        let mut payloader = webrtc::rtp::codecs::h264::H264Payloader::default();
        for unit in units {
            let payloads = if mime == "video/H264" {
                payloader.payload(40, &unit).unwrap()
            } else {
                let mut offsets: Vec<_> = (0..unit.len().saturating_sub(3))
                    .filter(|&i| unit[i..].starts_with(&[0, 0, 0, 1]))
                    .collect();
                offsets.push(unit.len());
                let mut payloads = Vec::new();
                for pair in offsets.windows(2) {
                    let nal = &unit[pair[0] + 4..pair[1]];
                    if nal.len() <= 40 {
                        payloads.push(bytes::Bytes::copy_from_slice(nal));
                    } else {
                        let chunks: Vec<_> = nal[2..].chunks(37).collect();
                        for (i, chunk) in chunks.iter().enumerate() {
                            let mut fu = vec![
                                (nal[0] & 0x81) | 98,
                                nal[1],
                                ((nal[0] >> 1) & 63)
                                    | if i == 0 {
                                        0x80
                                    } else if i + 1 == chunks.len() {
                                        0x40
                                    } else {
                                        0
                                    },
                            ];
                            fu.extend_from_slice(chunk);
                            payloads.push(fu.into());
                        }
                    }
                }
                payloads
            };
            for (i, payload) in payloads.iter().enumerate() {
                writer
                    .write_rtp(&Packet {
                        header: Header {
                            version: 2,
                            ssrc: context.ssrc(),
                            payload_type,
                            sequence_number: sequence,
                            timestamp,
                            marker: i + 1 == payloads.len(),
                            ..Default::default()
                        },
                        payload: payload.clone(),
                    })
                    .await
                    .unwrap();
                sequence = sequence.wrapping_add(1);
            }
            timestamp += 1500;
            tokio::time::sleep(Duration::from_millis(17)).await;
        }
        tokio::time::timeout(Duration::from_secs(5),async {
            while stats.decoded_total.load(Ordering::Relaxed)<before+10 || *stats.codec.lock().unwrap()!=mime {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await.expect("codec transition must produce decoded frames through production SRTP/receive/decoder path");
        assert_eq!(stats.res(), (128, 128));
        assert_eq!(stats.errors.load(Ordering::Relaxed), 0);
        eprintln!(
            "generated SRTP {mime}: {} frames, no errors",
            stats.decoded_total.load(Ordering::Relaxed) - before
        );
    }
    host.close().await.unwrap();
    media.pc.close().await.unwrap();
    uu_media::d3d11_frame::set_rendering(false);
}
