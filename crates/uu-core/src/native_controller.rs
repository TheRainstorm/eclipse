//! Native viewer adapter for the shared GUI session handle. No interoperability API.
use crate::{
    controller::{ControllerEvent, ControllerHandle},
    media::{frame_channel, FrameSender, MediaStats},
    native_pairing::Invitation,
};
use anyhow::{Context, Result};

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{
    mpsc::{unbounded_channel, UnboundedReceiver},
    watch,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage { PreparingHost, ConnectingMedia, WaitingVideo, Streaming }
impl Stage {
    fn message(self) -> &'static str {
        match self {
            Self::PreparingHost => uu_i18n::text("正在等待被控端共享屏幕并准备编码…"),
            Self::ConnectingMedia => uu_i18n::text("正在建立媒体连接…"),
            Self::WaitingVideo => uu_i18n::text("正在等待首帧…"),
            Self::Streaming => uu_i18n::text("原生串流已连接"),
        }
    }
}

/// Latest connection phase, without queuing old progress behind media work.
pub struct Progress(watch::Sender<Option<Stage>>);
impl Default for Progress {
    fn default() -> Self { Self(watch::channel(None).0) }
}
impl Progress {
    pub(crate) fn set(&self, stage: Stage) { self.0.send_replace(Some(stage)); }
    pub(crate) fn subscribe(&self) -> watch::Receiver<Option<Stage>> { self.0.subscribe() }
}

pub fn spawn(invitation_file: PathBuf, stats: Arc<MediaStats>) -> Result<ControllerHandle> {
    spawn_with_input(invitation_file,stats,false)
}
pub fn spawn_with_input(invitation_file: PathBuf, stats: Arc<MediaStats>,requested:bool) -> Result<ControllerHandle> {
    spawn_source(Source::Invitation(invitation_file),stats,requested)
}
pub fn spawn_device(devices_file:PathBuf,id:String,stats:Arc<MediaStats>,requested:bool)->Result<ControllerHandle>{
    anyhow::ensure!(uuid::Uuid::parse_str(&id).is_ok(),"{}", uu_i18n::format!("无效的原生设备编号"));
    spawn_source(Source::Device{devices_file,id},stats,requested)
}
pub fn spawn_internet(settings:crate::internet::Settings,config:crate::Config,stats:Arc<MediaStats>,requested:bool)->Result<ControllerHandle> {
    spawn_source(Source::Internet{settings,config},stats,requested)
}
pub fn spawn_profile(launch:crate::device_book::Launch,config:crate::Config,stats:Arc<MediaStats>,requested:bool)->Result<ControllerHandle> {
    spawn_source(Source::Profile{launch,config},stats,requested)
}
enum Source {Invitation(PathBuf),Device{devices_file:PathBuf,id:String},Internet{settings:crate::internet::Settings,config:crate::Config},Profile{launch:crate::device_book::Launch,config:crate::Config}}
impl Source {
    async fn resolve(&self)->Result<Invitation>{match self{
        Self::Invitation(path)=>Invitation::load(path),
        Self::Internet{..}|Self::Profile{..}=>anyhow::bail!("profile sessions resolve their own connection"),
        Self::Device{devices_file,id}=>{
            let book=crate::native_lan_store::Devices::open(devices_file)?;
            let device=book.paired(id).context(uu_i18n::text("该原生设备尚未配对，请返回设备页重新配对"))?;
            crate::native_lan::connect(device,&book.client_id).await
        }
    }}
}
fn spawn_source(source:Source,stats:Arc<MediaStats>,requested:bool)->Result<ControllerHandle>{
    stats.capabilities.set(0);
    stats.clipboard.enabled.store(false, Ordering::Release);
    stats.clipboard.available.store(false, Ordering::Release);
    let (events, event_rx) = std::sync::mpsc::channel();
    let (frames, frame_rx) = frame_channel();
    let (input, input_rx) = unbounded_channel();
    let (settings, settings_rx) = unbounded_channel();
    let (close, close_rx) = unbounded_channel();
    let (native_input,native_rx)=crate::native_input::channel(requested);
    let stopped = Arc::new(AtomicBool::new(false));
    let worker = Worker {
        source,
        events,
        frames,
        input: input_rx,
        settings: settings_rx,
        close: close_rx,
        stats: stats.clone(),
        stopped: stopped.clone(),
        native:Some(native_rx),
    };
    std::thread::Builder::new()
        .name("native-viewer".into())
        .spawn(move || worker.run())
        .context("cannot start native viewing thread")?;
    Ok(ControllerHandle {
        stopped,
        events: event_rx,
        frames: frame_rx,
        input,
        native_input:Some(native_input),
        settings,
        close,
        stats,
    })
}

struct Worker {
    source: Source,
    events: std::sync::mpsc::Sender<ControllerEvent>,
    frames: FrameSender,
    input: UnboundedReceiver<Vec<u8>>,
    settings: UnboundedReceiver<Vec<u8>>,
    close: UnboundedReceiver<()>,
    stats: Arc<MediaStats>,
    stopped: Arc<AtomicBool>,
    native:Option<crate::native_input::NativeInputReceiver>,
}
impl Worker {
    fn run(mut self) {
        let result = match tokio::runtime::Runtime::new() {
            Ok(runtime) => {
                let result = runtime.block_on(self.network());
                self.stop_local();
                runtime.shutdown_timeout(Duration::from_secs(2));
                result
            }
            Err(error) => Err(anyhow::anyhow!(
                "cannot start native viewing runtime: {error}"
            )),
        };
        match result {
            Err(error) => {
                let _ = self
                    .events
                    .send(ControllerEvent::Error(uu_i18n::format!("原生连接失败：{error:#}")));
            }
            Ok(()) => {
                let _ = self
                    .events
                    .send(ControllerEvent::Status(uu_i18n::text("原生会话已结束").into()));
            }
        }
        self.stop_local();
        self.stopped.store(true, Ordering::Release);
    }
    fn stop_local(&self) {
        self.stats.audio.stop();
        self.stats.clipboard.stopped.store(true, Ordering::Release);
    }
    async fn network(&mut self) -> Result<()> {
        let _ = self
            .events
            .send(ControllerEvent::Status(match &self.source {Source::Invitation(_)=>uu_i18n::text("正在验证原生邀请…"),Source::Device{..}=>uu_i18n::text("正在连接已配对的原生服务端…"),Source::Internet{..}=>uu_i18n::text("正在通过连接服务建立原生串流…"),Source::Profile{..}=>uu_i18n::text("正在验证并连接设备…")}.into()));
        let invitation=if matches!(&self.source,Source::Internet{..}|Source::Profile{..}){None}else{Some(tokio::select!{
            biased;
            _=self.close.recv()=>return Ok(()),
            result=self.source.resolve()=>result?,
        })};
        let (stop, stop_rx) = watch::channel(false);
        let native=self.native.take().context("native input interface was already consumed")?;
        let mut progress = self.stats.native_progress.subscribe();
        let session = async {
            if let Source::Profile{launch,config}=&self.source {
                crate::device_profile::view(launch.clone(),config.clone(),self.frames.clone(),self.stats.clone(),stop_rx,Some(native)).await
            } else if let Source::Internet{settings,config}=&self.source {
                crate::internet::view(settings.clone(),config.clone(),self.frames.clone(),self.stats.clone(),stop_rx,Some(native)).await
            } else {
                crate::native::view_with_input(invitation.as_ref().unwrap(),self.frames.clone(),self.stats.clone(),stop_rx,native).await
            }
        };
        tokio::pin!(session);
        loop {
            tokio::select! {
                result=&mut session=>return result,
                changed=progress.changed()=>{
                    if changed.is_ok() {
                        if let Some(stage) = *progress.borrow_and_update() {
                            let _ = self.events.send(ControllerEvent::Status(stage.message().into()));
                        }
                    }
                },
                _=self.close.recv()=>{let _=stop.send(true);return session.await;},
                data=self.input.recv()=>{if data.is_none(){let _=stop.send(true);return session.await;}},
                data=self.settings.recv()=>{if data.is_none(){let _=stop.send(true);return session.await;}},
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn closing_during_tls_setup_stops_without_waiting_for_the_auth_timeout() {
        use tokio::{io::AsyncReadExt, net::TcpListener, time::timeout};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let credentials = crate::native_pairing::HostCredentials::new(
            listener.local_addr().unwrap(),
            Duration::from_secs(60),
        )
        .unwrap();
        let path = std::env::temp_dir().join(format!(
            "uu-native-cancel-{}.eclipse-invite.json",
            uuid::Uuid::new_v4()
        ));
        credentials.invitation().save_new(&path).unwrap();
        let stats = Arc::new(MediaStats::default());
        let handle = spawn(path.clone(), stats.clone()).unwrap();
        let outcome = async {
            let (mut socket, _) = timeout(Duration::from_secs(2), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut first = [0u8; 1];
            timeout(Duration::from_secs(2), socket.read_exact(&mut first))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                first[0], 22,
                "expected a TLS handshake before any credential"
            );
            handle.input.send(vec![1, 2, 3]).unwrap();
            handle.settings.send(vec![4, 5, 6]).unwrap();
            handle.close.send(()).unwrap();
            timeout(Duration::from_secs(2), async {
                while !handle.stopped.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            let mut remaining = Vec::new();
            timeout(Duration::from_secs(1), socket.read_to_end(&mut remaining))
                .await
                .unwrap()
                .unwrap();
            assert!(!stats
                .capabilities
                .allows(crate::capabilities::Capability::Input));
            assert_eq!(stats.input_messages_sent.load(Ordering::Relaxed), 0);
        }
        .await;
        std::fs::remove_file(path).unwrap();
        outcome
    }
}

#[cfg(test)]mod lan_tests {
 use super::*;
 #[tokio::test]async fn closing_while_broker_metadata_is_pending_does_not_wait_for_network_timeout(){
  use tokio::{net::TcpListener,io::AsyncReadExt,time::timeout};
  let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();let root=std::env::temp_dir().join(format!("eclipse-lan-viewer-{}",uuid::Uuid::new_v4()));let path=root.join("paired-devices.json");
  let mut book=crate::native_lan_store::Devices::open(&path).unwrap();let id=uuid::Uuid::new_v4().to_string();book.remember(&path,crate::native_lan_store::Device{id:id.clone(),name:"Generated server".into(),endpoint:listener.local_addr().unwrap().to_string(),certificate:vec![1],token:vec![42;32]}).unwrap();
  let stats=Arc::new(MediaStats::default());let handle=spawn_device(path,id,stats.clone(),false).unwrap();let (mut socket,_)=timeout(Duration::from_secs(2),listener.accept()).await.unwrap().unwrap();
  let mut header=[0;17];socket.read_exact(&mut header).await.unwrap();assert_eq!(&header,b"ECLIPSE-LAN-INFO\n");handle.close.send(()).unwrap();
  timeout(Duration::from_secs(2),async{while !handle.stopped.load(Ordering::Acquire){tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();assert_eq!(stats.input_messages_sent.load(Ordering::Acquire),0);std::fs::remove_dir_all(root).unwrap();
 }
}
