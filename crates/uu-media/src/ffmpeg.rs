//! Out-of-process H.264 decoding through the `ffmpeg` command.
//!
//! FFmpeg is the pragmatic way to get hardware decode everywhere without
//! linking native libraries (painful when cross-compiling to Windows): pass
//! `-hwaccel d3d11va` on Windows, `-hwaccel vaapi` on Linux, etc. The
//! in-process OpenH264 software decoder stays the default and the fallback.
//!
//! Feeding is decoupled from the caller: access units are handed to a writer
//! thread and decoded frames are drained non-blockingly, so a live RTP loop is
//! never stalled by the child. `decode()` returns the newest ready frame.
//! The last frames may not surface until more input arrives (fine for a live
//! stream).

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use bytes::Bytes;

use crate::codec::{CodecError, VideoDecoder};
use crate::frame::{EncodedFrame, PixelFormat, RawFrame};
use crate::Timestamp;
use uu_proto::media::VideoCodec;

/// Is an `ffmpeg` binary on `PATH`?
pub fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

struct Inner {
    child: Child,
    au_tx: Option<Sender<Vec<u8>>>,
    rx: Receiver<Vec<u8>>,
    width: u32,
    height: u32,
    pts: u64,
}

pub struct FfmpegDecoder {
    hwaccel: String,
    software_fallback: bool,
    inner: Option<Inner>,
}

impl FfmpegDecoder {
    /// `hwaccel` is passed to ffmpeg's `-hwaccel` (`none` disables it).
    pub fn new(hwaccel: impl Into<String>) -> Self {
        Self {
            hwaccel: hwaccel.into(),
            software_fallback: false,
            inner: None,
        }
    }

    pub fn hwaccel(&self) -> &str {
        &self.hwaccel
    }

    pub fn dimensions(&self) -> Option<(u32, u32)> {
        self.inner.as_ref().map(|i| (i.width, i.height))
    }

    /// If a hardware accelerator failed, restart in software on the next SPS.
    fn fallback_to_software(&mut self, was_hw: &str) {
        if !self.software_fallback && !was_hw.is_empty() && was_hw != "none" {
            tracing::warn!("ffmpeg hwaccel '{was_hw}' failed; falling back to software H.264");
            self.hwaccel = "none".to_string();
            self.software_fallback = true;
        }
    }

    fn start(&mut self, width: u32, height: u32) -> Result<(), CodecError> {
        let mut cmd = Command::new("ffmpeg");
        cmd.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-fflags",
            "nobuffer",
            "-flags",
            "low_delay",
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
        ]);
        if !self.hwaccel.is_empty() && self.hwaccel != "none" {
            cmd.args(["-hwaccel", &self.hwaccel]);
        }
        cmd.args([
            "-f", "h264", "-i", "pipe:0", "-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1",
        ]);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd
            .spawn()
            .map_err(|e| CodecError::Backend(format!("spawn ffmpeg: {e}")))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| CodecError::Backend("ffmpeg stdin unavailable".into()))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| CodecError::Backend("ffmpeg stdout unavailable".into()))?;

        // Writer thread: owns stdin, so the caller never blocks on the child.
        let (au_tx, au_rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            while let Ok(au) = au_rx.recv() {
                if stdin.write_all(&au).is_err() {
                    break;
                }
                let _ = stdin.flush();
            }
            // stdin drops here -> EOF -> ffmpeg flushes and exits.
        });

        // Reader thread: fixed-size I420 frames.
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let frame_len = (width as usize * height as usize * 3) / 2;
        std::thread::spawn(move || {
            let mut buf = vec![0u8; frame_len];
            loop {
                match stdout.read_exact(&mut buf) {
                    Ok(()) => {
                        if tx.send(buf.clone()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        self.inner = Some(Inner {
            child,
            au_tx: Some(au_tx),
            rx,
            width,
            height,
            pts: 0,
        });
        Ok(())
    }

    fn ensure_started(&mut self, au: &[u8]) -> Result<(), CodecError> {
        if self.inner.is_none() {
            if let Some((w, h)) = crate::h264::h264_dimensions(au) {
                if w > 0 && h > 0 {
                    self.start(w, h)?;
                }
            }
        }
        Ok(())
    }

    /// Blocking one-frame receive, for offline use (tests/examples). Returns
    /// `None` when the stream ends.
    pub fn recv_frame(&mut self) -> Option<RawFrame> {
        let inner = self.inner.as_mut()?;
        let data = inner.rx.recv().ok()?;
        inner.pts += 1;
        Some(RawFrame {
            width: inner.width,
            height: inner.height,
            format: PixelFormat::I420,
            stride: inner.width,
            data: Bytes::from(data),
            pts: Timestamp(inner.pts),
            gpu: false,
        })
    }

    /// Queue one access unit without draining (offline use).
    pub fn push(&mut self, au: &[u8]) -> Result<(), CodecError> {
        self.ensure_started(au)?;
        if let Some(inner) = self.inner.as_mut() {
            if let Some(tx) = inner.au_tx.as_ref() {
                tx.send(au.to_vec())
                    .map_err(|_| CodecError::Backend("ffmpeg process ended".into()))?;
            }
        }
        Ok(())
    }

    /// Close the input so ffmpeg flushes and exits (offline use).
    pub fn finish(&mut self) {
        if let Some(inner) = self.inner.as_mut() {
            let _ = inner.au_tx.take();
        }
    }
}

impl VideoDecoder for FfmpegDecoder {
    fn codec(&self) -> VideoCodec {
        VideoCodec::H264
    }

    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<RawFrame>, CodecError> {
        self.ensure_started(&frame.data)?;
        let Some(inner) = self.inner.as_mut() else {
            return Ok(None);
        };
        let mut disconnected = false;
        if let Some(tx) = inner.au_tx.as_ref() {
            if tx.send(frame.data.to_vec()).is_err() {
                disconnected = true;
            }
        } else {
            disconnected = true;
        }
        let mut latest = None;
        if !disconnected {
            loop {
                match inner.rx.try_recv() {
                    Ok(data) => {
                        inner.pts += 1;
                        latest = Some(RawFrame {
                            width: inner.width,
                            height: inner.height,
                            format: PixelFormat::I420,
                            stride: inner.width,
                            data: Bytes::from(data),
                            pts: Timestamp(inner.pts),
                            gpu: false,
                        });
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        if disconnected && latest.is_none() {
            let was_hw = self.hwaccel.clone();
            self.inner = None;
            self.fallback_to_software(&was_hw);
        }
        Ok(latest)
    }
}

impl Drop for FfmpegDecoder {
    fn drop(&mut self) {
        if let Some(mut inner) = self.inner.take() {
            let _ = inner.au_tx.take();
            let _ = inner.child.kill();
            let _ = inner.child.wait();
        }
    }
}
