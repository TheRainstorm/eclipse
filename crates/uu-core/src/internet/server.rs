//! Small self-hosted signaling server. Media never passes through this service.
use anyhow::{ensure, Context, Result};
use futures_util::{SinkExt, StreamExt};
use rustls_pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::{
    sync::{mpsc, watch, Semaphore},
    time::{interval, timeout},
};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::{
    accept_hdr_async_with_config,
    tungstenite::{
        handshake::server::{Request as HttpRequest, Response},
        protocol::WebSocketConfig,
        Message,
    },
};

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Request {
    Host { version: u32, device: String },
    Join { version: u32, device: String },
    Signal { session: String, body: Value },
    Leave { session: String },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Event {
    Ready,
    Connected { session: String, stun: Vec<String> },
    Signal { session: String, body: Value },
    Ended { session: String },
    Error { code: String },
}
pub struct Config {
    pub bind: SocketAddr,
    pub certificate: PathBuf,
    pub private_key: PathBuf,
    pub token_file: PathBuf,
    pub stun: Vec<String>,
}
type Sender = mpsc::Sender<Event>;
struct Viewer {
    id: uuid::Uuid,
    session: String,
    tx: Sender,
}
struct Host {
    id: uuid::Uuid,
    tx: Sender,
    viewers: HashMap<String, Viewer>,
    max_sessions: usize,
}
#[derive(Default)]
struct Rooms {
    hosts: HashMap<String, Host>,
}
#[derive(Clone)]
struct State {
    rooms: Arc<Mutex<Rooms>>,
    stun: Vec<String>,
}
enum Membership {
    Host {
        device: String,
        id: uuid::Uuid,
    },
    Viewer {
        device: String,
        id: uuid::Uuid,
        session: String,
    },
}
impl State {
    fn register(&self, request: Request, tx: Sender) -> Result<Membership> {
        let mut rooms = self.rooms.lock().unwrap();
        match request {
            Request::Host { version, device } => {
                validate_identity(version, &device, 2)?;
                ensure!(!rooms.hosts.contains_key(&device), "device_already_online");
                ensure!(rooms.hosts.len() < 128, "server_full");
                let id = uuid::Uuid::new_v4();
                tx.try_send(Event::Ready)?;
                rooms.hosts.insert(
                    device.clone(),
                    Host {
                        id,
                        tx,
                        viewers: HashMap::new(),
                        max_sessions: if version==2{crate::shared_host::MAX_SESSIONS}else{1},
                    },
                );
                Ok(Membership::Host { device, id })
            }
            Request::Join { version, device } => {
                validate_identity(version, &device, 1)?;
                let host = rooms.hosts.get_mut(&device).context("device_offline")?;
                ensure!(host.viewers.len() < host.max_sessions, "device_busy");
                let id = uuid::Uuid::new_v4();
                let session = uuid::Uuid::new_v4().to_string();
                host.tx.try_send(Event::Connected {
                    session: session.clone(),
                    stun: self.stun.clone(),
                })?;
                tx.try_send(Event::Connected {
                    session: session.clone(),
                    stun: self.stun.clone(),
                })?;
                host.viewers.insert(session.clone(), Viewer {
                    id,
                    session: session.clone(),
                    tx,
                });
                Ok(Membership::Viewer {
                    device,
                    id,
                    session,
                })
            }
            _ => anyhow::bail!("registration_required"),
        }
    }
    fn route(&self, member: &Membership, request: Request) -> Result<()> {
        let mut rooms = self.rooms.lock().unwrap();
        let (session, body) = match request {
            Request::Signal { session, body } => (session, Some(body)),
            Request::Leave { session } => (session, None),
            _ => anyhow::bail!("already_registered"),
        };
        let target = match member {
            Membership::Host { device, id } => {
                let host = rooms
                    .hosts
                    .get(device)
                    .filter(|h| h.id == *id)
                    .context("stale_host")?;
                // The viewer may disconnect while an answer/release is queued.
                // Its retired route must not close the shared host socket.
                let Some(viewer) = host.viewers.get(&session) else { return Ok(()); };
                &viewer.tx
            }
            Membership::Viewer {
                device,
                id,
                session: owned,
            } => {
                ensure!(*owned == session, "stale_session");
                let host = rooms.hosts.get(device).context("device_offline")?;
                ensure!(
                    host.viewers.get(&session).is_some_and(|v| v.id == *id),
                    "stale_session"
                );
                &host.tx
            }
        };
        let leaving = body.is_none();
        target.try_send(match body {
            Some(body) => Event::Signal { session: session.clone(), body },
            None => Event::Ended { session: session.clone() },
        })?;
        if leaving {
            let device = match member { Membership::Host{device,..}|Membership::Viewer{device,..}=>device };
            if let Some(host)=rooms.hosts.get_mut(device) { host.viewers.remove(&session); }
        }
        Ok(())
    }
    fn remove(&self, member: &Membership) {
        let mut rooms = self.rooms.lock().unwrap();
        match member {
            Membership::Host { device, id } => {
                if rooms.hosts.get(device).is_some_and(|h| h.id == *id) {
                    if let Some(host) = rooms.hosts.remove(device) {
                        for viewer in host.viewers.into_values() {
                            let _ = viewer.tx.try_send(Event::Ended { session: viewer.session });
                        }
                    }
                }
            }
            Membership::Viewer {
                device,
                id,
                session,
            } => {
                if let Some(host) = rooms.hosts.get_mut(device).filter(|h| {
                    h.viewers.get(session).is_some_and(|v| v.id == *id)
                }) {
                    host.viewers.remove(session);
                    let _ = host.tx.try_send(Event::Ended {
                        session: session.clone(),
                    });
                }
            }
        }
    }
}
fn validate_identity(version: u32, device: &str, maximum: u32) -> Result<()> {
    ensure!((1..=maximum).contains(&version), "unsupported_version");
    ensure!(
        !device.is_empty()
            && device.len() <= 80
            && device
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
        "invalid_device"
    );
    Ok(())
}
struct MemberGuard {
    state: State,
    member: Membership,
}
impl Drop for MemberGuard {
    fn drop(&mut self) {
        self.state.remove(&self.member);
    }
}
pub(crate) fn ws_config() -> WebSocketConfig {
    WebSocketConfig {
        max_message_size: Some(super::MAX_MESSAGE),
        max_frame_size: Some(super::MAX_MESSAGE),
        ..Default::default()
    }
}
pub(crate) fn read_token(path: &std::path::Path) -> Result<String> {
    ensure!(
        std::fs::metadata(path)?.len() <= 128,
        "server token file too large"
    );
    let text = std::fs::read_to_string(path)?;
    let text = text.trim();
    ensure!(
        text.len() == 64 && hex::decode(text).is_ok(),
        "server token must be 32 random bytes encoded as hex"
    );
    Ok(text.to_owned())
}

pub async fn serve(config: Config, mut stop: watch::Receiver<bool>) -> Result<()> {
    crate::install_crypto_provider();
    let stun = crate::direct_ice::InternetIce::new(config.stun)?
        .urls()
        .to_vec();
    let token = Arc::new(read_token(&config.token_file)?);
    let chain = CertificateDer::pem_file_iter(&config.certificate)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let key = PrivateKeyDer::from_pem_file(&config.private_key)?;
    let tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)?;
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let listener = crate::listen_socket::tcp(config.bind)
        .context("cannot bind signaling service")?;
    tracing::info!(address=%listener.local_addr()?,"native signaling listening; media forwarding disabled");
    let state = State {
        rooms: Arc::default(),
        stun,
    };
    let slots = Arc::new(Semaphore::new(256));
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _=stop.changed()=>if *stop.borrow()||stop.has_changed().is_err(){break;},
            accepted=listener.accept()=>{
                let (tcp,_)=accepted?;
                let Ok(permit)=slots.clone().try_acquire_owned() else {continue};
                let acceptor=acceptor.clone();let token=token.clone();let state=state.clone();
                tasks.spawn(async move {
                    let _permit=permit;
                    let result=async {
                        let tls=timeout(Duration::from_secs(10),acceptor.accept(tcp)).await??;
                        let ws=timeout(Duration::from_secs(10),accept_hdr_async_with_config(tls,move |req:&HttpRequest,response:Response| {
                            let expected=format!("Bearer {}",token);
                            let valid=req.uri().path()=="/v1/signal" && req.headers().get("authorization")
                                .is_some_and(|v|v.as_bytes().ct_eq(expected.as_bytes()).into());
                            if valid {Ok(response)} else {Err(tokio_tungstenite::tungstenite::http::Response::builder()
                                .status(401).body(Some("unauthorized".into())).unwrap())}
                        },Some(ws_config()))).await??;
                        handle(ws,state).await
                    }.await;
                    if result.is_err(){tracing::debug!("signaling connection ended");}
                });
            },
            _=tasks.join_next(),if !tasks.is_empty()=>{},
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}

async fn handle<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    mut ws: tokio_tungstenite::WebSocketStream<S>,
    state: State,
) -> Result<()> {
    let first = timeout(Duration::from_secs(10), ws.next())
        .await?
        .context("closed before registration")??;
    let request = serde_json::from_str::<Request>(first.to_text()?)
        .map_err(|_| anyhow::anyhow!("invalid registration"))?;
    let (tx, mut rx) = mpsc::channel(16);
    let member = match state.register(request, tx) {
        Ok(member) => member,
        Err(error) => {
            ws.send(Message::Text(
                serde_json::to_string(&Event::Error {
                    code: error.to_string(),
                })?
                .into(),
            ))
            .await?;
            return Ok(());
        }
    };
    let guard = MemberGuard { state, member };
    let mut heartbeat = interval(Duration::from_secs(10));
    let mut seen = tokio::time::Instant::now();
    let mut signals = 0usize;
    let mut signal_window = tokio::time::Instant::now();
    loop {
        tokio::select! {
            _=heartbeat.tick()=>{
                ensure!(seen.elapsed()<Duration::from_secs(35),"signaling heartbeat timed out");
                ws.send(Message::Ping(Vec::new())).await?;
            },
            Some(event)=rx.recv()=>{ws.send(Message::Text(serde_json::to_string(&event)?.into())).await?;},
            message=ws.next()=>{
                let message=message.context("signaling disconnected")??;seen=tokio::time::Instant::now();
                match message {
                    Message::Text(text)=>{
                        if signal_window.elapsed()>=Duration::from_secs(1){signals=0;signal_window=tokio::time::Instant::now();}
                        signals+=1;ensure!(signals<=32,"signaling negotiation limit exceeded");
                        let request=serde_json::from_str::<Request>(&text).map_err(|_|anyhow::anyhow!("invalid signal"))?;
                        let leave=matches!(request,Request::Leave{..});
                        guard.state.route(&guard.member,request)?;
                        if leave && matches!(guard.member,Membership::Viewer{..}) {break;}
                    },
                    Message::Ping(payload)=>ws.send(Message::Pong(payload)).await?,
                    Message::Pong(_)=>{},Message::Close(_)=>break,
                    _=>anyhow::bail!("unsupported signaling message"),
                }
            }
        }
    }
    let _ = ws.close(None).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_hosts_keep_their_single_session_limit_and_version_two_explicitly_allows_multiple(){
        for (version,limit) in [(1,1),(2,crate::shared_host::MAX_SESSIONS)] {
            let state=State{rooms:Arc::default(),stun:vec![]};let (tx,_host_receive)=mpsc::channel(32);
            state.register(Request::Host{version,device:"generated-compatibility-host".into()},tx).unwrap();
            let mut receive=Vec::new();
            for _ in 0..limit {let (tx,rx)=mpsc::channel(16);receive.push(rx);state.register(Request::Join{version:1,device:"generated-compatibility-host".into()},tx).unwrap();}
            let (tx,_)=mpsc::channel(16);assert!(state.register(Request::Join{version:1,device:"generated-compatibility-host".into()},tx).is_err());
            assert_eq!(state.rooms.lock().unwrap().hosts["generated-compatibility-host"].viewers.len(),limit);
        }
    }
    #[tokio::test]
    async fn concurrent_routes_are_private_bounded_and_host_leaves_only_one_session() {
        let state=State{rooms:Arc::default(),stun:vec![]};
        let (host_tx,mut host_rx)=mpsc::channel(32);
        let host=state.register(Request::Host{version:2,device:"generated-host".into()},host_tx).unwrap();
        host_rx.recv().await.unwrap();let mut viewers=Vec::new();let mut receive=Vec::new();
        for _ in 0..crate::shared_host::MAX_SESSIONS {
            let (tx,mut rx)=mpsc::channel(16);
            viewers.push(state.register(Request::Join{version:1,device:"generated-host".into()},tx).unwrap());
            rx.recv().await.unwrap();host_rx.recv().await.unwrap();receive.push(rx);
        }
        let (tx,_)=mpsc::channel(1);assert!(state.register(Request::Join{version:1,device:"generated-host".into()},tx).is_err());
        let session=|member:&Membership|match member{Membership::Viewer{session,..}=>session.clone(),_=>unreachable!()};
        state.route(&host,Request::Signal{session:session(&viewers[1]),body:serde_json::json!({"fixture":2})}).unwrap();
        assert!(matches!(receive[1].recv().await.unwrap(),Event::Signal{body,..} if body["fixture"]==2));
        assert!(receive[0].try_recv().is_err());
        assert!(state.route(&viewers[0],Request::Signal{session:session(&viewers[1]),body:serde_json::json!({})}).is_err());
        state.route(&host,Request::Leave{session:session(&viewers[0])}).unwrap();
        state.route(&host,Request::Leave{session:session(&viewers[0])}).unwrap();
        state.remove(&viewers[0]);
        state.route(&viewers[1],Request::Signal{session:session(&viewers[1]),body:serde_json::json!({"alive":true})}).unwrap();
        assert!(matches!(host_rx.recv().await.unwrap(),Event::Signal{body,..} if body["alive"]==true));
        assert_eq!(state.rooms.lock().unwrap().hosts["generated-host"].viewers.len(),3);
        state.remove(&host);
        assert!(matches!(receive[1].recv().await.unwrap(),Event::Ended{..}));
    }
    #[tokio::test]
    async fn stale_viewer_release_cannot_close_the_next_session() {
        let state = State {
            rooms: Arc::default(),
            stun: vec![],
        };
        let (tx, mut host_rx) = mpsc::channel(16);
        let host = state
            .register(
                Request::Host {
                    version: 1,
                    device: "fixture".into(),
                },
                tx,
            )
            .unwrap();
        host_rx.recv().await.unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let old = state
            .register(
                Request::Join {
                    version: 1,
                    device: "fixture".into(),
                },
                tx,
            )
            .unwrap();
        state.remove(&old);
        let (tx, _rx) = mpsc::channel(16);
        let new = state
            .register(
                Request::Join {
                    version: 1,
                    device: "fixture".into(),
                },
                tx,
            )
            .unwrap();
        state.remove(&old);
        assert!(state
            .route(
                &old,
                Request::Leave {
                    session: match &old {
                        Membership::Viewer { session, .. } => session.clone(),
                        _ => unreachable!(),
                    }
                }
            )
            .is_err());
        assert!(state.rooms.lock().unwrap().hosts["fixture"]
            .viewers
            .len() == 1);
        state.remove(&new);
        state.remove(&host);
        assert!(state.rooms.lock().unwrap().hosts.is_empty());
    }
}
