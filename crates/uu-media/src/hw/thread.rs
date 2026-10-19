//! A dedicated OS thread per hardware decoder.
//!
//! Hardware decode APIs are thread-affine in ways that a tokio task cannot
//! promise: Media Foundation wants a COM apartment per thread, and the D3D11
//! immediate context (like a VA-API display and its surfaces) must be driven
//! from a single thread unless the device is created with multithread
//! protection. A decode loop spawned with `tokio::spawn` may resume on a
//! different worker thread after every `.await`, which is exactly the case
//! that produces intermittent driver corruption.
//!
//! Owning one thread here makes every backend call happen on the same thread
//! by construction, and also means the decoder (and its COM/driver state) is
//! torn down on that thread rather than on whatever thread drops the session.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;

use crate::codec::CodecError;
use crate::frame::{EncodedFrame, RawFrame};

/// A decoder implementation that lives entirely on the worker thread.
///
/// Deliberately not [`crate::codec::VideoDecoder`]: driver objects are not
/// `Send`, and they do not have to be — the whole point of this module is that
/// they are created, used and destroyed on one thread.
pub trait Backend: 'static {
    fn backend_name(&self) -> &str;
    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<RawFrame>, String>;
}

enum Job {
    Decode(EncodedFrame, Sender<Result<Option<RawFrame>, String>>),
    Stop,
}

/// Handle to a decoder living on its own thread.
pub struct Worker {
    tx: Sender<Job>,
    join: Option<JoinHandle<()>>,
    name: String,
}

impl Worker {
    /// Start `init` on a fresh thread and wait for it to report success.
    pub fn spawn<B, F>(init: F) -> Result<Self, CodecError>
    where
        B: Backend,
        F: FnOnce() -> Result<B, String> + Send + 'static,
    {
        let (tx, rx) = channel::<Job>();
        let (boot_tx, boot_rx) = channel::<Result<String, String>>();
        let join = std::thread::Builder::new()
            .name("uu-hwdec".to_string())
            .spawn(move || run(init, rx, boot_tx))
            .map_err(|e| CodecError::Backend(format!("hardware decoder thread: {e}")))?;

        match boot_rx.recv() {
            Ok(Ok(name)) => Ok(Self {
                tx,
                join: Some(join),
                name,
            }),
            Ok(Err(e)) => {
                let _ = join.join();
                Err(CodecError::Backend(e))
            }
            Err(_) => {
                let _ = join.join();
                Err(CodecError::Backend(
                    "hardware decoder thread died during setup".to_string(),
                ))
            }
        }
    }

    pub fn backend_name(&self) -> &str {
        &self.name
    }

    /// Decode one access unit on the owning thread.
    pub fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<RawFrame>, CodecError> {
        let (reply_tx, reply_rx) = channel();
        self.tx
            .send(Job::Decode(frame.clone(), reply_tx))
            .map_err(|_| CodecError::Backend("hardware decoder thread is gone".to_string()))?;
        match reply_rx.recv() {
            Ok(Ok(frame)) => Ok(frame),
            Ok(Err(e)) => Err(CodecError::Backend(e)),
            Err(_) => Err(CodecError::Backend(
                "hardware decoder thread is gone".to_string(),
            )),
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.tx.send(Job::Stop);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn run<B, F>(init: F, rx: Receiver<Job>, boot: Sender<Result<String, String>>)
where
    B: Backend,
    F: FnOnce() -> Result<B, String>,
{
    let mut decoder = match init() {
        Ok(decoder) => decoder,
        Err(e) => {
            let _ = boot.send(Err(e));
            return;
        }
    };
    if boot.send(Ok(decoder.backend_name().to_string())).is_err() {
        return; // the caller gave up while we were initialising
    }
    while let Ok(job) = rx.recv() {
        match job {
            Job::Decode(frame, reply) => {
                let result = decoder.decode(&frame).map_err(|e| e.to_string());
                // A closed reply channel means the caller dropped us; stop.
                if reply.send(result).is_err() {
                    break;
                }
            }
            Job::Stop => break,
        }
    }
    // `decoder` drops here: driver and COM teardown stay on this thread.
}
