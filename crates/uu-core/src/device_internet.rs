//! Public-key device sessions above UU/self-hosted rendezvous. No shared key
//! file is required. A provisional data-only peer cannot read the desktop.
use crate::{
    config::Config,
    device_auth, device_book,
    device_channel::Bootstrap,
    internet::{self, Connection, Provider, Settings},
    media::{FrameSender, MediaStats},
    native::{self, HostStatus, SharingConfig},
    shared_host::Access,
};
use anyhow::{ensure, Context, Result};
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;

#[derive(Clone, Debug)]
pub enum EndpointStatus {
    Starting,
    Ready,
    Authenticating,
    Streaming,
    Error(String),
    Stopped,
}
impl EndpointStatus {
    pub fn label(&self) -> String {
        match self {
            Self::Starting => uu_i18n::text("正在连接服务…").into(),
            Self::Ready => uu_i18n::text("可连接").into(),
            Self::Authenticating => uu_i18n::text("正在验证设备…").into(),
            Self::Streaming => uu_i18n::text("正在串流").into(),
            Self::Error(e) => e.clone(),
            Self::Stopped => uu_i18n::text("已停止").into(),
        }
    }
}
async fn cancelled(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow_and_update() {
            return;
        }
        if stop.changed().await.is_err() {
            return;
        }
    }
}

pub(crate) async fn view(
    connection: Connection,
    settings: &Settings,
    config: &Config,
    frames: FrameSender,
    stats: Arc<MediaStats>,
    mut stop: watch::Receiver<bool>,
    input: Option<crate::native_input::NativeInputReceiver>,
) -> Result<()> {
    let store = crate::device_trust::TrustStore::open(config.at("device-trust"))?;
    let book = device_book::Store::open(config.at("device-trust"))?;
    let name = book.read()?.sharing.name;
    let ice = connection.ice.clone();
    stats.connection.set_service(match &settings.provider {
        Provider::Plugin(id) => crate::plugins::registry().get(id)?.descriptor().name,
        Provider::SelfHosted => uu_i18n::text("自建连接服务"),
    });
    let mut bootstrap = tokio::select! {
        result=Bootstrap::viewer(connection)=>result?,
        _=cancelled(&mut stop)=>return Ok(()),
    };
    let result = async {
        let authentication = device_auth::client(
            &mut bootstrap.io,
            &store,
            &name,
            &bootstrap.binding,
            settings.expected_peer.as_deref(),
            false,
            &stats.pairing,
        );
        let remote=tokio::select! {result=authentication=>result?,_=cancelled(&mut stop)=>return Ok(())};
        if let Some(profile) = &settings.profile {
            book.bind(profile, &remote.device.id())?;
        }
        tokio::select! {result=bootstrap.ready(false)=>result?,_=cancelled(&mut stop)=>return Ok(())};
        native::view_direct(&mut bootstrap, &ice, frames, stats.clone(), stop, input).await
    }
    .await;
    bootstrap.close().await;
    result
}

pub async fn serve_connection(
    mut connection: Connection,
    mut sharing: SharingConfig,
    access: &Access,
    mut stop: watch::Receiver<bool>,
    status: Option<watch::Sender<HostStatus>>,
    clipboard: Option<crate::native_lan::ClipboardFactory>,
) -> Result<native::HostReport> {
    let first = tokio::select! {
        result=connection.transport.receive()=>result?,
        _=cancelled(&mut stop)=>{connection.transport.close().await;return Ok(native::HostReport::default());},
    };
    ensure!(
        crate::device_channel::is_bootstrap(&first),
        "{}", uu_i18n::format!("控制端仍使用旧配对文件协议，请升级并通过短码完成设备配对"));
    let ice = connection.ice.clone();
    let mut bootstrap = tokio::select! {
        result=Bootstrap::host(connection,first)=>result?,
        _=cancelled(&mut stop)=>return Ok(native::HostReport::default()),
    };
    let result = async {
        let authentication = device_auth::host(
            &mut bootstrap.io,
            &access.store,
            &access.name,
            &bootstrap.binding,
            &access.pairing,
            None,
        );
        let mut authorized=tokio::select! {result=authentication=>result?,_=cancelled(&mut stop)=>return Ok(native::HostReport::default())};
        let peer = authorized.device.id();
        let lease=match access.claim_authenticated(&mut authorized) {Ok(lease)=>lease,Err(error)=>{bootstrap.reject_media().await;return Err(error);}};
        let cancellation = access.cancellation(&lease, &peer, Some(authorized.permissions), stop);
        sharing.allow_input &= authorized.permissions.control;
        sharing.session=Some(crate::shared_host::SessionContext{access:Arc::new(access.clone()),peer:peer.clone(),permissions:authorized.permissions});
        if let Some(factory) = clipboard {
            sharing.clipboard = Some(factory(&uuid::Uuid::new_v4().to_string())?);
        }
        bootstrap.ready(true).await?;
        let (progress,mut observe)=watch::channel(HostStatus::Starting);
        let mut disconnected=bootstrap.closed_signal();
        let serving = native::serve_direct_with_authority(
            &mut bootstrap, &ice, sharing, cancellation.receiver.clone(), Some(progress), access.sessions.input.clone(),
        );
        tokio::pin!(serving);
        let mut observing=true;
        let mut watching_connection=true;
        let result=loop {tokio::select! {
            result=&mut serving=>break result,
            _=cancelled(&mut disconnected),if watching_connection=>{watching_connection=false;lease.cancel();},
            changed=observe.changed(),if observing=>{
                if changed.is_err(){observing=false;}else{
                    let phase=observe.borrow_and_update().clone();lease.progress(phase.clone());
                    if let Some(status)=&status {status.send_replace(phase);}
                }
            }
        }};
        drop(cancellation);
        drop(lease);
        result
    }
    .await;
    bootstrap.close().await;
    result
}

/// Keep each configured endpoint published across stream disconnects. The
/// access context and capture defaults are shared with the IP listener.
pub async fn listen(
    settings: Settings,
    config: Config,
    sharing: SharingConfig,
    access: Arc<Access>,
    stop: watch::Receiver<bool>,
    status: watch::Sender<EndpointStatus>,
    stream_status: watch::Sender<HostStatus>,
    clipboard: Option<crate::native_lan::ClipboardFactory>,
) {
    listen_concurrent(settings, config, sharing, access, stop, status, stream_status, clipboard).await;
}

enum Listener { Plugin(Box<dyn crate::plugins::Listener>), SelfHosted(internet::self_hosted::Listener) }
impl Listener {
    async fn online(settings:&Settings,config:&Config)->Result<Self> {
        match &settings.provider {
            Provider::Plugin(id)=>Ok(Self::Plugin(crate::plugins::registry().get(id)?.listen(config,settings.development_relay).await?)),
            Provider::SelfHosted=>Ok(Self::SelfHosted(internet::self_hosted::Listener::online(&settings.self_hosted()?,&settings.device).await?)),
        }
    }
    async fn accept(&mut self)->Result<crate::plugins::Incoming>{match self{Self::Plugin(listener)=>listener.accept_session().await,Self::SelfHosted(listener)=>listener.accept().await.map(crate::plugins::Incoming::Native)}}
    async fn close(&mut self){match self{Self::Plugin(listener)=>listener.close().await,Self::SelfHosted(listener)=>listener.close().await}}
}

async fn listen_concurrent(settings: Settings, config: Config, sharing: SharingConfig, access: Arc<Access>, mut stop: watch::Receiver<bool>,
    status: watch::Sender<EndpointStatus>, stream_status: watch::Sender<HostStatus>, clipboard: Option<crate::native_lan::ClipboardFactory>) {
    while !*stop.borrow() {
        status.send_replace(EndpointStatus::Starting);
        let opening=async { tokio::time::timeout(Duration::from_secs(20),Listener::online(&settings,&config)).await.context(uu_i18n::text("连接服务上线超时"))? };
        let opened=tokio::select! { result=opening=>result, _=cancelled(&mut stop)=>break };
        let mut listener=match opened {
            Ok(listener)=>listener,
            Err(error)=>{status.send_replace(EndpointStatus::Error(format!("{error:#}")));tokio::select!{_=cancelled(&mut stop)=>break,_=tokio::time::sleep(Duration::from_secs(3))=>{}}continue;}
        };
        status.send_replace(EndpointStatus::Ready);
        let mut sessions=tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                _=cancelled(&mut stop)=>break,
                result=sessions.join_next(),if !sessions.is_empty()=>{
                    if let Some(Ok(Err(error)))=result {tracing::warn!(%error,"native internet viewer ended");}
                },
                connection=listener.accept()=>{
                    let connection=match connection {Ok(connection)=>connection,Err(error)=>{status.send_replace(EndpointStatus::Error(format!("{error:#}")));break;}};
                    if sessions.len()>=crate::shared_host::MAX_SESSIONS {
                        if let crate::plugins::Incoming::Native(mut connection)=connection {connection.transport.close().await;}
                        continue;
                    }
                    let access=access.clone();let sharing=sharing.clone();let stop=stop.clone();let clipboard=clipboard.clone();let progress=stream_status.clone();
                    sessions.spawn(async move {
                        let crate::plugins::Incoming::Native(connection)=connection else {
                            if let crate::plugins::Incoming::Protocol(session)=connection {
                                return session.serve(crate::plugins::HostContext {sharing,sessions:access.sessions.clone(),stop,progress,clipboard}).await;
                            }
                            unreachable!()
                        };
                        let (phase,mut updates)=watch::channel(HostStatus::Starting);
                        let serving=serve_connection(connection,sharing,&access,stop,Some(phase),clipboard);
                        tokio::pin!(serving);
                        let mut observing=true;
                        loop {tokio::select! {
                            result=&mut serving=>return result,
                            result=updates.changed(),if observing=>{if result.is_err(){observing=false;}else{progress.send_replace(updates.borrow_and_update().clone());}}
                        }}
                    });
                },
            }
        }
        listener.close().await;
        let draining=async {while sessions.join_next().await.is_some(){}};
        if tokio::time::timeout(Duration::from_secs(5),draining).await.is_err(){sessions.abort_all();while sessions.join_next().await.is_some(){}}
        if !*stop.borrow(){tokio::time::sleep(Duration::from_millis(200)).await;}
    }
    status.send_replace(EndpointStatus::Stopped);
}
