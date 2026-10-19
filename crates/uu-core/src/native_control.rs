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

const MAX_CONTROL: usize = 1024;
const LEASE: Duration = Duration::from_secs(5);
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Packet {
    Ping { sequence: u64 },
    Pong { sequence: u64 },
    RequestInput { enabled: bool },
    InputStatus { available: bool, enabled: bool },
    Input { event: InputCommand },
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
}
struct InputWorker {
    state: Arc<WorkerState>,
    tx: SyncSender<(u64, InputCommand)>,
    task: Mutex<Option<std::thread::JoinHandle<()>>>,
}
impl InputWorker {
    async fn start(rect: DesktopRect) -> Result<Arc<Self>> {
        Self::with_factory(rect, uu_input::default_injector).await
    }
    async fn with_factory<F>(rect: DesktopRect, factory: F) -> Result<Arc<Self>>
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
        });
        let (tx, rx) = sync_channel::<(u64, InputCommand)>(128);
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
                        Ok((generation, command))
                            if generation == epoch && actor.enabled.load(Ordering::Acquire) =>
                        {
                            let result = match command.event(rect) {
                                Some(event) => injector.inject(event),
                                None => injector.release_all(),
                            };
                            if result.is_err() {
                                actor.errors.fetch_add(1, Ordering::Relaxed);
                                break;
                            }
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
        if !self.available() || !self.state.enabled.load(Ordering::Acquire) || !command.validate() {
            return false;
        }
        self.touch();
        if self
            .tx
            .try_send((self.state.epoch.load(Ordering::Acquire), command))
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
    worker: Option<Arc<InputWorker>>,
    registered: AtomicBool,
}
impl HostControl {
    pub async fn new(allow: bool, rect: Option<uu_capture::DesktopRect>) -> Arc<Self> {
        let worker = if let Some(rect) = rect.filter(|_| allow) {
            match DesktopRect::new(rect.x, rect.y, rect.width, rect.height) {
                Ok(rect) => match InputWorker::start(rect).await {
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
            worker,
            registered: AtomicBool::new(false),
        })
    }
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
                        match parse(&message.data) {
                            Some(Packet::Ping { sequence }) => {
                                if let Some(worker) = &host.worker {
                                    worker.touch();
                                }
                                let _ = send(&channel, Packet::Pong { sequence }).await;
                                let _ = send(&channel, host.status()).await;
                            }
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
                            _ => {}
                        }
                    })
                }));
            })
        }));
    }
    pub async fn stop(&self) {
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
            opened.control_open.store(1, Ordering::Release);
            notify.notify_one();
            Box::pin(async {})
        }));
        let closed = stats.clone();
        let notify = ready.clone();
        channel.on_close(Box::new(move || {
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
            match parse(&message.data) {
                Some(Packet::Pong { sequence }) => {
                    if let Some((mean, deviation)) = probes.lock().unwrap().reply_sequence(sequence)
                    {
                        incoming.rtt_us.store(mean, Ordering::Relaxed);
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
                _ => {}
            }
            Box::pin(async {})
        }));
        Self {
            channel,
            stats,
            input,
            ready,
            probe,
            requested,
            epoch,
        }
    }
    async fn input(&self, command: InputCommand) -> Result<()> {
        send(&self.channel, Packet::Input { event: command }).await?;
        self.stats
            .input_messages_sent
            .fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
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
                        NextInput::Mode(enabled)=>{
                            if !enabled{self.stats.capabilities.enable(Capability::Input,false);}
                            send(&self.channel,Packet::RequestInput{enabled}).await?;
                        },
                        NextInput::Reset=>{if self.stats.capabilities.allows(Capability::Input){self.input(InputCommand::ReleaseAll{}).await?;}},
                        NextInput::Command(envelope)=>{
                            if envelope.epoch==self.epoch.load(Ordering::Acquire)&&self.requested.load(Ordering::Acquire)&&self.stats.capabilities.allows(Capability::Input){self.input(envelope.command).await?;}
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
        assert!(HostControl::new(false, Some(rect)).await.worker.is_none());
        assert!(HostControl::new(true, None).await.worker.is_none());
    }
    #[test]
    fn control_messages_are_bounded_and_do_not_accept_interop_or_extra_commands() {
        assert!(parse(&vec![b'x'; MAX_CONTROL + 1]).is_none());
        assert!(parse(br#"{"action":"kbd_press","key":65}"#).is_none());
        assert!(parse(br#"{"type":"request_input","enabled":true,"command":"other"}"#).is_none());
    }
}
