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
        tokio_tungstenite::connect_async_tls_with_config(http, Some(ws_config()), false, connector),
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
        let mut socket = open(
            config,
            Request::Host {
                version: 1,
                device: device.to_owned(),
            },
        )
        .await?;
        match receive(&mut socket).await? {
            Event::Ready => Ok(Self {
                socket: Some(socket),
            }),
            Event::Error { code } => bail!("signaling registration rejected: {code}"),
            _ => bail!("unexpected signaling registration response"),
        }
    }
    pub async fn accept(&mut self) -> Result<Connection> {
        let socket = self
            .socket
            .as_mut()
            .context("host session already consumed")?;
        loop {
            match receive(socket).await? {
                Event::Connected { session, stun } => {
                    let ice = crate::direct_ice::InternetIce::new(stun)?;
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
    let mut socket = open(
        config,
        Request::Join {
            version: 1,
            device: device.to_owned(),
        },
    )
    .await?;
    match receive(&mut socket).await? {
        Event::Connected { session, stun } => {
            let ice = crate::direct_ice::InternetIce::new(stun)?;
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
        Event::Error { code } => bail!("signaling connection rejected: {code}"),
        _ => bail!("unexpected signaling join response"),
    }
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
impl Listener {
    pub async fn online(config: &Config, device: &str) -> Result<Self> {
        // Version 2 opts a host into multiplexing. A v1 host is kept at one
        // viewer by new servers, and an old server can explicitly decline v2.
        let mut socket = {
            let mut version=2;
            loop {
                let mut candidate=open(config,Request::Host{version,device:device.into()}).await?;
                match receive(&mut candidate).await? {
                    Event::Ready=>{tracing::info!(version,multiplexed=version==2,"self-hosted signaling registered");break candidate;}
                    Event::Error{code} if version==2&&code=="unsupported_version"=>{let _=candidate.close(None).await;version=1;},
                    Event::Error{code}=>bail!("signaling registration rejected: {code}"),
                    _=>bail!("unexpected signaling registration response"),
                }
            }
        };
        let (outgoing, mut requests) = tokio::sync::mpsc::channel::<Request>(128);
        let (accepted, incoming) = tokio::sync::mpsc::channel(crate::shared_host::MAX_SESSIONS);
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
                            Ok(Event::Connected{session,stun})=>{
                                let Ok(ice)=crate::direct_ice::InternetIce::new(stun) else{break;};
                                if sessions.len()>=crate::shared_host::MAX_SESSIONS || sessions.contains_key(&session) {break;}
                                let (send,receive)=tokio::sync::mpsc::channel(64);
                                sessions.insert(session.clone(),send);
                                let connection=Connection{transport:Box::new(RoutedTransport{outgoing:outgoing.clone(),incoming:receive,session:session.clone(),closed:false}),session,ice};
                                if accepted.try_send(connection).is_err(){break;}
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
