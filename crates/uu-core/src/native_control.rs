//! Optional control extension over the existing native SCTP channel.
//! TLS Offer/Answer remain unchanged so a 0.2 peer can continue viewing.
use crate::{
    capabilities::Capability,
    media::MediaStats,
    native_input::{InputCommand, NativeInputReceiver, NextInput},
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{sync_channel, SyncSender},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{mpsc,oneshot, Notify},
    time::timeout,
};
use uu_input::{DesktopRect, InputInjector};
use webrtc::{
    data_channel::{data_channel_state::RTCDataChannelState, RTCDataChannel},
    peer_connection::RTCPeerConnection,
};

const MAX_CONTROL: usize = 16 * 1024;
const LEASE: Duration = Duration::from_secs(5);
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Packet {
    Quality {request:u64,settings:crate::native_quality::Update},
    QualityApplied {applied:crate::native_quality::Applied},
    AdminStatus {available:bool},
    AdminRequest {request:u64,command:crate::host_admin::Request},
    AdminResponse {request:u64,response:crate::host_admin::Response},
    AdminApply {request:u64},
    Displays { catalog:crate::screen_control::RemoteScreens },
    SelectDisplay { request:u64, id:i32 },
    ApplyDisplay { request:u64 },
    DisplaySelected { request:u64, accepted:bool, restart:bool },
    Ping { sequence: u64 },
    Pong { sequence: u64 },
    RequestInput { enabled: bool },
    InputStatus { available: bool, enabled: bool },
    Input { event: InputCommand },
    Telemetry { version:u8, stages:crate::native_timing::HostSnapshot },
    InputMeasured { sequence:u64, event:InputCommand },
    InputApplied { sequence:u64, queue_us:u32, inject_us:u32 },
}
fn parse(data: &[u8]) -> Option<Packet> {
    if data.len() > MAX_CONTROL {
        return None;
    }
    serde_json::from_slice(data).ok()
}
async fn send(channel: &RTCDataChannel, packet: Packet) -> Result<()> {
    let bytes = serde_json::to_vec(&packet)?;
    timeout(Duration::from_millis(200), channel.send(&bytes.into()))
        .await
        .context("native control send timed out")??;
    Ok(())
}

struct WorkerState {
    allowed: std::sync::atomic::AtomicU8,
    gamepads: Mutex<Option<Arc<crate::native_gamepad::Worker>>>,
    gamepad_error: Mutex<Option<String>>,
    authority: Arc<crate::input_ownership::Authority>,
    ownership: Mutex<Option<crate::input_ownership::Lease>>,
    available: AtomicBool,
    enabled: AtomicBool,
    stop: AtomicBool,
    epoch: AtomicU64,
    applied_epoch: AtomicU64,
    seen: Mutex<Instant>,
    applied: AtomicU64,
    errors: AtomicU64,
    failure: Mutex<Option<String>>,
    timing:Arc<crate::native_timing::HostTiming>,
}
impl WorkerState {
    fn release(&self, injector: &mut dyn InputInjector, final_release: bool) -> bool {
        let mut ownership = self.ownership.lock().unwrap();
        let keyboard = injector.release_all();
        let pads=self.gamepads.lock().unwrap().clone();
        let controllers = pads.as_ref().is_none_or(|pads|pads.reset());
        let result = keyboard.is_ok() && controllers;
        if let Err(error)=keyboard {*self.failure.lock().unwrap()=Some(error.to_string());}
        else if !controllers {*self.failure.lock().unwrap()=Some("虚拟手柄释放失败".into());}
        if final_release {if let Some(pads)=pads {pads.stop();}}
        // Keep ownership while retrying cleanup. Only an unrecoverable final
        // shutdown poisons the host; a temporary desktop transition must not.
        if !result && final_release {if let Some(lease)=ownership.as_ref(){lease.poison();}}
        if result && (final_release || !self.enabled.load(Ordering::Acquire)) { ownership.take(); }
        result
    }
    fn suspend(&self,error:String) {
        self.enabled.store(false,Ordering::Release);
        if self.available.swap(false,Ordering::AcqRel) {
            self.epoch.fetch_add(1,Ordering::AcqRel);
            self.errors.fetch_add(1,Ordering::Relaxed);
            tracing::warn!(%error,"native input suspended; retrying owned input cleanup");
        }
        *self.failure.lock().unwrap()=Some(error);
    }
}
struct InputApplied {queue_us:u32,inject_us:u32}
struct InputJob {epoch:u64,command:Option<InputCommand>,queued:Instant,reply:Option<oneshot::Sender<InputApplied>>}
struct InputWorker {
    state: Arc<WorkerState>,
    tx: SyncSender<InputJob>,
    task: Mutex<Option<std::thread::JoinHandle<()>>>,
}
impl InputWorker {
    async fn start(rect: DesktopRect,timing:Arc<crate::native_timing::HostTiming>,authority:Arc<crate::input_ownership::Authority>) -> Result<Arc<Self>> {
        Self::with_timing_factory(rect, uu_input::default_injector,timing,authority).await
    }
    #[cfg(test)]
    async fn with_factory<F>(rect: DesktopRect, factory: F) -> Result<Arc<Self>>
    where F:FnOnce()->std::result::Result<Box<dyn InputInjector>,uu_input::InputError>+Send+'static {
        Self::with_timing_factory(rect,factory,Arc::default(),Arc::default()).await
    }
    async fn with_timing_factory<F>(rect: DesktopRect, factory: F,timing:Arc<crate::native_timing::HostTiming>,authority:Arc<crate::input_ownership::Authority>) -> Result<Arc<Self>>
    where
        F: FnOnce() -> std::result::Result<Box<dyn InputInjector>, uu_input::InputError>
            + Send
            + 'static,
    {
        let state = Arc::new(WorkerState {
            allowed:std::sync::atomic::AtomicU8::new(7),
            gamepads:Mutex::new(None),gamepad_error:Mutex::new(None),
            authority, ownership: Mutex::new(None),
            available: AtomicBool::new(false),
            enabled: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
            applied_epoch: AtomicU64::new(0),
            seen: Mutex::new(Instant::now()),
            applied: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            failure: Mutex::new(None),
            timing,
        });
        let (tx, rx) = sync_channel::<InputJob>(128);
        let (ready, boot) = oneshot::channel();
        let actor = state.clone();
        let task = std::thread::Builder::new()
            .name("native-input".into())
            .spawn(move || {
                let mut injector = match factory() {
                    Ok(i) => i,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                actor.available.store(true, Ordering::Release);
                if ready.send(Ok(())).is_err() {
                    return;
                }
                let mut epoch = 0;
                loop {
                    let received = rx.recv_timeout(Duration::from_millis(50));
                    if actor.stop.load(Ordering::Acquire) {
                        break;
                    }
                    if !actor.available.load(Ordering::Acquire) {
                        // Drop queued events from the old grant. Never replay
                        // failed presses; the viewer requests a fresh grant.
                        if actor.release(&mut *injector,false) {
                            epoch=actor.epoch.load(Ordering::Acquire);
                            actor.applied_epoch.store(epoch,Ordering::Release);
                            *actor.failure.lock().unwrap()=None;
                            actor.available.store(true,Ordering::Release);
                            tracing::info!("native input cleanup recovered; waiting for a fresh control grant");
                        }
                        continue;
                    }
                    if actor.enabled.load(Ordering::Acquire)
                        && actor.seen.lock().unwrap().elapsed() > LEASE
                    {
                        actor.enabled.store(false, Ordering::Release);
                        actor.epoch.fetch_add(1, Ordering::AcqRel);
                    }
                    let current = actor.epoch.load(Ordering::Acquire);
                    if current != epoch {
                        if !actor.release(&mut *injector, false) {
                            let error=actor.failure.lock().unwrap().clone().unwrap_or_else(||"输入释放失败".into());
                            actor.suspend(error);
                            continue;
                        }
                        epoch = current;
                        actor.applied_epoch.store(current,Ordering::Release);
                    }
                    match received {
                        Ok(job)
                            if job.epoch == epoch && actor.enabled.load(Ordering::Acquire) =>
                        {
                            let Some(command)=job.command else{continue;};
                            let queue_us=job.queued.elapsed().as_micros().min(u32::MAX as u128) as u32;
                            let started=Instant::now();
                            let result = match command.event(rect) {
                                Some(event) => injector.inject(event),
                                None => injector.release_all(),
                            };
                            if let Err(error)=result {
                                if matches!(error,uu_input::InputError::UnsupportedKey(_)) {
                                    actor.errors.fetch_add(1,Ordering::Relaxed);
                                    tracing::debug!(%error,"unsupported native input event skipped");
                                }else{actor.suspend(error.to_string());}
                                continue;
                            }
                            let inject_us=started.elapsed().as_micros().min(u32::MAX as u128) as u32;
                            actor.timing.input(queue_us,inject_us);
                            if let Some(reply)=job.reply {let _=reply.send(InputApplied{queue_us,inject_us});}
                            actor.applied.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                        _ => {}
                    }
                }
                actor.enabled.store(false, Ordering::Release);
                actor.available.store(false, Ordering::Release);
                if !actor.release(&mut *injector, true) {
                    actor.errors.fetch_add(1, Ordering::Relaxed);
                }
            })
            .context("cannot start authorized input thread")?;
        let worker = Arc::new(Self {
            state,
            tx,
            task: Mutex::new(Some(task)),
        });
        match boot.await.context("native input setup ended")? {
            Ok(()) => Ok(worker),
            Err(error) => {
                worker.stop().await;
                Err(error.into())
            }
        }
    }
    fn touch(&self) {
        *self.state.seen.lock().unwrap() = Instant::now();
    }
    fn available(&self) -> bool {
        self.state.available.load(Ordering::Acquire) && !self.state.stop.load(Ordering::Acquire)
    }
    fn wake(&self) {
        let _=self.tx.try_send(InputJob{epoch:self.state.epoch.load(Ordering::Acquire),command:None,queued:Instant::now(),reply:None});
    }
    fn request(&self, enabled: bool) {
        self.touch();
        if !enabled || !self.available() {
            self.state.enabled.store(false, Ordering::Release);
            self.state.epoch.fetch_add(1, Ordering::AcqRel);
            self.wake();
            return;
        }
        // An OS release can take time; the network task must never wait for it.
        let Ok(mut ownership) = self.state.ownership.try_lock() else { return; };
        if self.state.enabled.load(Ordering::Acquire) && ownership.is_some(){return;}
        if ownership.is_none() { *ownership = self.state.authority.claim(); }
        self.state.epoch.fetch_add(1, Ordering::AcqRel);
        self.state.enabled.store(ownership.is_some(), Ordering::Release);
        self.wake();
    }

    fn gamepads(&self, state: uu_input::gamepad::Snapshot, generation:u64) {
        if self.state.allowed.load(Ordering::Acquire)&4==0 {return;}
        if !self.available() || !self.state.enabled.load(Ordering::Acquire) || generation!=self.state.epoch.load(Ordering::Acquire){return;}
        let mut pads=self.state.gamepads.lock().unwrap();
        if pads.is_none() {
            if state.iter().all(Option::is_none){return;}
            let weak=Arc::downgrade(&self.state);
            match crate::native_gamepad::Worker::start(Arc::new(move||weak.upgrade().map_or((0,false),|s|(s.epoch.load(Ordering::Acquire),s.enabled.load(Ordering::Acquire)&&!s.stop.load(Ordering::Acquire)&&s.applied_epoch.load(Ordering::Acquire)==s.epoch.load(Ordering::Acquire))))) {
                Ok(worker)=>*pads=Some(worker),Err(error)=>{*self.state.gamepad_error.lock().unwrap()=Some(error.to_string());return;}
            }
        }
        pads.as_ref().unwrap().submit(generation,state);
    }
    fn gamepad_status(&self)->crate::native_gamepad::Status {
        self.state.gamepads.lock().unwrap().as_ref().map_or_else(||crate::native_gamepad::Status{connected:0,error:self.state.gamepad_error.lock().unwrap().clone()},|p|p.status())
    }
    fn push(&self, command: InputCommand) -> bool {
        self.push_with_ack(command,None)
    }
    fn push_with_ack(&self,command:InputCommand,reply:Option<oneshot::Sender<InputApplied>>)->bool {
        let required=match command {InputCommand::Key{..}=>1,InputCommand::ReleaseAll{}=>0,_=>2};
        if required!=0 && self.state.allowed.load(Ordering::Acquire)&required==0 {return false;}
        if !self.available() || !self.state.enabled.load(Ordering::Acquire) || !command.validate() {
            return false;
        }
        self.touch();
        match self.tx.try_send(InputJob {epoch:self.state.epoch.load(Ordering::Acquire),command:Some(command),queued:Instant::now(),reply}) {
            Ok(())=>true,
            Err(std::sync::mpsc::TrySendError::Full(_))=>{
                self.state.suspend("输入队列积压，正在释放并恢复".into());
                false
            },
            Err(std::sync::mpsc::TrySendError::Disconnected(_))=>{
                self.state.stop.store(true,Ordering::Release);
                self.state.suspend("输入工作线程已关闭".into());
                false
            },
        }
    }
    async fn stop(&self) {
        self.state.stop.store(true, Ordering::Release);
        self.wake();
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = tokio::task::spawn_blocking(move || task.join()).await;
        }
    }
}
impl Drop for InputWorker {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Release);
    }
}

pub(crate) struct HostControl {
    quality:Mutex<Option<crate::native_quality::Runtime>>,
    session:Mutex<Option<crate::shared_host::SessionContext>>,
    admin_busy:AtomicBool,
    pending_restart:Mutex<Option<u64>>,
    displays:Mutex<Option<Arc<crate::native_displays::Host>>>,
    pending_display:Mutex<Option<u64>>,
    stopped:AtomicBool,
    pub closed:Notify,
    timing:Arc<crate::native_timing::HostTiming>,
    worker: Option<Arc<InputWorker>>,
    registered: AtomicBool,
    clipboard: Option<Arc<crate::clipboard::Clipboard>>,
    clipboard_registered: AtomicBool,
    clipboard_version: std::sync::atomic::AtomicU8,
    gamepad_registered: AtomicBool,
    gamepad_typed: AtomicBool,
}
impl HostControl {
    pub fn clipboard_images(&self,enabled:bool){self.clipboard_version.store(if enabled{2}else{1},Ordering::Release);}
    pub fn gamepad_types(&self,enabled:bool){self.gamepad_typed.store(enabled,Ordering::Release);}
    pub fn set_quality(&self,quality:crate::native_quality::Runtime){*self.quality.lock().unwrap()=Some(quality);}
    pub fn set_session(&self,session:Option<crate::shared_host::SessionContext>) {*self.session.lock().unwrap()=session;}
    fn admin_status(&self)->Packet {
        Packet::AdminStatus{available:self.session.lock().unwrap().as_ref().is_some_and(|s|s.access.sessions.administration.lock().unwrap().is_some())}
    }
    async fn administer(self:Arc<Self>,channel:Arc<RTCDataChannel>,request:u64,command:crate::host_admin::Request) {
        use crate::host_admin::Response;
        if self.admin_busy.swap(true,Ordering::AcqRel) {
            let _=send(&channel,Packet::AdminResponse{request,response:Response::Error{message:"另一项管理请求尚未完成".into()}}).await;
            return;
        }
        struct Reset<'a>(&'a AtomicBool);
        impl Drop for Reset<'_> {fn drop(&mut self){self.0.store(false,Ordering::Release);}}
        let _reset=Reset(&self.admin_busy);
        let context=self.session.lock().unwrap().clone();
        let service=context.as_ref().and_then(|c|c.access.sessions.administration.lock().unwrap().clone());
        let result=match (context,service) {
            (Some(context),Some(service))=>tokio::task::spawn_blocking(move||service.execute(&context,command)).await.map_err(anyhow::Error::from).and_then(|r|r),
            _=>Err(anyhow::anyhow!("主机未提供连接管理")),
        };
        if self.stopped.load(Ordering::Acquire){return;}
        let response=result.unwrap_or_else(|error|Response::Error{message:error.to_string().chars().take(512).collect()});
        if matches!(response,Response::Saved{restart:true}) {*self.pending_restart.lock().unwrap()=Some(request);}
        let _=send(&channel,Packet::AdminResponse{request,response}).await;
    }
    pub fn set_permissions(&self,permissions:crate::device_trust::Permissions) {
        if let Some(worker)=&self.worker {worker.state.allowed.store(permissions.input_mask(),Ordering::Release);}
    }
    #[cfg(test)]
    pub async fn new(
        allow: bool,
        rect: Option<uu_capture::DesktopRect>,
        clipboard: Option<Arc<crate::clipboard::Clipboard>>,
    ) -> Arc<Self> {
        Self::with_injector(allow,rect,clipboard,None,Arc::default()).await
    }
    pub async fn with_injector(
        allow: bool,
        rect: Option<uu_capture::DesktopRect>,
        clipboard: Option<Arc<crate::clipboard::Clipboard>>,
        injector: Option<Box<dyn InputInjector>>,
        authority: Arc<crate::input_ownership::Authority>,
    ) -> Arc<Self> {
        let timing:Arc<crate::native_timing::HostTiming>=Arc::default();
        let worker = if let Some(rect) = rect.filter(|_| allow) {
            match DesktopRect::new(rect.x, rect.y, rect.width, rect.height) {
                Ok(rect) => match match injector {
                    Some(injector)=>InputWorker::with_timing_factory(rect,move || Ok(injector),timing.clone(),authority.clone()).await,
                    None=>InputWorker::start(rect,timing.clone(),authority.clone()).await,
                } {
                    Ok(worker) => Some(worker),
                    Err(error) => {
                        tracing::warn!(%error,"native input unavailable; continuing view only");
                        None
                    }
                },
                Err(_) => None,
            }
        } else {
            None
        };
        Arc::new(Self {
            quality:Mutex::new(None),
            session:Mutex::new(None),admin_busy:AtomicBool::new(false),pending_restart:Mutex::new(None),
            displays:Mutex::new(None),
            pending_display:Mutex::new(None),
            stopped:AtomicBool::new(false),
            closed:Notify::new(),
            timing,
            worker,
            clipboard,
            clipboard_registered: AtomicBool::new(false),
            clipboard_version: std::sync::atomic::AtomicU8::new(1),
            gamepad_registered: AtomicBool::new(false),
            gamepad_typed: AtomicBool::new(false),
            registered: AtomicBool::new(false),
        })
    }
    pub fn set_displays(&self,displays:Option<Arc<crate::native_displays::Host>>){*self.displays.lock().unwrap()=displays;}
    pub fn timing(&self)->Arc<crate::native_timing::HostTiming>{self.timing.clone()}
    fn status(&self) -> Packet {
        Packet::InputStatus {
            available: self.worker.as_ref().is_some_and(|w| w.available()),
            enabled: self
                .worker
                .as_ref()
                .is_some_and(|w| w.state.enabled.load(Ordering::Acquire)),
        }
    }
    pub fn attach(self: &Arc<Self>, pc: &RTCPeerConnection) {
        let host = self.clone();
        pc.on_data_channel(Box::new(move |channel| {
            let host = host.clone();
            Box::pin(async move {
                let gamepad_label=if host.gamepad_typed.load(Ordering::Acquire){crate::native_gamepad::LABEL_V2}else{crate::native_gamepad::LABEL};
                if channel.label()==gamepad_label && !channel.ordered() && channel.max_packet_lifetime()==crate::native_gamepad::LIFETIME_MS && channel.max_retransmits()==0 && !host.gamepad_registered.swap(true,Ordering::AcqRel) {
                    host.attach_gamepads(channel);return;
                }
                let clipboard_label=if host.clipboard_version.load(Ordering::Acquire)==2 {crate::native_clipboard::LABEL_V2}else{crate::native_clipboard::LABEL};
                if channel.label() == clipboard_label
                    && channel.ordered()
                    && channel.max_retransmits() == 0
                    && channel.max_packet_lifetime() == 0
                    && !host.clipboard_registered.swap(true, Ordering::AcqRel)
                {
                    crate::native_clipboard::host(channel, host.clipboard.clone());
                    return;
                }
                if channel.label() != "eclipse-control-v1"
                    || !channel.ordered()
                    || channel.max_retransmits() != 0
                    || channel.max_packet_lifetime() != 0
                    || host.registered.swap(true, Ordering::AcqRel)
                {
                    let _ = channel.close().await;
                    return;
                }
                let closed = host.clone();
                channel.on_close(Box::new(move || {
                    closed.closed.notify_one();
                    if let Some(worker) = &closed.worker {
                        worker.state.stop.store(true, Ordering::Release);
                    }
                    Box::pin(async {})
                }));
                let weak = Arc::downgrade(&channel);
                let handler = host.clone();
                channel.on_message(Box::new(move |message| {
                    let host = handler.clone();
                    let weak = weak.clone();
                    Box::pin(async move {
                        let Some(channel) = weak.upgrade() else {
                            return;
                        };
                        if host.stopped.load(Ordering::Acquire){return;}
                        match parse(&message.data) {
                            Some(Packet::Quality{request,settings})=>{
                                let quality=host.quality.lock().unwrap().clone();
                                tokio::spawn(async move {
                                    let result=async {
                                        let receiver=quality.context("主机尚未准备好动态画质")?.request(settings)?;
                                        timeout(Duration::from_secs(10),receiver).await.context("主机画质更新超时")?.context("画质更新已经取消")?.map_err(anyhow::Error::msg)
                                    }.await;
                                    let (bitrate_kbps,error)=match result {Ok(kbps)=>(kbps,None),Err(error)=>(0,Some(error.to_string()))};
                                    let _=send(&channel,Packet::QualityApplied{applied:crate::native_quality::Applied{request,settings,bitrate_kbps,error}}).await;
                                });
                            },
                            Some(Packet::AdminRequest{request,command})=>{
                                tokio::spawn(host.clone().administer(channel.clone(),request,command));
                            },
                            Some(Packet::AdminApply{request})=>{
                                let matched={let mut pending=host.pending_restart.lock().unwrap();if *pending==Some(request){pending.take();true}else{false}};
                                if matched {
                                    let context=host.session.lock().unwrap().clone();
                                    if let Some(context)=context {
                                        let service=context.access.sessions.administration.lock().unwrap().clone();
                                        if let Some(service)=service {tokio::task::spawn_blocking(move||service.restart(&context));}
                                    }
                                }
                            },
                            Some(Packet::Ping { sequence }) => {
                                if let Some(worker) = &host.worker {
                                    worker.touch();
                                }
                                let _ = send(&channel, Packet::Pong { sequence }).await;
                                let _ = send(&channel, host.status()).await;
                                let _ = send(&channel, host.admin_status()).await;
                                let _ = send(&channel,Packet::Telemetry{version:1,stages:host.timing.snapshot()}).await;
                                let displays=host.displays.lock().unwrap().clone();
                                if let Some(displays)=displays {let _=send(&channel,Packet::Displays{catalog:displays.catalog()}).await;}

                            }
                            Some(Packet::SelectDisplay{request,id})=>{
                                let displays=host.displays.lock().unwrap().clone();
                                let decision=if host.pending_display.lock().unwrap().is_some(){None}else{displays.as_ref().and_then(|d|d.select(id))};
                                let restart=decision==Some(true);
                                if restart {*host.pending_display.lock().unwrap()=Some(request);}
                                let _=send(&channel,Packet::DisplaySelected{request,accepted:decision.is_some(),restart}).await;
                            },
                            Some(Packet::ApplyDisplay{request})=>{
                                let matched={let mut pending=host.pending_display.lock().unwrap();if *pending==Some(request){pending.take();true}else{false}};
                                if matched {let displays=host.displays.lock().unwrap().clone();if let Some(displays)=displays{displays.commit();}}
                            },
                            Some(Packet::RequestInput { enabled }) => {
                                if let Some(worker) = &host.worker {
                                    worker.request(enabled);
                                }
                                let _ = send(&channel, host.status()).await;
                                let _ = send(&channel, host.admin_status()).await;
                            }
                            Some(Packet::Input { event }) => {
                                if !host
                                    .worker
                                    .as_ref()
                                    .is_some_and(|worker| worker.push(event))
                                {
                                    let _ = send(&channel, host.status()).await;
                                }
                            }
                            Some(Packet::InputMeasured{sequence,event})=>{
                                let (reply,applied)=oneshot::channel();
                                if host.worker.as_ref().is_some_and(|worker|worker.push_with_ack(event,Some(reply))) {
                                    let weak=Arc::downgrade(&channel);
                                    tokio::spawn(async move {
                                        if let Ok(Ok(applied))=timeout(Duration::from_secs(1),applied).await {
                                            if let Some(channel)=weak.upgrade() {let _=send(&channel,Packet::InputApplied{sequence,queue_us:applied.queue_us,inject_us:applied.inject_us}).await;}
                                        }
                                    });
                                }else{let _=send(&channel,host.status()).await;}
                            },
                            _ => {}
                        }
                    })
                }));
            })
        }));
    }
    fn attach_gamepads(self:&Arc<Self>,channel:Arc<RTCDataChannel>) {
        let version=crate::native_gamepad::version(&channel);
        let worker=self.worker.clone();let closed=worker.clone();
        channel.on_close(Box::new(move||{if let Some(worker)=&closed{worker.gamepads([None;uu_input::gamepad::SLOTS],worker.state.epoch.load(Ordering::Acquire));}Box::pin(async{})}));
        let weak=Arc::downgrade(&channel);let last=Arc::new(AtomicU64::new(0));let reported=Arc::new(Mutex::new(Instant::now()-Duration::from_secs(1)));
        channel.on_message(Box::new(move|message|{
            let worker=worker.clone();let weak=weak.clone();let last=last.clone();let reported=reported.clone();
            Box::pin(async move {
                if let Some(crate::native_gamepad::Packet::State{version:packet_version,sequence,generation,state})=crate::native_gamepad::parse(&message.data) {
                    if packet_version!=version{return;}
                    if last.fetch_update(Ordering::AcqRel,Ordering::Acquire,|old|(sequence>old).then_some(sequence)).is_err(){return;}
                    if let Some(worker)=&worker {worker.gamepads(state,generation);}
                    let reporting={let mut at=reported.lock().unwrap();if worker.as_ref().is_some_and(|w|w.state.epoch.load(Ordering::Acquire)!=generation)||at.elapsed()>=Duration::from_secs(1){*at=Instant::now();true}else{false}};
                    if reporting {if let Some(channel)=weak.upgrade(){
                        let status=worker.as_ref().map_or_else(||crate::native_gamepad::Status{connected:0,error:Some("被控端未允许控制或输入后端不可用".into())},|worker|worker.gamepad_status());
                        let packet=crate::native_gamepad::Packet::Status{version,connected:status.connected,generation:worker.as_ref().map_or(0,|w|w.state.epoch.load(Ordering::Acquire)),error:status.error};
                        if let Ok(bytes)=serde_json::to_vec(&packet){let _=timeout(Duration::from_millis(20),channel.send(&bytes.into())).await;}
                    }}
                }
            })
        }));
    }
    pub async fn stop(&self) {
        self.stopped.store(true,Ordering::Release);
        if let Some(clipboard) = &self.clipboard {
            if self.displays.lock().unwrap().is_none(){crate::native_clipboard::stop(clipboard);}else{crate::native_clipboard::suspend(clipboard);}
        }
        if let Some(worker) = &self.worker {
            worker.stop().await;
        }
    }
    pub fn counters(&self) -> (u64, u64) {
        self.worker.as_ref().map_or((0, 0), |w| {
            (
                w.state.applied.load(Ordering::Relaxed),
                w.state.errors.load(Ordering::Relaxed),
            )
        })
    }
}

pub(crate) struct ViewerControl {
    admin:mpsc::Receiver<crate::host_admin::Command>,
    gamepads:Option<crate::native_gamepad::Viewer>,
    media_epoch:u64,
    channel: Arc<RTCDataChannel>,
    stats: Arc<MediaStats>,
    input: Option<NativeInputReceiver>,
    ready: Arc<Notify>,
    probe: Arc<Mutex<crate::network_probe::NetworkProbe>>,
    requested: Arc<AtomicBool>,
    epoch: Arc<AtomicU64>,
}
impl ViewerControl {
    pub fn new(
        channel: Arc<RTCDataChannel>,
        stats: Arc<MediaStats>,
        input: Option<NativeInputReceiver>,
    ) -> Self {
        let media_epoch=stats.native_media_epoch.load(Ordering::Acquire);
        let admin=stats.host_admin.attach();
        *stats.native_quality_applied.lock().unwrap()=None;
        stats.control_open.store(0,Ordering::Release);
        stats.capabilities.enable(Capability::Input,false);
        stats.native_input_available.store(false,Ordering::Release);
        stats.native_timing.reset();
        stats.rtt_updated_ms.store(0,Ordering::Relaxed);
        let requested = input
            .as_ref()
            .map(|i| i.requested.clone())
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
        let epoch = input
            .as_ref()
            .map(|i| i.epoch.clone())
            .unwrap_or_else(|| Arc::new(AtomicU64::new(0)));
        let ready = Arc::new(Notify::new());
        let probe = Arc::new(Mutex::new(crate::network_probe::NetworkProbe::default()));
        let opened = stats.clone();
        let notify = ready.clone();
        channel.on_open(Box::new(move || {
            if opened.native_media_epoch.load(Ordering::Acquire)!=media_epoch{return Box::pin(async {});}
            opened.control_open.store(1, Ordering::Release);
            notify.notify_one();
            Box::pin(async {})
        }));
        let closed = stats.clone();
        let notify = ready.clone();
        channel.on_close(Box::new(move || {
            if closed.native_media_epoch.load(Ordering::Acquire)!=media_epoch{return Box::pin(async {});}
            closed.control_open.store(0, Ordering::Release);
            closed.host_admin.disconnect();
            closed.capabilities.enable(Capability::Input, false);
            closed
                .native_input_available
                .store(false, Ordering::Release);
            notify.notify_one();
            Box::pin(async {})
        }));
        let incoming = stats.clone();
        let requested_copy = requested.clone();
        let generation = epoch.clone();
        let probes = probe.clone();
        let admin_channel=Arc::downgrade(&channel);
        channel.on_message(Box::new(move |message| {
            if incoming.native_media_epoch.load(Ordering::Acquire)!=media_epoch{return Box::pin(async {});}
            match parse(&message.data) {
                Some(Packet::QualityApplied{applied})=>{
                    let mut previous=incoming.native_quality_applied.lock().unwrap();
                    if previous.as_ref().is_none_or(|old|applied.request>=old.request) {
                        if applied.error.is_none()&&applied.settings.validate().is_ok() {
                            *incoming.native_quality_current.lock().unwrap()=Some(applied.settings);
                            incoming.configured_fps.store(applied.settings.fps,Ordering::Release);
                        }
                        *previous=Some(applied);
                    }
                },
                Some(Packet::AdminStatus{available})=>{incoming.host_admin.available.store(available,Ordering::Release);},
                Some(Packet::AdminResponse{request,response})=>{
                    let restart=matches!(response,crate::host_admin::Response::Saved{restart:true});
                    incoming.host_admin.receive(request,response);
                    if restart {
                        let channel=admin_channel.clone();
                        return Box::pin(async move{if let Some(channel)=channel.upgrade(){let _=send(&channel,Packet::AdminApply{request}).await;}});
                    }
                },
                Some(Packet::Displays{catalog}) if crate::native_displays::valid(&catalog)=>{
                    *incoming.remote_screens.lock().unwrap()=catalog;
                    incoming.native_displays.catalog(media_epoch);
                    incoming.remote_screens_revision.fetch_add(1,Ordering::Release);
                    incoming.capabilities.enable(Capability::Displays,true);
                },
                Some(Packet::DisplaySelected{request,accepted,restart})=>incoming.native_displays.response(crate::native_displays::Response{request,accepted,restart}),
                Some(Packet::Pong { sequence }) => {
                    let reading = {
                        let mut probe = probes.lock().unwrap();
                        probe.reply_sequence(sequence).map(|values| (values, probe.distribution_us()))
                    };
                    if let Some(((mean, deviation), distribution)) = reading {
                        *incoming.rtt_distribution_us.lock().unwrap() = distribution;
                        incoming.rtt_us.store(mean, Ordering::Relaxed);
                        incoming.rtt_ms.store(mean.div_ceil(1000),Ordering::Relaxed);
                        incoming.rtt_updated_ms.store(crate::control_wire::now_ms(),Ordering::Relaxed);
                        incoming
                            .rtt_deviation_us
                            .store(deviation, Ordering::Relaxed);
                    }
                }
                Some(Packet::InputStatus { available, enabled }) => {
                    let previous_available=incoming.native_input_available.swap(available,Ordering::AcqRel);
                    let previous_grant=incoming.capabilities.allows(Capability::Input);
                    let enabled = available && enabled && requested_copy.load(Ordering::Acquire);
                    if enabled && !incoming.capabilities.allows(Capability::Input) {
                        generation.fetch_add(1, Ordering::AcqRel);
                    }
                    incoming.capabilities.enable(Capability::Input, enabled);
                    if previous_available!=available || previous_grant!=enabled {
                        incoming.diagnostics.record(serde_json::json!({"event":"native_input_status","available":available,"granted":enabled,"requested":requested_copy.load(Ordering::Acquire)}));
                    }
                }
                Some(Packet::Telemetry{version:1,stages})=>incoming.native_timing.host(stages),
                Some(Packet::InputApplied{sequence,queue_us,inject_us})=>incoming.native_timing.applied(sequence,queue_us,inject_us),
                _ => {}
            }
            Box::pin(async {})
        }));
        Self {
            admin,
            gamepads:None,
            media_epoch,
            channel,
            stats,
            input,
            ready,
            probe,
            requested,
            epoch,
        }
    }
    async fn input(&self, command: InputCommand,created:Option<Instant>) -> Result<()> {
        if let Some(created)=created {self.stats.native_timing.client_queue(created);}
        let measured=if matches!(command,InputCommand::ReleaseAll{}) {None}else{self.stats.native_timing.begin_input()};
        let packet=match measured {Some(sequence)=>Packet::InputMeasured{sequence,event:command},None=>Packet::Input{event:command}};
        send(&self.channel,packet).await?;
        self.stats
            .input_messages_sent
            .fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    pub fn gamepads(&mut self, viewer:Option<crate::native_gamepad::Viewer>){self.gamepads=viewer;}
    pub async fn apply_display(&self,request:u64)->Result<()> {
        // The host keeps the old peer alive until this confirmation proves its
        // acceptance reached us. Closing immediately after send could discard
        // the buffered SCTP acceptance before the client received it.
        send(&self.channel,Packet::ApplyDisplay{request}).await?;
        timeout(Duration::from_secs(15),async{
            loop {
                if matches!(self.channel.ready_state(),RTCDataChannelState::Closing|RTCDataChannelState::Closed){break;}
                self.ready.notified().await;
            }
        }).await.context("被控端未完成显示器切换")?;
        Ok(())
    }
    pub fn take_input(&mut self)->Option<NativeInputReceiver>{self.input.take()}
    pub async fn run(&mut self) -> Result<()> {
        timeout(Duration::from_secs(15), async {
            loop {
                match self.channel.ready_state() {
                    RTCDataChannelState::Open => return Ok(()),
                    RTCDataChannelState::Closed | RTCDataChannelState::Closing => {
                        anyhow::bail!("native control closed")
                    }
                    _ => self.ready.notified().await,
                }
            }
        })
        .await
        .context("native control did not open")??;
        if let Some(input) = &mut self.input {
            input.mode.borrow_and_update();
            input.reset.borrow_and_update();
        }
        send(
            &self.channel,
            Packet::RequestInput {
                enabled: self.requested.load(Ordering::Acquire),
            },
        )
        .await?;
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut sequence = 0u64;
        loop {
            tokio::select! {biased;
                Some(command)=self.admin.recv()=>{
                    send(&self.channel,Packet::AdminRequest{request:command.id,command:command.request}).await?;
                },
                action=async{match self.input.as_mut(){Some(input)=>input.next().await,None=>std::future::pending().await}}=>{
                    match action {
                        NextInput::Quality(request,settings)=>send(&self.channel,Packet::Quality{request,settings}).await?,
                        NextInput::Display(request,id)=>{
                            if self.stats.capabilities.allows(Capability::Displays) && self.stats.native_displays.request(request){
                                send(&self.channel,Packet::SelectDisplay{request,id}).await?;
                            }
                        },
                        NextInput::Mode(enabled)=>{
                            if !enabled{self.stats.capabilities.enable(Capability::Input,false);}
                            send(&self.channel,Packet::RequestInput{enabled}).await?;
                        },
                        NextInput::Reset=>{if self.stats.capabilities.allows(Capability::Input){self.input(InputCommand::ReleaseAll{},None).await?;}},
                        NextInput::Command(envelope)=>{
                            if envelope.epoch==self.epoch.load(Ordering::Acquire)&&self.requested.load(Ordering::Acquire)&&self.stats.capabilities.allows(Capability::Input){self.input(envelope.command,Some(envelope.created)).await?;}
                        },
                        NextInput::Closed=>{
                            self.requested.store(false,Ordering::Release);self.stats.capabilities.enable(Capability::Input,false);
                            send(&self.channel,Packet::RequestInput{enabled:false}).await?;self.input=None;
                        },
                    }
                },
                _=ticker.tick()=>{
                    sequence=sequence.wrapping_add(1);self.probe.lock().unwrap().begin(sequence);
                    send(&self.channel,Packet::Ping{sequence}).await?;
                    if self.requested.load(Ordering::Acquire) && self.stats.native_input_available.load(Ordering::Acquire) && !self.stats.capabilities.allows(Capability::Input) {
                        send(&self.channel,Packet::RequestInput{enabled:true}).await?;
                    }
                },
            }
        }
    }
}
impl Drop for ViewerControl {
    fn drop(&mut self) {
        if self.stats.native_media_epoch.load(Ordering::Acquire)!=self.media_epoch{return;}
        self.stats.capabilities.enable(Capability::Input, false);
        self.stats
            .native_input_available
            .store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        events: Arc<Mutex<Vec<uu_input::InputEvent>>>,
        releases: Arc<AtomicU64>,
    }
    impl InputInjector for Fixture {
        fn inject(
            &mut self,
            event: uu_input::InputEvent,
        ) -> std::result::Result<(), uu_input::InputError> {
            self.events.lock().unwrap().push(event);
            Ok(())
        }
        fn release_all(&mut self) -> std::result::Result<(), uu_input::InputError> {
            self.releases.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }
    async fn wait(check: impl Fn() -> bool) {
        timeout(Duration::from_secs(2), async {
            while !check() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn temporary_input_failure_releases_before_regrant_and_drops_old_events() {
        struct Interrupted {fail:Arc<AtomicBool>,cleanup:Arc<AtomicBool>,events:Arc<Mutex<Vec<uu_input::InputEvent>>>}
        impl InputInjector for Interrupted {
            fn inject(&mut self,event:uu_input::InputEvent)->std::result::Result<(),uu_input::InputError>{
                if self.fail.swap(false,Ordering::AcqRel){return Err(uu_input::InputError::Unavailable("owned desktop temporarily unavailable".into()));}
                self.events.lock().unwrap().push(event);Ok(())
            }
            fn release_all(&mut self)->std::result::Result<(),uu_input::InputError>{
                if !self.cleanup.load(Ordering::Acquire){return Err(uu_input::InputError::Unavailable("owned releases temporarily blocked".into()));}Ok(())
            }
        }
        let fail=Arc::new(AtomicBool::new(false));let cleanup=Arc::new(AtomicBool::new(true));let events=Arc::new(Mutex::new(Vec::new()));
        let (failure,release,recorded)=(fail.clone(),cleanup.clone(),events.clone());
        let authority=Arc::new(crate::input_ownership::Authority::default());let rect=DesktopRect::new(0,0,640,480).unwrap();
        let a=InputWorker::with_timing_factory(rect,move||Ok(Box::new(Interrupted{fail:failure,cleanup:release,events:recorded})),Arc::default(),authority.clone()).await.unwrap();
        let b=InputWorker::with_timing_factory(rect,||Ok(Box::new(Fixture{events:Arc::default(),releases:Arc::default()})),Arc::default(),authority).await.unwrap();
        a.request(true);assert!(a.push(InputCommand::Key{usage:4,pressed:true}));wait(||a.state.applied.load(Ordering::Acquire)==1).await;
        cleanup.store(false,Ordering::Release);fail.store(true,Ordering::Release);
        assert!(a.push(InputCommand::Relative{dx:1,dy:1}));
        wait(||!a.available()).await;
        b.request(true);assert!(!b.state.enabled.load(Ordering::Acquire),"failed releases retain the previous owner");
        assert!(!a.push(InputCommand::Key{usage:5,pressed:true}));
        cleanup.store(true,Ordering::Release);wait(||a.available()).await;
        assert!(!a.state.enabled.load(Ordering::Acquire),"backend recovery does not grant control itself");
        a.request(true);assert!(a.push(InputCommand::Key{usage:6,pressed:true}));
        wait(||a.state.applied.load(Ordering::Acquire)==2).await;
        assert_eq!(events.lock().unwrap().len(),2);
        a.request(false);wait(||a.state.ownership.lock().unwrap().is_none()).await;
        b.request(true);assert!(b.push(InputCommand::Relative{dx:2,dy:2}));wait(||b.state.applied.load(Ordering::Acquire)==1).await;
        a.stop().await;b.stop().await;
    }
    #[tokio::test]
    async fn overflowing_input_queue_recovers_without_replaying_queued_presses() {
        struct Blocked {entered:Option<oneshot::Sender<()>>,gate:Arc<(Mutex<bool>,std::sync::Condvar)>}
        impl InputInjector for Blocked {
            fn inject(&mut self,_:uu_input::InputEvent)->std::result::Result<(),uu_input::InputError>{
                if let Some(entered)=self.entered.take(){let _=entered.send(());let mut open=self.gate.0.lock().unwrap();while !*open {open=self.gate.1.wait(open).unwrap();}}
                Ok(())
            }
            fn release_all(&mut self)->std::result::Result<(),uu_input::InputError>{Ok(())}
        }
        let gate=Arc::new((Mutex::new(false),std::sync::Condvar::new()));let wait_gate=gate.clone();let (started,entered)=oneshot::channel();
        let worker=InputWorker::with_factory(DesktopRect::new(0,0,640,480).unwrap(),move||Ok(Box::new(Blocked{entered:Some(started),gate:wait_gate}))).await.unwrap();
        worker.request(true);assert!(worker.push(InputCommand::Key{usage:4,pressed:true}));entered.await.unwrap();
        for _ in 0..128 {assert!(worker.push(InputCommand::Key{usage:5,pressed:true}));}
        let overflow=worker.push(InputCommand::Key{usage:5,pressed:true});
        *gate.0.lock().unwrap()=true;gate.1.notify_all();
        assert!(!overflow);assert!(!worker.state.stop.load(Ordering::Acquire));
        wait(||worker.available()).await;worker.request(true);
        assert!(worker.push(InputCommand::Key{usage:6,pressed:true}));
        wait(||worker.state.applied.load(Ordering::Acquire)==2).await;
        assert_eq!(worker.state.applied.load(Ordering::Acquire),2,"old queued events must not execute after a fresh grant");
        worker.stop().await;
    }
    #[tokio::test]
    async fn unsupported_platform_key_does_not_stop_other_input() {
        struct Unsupported;
        impl InputInjector for Unsupported {
            fn inject(&mut self,event:uu_input::InputEvent)->std::result::Result<(),uu_input::InputError>{
                match event {uu_input::InputEvent::Key{key,..}=>Err(uu_input::InputError::UnsupportedKey(key.0)),_=>Ok(())}
            }
            fn release_all(&mut self)->std::result::Result<(),uu_input::InputError>{Ok(())}
        }
        let worker=InputWorker::with_factory(DesktopRect::new(0,0,640,480).unwrap(),||Ok(Box::new(Unsupported))).await.unwrap();
        worker.request(true);assert!(worker.push(InputCommand::Key{usage:4,pressed:true}));
        wait(||worker.state.errors.load(Ordering::Acquire)==1).await;
        assert!(worker.available());assert!(worker.push(InputCommand::Relative{dx:1,dy:1}));
        wait(||worker.state.applied.load(Ordering::Acquire)==1).await;worker.stop().await;
    }
    #[tokio::test]
    async fn input_ack_waits_for_the_injector_and_includes_its_processing_time() {
        struct Delayed {started:Option<oneshot::Sender<()>>,gate:Arc<(Mutex<bool>,std::sync::Condvar)>}
        impl InputInjector for Delayed {
            fn inject(&mut self,_:uu_input::InputEvent)->std::result::Result<(),uu_input::InputError> {
                if let Some(started)=self.started.take(){let _=started.send(());}
                let (lock,signal)=&*self.gate;let mut ready=lock.lock().unwrap();
                while !*ready {ready=signal.wait(ready).unwrap();}Ok(())
            }
            fn release_all(&mut self)->std::result::Result<(),uu_input::InputError>{Ok(())}
        }
        let gate=Arc::new((Mutex::new(false),std::sync::Condvar::new()));
        let wait_gate=gate.clone();let (started,entered)=oneshot::channel();
        let worker=InputWorker::with_factory(DesktopRect::new(0,0,640,480).unwrap(),move||Ok(Box::new(Delayed{started:Some(started),gate:wait_gate}))).await.unwrap();
        worker.request(true);let (reply,mut applied)=oneshot::channel();
        assert!(worker.push_with_ack(InputCommand::Key{usage:4,pressed:true},Some(reply)));
        entered.await.unwrap();
        let early=timeout(Duration::from_millis(20),&mut applied).await;
        *gate.0.lock().unwrap()=true;gate.1.notify_all();
        assert!(early.is_err(),"queuing is not an injection acknowledgement");
        let result=applied.await.unwrap();assert!(result.inject_us>=15_000);
        assert_eq!(worker.state.timing.snapshot().input.unwrap().samples,1);
        worker.stop().await;
    }
    #[tokio::test]
    async fn input_requires_a_grant_and_is_released_on_lease_expiry_and_shutdown() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let releases = Arc::new(AtomicU64::new(0));
        let (recorded, released) = (events.clone(), releases.clone());
        let worker =
            InputWorker::with_factory(DesktopRect::new(0, 0, 1280, 720).unwrap(), move || {
                Ok(Box::new(Fixture {
                    events: recorded,
                    releases: released,
                }))
            })
            .await
            .unwrap();
        let command = InputCommand::Key {
            usage: 4,
            pressed: true,
        };
        assert!(!worker.push(command));
        assert!(events.lock().unwrap().is_empty());
        worker.request(true);
        assert!(worker.push(command));
        wait(|| worker.state.applied.load(Ordering::Acquire) == 1).await;
        let before = releases.load(Ordering::Acquire);
        *worker.state.seen.lock().unwrap() = Instant::now() - LEASE - Duration::from_secs(1);
        wait(|| {
            !worker.state.enabled.load(Ordering::Acquire)
                && releases.load(Ordering::Acquire) > before
        })
        .await;
        assert!(!worker.push(command));
        worker.request(true);
        assert!(worker.push(command));
        wait(|| worker.state.applied.load(Ordering::Acquire) == 2).await;
        let before = releases.load(Ordering::Acquire);
        worker.stop().await;
        assert!(!worker.available());
        assert!(releases.load(Ordering::Acquire) > before);
        assert!(!worker.push(command));
        worker.stop().await;
    }
    #[tokio::test]
    async fn keyboard_mouse_and_gamepad_grants_are_enforced_before_dispatch() {
        let events=Arc::new(Mutex::new(Vec::new()));
        let releases=Arc::new(AtomicU64::new(0));
        let recorded=events.clone();let released=releases.clone();
        let worker=InputWorker::with_factory(DesktopRect::new(0,0,640,480).unwrap(),move||Ok(Box::new(Fixture{events:recorded,releases:released}))).await.unwrap();
        worker.state.allowed.store(1,Ordering::Release);
        worker.request(true);
        assert!(worker.push(InputCommand::Key{usage:4,pressed:true}));
        assert!(!worker.push(InputCommand::Relative{dx:1,dy:1}));
        let mut pads=[None;uu_input::gamepad::SLOTS];pads[0]=Some(Default::default());
        worker.gamepads(pads,worker.state.epoch.load(Ordering::Acquire));
        assert!(worker.state.gamepads.lock().unwrap().is_none());
        wait(||worker.state.applied.load(Ordering::Acquire)==1).await;
        assert_eq!(events.lock().unwrap().len(),1);
        worker.state.allowed.store(2,Ordering::Release);
        assert!(!worker.push(InputCommand::Key{usage:4,pressed:true}));
        assert!(worker.push(InputCommand::Relative{dx:1,dy:1}));
        assert!(worker.push(InputCommand::ReleaseAll{}));
        wait(||worker.state.applied.load(Ordering::Acquire)==3).await;
        worker.stop().await;
        assert!(releases.load(Ordering::Acquire)>0);
    }
    #[tokio::test(flavor="multi_thread",worker_threads=2)]
    async fn another_viewer_cannot_inject_until_the_previous_owners_os_release_finishes() {
        struct BlockingRelease { entered: Option<oneshot::Sender<()>>, gate: Arc<(Mutex<bool>,std::sync::Condvar)>, armed: Arc<AtomicBool> }
        impl InputInjector for BlockingRelease {
            fn inject(&mut self,_:uu_input::InputEvent)->std::result::Result<(),uu_input::InputError>{self.armed.store(true,Ordering::Release);Ok(())}
            fn release_all(&mut self)->std::result::Result<(),uu_input::InputError>{
                if self.armed.swap(false,Ordering::AcqRel) {
                    if let Some(entered)=self.entered.take(){let _=entered.send(());}
                    let mut ready=self.gate.0.lock().unwrap();while !*ready {ready=self.gate.1.wait(ready).unwrap();}
                }Ok(())
            }
        }
        let authority=Arc::new(crate::input_ownership::Authority::default());
        let gate=Arc::new((Mutex::new(false),std::sync::Condvar::new()));
        let armed=Arc::new(AtomicBool::new(false));let state=armed.clone();let block=gate.clone();let (entered,wait_release)=oneshot::channel();
        let rect=DesktopRect::new(0,0,640,480).unwrap();
        let a=InputWorker::with_timing_factory(rect,move||Ok(Box::new(BlockingRelease{entered:Some(entered),gate:block,armed})),Arc::default(),authority.clone()).await.unwrap();
        let b=InputWorker::with_timing_factory(rect,||Ok(Box::new(Fixture{events:Arc::default(),releases:Arc::default()})),Arc::default(),authority).await.unwrap();
        a.request(true);assert!(a.push(InputCommand::Key{usage:4,pressed:true}));wait(||state.load(Ordering::Acquire)).await;
        b.request(true);assert!(!b.push(InputCommand::Relative{dx:1,dy:1}));
        a.request(false);wait_release.await.unwrap();
        b.request(true);let blocked=!b.push(InputCommand::Relative{dx:1,dy:1});
        *gate.0.lock().unwrap()=true;gate.1.notify_all();
        assert!(blocked,"ownership must outlive OS release");
        wait(||a.state.ownership.lock().unwrap().is_none()).await;
        b.request(true);assert!(b.push(InputCommand::Key{usage:4,pressed:true}));
        wait(||b.state.applied.load(Ordering::Acquire)==1).await;
        a.stop().await;
        assert!(b.state.enabled.load(Ordering::Acquire),"old owner cleanup cannot disable the new owner");
        b.stop().await;
    }
    #[tokio::test(flavor="multi_thread",worker_threads=4)]
    async fn encrypted_gamepad_snapshots_share_control_focus_but_not_the_reliable_key_queue(){
        encrypted_gamepad_snapshots(false).await;
    }
    #[tokio::test(flavor="multi_thread",worker_threads=4)]
    async fn typed_gamepad_snapshots_preserve_xbox_identity_and_control_generations(){
        encrypted_gamepad_snapshots(true).await;
    }
    async fn encrypted_gamepad_snapshots(typed:bool){
        use crate::native_media::NativePeer;
        use uu_input::gamepad::{Snapshot,VirtualGamepads};
        struct Pads(Arc<Mutex<Vec<Snapshot>>>);
        impl VirtualGamepads for Pads {
            fn update(&mut self,state:&Snapshot)->std::result::Result<(),uu_input::InputError>{self.0.lock().unwrap().push(*state);Ok(())}
            fn reset(&mut self)->std::result::Result<(),uu_input::InputError>{self.0.lock().unwrap().push([None;4]);Ok(())}
        }
        let host=HostControl::with_injector(true,Some(uu_capture::DesktopRect{x:0,y:0,width:640,height:480}),None,
            Some(Box::new(Fixture{events:Arc::default(),releases:Arc::default()})),Arc::default()).await;
        host.gamepad_types(typed);
        let label=if typed{crate::native_gamepad::LABEL_V2}else{crate::native_gamepad::LABEL};
        let version=if typed{2}else{1};
        let input_worker=host.worker.as_ref().unwrap();let weak=Arc::downgrade(&input_worker.state);
        let events=Arc::new(Mutex::new(Vec::new()));let record=events.clone();
        *input_worker.state.gamepads.lock().unwrap()=Some(crate::native_gamepad::Worker::with_factory(Arc::new(move||weak.upgrade().map_or((0,false),|s|(s.epoch.load(Ordering::Acquire),s.enabled.load(Ordering::Acquire)&&!s.stop.load(Ordering::Acquire)&&s.applied_epoch.load(Ordering::Acquire)==s.epoch.load(Ordering::Acquire)))),Box::new(move||Ok(Box::new(Pads(record.clone()))))).unwrap());
        let ip="127.0.0.1".parse().unwrap();let mut server=NativePeer::new(ip,None,true).await.unwrap();let mut client=NativePeer::new(ip,None,false).await.unwrap();host.attach(&server.pc);
        let channel=client.pc.create_data_channel("eclipse-control-v1",None).await.unwrap();
        let pad_channel=client.pc.create_data_channel(label,Some(webrtc::data_channel::data_channel_init::RTCDataChannelInit{ordered:Some(false),max_packet_life_time:Some(crate::native_gamepad::LIFETIME_MS),..Default::default()})).await.unwrap();
        let (input,receiver)=crate::native_input::channel(true);let stats=Arc::new(MediaStats::default());
        let pad_viewer=crate::native_gamepad::Viewer::start(pad_channel.clone(),stats.clone(),receiver.gamepads.clone(),receiver.requested.clone(),receiver.epoch.clone());
        let mut controls=ViewerControl::new(channel,stats.clone(),Some(receiver));
        server.remote(client.description(true).await.unwrap(),ip).await.unwrap();client.remote(server.description(false).await.unwrap(),ip).await.unwrap();
        let (a,b)=tokio::join!(client.connected(),server.connected());a.unwrap();b.unwrap();
        let controlling=tokio::spawn(async move{controls.run().await});
        wait(||stats.capabilities.allows(Capability::Input)).await;
        let mut state=[None;4];state[0]=Some(uu_input::gamepad::State{kind:if typed{uu_input::gamepad::Kind::Xbox360}else{Default::default()},buttons:1,timestamp_us:10000,gyro:Some([0.2,0.3,-0.4]),..Default::default()});
        input.send(InputCommand::Key{usage:4,pressed:true}).unwrap();wait(||host.counters().0>0).await;
        timeout(Duration::from_secs(3),async {loop{input.gamepads(state);if events.lock().unwrap().last()==Some(&state){break;}tokio::time::sleep(Duration::from_millis(8)).await;}}).await.expect("latest snapshots must arrive over the unreliable channel");
        let previous_generation=input_worker.state.epoch.load(Ordering::Acquire);
        input.set_enabled(false);wait(||!input_worker.state.enabled.load(Ordering::Acquire)).await;
        wait(||events.lock().unwrap().last()==Some(&[None;4])).await;
        let count=events.lock().unwrap().iter().filter(|s|s.iter().any(Option::is_some)).count();
        input.gamepads(state);tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(events.lock().unwrap().iter().filter(|s|s.iter().any(Option::is_some)).count(),count);
        input.set_enabled(true);wait(||stats.capabilities.allows(Capability::Input)).await;
        let applied=host.counters().0;input.send(InputCommand::Key{usage:4,pressed:false}).unwrap();wait(||host.counters().0>applied).await;
        state[0].as_mut().unwrap().buttons=2;
        timeout(Duration::from_secs(3),async{loop{input.gamepads(state);if events.lock().unwrap().last()==Some(&state){break;}tokio::time::sleep(Duration::from_millis(8)).await;}}).await.unwrap();
        drop(pad_viewer);
        let packet=|sequence,generation,state|serde_json::to_vec(&crate::native_gamepad::Packet::State{version,sequence,generation,state}).unwrap().into();
        let generation=input_worker.state.epoch.load(Ordering::Acquire);assert!(generation>previous_generation);
        state[0].as_mut().unwrap().buttons=4;
        timeout(Duration::from_secs(3),async{loop{pad_channel.send(&packet(100,generation,state)).await.unwrap();if events.lock().unwrap().last()==Some(&state){break;}tokio::time::sleep(Duration::from_millis(8)).await;}}).await.unwrap();
        let mut stale=state;stale[0].as_mut().unwrap().buttons=8;pad_channel.send(&packet(99,generation,stale)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;assert!(!events.lock().unwrap().iter().any(|s|*s==stale));
        pad_channel.send(&packet(200,previous_generation,stale)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;assert!(!events.lock().unwrap().iter().any(|s|*s==stale),"newer sequence cannot revive input from a previous focus generation");
        state[0].as_mut().unwrap().buttons=16;
        timeout(Duration::from_secs(3),async{loop{pad_channel.send(&packet(201,generation,state)).await.unwrap();if events.lock().unwrap().last()==Some(&state){break;}tokio::time::sleep(Duration::from_millis(8)).await;}}).await.unwrap();
        wait(||events.lock().unwrap().last()==Some(&[None;4])).await;
        host.stop().await;client.close().await;server.close().await;controlling.abort();let _=controlling.await;
    }
    #[tokio::test]
    async fn missing_host_permission_or_capture_geometry_never_opens_an_injector() {
        let rect = uu_capture::DesktopRect {
            x: 0,
            y: 0,
            width: 1280,
            height: 720,
        };
        assert!(HostControl::new(false, Some(rect), None)
            .await
            .worker
            .is_none());
        assert!(HostControl::new(true, None, None).await.worker.is_none());
    }
    #[test]
    fn control_messages_are_bounded_and_do_not_accept_interop_or_extra_commands() {
        assert!(parse(&vec![b'x'; MAX_CONTROL + 1]).is_none());
        assert!(parse(br#"{"action":"kbd_press","key":65}"#).is_none());
        assert!(parse(br#"{"type":"request_input","enabled":true,"command":"other"}"#).is_none());
    }
}
