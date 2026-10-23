//! Bounded synchronous encoder facade; driver resources never cross OS threads.
use super::error;
use crate::{codec::CodecError, EncodedFrame, RawFrame, VideoEncoder};
use std::{
    sync::mpsc::{self, SyncSender},
    thread::JoinHandle,
    time::Duration,
};

/// Whether the accepted update retained the encoded reference chain. Drivers
/// must require a fresh picture unless their live-update API guarantees this.
pub(super) enum BitrateChange {
    // Some platform builds only include drivers requiring a new reference chain.
    #[allow(dead_code)]
    Preserved,
    KeyframeRequired,
}
pub(super) trait Driver {
    fn backend_name(&self) -> &str;
    fn gpu_input(&self)->bool {false}
    fn gpu_vendor(&self)->Option<u16> {None}
    fn codec(&self)->uu_proto::media::VideoCodec{uu_proto::media::VideoCodec::H264}
    fn encode(&mut self, frame: &RawFrame, keyframe: bool) -> Result<Vec<EncodedFrame>, String>;
    fn bitrate(&mut self, kbps: u32) -> Result<BitrateChange, String>;
}

enum Job {
    Encode(
        RawFrame,
        bool,
        mpsc::Sender<Result<Vec<EncodedFrame>, String>>,
    ),
    Bitrate(u32, mpsc::Sender<Result<BitrateChange, String>>),
}
pub(super) struct Worker {
    sender: Option<SyncSender<Job>>,
    thread: Option<JoinHandle<()>>,
    name: String,
    codec:uu_proto::media::VideoCodec,
    gpu_input:bool,
    gpu_vendor:Option<u16>,
    keyframe: bool,
}
impl Worker {
    pub fn spawn<D: Driver + 'static>(
        open: impl FnOnce() -> Result<D, String> + Send + 'static,
    ) -> Result<Self, CodecError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let (ready, ready_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("uu-hwenc".into())
            .spawn(move || {
                let mut driver = match open() {
                    Ok(driver) => driver,
                    Err(reason) => {
                        let _ = ready.send(Err(reason));
                        return;
                    }
                };
                if ready.send(Ok((driver.backend_name().to_owned(),driver.codec(),driver.gpu_input(),driver.gpu_vendor()))).is_err() {
                    return;
                }
                while let Ok(job) = receiver.recv() {
                    let replied = match job {
                        Job::Encode(frame, keyframe, reply) => {
                            reply.send(driver.encode(&frame, keyframe)).is_ok()
                        }
                        Job::Bitrate(kbps, reply) => reply.send(driver.bitrate(kbps)).is_ok(),
                    };
                    if !replied {
                        break;
                    }
                }
                // Driver resources and COM apartments stay on their creating thread.
            })
            .map_err(|e| error(format!("hardware encoder thread: {e}")))?;
        let (name,codec,gpu_input,gpu_vendor) = match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(name)) => name,
            Ok(Err(reason)) => {
                let _ = thread.join();
                return Err(error(reason));
            }
            Err(_) => {
                return Err(error(
                    "hardware encoder did not initialize within 10 seconds",
                ))
            }
        };
        Ok(Self {
            sender: Some(sender),
            thread: Some(thread),
            name,
            codec,
            gpu_input,
            gpu_vendor,
            keyframe: true,
        })
    }
    fn reply<T>(&mut self, receiver: mpsc::Receiver<Result<T, String>>) -> Result<T, CodecError> {
        match receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(result) => result.map_err(error),
            Err(_) => {
                // Do not enqueue more work or block Drop on a stalled driver.
                // A returning driver sees the disconnected reply/command queue
                // and releases its own resources; no thread is forcibly killed.
                self.sender = None;
                Err(error("hardware encoder stopped responding"))
            }
        }
    }
}
impl VideoEncoder for Worker {
    fn is_hardware(&self)->bool {true}
    fn gpu_input(&self)->bool {self.gpu_input}
    fn gpu_vendor(&self)->Option<u16> {self.gpu_vendor}
    fn backend_name(&self) -> &str {
        &self.name
    }
    fn codec(&self) -> uu_proto::media::VideoCodec {
        self.codec
    }
    fn encode(&mut self, frame: &RawFrame) -> Result<Vec<EncodedFrame>, CodecError> {
        let (reply, receiver) = mpsc::channel();
        self.sender
            .as_ref()
            .ok_or_else(|| error("hardware encoder is unavailable"))?
            .send(Job::Encode(frame.clone(), self.keyframe, reply))
            .map_err(|_| error("hardware encoder thread ended"))?;
        let result = self.reply(receiver);
        if result.is_ok() {
            self.keyframe = false;
        }
        result
    }
    fn request_keyframe(&mut self) {
        self.keyframe = true;
    }
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), CodecError> {
        let (reply, receiver) = mpsc::channel();
        self.sender
            .as_ref()
            .ok_or_else(|| error("hardware encoder is unavailable"))?
            .send(Job::Bitrate(kbps, reply))
            .map_err(|_| error("hardware encoder thread ended"))?;
        let change=self.reply(receiver)?;
        self.keyframe |= matches!(change,BitrateChange::KeyframeRequired);
        Ok(())
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let responding = self.sender.take().is_some();
        if let Some(thread) = self.thread.take() {
            if responding || thread.is_finished() {
                let _ = thread.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        rc::Rc,
        sync::{Arc, Mutex},
        thread::ThreadId,
    };
    struct Affine {
        // A !Send driver is constructed and destroyed inside the worker.
        _owner: Rc<()>,
        trace: Arc<Mutex<Vec<ThreadId>>>,
        preserve: bool,
    }
    impl Driver for Affine {
        fn backend_name(&self) -> &str {
            "owned affine fixture"
        }
        fn encode(
            &mut self,
            frame: &RawFrame,
            keyframe: bool,
        ) -> Result<Vec<EncodedFrame>, String> {
            self.trace.lock().unwrap().push(std::thread::current().id());
            Ok(vec![EncodedFrame {
                data: Vec::new().into(),
                pts: frame.pts,
                is_keyframe: keyframe,
            }])
        }
        fn bitrate(&mut self, kbps: u32) -> Result<BitrateChange, String> {
            self.trace.lock().unwrap().push(std::thread::current().id());
            if kbps==0{return Err("invalid fixture bitrate".into());}
            Ok(if self.preserve {BitrateChange::Preserved}else{BitrateChange::KeyframeRequired})
        }
    }
    impl Drop for Affine {
        fn drop(&mut self) {
            self.trace.lock().unwrap().push(std::thread::current().id());
        }
    }
    #[test]
    fn driver_lifecycle_stays_on_one_thread_and_requests_do_not_queue_old_frames() {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let observed = trace.clone();
        let mut worker = Worker::spawn(move || {
            observed.lock().unwrap().push(std::thread::current().id());
            Ok(Affine {
                _owner: Rc::new(()),
                trace: observed,
                preserve: false,
            })
        })
        .unwrap();
        let mut frame = RawFrame {
            width: 2,
            height: 2,
            stride: 2,
            format: crate::PixelFormat::I420,
            data: vec![128; 6].into(),
            pts: crate::Timestamp(0),
            gpu: false,
            dma_buf: None,
            d3d11: None,
        };
        assert!(worker.encode(&frame).unwrap()[0].is_keyframe);
        frame.pts = crate::Timestamp(16_667);
        let coded = worker.encode(&frame).unwrap().remove(0);
        assert!(!coded.is_keyframe);
        assert_eq!(coded.pts, frame.pts);
        worker.request_keyframe();
        assert!(worker.encode(&frame).unwrap()[0].is_keyframe);
        worker.set_bitrate(1000).unwrap();
        drop(worker);
        let trace = trace.lock().unwrap();
        assert_eq!(trace.len(), 6);
        assert_ne!(trace[0], std::thread::current().id());
        assert!(trace.iter().all(|thread| *thread == trace[0]));
    }
    #[test]
    fn bitrate_updates_follow_reference_requirements_without_losing_pending_recovery() {
        for preserve in [false,true] {
            let mut worker=Worker::spawn(move||Ok(Affine{_owner:Rc::new(()),trace:Arc::default(),preserve})).unwrap();
            let frame=RawFrame {width:2,height:2,stride:2,format:crate::PixelFormat::I420,
                data:vec![128;6].into(),pts:crate::Timestamp(0),gpu:false,dma_buf:None,d3d11:None};
            assert!(worker.encode(&frame).unwrap()[0].is_keyframe);
            worker.set_bitrate(1000).unwrap();
            assert_eq!(worker.encode(&frame).unwrap()[0].is_keyframe,!preserve);
            worker.request_keyframe();worker.set_bitrate(2000).unwrap();
            assert!(worker.encode(&frame).unwrap()[0].is_keyframe,"rate update lost an explicit recovery request");
            worker.request_keyframe();assert!(worker.set_bitrate(0).is_err());
            assert!(worker.encode(&frame).unwrap()[0].is_keyframe,"failed rate update lost pending recovery");
            assert!(!worker.encode(&frame).unwrap()[0].is_keyframe);
        }
    }
}
