//! Receive-side TWCC metadata must never await its RTCP feedback writer.
//! Keep the negotiated protocol and upstream RTCP encoder; collect small bounded
//! metadata records synchronously, and encode/send them on a separate task.
use async_trait::async_trait;
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use webrtc::{
    interceptor::{
        self, stream_info::StreamInfo, twcc::Recorder, Attributes, Interceptor, InterceptorBuilder,
        RTCPReader, RTCPWriter, RTPReader, RTPWriter,
    },
    rtp::{extension::transport_cc_extension::TransportCcExtension, packet::Packet},
    util::Unmarshal,
};

type Result<T> = std::result::Result<T, interceptor::Error>;
const LIMIT: usize = 16_384;
const URI: &str = "http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01";
#[derive(Default)]
struct Pending {
    packets: VecDeque<(u32, u16, i64)>,
}
struct Shared {
    pending: Mutex<Pending>,
    start: tokio::time::Instant,
    closed: AtomicBool,
    dropped: Arc<AtomicU64>,
}
impl Shared {
    fn record(&self, ssrc: u32, seq: u16) {
        if self.closed.load(Ordering::Relaxed) {
            return;
        }
        let at = self.start.elapsed().as_micros() as i64;
        let mut pending = self.pending.lock().unwrap();
        if pending.packets.len() == LIMIT {
            // A stalled feedback writer must not create an unbounded queue or
            // block video. Retire old metadata, never actual RTP/reference data.
            pending.packets.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        pending.packets.push_back((ssrc, seq, at));
    }
}
pub(crate) struct Builder(pub Arc<AtomicU64>);
impl InterceptorBuilder for Builder {
    fn build(&self, _: &str) -> Result<Arc<dyn Interceptor + Send + Sync>> {
        Ok(Arc::new(Receiver {
            shared: Arc::new(Shared {
                pending: Mutex::new(Pending::default()),
                start: tokio::time::Instant::now(),
                closed: AtomicBool::new(false),
                dropped: self.0.clone(),
            }),
            task: Mutex::new(None),
        }))
    }
}
struct Receiver {
    shared: Arc<Shared>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl Drop for Receiver {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Relaxed);
        if let Some(task) = self.task.get_mut().unwrap().take() {
            task.abort();
        }
    }
}
struct Reader {
    parent: Arc<dyn RTPReader + Send + Sync>,
    id: u8,
    shared: Arc<Shared>,
}
#[async_trait]
impl RTPReader for Reader {
    async fn read(&self, buf: &mut [u8], attr: &Attributes) -> Result<(Packet, Attributes)> {
        let (packet, attr) = self.parent.read(buf, attr).await?;
        if let Some(mut ext) = packet.header.get_extension(self.id) {
            let seq = TransportCcExtension::unmarshal(&mut ext)?.transport_sequence;
            self.shared.record(packet.header.ssrc, seq);
        }
        Ok((packet, attr))
    }
}
#[async_trait]
impl Interceptor for Receiver {
    async fn bind_remote_stream(
        &self,
        info: &StreamInfo,
        parent: Arc<dyn RTPReader + Send + Sync>,
    ) -> Arc<dyn RTPReader + Send + Sync> {
        match info
            .rtp_header_extensions
            .iter()
            .find(|e| e.uri == URI)
            .and_then(|e| u8::try_from(e.id).ok())
            .filter(|id| *id != 0)
        {
            Some(id) => Arc::new(Reader {
                parent,
                id,
                shared: self.shared.clone(),
            }),
            None => parent,
        }
    }
    async fn bind_rtcp_writer(
        &self,
        writer: Arc<dyn RTCPWriter + Send + Sync>,
    ) -> Arc<dyn RTCPWriter + Send + Sync> {
        let mut slot = self.task.lock().unwrap();
        if slot.is_some() || self.shared.closed.load(Ordering::Relaxed) {
            return writer;
        }
        let shared = self.shared.clone();
        let output = writer.clone();
        *slot = Some(tokio::spawn(async move {
            let mut recorder = Recorder::new(rand::random());
            let mut tick = tokio::time::interval(Duration::from_millis(100));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let packets = { std::mem::take(&mut shared.pending.lock().unwrap().packets) };
                for (ssrc, seq, at) in packets {
                    recorder.record(ssrc, seq, at);
                }
                let feedback = recorder.build_feedback_packet();
                // Neither metadata nor receive locks cross this await.
                if !feedback.is_empty()
                    && output.write(&feedback, &Attributes::new()).await.is_err()
                {
                    break;
                }
            }
        }));
        writer
    }
    async fn bind_rtcp_reader(
        &self,
        r: Arc<dyn RTCPReader + Send + Sync>,
    ) -> Arc<dyn RTCPReader + Send + Sync> {
        r
    }
    async fn bind_local_stream(
        &self,
        _: &StreamInfo,
        w: Arc<dyn RTPWriter + Send + Sync>,
    ) -> Arc<dyn RTPWriter + Send + Sync> {
        w
    }
    async fn unbind_local_stream(&self, _: &StreamInfo) {}
    async fn unbind_remote_stream(&self, _: &StreamInfo) {}
    async fn close(&self) -> Result<()> {
        self.shared.closed.store(true, Ordering::Relaxed);
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        self.shared.pending.lock().unwrap().packets.clear();
        Ok(())
    }
}

/// Same negotiation as webrtc-rs' receiver-only TWCC configuration, with no
/// one-slot async handoff inserted into every audio/video packet read.
pub(crate) fn configure(
    mut registry: interceptor::registry::Registry,
    media: &mut webrtc::api::media_engine::MediaEngine,
    dropped: Arc<AtomicU64>,
) -> anyhow::Result<interceptor::registry::Registry> {
    use webrtc::rtp_transceiver::{
        rtp_codec::{RTCRtpHeaderExtensionCapability, RTPCodecType},
        RTCPFeedback,
    };
    for kind in [RTPCodecType::Video, RTPCodecType::Audio] {
        media.register_feedback(
            RTCPFeedback {
                typ: "transport-cc".into(),
                parameter: String::new(),
            },
            kind,
        );
        media.register_header_extension(
            RTCRtpHeaderExtensionCapability { uri: URI.into() },
            kind,
            None,
        )?;
    }
    registry.add(Box::new(Builder(dropped)));
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use webrtc::{interceptor::stream_info::RTPHeaderExtension, util::Marshal};
    struct Source(AtomicU64);
    #[async_trait]
    impl RTPReader for Source {
        async fn read(&self, _: &mut [u8], attr: &Attributes) -> Result<(Packet, Attributes)> {
            let seq = self.0.fetch_add(1, Ordering::Relaxed) as u16;
            let mut header = webrtc::rtp::header::Header {
                ssrc: 42,
                sequence_number: seq,
                ..Default::default()
            };
            header.set_extension(
                3,
                TransportCcExtension {
                    transport_sequence: seq,
                }
                .marshal()?,
            )?;
            Ok((
                Packet {
                    header,
                    payload: bytes::Bytes::from_static(&[0x41, 7]),
                    ..Default::default()
                },
                attr.clone(),
            ))
        }
    }
    struct BlockedWriter {
        entered: tokio::sync::Notify,
        release: tokio::sync::Semaphore,
        feedback: Mutex<Vec<(u16, u16)>>,
    }
    #[async_trait]
    impl RTCPWriter for BlockedWriter {
        async fn write(
            &self,
            packets: &[Box<dyn webrtc::rtcp::packet::Packet + Send + Sync>],
            _: &Attributes,
        ) -> Result<usize> {
            for packet in packets {
                let cc = packet.as_any().downcast_ref::<webrtc::rtcp::transport_feedbacks::transport_layer_cc::TransportLayerCc>().unwrap();
                self.feedback
                    .lock()
                    .unwrap()
                    .push((cc.base_sequence_number, cc.packet_status_count));
            }
            self.entered.notify_one();
            self.release.acquire().await.unwrap().forget();
            Ok(packets.len())
        }
    }
    fn info() -> StreamInfo {
        StreamInfo {
            ssrc: 42,
            rtp_header_extensions: vec![RTPHeaderExtension {
                uri: URI.into(),
                id: 3,
            }],
            ..Default::default()
        }
    }
    async fn blocked_feedback_allows_video(
        interceptor: Arc<dyn Interceptor + Send + Sync>,
    ) -> bool {
        let writer = Arc::new(BlockedWriter {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
            feedback: Mutex::new(Vec::new()),
        });
        interceptor.bind_rtcp_writer(writer.clone()).await;
        let reader = interceptor
            .bind_remote_stream(&info(), Arc::new(Source(AtomicU64::new(0))))
            .await;
        for _ in 0..2 {
            reader.read(&mut [], &Attributes::new()).await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(2), writer.entered.notified())
            .await
            .unwrap();
        assert_eq!(writer.feedback.lock().unwrap()[0], (0, 2));
        let progressed = tokio::time::timeout(Duration::from_millis(50), async {
            for i in 2..130 {
                let (packet, _) = reader.read(&mut [], &Attributes::new()).await.unwrap();
                assert_eq!(packet.header.sequence_number, i);
                assert_eq!(packet.payload.as_ref(), &[0x41, 7]);
            }
        })
        .await
        .is_ok();
        writer.release.add_permits(10);
        tokio::time::timeout(Duration::from_secs(2), interceptor.close())
            .await
            .unwrap()
            .unwrap();
        progressed
    }
    #[tokio::test]
    async fn stalled_feedback_reproduces_upstream_block_but_does_not_block_new_receiver() {
        let upstream = webrtc::interceptor::twcc::receiver::Receiver::builder()
            .build("")
            .unwrap();
        assert!(
            !blocked_feedback_allows_video(upstream).await,
            "baseline must reproduce metadata backpressure"
        );
        assert!(blocked_feedback_allows_video(Builder(Arc::default()).build("").unwrap()).await);
    }
    #[tokio::test]
    async fn missing_extension_passthrough_and_metadata_memory_bound() {
        let interceptor = Builder(Arc::default()).build("").unwrap();
        let source: Arc<dyn RTPReader + Send + Sync> = Arc::new(Source(AtomicU64::new(0)));
        let reader = interceptor
            .bind_remote_stream(&StreamInfo::default(), source.clone())
            .await;
        assert!(Arc::ptr_eq(&reader, &source));
        interceptor.close().await.unwrap();
        let shared = Shared {
            pending: Mutex::default(),
            start: tokio::time::Instant::now(),
            closed: AtomicBool::new(false),
            dropped: Arc::default(),
        };
        for seq in 0..LIMIT + 7 {
            shared.record(42, seq as u16);
        }
        assert_eq!(shared.pending.lock().unwrap().packets.len(), LIMIT);
        assert_eq!(shared.dropped.load(Ordering::Relaxed), 7);
        shared.closed.store(true, Ordering::Relaxed);
        shared.record(42, 0);
        assert_eq!(shared.pending.lock().unwrap().packets.len(), LIMIT);
    }
}
