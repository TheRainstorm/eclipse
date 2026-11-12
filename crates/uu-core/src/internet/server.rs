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
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
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
#[path = "groups.rs"]
pub mod groups;
pub use super::turn::Config as TurnConfig;
use super::turn::Credentials;
use webrtc::ice_transport::ice_server::RTCIceServer;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    pub device: String,
    pub name: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Request {
    Probe { version: u32, devices: Vec<String> },
    Directory { version: u32 },
    Host { version: u32, device: String },
    Join { version: u32, device: String },
    Signal { session: String, body: Value },
    Leave { session: String },
    Publish { name: String },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Event {
    Presence { online: Vec<String> },
    Directory { devices: Vec<Device> },
    Ready,
    Connected { session: String, stun: Vec<String> },
    Session { session: String, stun: Vec<String>, turn: Vec<RTCIceServer> },
    Signal { session: String, body: Value },
    Ended { session: String },
    Error { code: String },
}
pub struct Config {
    pub bind: SocketAddr,
    pub certificate: PathBuf,
    pub private_key: PathBuf,
    pub groups_file: PathBuf,
    pub stun: Vec<String>,
    pub turn: Option<TurnConfig>,
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
    multiplexed: bool,
    version: u32,
    name: String,
}
#[derive(Default)]
struct Rooms {
    hosts: HashMap<String, Host>,
}
#[derive(Clone)]
struct State {
    rooms: Arc<Mutex<Rooms>>,
    stun: Vec<String>,
    turn: Option<Arc<Credentials>>,
}
// Authentication chooses a namespace once. Every operation on the socket then
// uses its independent Rooms, including directory, presence and media routing.
// Weak entries retain an active group's identity across credential reloads
// without keeping removed, idle groups alive forever.
struct GroupStates {
    stun: Vec<String>,
    turn: Option<Arc<Credentials>>,
    rooms: Mutex<HashMap<uuid::Uuid, Weak<Mutex<Rooms>>>>,
}
impl GroupStates {
    fn state(&self, group: uuid::Uuid) -> State {
        let mut groups = self.rooms.lock().unwrap();
        groups.retain(|_, rooms| rooms.strong_count() != 0);
        let rooms = groups.get(&group).and_then(Weak::upgrade).unwrap_or_else(|| {
            let rooms = Arc::new(Mutex::new(Rooms::default()));
            groups.insert(group, Arc::downgrade(&rooms));
            rooms
        });
        State { rooms, stun: self.stun.clone(), turn: self.turn.clone() }
    }
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
    fn directory(&self, version:u32)->Result<Vec<Device>> {
        ensure!(version==1,"unsupported_version");
        let rooms=self.rooms.lock().unwrap();
        let mut devices:Vec<_>=rooms.hosts.iter().filter(|(_,h)|!h.tx.is_closed())
            .map(|(device,h)|Device{device:device.clone(),name:h.name.clone()}).collect();
        devices.sort_by(|a,b|a.name.cmp(&b.name).then_with(||a.device.cmp(&b.device)));
        Ok(devices)
    }
    fn connected(&self, session:&str, modern:bool, role:&str)->Result<Event> {
        Ok(if modern {Event::Session{session:session.into(),stun:self.stun.clone(),
            turn:self.turn.as_ref().map(|t|t.issue(session,role)).transpose()?.unwrap_or_default()}}
        else {Event::Connected{session:session.into(),stun:self.stun.clone()}})
    }
    fn presence(&self, version:u32,devices:&[String])->Result<Vec<String>> {
        ensure!(version==1 && !devices.is_empty() && devices.len()<=128,"invalid_probe");
        for device in devices {validate_identity(1,device,1)?;}
        let rooms=self.rooms.lock().unwrap();
        Ok(devices.iter().filter(|id|rooms.hosts.get(*id).is_some_and(|host|!host.tx.is_closed())).cloned().collect())
    }
    fn register(&self, request: Request, tx: Sender) -> Result<Membership> {
        let mut rooms = self.rooms.lock().unwrap();
        match request {
            Request::Host { version, device } => {
                validate_identity(version, &device, 3)?;
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
                        multiplexed: version>=2,
                        version,
                        name: device.clone(),
                    },
                );
                Ok(Membership::Host { device, id })
            }
            Request::Join { version, device } => {
                validate_identity(version, &device, 2)?;
                let host = rooms.hosts.get_mut(&device).context("device_offline")?;
                // v1 hosts opt into a single route; v2's session IDs allow
                // independent viewers without a fixed per-host count limit.
                ensure!(host.multiplexed || host.viewers.is_empty(), "device_busy");
                let id = uuid::Uuid::new_v4();
                let session = uuid::Uuid::new_v4().to_string();
                let modern=host.version>=3 && version>=2;
                host.tx.try_send(self.connected(&session,modern,"host")?)?;
                tx.try_send(self.connected(&session,modern,"viewer")?)?;
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
        if let Request::Publish{name}=request {
            ensure!(!name.trim().is_empty() && name.chars().count()<=64 && !name.chars().any(char::is_control),"invalid_device_name");
            let Membership::Host{device,id}=member else {anyhow::bail!("host_required");};
            let host=rooms.hosts.get_mut(device).filter(|h|h.id==*id && h.version>=3).context("stale_host")?;
            host.name=name.trim().into(); return Ok(());
        }
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
    let turn=config.turn.map(Credentials::load).transpose()?.map(Arc::new);
    let access = Arc::new(groups::Access::new(groups::Groups::load(&config.groups_file)?)?);
    let chain = CertificateDer::pem_file_iter(&config.certificate)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let key = PrivateKeyDer::from_pem_file(&config.private_key)?;
    let tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)?;
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let listener = crate::listen_socket::tcp(config.bind)
        .context("cannot bind signaling service")?;
    tracing::info!(address=%listener.local_addr()?,turn_configured=turn.is_some(),"native connection signaling listening");
    let states = Arc::new(GroupStates {
        stun, turn,
        rooms: Mutex::default(),
    });
    let slots = Arc::new(Semaphore::new(256));
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(access.clone().reload(config.groups_file, stop.clone()));
    loop {
        tokio::select! {
            _=stop.changed()=>if *stop.borrow()||stop.has_changed().is_err(){break;},
            accepted=listener.accept()=>{
                let (tcp,_)=accepted?;
                let Ok(permit)=slots.clone().try_acquire_owned() else {continue};
                let acceptor=acceptor.clone();let access=access.clone();let states=states.clone();
                tasks.spawn(async move {
                    let _permit=permit;
                    let result=async {
                        tcp.set_nodelay(true)?;
                        let tls=timeout(Duration::from_secs(10),acceptor.accept(tcp)).await??;
                        let mut group = None;
                        let ws=timeout(Duration::from_secs(10),accept_hdr_async_with_config(tls, |req:&HttpRequest,response:Response| {
                            group = access.authenticate(req);
                            if group.is_some() {Ok(response)} else {Err(tokio_tungstenite::tungstenite::http::Response::builder()
                                .status(401).body(Some("unauthorized".into())).unwrap())}
                        },Some(ws_config()))).await??;
                        let state = states.state(group.context("missing authenticated private group")?);
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
    // Authenticated metadata only: a probe never joins a room, consumes a host
    // session or wakes its desktop/capture/pairing paths.
    if let Request::Probe{version,devices}=&request {
        let online=state.presence(*version,devices)?;
        ws.send(Message::Text(serde_json::to_string(&Event::Presence{online})?.into())).await?;
        let _=ws.close(None).await;
        return Ok(());
    }
    if let Request::Directory{version}=request {
        let devices=state.directory(version)?;
        ws.send(Message::Text(serde_json::to_string(&Event::Directory{devices})?.into())).await?;
        let _=ws.close(None).await;return Ok(());
    }
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
    #[tokio::test]
    async fn identical_device_ids_and_guessed_sessions_stay_inside_the_authenticated_group() {
        let states = GroupStates { stun: Vec::new(), turn: None, rooms: Mutex::default() };
        let a = states.state(uuid::Uuid::new_v4());
        let b = states.state(uuid::Uuid::new_v4());
        let mut hosts = Vec::new();
        let mut viewers = Vec::new();
        let mut receiving = Vec::new();
        let mut host_events = Vec::new();
        for state in [&a, &b] {
            let (send, mut receive) = mpsc::channel(16);
            let host = state.register(Request::Host { version: 3, device: "same-device".into() }, send).unwrap();
            receive.recv().await.unwrap();
            let (send, mut viewer_receive) = mpsc::channel(16);
            let viewer = state.register(Request::Join { version: 2, device: "same-device".into() }, send).unwrap();
            viewer_receive.recv().await.unwrap(); receive.recv().await.unwrap();
            hosts.push(host); viewers.push(viewer); receiving.push(viewer_receive); host_events.push(receive);
        }
        let session = |member: &Membership| match member { Membership::Viewer { session, .. } => session.clone(), _ => unreachable!() };
        let guessed = session(&viewers[1]);
        a.route(&hosts[0], Request::Signal { session: guessed.clone(), body: serde_json::json!({"cross_group":true}) }).unwrap();
        assert!(a.route(&viewers[0], Request::Signal { session: guessed, body: serde_json::json!({}) }).is_err());
        assert!(receiving[1].try_recv().is_err());
        assert!(host_events[1].try_recv().is_err());
        a.remove(&hosts[1]);
        assert_eq!(a.directory(1).unwrap().len(), 1, "foreign cleanup cannot retire this group's host");
        b.route(&hosts[1], Request::Signal { session: session(&viewers[1]), body: serde_json::json!({"same_group":true}) }).unwrap();
        assert!(matches!(receiving[1].recv().await, Some(Event::Signal { body, .. }) if body["same_group"] == true));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn real_wss_groups_hide_devices_and_reload_credentials_without_interrupting_existing_routes() {
        use super::super::{enrollment::group_url, self_hosted};
        crate::install_crypto_provider();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
        let root = Cleanup(std::env::temp_dir().join(format!("eclipse-group-wss-{}", uuid::Uuid::new_v4())));
        std::fs::create_dir(&root.0).unwrap();
        let certificate = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let cert = root.0.join("cert.pem"); let key = root.0.join("key.pem"); let registry = root.0.join("groups.json");
        crate::private_file::write_atomic(&cert, certificate.cert.pem().as_bytes()).unwrap();
        crate::private_file::write_atomic(&key, certificate.key_pair.serialize_pem().as_bytes()).unwrap();
        let first = groups::Group::new("Generated first group".into()).unwrap();
        let second = groups::Group::new("Generated second group".into()).unwrap();
        groups::Groups { version: 1, groups: vec![first.clone(), second.clone()] }.save(&registry).unwrap();
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let address = reserved.local_addr().unwrap(); drop(reserved);
        let base = format!("wss://{address}/v1/signal");
        let endpoint = |group: &groups::Group, suffix: &str| {
            let token_file = root.0.join(format!("token-{suffix}"));
            crate::private_file::write_atomic(&token_file, group.token.as_bytes()).unwrap();
            self_hosted::Config { url: group_url(&base, &group.id).unwrap(), token_file, ca_file: Some(cert.clone()) }
        };
        let a = endpoint(&first, "first"); let b = endpoint(&second, "second");
        let (stop, receiver) = watch::channel(false);
        let server = tokio::spawn(serve(Config { bind: address, certificate: cert.clone(), private_key: key, groups_file: registry.clone(), stun: Vec::new(), turn: None }, receiver));
        let mut host_a = timeout(Duration::from_secs(4), async {
            loop { match self_hosted::Listener::online_named(&a, "same-device", "Generated A").await {
                Ok(host) => break host, Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }}
        }).await.unwrap();
        let mut host_b = self_hosted::Listener::online_named(&b, "same-device", "Generated B").await.unwrap();
        let mut private_b = self_hosted::Listener::online_named(&b, "only-second", "Generated B private").await.unwrap();
        async fn named(config: &self_hosted::Config, name: &str) -> Vec<Device> {
            timeout(Duration::from_secs(4), async {
                loop {
                    if let Ok(devices) = self_hosted::directory(config).await {
                        if devices.iter().any(|device| device.name == name) { break devices; }
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }).await.unwrap()
        }
        assert_eq!(named(&a, "Generated A").await.len(), 1);
        assert_eq!(named(&b, "Generated B private").await.len(), 2);
        let probe = vec!["same-device".into(), "only-second".into()];
        assert_eq!(self_hosted::presence(&a, &probe).await.unwrap(), vec!["same-device"]);
        assert_eq!(self_hosted::presence(&b, &probe).await.unwrap().len(), 2);
        assert!(self_hosted::join(&a, "only-second").await.is_err());
        let missing = self_hosted::Config { url: base.clone(), ..a.clone() };
        let wrong = self_hosted::Config { url: b.url.clone(), ..a.clone() };
        assert!(self_hosted::directory(&missing).await.is_err());
        assert!(self_hosted::directory(&wrong).await.is_err());
        let mut viewer_a = self_hosted::join(&a, "same-device").await.unwrap();
        let mut at_a = host_a.accept().await.unwrap();
        let mut viewer_b = self_hosted::join(&b, "same-device").await.unwrap();
        let mut at_b = host_b.accept().await.unwrap();
        for (viewer, host, marker) in [(&mut viewer_a, &mut at_a, "first"), (&mut viewer_b, &mut at_b, "second")] {
            viewer.transport.send(serde_json::json!({"group":marker})).await.unwrap();
            assert_eq!(host.transport.receive().await.unwrap()["group"], marker);
        }
        let mut rotated = first.clone();
        rotated.token = groups::Group::new("Generated rotated credential".into()).unwrap().token;
        let new_a = endpoint(&rotated, "rotated");
        groups::Groups::edit(&registry, |groups| {
            groups.groups.iter_mut().find(|group| group.id == first.id).unwrap().token = rotated.token.clone();
            Ok(())
        }).unwrap();
        timeout(Duration::from_secs(4), async {
            while self_hosted::directory(&a).await.is_ok() { tokio::time::sleep(Duration::from_millis(20)).await; }
        }).await.unwrap();
        assert_eq!(named(&new_a, "Generated A").await.len(), 1, "credential rotation retains the active group's Rooms");
        viewer_a.transport.send(serde_json::json!({"after_rotation":true})).await.unwrap();
        assert_eq!(at_a.transport.receive().await.unwrap()["after_rotation"], true);
        let third = groups::Group::new("Generated added group".into()).unwrap();
        let c = endpoint(&third, "third");
        groups::Groups::edit(&registry, |groups| { groups.groups.push(third); Ok(()) }).unwrap();
        // A newly added group must become usable without restarting the broker.
        timeout(Duration::from_secs(4), async {
            while self_hosted::directory(&c).await.is_err() { tokio::time::sleep(Duration::from_millis(20)).await; }
        }).await.unwrap();
        assert!(self_hosted::directory(&c).await.unwrap().is_empty());
        crate::private_file::write_atomic(&registry, b"invalid generated registry").unwrap();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(named(&new_a, "Generated A").await.len(), 1);
        assert_eq!(named(&b, "Generated B private").await.len(), 2);
        viewer_b.transport.send(serde_json::json!({"after_reload_error":true})).await.unwrap();
        assert_eq!(at_b.transport.receive().await.unwrap()["after_reload_error"], true);
        for connection in [&mut viewer_a, &mut at_a, &mut viewer_b, &mut at_b] { connection.transport.close().await; }
        host_a.close().await; host_b.close().await; private_b.close().await;
        stop.send_replace(true); server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn relay_capability_is_negotiated_per_pair_and_directory_names_do_not_authorize_sessions() {
        for (host_version,viewer_version,modern) in [(1,1,false),(2,2,false),(3,1,false),(3,2,true)] {
            let state=State{rooms:Arc::default(),stun:vec![],turn:None};
            let (tx,mut host_events)=mpsc::channel(16);
            let host=state.register(Request::Host{version:host_version,device:"generated-host".into()},tx).unwrap();
            assert!(matches!(host_events.recv().await,Some(Event::Ready)));
            if host_version==3 {
                state.route(&host,Request::Publish{name:"Generated host".into()}).unwrap();
                assert_eq!(state.directory(1).unwrap()[0].name,"Generated host");
            }
            let (tx,mut viewer_events)=mpsc::channel(16);
            let viewer=state.register(Request::Join{version:viewer_version,device:"generated-host".into()},tx).unwrap();
            for event in [host_events.recv().await.unwrap(),viewer_events.recv().await.unwrap()] {
                assert_eq!(matches!(event,Event::Session{..}),modern);
            }
            assert!(state.route(&viewer,Request::Publish{name:"Another host".into()}).is_err());
            state.remove(&host);
            assert!(state.directory(1).unwrap().is_empty());
        }
    }
    #[test]
    fn signaling_v1_retains_one_route_and_v2_accepts_more_than_four_viewers(){
        for (version,count) in [(1,1),(2,6)] {
            let state=State{rooms:Arc::default(),turn:None,stun:vec![]};let (tx,_host_receive)=mpsc::channel(32);
            state.register(Request::Host{version,device:"generated-compatibility-host".into()},tx).unwrap();
            let mut receive=Vec::new();
            for _ in 0..count {let (tx,rx)=mpsc::channel(16);receive.push(rx);state.register(Request::Join{version:1,device:"generated-compatibility-host".into()},tx).unwrap();}
            let (tx,rx)=mpsc::channel(16);receive.push(rx);
            let joined=state.register(Request::Join{version:1,device:"generated-compatibility-host".into()},tx);
            assert_eq!(joined.is_ok(),version==2);
            assert_eq!(state.rooms.lock().unwrap().hosts["generated-compatibility-host"].viewers.len(),count+usize::from(version==2));
        }
    }
    #[tokio::test]
    async fn concurrent_routes_are_private_and_host_leaves_only_one_session() {
        let state=State{rooms:Arc::default(),turn:None,stun:vec![]};
        let (host_tx,mut host_rx)=mpsc::channel(32);
        let host=state.register(Request::Host{version:2,device:"generated-host".into()},host_tx).unwrap();
        host_rx.recv().await.unwrap();let mut viewers=Vec::new();let mut receive=Vec::new();
        for _ in 0..6 {
            let (tx,mut rx)=mpsc::channel(16);
            viewers.push(state.register(Request::Join{version:1,device:"generated-host".into()},tx).unwrap());
            rx.recv().await.unwrap();host_rx.recv().await.unwrap();receive.push(rx);
        }
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
        assert_eq!(state.rooms.lock().unwrap().hosts["generated-host"].viewers.len(),5);
        state.remove(&host);
        assert!(matches!(receive[1].recv().await.unwrap(),Event::Ended{..}));
    }
    #[tokio::test]
    async fn stale_viewer_release_cannot_close_the_next_session() {
        let state = State {
            rooms: Arc::default(),
            turn: None,
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
