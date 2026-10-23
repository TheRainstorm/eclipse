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
    fn prepare(&mut self, _width: u32, _height: u32) -> Result<(), String> {
        Ok(())
    }
    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<RawFrame>, String>;
    fn decode_frames(&mut self, frame: &EncodedFrame) -> Result<Vec<RawFrame>, String> {
        Ok(self.decode(frame)?.into_iter().collect())
    }
}

enum Job {
    Prepare(u32, u32, Sender<Result<(), String>>),
    Decode(EncodedFrame, Sender<Result<Option<RawFrame>, String>>),
    DecodeFrames(EncodedFrame, Sender<Result<Vec<RawFrame>, String>>),
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

    pub fn prepare(&mut self, width: u32, height: u32) -> Result<(), CodecError> {
        let (reply_tx, reply_rx) = channel();
        self.tx.send(Job::Prepare(width, height, reply_tx))
            .map_err(|_| CodecError::Backend("hardware decoder thread is gone".into()))?;
        reply_rx.recv()
            .map_err(|_| CodecError::Backend("hardware decoder thread is gone".into()))?
            .map_err(CodecError::Backend)
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

    pub fn decode_frames(&mut self, frame: &EncodedFrame) -> Result<Vec<RawFrame>, CodecError> {
        let (reply_tx, reply_rx) = channel();
        self.tx.send(Job::DecodeFrames(frame.clone(), reply_tx))
            .map_err(|_| CodecError::Backend("hardware decoder thread is gone".into()))?;
        reply_rx.recv()
            .map_err(|_| CodecError::Backend("hardware decoder thread is gone".into()))?
            .map_err(CodecError::Backend)
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
            Job::Prepare(width, height, reply) => {
                if reply.send(decoder.prepare(width, height)).is_err() {
                    break;
                }
            }
            Job::Decode(frame, reply) => {
                let result = decoder.decode(&frame).map_err(|e| e.to_string());
                // A closed reply channel means the caller dropped us; stop.
                if reply.send(result).is_err() {
                    break;
                }
            }
            Job::DecodeFrames(frame, reply) => {
                if reply.send(decoder.decode_frames(&frame)).is_err() {
                    break;
                }
            }
            Job::Stop => break,
        }
    }
    // `decoder` drops here: driver and COM teardown stay on this thread.
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{rc::Rc, sync::{Arc, Mutex}, thread};

    #[test]
    fn batch_output_crosses_the_thread_once_without_replaying_the_access_unit() {
        struct Driver { owner:thread::ThreadId, calls:Arc<Mutex<Vec<&'static str>>> }
        impl Backend for Driver {
            fn backend_name(&self) -> &str { "batch fixture" }
            fn decode(&mut self, _: &EncodedFrame) -> Result<Option<RawFrame>, String> {
                panic!("batch request must reach the batch backend")
            }
            fn decode_frames(&mut self, frame: &EncodedFrame) -> Result<Vec<RawFrame>, String> {
                assert_eq!(thread::current().id(), self.owner);
                assert_eq!(frame.pts.0, 1);
                self.calls.lock().unwrap().push("batch");
                Ok([0,1].into_iter().map(|pts|RawFrame {
                    width:2,height:2,stride:2,format:crate::PixelFormat::I420,
                    data:bytes::Bytes::from(vec![0;6]),pts:crate::Timestamp(pts),
                    gpu:false,dma_buf:None,d3d11:None,
                }).collect())
            }
        }
        impl Drop for Driver {
            fn drop(&mut self) {
                assert_eq!(thread::current().id(), self.owner);
                self.calls.lock().unwrap().push("drop");
            }
        }
        let calls = Arc::new(Mutex::new(Vec::new()));
        let log = calls.clone();
        let mut worker = Worker::spawn(move || Ok(Driver { owner:thread::current().id(), calls:log })).unwrap();
        let pictures = worker.decode_frames(&EncodedFrame {
            data:bytes::Bytes::from_static(&[1]),is_keyframe:false,pts:crate::Timestamp(1),
        }).unwrap();
        assert_eq!(pictures.iter().map(|f|f.pts.0).collect::<Vec<_>>(), [0,1]);
        drop(worker);
        assert_eq!(*calls.lock().unwrap(), ["batch","drop"]);
    }

    #[test]
    fn preparation_decode_and_drop_keep_the_driver_thread_after_handle_moves() {
        struct Driver {
            owner: thread::ThreadId,
            calls: Arc<Mutex<Vec<&'static str>>>,
            _not_send: Rc<()>,
        }
        impl Backend for Driver {
            fn backend_name(&self) -> &str { "thread fixture" }
            fn prepare(&mut self, width: u32, height: u32) -> Result<(), String> {
                assert_eq!(thread::current().id(), self.owner);
                assert_eq!((width, height), (1280, 720));
                self.calls.lock().unwrap().push("prepare");
                Ok(())
            }
            fn decode(&mut self, _: &EncodedFrame) -> Result<Option<RawFrame>, String> {
                assert_eq!(thread::current().id(), self.owner);
                self.calls.lock().unwrap().push("decode");
                Ok(None)
            }
        }
        impl Drop for Driver {
            fn drop(&mut self) {
                assert_eq!(thread::current().id(), self.owner);
                self.calls.lock().unwrap().push("drop");
            }
        }
        let calls = Arc::new(Mutex::new(Vec::new()));
        let log = calls.clone();
        let caller = thread::current().id();
        let mut worker = Worker::spawn(move || {
            assert_ne!(thread::current().id(), caller);
            log.lock().unwrap().push("init");
            Ok(Driver { owner: thread::current().id(), calls: log, _not_send: Rc::new(()) })
        }).unwrap();
        thread::spawn(move || {
            worker.prepare(1280, 720).unwrap();
            worker.decode(&EncodedFrame {
                data: bytes::Bytes::new(), is_keyframe: true, pts: crate::Timestamp(0),
            }).unwrap();
            drop(worker);
        }).join().unwrap();
        assert_eq!(*calls.lock().unwrap(), ["init", "prepare", "decode", "drop"]);
    }
}
