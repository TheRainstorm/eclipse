//! Self-hosted WSS provider. Uses the same authenticated native session as UU.
use super::{
    server::{read_token, ws_config, Event, Request},
    Connection, Transport,
};
use anyhow::{bail, ensure, Context, Result};
use futures_util::{SinkExt, StreamExt};
use rustls_pki_types::{pem::PemObject, CertificateDer};
use serde_json::Value;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    net::TcpStream,
    time::{timeout, Instant},
};
use tokio_tungstenite::{
    tungstenite::{client::IntoClientRequest, Message},
    Connector, MaybeTlsStream, WebSocketStream,
};

#[derive(Clone, Debug)]
pub struct Config {
    pub url: String,
    pub token_file: PathBuf,
    /// Trust a private certificate for deployments without a domain. Public
    /// certificates use the normal system/webpki trust path when omitted.
    pub ca_file: Option<PathBuf>,
}
impl Config {
    /// Check local inputs before saving a service. This makes no connection.
    pub fn validate_local(&self)->Result<()> {
        let url=url::Url::parse(&self.url).context(uu_i18n::text("服务器地址格式无效"))?;
        ensure!(url.scheme()=="wss" && url.host_str().is_some() && url.username().is_empty() && url.password().is_none(),uu_i18n::text("服务器需要有效的 WSS 地址，访问凭据请单独填写"));
        let _=read_token(&self.token_file).context(uu_i18n::text("无法读取服务器访问凭据文件"))?;
        if let Some(path)=&self.ca_file {
            let mut roots=rustls::RootCertStore::empty();
            for cert in CertificateDer::pem_file_iter(path).context(uu_i18n::text("无法读取服务器证书"))? {roots.add(cert?)?;}
            ensure!(!roots.is_empty(),"{}", uu_i18n::format!("证书文件中没有有效的 PEM 证书"));
        }
        Ok(())
    }
}
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
async fn open(config: &Config, request: Request) -> Result<Socket> {
    crate::install_crypto_provider();
    ensure!(
        config.url.starts_with("wss://"),
        "self-hosted signaling requires WSS"
    );
    let token = read_token(&config.token_file)?;
    let mut http = config
        .url
        .clone()
        .into_client_request()
        .context("invalid signaling URL")?;
    http.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse()?);
    let connector = if let Some(ca) = &config.ca_file {
        let mut roots = rustls::RootCertStore::empty();
        for cert in CertificateDer::pem_file_iter(ca)? {
            roots.add(cert?)?;
        }
        Some(Connector::Rustls(Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        )))
    } else {
        None
    };
    let (mut socket, _) = timeout(
        Duration::from_secs(15),
        // Signaling exchanges small request/response messages; do not defer
        // them behind TCP's small-packet coalescing and delayed ACKs.
        tokio_tungstenite::connect_async_tls_with_config(http, Some(ws_config()), true, connector),
    )
    .await
    .context("self-hosted signaling connection timed out")??;
    socket
        .send(Message::Text(serde_json::to_string(&request)?.into()))
        .await?;
    Ok(socket)
}
async fn receive(socket: &mut Socket) -> Result<Event> {
    loop {
        match timeout(Duration::from_secs(35), socket.next())
            .await
            .context("self-hosted signaling heartbeat timed out")?
            .context("self-hosted signaling disconnected")??
        {
            Message::Text(text) => {
                return serde_json::from_str(&text)
                    .map_err(|_| anyhow::anyhow!("invalid signaling response"))
            }
            Message::Ping(payload) => socket.send(Message::Pong(payload)).await?,
            Message::Pong(_) => {}
            Message::Close(_) => bail!("self-hosted signaling session ended"),
            _ => bail!("unexpected signaling frame"),
        }
    }
}
pub struct Host {
    socket: Option<Socket>,
}
pub use super::server::Device;
/// The service credential reveals presence, never desktop authorization.
pub async fn directory(config:&Config)->Result<Vec<Device>> {
    let mut socket=open(config,Request::Directory{version:1}).await?;
    let result=match receive(&mut socket).await? {
        Event::Directory{devices}=>{
            ensure!(devices.len()<=128 && devices.iter().all(|d| !d.device.is_empty() && d.device.len()<=80
                && d.device.bytes().all(|b|b.is_ascii_alphanumeric()||b"-_".contains(&b))
                && !d.name.trim().is_empty() && d.name.chars().count()<=64 && !d.name.chars().any(char::is_control)),"invalid device directory");
            Ok(devices)
        },
        _=>bail!("connection service does not support device discovery"),
    };
    let _=socket.close(None).await;result
}
async fn register(config:&Config,device:&str,name:&str)->Result<Socket> {
    let mut version=3;
    loop {
        let mut socket=open(config,Request::Host{version,device:device.into()}).await?;
        match receive(&mut socket).await? {
            Event::Ready=>{
                if version>=3 {
                    let label: String=name.trim().chars().filter(|c|!c.is_control()).take(64).collect();
                    let label=if label.is_empty(){device.chars().take(64).collect()}else{label};
                    socket.send(Message::Text(serde_json::to_string(&Request::Publish{name:label})?.into())).await?;
                }
                return Ok(socket);
            },
            Event::Error{code} if version>1 && code=="unsupported_version"=>{let _=socket.close(None).await;version-=1;},
            Event::Error{code}=>bail!("signaling registration rejected: {code}"),
            _=>bail!("unexpected signaling registration response"),
        }
    }
}
fn session(event:Event)->Result<(String,crate::direct_ice::InternetIce)> {
    match event {
        Event::Connected{session,stun}=>Ok((session,crate::direct_ice::InternetIce::new(stun)?)),
        Event::Session{session,stun,turn}=>Ok((session,crate::direct_ice::InternetIce::automatic(stun,turn)?)),
        Event::Error{code}=>bail!("signaling connection rejected: {code}"),
        _=>bail!("unexpected signaling session response"),
    }
}
/// Read presence using the service credential, without creating a viewer or
/// asking the host to start a media/authentication session. Old servers fail
/// this request; callers must display unknown rather than offline in that case.
pub async fn presence(config:&Config,devices:&[String])->Result<Vec<String>> {
    ensure!(!devices.is_empty() && devices.len()<=128,"invalid presence query");
    let mut socket=open(config,Request::Probe{version:1,devices:devices.to_vec()}).await?;
    let result=match receive(&mut socket).await? {
        Event::Presence{online}=>{
            ensure!(online.len()<=devices.len() && online.iter().all(|id|devices.contains(id)),"invalid presence response");
            Ok(online)
        },
        _=>bail!("{}", uu_i18n::format!("连接服务尚不支持在线状态查询")),
    };
    let _=socket.close(None).await;
    result
}
impl Host {
    pub async fn online(config: &Config, device: &str) -> Result<Self> {
        Ok(Self { socket: Some(register(config,device,device).await?) })
    }
    pub async fn accept(&mut self) -> Result<Connection> {
        let socket = self
            .socket
            .as_mut()
            .context("host session already consumed")?;
        loop {
            match receive(socket).await? {
                event @ (Event::Connected { .. } | Event::Session { .. }) => {
                    let (session,ice)=session(event)?;
                    return Ok(Connection {
                        transport: Box::new(SelfTransport {
                            socket: self.socket.take().unwrap(),
                            session: session.clone(),
                            seen: Instant::now(),
                        }),
                        session,
                        ice,
                    });
                }
                Event::Error { code } => bail!("signaling connection rejected: {code}"),
                _ => bail!("unexpected signaling host event"),
            }
        }
    }
    pub async fn close(&mut self) {
        if let Some(socket) = &mut self.socket {
            let _ = socket.close(None).await;
        }
    }
}
pub async fn join(config: &Config, device: &str) -> Result<Connection> {
    let mut version=2;
    let (socket,event)=loop {
      let mut socket = open(
        config,
        Request::Join {
            version,
            device: device.to_owned(),
        },
    )
      .await?;
      let event=receive(&mut socket).await?;
      if matches!(&event,Event::Error{code}if version==2 && code=="unsupported_version") {
          let _=socket.close(None).await;version=1;continue;
      }
      break (socket,event);
    };
    let (session,ice)=session(event)?;
    Ok(Connection {
                transport: Box::new(SelfTransport {
                    socket,
                    session: session.clone(),
                    seen: Instant::now(),
                }),
                session,
                ice,
    })
}
struct SelfTransport {
    socket: Socket,
    session: String,
    seen: Instant,
}
#[async_trait::async_trait]
impl Transport for SelfTransport {
    async fn send(&mut self, body: Value) -> Result<()> {
        self.socket
            .send(Message::Text(
                serde_json::to_string(&Request::Signal {
                    session: self.session.clone(),
                    body,
                })?
                .into(),
            ))
            .await?;
        Ok(())
    }
    async fn receive(&mut self) -> Result<Value> {
        loop {
            let event = receive(&mut self.socket).await?;
            self.seen = Instant::now();
            match event {
                Event::Signal { session, body } => {
                    ensure!(session == self.session, "signal belongs to another session");
                    return Ok(body);
                }
                Event::Ended { session } if session == self.session => {
                    bail!("native peer disconnected")
                }
                Event::Error { code } => bail!("signaling error: {code}"),
                _ => bail!("unexpected signaling session event"),
            }
        }
    }
    async fn close(&mut self) {
        let request = Request::Leave {
            session: self.session.clone(),
        };
        let _ = timeout(Duration::from_secs(1), async {
            self.socket
                .send(Message::Text(serde_json::to_string(&request)?.into()))
                .await?;
            self.socket.close(None).await?;
            Ok::<_, anyhow::Error>(())
        })
        .await;
    }
}

/// One published device, with independently routed signaling for each viewer.
/// Only SDP/ICE and disconnect notifications traverse this socket.
pub struct Listener {
    incoming: tokio::sync::mpsc::Receiver<Connection>,
    task: tokio::task::JoinHandle<()>,
}
// Pending delivery only; authenticated sessions do not retain queue capacity.
const INCOMING_QUEUE_CAPACITY: usize = 4;
impl Listener {
    pub async fn online(config: &Config, device: &str) -> Result<Self> {
        Self::online_named(config,device,device).await
    }
    pub async fn online_named(config:&Config,device:&str,name:&str)->Result<Self> {
        // Version 2 opts a host into multiplexing. A v1 host is kept at one
        // viewer by new servers, and an old server can explicitly decline v2.
        let mut socket = register(config,device,name).await?;
        let (outgoing, mut requests) = tokio::sync::mpsc::channel::<Request>(128);
        let (accepted, incoming) = tokio::sync::mpsc::channel(INCOMING_QUEUE_CAPACITY);
        let task = tokio::spawn(async move {
            let mut sessions = std::collections::HashMap::<String, tokio::sync::mpsc::Sender<Value>>::new();
            loop {
                tokio::select! {
                    request=requests.recv()=>{
                        let Some(request)=request else{break;};
                        if let Request::Leave{session}=&request {sessions.remove(session);}
                        let Ok(text)=serde_json::to_string(&request) else{break;};
                        if socket.send(Message::Text(text.into())).await.is_err(){break;}
                    },
                    event=receive(&mut socket)=>{
                        match event {
                            Ok(event @ (Event::Connected{..} | Event::Session{..}))=>{
                                let Ok((session,ice))=session(event) else{break;};
                                if sessions.contains_key(&session) {break;}
                                let (send,receive)=tokio::sync::mpsc::channel(64);
                                sessions.insert(session.clone(),send);
                                let connection=Connection{transport:Box::new(RoutedTransport{outgoing:outgoing.clone(),incoming:receive,session:session.clone(),closed:false}),session:session.clone(),ice};
                                if accepted.try_send(connection).is_err(){
                                    sessions.remove(&session);
                                    let request=Request::Leave{session};
                                    if socket.send(Message::Text(serde_json::to_string(&request).unwrap().into())).await.is_err(){break;}
                                }
                            },
                            Ok(Event::Signal{session,body})=>{
                                if let Some(target)=sessions.get(&session) {
                                    if target.try_send(body).is_err() {
                                        sessions.remove(&session);
                                        let request=Request::Leave{session};
                                        if socket.send(Message::Text(serde_json::to_string(&request).unwrap().into())).await.is_err(){break;}
                                    }
                                }
                            },
                            Ok(Event::Ended{session})=>{sessions.remove(&session);},
                            _=>break,
                        }
                    },
                }
            }
            let _ = timeout(Duration::from_secs(1), socket.close(None)).await;
        });
        Ok(Self { incoming, task })
    }
    pub async fn accept(&mut self) -> Result<Connection> {
        self.incoming.recv().await.context("self-hosted signaling listener disconnected")
    }
    pub async fn close(&mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
        self.incoming.close();
    }
}
impl Drop for Listener { fn drop(&mut self) { self.task.abort(); } }
struct RoutedTransport {
    outgoing: tokio::sync::mpsc::Sender<Request>,
    incoming: tokio::sync::mpsc::Receiver<Value>,
    session: String,
    closed: bool,
}
#[async_trait::async_trait]
impl Transport for RoutedTransport {
    async fn send(&mut self, body: Value) -> Result<()> {
        ensure!(!self.closed, "signaling session closed");
        timeout(Duration::from_secs(1), self.outgoing.send(Request::Signal{session:self.session.clone(),body})).await.context("signaling queue timed out")?.context("signaling listener closed")
    }
    async fn receive(&mut self) -> Result<Value> { self.incoming.recv().await.context("native peer disconnected") }
    async fn close(&mut self) {
        if !self.closed {
            self.closed=true;
            let _=timeout(Duration::from_secs(1),self.outgoing.send(Request::Leave{session:self.session.clone()})).await;
            self.incoming.close();
        }
    }
}
impl Drop for RoutedTransport {
    fn drop(&mut self) {
        if !self.closed { let _=self.outgoing.try_send(Request::Leave{session:self.session.clone()}); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn modern_clients_downgrade_only_on_explicit_unsupported_version() {
        for maximum in [1,2] {
            crate::install_crypto_provider();
            let root=std::env::temp_dir().join(format!("eclipse-wss-legacy-{}",uuid::Uuid::new_v4()));std::fs::create_dir(&root).unwrap();
            let certificate=rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
            let ca=root.join("ca.pem");let token=root.join("token");
            std::fs::write(&ca,certificate.cert.pem()).unwrap();std::fs::write(&token,"00".repeat(32)).unwrap();
            let tls=rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(vec![certificate.cert.der().clone()],
                rustls_pki_types::PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der()).into()).unwrap();
            let acceptor=tokio_rustls::TlsAcceptor::from(Arc::new(tls));
            let tcp=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=tcp.local_addr().unwrap();
            let server=tokio::spawn(async move {
                for expected in (maximum..=3).rev() {
                    let (tcp,_)=tcp.accept().await.unwrap();
                    let mut ws=tokio_tungstenite::accept_async(acceptor.accept(tcp).await.unwrap()).await.unwrap();
                    let first=receive_request(&mut ws).await;
                    assert!(matches!(first,Request::Host{version,..}if version==expected));
                    if expected>maximum {
                        ws.send(Message::Text(serde_json::to_string(&Event::Error{code:"unsupported_version".into()}).unwrap().into())).await.unwrap();continue;
                    }
                    ws.send(Message::Text(serde_json::to_string(&Event::Ready).unwrap().into())).await.unwrap();
                    ws.send(Message::Text(serde_json::to_string(&Event::Connected{session:"legacy-session".into(),stun:vec![]}).unwrap().into())).await.unwrap();
                    assert!(matches!(receive_request(&mut ws).await,Request::Signal{..}),"legacy hosts never receive Publish or new ICE fields");
                }
            });
            let config=Config{url:format!("wss://{address}/v1/signal"),token_file:token,ca_file:Some(ca)};
            let mut listener=Listener::online_named(&config,"legacy-host","Named host").await.unwrap();
            let mut connection=timeout(Duration::from_secs(3),listener.accept()).await.unwrap().unwrap();
            assert!(connection.ice.configuration().ice_servers.is_empty());
            connection.transport.send(serde_json::json!({"legacy":true})).await.unwrap();
            timeout(Duration::from_secs(3),server).await.unwrap().unwrap();
            listener.close().await;std::fs::remove_dir_all(root).unwrap();
        }
        async fn receive_request(ws:&mut WebSocketStream<tokio_rustls::server::TlsStream<TcpStream>>)->Request {
            let message=timeout(Duration::from_secs(3),ws.next()).await.unwrap().unwrap().unwrap();
            serde_json::from_str(message.to_text().unwrap()).unwrap()
        }
    }
    #[tokio::test]
    async fn two_outbound_hosts_discover_and_connect_in_both_directions() {
        crate::install_crypto_provider();
        let root=std::env::temp_dir().join(format!("eclipse-wss-bidirectional-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let certificate=rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let ca_file=root.join("ca.pem");let key_file=root.join("key.pem");let token_file=root.join("token");
        std::fs::write(&ca_file,certificate.cert.pem()).unwrap();
        std::fs::write(&key_file,certificate.key_pair.serialize_pem()).unwrap();
        let group = super::super::server::groups::Group::new("Generated bidirectional group".into()).unwrap();
        let group_id = group.id;
        std::fs::write(&token_file, group.token.as_bytes()).unwrap();
        let groups_file = root.join("groups.json");
        super::super::server::groups::Groups { version: 1, groups: vec![group] }.save(&groups_file).unwrap();
        let reservation=std::net::TcpListener::bind("127.0.0.1:0").unwrap();let address=reservation.local_addr().unwrap();drop(reservation);
        let (stop,rx)=tokio::sync::watch::channel(false);
        let serving=tokio::spawn(super::super::server::serve(super::super::server::Config {
            bind:address,certificate:ca_file.clone(),private_key:key_file,groups_file,stun:vec![],turn:None,
        },rx));
        let config=Config{url:super::super::enrollment::group_url(&format!("wss://{address}/v1/signal"), &group_id).unwrap(),token_file,ca_file:Some(ca_file)};
        let mut a=timeout(Duration::from_secs(3),async {
            loop {match Listener::online_named(&config,"device-a","Generated A").await {
                Ok(listener)=>break listener,Err(_)=>tokio::time::sleep(Duration::from_millis(10)).await,
            }}
        }).await.unwrap();
        let mut b=Listener::online_named(&config,"device-b","Generated B").await.unwrap();
        timeout(Duration::from_secs(3),async {
            loop {
                let devices=directory(&config).await.unwrap();
                if devices.len()==2 && devices[0].name=="Generated A" && devices[1].name=="Generated B" {break;}
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.unwrap();
        let mut b_to_a=join(&config,"device-a").await.unwrap();
        let mut at_a=a.accept().await.unwrap();
        let mut a_to_b=join(&config,"device-b").await.unwrap();
        let mut at_b=b.accept().await.unwrap();
        for (viewer,host,marker) in [(&mut b_to_a,&mut at_a,1),(&mut a_to_b,&mut at_b,2)] {
            assert_eq!(viewer.session,host.session);
            viewer.transport.send(serde_json::json!({"from_viewer":marker})).await.unwrap();
            assert_eq!(host.transport.receive().await.unwrap()["from_viewer"],marker);
            host.transport.send(serde_json::json!({"from_host":marker})).await.unwrap();
            assert_eq!(viewer.transport.receive().await.unwrap()["from_host"],marker);
            viewer.transport.close().await;host.transport.close().await;
        }
        a.close().await;b.close().await;stop.send_replace(true);serving.await.unwrap().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn six_wss_routes_survive_a_full_pending_delivery_queue() {
        crate::install_crypto_provider();
        let root=std::env::temp_dir().join(format!("eclipse-wss-routes-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let certificate=rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let ca_file=root.join("ca.pem");
        let token_file=root.join("token");
        std::fs::write(&ca_file,certificate.cert.pem()).unwrap();
        std::fs::write(&token_file,"00".repeat(32)).unwrap();
        let tls=rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(
            vec![certificate.cert.der().clone()],
            rustls_pki_types::PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der()).into(),
        ).unwrap();
        let acceptor=tokio_rustls::TlsAcceptor::from(Arc::new(tls));
        let tcp=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address=tcp.local_addr().unwrap();
        let (commands,mut events)=tokio::sync::mpsc::channel::<Event>(16);
        let (observed,mut requests)=tokio::sync::mpsc::channel::<Request>(16);
        let server=tokio::spawn(async move {
            let (tcp,_)=tcp.accept().await.unwrap();
            let mut socket=tokio_tungstenite::accept_async(acceptor.accept(tcp).await.unwrap()).await.unwrap();
            let registration=socket.next().await.unwrap().unwrap();
            assert!(matches!(serde_json::from_str::<Request>(registration.to_text().unwrap()).unwrap(),Request::Host{version:3,..}));
            socket.send(Message::Text(serde_json::to_string(&Event::Ready).unwrap().into())).await.unwrap();
            loop {tokio::select! {
                event=events.recv()=>{
                    let Some(event)=event else{break;};
                    if socket.send(Message::Text(serde_json::to_string(&event).unwrap().into())).await.is_err(){break;}
                },
                message=socket.next()=>match message {
                    Some(Ok(Message::Text(text)))=>{
                        if observed.send(serde_json::from_str(&text).unwrap()).await.is_err(){break;}
                    },
                    _=>break,
                },
            }}
        });
        let mut listener=Listener::online(&Config{url:format!("wss://{address}/v1/signal"),token_file,ca_file:Some(ca_file)},"generated-host").await.unwrap();
        for n in 1..=5 {
            commands.send(Event::Connected{session:format!("session-{n}"),stun:vec![]}).await.unwrap();
        }
        timeout(Duration::from_secs(3),async {
            loop {if matches!(requests.recv().await,Some(Request::Leave{session}) if session=="session-5"){break;}}
        }).await.unwrap();
        let mut connections=Vec::new();
        for n in 1..=4 {
            let connection=timeout(Duration::from_secs(3),listener.accept()).await.unwrap().unwrap();
            assert_eq!(connection.session,format!("session-{n}"));
            connections.push(connection);
        }
        for n in 6..=7 {
            commands.send(Event::Connected{session:format!("session-{n}"),stun:vec![]}).await.unwrap();
            connections.push(timeout(Duration::from_secs(3),listener.accept()).await.unwrap().unwrap());
        }
        assert_eq!(connections.len(),6);
        for (n,connection) in connections.iter_mut().enumerate() {
            commands.send(Event::Signal{session:connection.session.clone(),body:serde_json::json!({"route":n})}).await.unwrap();
            assert_eq!(timeout(Duration::from_secs(3),connection.transport.receive()).await.unwrap().unwrap()["route"],n);
            connection.transport.close().await;
        }
        listener.close().await;
        drop(commands);
        timeout(Duration::from_secs(3),server).await.unwrap().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
