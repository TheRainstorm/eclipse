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
    sync::{oneshot, Notify},
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
    available: AtomicBool,
    enabled: AtomicBool,
    stop: AtomicBool,
    epoch: AtomicU64,
    seen: Mutex<Instant>,
    applied: AtomicU64,
    errors: AtomicU64,
    timing:Arc<crate::native_timing::HostTiming>,
}
struct InputApplied {queue_us:u32,inject_us:u32}
struct InputJob {epoch:u64,command:InputCommand,queued:Instant,reply:Option<oneshot::Sender<InputApplied>>}
struct InputWorker {
    state: Arc<WorkerState>,
    tx: SyncSender<InputJob>,
    task: Mutex<Option<std::thread::JoinHandle<()>>>,
}
impl InputWorker {
    async fn start(rect: DesktopRect,timing:Arc<crate::native_timing::HostTiming>) -> Result<Arc<Self>> {
        Self::with_timing_factory(rect, uu_input::default_injector,timing).await
    }
    #[cfg(test)]
    async fn with_factory<F>(rect: DesktopRect, factory: F) -> Result<Arc<Self>>
    where F:FnOnce()->std::result::Result<Box<dyn InputInjector>,uu_input::InputError>+Send+'static {
        Self::with_timing_factory(rect,factory,Arc::default()).await
    }
    async fn with_timing_factory<F>(rect: DesktopRect, factory: F,timing:Arc<crate::native_timing::HostTiming>) -> Result<Arc<Self>>
    where
        F: FnOnce() -> std::result::Result<Box<dyn InputInjector>, uu_input::InputError>
            + Send
            + 'static,
    {
        let state = Arc::new(WorkerState {
            available: AtomicBool::new(false),
            enabled: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
            seen: Mutex::new(Instant::now()),
            applied: AtomicU64::new(0),
            errors: AtomicU64::new(0),
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
                    if actor.enabled.load(Ordering::Acquire)
                        && actor.seen.lock().unwrap().elapsed() > LEASE
                    {
                        actor.enabled.store(false, Ordering::Release);
                        actor.epoch.fetch_add(1, Ordering::AcqRel);
                    }
                    let current = actor.epoch.load(Ordering::Acquire);
                    if current != epoch {
                        if injector.release_all().is_err() {
                            actor.errors.fetch_add(1, Ordering::Relaxed);
                            break;
                        }
                        epoch = current;
                    }
                    match received {
                        Ok(job)
                            if job.epoch == epoch && actor.enabled.load(Ordering::Acquire) =>
                        {
                            let queue_us=job.queued.elapsed().as_micros().min(u32::MAX as u128) as u32;
                            let started=Instant::now();
                            let result = match job.command.event(rect) {
                                Some(event) => injector.inject(event),
                                None => injector.release_all(),
                            };
                            if result.is_err() {
                                actor.errors.fetch_add(1, Ordering::Relaxed);
                                break;
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
                if injector.release_all().is_err() {
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
    fn request(&self, enabled: bool) {
        self.touch();
        self.state.epoch.fetch_add(1, Ordering::AcqRel);
        self.state
            .enabled
            .store(enabled && self.available(), Ordering::Release);
    }
    fn push(&self, command: InputCommand) -> bool {
        self.push_with_ack(command,None)
    }
    fn push_with_ack(&self,command:InputCommand,reply:Option<oneshot::Sender<InputApplied>>)->bool {
        if !self.available() || !self.state.enabled.load(Ordering::Acquire) || !command.validate() {
            return false;
        }
        self.touch();
        if self
            .tx
            .try_send(InputJob {epoch:self.state.epoch.load(Ordering::Acquire),command,queued:Instant::now(),reply})
            .is_err()
        {
            self.state.stop.store(true, Ordering::Release);
            self.state.enabled.store(false, Ordering::Release);
            return false;
        }
        true
    }
    async fn stop(&self) {
        self.state.stop.store(true, Ordering::Release);
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
    displays:Mutex<Option<Arc<crate::native_displays::Host>>>,
    pending_display:Mutex<Option<u64>>,
    stopped:AtomicBool,
    pub closed:Notify,
    timing:Arc<crate::native_timing::HostTiming>,
    worker: Option<Arc<InputWorker>>,
    registered: AtomicBool,
    clipboard: Option<Arc<crate::clipboard::Clipboard>>,
    clipboard_registered: AtomicBool,
}
impl HostControl {
    #[cfg(test)]
    pub async fn new(
        allow: bool,
        rect: Option<uu_capture::DesktopRect>,
        clipboard: Option<Arc<crate::clipboard::Clipboard>>,
    ) -> Arc<Self> {
        Self::with_injector(allow,rect,clipboard,None).await
    }
    pub async fn with_injector(
        allow: bool,
        rect: Option<uu_capture::DesktopRect>,
        clipboard: Option<Arc<crate::clipboard::Clipboard>>,
        injector: Option<Box<dyn InputInjector>>,
    ) -> Arc<Self> {
        let timing:Arc<crate::native_timing::HostTiming>=Arc::default();
        let worker = if let Some(rect) = rect.filter(|_| allow) {
            match DesktopRect::new(rect.x, rect.y, rect.width, rect.height) {
                Ok(rect) => match match injector {
                    Some(injector)=>InputWorker::with_timing_factory(rect,move || Ok(injector),timing.clone()).await,
                    None=>InputWorker::start(rect,timing.clone()).await,
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
            displays:Mutex::new(None),
            pending_display:Mutex::new(None),
            stopped:AtomicBool::new(false),
            closed:Notify::new(),
            timing,
            worker,
            clipboard,
            clipboard_registered: AtomicBool::new(false),
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
                if channel.label() == crate::native_clipboard::LABEL
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
                            Some(Packet::Ping { sequence }) => {
                                if let Some(worker) = &host.worker {
                                    worker.touch();
                                }
                                let _ = send(&channel, Packet::Pong { sequence }).await;
                                let _ = send(&channel, host.status()).await;
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
        channel.on_message(Box::new(move |message| {
            if incoming.native_media_epoch.load(Ordering::Acquire)!=media_epoch{return Box::pin(async {});}
            match parse(&message.data) {
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
                    incoming
                        .native_input_available
                        .store(available, Ordering::Release);
                    let enabled = available && enabled && requested_copy.load(Ordering::Acquire);
                    if enabled && !incoming.capabilities.allows(Capability::Input) {
                        generation.fetch_add(1, Ordering::AcqRel);
                    }
                    incoming.capabilities.enable(Capability::Input, enabled);
                }
                Some(Packet::Telemetry{version:1,stages})=>incoming.native_timing.host(stages),
                Some(Packet::InputApplied{sequence,queue_us,inject_us})=>incoming.native_timing.applied(sequence,queue_us,inject_us),
                _ => {}
            }
            Box::pin(async {})
        }));
        Self {
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
                action=async{match self.input.as_mut(){Some(input)=>input.next().await,None=>std::future::pending().await}}=>{
                    match action {
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
