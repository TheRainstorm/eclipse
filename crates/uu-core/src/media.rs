//! Controller-side WebRTC media plane (webrtc-rs).
//!
//! Mirrors the validated `tools/linux_controller/proto.py` flow: five
//! recv-only video transceivers, one audio transceiver and a data channel.
//! The signalling layer (`socketio.rs`) owns the SDP/ICE exchange; this module
//! only wraps the PeerConnection.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use webrtc::api::media_engine::{MIME_TYPE_H264, MIME_TYPE_OPUS};
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtcp::payload_feedbacks::full_intra_request::{FirEntry, FullIntraRequest};
use webrtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use webrtc::rtcp::payload_feedbacks::receiver_estimated_maximum_bitrate::ReceiverEstimatedMaximumBitrate;
use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;

pub use crate::display_frames::{channel as frame_channel, FrameReceiver, FrameSender};
pub use uu_media::DecoderKind;

/// Live media statistics shared between the decode task and the UI (HUD).
/// Fixed-point where noted so the hot path stays lock-free.
#[derive(Default)]
pub struct MediaStats {
    /// A protocol adapter can require held-modifier refresh. Native sessions
    /// leave it false; the UI never identifies a particular provider.
    pub display_track_binding:std::sync::atomic::AtomicBool,
    pub refresh_modifiers:std::sync::atomic::AtomicBool,
    pub native_cursor:crate::native_cursor::Client,
    pub native_hdr_requested:std::sync::OnceLock<bool>,
    pub native_hdr_capable:std::sync::atomic::AtomicBool,
    pub native_hdr_codecs:std::sync::OnceLock<Vec<uu_proto::media::VideoCodec>>,
    pub native_hdr_response:std::sync::Mutex<Option<crate::native_color::Response>>,
    pub native_progress:crate::native_controller::Progress,
    pub decoder_preference:std::sync::OnceLock<DecoderKind>,
    pub host_admin:crate::host_admin::Client,
    pub native_quality_request:std::sync::OnceLock<crate::native_quality::Request>,
    pub native_rate_request:std::sync::OnceLock<crate::native_quality::RateRequest>,
    pub native_rate_response:std::sync::Mutex<Option<crate::native_quality::RateResponse>>,
    pub native_quality_response:std::sync::Mutex<Option<crate::native_quality::Response>>,
    pub native_quality_current:std::sync::Mutex<Option<crate::native_quality::Update>>,
    pub native_quality_applied:std::sync::Mutex<Option<crate::native_quality::Applied>>,
    pub native_codec_capabilities:std::sync::OnceLock<Vec<uu_proto::media::VideoCodec>>,
    pub native_media_epoch:AtomicU64,
    /// Retires cached-image observations when the native controller ends.
    pub native_media_retired:std::sync::atomic::AtomicBool,
    pub native_displays:crate::native_displays::Client,
    pub native_timing:crate::native_timing::ClientTiming,
    pub pairing:crate::device_auth::PairingDisplay,
    pub connection:crate::connection_info::Details,
    pub capabilities:crate::capabilities::Capabilities,
    pub native_input_available:std::sync::atomic::AtomicBool,
    /// Host-confirmed source target; zero when only the client's request is known.
    pub configured_fps:AtomicU32,
    /// Immutable per-session output policy, supplied before signalling starts.
    pub output_resolution: std::sync::OnceLock<crate::video_options::CaptureResolution>,
    /// Requested preference is immutable after spawn; enabled only after the
    /// local HEVC decoder actually produces a valid hardware image.
    pub prefer_hevc: std::sync::atomic::AtomicBool,
    pub hevc_enabled: std::sync::OnceLock<bool>,
    pub diagnostics: crate::session_diagnostics::SessionDiagnostics,
    pub pipeline_trace:crate::pipeline_trace::Trace,
    pub video_e2e:crate::video_e2e::Client,
    pub native_frame_trace_supported: std::sync::atomic::AtomicBool,
    pub native_frame_trace: std::sync::Mutex<Option<crate::frame_trace::Report>>,
    pub(crate) native_frame_trace_received: std::sync::Mutex<Option<(u64,std::time::Instant)>>,
    pub control_open: AtomicU32,
    /// Snapshot of the user's connection policy: 0 preserve, 1 exclusive UU.
    pub display_mode: AtomicU32,
    pub display_responses: std::sync::Mutex<std::collections::VecDeque<(u64,u32,i32)>>,
    pub desktop_capture_source: std::sync::Mutex<Option<i32>>,
    pub exclusive_owned: std::sync::atomic::AtomicBool,
    pub exclusive_result: AtomicU64,
    pub audio: Arc<uu_media::audio::AudioControl>,
    pub clipboard: Arc<crate::clipboard::Clipboard>,
    pub gamepad: Arc<crate::native_gamepad::Stats>,
    pub cursor: std::sync::Mutex<Option<(u64,crate::cursor::Shape)>>,
    pub input_messages_sent: AtomicU64,
    pub input_send_errors: AtomicU64,
    pub selected_video_track:AtomicU32,
    pub width: AtomicU32,
    pub height: AtomicU32,
    /// Source (network) frame rate x100.
    pub src_fps_x100: AtomicU32,
    /// Decoded frame rate x100.
    pub dec_fps_x100: AtomicU32,
    /// Average decode time in microseconds.
    pub decode_us: AtomicU32,
    pub frame_cadence: std::sync::Mutex<crate::frame_cadence::Snapshot>,
    pub decoded_display_skips: AtomicU64,
    /// Received video bitrate in kbit/s.
    pub bitrate_kbps: AtomicU32,
    /// RTP packets received.
    pub packets: AtomicU64,
    /// RTP packets estimated lost (sequence gaps).
    pub lost: AtomicU64,
    /// Total frames decoded.
    pub decoded_total: AtomicU64,
    /// Decode errors.
    pub errors: AtomicU64,
    /// Retired feedback metadata when the bounded TWCC collector overflows.
    pub twcc_metadata_drops: Arc<AtomicU64>,
    pub twcc_received_packets: Arc<AtomicU64>,
    /// Application CONTROL round-trip estimate, not ICMP or pure wire time.
    pub rtt_ms: AtomicU32,
    pub rtt_us: AtomicU32,
    pub rtt_deviation_us: AtomicU32,
    pub rtt_distribution_us: std::sync::Mutex<Option<[u32; 5]>>,
    pub control_probe_send_us: AtomicU32,
    pub rtt_updated_ms: AtomicU64,
    /// Capture-setting response: request_id << 1 | accepted.
    pub settings_response: AtomicU64,
    pub remote_screens: std::sync::Mutex<crate::screen_control::RemoteScreens>,
    pub remote_screens_revision: AtomicU64,
    /// Unrecovered RTP packet loss in the current receive window, parts/million.
    pub loss_ppm: AtomicU32,
    /// Original RTP before FEC/RTX; independent of the post-repair loss above.
    pub network_loss:std::sync::Mutex<Option<crate::network_loss::Snapshot>>,
    pub codec: std::sync::Mutex<String>,
    pub nack_requests: AtomicU64,
    pub rtx_packets: AtomicU64,
    pub rtx_drops: AtomicU64,
    pub fec_enabled: std::sync::atomic::AtomicBool,
    pub fec_packets: AtomicU64,
    pub fec_bytes: AtomicU64,
    pub fec_recovered: AtomicU64,
    pub fec_drops: AtomicU64,
    pub reordered: AtomicU64,
    pub late_packets: AtomicU64,
    pub damaged_frames: AtomicU64,
    pub queue_drops: AtomicU64,
    pub recovery_receive_damage:AtomicU64,
    pub recovery_decode_queue:AtomicU64,
    pub recovery_decoder_error:AtomicU64,
    pub recovery_format_change:AtomicU64,
    pub injected_loss: AtomicU64,
    pub keyframe_requests: AtomicU64,
    pub waiting_for_keyframe: AtomicU32,
    pub backend: std::sync::Mutex<String>,
}

#[derive(Clone,Copy)]
enum RecoveryCause {ReceiveDamage,DecodeQueue,DecoderError,FormatChange}

impl MediaStats {
    fn recovery(&self,cause:RecoveryCause) {
        match cause {
            RecoveryCause::ReceiveDamage=>&self.recovery_receive_damage,
            RecoveryCause::DecodeQueue=>&self.recovery_decode_queue,
            RecoveryCause::DecoderError=>&self.recovery_decoder_error,
            RecoveryCause::FormatChange=>&self.recovery_format_change,
        }.fetch_add(1,Ordering::Relaxed);
    }
    pub fn decoder_kind(&self)->DecoderKind {self.decoder_preference.get().copied().unwrap_or_else(DecoderKind::from_env)}
    pub fn select_video_track(&self,track:u32) {
        if self.selected_video_track.swap(track,Ordering::AcqRel)!=track {
            for value in [&self.src_fps_x100,&self.dec_fps_x100,&self.decode_us,&self.bitrate_kbps,&self.width,&self.height,&self.loss_ppm] {value.store(0,Ordering::Relaxed);}
            *self.frame_cadence.lock().unwrap() = Default::default();
            *self.network_loss.lock().unwrap()=None;
            self.set_backend("");
        }
    }
    pub fn set_backend(&self, backend: impl Into<String>) {
        if let Ok(mut b) = self.backend.lock() {
            *b = backend.into();
        }
    }

    pub fn backend(&self) -> String {
        self.backend.lock().map(|b| b.clone()).unwrap_or_default()
    }

    pub fn src_fps(&self) -> f32 {
        self.src_fps_x100.load(Ordering::Relaxed) as f32 / 100.0
    }
    pub fn dec_fps(&self) -> f32 {
        self.dec_fps_x100.load(Ordering::Relaxed) as f32 / 100.0
    }
    pub fn decode_ms(&self) -> f32 {
        self.decode_us.load(Ordering::Relaxed) as f32 / 1000.0
    }
    pub fn res(&self) -> (u32, u32) {
        (
            self.width.load(Ordering::Relaxed),
            self.height.load(Ordering::Relaxed),
        )
    }
}

pub(crate) async fn receive_audio(track: Arc<webrtc::track::track_remote::TrackRemote>, stats: Option<Arc<MediaStats>>) {
    let control = stats.as_ref().map(|s|s.audio.clone()).unwrap_or_default();
    let mut playback=None;
    while let Ok((packet,_)) = track.read_rtp().await {
    let codec=track.codec().capability;
    let red=codec.mime_type.eq_ignore_ascii_case("audio/red");
    if !red && !codec.mime_type.eq_ignore_ascii_case(MIME_TYPE_OPUS) { return; }
    let opus_payload=if red {match codec.sdp_fmtp_line.split('/').next().and_then(|v|v.parse::<u8>().ok()).filter(|pt|*pt<128){Some(pt)=>pt,None=>return}}else{111};
        let tx=playback.get_or_insert_with(||uu_media::audio::playback(control.clone()));
        if control.stopped() { break; }
        let number=control.packets.fetch_add(1,Ordering::Relaxed)+1;
        let period=control.test_loss_every.load(Ordering::Relaxed);
        if period>=2 && number%u64::from(period)==0 {control.injected_loss.fetch_add(1,Ordering::Relaxed);continue;}
        let received=std::time::Instant::now();
        let (payload,backup)=if red {match uu_media::audio_red::unpack(opus_payload,&packet.payload) {
            Ok(parts)=>(parts.primary,parts.previous),
            Err(_)=>{control.dropped.fetch_add(1,Ordering::Relaxed);continue;},
        }}else{(packet.payload.as_ref(),None)};
        if let Some((offset,data))=backup {
            let repair=uu_media::audio::Packet {repaired:true,sequence:packet.header.sequence_number.wrapping_sub(1),timestamp:packet.header.timestamp.wrapping_sub(u32::from(offset)),payload:data.to_vec(),received};
            if let Err(std::sync::mpsc::TrySendError::Full(_))=tx.try_send(repair) {control.dropped.fetch_add(1,Ordering::Relaxed);}
        }
        let packet = uu_media::audio::Packet {repaired:false,sequence:packet.header.sequence_number,timestamp:packet.header.timestamp,
            payload:payload.to_vec(),received};
        match tx.try_send(packet) {
            Ok(()) => {},
            Err(std::sync::mpsc::TrySendError::Full(_)) => { control.dropped.fetch_add(1,Ordering::Relaxed); },
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => break,
        }
    }
}

pub(crate) fn video_codec(capability: &webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability) -> Option<uu_proto::media::VideoCodec> {
    use uu_proto::media::VideoCodec;
    if capability.mime_type.eq_ignore_ascii_case("video/PYROWAVE") {return crate::native_media::pyrowave_format(&capability.sdp_fmtp_line).then_some(VideoCodec::Pyrowave);}
    if capability.mime_type.eq_ignore_ascii_case(MIME_TYPE_H264) { return Some(VideoCodec::H264); }
    if capability.mime_type.eq_ignore_ascii_case("video/AV1") {
        if capability.sdp_fmtp_line.split(';').any(|p|p.trim().split_once('=').is_some_and(|(k,v)|k.trim().eq_ignore_ascii_case("profile")&&v.trim()!="0")){return None;}
        return Some(VideoCodec::Av1);
    }
    if !capability.mime_type.eq_ignore_ascii_case("video/H265") { return None; }
    // This receiver negotiates the non-interleaved, single-layer format.
    if capability.sdp_fmtp_line.split(';').any(|p| p.trim().split_once('=')
        .is_some_and(|(k,v)| k.trim().eq_ignore_ascii_case("sprop-max-don-diff") && v.trim() != "0")) { return None; }
    Some(VideoCodec::Hevc)
}

pub(crate) struct PreparedVideoDecoder {
    pub decoder: Box<dyn uu_media::VideoDecoder>,
    pub visible: Option<[u32;2]>,
}

pub(crate) async fn receive_video(
    pc: Arc<RTCPeerConnection>,
    track: Arc<webrtc::track::track_remote::TrackRemote>,
    track_idx: u32,
    frame_tx: Option<FrameSender>,
    stats: Option<Arc<MediaStats>>,
    repairs: Arc<crate::rtx::RepairBus>,
    prepared_decoder: Option<PreparedVideoDecoder>,
    fec: Option<Arc<crate::native_fec::Receive>>,
    socket_trace: Option<Arc<crate::socket_trace::Observation>>,
) {
    use crate::video_receiver::VideoReceiver;
    use std::time::Instant;
    use uu_proto::media::VideoCodec;
    if track.kind() != RTPCodecType::Video { return; }
    let capability = track.codec().capability;
    let Some(mut codec) = video_codec(&capability) else { return; };
    #[cfg(windows)]
    let _timer_resolution = {
        let timer = crate::video_timing::VideoTimerResolution::request();
        if let Some(s) = &stats {
            s.diagnostics.record(serde_json::json!({"event":"video_timer_resolution",
                "track":track_idx, "requested_ms":1, "result":timer.result()}));
        }
        timer
    };
    let debug = std::env::var("UU_MEDIA_DEBUG").is_ok_and(|v| v != "0");
    if let Some(s) = stats.as_ref().filter(|s|s.selected_video_track.load(Ordering::Acquire)==track_idx) {
        *s.codec.lock().unwrap() = track.codec().capability.mime_type.clone();
    }
    let generation = Arc::new(AtomicU64::new(0));
    let request_generation = Arc::new(crate::decode_gate::RecoverySignal::default());
    let refresh = Arc::new(tokio::sync::Notify::new());
    let backlog=Arc::new(crate::decode_backlog::Budget::default());
    let worker_backlog=backlog.clone();
    let (sample_tx, mut sample_rx) = tokio::sync::mpsc::channel::<(bytes::Bytes, u32, u64, u64, Instant, VideoCodec,crate::decode_backlog::Permit)>(crate::decode_backlog::MAX_FRAMES);
    let source_ssrc=track.ssrc();
    let trace_epoch=stats.as_ref().map_or(0,|s|s.native_media_epoch.load(Ordering::Acquire));
    let worker_stats = stats.clone();
    let worker_generation = generation.clone();
    let worker_requests = request_generation.clone();
    let worker_refresh = refresh.clone();
    let track_decoded = Arc::new(AtomicU64::new(0));
    let worker_decoded = track_decoded.clone();
    // Decode, colour conversion and driver IPC are blocking CPU/OS work. Keep
    // them off tokio's RTP and RTCP executor threads.
    let decoder_kind=worker_stats.as_ref().map_or_else(DecoderKind::from_env,|s|s.decoder_kind());
    let worker = tokio::task::spawn_blocking(move || {
        let mut codec = codec;
        // Negotiated display geometry belongs to the session, not one driver
        // instance. Preserve it on both codec changes and decoder recovery.
        let visible = prepared_decoder.as_ref().and_then(|d|d.visible);
        let prepare = |mut decoder:Box<dyn uu_media::VideoDecoder>| {
            if let Some([width,height])=visible {decoder.prepare(width,height)?;}
            Ok::<_,uu_media::CodecError>(decoder)
        };
        let create = |codec| uu_media::decoder_for_codec_with_kind(codec,decoder_kind).and_then(&prepare);
        let mut decoder = match prepared_decoder.filter(|d|d.decoder.codec()==codec).map(|d|Ok(d.decoder))
            .unwrap_or_else(|| create(codec)) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("video decoder unavailable: {e}");
                return;
            }
        };
        if let Some(s) = worker_stats.as_ref().filter(|s|s.selected_video_track.load(Ordering::Acquire)==track_idx) {
            s.set_backend(decoder.backend_name());
        }
        let mut dump = std::env::var(match codec {VideoCodec::H264=>"UU_DUMP_H264",VideoCodec::Hevc=>"UU_DUMP_HEVC",VideoCodec::Av1=>"UU_DUMP_AV1",VideoCodec::Pyrowave=>"UU_DUMP_PYROWAVE"})
            .ok()
            .and_then(|p| std::fs::File::create(p).ok());
        let mut gate = crate::decode_gate::DecodeGate::default();
        let mut reset_decoder = false;
        let mut recovery = None;
        let mut color = uu_media::color::YuvColor::default();
        let mut decoded = 0;
        let mut errors = 0;
        let mut recovery_errors = 0u32;
        let mut window = Instant::now();
        let mut window_frames = 0u64;
        let mut decode_us = 0u64;
        let mut queue_times = crate::frame_cadence::Samples::default();
        let mut pts_base = None;
        while let Some((data, timestamp, sample_epoch, sample_request, enqueued, sample_codec,_permit)) = sample_rx.blocking_recv()
        {
            if window.elapsed() >= Duration::from_millis(500) {
                let queue_summary = queue_times.take();
                if let Some(s) = worker_stats.as_ref().filter(|s|s.selected_video_track.load(Ordering::Acquire)==track_idx) {
                    s.set_backend(decoder.backend_name());
                    *s.codec.lock().unwrap() = match codec { VideoCodec::Hevc => "video/H265", VideoCodec::Av1=>"video/AV1",VideoCodec::H264 => "video/H264",VideoCodec::Pyrowave=>"video/PYROWAVE" }.into();
                    s.frame_cadence.lock().unwrap().decode_queue_ms = queue_summary;
                    s.decode_us
                        .store((decode_us / window_frames.max(1)) as u32, Ordering::Relaxed);
                    s.waiting_for_keyframe
                        .store(worker_requests.is_waiting() as u32, Ordering::Relaxed);
                }
                if debug {
                    println!("dec frames={decoded} errors={errors} waiting_idr={}", gate.waiting);
                }
                window = Instant::now();
                window_frames = 0;
                decode_us = 0;
            }
            queue_times.push(Instant::now().duration_since(enqueued).as_secs_f64() * 1000.0);
            let current_epoch = worker_generation.load(Ordering::Acquire);
            let key = match sample_codec {VideoCodec::Hevc=>uu_media::hevc::is_keyframe(&data),VideoCodec::Av1=>uu_media::av1::is_keyframe(&data),VideoCodec::H264=>uu_media::EncodedFrame::detect_h264_keyframe(&data),VideoCodec::Pyrowave=>uu_media::pyrowave::sequence_header(&data).is_ok()};
            if !gate.accepts(sample_epoch, current_epoch, key) { continue; }
            if sample_codec != codec {
                // Codec ownership travels with each AU, not a racy track
                // snapshot. A format change cannot feed the previous decoder.
                if !key { continue; }
                match create(sample_codec) {
                    Ok(next) => decoder = next,
                    Err(_) => { gate.failed(); worker_requests.request(); worker_refresh.notify_one();if let Some(s)=&worker_stats{s.recovery(RecoveryCause::DecoderError);}continue; }
                }
                codec = sample_codec;
                worker_backlog.reset_estimate();
                if let Some(s) = worker_stats.as_ref().filter(|s|s.selected_video_track.load(Ordering::Acquire)==track_idx) {
                    *s.codec.lock().unwrap() = match codec { VideoCodec::Hevc => "video/H265", VideoCodec::Av1=>"video/AV1",VideoCodec::H264 => "video/H264",VideoCodec::Pyrowave=>"video/PYROWAVE" }.into();
                }
                gate.failed();
                reset_decoder = false;
                recovery_errors = 0;
                recovery = None;
                color = uu_media::color::YuvColor::default();
                dump = None;
            }
            // An IDR resets H.264 references itself. Recreate the decoder only
            // after a decoder/driver error, never for ordinary network loss.
            if key && reset_decoder {
                let next = if codec == VideoCodec::H264 && (recovery_errors >= 3 || decoder.backend_name().starts_with("openh264")) {
                    uu_media::software_decoder().and_then(&prepare)
                } else {
                    create(codec)
                };
                match next {
                    Ok(fresh) => decoder = fresh,
                    Err(_) => continue,
                }
                reset_decoder = false;
                worker_backlog.reset_estimate();
            }
            if let Some(file) = dump.as_mut() {
                use std::io::Write;
                let _ = file.write_all(&data);
            }
            let base = *pts_base.get_or_insert(timestamp);
            if key {
                let updated = match codec {VideoCodec::Hevc=>uu_media::hevc::sequence(&data).map(|s|s.color),VideoCodec::Av1=>uu_media::av1::sequence(&data).map(|s|s.color),VideoCodec::H264=>uu_media::color::from_h264(&data),VideoCodec::Pyrowave=>Some(uu_media::color::YuvColor::Full709)};
                if let Some(updated) = updated { color = updated; }
            }
            let encoded = uu_media::EncodedFrame {
                data,
                is_keyframe: key,
                pts: uu_media::Timestamp(timestamp.wrapping_sub(base) as u64 * 1_000_000 / 90_000),
            };
            if key {
                recovery = Some((sample_epoch, sample_request, encoded.pts));
            }
            if let Some(s)=&worker_stats{s.pipeline_trace.frame(trace_epoch,track_idx,source_ssrc,timestamp,crate::pipeline_trace::Stage::DecodeStart,Some((encoded.pts.0,sample_epoch)));}
            if let Some(s)=&worker_stats{s.video_e2e.decode_input(trace_epoch,track_idx,source_ssrc,timestamp,encoded.pts.0);}
            let started = Instant::now();
            let outcome=decoder.decode_frames(&encoded);
            let output_available=Instant::now();
            let decode_elapsed = started.elapsed();
            if let Some(s)=&worker_stats{s.pipeline_trace.frame(trace_epoch,track_idx,source_ssrc,timestamp,crate::pipeline_trace::Stage::DecodeReturn,None);}
            worker_backlog.observe(decode_elapsed);
            decode_us += decode_elapsed.as_micros() as u64;
            match outcome {
                Ok(frames) => {
                    // One call may yield prior inputs. Stamp all actual output
                    // PTS at the API return, before publishing any of the batch.
                    if let Some(s)=&worker_stats{for frame in &frames{s.pipeline_trace.image_at(trace_epoch,track_idx,frame.pts.0,crate::pipeline_trace::Stage::OutputAvailable,output_available);}}
                    gate.decoded(sample_epoch, worker_generation.load(Ordering::Acquire), key);
                    let last = frames.len().saturating_sub(1);
                    for (index, frame) in frames.into_iter().enumerate() {
                        recovery_errors = 0;
                        if recovery.is_some_and(|(generation, request, pts)| {
                            frame.pts >= pts
                                && generation == worker_generation.load(Ordering::Acquire)
                                && request == worker_requests.requested()
                        }) {
                            worker_requests.complete(recovery.unwrap().1);
                            recovery = None;
                        }
                        decoded += 1;
                        worker_decoded.fetch_add(1, Ordering::Relaxed);
                        window_frames += 1;
                        if let Some(s) = &worker_stats {
                            if s.selected_video_track.load(Ordering::Acquire)==track_idx {
                                s.width.store(frame.width, Ordering::Relaxed);
                                s.height.store(frame.height, Ordering::Relaxed);
                            }
                            s.decoded_total.fetch_add(1, Ordering::Relaxed);
                        }
                        // Decode all reference pictures, but spend RGBA and
                        // upload work only on the newest one after a burst.
                        if let Some(tx) = frame_tx.as_ref().filter(|_| index == last && (key || sample_rx.is_empty())) {
                            if let Some(s)=&worker_stats{s.pipeline_trace.image(trace_epoch,track_idx,frame.pts.0,crate::pipeline_trace::Stage::Publish);}
                            let observation=worker_stats.as_ref().map(|s|crate::display_frames::DecodedObservation {
                                media_epoch:trace_epoch,track:track_idx,pts_us:frame.pts.0,
                                token:s.video_e2e.frame_for_pts(trace_epoch,track_idx,frame.pts.0),
                                receive:s.pipeline_trace.image_identity(trace_epoch,track_idx,frame.pts.0),
                            });
                            let _ = tx.send_observed_decoded(track_idx, frame, color,observation);
                        } else if let Some(s) = worker_stats.as_ref().filter(|_|frame_tx.is_some()) {
                            s.decoded_display_skips.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                Err(e) => {
                    errors += 1;
                    // Preserve the fallback budget across decoder recreation;
                    // repeatedly rebuilding a failing driver must not reset it.
                    recovery_errors = recovery_errors.saturating_add(1);
                    gate.failed();
                    reset_decoder = true;
                    recovery = None;
                    worker_requests.request();
                    worker_refresh.notify_one();
                    if let Some(s) = &worker_stats {
                        s.recovery(RecoveryCause::DecoderError);
                        s.errors.fetch_add(1, Ordering::Relaxed);
                    }
                    if debug {
                        eprintln!("decode error, requesting IDR: {e}");
                    }
                }
            }
        }
        if debug {
            println!("decode done: frames={decoded} errors={errors}");
        }
    });
    let feedback_pc = pc.clone();
    let feedback_waiting = request_generation.clone();
    let feedback_stats = stats.clone();
    let feedback_refresh = refresh.clone();
    let ssrc = track.ssrc();
    let feedback = tokio::spawn(async move {
        let sender_ssrc =
            u32::from_be_bytes(uuid::Uuid::new_v4().as_bytes()[..4].try_into().unwrap());
        let remb_bps = std::env::var("UU_BITRATE")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(55.0)
            * 1_000_000.0;
        let mut ticker = tokio::time::interval(Duration::from_millis(100));
        let mut last_refresh = None;
        let mut last_remb = None;
        let mut fir_seq = 0u8;
        let mut retries = 0u32;
        loop {
            tokio::select! { _ = ticker.tick() => {}, _ = feedback_refresh.notified() => {} }
            let now = Instant::now();
            let mut packets: Vec<Box<dyn webrtc::rtcp::packet::Packet + Send + Sync>> = Vec::new();
            let needs_refresh = feedback_waiting.is_waiting();
            if !needs_refresh {
                retries = 0;
                last_refresh = None;
            }
            if let Some(s) = &feedback_stats {
                s.waiting_for_keyframe
                    .store(needs_refresh as u32, Ordering::Relaxed);
            }
            if needs_refresh
                && last_refresh.map_or(true, |at: Instant| {
                    now.duration_since(at) >= Duration::from_millis(if retries < 2 { 200 } else { 500 })
                })
            {
                packets.push(Box::new(PictureLossIndication {
                    sender_ssrc,
                    media_ssrc: ssrc,
                }));
                retries += 1;
                if retries % 4 == 0 {
                    fir_seq = fir_seq.wrapping_add(1);
                    packets.push(Box::new(FullIntraRequest {
                        sender_ssrc,
                        media_ssrc: 0,
                        fir: vec![FirEntry {
                            ssrc,
                            sequence_number: fir_seq,
                            ..Default::default()
                        }],
                    }));
                }
                last_refresh = Some(now);
                if let Some(s) = &feedback_stats {
                    s.keyframe_requests.fetch_add(1, Ordering::Relaxed);
                }
            }
            // A healthy stream no longer gets forced into a large IDR every
            // two seconds. Periodic REMB is independent of recovery requests.
            if last_remb.map_or(true, |at: Instant| {
                now.duration_since(at) >= Duration::from_secs(2)
            }) {
                packets.push(Box::new(ReceiverEstimatedMaximumBitrate {
                    sender_ssrc,
                    bitrate: remb_bps,
                    ssrcs: vec![ssrc],
                }));
                last_remb = Some(now);
            }
            if !packets.is_empty() && feedback_pc.write_rtcp(&packets).await.is_err() {
                break;
            }
        }
    });
    let wait_ms = std::env::var("UU_JITTER_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(60)
        .clamp(10, 200);
    let mut repair_rx = repairs.subscribe(track.ssrc());
    let nack_sender = u32::from_be_bytes(uuid::Uuid::new_v4().as_bytes()[..4].try_into().unwrap());
    let mut last_nack = Instant::now();
    let mut receiver = VideoReceiver::with_codec(Duration::from_millis(wait_ms), codec);
    let mut network_loss=crate::network_loss::Observer::new(Duration::from_millis(wait_ms));
    let mut ticker = tokio::time::interval(Duration::from_millis(5));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let strict = !std::env::var("UU_STRICT_RECOVERY").is_ok_and(|v| v == "0");
    let recover = |hard: bool,cause:RecoveryCause| {
        if hard {
            generation.fetch_add(1, Ordering::AcqRel);
        }
        request_generation.request();
        if let Some(s)=&stats{s.recovery(cause);}
        refresh.notify_one();
    };
    let mut receiver_epoch = 0;
    let mut test_loss = crate::test_loss::TestLoss::from_env();
    let mut bytes = 0u64;
    let mut previous_received = 0u64;
    let mut previous_lost = 0u64;
    let mut previous_decoded = 0u64;
    let mut src = 0u64;
    let mut last_ts = None;
    let mut cadence = crate::frame_cadence::Cadence::default();
    let mut tick_late = crate::frame_cadence::Samples::default();
    let mut window = Instant::now();
    // TrackRemote::read_rtp is not cancellation safe. Preserve the pending
    // read across repair/timer events instead of dropping it every 5 ms.
    let packet_read=track.read_rtp();
    tokio::pin!(packet_read);
    let mut active_payload_type = track.payload_type();
    if let Some(fec) = &fec { fec.source(track.ssrc(), active_payload_type); }
    let mut latest_primary: Option<u16> = None;
    loop {
        let packet = tokio::select! {
            result=&mut packet_read=>{
                let Ok((packet,_))=result else {break};
                if let Some(s)=&stats{if let Some(trace)=&socket_trace{let at=Instant::now();trace.primary(trace_epoch,track_idx,&packet.header,at,s.pipeline_trace.clock_offset(at));}}
                packet_read.set(track.read_rtp());
                Some((packet, true))
            }
            Some(packet)=repair_rx.recv()=>Some((packet, false)),
            packet=async {match &fec {Some(fec)=>fec.next().await,None=>std::future::pending().await}}=>Some((packet,false)),
            due=ticker.tick()=>{tick_late.push(due.elapsed().as_secs_f64() * 1000.0);None},
        };
        let now = Instant::now();
        network_loss.advance(now);
        if packet.as_ref().is_some_and(|(p,_)| test_loss.drop_packet(p.header.sequence_number, now)) {
            if let Some(s) = &stats { s.injected_loss.fetch_add(1, Ordering::Relaxed); }
            continue;
        }
        let units = if let Some((packet, primary)) = packet {
            if primary {network_loss.observe(packet.header.sequence_number,now);}
            if packet.header.payload_type != active_payload_type {
                // Old RTX/late primary packets cannot switch the format back.
                if !primary || latest_primary.is_some_and(|last| (packet.header.sequence_number.wrapping_sub(last) as i16) < 0) { continue; }
                let Some(next_codec) = video_codec(&track.codec().capability) else { continue; };
                active_payload_type = packet.header.payload_type;
                if let Some(fec) = &fec { fec.source(track.ssrc(), active_payload_type); }
                if codec != next_codec {
                    codec = next_codec;
                    receiver.change_codec(codec, packet.header.sequence_number);
                    receiver_epoch = receiver.epoch;
                    recover(true,RecoveryCause::FormatChange);
                    if let Some(s) = &stats { s.diagnostics.record(serde_json::json!({"event":"video_codec_change","track":track_idx,"codec":format!("{codec:?}")})); }
                }
            }
            if primary && latest_primary.map_or(true, |last| (packet.header.sequence_number.wrapping_sub(last) as i16) > 0) {
                latest_primary = Some(packet.header.sequence_number);
            }
            if let Some(fec) = &fec { fec.observe(&packet); }
            if let Some(s) = &stats {
                s.packets.fetch_add(1, Ordering::Relaxed);
            }
            bytes += packet.payload.len() as u64;
            if last_ts.map_or(true, |last| {
                packet.header.timestamp.wrapping_sub(last) as i32 > 0
            }) {
                cadence.observe(packet.header.timestamp, now);
                last_ts = Some(packet.header.timestamp);
                src += 1;
            }
            if let Some(s)=&stats{s.pipeline_trace.packet(trace_epoch,track_idx,source_ssrc,packet.header.timestamp,primary,now);}
            receiver.push(packet, now)
        } else {
            receiver.poll(now)
        };
        // Capture assembly completion before any asynchronous feedback write.
        for unit in &units {
            if let Some(s)=&stats{s.pipeline_trace.frame(trace_epoch,track_idx,source_ssrc,unit.timestamp,crate::pipeline_trace::Stage::CompleteAu,None);}
        }
        if last_nack.elapsed() >= Duration::from_millis(20) {
            if let Some(s) = &stats {
                let measured_ms = (s.rtt_us.load(Ordering::Relaxed) as u64
                    + 4 * s.rtt_deviation_us.load(Ordering::Relaxed) as u64) / 1000 + 20;
                receiver.set_repair_wait(Duration::from_millis(wait_ms.max(measured_ms).min(200)));
            }
            let missing = receiver.missing(now);
            if !missing.is_empty() {
                use webrtc::rtcp::transport_feedbacks::transport_layer_nack::{
                    nack_pairs_from_sequence_numbers, TransportLayerNack,
                };
                let nack = TransportLayerNack {
                    sender_ssrc: nack_sender,
                    media_ssrc: track.ssrc(),
                    nacks: nack_pairs_from_sequence_numbers(&missing),
                };
                if pc.write_rtcp(&[Box::new(nack)]).await.is_ok() {
                    if let Some(s) = &stats {
                        s.nack_requests
                            .fetch_add(missing.len() as u64, Ordering::Relaxed);
                    }
                }
            }
            last_nack = now;
        }
        for unit in units {
            if unit.epoch != receiver_epoch {
                receiver_epoch = unit.epoch;
                // With balanced recovery keep decoding intact reference
                // pictures while requesting refresh. Strict IDR-only recovery
                // can freeze indefinitely when large keyframes also lose a packet.
                recover(strict,RecoveryCause::ReceiveDamage);
            }
            let epoch = generation.load(Ordering::Acquire);
            let Some(permit)=backlog.reserve(unit.data.len()) else {
                recover(true,RecoveryCause::DecodeQueue);
                if let Some(s)=&stats{s.queue_drops.fetch_add(1,Ordering::Relaxed);}
                continue;
            };
            if sample_tx
                .try_send((
                    unit.data,
                    unit.timestamp,
                    epoch,
                    request_generation.requested(),
                    Instant::now(),
                    codec,
                    permit,
                ))
                .is_err()
            {
                recover(true,RecoveryCause::DecodeQueue);
                if let Some(s) = &stats {
                    s.queue_drops.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        if receiver.epoch != receiver_epoch {
            receiver_epoch = receiver.epoch;
            recover(strict,RecoveryCause::ReceiveDamage);
        }
        let _ = receiver.take_refresh();
        if let Some(s) = stats.as_ref().filter(|s|s.selected_video_track.load(Ordering::Acquire)==track_idx) {
            s.lost.store(receiver.stats.lost, Ordering::Relaxed);
            s.reordered
                .store(receiver.stats.reordered, Ordering::Relaxed);
            s.late_packets.store(receiver.stats.late, Ordering::Relaxed);
            s.damaged_frames
                .store(receiver.stats.damaged, Ordering::Relaxed);
        }
        if window.elapsed() >= Duration::from_millis(500) {
            let network_snapshot=network_loss.report(now);
            let decoded = track_decoded.load(Ordering::Relaxed);
            let mut timing = crate::frame_cadence::Snapshot::default();
            cadence.publish(&mut timing);
            timing.receiver_tick_late_ms = tick_late.take();
            if let Some(s) = stats.as_ref().filter(|s|s.selected_video_track.load(Ordering::Acquire)==track_idx) {
                *s.network_loss.lock().unwrap()=Some(network_snapshot);
                {
                    let mut snapshot = s.frame_cadence.lock().unwrap();
                    snapshot.source_spacing_ms = timing.source_spacing_ms;
                    snapshot.arrival_spacing_ms = timing.arrival_spacing_ms;
                    snapshot.receiver_tick_late_ms = timing.receiver_tick_late_ms;
                }
                let seconds = window.elapsed().as_secs_f64();
                // The receive timer keeps running when no sample reaches the
                // decoder. An idle track must report zero, not its last rate.
                s.dec_fps_x100.store(
                    (decoded.saturating_sub(previous_decoded) as f64 / seconds * 100.0) as u32,
                    Ordering::Relaxed,
                );
                let lost = receiver.stats.lost.saturating_sub(previous_lost);
                let window_packets = receiver.stats.received.saturating_sub(previous_received);
                s.loss_ppm.store((lost.saturating_mul(1_000_000) / (window_packets + lost).max(1)) as u32, Ordering::Relaxed);
                s.src_fps_x100
                    .store((src as f64 / seconds * 100.0) as u32, Ordering::Relaxed);
                s.bitrate_kbps.store(
                    (bytes as f64 * 8.0 / seconds / 1000.0) as u32,
                    Ordering::Relaxed,
                );
                if debug {
                    println!(
                        "media: kbps={} lost={} bad={} nack={} rtx={} repair_drop={} waiting={}",
                        s.bitrate_kbps.load(Ordering::Relaxed),
                        receiver.stats.lost,
                        receiver.stats.damaged,
                        s.nack_requests.load(Ordering::Relaxed),
                        s.rtx_packets.load(Ordering::Relaxed),
                        s.rtx_drops.load(Ordering::Relaxed),
                        request_generation.is_waiting()
                    );
                }
            }
            src = 0;
            bytes = 0;
            previous_received = receiver.stats.received;
            previous_lost = receiver.stats.lost;
            previous_decoded = decoded;
            window = Instant::now();
        }
    }
    repairs.unsubscribe(track.ssrc());
    feedback.abort();
    drop(sample_tx);
    let _ = worker.await;
}
