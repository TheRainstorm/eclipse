//! Persistent native LAN broker: public discovery, human-confirmed pairing,
//! pinned TLS resumption, and short-lived authenticated media sessions.
use crate::{
    native,
    native_discovery::ServerInfo,
    native_lan_store::{self as store, Device, Grants, Identity},
    native_pairing::Invitation,
};
use anyhow::{ensure, Context, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{mpsc, oneshot, watch, Semaphore},
    task::JoinSet,
    time::timeout,
};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use zeroize::{Zeroize, Zeroizing};
const INFO: &[u8; 17] = b"ECLIPSE-LAN-INFO\n";
const TLS: &[u8; 17] = b"ECLIPSE-LAN-TLS1\n";
const MAX: usize = 16 * 1024;
const IO_WAIT: Duration = Duration::from_secs(5);
const PAIR_WAIT: Duration = Duration::from_secs(90);
const EXPORTER: &[u8] = b"EXPORTER-Eclipse-LAN-Pair-v1";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicInfo {
    version: u8,
    server: ServerInfo,
    certificate: Vec<u8>,
    pairing_open: bool,
}
#[derive(Serialize, Deserialize)]
struct Secret(Vec<u8>);
impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Pair {
        client_id: String,
        name: String,
        nonce: Vec<u8>,
    },
    Connect {
        client_id: String,
        token: Secret,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Pending { server_id: String },
    Paired { token: Secret },
    Session { invitation: Invitation },
    Error { message: String },
}
async fn read<T: DeserializeOwned>(stream: &mut (impl AsyncRead + Unpin)) -> Result<T> {
    read_with_wait(stream, IO_WAIT).await
}
async fn read_with_wait<T: DeserializeOwned>(
    stream: &mut (impl AsyncRead + Unpin),
    wait: Duration,
) -> Result<T> {
    timeout(wait, async {
        let size = stream.read_u32().await? as usize;
        ensure!((1..=MAX).contains(&size), "invalid LAN message size");
        let mut bytes = Zeroizing::new(vec![0; size]);
        stream.read_exact(&mut bytes).await?;
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid LAN message"))
    })
    .await
    .context("LAN response timed out")?
}
async fn write<T: Serialize>(stream: &mut (impl AsyncWrite + Unpin), value: &T) -> Result<()> {
    let bytes = Zeroizing::new(serde_json::to_vec(value)?);
    ensure!(bytes.len() <= MAX, "LAN message exceeds size limit");
    timeout(IO_WAIT, async {
        stream.write_u32(bytes.len() as u32).await?;
        stream.write_all(&bytes).await?;
        stream.flush().await
    })
    .await
    .context("LAN write timed out")??;
    Ok(())
}
fn tls_acceptor(identity: &Identity) -> Result<TlsAcceptor> {
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key.clone()));
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])?
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from(identity.certificate.clone())],
        key,
    )?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}
async fn tls_connect(
    address: SocketAddr,
    certificate: &[u8],
) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
    ensure!(
        !certificate.is_empty() && certificate.len() <= 8192,
        "invalid server certificate"
    );
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from(certificate.to_vec()))
        .context("invalid server certificate")?;
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])?
    .with_root_certificates(roots)
    .with_no_client_auth();
    timeout(IO_WAIT, async {
        let mut socket = TcpStream::connect(address).await?;
        socket.set_nodelay(true)?;
        socket.write_all(TLS).await?;
        TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from(store::TLS_NAME)?, socket)
            .await
            .context("server identity verification failed")
    })
    .await
    .context("LAN TLS connection timed out")?
}
fn comparison(exported: [u8; 32]) -> String {
    let hash = Sha256::digest(exported);
    let n = u64::from_be_bytes(hash[..8].try_into().unwrap()) % 100_000_000;
    format!("{:04} {:04}", n / 10_000, n % 10_000)
}
fn context(server_id: &str, client_id: &str, nonce: &[u8]) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(server_id.as_bytes());
    h.update([0]);
    h.update(client_id.as_bytes());
    h.update([0]);
    h.update(nonce);
    h.finalize().to_vec()
}

/// The host UI owns this decision. No production path auto-approves pairing.
pub struct PairingRequest {
    pub id: String,
    pub client_name: String,
    pub code: String,
    pub decision: oneshot::Sender<bool>,
}
pub enum Event {
    Pairing(PairingRequest),
    PairingEnded(String),
    Warning(String),
    SessionStarted(String),
    SessionEnded(String),
}
#[derive(Clone, Debug)]
pub enum Status {
    Starting,
    Listening {
        address: SocketAddr,
        id: String,
        fingerprint: String,
    },
    Stopped,
}
#[derive(Clone)]
pub struct Config {
    pub name: String,
    pub bind: SocketAddr,
    pub discovery_bind: Option<SocketAddr>,
    pub data_dir: PathBuf,
    pub source: native::CaptureSource,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub encoder: native::EncoderPreference,
    pub allow_input: bool,
    pub audio: Option<native::AudioSource>,
    pub clipboard: Option<Arc<crate::clipboard::Clipboard>>,
}
struct Active {
    client: String,
    stop: watch::Sender<bool>,
    finished: Arc<AtomicBool>,
}
struct State {
    config: Config,
    info: ServerInfo,
    certificate: Vec<u8>,
    acceptor: TlsAcceptor,
    grants: Mutex<Grants>,
    events: mpsc::UnboundedSender<Event>,
    pairing_until: Mutex<Option<Instant>>,
    pair_slot: Arc<Semaphore>,
    active: Mutex<Option<Active>>,
    sessions: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    stop: watch::Receiver<bool>,
}
#[derive(Clone)]
pub struct Handle {
    state: Arc<State>,
}
impl Handle {
    pub fn allow_pairing(&self) {
        *self.state.pairing_until.lock().unwrap() = Some(Instant::now() + Duration::from_secs(120));
    }
    pub fn close_pairing(&self) {
        *self.state.pairing_until.lock().unwrap() = None;
    }
    pub fn clients(&self) -> Vec<store::AuthorizedClient> {
        self.state.grants.lock().unwrap().clients.clone()
    }
    pub fn revoke(&self, id: &str) -> Result<()> {
        self.state.grants.lock().unwrap().revoke(
            &self.state.config.data_dir.join("authorized-clients.json"),
            id,
        )?;
        self.state.stop_client(id);
        Ok(())
    }
    pub fn stop_session(&self) {
        if let Some(active) = &*self.state.active.lock().unwrap() {
            active.stop.send_replace(true);
        }
    }
}
impl State {
    fn pairing_open(&self) -> bool {
        self.pairing_until
            .lock()
            .unwrap()
            .is_some_and(|until| Instant::now() < until)
    }
    fn stop_client(&self, id: &str) {
        if let Some(active) = self
            .active
            .lock()
            .unwrap()
            .as_ref()
            .filter(|s| s.client == id)
        {
            active.stop.send_replace(true);
        }
    }
    fn public(&self) -> PublicInfo {
        PublicInfo {
            version: 1,
            server: self.info.clone(),
            certificate: self.certificate.clone(),
            pairing_open: self.pairing_open(),
        }
    }
}
pub struct Server {
    listener: TcpListener,
    discovery: Option<UdpSocket>,
    state: Arc<State>,
}
impl Server {
    pub async fn bind(
        config: Config,
        stop: watch::Receiver<bool>,
        events: mpsc::UnboundedSender<Event>,
    ) -> Result<(Self, Handle)> {
        ensure!(
            !config.bind.ip().is_multicast(),
            "invalid LAN listen address"
        );
        let identity = Identity::open(&config.data_dir.join("server-identity.json"))?;
        let acceptor = tls_acceptor(&identity)?;
        let listener = TcpListener::bind(config.bind)
            .await
            .context("cannot start LAN server; address may already be in use")?;
        let info = ServerInfo {
            id: identity.id.clone(),
            name: config.name.clone(),
            port: listener.local_addr()?.port(),
            fingerprint: identity.fingerprint(),
        };
        info.validate()?;
        let grants = Grants::open(
            &config.data_dir.join("authorized-clients.json"),
            &identity.id,
        )?;
        let discovery = if let Some(address) = config.discovery_bind {
            match UdpSocket::bind(address).await {
                Ok(socket) => Some(socket),
                Err(_) => {
                    let _ = events.send(Event::Warning(
                        "局域网发现端口不可用，仍可通过 IP 连接".into(),
                    ));
                    None
                }
            }
        } else {
            None
        };
        let state = Arc::new(State {
            config,
            info,
            certificate: identity.certificate.clone(),
            acceptor,
            grants: Mutex::new(grants),
            events,
            pairing_until: Mutex::new(None),
            pair_slot: Arc::new(Semaphore::new(1)),
            active: Mutex::new(None),
            sessions: Mutex::new(Vec::new()),
            stop,
        });
        let handle = Handle {
            state: state.clone(),
        };
        Ok((
            Self {
                listener,
                discovery,
                state,
            },
            handle,
        ))
    }
    pub fn address(&self) -> Result<SocketAddr> {
        Ok(self.listener.local_addr()?)
    }
    pub fn status(&self) -> Result<Status> {
        Ok(Status::Listening {
            address: self.address()?,
            id: self.state.info.id.clone(),
            fingerprint: self.state.info.fingerprint.clone(),
        })
    }
    pub async fn run(mut self) -> Result<()> {
        let mut stop = self.state.stop.clone();
        let mut clients = JoinSet::new();
        let permits = Arc::new(Semaphore::new(8));
        let discovery = self.discovery.take().map(|socket| {
            tokio::spawn(crate::native_discovery::respond(
                socket,
                self.state.info.clone(),
                stop.clone(),
            ))
        });
        let mut attempts = std::collections::HashMap::<std::net::IpAddr, (Instant, u32)>::new();
        loop {
            if *stop.borrow() {
                break;
            }
            tokio::select! {
             _=stop.changed()=>break,
             accepted=self.listener.accept()=>{
              let (socket,address)=accepted?;let Ok(permit)=permits.clone().try_acquire_owned() else{continue;};
              let now=Instant::now();attempts.retain(|_,(at,_)|now.duration_since(*at)<Duration::from_secs(60));if attempts.len()>=64&&!attempts.contains_key(&address.ip()){continue;}
              let entry=attempts.entry(address.ip()).or_insert((now,0));if entry.1>=24{continue;}entry.1+=1;
              let state=self.state.clone();clients.spawn(async move{let _permit=permit;let _=handle_socket(socket,state).await;});
             },
             _=clients.join_next(),if !clients.is_empty()=>{},
            }
        }
        if let Some(active) = &*self.state.active.lock().unwrap() {
            active.stop.send_replace(true);
        }
        clients.abort_all();
        while clients.join_next().await.is_some() {}
        let tasks = std::mem::take(&mut *self.state.sessions.lock().unwrap());
        for mut task in tasks {
            if timeout(Duration::from_secs(3), &mut task).await.is_err() {
                task.abort();
                let _ = task.await;
            }
        }
        if let Some(task) = discovery {
            task.abort();
            let _ = task.await;
        }
        Ok(())
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(active) = &*self.state.active.lock().unwrap() {
            active.stop.send_replace(true);
        }
        for task in self.state.sessions.lock().unwrap().iter() {
            task.abort();
        }
    }
}
async fn handle_socket(mut socket: TcpStream, state: Arc<State>) -> Result<()> {
    socket.set_nodelay(true)?;
    let local = socket.local_addr()?;
    let mut header = [0; 17];
    timeout(IO_WAIT, socket.read_exact(&mut header))
        .await
        .context("LAN request timed out")??;
    if &header == INFO {
        return write(&mut socket, &state.public()).await;
    }
    ensure!(&header == TLS, "unknown LAN protocol");
    let mut stream = timeout(IO_WAIT, state.acceptor.accept(socket))
        .await
        .context("LAN TLS handshake timed out")??;
    let request: Request = read(&mut stream).await?;
    match request {
        Request::Pair {
            client_id,
            name,
            nonce,
        } => {
            if !state.pairing_open() {
                return write(
                    &mut stream,
                    &Reply::Error {
                        message: "请先在主机允许新设备配对".into(),
                    },
                )
                .await;
            }
            let Ok(_slot) = state.pair_slot.clone().try_acquire_owned() else {
                return write(
                    &mut stream,
                    &Reply::Error {
                        message: "主机正在处理另一项配对请求".into(),
                    },
                )
                .await;
            };
            ensure!(
                uuid::Uuid::parse_str(&client_id).is_ok()
                    && client_id.len() <= 36
                    && name.chars().count() <= 64
                    && !name.trim().is_empty()
                    && !name.chars().any(char::is_control)
                    && nonce.len() == 32,
                "invalid pairing request"
            );
            let context = context(&state.info.id, &client_id, &nonce);
            let exported =
                stream
                    .get_ref()
                    .1
                    .export_keying_material([0; 32], EXPORTER, Some(&context))?;
            let id = uuid::Uuid::new_v4().to_string();
            let (decision, rx) = oneshot::channel();
            write(
                &mut stream,
                &Reply::Pending {
                    server_id: state.info.id.clone(),
                },
            )
            .await?;
            state
                .events
                .send(Event::Pairing(PairingRequest {
                    id: id.clone(),
                    client_name: name.clone(),
                    code: comparison(exported),
                    decision,
                }))
                .map_err(|_| anyhow::anyhow!("host confirmation UI is unavailable"))?;
            let mut stop = state.stop.clone();
            let mut unexpected = [0];
            let allowed = tokio::select! {value=timeout(PAIR_WAIT,rx)=>matches!(value,Ok(Ok(true))),_=stop.changed()=>false,_=stream.read(&mut unexpected)=>false};
            let _ = state.events.send(Event::PairingEnded(id));
            if !allowed {
                return write(
                    &mut stream,
                    &Reply::Error {
                        message: "主机未允许配对或请求已超时".into(),
                    },
                )
                .await;
            }
            let token = Secret(store::fresh_token());
            state.grants.lock().unwrap().authorize(
                &state.config.data_dir.join("authorized-clients.json"),
                &client_id,
                &name,
                &token.0,
            )?;
            state.stop_client(&client_id);
            write(&mut stream, &Reply::Paired { token }).await?;
        }
        Request::Connect { client_id, token } => {
            if !state.grants.lock().unwrap().accepts(&client_id, &token.0) {
                return write(
                    &mut stream,
                    &Reply::Error {
                        message: "配对已失效，请在主机允许后重新配对".into(),
                    },
                )
                .await;
            }
            match start_session(state, client_id, local).await {
                Ok(invitation) => write(&mut stream, &Reply::Session { invitation }).await?,
                Err(error) => {
                    write(
                        &mut stream,
                        &Reply::Error {
                            message: error.to_string(),
                        },
                    )
                    .await?
                }
            }
        }
    }
    Ok(())
}
struct SessionGuard {
    state: Arc<State>,
    id: String,
    finished: Arc<AtomicBool>,
}
impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.finished.store(true, Ordering::Release);
        let _ = self.state.events.send(Event::SessionEnded(self.id.clone()));
    }
}
async fn start_session(state: Arc<State>, client: String, local: SocketAddr) -> Result<Invitation> {
    let (stop, rx) = watch::channel(false);
    let finished = Arc::new(AtomicBool::new(false));
    let id = uuid::Uuid::new_v4().to_string();
    {
        let mut active = state.active.lock().unwrap();
        ensure!(
            active
                .as_ref()
                .is_none_or(|s| s.finished.load(Ordering::Acquire)),
            "主机正在串流，请先结束已有会话"
        );
        *active = Some(Active {
            client: client.clone(),
            stop: stop.clone(),
            finished: finished.clone(),
        });
    }
    let path = state
        .config
        .data_dir
        .join(format!("session-{id}.eclipse-invite.json"));
    let (phase, mut status) = watch::channel(native::HostStatus::Starting);
    let config = native::HostConfig {
        bind: SocketAddr::new(local.ip(), 0),
        source: state.config.source.clone(),
        invitation_file: path.clone(),
        invitation_lifetime: Duration::from_secs(30),
        fps: state.config.fps,
        bitrate_kbps: state.config.bitrate_kbps,
        encoder: state.config.encoder,
        session_limit: None,
        allow_input: state.config.allow_input,
        audio: state.config.audio,
        clipboard: state.config.clipboard.clone(),
    };
    let task_state = state.clone();
    let task_id = id.clone();
    let (done, mut failure) = oneshot::channel();
    let task = tokio::spawn(async move {
        let _guard = SessionGuard {
            state: task_state.clone(),
            id: task_id.clone(),
            finished,
        };
        let _ = task_state.events.send(Event::SessionStarted(client));
        let result = native::serve_with_status(config, rx, Some(phase)).await;
        let _ = done.send(
            result
                .map(|_| ())
                .map_err(|_| "原生串流服务启动或运行失败".to_string()),
        );
    });
    {
        let mut tasks = state.sessions.lock().unwrap();
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
    }
    let ready=timeout(Duration::from_secs(6),async{loop{if matches!(*status.borrow(),native::HostStatus::Waiting{..}){return Invitation::load(&path);}
  tokio::select!{_=status.changed()=>{},result=&mut failure=>{return Err(anyhow::anyhow!(match result{Ok(Err(error))=>error,_=>"原生串流服务已结束".into()}));}}
 }}).await;
    match ready {
        Ok(Ok(invitation)) => Ok(invitation),
        Ok(Err(error)) => {
            stop.send_replace(true);
            Err(error)
        }
        Err(_) => {
            stop.send_replace(true);
            Err(anyhow::anyhow!("原生串流服务启动超时"))
        }
    }
}

pub struct Found {
    pub info: ServerInfo,
    pub address: SocketAddr,
    pub certificate: Vec<u8>,
    pub pairing_open: bool,
}
pub async fn inspect(endpoint: &str) -> Result<Found> {
    let endpoint = crate::native_discovery::endpoint(endpoint)?;
    timeout(IO_WAIT, async {
        let mut socket = TcpStream::connect(&endpoint)
            .await
            .context("无法连接服务端，请检查地址及是否已启用共享")?;
        let address = socket.peer_addr()?;
        socket.write_all(INFO).await?;
        let public: PublicInfo = read(&mut socket).await?;
        public.server.validate()?;
        ensure!(
            public.version == 1
                && !public.certificate.is_empty()
                && public.certificate.len() <= 8192
                && hex::encode(Sha256::digest(&public.certificate)) == public.server.fingerprint,
            "服务端身份信息无效"
        );
        Ok(Found {
            info: public.server,
            address,
            certificate: public.certificate,
            pairing_open: public.pairing_open,
        })
    })
    .await
    .context("检查服务端超时")?
}
pub async fn pair(
    endpoint: &str,
    client_id: &str,
    client_name: &str,
    progress: mpsc::UnboundedSender<String>,
) -> Result<Device> {
    let found = inspect(endpoint).await?;
    ensure!(found.pairing_open, "请先在服务端允许新设备配对");
    let mut stream = tls_connect(found.address, &found.certificate).await?;
    let nonce = store::fresh_token();
    let context = context(&found.info.id, client_id, &nonce);
    let code = comparison(stream.get_ref().1.export_keying_material(
        [0; 32],
        EXPORTER,
        Some(&context),
    )?);
    write(
        &mut stream,
        &Request::Pair {
            client_id: client_id.into(),
            name: client_name.into(),
            nonce,
        },
    )
    .await?;
    match read::<Reply>(&mut stream).await? {
        Reply::Pending { server_id } => ensure!(server_id == found.info.id, "服务端身份发生变化"),
        Reply::Error { message } => return Err(anyhow::anyhow!(message)),
        _ => anyhow::bail!("配对响应无效"),
    }
    progress
        .send(code)
        .map_err(|_| anyhow::anyhow!("配对窗口已关闭"))?;
    let response = read_with_wait::<Reply>(&mut stream, PAIR_WAIT)
        .await
        .context("等待主机确认超时")?;
    match response {
        Reply::Paired { mut token } => {
            ensure!(token.0.len() == 32, "配对凭据无效");
            Ok(Device {
                id: found.info.id,
                name: found.info.name,
                endpoint: crate::native_discovery::endpoint(endpoint)?,
                certificate: found.certificate,
                token: std::mem::take(&mut token.0),
            })
        }
        Reply::Error { message } => Err(anyhow::anyhow!(message)),
        _ => Err(anyhow::anyhow!("配对响应无效")),
    }
}
/// Check a discovered new address against an already-paired certificate before
/// updating the saved endpoint. No authorization token or media request is sent.
pub async fn verify_address(device: &Device, endpoint: &str) -> Result<Device> {
    let found = inspect(endpoint).await?;
    ensure!(
        found.info.id == device.id && found.certificate == device.certificate,
        "服务端身份已变化，未发送连接凭据"
    );
    let _stream = tls_connect(found.address, &device.certificate).await?;
    let mut updated = device.clone();
    updated.endpoint = crate::native_discovery::endpoint(endpoint)?;
    Ok(updated)
}
pub async fn connect(device: &Device, client_id: &str) -> Result<Invitation> {
    let found = inspect(&device.endpoint).await?;
    ensure!(
        found.info.id == device.id && found.certificate == device.certificate,
        "服务端身份已变化；请核实设备后重新配对，未发送连接凭据"
    );
    let mut stream = tls_connect(found.address, &device.certificate).await?;
    write(
        &mut stream,
        &Request::Connect {
            client_id: client_id.into(),
            token: Secret(device.token.clone()),
        },
    )
    .await?;
    match read_with_wait::<Reply>(&mut stream, Duration::from_secs(10)).await? {
        Reply::Session { invitation } => Ok(invitation),
        Reply::Error { message } => Err(anyhow::anyhow!(message)),
        _ => Err(anyhow::anyhow!("连接响应无效")),
    }
}

#[cfg(test)]
#[path = "native_lan_tests.rs"]
mod tests;
