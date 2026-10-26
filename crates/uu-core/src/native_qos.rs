//! Default-off Windows LAN sender experiment. One owned socket/handle/flow.
//! Applies to the whole UDP destination flow (including audio/control/repairs).
use serde::Serialize;
use std::{net::SocketAddrV4, sync::Arc, time::Instant};
use tokio::net::UdpSocket;
#[cfg(windows)]
mod windows;

pub(crate) fn requested(sender: bool) -> bool {
    enabled(
        cfg!(windows),
        sender,
        std::env::var("UU_NATIVE_QOS").ok().as_deref(),
    )
}
fn enabled(windows: bool, sender: bool, value: Option<&str>) -> bool {
    windows && sender && value == Some("1")
}
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[repr(C)]
pub struct Priority {
    pub conformant_dscp: u32,
    pub nonconformant_dscp: u32,
    pub conformant_l2: u32,
    pub nonconformant_l2: u32,
}
#[derive(Debug, Clone, Serialize)]
pub struct Error {
    pub api: &'static str,
    pub code: u32,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} failed ({})", self.api, self.code)
    }
}
impl std::error::Error for Error {}
trait Api: Send + Sync {
    fn open(&self) -> Result<usize, Error>;
    fn add(&self, handle: usize, socket: usize, dest: SocketAddrV4) -> Result<u32, Error>;
    fn query(&self, handle: usize, flow: u32) -> Result<Priority, Error>;
    fn remove(&self, handle: usize, socket: usize, flow: u32) -> Result<(), Error>;
    fn close(&self, handle: usize) -> Result<(), Error>;
}
/// Explicit lifecycle object also used by the isolated Windows capability probe.
/// Callers must supply their own socket and a validated destination. NativePeer
/// obtains that destination from authenticated, selected ICE, never packet data.
pub struct Guard {
    api: Arc<dyn Api>,
    socket: Arc<UdpSocket>,
    handle: Option<usize>,
    flow: Option<u32>,
    instance: String,
    destination: SocketAddrV4,
    started: Instant,
}
impl Guard {
    #[cfg(windows)]
    pub fn acquire(socket: Arc<UdpSocket>, destination: SocketAddrV4) -> Result<Self, Error> {
        Self::load_with(socket, destination, windows::load)
    }
    fn load_with(
        socket: Arc<UdpSocket>,
        destination: SocketAddrV4,
        load: impl FnOnce() -> Result<Arc<dyn Api>, Error>,
    ) -> Result<Self, Error> {
        let api = load().map_err(|e| {
            event(serde_json::json!({"action":"error","error":e}));
            e
        })?;
        Self::with_api(api, socket, destination)
    }
    fn with_api(
        api: Arc<dyn Api>,
        socket: Arc<UdpSocket>,
        destination: SocketAddrV4,
    ) -> Result<Self, Error> {
        let mut guard = Self {
            api,
            socket,
            handle: None,
            flow: None,
            instance: uuid::Uuid::new_v4().to_string(),
            destination,
            started: Instant::now(),
        };
        let result = (|| {
            guard.handle = Some(guard.api.open()?);
            guard.flow = Some(guard.api.add(
                guard.handle.unwrap(),
                raw(&guard.socket),
                destination,
            )?);
            if guard.flow == Some(0) {
                return Err(Error {
                    api: "QOSAddSocketToFlow.zero_flow",
                    code: 13,
                });
            }
            guard.query()
        })();
        match result {
            Ok(priority) => {
                event(
                    serde_json::json!({"action":"ready","instance":guard.instance,"local":guard.socket.local_addr().ok(),"destination":guard.destination,"traffic_type":3,"non_adaptive":true,"flow":guard.flow,"priority":priority,"ready_us":guard.started.elapsed().as_micros() as u64}),
                );
                Ok(guard)
            }
            Err(error) => {
                event(
                    serde_json::json!({"action":"error","instance":guard.instance,"flow":guard.flow,"error":error}),
                );
                Err(error)
            }
        }
    }
    pub fn query(&self) -> Result<Priority, Error> {
        let handle = self.handle.ok_or(Error {
            api: "closed",
            code: 6,
        })?;
        self.api.query(
            handle,
            self.flow.ok_or(Error {
                api: "missing_flow",
                code: 6,
            })?,
        )
    }
    /// Idempotent, per-handle cleanup. Close still runs when Remove fails.
    pub fn close(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        let flow = self.flow.take();
        let removed = flow.map(|flow| self.api.remove(handle, raw(&self.socket), flow));
        let closed = self.api.close(handle);
        event(
            serde_json::json!({"action":"close","instance":self.instance,"flow":flow,"remove_error":removed.and_then(Result::err),"close_error":closed.err(),"elapsed_us":self.started.elapsed().as_micros() as u64}),
        );
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.close();
    }
}
fn raw(socket: &UdpSocket) -> usize {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        socket.as_raw_socket() as usize
    }
    #[cfg(not(windows))]
    {
        use std::os::fd::AsRawFd;
        socket.as_raw_fd() as usize
    }
}
pub(crate) fn event(mut value: serde_json::Value) {
    value["event"] = "native_qos".into();
    eprintln!("{value}");
}

/// Only enabled QoS connections need shared ownership. Disabled connections
/// continue to pass the original UdpSocket directly into UDPMuxParams.
#[cfg(any(windows, test))]
pub(crate) struct SharedSocket(pub Arc<UdpSocket>);
#[cfg(any(windows, test))]
#[async_trait::async_trait]
impl webrtc::util::conn::Conn for SharedSocket {
    async fn connect(&self, a: std::net::SocketAddr) -> webrtc::util::Result<()> {
        Ok(self.0.connect(a).await?)
    }
    async fn recv(&self, b: &mut [u8]) -> webrtc::util::Result<usize> {
        Ok(self.0.recv(b).await?)
    }
    async fn recv_from(&self, b: &mut [u8]) -> webrtc::util::Result<(usize, std::net::SocketAddr)> {
        Ok(self.0.recv_from(b).await?)
    }
    async fn send(&self, b: &[u8]) -> webrtc::util::Result<usize> {
        Ok(self.0.send(b).await?)
    }
    async fn send_to(&self, b: &[u8], a: std::net::SocketAddr) -> webrtc::util::Result<usize> {
        Ok(self.0.send_to(b, a).await?)
    }
    fn local_addr(&self) -> webrtc::util::Result<std::net::SocketAddr> {
        webrtc::util::conn::Conn::local_addr(&*self.0)
    }
    fn remote_addr(&self) -> Option<std::net::SocketAddr> {
        webrtc::util::conn::Conn::remote_addr(&*self.0)
    }
    async fn close(&self) -> webrtc::util::Result<()> {
        webrtc::util::conn::Conn::close(&*self.0).await
    }
    fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
        webrtc::util::conn::Conn::as_any(&*self.0)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    };
    #[derive(Default)]
    struct Mock {
        next: AtomicUsize,
        live: Mutex<Vec<usize>>,
        calls: Mutex<Vec<String>>,
        fail: Option<&'static str>,
    }
    impl Mock {
        fn call(&self, name: &'static str, h: usize) -> Result<(), Error> {
            self.calls.lock().unwrap().push(format!("{name}:{h}"));
            if self.fail == Some(name) {
                Err(Error { api: name, code: 5 })
            } else {
                Ok(())
            }
        }
    }
    impl Api for Mock {
        fn open(&self) -> Result<usize, Error> {
            let h = self.next.fetch_add(1, Ordering::Relaxed) + 1;
            self.call("open", h)?;
            self.live.lock().unwrap().push(h);
            Ok(h)
        }
        fn add(&self, h: usize, _: usize, _: SocketAddrV4) -> Result<u32, Error> {
            self.call("add", h)?;
            Ok(h as u32)
        }
        fn query(&self, h: usize, _: u32) -> Result<Priority, Error> {
            assert!(self.live.lock().unwrap().contains(&h));
            self.call("query", h)?;
            Ok(Priority::default())
        }
        fn remove(&self, h: usize, _: usize, _: u32) -> Result<(), Error> {
            self.call("remove", h)
        }
        fn close(&self, h: usize) -> Result<(), Error> {
            self.live.lock().unwrap().retain(|v| *v != h);
            self.call("close", h)
        }
    }
    async fn socket() -> Arc<UdpSocket> {
        Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap())
    }
    fn dest() -> SocketAddrV4 {
        "127.0.0.1:1234".parse().unwrap()
    }
    #[tokio::test]
    async fn partial_failures_and_independent_idempotent_close() {
        for fail in ["open", "add", "query"] {
            let api = Arc::new(Mock {
                fail: Some(fail),
                ..Default::default()
            });
            assert!(Guard::with_api(api.clone(), socket().await, dest()).is_err());
            assert!(api.live.lock().unwrap().is_empty());
            assert_eq!(
                api.calls
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|v| v.starts_with("close:"))
                    .count(),
                usize::from(fail != "open")
            );
        }
        let api = Arc::new(Mock::default());
        let mut a = Guard::with_api(api.clone(), socket().await, dest()).unwrap();
        let b = Guard::with_api(api.clone(), socket().await, dest()).unwrap();
        a.close();
        a.close();
        b.query().unwrap();
        assert_eq!(api.live.lock().unwrap().len(), 1);
        drop(b);
        assert!(api.live.lock().unwrap().is_empty());
        assert_eq!(
            api.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|v| v.starts_with("close:"))
                .count(),
            2
        );
        for fail in ["remove", "close"] {
            let api = Arc::new(Mock {
                fail: Some(fail),
                ..Default::default()
            });
            drop(Guard::with_api(api.clone(), socket().await, dest()).unwrap());
            assert!(api.live.lock().unwrap().is_empty());
        }
    }
    #[tokio::test]
    async fn cancelled_creation_keeps_socket_owned_and_drops_late_guard() {
        let api = Arc::new(Mock::default());
        let owned = socket().await;
        let weak = Arc::downgrade(&owned);
        let (entered, ready) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let cloned = api.clone();
        let task = tokio::task::spawn_blocking(move || {
            entered.send(()).unwrap();
            wait.recv().unwrap();
            Guard::with_api(cloned, owned, dest()).unwrap()
        });
        ready.recv().unwrap();
        task.abort();
        drop(task);
        assert!(weak.upgrade().is_some());
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while weak.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(api.live.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn shared_socket_preserves_original_udp_and_ownership() {
        use webrtc::util::conn::Conn;
        let owned = socket().await;
        let weak = Arc::downgrade(&owned);
        let wrapped = SharedSocket(owned);
        let other = socket().await;
        let address = Conn::local_addr(&wrapped).unwrap();
        let mut buf = [0; 64];
        assert!(Conn::send(&wrapped, b"unconnected").await.is_err());
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(5),
            Conn::recv_from(&wrapped, &mut buf)
        )
        .await
        .is_err());
        Conn::send_to(&wrapped, b"one", other.local_addr().unwrap())
            .await
            .unwrap();
        let (n, from) = other.recv_from(&mut buf).await.unwrap();
        assert_eq!(from, address);
        assert_eq!(&buf[..n], b"one");
        other.send_to(b"two", address).await.unwrap();
        let (n, from) = Conn::recv_from(&wrapped, &mut buf).await.unwrap();
        assert_eq!(from, other.local_addr().unwrap());
        assert_eq!(&buf[..n], b"two");
        Conn::connect(&wrapped, other.local_addr().unwrap())
            .await
            .unwrap();
        Conn::send(&wrapped, b"three").await.unwrap();
        other.recv_from(&mut buf).await.unwrap();
        other.send_to(b"four", address).await.unwrap();
        assert_eq!(Conn::recv(&wrapped, &mut buf).await.unwrap(), 4);
        assert!(Conn::remote_addr(&wrapped).is_none());
        assert!(Conn::as_any(&wrapped).is::<UdpSocket>());
        Conn::close(&wrapped).await.unwrap();
        assert!(weak.upgrade().is_some());
        drop(wrapped);
        assert!(weak.upgrade().is_none());
    }
    #[test]
    fn default_off_policy() {
        for value in [None, Some("0"), Some("true"), Some("2"), Some("1 ")] {
            assert!(!enabled(true, true, value));
        }
        assert!(enabled(true, true, Some("1")));
        assert!(!enabled(true, false, Some("1")));
        assert!(!enabled(false, true, Some("1")));
        assert!(!requested(false));
        assert_eq!(std::mem::size_of::<Priority>(), 16);
    }
    #[tokio::test]
    async fn library_or_export_failure_releases_socket_without_opening() {
        for name in [
            "LoadLibraryExW.qwave.System32",
            "GetProcAddress.QOSQueryFlow",
        ] {
            let socket = socket().await;
            let weak = Arc::downgrade(&socket);
            let result = Guard::load_with(socket, dest(), || {
                Err(Error {
                    api: name,
                    code: 127,
                })
            });
            assert!(result.is_err());
            assert!(weak.upgrade().is_none());
        }
    }
}
