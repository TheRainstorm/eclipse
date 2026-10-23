//! Bounded, optional native video XOR protection over an unreliable DTLS/SCTP
//! channel. Source RTP never waits for parity delivery or a missing packet.
mod codec;
#[cfg(test)]
mod tests;
use anyhow::{ensure, Result};
use bytes::Bytes;
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};
use tokio::sync::{mpsc, Notify};
use webrtc::{
    data_channel::{
        data_channel_init::RTCDataChannelInit, data_channel_state::RTCDataChannelState,
        RTCDataChannel,
    },
    peer_connection::{sdp::session_description::RTCSessionDescription, RTCPeerConnection},
    rtp::packet::Packet,
};

pub(crate) const PAYLOAD: usize = codec::PAYLOAD;
// Explicitly negotiated, reserved by the native protocol. The automatically
// negotiated control/clipboard/gamepad channels must not use this stream id.
const STREAM: u16 = 62;
const ATTRIBUTE: &str = "a=x-eclipse-video-fec:xor1-8-20-s62";
fn supported(sdp: &str) -> bool {
    let mut attributes = sdp
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("a=x-eclipse-video-fec:"));
    attributes.next() == Some(ATTRIBUTE) && attributes.next().is_none()
}
fn advertise(description: &mut RTCSessionDescription) {
    let at = description.sdp.find("m=").unwrap_or(description.sdp.len());
    description.sdp.insert_str(at, &format!("{ATTRIBUTE}\r\n"));
}
#[derive(Default)]
struct ReceiveState {
    decoder: codec::Decoder,
    ready: VecDeque<Packet>,
}
pub(crate) struct Receive {
    state: Mutex<ReceiveState>,
    notify: Notify,
    enabled: AtomicBool,
    stopped: AtomicBool,
    stats: Option<Arc<crate::media::MediaStats>>,
}
impl Receive {
    fn new(stats: Option<Arc<crate::media::MediaStats>>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ReceiveState::default()),
            notify: Notify::new(),
            enabled: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            stats,
        })
    }
    pub fn source(&self, ssrc: u32, payload: u8) {
        self.state.lock().unwrap().decoder.source(ssrc, payload);
    }
    fn push(&self, state: &mut ReceiveState, packets: Vec<Packet>) {
        for packet in packets {
            if state.ready.len() < 64 {
                state.ready.push_back(packet);
            } else {
                self.dropped();
            }
        }
        if !state.ready.is_empty() {
            self.notify.notify_one();
        }
    }
    fn dropped(&self) {
        if let Some(s) = &self.stats {
            s.fec_drops.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn parity(&self, data: &[u8]) {
        if !self.enabled.load(Ordering::Acquire) || self.stopped.load(Ordering::Acquire) {
            return;
        }
        let Some(parity) = codec::Parity::parse(data) else {
            self.dropped();
            return;
        };
        if let Some(s) = &self.stats {
            s.fec_packets.fetch_add(1, Ordering::Relaxed);
            s.fec_bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
        }
        let mut state = self.state.lock().unwrap();
        let packets = state.decoder.parity(parity, Instant::now());
        self.push(&mut state, packets);
    }
    pub fn observe(&self, packet: &Packet) {
        if !self.enabled.load(Ordering::Acquire) || self.stopped.load(Ordering::Acquire) {
            return;
        }
        let mut state = self.state.lock().unwrap();
        let packets = state.decoder.packet(packet, Instant::now());
        self.push(&mut state, packets);
    }
    pub async fn next(&self) -> Packet {
        loop {
            let notified = self.notify.notified();
            if let Some(packet) = self.state.lock().unwrap().ready.pop_front() {
                if let Some(s) = &self.stats {
                    s.fec_recovered.fetch_add(1, Ordering::Relaxed);
                }
                return packet;
            }
            notified.await;
        }
    }
}
pub(crate) struct Transport {
    sender: bool,
    channel: Mutex<Option<Arc<RTCDataChannel>>>,
    outgoing: Mutex<Option<mpsc::Sender<(Bytes, Instant)>>>,
    pub receive: Arc<Receive>,
    negotiated: AtomicBool,
    opened: Arc<AtomicBool>,
}
impl Transport {
    pub fn new(sender: bool, stats: Option<Arc<crate::media::MediaStats>>) -> Arc<Self> {
        Arc::new(Self {
            sender,
            channel: Mutex::new(None),
            outgoing: Mutex::new(None),
            receive: Receive::new(stats),
            negotiated: AtomicBool::new(false),
            opened: Arc::new(AtomicBool::new(false)),
        })
    }
    pub async fn receiver(&self, pc: &RTCPeerConnection) -> Result<()> {
        ensure!(!self.sender, "FEC direction mismatch");
        self.open(pc).await
    }
    async fn open(&self, pc: &RTCPeerConnection) -> Result<()> {
        if self.channel.lock().unwrap().is_some() {
            return Ok(());
        }
        let channel = pc
            .create_data_channel(
                "eclipse-video-fec-v1",
                Some(RTCDataChannelInit {
                    ordered: Some(false),
                    // This webrtc-rs version collapses Some(0) retransmits to
                    // its reliable default. A positive, short PR-SCTP lifetime
                    // gives parity an actual expiry without changing video.
                    max_packet_life_time: Some(5),
                    negotiated: Some(STREAM),
                    ..Default::default()
                }),
            )
            .await?;
        let receive = self.receive.clone();
        let opened = self.opened.clone();
        channel.on_open(Box::new(move || {
            opened.store(true, Ordering::Release);
            Box::pin(async {})
        }));
        let opened = self.opened.clone();
        let closed = self.receive.clone();
        channel.on_close(Box::new(move || {
            opened.store(false, Ordering::Release);
            closed.enabled.store(false, Ordering::Release);
            if let Some(stats) = &closed.stats {
                stats.fec_enabled.store(false, Ordering::Release);
            }
            Box::pin(async {})
        }));
        channel.on_message(Box::new(move |message| {
            receive.parity(&message.data);
            Box::pin(async {})
        }));
        if self.sender {
            let (tx, mut rx) = mpsc::channel::<(Bytes, Instant)>(8);
            *self.outgoing.lock().unwrap() = Some(tx);
            let weak = Arc::downgrade(&channel);
            tokio::spawn(async move {
                while let Some((bytes, at)) = rx.recv().await {
                    let Some(channel) = weak.upgrade() else {
                        break;
                    };
                    if at.elapsed() > std::time::Duration::from_millis(10)
                        || channel.ready_state() != RTCDataChannelState::Open
                        || channel.buffered_amount().await > 4 * codec::MAX_WIRE
                    {
                        continue;
                    }
                    // Never put SCTP backpressure on capture/RTP. An obsolete
                    // parity send may be skipped; NACK/RTX remains available.
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_millis(5),
                        channel.send(&bytes),
                    )
                    .await;
                }
            });
        }
        *self.channel.lock().unwrap() = Some(channel);
        Ok(())
    }
    pub fn description(&self, description: &mut RTCSessionDescription) {
        if self.channel.lock().unwrap().is_some() {
            advertise(description);
        }
    }
    pub async fn remote(
        &self,
        pc: &RTCPeerConnection,
        description: &RTCSessionDescription,
    ) -> Result<()> {
        let enabled = supported(&description.sdp);
        if self.sender && enabled {
            self.open(pc).await?;
        }
        self.negotiated.store(enabled, Ordering::Release);
        self.receive.enabled.store(enabled, Ordering::Release);
        if let Some(s) = &self.receive.stats {
            s.fec_enabled.store(enabled, Ordering::Release);
            s.diagnostics.record(serde_json::json!({"event":"native_fec","enabled":enabled,"scheme":"xor1-8","protection_budget_percent":20}));
        }
        Ok(())
    }
    pub fn enabled(&self) -> bool {
        self.negotiated.load(Ordering::Acquire)
    }
    pub fn ready(&self) -> bool {
        self.enabled() && self.opened.load(Ordering::Acquire)
    }
    pub fn send(&self, bytes: Bytes) {
        if self.ready() {
            if let Some(tx) = self.outgoing.lock().unwrap().as_ref() {
                let _ = tx.try_send((bytes, Instant::now()));
            }
        }
    }
    pub fn stop(&self) {
        self.negotiated.store(false, Ordering::Release);
        self.receive.enabled.store(false, Ordering::Release);
        self.receive.stopped.store(true, Ordering::Release);
        self.outgoing.lock().unwrap().take();
        *self.receive.state.lock().unwrap() = ReceiveState::default();
        if let Some(s) = &self.receive.stats {
            s.fec_enabled.store(false, Ordering::Release);
        }
    }
}
pub(crate) struct Encoder {
    transport: Arc<Transport>,
    codec: codec::Encoder,
}
impl Encoder {
    pub fn new(transport: Arc<Transport>) -> Self {
        Self {
            transport,
            codec: codec::Encoder::default(),
        }
    }
    pub fn enabled(&self) -> bool {
        self.transport.ready()
    }
    pub fn packet(&mut self, packet: &Packet) {
        if self.transport.ready() {
            if let Some(bytes) = self.codec.push(packet) {
                self.transport.send(bytes);
            }
        }
    }
}
