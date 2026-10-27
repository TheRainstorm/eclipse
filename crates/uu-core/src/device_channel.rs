//! A data-only bootstrap peer. No decoder, capture, input or media tracks are
//! created before device authentication. Media retains its existing pipeline.
use crate::{
    device_auth::{AuthIo, MAX_AUTH_MESSAGE},
    direct_ice::InternetIce,
    internet::{Connection, Transport},
    native_media::NativePeer,
    native_signaling::SessionSignaling,
};
use anyhow::{ensure, Context, Result};
use bytes::Bytes;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    ops::{Deref, DerefMut},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};
use webrtc::{
    data_channel::{data_channel_state::RTCDataChannelState, RTCDataChannel},
    peer_connection::{
        peer_connection_state::RTCPeerConnectionState,
        sdp::session_description::RTCSessionDescription,
    },
};

const LABEL: &str = "eclipse-device-auth-v2";
const MAX_SIGNAL: usize = 256 * 1024;

pub struct DataIo {
    dc: Arc<RTCDataChannel>,
    messages: mpsc::Receiver<Value>,
    state: watch::Receiver<bool>,
    authenticated: Arc<AtomicBool>,
}
impl DataIo {
    fn attach(dc: Arc<RTCDataChannel>) -> Self {
        let (sender, messages) = mpsc::channel(8);
        let (closed, state) = watch::channel(false);
        let authenticated = Arc::new(AtomicBool::new(false));
        let allowed = authenticated.clone();
        let weak = Arc::downgrade(&dc);
        let invalid = closed.clone();
        dc.on_message(Box::new(move |message| {
            let max = if allowed.load(Ordering::Acquire) {
                MAX_SIGNAL
            } else {
                MAX_AUTH_MESSAGE
            };
            let parsed = if message.data.len() <= max {
                serde_json::from_slice::<Value>(&message.data).ok()
            } else {
                None
            };
            let valid = parsed.is_some_and(|value| sender.try_send(value).is_ok());
            let weak = weak.clone();
            let invalid = invalid.clone();
            Box::pin(async move {
                if !valid {
                    invalid.send_replace(true);
                    if let Some(dc) = weak.upgrade() {
                        let _ = dc.close().await;
                    }
                }
            })
        }));
        dc.on_close(Box::new(move || {
            closed.send_replace(true);
            Box::pin(async {})
        }));
        Self {
            dc,
            messages,
            state,
            authenticated,
        }
    }
    async fn opened(&self) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                match self.dc.ready_state() {
                    RTCDataChannelState::Open => return Ok(()),
                    RTCDataChannelState::Closed | RTCDataChannelState::Closing => {
                        anyhow::bail!("{}", uu_i18n::format!("设备配对通道已关闭"))
                    }
                    _ => tokio::time::sleep(Duration::from_millis(5)).await,
                }
            }
        })
        .await
        .context(uu_i18n::text("建立设备配对通道超时"))?
    }
}
#[async_trait::async_trait]
impl AuthIo for DataIo {
    async fn send(&mut self, value: Value) -> Result<()> {
        let bytes = serde_json::to_vec(&value)?;
        let max = if self.authenticated.load(Ordering::Acquire) {
            MAX_SIGNAL
        } else {
            MAX_AUTH_MESSAGE
        };
        ensure!(
            bytes.len() <= max && self.dc.buffered_amount().await <= MAX_SIGNAL,
            "{}", uu_i18n::format!("设备消息超出通道限制"));
        self.dc.send(&Bytes::from(bytes)).await?;
        Ok(())
    }
    async fn receive(&mut self) -> Result<Value> {
        ensure!(!*self.state.borrow(), "{}", uu_i18n::format!("设备配对通道已关闭"));
        tokio::select! {
            result=self.messages.recv()=>result.context("设备消息通道已关闭"),
            _=self.state.changed()=>anyhow::bail!("设备配对通道已关闭"),
        }
    }
}

struct Peer(NativePeer);
impl Deref for Peer {
    type Target = NativePeer;
    fn deref(&self) -> &NativePeer {
        &self.0
    }
}
impl DerefMut for Peer {
    fn deref_mut(&mut self) -> &mut NativePeer {
        &mut self.0
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        if self.pc.connection_state() != RTCPeerConnectionState::Closed {
            let pc = self.pc.clone();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = pc.close().await;
                });
            }
        }
    }
}

pub struct Bootstrap {
    peer: Peer,
    pub io: DataIo,
    pub binding: Vec<u8>,
    provider: Provider,
}
struct Provider {
    stop: watch::Sender<bool>,
    closed: watch::Receiver<bool>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl Provider {
    fn start(
        mut transport: Box<dyn Transport>,
        pc: &Arc<webrtc::peer_connection::RTCPeerConnection>,
    ) -> Self {
        let (stop, mut stopping) = watch::channel(false);
        let (ended, closed) = watch::channel(false);
        let peer = Arc::downgrade(pc);
        let task = tokio::spawn(async move {
            // recv drives the provider's heartbeats even while a human is
            // entering the pairing code. Further SDP uses the authenticated DC.
            loop {
                tokio::select! {
                    _=stopping.changed()=>break,
                    result=transport.receive()=>if result.is_err(){break;},
                }
            }
            ended.send_replace(true);
            if let Some(pc) = peer.upgrade() {
                let _ = pc.close().await;
            }
            let _ = tokio::time::timeout(Duration::from_secs(2), transport.close()).await;
        });
        Self {
            stop,
            closed,
            task: Some(task),
        }
    }
    async fn close(&mut self) {
        self.stop.send_replace(true);
        if let Some(mut task) = self.task.take() {
            if tokio::time::timeout(Duration::from_secs(3), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}
fn envelope(sdp: RTCSessionDescription) -> Value {
    json!({"v":2,"kind":"device_bootstrap","payload":{"sdp":sdp}})
}
pub fn is_bootstrap(value: &Value) -> bool {
    value["v"] == 2 && value["kind"] == "device_bootstrap"
}
fn description(value: &Value) -> Result<RTCSessionDescription> {
    ensure!(
        is_bootstrap(value),
        "{}", uu_i18n::format!("对端尚不支持设备公钥配对，请升级两端或使用已有配对入口"));
    serde_json::from_value(value["payload"]["sdp"].clone()).context(uu_i18n::text("设备引导 SDP 无效"))
}
fn binding(session: &str, offer: &Value, answer: &Value) -> Result<Vec<u8>> {
    Ok(Sha256::digest(serde_json::to_vec(
        &json!({"domain":"eclipse-bootstrap-v2","session":session,"offer":offer,"answer":answer}),
    )?)
    .to_vec())
}
impl Bootstrap {
    pub async fn viewer(mut connection: Connection) -> Result<Self> {
        let mut peer = Peer(NativePeer::new_direct(&connection.ice, None, false).await?);
        let dc = peer.pc.create_data_channel(LABEL, None).await?;
        let io = DataIo::attach(dc);
        let offer = envelope(peer.description(true).await?);
        connection.transport.send(offer.clone()).await?;
        let answer = tokio::time::timeout(Duration::from_secs(30), connection.transport.receive())
            .await
            .context(uu_i18n::text("等待主机配对应答超时"))??;
        peer.remote_direct(description(&answer)?).await?;
        peer.connected().await?;
        peer.direct_path().await?;
        io.opened().await?;
        let bound = binding(&connection.session, &offer, &answer)?;
        let provider = Provider::start(connection.transport, &peer.pc);
        Ok(Self {
            peer,
            io,
            binding: bound,
            provider,
        })
    }
    pub async fn host(mut connection: Connection, offer: Value) -> Result<Self> {
        let mut peer = Peer(NativePeer::new_direct(&connection.ice, None, true).await?);
        let (send, receive) = oneshot::channel();
        let send = Arc::new(std::sync::Mutex::new(Some(send)));
        peer.pc.on_data_channel(Box::new(move |dc| {
            let send = send.clone();
            Box::pin(async move {
                if dc.label() != LABEL {
                    let _ = dc.close().await;
                    return;
                }
                let io = DataIo::attach(dc);
                if let Some(send) = send.lock().unwrap().take() {
                    let _ = send.send(io);
                }
            })
        }));
        peer.remote_direct(description(&offer)?).await?;
        let answer = envelope(peer.description(false).await?);
        connection.transport.send(answer.clone()).await?;
        peer.connected().await?;
        peer.direct_path().await?;
        let io = tokio::time::timeout(Duration::from_secs(15), receive)
            .await
            .context(uu_i18n::text("等待设备配对通道超时"))??;
        io.opened().await?;
        let bound = binding(&connection.session, &offer, &answer)?;
        let provider = Provider::start(connection.transport, &peer.pc);
        Ok(Self {
            peer,
            io,
            binding: bound,
            provider,
        })
    }
    /// A barrier prevents an early large SDP from racing the host's transition
    /// out of the strict authentication message limit.
    pub async fn ready(&mut self, host: bool) -> Result<()> {
        if host {
            self.io.authenticated.store(true, Ordering::Release);
            AuthIo::send(&mut self.io, json!({"stage":"media-ready"})).await?;
        } else {
            let ready =
                tokio::time::timeout(Duration::from_secs(15), AuthIo::receive(&mut self.io))
                    .await
                    .context(uu_i18n::text("等待设备授权完成超时"))??;
            ensure!(ready["stage"]!="media-rejected",uu_i18n::text("主机正在串流或设备授权已变更，请在被控端检查后重试"));
            ensure!(
                ready == json!({"stage":"media-ready"}),
                "{}", uu_i18n::format!("主机未完成设备授权"));
            self.io.authenticated.store(true, Ordering::Release);
        }
        Ok(())
    }
    pub async fn reject_media(&mut self) {
        let _=AuthIo::send(&mut self.io,json!({"stage":"media-rejected"})).await;
    }
    pub async fn close(&mut self) {
        let _ = self.io.dc.close().await;
        self.peer.close().await;
        self.provider.close().await;
    }
    pub fn closed_signal(&self) -> watch::Receiver<bool> { self.io.state.clone() }
}
#[async_trait::async_trait]
impl SessionSignaling for Bootstrap {
    async fn send(&mut self, value: Value) -> Result<()> {
        ensure!(
            self.io.authenticated.load(Ordering::Acquire),
            "{}", uu_i18n::format!("设备尚未获得授权"));
        AuthIo::send(&mut self.io, value).await
    }
    async fn receive(&mut self) -> Result<Value> {
        ensure!(
            self.io.authenticated.load(Ordering::Acquire),
            "{}", uu_i18n::format!("设备尚未获得授权"));
        AuthIo::receive(&mut self.io).await
    }
    async fn disconnected(&mut self) -> Result<()> {
        ensure!(!*self.provider.closed.borrow(), "{}", uu_i18n::format!("连接服务已关闭"));
        tokio::select! {
            _=AuthIo::receive(&mut self.io)=>anyhow::bail!("原生设备信令已结束"),
            _=self.provider.closed.changed()=>anyhow::bail!("连接服务会话已结束"),
        }
    }
}

/// Copy only public candidate service configuration; the bootstrap has no
/// render statistics, so the HUD/details refer to the actual media peer.
pub fn ice(connection: &Connection) -> InternetIce {
    connection.ice.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        device_auth::{self, PairApproval, PairingControl, PairingDisplay},
        device_trust::{Permissions, TrustStore},
    };
    struct Memory {
        tx: mpsc::Sender<Value>,
        rx: mpsc::Receiver<Value>,
    }
    #[async_trait::async_trait]
    impl Transport for Memory {
        async fn send(&mut self, v: Value) -> Result<()> {
            // Exercise the provider-neutral SDP envelope roundtrip,
            // including a data-only m=application offer and answer.
            let encoded=crate::internet::pack_sdp(v.clone())?;
            let restored=crate::internet::unpack_sdp(&encoded)?;
            ensure!(restored==v,"UU SDP encoding changed the bootstrap binding");
            self.tx.send(restored).await.map_err(Into::into)
        }
        async fn receive(&mut self) -> Result<Value> {
            self.rx.recv().await.context("closed")
        }
        async fn close(&mut self) {
            self.rx.close();
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn real_data_only_bootstrap_authenticates_and_binds_the_channel_without_media() {
        let root = std::env::temp_dir().join(format!("eclipse-channel-{}", uuid::Uuid::new_v4()));
        let a = TrustStore::open(root.join("a")).unwrap();
        let b = TrustStore::open(root.join("b")).unwrap();
        let (tx, rx) = mpsc::channel(8);
        let duplicate = tx.clone();
        let (tx2, rx2) = mpsc::channel(8);
        let viewer = Connection {
            transport: Box::new(Memory { tx, rx: rx2 }),
            session: "generated".into(),
            ice: InternetIce::default(),
        };
        let mut server = Connection {
            transport: Box::new(Memory { tx: tx2, rx }),
            session: "generated".into(),
            ice: InternetIce::default(),
        };
        let receive = async {
            let offer = server.transport.receive().await?;
            Bootstrap::host(server, offer).await
        };
        let (viewer, host) = tokio::join!(Bootstrap::viewer(viewer), receive);
        let mut viewer = viewer.unwrap();
        let mut host = host.unwrap();
        assert_eq!(viewer.binding, host.binding);
        let (events, mut requests) = mpsc::unbounded_channel();
        let pairing = PairingControl::new(events);
        pairing.open();
        let display = PairingDisplay::default();
        let approve = async {
            let request = requests.recv().await.unwrap();
            request
                .respond
                .send(Some(
                    PairApproval::new(&display.code().unwrap(), Permissions::VIEW).unwrap(),
                ))
                .ok();
        };
        let (client, server, _) = tokio::join!(
            device_auth::client(
                &mut viewer.io,
                &a,
                "A",
                &viewer.binding,
                None,
                false,
                &display
            ),
            device_auth::host(&mut host.io, &b, "B", &host.binding, &pairing, None),
            approve
        );
        client.unwrap();
        server.unwrap();
        let (a, b) = tokio::join!(viewer.ready(false), host.ready(true));
        a.unwrap();
        b.unwrap();
        // Providers may repeat the original bootstrap after authentication.
        // It is neither a new media negotiation nor a transport disconnect.
        duplicate.send(envelope(host.peer.pc.local_description().await.unwrap())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let payload = json!({"generated":true,"padding":"x".repeat(9000)});
        SessionSignaling::send(&mut viewer, payload.clone())
            .await
            .unwrap();
        assert_eq!(SessionSignaling::receive(&mut host).await.unwrap(), payload);
        viewer.close().await;
        host.close().await;
        std::fs::remove_dir_all(root).unwrap();
    }
}
