//! Default-off, connection-owned LAN receive driver. Protocol tasks stay on the caller runtime.
use anyhow::{Context, Result};
use std::{
    net::{IpAddr, SocketAddr},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::{
    net::UdpSocket,
    sync::{oneshot, watch},
};
use webrtc::{
    ice::udp_mux::{UDPMux, UDPMuxDefault, UDPMuxParams},
    util::Conn,
};

pub(crate) fn requested(sender: bool) -> Result<bool> {
    enabled(
        cfg!(windows),
        sender,
        std::env::var("UU_NATIVE_DEDICATED_IO").ok().as_deref(),
    )
}
fn enabled(windows: bool, sender: bool, value: Option<&str>) -> Result<bool> {
    if !windows || sender {
        return Ok(false);
    }
    match value {
        None | Some("0") => Ok(false),
        Some("1") => Ok(true),
        _ => anyhow::bail!("UU_NATIVE_DEDICATED_IO must be 0 or 1"),
    }
}

fn event(instance: &str, action: &str, detail: serde_json::Value) {
    println!(
        "{}",
        serde_json::json!({"event":"native_dedicated_io","instance":instance,"action":action,"thread":format!("{:?}",std::thread::current().id()),"detail":detail})
    );
}
#[derive(Clone, Debug)]
pub(crate) struct Exit {
    pub error: Option<String>,
}
struct State {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<Exit>>,
}
/// Starting -> Ready -> Stopping -> joined (Stopped/Failed). Drop requests retirement;
/// the short-lived external reaper owns the actual join, never an async/UI thread.
pub(crate) struct Owner {
    instance: String,
    state: Mutex<State>,
    done_tx: watch::Sender<Option<Exit>>,
    done: watch::Receiver<Option<Exit>>,
}
impl Owner {
    pub async fn start(
        ip: IpAddr,
        observation: Option<Arc<crate::socket_trace::Observation>>,
    ) -> Result<(Arc<UDPMuxDefault>, Self)> {
        let (owner, ready) = Self::begin(ip, observation, || {})?;
        let mux = tokio::time::timeout(Duration::from_secs(3), ready)
            .await
            .context("dedicated IO startup timeout")?
            .context("dedicated IO startup ended before ready")?
            .map_err(anyhow::Error::msg)?;
        Ok((mux, owner))
    }
    fn begin(
        ip: IpAddr,
        observation: Option<Arc<crate::socket_trace::Observation>>,
        before_ready: impl FnOnce() + Send + 'static,
    ) -> Result<(
        Self,
        oneshot::Receiver<std::result::Result<Arc<UDPMuxDefault>, String>>,
    )> {
        let instance = uuid::Uuid::new_v4().to_string();
        let (stop, stopping) = oneshot::channel();
        let (ready_tx, ready) = oneshot::channel();
        let (done_tx, done) = watch::channel(None);
        let id = instance.clone();
        let thread=std::thread::Builder::new().name("native-udp-io".into()).spawn(move || {
            let started=Instant::now(); event(&id,"starting",serde_json::json!({}));
            let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                runtime.block_on(async {
                    let init=async {
                        let socket=UdpSocket::bind(SocketAddr::new(ip,0)).await?;
                        #[cfg(windows)]
                        super::native_media::configure_udp_probing(&socket)?;
                        let local=socket.local_addr()?;
                        let socket=FirstRead{socket,instance:id.clone(),first:AtomicBool::new(true)};
                        let params=match observation {
                            Some(trace)=>UDPMuxParams::new(crate::socket_trace::Observed{socket,trace}),
                            None=>UDPMuxParams::new(socket),
                        };
                        let (mux,worker)=UDPMuxDefault::new_with_worker(params);
                        Ok::<_,anyhow::Error>((mux,worker,local))
                    }.await;
                    let (mux,mut worker,local)=match init {
                        Ok(v)=>v,
                        Err(e)=>{let message=e.to_string();let _=ready_tx.send(Err(message));return Err(e);}
                    };
                    before_ready();
                    event(&id,"ready",serde_json::json!({"local":local,"runtime":"current_thread","requested":true,"actual":true,"create_thread":format!("{:?}",std::thread::current().id()),"bind_thread":format!("{:?}",std::thread::current().id())}));
                    if ready_tx.send(Ok(mux.clone())).is_ok() {let _=stopping.await;}
                    event(&id,"stopping",serde_json::json!({}));
                    let _=mux.close().await;
                    match tokio::time::timeout(Duration::from_secs(2),&mut worker).await {
                        Ok(result)=>{result.context("mux receive worker failed")?;event(&id,"worker_stopped",serde_json::json!({}));Ok(())},
                        Err(_)=>{worker.abort();let _=worker.await;anyhow::bail!("mux worker retirement exceeded two seconds (aborted)")}
                    }
                })
                // Runtime drops here on its own OS thread, after receive task retirement.
            }));
            let error=match result {Ok(Ok(()))=>None,Ok(Err(e))=>Some(e.to_string()),Err(_)=>Some("I/O thread panicked".into())};
            event(&id,"exited",serde_json::json!({"error":error,"elapsed_us":started.elapsed().as_micros() as u64}));Exit{error}
        })?;
        let owner = Self {
            instance,
            state: Mutex::new(State {
                stop: Some(stop),
                thread: Some(thread),
            }),
            done_tx,
            done,
        };
        Ok((owner, ready))
    }
    fn request_stop(&self) {
        let mut state = self.state.lock().unwrap();
        if let Some(stop) = state.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = state.thread.take() {
            let tx = self.done_tx.clone();
            let id = self.instance.clone();
            // The join handle moves into this reaper and remains owned even if the
            // close future is cancelled. No runtime or borrowed socket escapes it.
            std::thread::spawn(move || {
                let started = Instant::now();
                let exit = thread.join().unwrap_or(Exit {
                    error: Some("I/O thread join panic".into()),
                });
                event(
                    &id,
                    "joined",
                    serde_json::json!({"error":exit.error,"join_us":started.elapsed().as_micros() as u64}),
                );
                let _ = tx.send(Some(exit));
            });
        }
    }
    #[cfg(test)]
    pub(crate) fn join_observer(&self) -> watch::Receiver<Option<Exit>> {
        self.done.clone()
    }
    pub async fn stop(&self) -> Result<()> {
        self.request_stop();
        let mut done = self.done.clone();
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(exit) = done.borrow_and_update().clone() {
                    return Ok::<_, anyhow::Error>(exit);
                }
                done.changed().await.context("I/O join observer lost")?;
            }
        })
        .await;
        let exit = match result {
            Ok(r) => r?,
            Err(_) => {
                event(
                    &self.instance,
                    "cleanup_pending",
                    serde_json::json!({"reason":"join not yet confirmed"}),
                );
                anyhow::bail!("dedicated I/O join still pending");
            }
        };
        if let Some(e) = exit.error {
            anyhow::bail!(e);
        }
        Ok(())
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// One connection-level observation of the actual UDP API read, no packet queue.
struct FirstRead {
    socket: UdpSocket,
    instance: String,
    first: AtomicBool,
}
#[async_trait::async_trait]
impl Conn for FirstRead {
    async fn connect(&self, a: SocketAddr) -> webrtc::util::Result<()> {
        Ok(self.socket.connect(a).await?)
    }
    async fn recv(&self, b: &mut [u8]) -> webrtc::util::Result<usize> {
        Ok(self.socket.recv(b).await?)
    }
    async fn recv_from(&self, b: &mut [u8]) -> webrtc::util::Result<(usize, SocketAddr)> {
        let result = self.socket.recv_from(b).await;
        if result.is_ok() && self.first.swap(false, Ordering::Relaxed) {
            event(
                &self.instance,
                "first_recv",
                serde_json::json!({"local":self.socket.local_addr().ok()}),
            );
        }
        Ok(result?)
    }
    async fn send(&self, b: &[u8]) -> webrtc::util::Result<usize> {
        Ok(self.socket.send(b).await?)
    }
    async fn send_to(&self, b: &[u8], a: SocketAddr) -> webrtc::util::Result<usize> {
        Ok(self.socket.send_to(b, a).await?)
    }
    fn local_addr(&self) -> webrtc::util::Result<SocketAddr> {
        Conn::local_addr(&self.socket)
    }
    fn remote_addr(&self) -> Option<SocketAddr> {
        Conn::remote_addr(&self.socket)
    }
    async fn close(&self) -> webrtc::util::Result<()> {
        Conn::close(&self.socket).await
    }
    fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
        Conn::as_any(&self.socket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_is_default_off_strict_and_receive_windows_only() {
        assert!(!enabled(true, false, None).unwrap());
        assert!(!enabled(true, false, Some("0")).unwrap());
        assert!(enabled(true, false, Some("1")).unwrap());
        for v in ["true", "yes", "2", ""] {
            assert!(enabled(true, false, Some(v)).is_err());
        }
        assert!(!enabled(false, false, Some("1")).unwrap());
        assert!(!enabled(true, true, Some("1")).unwrap());
    }
    async fn joined(mut done: watch::Receiver<Option<Exit>>) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(exit) = done.borrow_and_update().clone() {
                    assert!(exit.error.is_none(), "{exit:?}");
                    break;
                }
                done.changed().await.unwrap();
            }
        })
        .await
        .expect("actual external join");
    }
    #[tokio::test]
    async fn two_drivers_preserve_addresses_and_retire_independently() {
        let ip = "127.0.0.1".parse().unwrap();
        let (a, owner_a) = Owner::start(ip, None).await.unwrap();
        let (b, owner_b) = Owner::start(ip, None).await.unwrap();
        let a = a.get_conn("a").await.unwrap();
        let b = b.get_conn("b").await.unwrap();
        let udp = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = udp.local_addr().unwrap();
        let mut data = [0; 64];
        for c in [&a, &b] {
            let local = c.local_addr().unwrap();
            assert_eq!(local.ip(), ip);
            c.send_to(b"out", addr).await.unwrap();
            let (n, from) = udp.recv_from(&mut data).await.unwrap();
            assert_eq!(&data[..n], b"out");
            assert_eq!(from, local);
            udp.send_to(b"in", local).await.unwrap();
            let (n, from) = tokio::time::timeout(Duration::from_secs(1), c.recv_from(&mut data))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(&data[..n], b"in");
            assert_eq!(from, addr);
        }
        owner_a.stop().await.unwrap();
        owner_a.stop().await.unwrap();
        b.send_to(b"still alive", addr).await.unwrap();
        let (n, _) = udp.recv_from(&mut data).await.unwrap();
        assert_eq!(&data[..n], b"still alive");
        owner_b.stop().await.unwrap();
    }
    #[tokio::test]
    async fn cancelled_start_and_lost_ready_receiver_are_joined() {
        for cancel in [false, true] {
            let (gate_tx, gate_rx) = std::sync::mpsc::channel();
            let (entered_tx, entered_rx) = oneshot::channel();
            let (owner, ready) = Owner::begin("127.0.0.1".parse().unwrap(), None, move || {
                let _ = entered_tx.send(());
                gate_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            })
            .unwrap();
            let done = owner.done.clone();
            entered_rx.await.unwrap();
            if cancel {
                let task = tokio::spawn(async move {
                    let _owner = owner;
                    let _ = ready.await;
                });
                tokio::task::yield_now().await;
                task.abort();
                let _ = task.await;
            } else {
                drop(ready);
                drop(owner);
            }
            gate_tx.send(()).unwrap();
            joined(done).await;
        }
    }
    #[tokio::test]
    async fn startup_bind_failure_is_joined_as_failure() {
        let (owner, ready) = Owner::begin("192.0.2.1".parse().unwrap(), None, || {}).unwrap();
        assert!(ready.await.unwrap().is_err());
        assert!(owner.stop().await.is_err());
        assert!(owner.done.borrow().is_some());
    }
}
