//! Default-off observation of the existing native UDP Conn. Headers here are
//! unauthenticated: only offline unique matches to primary reads are evidence.
use async_trait::async_trait;
use serde::Serialize;
use std::{
    collections::VecDeque,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};
use tokio::net::UdpSocket;
use webrtc::util::{conn::Conn, Result};
/// Bracket a common same-machine QPC reading with this trace's Instant clock.
/// Never compare anchors from different machines.
#[derive(Serialize)]
pub(crate) struct ClockAnchor {
    before_us: u64,
    after_us: u64,
    qpc_ticks: i64,
    qpc_frequency: i64,
}
pub(crate) fn clock_anchor(started: Instant) -> Option<ClockAnchor> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Performance::{
            QueryPerformanceCounter, QueryPerformanceFrequency,
        };
        let (mut ticks, mut frequency) = (0i64, 0i64);
        if unsafe { QueryPerformanceFrequency(&mut frequency) } == 0 || frequency <= 0 {
            return None;
        }
        let before = Instant::now();
        let ok = unsafe { QueryPerformanceCounter(&mut ticks) };
        let after = Instant::now();
        if ok == 0 {
            return None;
        }
        Some(ClockAnchor {
            before_us: before.saturating_duration_since(started).as_micros() as u64,
            after_us: after.saturating_duration_since(started).as_micros() as u64,
            qpc_ticks: ticks,
            qpc_frequency: frequency,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = started;
        None
    }
}
const LIMIT: usize = 524_288;
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, Hash)]
pub(crate) struct Header {
    pub ssrc: u32,
    pub rtp: u32,
    pub seq: u16,
    pub pt: u8,
    pub marker: bool,
}
fn header(b: &[u8]) -> (u8, Option<Header>) {
    // RFC 7983 demux; RFC 5761 excludes the full RTCP type range before RTP.
    if b.len() >= 12 && b[0] >> 6 == 2 && !(192..=223).contains(&b[1]) {
        let minimum = 12 + 4 * usize::from(b[0] & 15);
        if b.len() >= minimum {
            return (
                1,
                Some(Header {
                    ssrc: u32::from_be_bytes(b[8..12].try_into().unwrap()),
                    rtp: u32::from_be_bytes(b[4..8].try_into().unwrap()),
                    seq: u16::from_be_bytes(b[2..4].try_into().unwrap()),
                    pt: b[1] & 127,
                    marker: b[1] & 128 != 0,
                }),
            );
        }
    }
    (
        match b.first() {
            Some(0..=3) => 2,
            Some(20..=63) => 3,
            Some(128..=191) => 4,
            _ => 0,
        },
        None,
    )
}
#[derive(Clone, Copy, Serialize)]
struct Wire {
    direction: u8,
    method: u8,
    outcome: u8,
    kind: u8,
    len: u32,
    start_us: u64,
    return_us: u64,
    previous_return_us: u64,
    header: Option<Header>,
}
#[derive(Clone, Copy, Serialize)]
struct Primary {
    epoch: u64,
    track: u32,
    header: Header,
    read_us: u64,
    pipeline_us: u64,
}
#[derive(Default)]
struct State {
    wire: VecDeque<Wire>,
    primary: VecDeque<Primary>,
    wire_total: u64,
    primary_total: u64,
    wire_evicted: u64,
    primary_evicted: u64,
}
pub(crate) struct Observation {
    started: Instant,
    instance: String,
    path: PathBuf,
    state: Mutex<State>,
    last_recv: AtomicU64,
    active: AtomicU64,
    cancelled: AtomicU64,
    failed: AtomicU64,
    dumped: AtomicBool,
}
impl Observation {
    pub fn configured() -> anyhow::Result<Option<Arc<Self>>> {
        if std::env::var("ECLIPSE_SOCKET_TRACE").as_deref() != Ok("1") {
            return Ok(None);
        }
        let path = std::env::var_os("ECLIPSE_SOCKET_TRACE_FILE")
            .ok_or_else(|| anyhow::anyhow!("socket trace needs an owned output file"))?;
        Ok(Some(Arc::new(Self::new(path.into()))))
    }
    pub(crate) fn new(path: PathBuf) -> Self {
        let instance = uuid::Uuid::new_v4().to_string();
        let path = PathBuf::from(path.to_string_lossy().replace("{instance}", &instance));
        Self {
            instance,
            started: Instant::now(),
            path,
            state: Mutex::new(State {
                wire: VecDeque::with_capacity(LIMIT),
                primary: VecDeque::with_capacity(LIMIT),
                ..Default::default()
            }),
            last_recv: AtomicU64::new(0),
            active: AtomicU64::new(0),
            cancelled: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            dumped: AtomicBool::new(false),
        }
    }
    fn offset(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.started)
            .as_micros()
            .min(u64::MAX as u128) as u64
    }
    fn call(&self, direction: u8, method: u8) -> Call<'_> {
        self.active.fetch_add(1, Ordering::Relaxed);
        Call {
            trace: self,
            direction,
            method,
            start: self.offset(Instant::now()),
            previous: if direction == 0 {
                self.last_recv.load(Ordering::Relaxed)
            } else {
                0
            },
            done: false,
        }
    }
    fn wire(&self, row: Wire) {
        let mut s = self.state.lock().unwrap();
        if self.dumped.load(Ordering::Acquire) { return; }
        s.wire_total += 1;
        if s.wire.len() == LIMIT {
            s.wire.pop_front();
            s.wire_evicted += 1;
        }
        s.wire.push_back(row);
    }
    pub fn primary(
        &self,
        epoch: u64,
        track: u32,
        h: &webrtc::rtp::header::Header,
        at: Instant,
        pipeline_us: u64,
    ) {
        let row = Primary {
            epoch,
            track,
            header: Header {
                ssrc: h.ssrc,
                rtp: h.timestamp,
                seq: h.sequence_number,
                pt: h.payload_type,
                marker: h.marker,
            },
            read_us: self.offset(at),
            pipeline_us,
        };
        let mut s = self.state.lock().unwrap();
        if self.dumped.load(Ordering::Acquire) { return; }
        s.primary_total += 1;
        if s.primary.len() == LIMIT {
            s.primary.pop_front();
            s.primary_evicted += 1;
        }
        s.primary.push_back(row);
    }
    pub async fn finish(self: &Arc<Self>) {
        // The mux owns its reader; do not create a competing receive task. Its
        // existing close signal cancels the pending Conn read.
        for _ in 0..100 {
            if self.active.load(Ordering::Acquire) == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        let this = self.clone();
        let _ = tokio::task::spawn_blocking(move || this.dump()).await;
    }
    fn dump(&self) {
        if self.dumped.swap(true, Ordering::AcqRel) {
            return;
        }
        let s = std::mem::take(&mut *self.state.lock().unwrap());
        #[derive(Serialize)]
        struct Report<'a> {
            qpc_anchor: Option<ClockAnchor>,
            version: u8,
            instance: String,
            clock: &'static str,
            unauthenticated_wire: bool,
            limit: usize,
            max_record_bytes: usize,
            wire_total: u64,
            primary_total: u64,
            wire_evicted: u64,
            primary_evicted: u64,
            cancelled: u64,
            failed: u64,
            active_at_dump: u64,
            wire: &'a VecDeque<Wire>,
            primary: &'a VecDeque<Primary>,
        }
        let report = Report {
            qpc_anchor: clock_anchor(self.started),
            version: 1,
            instance: self.instance.clone(),
            clock: "connection_monotonic_us",
            unauthenticated_wire: true,
            limit: LIMIT,
            max_record_bytes: LIMIT
                * (std::mem::size_of::<Wire>() + std::mem::size_of::<Primary>()),
            wire_total: s.wire_total,
            primary_total: s.primary_total,
            wire_evicted: s.wire_evicted,
            primary_evicted: s.primary_evicted,
            cancelled: self.cancelled.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            active_at_dump: self.active.load(Ordering::Acquire),
            wire: &s.wire,
            primary: &s.primary,
        };
        // Streaming serialization once, after moving records outside the lock.
        let result = (|| -> anyhow::Result<()> {
            use std::io::Write;
            let mut out = std::io::BufWriter::new(std::fs::File::create(&self.path)?);
            serde_json::to_writer(&mut out, &report)?;
            out.flush()?;
            Ok(())
        })();
        eprintln!(
            "{}",
            serde_json::json!({"event":"native_socket_trace","path":self.path,"written":result.is_ok(),"error":result.err().map(|e|e.to_string()),"wire_total":s.wire_total,"primary_total":s.primary_total,"active_at_dump":report.active_at_dump})
        );
    }
}
struct Call<'a> {
    trace: &'a Observation,
    direction: u8,
    method: u8,
    start: u64,
    previous: u64,
    done: bool,
}
impl Call<'_> {
    fn finish(mut self, bytes: Option<&[u8]>) {
        let end = self.trace.offset(Instant::now());
        let (kind, h) = bytes.map(header).unwrap_or((0, None));
        if bytes.is_none() {
            self.trace.failed.fetch_add(1, Ordering::Relaxed);
        } else if self.direction == 0 {
            self.trace.last_recv.store(end, Ordering::Relaxed);
        }
        self.trace.wire(Wire {
            direction: self.direction,
            method: self.method,
            outcome: u8::from(bytes.is_none()),
            kind,
            len: bytes.map_or(0, |b| b.len() as u32),
            start_us: self.start,
            return_us: end,
            previous_return_us: self.previous,
            header: h,
        });
        self.done = true;
    }
}
impl Drop for Call<'_> {
    fn drop(&mut self) {
        if !self.done {
            self.trace.cancelled.fetch_add(1, Ordering::Relaxed);
            self.trace.wire(Wire {
                direction: self.direction,
                method: self.method,
                outcome: 2,
                kind: 0,
                len: 0,
                start_us: self.start,
                return_us: self.trace.offset(Instant::now()),
                previous_return_us: self.previous,
                header: None,
            });
        }
        self.trace.active.fetch_sub(1, Ordering::Release);
    }
}
pub(crate) struct Observed<C = UdpSocket> {
    pub socket: C,
    pub trace: Arc<Observation>,
}
#[async_trait]
impl<C: Conn + Send + Sync> Conn for Observed<C> {
    async fn connect(&self, addr: SocketAddr) -> Result<()> {
        Conn::connect(&self.socket, addr).await
    }
    async fn recv(&self, b: &mut [u8]) -> Result<usize> {
        let c = self.trace.call(0, 0);
        let r = Conn::recv(&self.socket, b).await;
        c.finish(r.as_ref().ok().map(|n| &b[..*n]));
        r
    }
    async fn recv_from(&self, b: &mut [u8]) -> Result<(usize, SocketAddr)> {
        let c = self.trace.call(0, 1);
        let r = Conn::recv_from(&self.socket, b).await;
        c.finish(r.as_ref().ok().map(|(n, _)| &b[..*n]));
        r
    }
    async fn send(&self, b: &[u8]) -> Result<usize> {
        let c = self.trace.call(1, 2);
        let r = Conn::send(&self.socket, b).await;
        c.finish(r.as_ref().ok().map(|n| &b[..*n]));
        r
    }
    async fn send_to(&self, b: &[u8], addr: SocketAddr) -> Result<usize> {
        let c = self.trace.call(1, 3);
        let r = Conn::send_to(&self.socket, b, addr).await;
        c.finish(r.as_ref().ok().map(|n| &b[..*n]));
        r
    }
    fn local_addr(&self) -> Result<SocketAddr> {
        Conn::local_addr(&self.socket)
    }
    fn remote_addr(&self) -> Option<SocketAddr> {
        Conn::remote_addr(&self.socket)
    }
    async fn close(&self) -> Result<()> {
        Conn::close(&self.socket).await
    }
    fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
        Conn::as_any(&self.socket)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn forwards_bytes_addresses_errors_cancel_and_close() {
        assert!(
            LIMIT * (std::mem::size_of::<Wire>() + std::mem::size_of::<Primary>())
                < 96 * 1024 * 1024
        );
        let t = Arc::new(Observation::new(PathBuf::new()));
        let a = Observed {
            socket: UdpSocket::bind("127.0.0.1:0").await.unwrap(),
            trace: t.clone(),
        };
        let b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut buf = [0; 64];
        assert!(Conn::send(&a, b"unconnected").await.is_err());
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(5),
            Conn::recv_from(&a, &mut buf)
        )
        .await
        .is_err());
        Conn::send_to(&a, b"abc", b.local_addr().unwrap())
            .await
            .unwrap();
        let (n, src) = b.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"abc");
        assert_eq!(src, Conn::local_addr(&a).unwrap());
        b.send_to(b"xyz", src).await.unwrap();
        let (n, src2) = Conn::recv_from(&a, &mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"xyz");
        assert_eq!(src2, b.local_addr().unwrap());
        Conn::connect(&a, src2).await.unwrap();
        Conn::send(&a, b"connected").await.unwrap();
        b.recv_from(&mut buf).await.unwrap();
        b.send_to(b"ok", src).await.unwrap();
        assert_eq!(Conn::recv(&a, &mut buf).await.unwrap(), 2);
        assert!(Conn::remote_addr(&a).is_none());
        assert!(a.as_any().is::<UdpSocket>());
        Conn::close(&a).await.unwrap(); // Original UDP close is a no-op.
        Conn::send(&a, b"after close").await.unwrap();
        assert_eq!(t.cancelled.load(Ordering::Relaxed), 1);
        assert_eq!(t.failed.load(Ordering::Relaxed), 1);
        assert_eq!(t.active.load(Ordering::Relaxed), 0);
    }
    #[tokio::test]
    async fn dump_seals_late_primary_and_cancelled_socket_calls() {
        let path=std::env::temp_dir().join(format!("uu-trace-seal-{}.json",uuid::Uuid::new_v4()));
        let trace=Arc::new(Observation::new(path.clone()));
        let late=trace.call(0,1);
        trace.dump();
        let bytes=std::fs::read(&path).unwrap();
        drop(late);
        trace.primary(1,0,&Default::default(),Instant::now(),0);
        trace.call(0,1).finish(Some(&[1,2,3]));
        trace.finish().await;
        assert_eq!(std::fs::read(&path).unwrap(),bytes);
        let state=trace.state.lock().unwrap();
        assert!(state.wire.is_empty()&&state.primary.is_empty());
        assert_eq!(state.wire_total+state.primary_total,0);
        assert_eq!(trace.active.load(Ordering::Acquire),0);
        drop(state);std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn classification_and_fixed_ring() {
        assert!(header(&[128, 200, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0])
            .1
            .is_none());
        let mut p = [0u8; 12];
        p[0] = 128;
        p[1] = 234;
        p[2..4].copy_from_slice(&u16::MAX.to_be_bytes());
        let h = header(&p).1.unwrap();
        assert_eq!(h.seq, u16::MAX);
        assert!(h.marker);
        assert_eq!(h.pt, 106);
        let t = Observation::new(PathBuf::new());
        for _ in 0..LIMIT + 2 {
            t.wire(Wire {
                direction: 0,
                method: 1,
                outcome: 0,
                kind: 0,
                len: 0,
                start_us: 0,
                return_us: 1,
                previous_return_us: 0,
                header: None,
            });
        }
        let s = t.state.lock().unwrap();
        assert_eq!(s.wire.len(), LIMIT);
        assert_eq!(s.wire_evicted, 2);
    }
}
