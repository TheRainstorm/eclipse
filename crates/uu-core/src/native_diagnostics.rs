//! Always-on, bounded transport metadata. No addresses, SDP, credentials,
//! clipboard, input contents or media bytes are retained. Packet paths only
//! update atomics; a connection-owned sampler writes a summary every 5 seconds.
use serde_json::{json, Value};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
        Arc, Mutex, Weak,
    },
    time::{Duration, Instant},
};
use webrtc::{
    peer_connection::RTCPeerConnection,
    stats::StatsReportType,
    util::{Conn, Result},
};

#[derive(Clone, Copy)]
pub(crate) enum ProducerPhase {
    Preparing,
    Pacing,
    Reconfiguring,
    Capturing,
    Encoding,
    Enqueueing,
    Stopping,
    Stopped,
}
pub(crate) struct ProducerProgress {
    phase: AtomicU64,
    changed_ms: AtomicU64,
}
impl Default for ProducerProgress {
    fn default() -> Self {
        Self {
            phase: AtomicU64::new(ProducerPhase::Preparing as u64),
            changed_ms: AtomicU64::new(crate::time::now_ms()),
        }
    }
}
impl ProducerProgress {
    pub fn set(&self, phase: ProducerPhase) {
        self.changed_ms.store(crate::time::now_ms(), Relaxed);
        self.phase.store(phase as u64, Relaxed);
    }
    fn snapshot(&self) -> Value {
        let phase = match self.phase.load(Relaxed) {
            0 => "preparing",
            1 => "pacing",
            2 => "reconfiguring",
            3 => "capturing",
            4 => "encoding",
            5 => "enqueueing",
            6 => "stopping",
            _ => "stopped",
        };
        json!({"phase":phase,"phase_age_ms":crate::time::now_ms().saturating_sub(self.changed_ms.load(Relaxed))})
    }
}

#[derive(Default)]
struct Direction {
    packets: AtomicU64,
    bytes: AtomicU64,
    last: AtomicU64,
    errors: AtomicU64,
    pending: AtomicU64,
}
impl Direction {
    fn snapshot(&self, now: u64) -> Value {
        let age = |value| (value != 0).then(|| now.saturating_sub(value));
        json!({"packets":self.packets.load(Relaxed),"bytes":self.bytes.load(Relaxed),
            "last_age_ms":age(self.last.load(Relaxed)),"errors":self.errors.load(Relaxed),
            "pending_ms":age(self.pending.load(Relaxed))})
    }
}

pub(crate) struct Diagnostics {
    id: uuid::Uuid,
    role: &'static str,
    started: Instant,
    log: crate::session_diagnostics::SessionDiagnostics,
    stats: Option<Weak<crate::media::MediaStats>>,
    offer_id: AtomicU64,
    closing: AtomicBool,
    exited: AtomicBool,
    socket_observed: AtomicBool,
    receive: Direction,
    send: Direction,
    captured: Mutex<Option<Arc<AtomicU64>>>,
    producer: Mutex<Option<Arc<ProducerProgress>>>,
    input: Mutex<Option<Weak<crate::native_control::HostControl>>>,
    sent: AtomicU64,
    video_send_started: AtomicU64,
    audio_send_started: AtomicU64,
}
impl Diagnostics {
    /// Only the capture/encoder worker may supply this error. Authentication,
    /// network payloads and user input must never enter the diagnostic detail.
    pub fn producer_failed(&self, error: &anyhow::Error) {
        let kind = match error.downcast_ref::<uu_capture::CaptureError>() {
            Some(uu_capture::CaptureError::Suspended(_)) => "capture_suspended",
            Some(_) => "capture_failed",
            None => "producer_failed",
        };
        let detail: String = format!("{error:#}").chars().take(1024)
            .map(|c| if c.is_control() { ' ' } else { c }).collect();
        self.record(json!({"event":"native_producer_failed","kind":kind,"detail":detail}));
        self.exit("capture_or_encoder_failed");
    }
    pub fn new(stats: Option<&Arc<crate::media::MediaStats>>, sender: bool) -> Arc<Self> {
        let log = crate::session_diagnostics::SessionDiagnostics::default();
        #[cfg(not(test))]
        if let Ok(config) = crate::Config::from_env() {
            log.start_transport([config.state_dir.join("diagnostics")]);
        }
        let this = Arc::new(Self {
            id: uuid::Uuid::new_v4(),
            role: if sender { "host" } else { "viewer" },
            started: Instant::now(),
            log,
            stats: stats.map(Arc::downgrade),
            offer_id: AtomicU64::new(0),
            closing: AtomicBool::new(false),
            exited: AtomicBool::new(false),
            socket_observed: AtomicBool::new(false),
            receive: Direction::default(),
            send: Direction::default(),
            captured: Mutex::new(None),
            producer: Mutex::new(None),
            input: Mutex::new(None),
            sent: AtomicU64::new(0),
            video_send_started: AtomicU64::new(0),
            audio_send_started: AtomicU64::new(0),
        });
        this.record(json!({"event":"native_transport_created","pid":std::process::id(),"version":env!("CARGO_PKG_VERSION")}));
        if let Some(stats) = stats {
            stats.diagnostics.record(
                json!({"event":"native_transport_link","peer_id":this.id,"role":this.role}),
            );
        }
        this
    }
    fn now(&self) -> u64 {
        1 + self
            .started
            .elapsed()
            .as_millis()
            .min((u64::MAX - 1) as u128) as u64
    }
    pub fn record(&self, mut value: Value) {
        value["peer_id"] = json!(self.id);
        value["role"] = json!(self.role);
        value["elapsed_ms"] = json!(self.started.elapsed().as_millis() as u64);
        let offer = self.offer_id.load(Relaxed);
        if offer != 0 {
            value["offer_id"] = json!(offer);
        }
        self.log.record(value);
    }
    /// The public numeric SDP origin is only a correlation nonce. Never log
    /// its username/address, ICE ufrag/password, certificate or the SDP itself.
    pub fn offer(
        &self,
        description: &webrtc::peer_connection::sdp::session_description::RTCSessionDescription,
    ) {
        if let Ok(sdp) = description.unmarshal() {
            self.offer_id.store(sdp.origin.session_id, Relaxed);
            self.record(json!({"event":"native_transport_negotiation"}));
        }
    }
    pub fn exit(&self, cause: &'static str) {
        if !self.exited.swap(true, Relaxed) {
            self.record(json!({"event":"native_session_exit","cause":cause}));
            tracing::info!(peer_id=%self.id,offer_id=self.offer_id.load(Relaxed),role=self.role,cause,"native session exit");
        }
    }
    pub fn closing(&self) {
        if !self.closing.swap(true, Relaxed) {
            self.record(json!({"event":"native_transport_closing"}));
        }
    }
    pub fn capture_counter(&self, counter: &Arc<AtomicU64>, producer: &Arc<ProducerProgress>) {
        *self.captured.lock().unwrap() = Some(counter.clone());
        *self.producer.lock().unwrap() = Some(producer.clone());
    }
    pub fn input_source(&self, input: &Arc<crate::native_control::HostControl>) {
        *self.input.lock().unwrap() = Some(Arc::downgrade(input));
    }
    pub fn video_send(&self) -> VideoSend<'_> {
        self.video_send_started.store(self.now(), Relaxed);
        VideoSend {
            log: self,
            video: true,
        }
    }
    pub fn audio_send(&self) -> VideoSend<'_> {
        self.audio_send_started.store(self.now(), Relaxed);
        VideoSend {
            log: self,
            video: false,
        }
    }
    fn snapshot(&self, pc: &RTCPeerConnection) -> Value {
        let now = self.now();
        let capture = self
            .captured
            .lock()
            .unwrap()
            .as_ref()
            .map(|v| v.load(Relaxed));
        let producer = self.producer.lock().unwrap().as_ref().map(|v| v.snapshot());
        let input = self.input.lock().unwrap().as_ref().and_then(Weak::upgrade).map(|v|v.diagnostic_state());
        let stats = self.stats.as_ref().and_then(Weak::upgrade);
        let pending = self.video_send_started.load(Relaxed);
        let audio_pending = self.audio_send_started.load(Relaxed);
        json!({"event":"native_transport_sample","peer_state":pc.connection_state().to_string(),
            "ice_state":pc.ice_connection_state().to_string(),"dtls_state":pc.sctp().transport().state().to_string(),
            "sctp_state":pc.sctp().state().to_string(),"closing":self.closing.load(Relaxed),
            "udp_observed":self.socket_observed.load(Relaxed),"udp_receive":self.receive.snapshot(now),"udp_send":self.send.snapshot(now),
            "captured":capture,"producer":producer,"input":input,"video_sent":self.sent.load(Relaxed),"video_send_pending_ms":(pending!=0).then(||now.saturating_sub(pending)),
            "audio_send_pending_ms":(audio_pending!=0).then(||now.saturating_sub(audio_pending)),
            "rtp_received":stats.as_ref().map(|s|s.packets.load(Relaxed)),
            "decode_errors":stats.as_ref().map(|s|s.errors.load(Relaxed)),
            "decoded":stats.as_ref().map(|s|s.decoded_total.load(Relaxed)),
            "control_open":stats.as_ref().map(|s|s.control_open.load(Relaxed)),
            "media_epoch":stats.as_ref().map(|s|s.native_media_epoch.load(Relaxed))})
    }
    pub fn final_sample(&self, pc: &RTCPeerConnection) {
        self.record(self.snapshot(pc));
    }
    pub fn monitor(self: &Arc<Self>, pc: &Arc<RTCPeerConnection>) -> Monitor {
        let pc = Arc::downgrade(pc);
        let log = self.clone();
        Monitor(tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_secs(5));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                timer.tick().await;
                let Some(pc) = pc.upgrade() else { break };
                log.record(log.snapshot(&pc));
                // The summary above survives a stats API stall. Its bounded
                // poll neither delays packet reads nor keeps teardown waiting.
                match tokio::time::timeout(Duration::from_millis(750), pc.get_stats()).await {
                    Ok(reports) => {
                        for report in reports.reports.values() {
                            if let StatsReportType::CandidatePair(pair) = report {
                                if pair.nominated {
                                    log.record(json!({"event":"native_ice_sample","state":pair.state.to_string(),
                                    "bytes_sent":pair.bytes_sent,"bytes_received":pair.bytes_received,
                                    "requests_sent":pair.requests_sent,"responses_received":pair.responses_received,
                                    "current_rtt_ms":pair.current_round_trip_time*1000.0}));
                                }
                            }
                        }
                    }
                    Err(_) => log.record(json!({"event":"native_transport_stats_timeout"})),
                }
            }
        }))
    }
    fn io_result(&self, send: bool, result: std::result::Result<usize, &webrtc::util::Error>) {
        let direction = if send { &self.send } else { &self.receive };
        match result {
            Ok(bytes) => {
                direction.packets.fetch_add(1, Relaxed);
                direction.bytes.fetch_add(bytes as u64, Relaxed);
                direction.last.store(self.now(), Relaxed);
            }
            Err(error) => {
                let count = direction.errors.fetch_add(1, Relaxed) + 1;
                if count.is_power_of_two() {
                    let (kind, os) = match error {
                        webrtc::util::Error::Io(e) => {
                            (format!("{:?}", e.0.kind()), e.0.raw_os_error())
                        }
                        _ => ("transport_error".into(), None),
                    };
                    self.record(json!({"event":"native_udp_error","direction":if send {"send"} else {"receive"},
                        "count":count,"io_kind":kind,"os_error":os,"closing":self.closing.load(Relaxed)}));
                }
            }
        }
    }
}
pub(crate) struct Monitor(tokio::task::JoinHandle<()>);
impl Drop for Monitor {
    fn drop(&mut self) {
        self.0.abort();
    }
}
pub(crate) struct VideoSend<'a> {
    log: &'a Diagnostics,
    video: bool,
}
impl VideoSend<'_> {
    pub fn complete(&self) {
        if self.video {
            self.log.sent.fetch_add(1, Relaxed);
        }
    }
}
impl Drop for VideoSend<'_> {
    fn drop(&mut self) {
        if self.video {
            &self.log.video_send_started
        } else {
            &self.log.audio_send_started
        }
        .store(0, Relaxed);
    }
}
struct ReadCall<'a>(&'a Direction);
impl Drop for ReadCall<'_> {
    fn drop(&mut self) {
        self.0.pending.store(0, Relaxed);
    }
}

pub(crate) struct Observed<C> {
    pub socket: C,
    pub log: Arc<Diagnostics>,
}
#[async_trait::async_trait]
impl<C: Conn + Send + Sync> Conn for Observed<C> {
    async fn connect(&self, addr: SocketAddr) -> Result<()> {
        self.socket.connect(addr).await
    }
    async fn recv(&self, b: &mut [u8]) -> Result<usize> {
        self.log.receive.pending.store(self.log.now(), Relaxed);
        let _call = ReadCall(&self.log.receive);
        let result = self.socket.recv(b).await;
        self.log.io_result(false, result.as_ref().copied());
        result
    }
    async fn recv_from(&self, b: &mut [u8]) -> Result<(usize, SocketAddr)> {
        self.log.receive.pending.store(self.log.now(), Relaxed);
        let _call = ReadCall(&self.log.receive);
        let result = self.socket.recv_from(b).await;
        self.log.io_result(false, result.as_ref().map(|v| v.0));
        result
    }
    async fn send(&self, b: &[u8]) -> Result<usize> {
        let result = self.socket.send(b).await;
        self.log.io_result(true, result.as_ref().copied());
        result
    }
    async fn send_to(&self, b: &[u8], addr: SocketAddr) -> Result<usize> {
        let result = self.socket.send_to(b, addr).await;
        self.log.io_result(true, result.as_ref().copied());
        result
    }
    fn local_addr(&self) -> Result<SocketAddr> {
        self.socket.local_addr()
    }
    fn remote_addr(&self) -> Option<SocketAddr> {
        self.socket.remote_addr()
    }
    async fn close(&self) -> Result<()> {
        self.socket.close().await
    }
    fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
        self.socket.as_any()
    }
}

pub(crate) fn udp_params<C: Conn + Send + Sync + 'static>(
    socket: C,
    trace: Option<Arc<crate::socket_trace::Observation>>,
    log: Arc<Diagnostics>,
) -> webrtc::ice::udp_mux::UDPMuxParams {
    log.socket_observed.store(true, Relaxed);
    let observed = Observed { socket, log };
    match trace {
        Some(trace) => webrtc::ice::udp_mux::UDPMuxParams::new(crate::socket_trace::Observed {
            socket: observed,
            trace,
        }),
        None => webrtc::ice::udp_mux::UDPMuxParams::new(observed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UdpSocket;
    use webrtc::ice::udp_mux::{UDPMux, UDPMuxDefault};

    #[test]
    fn producer_failure_keeps_the_capture_cause_and_bounded_detail_before_cleanup() {
        let log = Diagnostics::new(None, true);
        let events = log.log.capture().unwrap();
        let error = anyhow::Error::new(uu_capture::CaptureError::Suspended("fixture compositor pause".into()))
            .context("capture worker");
        log.producer_failed(&error);
        log.exit("stop_requested");
        let rows: Vec<_> = events.try_iter().collect();
        let failure = rows.iter().find(|row| row["event"] == "native_producer_failed").unwrap();
        assert_eq!(failure["kind"], "capture_suspended");
        assert!(failure["detail"].as_str().unwrap().contains("fixture compositor pause"));
        assert!(rows.iter().any(|row| row["event"] == "native_session_exit" && row["cause"] == "capture_or_encoder_failed"));
        assert!(!rows.iter().any(|row| row["cause"] == "stop_requested"));
        log.producer_failed(&anyhow::anyhow!("{}\nend", "x".repeat(2048)));
        let rows: Vec<_> = events.try_iter().collect();
        let detail = rows[0]["detail"].as_str().unwrap();
        assert_eq!(detail.chars().count(), 1024);
        assert!(!detail.chars().any(char::is_control));
    }

    #[tokio::test]
    async fn real_udp_and_cancelled_reads_preserve_socket_semantics_and_only_record_metadata() {
        let log = Diagnostics::new(None, false);
        let events = log.log.capture().unwrap();
        let a = Observed {
            socket: UdpSocket::bind("127.0.0.1:0").await.unwrap(),
            log: log.clone(),
        };
        let b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let a_addr = a.local_addr().unwrap();
        let b_addr = b.local_addr().unwrap();
        let payload = b"private media and clipboard must never appear in logs";
        assert_eq!(a.send_to(payload, b_addr).await.unwrap(), payload.len());
        let mut buffer = [0; 128];
        let (size, from) = b.recv_from(&mut buffer).await.unwrap();
        assert_eq!(&buffer[..size], payload);
        assert_eq!(from, a_addr);
        b.send_to(b"reply", a_addr).await.unwrap();
        let (size, from) = a.recv_from(&mut buffer).await.unwrap();
        assert_eq!(&buffer[..size], b"reply");
        assert_eq!(from, b_addr);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), a.recv_from(&mut buffer))
                .await
                .is_err()
        );
        assert_eq!(log.receive.pending.load(Relaxed), 0);
        assert_eq!(log.receive.packets.load(Relaxed), 1);
        assert_eq!(log.send.packets.load(Relaxed), 1);
        assert!(a.as_any().is::<UdpSocket>());
        assert!(a.send(payload).await.is_err());
        let errors: Vec<_> = events.try_iter().collect();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["event"], "native_udp_error");
        let serialized = serde_json::to_string(&errors).unwrap();
        assert!(!serialized.contains("private media") && !serialized.contains("127.0.0.1"));
        assert!(!serialized.contains("\"address\""));
    }

    struct FailedReceive(UdpSocket);
    #[async_trait::async_trait]
    impl Conn for FailedReceive {
        async fn connect(&self, a: SocketAddr) -> Result<()> {
            Ok(self.0.connect(a).await?)
        }
        async fn recv(&self, _: &mut [u8]) -> Result<usize> {
            Err(std::io::Error::from_raw_os_error(10054).into())
        }
        async fn recv_from(&self, _: &mut [u8]) -> Result<(usize, SocketAddr)> {
            Err(std::io::Error::from_raw_os_error(10054).into())
        }
        async fn send(&self, b: &[u8]) -> Result<usize> {
            Ok(self.0.send(b).await?)
        }
        async fn send_to(&self, b: &[u8], a: SocketAddr) -> Result<usize> {
            Ok(self.0.send_to(b, a).await?)
        }
        fn local_addr(&self) -> Result<SocketAddr> {
            Ok(self.0.local_addr()?)
        }
        fn remote_addr(&self) -> Option<SocketAddr> {
            None
        }
        async fn close(&self) -> Result<()> {
            Ok(())
        }
        fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
            self
        }
    }
    #[tokio::test]
    async fn real_mux_reader_failure_keeps_os_error_before_ice_timeout() {
        let log = Diagnostics::new(None, true);
        let events = log.log.capture().unwrap();
        let socket = FailedReceive(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let (mux, worker) = UDPMuxDefault::new_with_worker(udp_params(socket, None, log.clone()));
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap();
        let event = events.try_recv().unwrap();
        assert_eq!(event["event"], "native_udp_error");
        assert_eq!(event["direction"], "receive");
        assert_eq!(event["os_error"], 10054);
        assert_eq!(event["closing"], false);
        assert_eq!(log.receive.pending.load(Relaxed), 0);
        mux.close().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn connected_peers_share_only_public_offer_nonce_and_keep_original_exit_before_cleanup() {
        use crate::native_media::NativePeer;
        let ip = "127.0.0.1".parse().unwrap();
        let mut viewer = NativePeer::new(ip, None, false).await.unwrap();
        let mut host = NativePeer::new(ip, None, true).await.unwrap();
        let viewer_events = viewer.diagnostics.log.capture().unwrap();
        let host_events = host.diagnostics.log.capture().unwrap();
        viewer
            .pc
            .create_data_channel("test-only", None)
            .await
            .unwrap();
        let offer = viewer.description(true).await.unwrap();
        let offer_id = offer.unmarshal().unwrap().origin.session_id;
        host.remote(offer, ip).await.unwrap();
        let answer = host.description(false).await.unwrap();
        viewer.remote(answer, ip).await.unwrap();
        let (a, b) = tokio::join!(viewer.connected(), host.connected());
        a.unwrap();
        b.unwrap();
        viewer.diagnostics.exit("test_transport_failure");
        viewer.diagnostics.exit("stop_requested");
        viewer.close().await;
        host.close().await;
        let a: Vec<_> = viewer_events.try_iter().collect();
        let b: Vec<_> = host_events.try_iter().collect();
        for events in [&a, &b] {
            assert!(
                events
                    .iter()
                    .any(|e| e["event"] == "native_transport_negotiation"
                        && e["offer_id"] == offer_id)
            );
            assert!(events
                .iter()
                .any(|e| e["event"] == "native_peer_state" && e["state"] == "connected"));
            assert!(events.iter().any(|e| e["event"] == "native_ice_state"));
            assert!(events
                .iter()
                .any(|e| e["event"] == "native_transport_closed"));
            let text = serde_json::to_string(events).unwrap();
            assert!(
                !text.contains("candidate:")
                    && !text.contains("ice-pwd")
                    && !text.contains("127.0.0.1")
            );
        }
        let exits: Vec<_> = a
            .iter()
            .filter(|e| e["event"] == "native_session_exit")
            .collect();
        assert_eq!(exits.len(), 1);
        assert_eq!(exits[0]["cause"], "test_transport_failure");
        assert_ne!(a[0]["peer_id"], b[0]["peer_id"]);
    }

    #[tokio::test]
    async fn full_log_queue_never_blocks_socket_errors_or_overwrites_first_exit() {
        let log = Diagnostics::new(None, false);
        let events = log.log.capture().unwrap();
        let socket = Observed {
            socket: FailedReceive(UdpSocket::bind("127.0.0.1:0").await.unwrap()),
            log: log.clone(),
        };
        let mut bytes = [0; 64];
        tokio::time::timeout(Duration::from_secs(2), async {
            for _ in 0..1000 {
                assert!(socket.recv_from(&mut bytes).await.is_err());
            }
            for _ in 0..1000 {
                log.record(json!({"event":"generated"}));
            }
        })
        .await
        .unwrap();
        assert_eq!(log.receive.errors.load(Relaxed), 1000);
        let events: Vec<_> = events.try_iter().collect();
        assert_eq!(events.len(), 64);
        assert_eq!(
            events
                .iter()
                .filter(|e| e["event"] == "native_udp_error")
                .count(),
            10
        );
    }
}
