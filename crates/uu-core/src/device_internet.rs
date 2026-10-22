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
            Self::Starting => "正在连接服务…".into(),
            Self::Ready => "可连接".into(),
            Self::Authenticating => "正在验证设备…".into(),
            Self::Streaming => "正在串流".into(),
            Self::Error(e) => e.clone(),
            Self::Stopped => "已停止".into(),
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
    stats.connection.set_service(match settings.provider {
        Provider::Uu => "UU 连接服务",
        Provider::SelfHosted => "自建连接服务",
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

async fn accept(
    settings: &Settings,
    config: &Config,
    status: &watch::Sender<EndpointStatus>,
) -> Result<Connection> {
    match settings.provider {
        Provider::Uu => {
            let mut host = tokio::time::timeout(
                Duration::from_secs(20),
                internet::uu::Host::online(config, settings.development_relay),
            )
            .await
            .context("UU 主机上线超时")??;
            status.send_replace(EndpointStatus::Ready);
            let result = host.accept().await;
            host.close().await;
            result
        }
        Provider::SelfHosted => {
            let service = settings.self_hosted()?;
            let mut host = tokio::time::timeout(
                Duration::from_secs(20),
                internet::self_hosted::Host::online(&service, &settings.device),
            )
            .await
            .context("自建主机上线超时")??;
            status.send_replace(EndpointStatus::Ready);
            host.accept().await
        }
    }
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
        "控制端仍使用旧配对文件协议，请升级并通过短码完成设备配对"
    );
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
        let authorized=tokio::select! {result=authentication=>result?,_=cancelled(&mut stop)=>return Ok(native::HostReport::default())};
        let peer = authorized.device.id();
        let lease=match access.claim(&peer) {Ok(lease)=>lease,Err(error)=>{bootstrap.reject_media().await;return Err(error);}};
        let cancellation = access.cancellation(&lease, &peer, Some(authorized.permissions), stop);
        sharing.allow_input &= authorized.permissions.control;
        if let Some(factory) = clipboard {
            sharing.clipboard = Some(factory(&uuid::Uuid::new_v4().to_string())?);
        }
        bootstrap.ready(true).await?;
        let result = native::serve_direct(
            &mut bootstrap,
            &ice,
            sharing,
            cancellation.receiver.clone(),
            status,
        )
        .await;
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
    mut stop: watch::Receiver<bool>,
    status: watch::Sender<EndpointStatus>,
    stream_status: watch::Sender<HostStatus>,
    clipboard: Option<crate::native_lan::ClipboardFactory>,
) {
    while !*stop.borrow() {
        status.send_replace(EndpointStatus::Starting);
        let connection = tokio::select! {
            result=accept(&settings,&config,&status)=>result,
            _=cancelled(&mut stop)=>break,
        };
        let outcome = match connection {
            Ok(connection) => {
                status.send_replace(EndpointStatus::Authenticating);
                let (phase, mut observe) = watch::channel(HostStatus::Starting);
                let serving = serve_connection(
                    connection,
                    sharing.clone(),
                    &access,
                    stop.clone(),
                    Some(phase),
                    clipboard.clone(),
                );
                tokio::pin!(serving);
                let mut observing = true;
                let result = loop {
                    tokio::select! {
                        result=&mut serving=>break result,
                        _=cancelled(&mut stop)=>break match tokio::time::timeout(Duration::from_secs(5),&mut serving).await {
                            Ok(result)=>result,Err(_)=>Err(anyhow::anyhow!("共享退出超时")),
                        },
                        changed=observe.changed(),if observing=>{
                            if changed.is_err(){observing=false;}else{
                                let phase=observe.borrow_and_update().clone();
                                if matches!(phase,HostStatus::Streaming{..}){status.send_replace(EndpointStatus::Streaming);}
                                stream_status.send_replace(phase);
                            }
                        }
                    }
                };
                result.map(|_| ())
            }
            Err(error) => Err(error),
        };
        if let Err(error) = outcome {
            status.send_replace(EndpointStatus::Error(format!("{error:#}")));
            tokio::select! {_=cancelled(&mut stop)=>break,_=tokio::time::sleep(Duration::from_secs(3))=>{}}
        } else {
            tokio::select! {_=cancelled(&mut stop)=>break,_=tokio::time::sleep(Duration::from_millis(200))=>{}}
        }
    }
    status.send_replace(EndpointStatus::Stopped);
}
