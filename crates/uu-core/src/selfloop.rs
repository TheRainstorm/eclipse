//! M2 self-loop: the full host -> controller pipeline over real UDP.
//!
//! This deliberately does **not** talk to NetEase: it proves the pipeline
//! (capture -> H.264 encode -> fragment -> UDP -> reassemble -> decode)
//! end to end, and gates on decoded quality. That keeps the authentication
//! work (see `docs/protocol-notes.md`) off the critical path.
//!
//! Capture runs on a blocking thread behind the `Capture` trait, exactly as
//! a real backend would; the pattern source stands in when there is no
//! display.
//!
//! It lives in the library (not the CLI) so `cargo test` exercises it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use uu_capture::backend::Capture;
use uu_capture::SyntheticCapture;
use uu_media::{default_encoder, luma_mse, psnr_from_mse, software_decoder, RawFrame};
use uu_proto::pkt::{packetize, Reassembler};

/// Receive buffer for the controller socket. A 720p keyframe is ~100 KB and
/// the sender bursts ~8 datagrams per frame, so the default ~200 KB kernel
/// buffer drops datagrams under load. Production needs this too.
const RCVBUF_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct SelfLoopConfig {
    pub frames: u32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub mtu: usize,
    /// Minimum average PSNR (dB) for the run to count as a pass.
    pub min_psnr_db: f64,
}

impl Default for SelfLoopConfig {
    fn default() -> Self {
        Self {
            frames: 120,
            width: 640,
            height: 360,
            fps: 30,
            bitrate_kbps: 4_000,
            mtu: uu_proto::pkt::DEFAULT_MTU,
            min_psnr_db: 30.0,
        }
    }
}

#[derive(Debug, Default)]
struct HostStats {
    /// Frames pulled from the capture backend.
    captured: u32,
    /// Frames that actually produced a bitstream. Fewer than `captured`
    /// whenever the encoder is still holding a frame (see the flush note in
    /// `run`): OpenH264 has one frame of latency.
    encoded: u32,
    keyframes: u32,
    packets: u64,
    payload_bytes: u64,
    encoded_bytes: u64,
}

/// Bind a UDP socket with an explicit receive buffer.
fn bind_udp(addr: &str, rcvbuf: usize) -> Result<UdpSocket> {
    let sock = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    let _ = sock.set_recv_buffer_size(rcvbuf);
    let _ = sock.set_send_buffer_size(rcvbuf);
    let target: std::net::SocketAddr = addr.parse()?;
    sock.bind(&target.into())?;
    sock.set_nonblocking(true)?;
    Ok(UdpSocket::from_std(sock.into())?)
}

#[derive(Debug, Default)]
struct ControllerStats {
    packets: u64,
    frames_reassembled: u32,
    delivered_bytes: u64,
    frames_decoded: u32,
    /// Frames still missing chunks when the run stopped.
    pending_at_end: usize,
    keyframes: u32,
    psnr_sum: f64,
    psnr_min: f64,
    psnr_samples: u32,
}

pub async fn run(cfg: SelfLoopConfig) -> Result<()> {
    if cfg.frames == 0 {
        bail!("--frames must be at least 1");
    }
    println!("uu self-loop  (M2 pipeline check, no NetEase servers involved)");
    println!(
        "  source      synthetic {}x{} @{}fps (no display required)",
        cfg.width, cfg.height, cfg.fps
    );
    println!(
        "  codec       OpenH264 software, target {} kbps, mtu {} B",
        cfg.bitrate_kbps, cfg.mtu
    );

    // Controller end: bind first so the host has a real destination.
    let controller = bind_udp("127.0.0.1:0", RCVBUF_BYTES).context("binding controller socket")?;
    let controller_addr = controller.local_addr()?;
    let host = bind_udp("127.0.0.1:0", RCVBUF_BYTES).context("binding host socket")?;
    println!("  transport   udp 127.0.0.1:{} -> {}", host.local_addr()?.port(), controller_addr.port());

    // Source frames are shared with the controller task so it can score the
    // decoded output. Test-harness only: the wire protocol has no such channel.
    let (src_tx, src_rx) = mpsc::unbounded_channel::<(u32, RawFrame)>();
    let (cap_tx, mut cap_rx) = mpsc::channel::<RawFrame>(3);

    // Blocking capture thread, behind the real Capture trait.
    let (cw, ch, cfps, total) = (cfg.width, cfg.height, cfg.fps, cfg.frames);
    tokio::task::spawn_blocking(move || {
        let mut capture = SyntheticCapture::new(cw, ch, cfps);
        for _ in 0..total {
            match capture.next_frame() {
                Ok(frame) => {
                    if cap_tx.blocking_send(frame).is_err() {
                        break; // host stopped early
                    }
                }
                Err(e) => {
                    eprintln!("capture error: {e}");
                    break;
                }
            }
        }
    });

    let controller_task = tokio::spawn(controller_loop(controller, src_rx, cfg.frames));
    let host_stats = host_loop(host, controller_addr, &mut cap_rx, &cfg, src_tx).await?;
    let ctrl_stats = controller_task.await.context("controller task panicked")?;

    let expected = cfg.frames as u64;
    let avg_psnr = if ctrl_stats.psnr_samples > 0 {
        ctrl_stats.psnr_sum / ctrl_stats.psnr_samples as f64
    } else {
        f64::NAN
    };

    println!();
    let latency = host_stats.captured.saturating_sub(host_stats.encoded);
    let decode_ratio = if host_stats.encoded > 0 {
        ctrl_stats.frames_decoded as f64 / host_stats.encoded as f64
    } else {
        0.0
    };
    println!(
        "  host        captured={} encoded={} keyframes={} packets={} bytes={}B (payload {}B)",
        host_stats.captured,
        host_stats.encoded,
        host_stats.keyframes,
        host_stats.packets,
        host_stats.encoded_bytes,
        host_stats.payload_bytes
    );
    if latency > 0 {
        println!(
            "              note: {latency} frame(s) still inside the encoder at shutdown \
(one-frame codec latency; a session must flush before ending)"
        );
    }
    println!(
        "  controller  packets={} delivered={}B reassembled={} (keyframes={}) decoded={}/{} ({:.1}%) incomplete={}",
        ctrl_stats.packets,
        ctrl_stats.delivered_bytes,
        ctrl_stats.frames_reassembled,
        ctrl_stats.keyframes,
        ctrl_stats.frames_decoded,
        expected,
        decode_ratio * 100.0,
        ctrl_stats.pending_at_end
    );
    println!("  quality     avg PSNR={:.2} dB  min={:.2} dB  (samples={})",
             avg_psnr, ctrl_stats.psnr_min, ctrl_stats.psnr_samples);

    // Hard gates: the point of the harness is to fail loudly on regressions.
    if (host_stats.captured as u64) != expected {
        bail!("capture produced {} of {} frames", host_stats.captured, expected);
    }
    if host_stats.encoded == 0 {
        bail!("encoder produced no bitstream at all");
    }
    // Loopback is lossless, so these two must match exactly. A mismatch means
    // a reassembly bug, not acceptable network loss: production links need
    // NACK/RTX before this can be relaxed.
    if ctrl_stats.packets != host_stats.packets {
        bail!(
            "packet accounting mismatch: sent {} but received {}",
            host_stats.packets,
            ctrl_stats.packets
        );
    }
    if ctrl_stats.frames_decoded != host_stats.encoded {
        bail!(
            "decoded {} frames but the encoder emitted {} (incomplete frames left: {})",
            ctrl_stats.frames_decoded,
            host_stats.encoded,
            ctrl_stats.pending_at_end
        );
    }
    if ctrl_stats.pending_at_end != 0 {
        bail!("{} frames were left with missing chunks", ctrl_stats.pending_at_end);
    }
    if decode_ratio < 0.95 {
        bail!("decode ratio {:.1}% is below 95%", decode_ratio * 100.0);
    }
    if ctrl_stats.psnr_samples == 0 {
        bail!("no decoded frame could be scored against its source");
    }
    if avg_psnr < cfg.min_psnr_db {
        bail!(
            "decoded quality {:.2} dB is below the {:.1} dB gate",
            avg_psnr, cfg.min_psnr_db
        );
    }
    println!();
    println!(
        "  PASS  {} encoded, {} decoded ({:.1}%), all {} packets accounted for, {:.2} dB avg over {} frames (gate {:.1} dB)",
        host_stats.encoded,
        ctrl_stats.frames_decoded,
        decode_ratio * 100.0,
        ctrl_stats.packets,
        avg_psnr,
        ctrl_stats.psnr_samples,
        cfg.min_psnr_db
    );
    Ok(())
}

async fn host_loop(
    host: UdpSocket,
    controller_addr: std::net::SocketAddr,
    cap_rx: &mut mpsc::Receiver<RawFrame>,
    cfg: &SelfLoopConfig,
    src_tx: mpsc::UnboundedSender<(u32, RawFrame)>,
) -> Result<HostStats> {
    let mut encoder = default_encoder(cfg.bitrate_kbps).context("creating encoder")?;
    let mut stats = HostStats::default();
    let frame_interval = Duration::from_micros(1_000_000 / cfg.fps.max(1) as u64);
    let start = Instant::now();
    let mut frame_id: u32 = 0;
    let mut next_deadline = start;

    while let Some(frame) = cap_rx.recv().await {
        let encoded = encoder.encode(&frame).context("encoding frame")?;
        if !encoded.is_empty() {
            stats.encoded += 1;
        }
        for ef in &encoded {
            let packets = packetize(frame_id, ef.pts.as_micros(), ef.is_keyframe, &ef.data, cfg.mtu)
                .context("packetizing")?;
            if ef.is_keyframe {
                stats.keyframes += 1;
            }
            for p in &packets {
                host.send_to(p, controller_addr).await.context("sending datagram")?;
                stats.packets += 1;
                stats.payload_bytes += p.len() as u64;
            }
            stats.encoded_bytes += ef.data.len() as u64;
        }
        // Feed the scorer. Ignore failure: the controller may have finished.
        let _ = src_tx.send((frame_id, frame.clone()));
        stats.captured += 1;
        frame_id = frame_id.wrapping_add(1);

        // Pace to the requested frame rate.
        next_deadline += frame_interval;
        let now = Instant::now();
        if next_deadline > now {
            tokio::time::sleep(next_deadline - now).await;
        } else {
            next_deadline = now;
        }
    }
    Ok(stats)
}

async fn controller_loop(
    socket: UdpSocket,
    mut src_rx: mpsc::UnboundedReceiver<(u32, RawFrame)>,
    expected: u32,
) -> ControllerStats {
    use tokio::sync::mpsc::error::TryRecvError;

    // Deliberately the software decoder: this loop is a quality gate on our
    // own encode/decode, so it must not depend on a GPU being present (or on
    // a driver behaving). Hardware paths are exercised by a real session,
    // where `uu_media::FallbackDecoder` also bounds the damage.
    let mut decoder = match software_decoder() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("decoder unavailable: {e}");
            return ControllerStats::default();
        }
    };
    let mut reassembler = Reassembler::new();
    let mut sources: HashMap<u32, RawFrame> = HashMap::new();
    // Decoded frames are kept until the end so scoring is not racing the
    // source channel: a frame can be decoded long before its source arrives.
    let mut decoded_frames: Vec<(u32, RawFrame)> = Vec::new();
    let mut stats = ControllerStats::default();
    let mut buf = vec![0u8; 65_536];
    let mut src_done = false;
    let mut quiet_ticks = 0u32;

    'outer: loop {
        // Drain every source frame currently available.
        loop {
            match src_rx.try_recv() {
                Ok((id, frame)) => {
                    sources.insert(id, frame);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    src_done = true;
                    break;
                }
            }
        }

        // Burst-drain the socket without awaiting. A single awaited recv per
        // iteration cannot keep up with a 720p sender, and the kernel then
        // drops datagrams, which strands a frame forever (there is no NACK
        // in this harness).
        loop {
            match socket.try_recv_from(&mut buf) {
                Ok((n, _)) => {
                    stats.packets += 1;
                    stats.delivered_bytes += n as u64;
                    ingest(&mut reassembler, &mut decoder, &mut decoded_frames, &mut stats, &buf[..n]);
                    quiet_ticks = 0;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break 'outer,
            }
        }

        if decoded_frames.len() as u32 >= expected {
            break;
        }

        // Bounded wait so we can notice the host finishing.
        match tokio::time::timeout(Duration::from_millis(100), socket.recv_from(&mut buf)).await {
            Ok(Ok((n, _))) => {
                stats.packets += 1;
                stats.delivered_bytes += n as u64;
                ingest(&mut reassembler, &mut decoder, &mut decoded_frames, &mut stats, &buf[..n]);
                quiet_ticks = 0;
            }
            Ok(Err(_)) => break,
            Err(_elapsed) => {
                quiet_ticks += 1;
                if src_done && quiet_ticks >= 5 {
                    break;
                }
                if quiet_ticks >= 60 {
                    break; // safety net: never hang a test
                }
            }
        }
    }

    stats.pending_at_end = reassembler.pending_len();

    // Late source frames may still be queued; collect them before scoring.
    while let Ok((id, frame)) = src_rx.try_recv() {
        sources.insert(id, frame);
    }

    for (frame_id, decoded) in &decoded_frames {
        stats.frames_decoded += 1;
        if let Some(src) = sources.get(frame_id) {
            if let Some(mse) = luma_mse(src, decoded) {
                let psnr = psnr_from_mse(mse);
                if psnr.is_finite() {
                    stats.psnr_sum += psnr;
                    stats.psnr_samples += 1;
                    stats.psnr_min = if stats.psnr_min == 0.0 {
                        psnr
                    } else {
                        stats.psnr_min.min(psnr)
                    };
                }
            }
        }
    }
    stats
}

/// Reassemble one datagram and decode it if it completed a frame.
fn ingest(
    reassembler: &mut Reassembler,
    decoder: &mut Box<dyn uu_media::VideoDecoder>,
    decoded_frames: &mut Vec<(u32, RawFrame)>,
    stats: &mut ControllerStats,
    datagram: &[u8],
) {
    let frame = match reassembler.push(datagram) {
        Ok(Some(frame)) => frame,
        Ok(None) => return,
        Err(e) => {
            tracing::debug!("dropping malformed datagram: {e}");
            return;
        }
    };
    stats.frames_reassembled += 1;
    if frame.keyframe {
        stats.keyframes += 1;
    }
    let access_unit = uu_media::EncodedFrame {
        data: frame.data.clone(),
        pts: uu_media::Timestamp(frame.pts),
        is_keyframe: frame.keyframe,
    };
    match decoder.decode(&access_unit) {
        Ok(Some(decoded)) => decoded_frames.push((frame.frame_id, decoded)),
        Ok(None) => {}
        Err(e) => eprintln!("decode error: {e}"),
    }
}
