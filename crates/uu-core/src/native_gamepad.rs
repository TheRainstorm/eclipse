//! Latest controller state, independent from reliable keyboard/control traffic.
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Condvar, Mutex,
};
use std::time::{Duration, Instant};
use uu_input::gamepad::{Snapshot, VirtualGamepads};
pub(crate) const LABEL: &str = "eclipse-gamepad-v1";
pub(crate) const LABEL_V2: &str = "eclipse-gamepad-v2";
const ATTRIBUTE:&str="a=x-eclipse-gamepad:types1";
pub(crate) fn typed_offered(sdp:&webrtc::peer_connection::sdp::session_description::RTCSessionDescription)->bool {
    let mut attributes=sdp.sdp.lines().map(str::trim).filter(|line|line.starts_with("a=x-eclipse-gamepad:"));
    attributes.next()==Some(ATTRIBUTE)&&attributes.next().is_none()
}
pub(crate) fn advertise(sdp:&mut webrtc::peer_connection::sdp::session_description::RTCSessionDescription) {
    let at=sdp.sdp.find("m=").unwrap_or(sdp.sdp.len());sdp.sdp.insert_str(at,&format!("{ATTRIBUTE}\r\n"));
}
pub(crate) fn version(channel:&webrtc::data_channel::RTCDataChannel)->u8{if channel.label()==LABEL_V2{2}else{1}}
pub(crate) const LIFETIME_MS: u16 = 50;
const STALE: Duration = Duration::from_millis(250);
#[derive(Default)]
pub struct Stats {
    pub local_count: AtomicUsize,
    pub local_status: Mutex<String>,
    pub motion: AtomicBool,
    pub active: AtomicBool,
    pub remote_count: AtomicUsize,
    pub status: Mutex<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Packet {
    State {
        version: u8,
        sequence: u64,
        generation: u64,
        state: Snapshot,
    },
    Status {
        version: u8,
        connected: usize,
        generation: u64,
        error: Option<String>,
    },
}
pub(crate) fn parse(data: &[u8]) -> Option<Packet> {
    if data.len() > 4096 {
        return None;
    }
    let packet: Packet = serde_json::from_slice(data).ok()?;
    let valid = match &packet {
        Packet::State {
            version: version @ (1|2),
            sequence,
            state,
            ..
        } => *sequence > 0 && state.iter().flatten().all(|s| s.valid() && (*version==2 || s.kind==uu_input::gamepad::Kind::DualShock4)),
        Packet::Status {
            version: 1|2,
            connected,
            error,
            ..
        } => {
            *connected <= uu_input::gamepad::SLOTS
                && error
                    .as_ref()
                    .is_none_or(|e| e.len() <= 512 && !e.chars().any(char::is_control))
        }
        _ => false,
    };
    valid.then_some(packet)
}

#[derive(Clone, Default)]
pub(crate) struct Status {
    pub connected: usize,
    pub error: Option<String>,
}
struct Pending {
    epoch: u64,
    state: Snapshot,
    received: Instant,
}
struct Mailbox {
    latest: Option<Pending>,
    reset: u64,
    completed: u64,
    reset_ok: bool,
    stop: bool,
    status: Status,
}
struct Shared {
    mail: Mutex<Mailbox>,
    wake: Condvar,
}
pub(crate) struct Worker {
    shared: Arc<Shared>,
    task: Mutex<Option<std::thread::JoinHandle<()>>>,
}
type Authorization = Arc<dyn Fn() -> (u64, bool) + Send + Sync>;
impl Worker {
    pub fn start(authorization: Authorization) -> anyhow::Result<Arc<Self>> {
        Self::with_factory(authorization, Box::new(uu_input::gamepad::default_backend))
    }
    pub(crate) fn with_factory(
        authorization: Authorization,
        factory: Box<dyn Fn() -> Result<Box<dyn VirtualGamepads>, uu_input::InputError> + Send>,
    ) -> anyhow::Result<Arc<Self>> {
        let shared = Arc::new(Shared {
            mail: Mutex::new(Mailbox {
                latest: None,
                reset: 0,
                completed: 0,
                reset_ok: true,
                stop: false,
                status: Status::default(),
            }),
            wake: Condvar::new(),
        });
        let actor = shared.clone();
        let task = std::thread::Builder::new()
            .name("native-gamepad".into())
            .spawn(move || {
                let mut backend: Option<Box<dyn VirtualGamepads>> = None;
                let mut last = None;
                let mut active = false;
                let mut retry = Instant::now();
                loop {
                    let (pending, reset, completed, stopping) = {
                        let mut mail = actor.mail.lock().unwrap();
                        if mail.latest.is_none() && mail.reset == mail.completed && !mail.stop {
                            mail = actor
                                .wake
                                .wait_timeout(
                                    mail,
                                    if active {
                                        Duration::from_millis(8)
                                    } else {
                                        Duration::from_millis(500)
                                    },
                                )
                                .unwrap()
                                .0;
                        }
                        (mail.latest.take(), mail.reset, mail.completed, mail.stop)
                    };
                    if stopping {
                        break;
                    }
                    let (epoch, authorized) = authorization();
                    let needs_reset = reset != completed
                        || !authorized
                        || last.is_some_and(|at: Instant| at.elapsed() > STALE);
                    let mut failure = None;
                    if needs_reset {
                        if let Some(backend) = &mut backend {
                            if let Err(e) = backend.reset() {
                                failure = Some(e.to_string());
                            }
                        }
                        active = false;
                        last = None;
                        let mut mail = actor.mail.lock().unwrap();
                        mail.completed = reset;
                        mail.reset_ok = failure.is_none();
                        mail.status.connected = 0;
                        if failure.is_some() {
                            mail.status.error = failure.clone();
                        }
                        actor.wake.notify_all();
                    }
                    if let Some(mut pending) = pending.filter(|p| {
                        !needs_reset
                            && p.epoch == epoch
                            && authorized
                            && p.received.elapsed() <= STALE
                    }) {
                        if pending.state.iter().any(Option::is_some)
                            && backend.is_none()
                            && Instant::now() >= retry
                        {
                            match factory() {
                                Ok(created) => backend = Some(created),
                                Err(e) => failure = Some(e.to_string()),
                            }
                        }
                        // Opening an OS device may block. Consume the newest full
                        // state and recheck authority after that operation, before
                        // submitting any buttons or motion to the driver.
                        let ready = {
                            let mut mail = actor.mail.lock().unwrap();
                            if let Some(latest) = mail.latest.take() {
                                pending = latest;
                            }
                            !mail.stop && mail.reset == reset
                        };
                        let (current_epoch, current_authorized) = authorization();
                        if ready
                            && current_authorized
                            && current_epoch == pending.epoch
                            && pending.received.elapsed() <= STALE
                        {
                            if let Some(backend) = &mut backend {
                                match backend.update(&pending.state) {
                                    Ok(()) => {
                                        active = pending.state.iter().any(Option::is_some);
                                        last = Some(pending.received);
                                        let mut mail = actor.mail.lock().unwrap();
                                        mail.status.connected =
                                            pending.state.iter().flatten().count();
                                        mail.status.error = None;
                                    }
                                    Err(e) => failure = Some(e.to_string()),
                                }
                            }
                        }
                    }
                    if let Some(backend) = &mut backend {
                        if let Err(e) = backend.poll() {
                            failure = Some(e.to_string());
                        }
                    }
                    if let Some(error) = failure {
                        if let Some(mut current) = backend.take() {
                            let _ = current.reset();
                        }
                        active = false;
                        last = None;
                        retry = Instant::now() + Duration::from_secs(3);
                        let error = error
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(128)
                            .collect::<String>();
                        let mut mail = actor.mail.lock().unwrap();
                        mail.status.connected = 0;
                        mail.status.error = Some(error);
                    }
                }
                if let Some(mut backend) = backend {
                    let _ = backend.reset();
                }
                let mut mail = actor.mail.lock().unwrap();
                mail.status.connected = 0;
                actor.wake.notify_all();
            })?;
        Ok(Arc::new(Self {
            shared,
            task: Mutex::new(Some(task)),
        }))
    }
    pub fn submit(&self, epoch: u64, state: Snapshot) {
        let mut mail = self.shared.mail.lock().unwrap();
        if mail.stop {
            return;
        }
        mail.latest = Some(Pending {
            epoch,
            state,
            received: Instant::now(),
        });
        self.shared.wake.notify_one();
    }
    pub fn status(&self) -> Status {
        self.shared.mail.lock().unwrap().status.clone()
    }
    /// Called before the session relinquishes its input ownership. Keyboard
    /// processing never waits for driver creation or normal gamepad updates.
    pub fn reset(&self) -> bool {
        let mut mail = self.shared.mail.lock().unwrap();
        mail.latest = None;
        mail.reset = mail.reset.wrapping_add(1);
        let request = mail.reset;
        self.shared.wake.notify_one();
        let end = Instant::now() + Duration::from_secs(3);
        while mail.completed != request && !mail.stop {
            let remaining = end.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                mail.stop = true;
                self.shared.wake.notify_one();
                return false;
            }
            mail = self.shared.wake.wait_timeout(mail, remaining).unwrap().0;
        }
        mail.completed == request && mail.reset_ok
    }
    pub fn stop(&self) {
        self.shared.mail.lock().unwrap().stop = true;
        self.shared.wake.notify_one();
        let mut task = self.task.lock().unwrap();
        if task.as_ref().is_some_and(|t| t.is_finished()) {
            let _ = task.take().unwrap().join();
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Recorder(Arc<Mutex<Vec<Snapshot>>>);
    impl VirtualGamepads for Recorder {
        fn update(&mut self, state: &Snapshot) -> Result<(), uu_input::InputError> {
            self.0.lock().unwrap().push(*state);
            Ok(())
        }
        fn reset(&mut self) -> Result<(), uu_input::InputError> {
            self.0.lock().unwrap().push([None; 4]);
            Ok(())
        }
    }
    struct OpeningRecorder {
        events: Arc<Mutex<Vec<Snapshot>>>,
        polled: Arc<AtomicBool>,
    }
    impl VirtualGamepads for OpeningRecorder {
        fn update(&mut self, state: &Snapshot) -> Result<(), uu_input::InputError> {
            self.events.lock().unwrap().push(*state);
            Ok(())
        }
        fn reset(&mut self) -> Result<(), uu_input::InputError> {
            self.events.lock().unwrap().push([None; 4]);
            Ok(())
        }
        fn poll(&mut self) -> Result<(), uu_input::InputError> {
            self.polled.store(true, Ordering::Release);
            Ok(())
        }
    }
    async fn wait(check: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !check() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn stale_and_unauthorized_snapshots_are_neutralized_before_reset_ack() {
        use std::sync::atomic::AtomicU64;
        let epoch = Arc::new(AtomicU64::new(1));
        let enabled = Arc::new(AtomicBool::new(true));
        let e = epoch.clone();
        let active = enabled.clone();
        let events = Arc::new(Mutex::new(Vec::new()));
        let record = events.clone();
        let worker = Worker::with_factory(
            Arc::new(move || (e.load(Ordering::Acquire), active.load(Ordering::Acquire))),
            Box::new(move || Ok(Box::new(Recorder(record.clone())))),
        )
        .unwrap();
        let mut state = [None; 4];
        state[0] = Some(uu_input::gamepad::State {
            buttons: 1,
            ..Default::default()
        });
        worker.submit(0, state);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(events.lock().unwrap().is_empty());
        worker.submit(1, state);
        wait(|| worker.status().connected == 1).await;
        enabled.store(false, Ordering::Release);
        assert!(worker.reset());
        assert_eq!(worker.status().connected, 0);
        assert_eq!(events.lock().unwrap().last(), Some(&[None; 4]));
        epoch.store(2, Ordering::Release);
        enabled.store(true, Ordering::Release);
        worker.submit(1, state);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(worker.status().connected, 0);
        worker.submit(2, state);
        wait(|| worker.status().connected == 1).await;
        wait(|| worker.status().connected == 0).await;
        assert_eq!(events.lock().unwrap().last(), Some(&[None; 4]));
        worker.stop();
    }
    #[tokio::test]
    async fn missing_driver_reports_error_without_blocking_release_of_keyboard_ownership() {
        let worker = Worker::with_factory(
            Arc::new(|| (1, true)),
            Box::new(|| {
                Err(uu_input::InputError::Unavailable(
                    "generated missing driver".into(),
                ))
            }),
        )
        .unwrap();
        let mut state = [None; 4];
        state[0] = Some(Default::default());
        worker.submit(1, state);
        wait(|| worker.status().error.is_some()).await;
        assert!(
            worker.reset(),
            "a failed creation owns no virtual device and must not poison keyboard control"
        );
        worker.stop();
    }
    #[tokio::test]
    async fn device_open_rechecks_authority_and_uses_latest_state() {
        use std::sync::atomic::AtomicU64;
        for revoke in [false, true] {
            let epoch = Arc::new(AtomicU64::new(1));
            let enabled = Arc::new(AtomicBool::new(true));
            let opened = Arc::new(AtomicBool::new(false));
            let polled = Arc::new(AtomicBool::new(false));
            let events = Arc::new(Mutex::new(Vec::new()));
            let (release, gate) = std::sync::mpsc::sync_channel(1);
            let (e, active, started, record) = (
                epoch.clone(),
                enabled.clone(),
                opened.clone(),
                events.clone(),
            );
            let ready = polled.clone();
            let worker = Worker::with_factory(
                Arc::new(move || (e.load(Ordering::Acquire), active.load(Ordering::Acquire))),
                Box::new(move || {
                    started.store(true, Ordering::Release);
                    gate.recv_timeout(Duration::from_secs(2)).unwrap();
                    Ok(Box::new(OpeningRecorder {
                        events: record.clone(),
                        polled: ready.clone(),
                    }))
                }),
            )
            .unwrap();
            let mut first = [None; 4];
            first[0] = Some(uu_input::gamepad::State {
                buttons: 1,
                ..Default::default()
            });
            worker.submit(1, first);
            wait(|| opened.load(Ordering::Acquire)).await;
            let mut latest = first;
            latest[0].as_mut().unwrap().buttons = 2;
            if revoke {
                enabled.store(false, Ordering::Release);
                epoch.store(2, Ordering::Release);
            } else {
                worker.submit(1, latest);
            }
            release.send(()).unwrap();
            wait(|| polled.load(Ordering::Acquire)).await;
            if revoke {
                assert!(events.lock().unwrap().iter().all(|state| state.iter().all(Option::is_none)),
                        "a device opening after control revocation must never receive old pressed input");
                assert!(worker.reset());
            } else {
                wait(|| worker.status().connected == 1).await;
                assert_eq!(
                    events.lock().unwrap().first(),
                    Some(&latest),
                    "new snapshots supersede input cached before driver creation"
                );
                assert!(worker.reset());
            }
            worker.stop();
        }
    }
    #[tokio::test]
    async fn slow_device_open_does_not_replay_expired_input() {
        let opened = Arc::new(AtomicBool::new(false));
        let polled = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(Vec::new()));
        let (release, gate) = std::sync::mpsc::sync_channel(1);
        let (started, record) = (opened.clone(), events.clone());
        let ready = polled.clone();
        let worker = Worker::with_factory(
            Arc::new(|| (1, true)),
            Box::new(move || {
                started.store(true, Ordering::Release);
                gate.recv_timeout(Duration::from_secs(2)).unwrap();
                Ok(Box::new(OpeningRecorder {
                    events: record.clone(),
                    polled: ready.clone(),
                }))
            }),
        )
        .unwrap();
        let mut state = [None; 4];
        state[0] = Some(Default::default());
        worker.submit(1, state);
        wait(|| opened.load(Ordering::Acquire)).await;
        tokio::time::sleep(STALE + Duration::from_millis(20)).await;
        release.send(()).unwrap();
        wait(|| polled.load(Ordering::Acquire)).await;
        assert!(events
            .lock()
            .unwrap()
            .iter()
            .all(|state| state.iter().all(Option::is_none)));
        assert!(worker.reset());
        worker.submit(1, state);
        wait(|| worker.status().connected == 1).await;
        assert!(worker.reset());
        worker.stop();
    }
    #[test]
    fn typed_controller_packets_require_v2_and_legacy_packets_omit_new_fields(){
        let mut state=[None;4];state[0]=Some(Default::default());
        let packet=|version,state|serde_json::to_vec(&Packet::State{version,sequence:1,generation:1,state}).unwrap();
        let legacy=packet(1,state);assert!(!String::from_utf8_lossy(&legacy).contains("kind"));assert!(parse(&legacy).is_some());
        state[0].as_mut().unwrap().kind=uu_input::gamepad::Kind::Xbox360;
        assert!(parse(&packet(1,state)).is_none());assert!(parse(&packet(2,state)).is_some());
        let mut sdp=webrtc::peer_connection::sdp::session_description::RTCSessionDescription::default();
        sdp.sdp="v=0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n".into();
        assert!(!typed_offered(&sdp));advertise(&mut sdp);assert!(typed_offered(&sdp));advertise(&mut sdp);assert!(!typed_offered(&sdp));
    }
    #[test]
    fn packet_parser_bounds_version_slots_and_nonfinite_sensor_values() {
        let mut state = [None; 4];
        state[0] = Some(Default::default());
        let bytes = serde_json::to_vec(&Packet::State {
            version: 1,
            sequence: 1,
            generation: 1,
            state,
        })
        .unwrap();
        assert!(parse(&bytes).is_some());
        assert!(parse(br#"{"type":"state","version":1,"sequence":1,"state":[]}"#).is_none());
        assert!(parse(
            &serde_json::to_vec(&Packet::State {
                version: 3,
                sequence: 1,
                generation: 1,
                state
            })
            .unwrap()
        )
        .is_none());
        assert!(parse(
            &serde_json::to_vec(&Packet::State {
                version: 1,
                sequence: 0,
                generation: 1,
                state
            })
            .unwrap()
        )
        .is_none());
    }
}

pub(crate) struct Viewer {
    stop: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}
impl Viewer {
    pub fn start(
        channel: Arc<webrtc::data_channel::RTCDataChannel>,
        stats: Arc<crate::media::MediaStats>,
        mut samples: tokio::sync::watch::Receiver<Snapshot>,
        requested: Arc<AtomicBool>,
        input_epoch: Arc<std::sync::atomic::AtomicU64>,
    ) -> Self {
        let media_epoch = stats.native_media_epoch.load(Ordering::Acquire);
        stats.gamepad.remote_count.store(0, Ordering::Release);
        *stats.gamepad.status.lock().unwrap() = "等待被控端手柄状态".into();
        let notify = Arc::new(tokio::sync::Notify::new());
        let opened = notify.clone();
        channel.on_open(Box::new(move || {
            opened.notify_one();
            Box::pin(async {})
        }));
        let version=version(&channel);
        let generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let current_generation = generation.clone();
        let incoming = stats.clone();
        channel.on_message(Box::new(move |message| {
            if incoming.native_media_epoch.load(Ordering::Acquire) == media_epoch {
                if let Some(Packet::Status {
                    version: packet_version,
                    connected,
                    generation,
                    error,
                    ..
                }) = parse(&message.data)
                {
                    if packet_version!=version{return Box::pin(async{});}
                    let previous = current_generation.fetch_max(generation, Ordering::AcqRel);
                    if generation < previous {
                        return Box::pin(async {});
                    }
                    incoming
                        .gamepad
                        .remote_count
                        .store(connected, Ordering::Release);
                    *incoming.gamepad.status.lock().unwrap() = error.unwrap_or_else(|| {
                        if connected > 0 {
                            if version==1 {format!("已转发 {connected} 个手柄（旧版被控端统一模拟 DS4）")}else{format!("已转发 {connected} 个手柄")}
                        } else {
                            "等待本地手柄或控制权".into()
                        }
                    });
                }
            }
            Box::pin(async {})
        }));
        let closed = stats.clone();
        channel.on_close(Box::new(move || {
            if closed.native_media_epoch.load(Ordering::Acquire) == media_epoch {
                closed.gamepad.remote_count.store(0, Ordering::Release);
                *closed.gamepad.status.lock().unwrap() = "手柄通道已关闭或被控端不支持".into();
            }
            Box::pin(async {})
        }));
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            let mut sequence = 0u64;
            loop {
                tokio::select! {
                    _=stopping.changed()=>break,
                    _=notify.notified()=>{},
                    changed=samples.changed()=>if changed.is_err(){break;},
                }
                if stats.native_media_epoch.load(Ordering::Acquire) != media_epoch {
                    break;
                }
                let sample_epoch = input_epoch.load(Ordering::Acquire);
                let grant = generation.load(Ordering::Acquire);
                let mut state = if requested.load(Ordering::Acquire)
                    && stats
                        .capabilities
                        .allows(crate::capabilities::Capability::Input)
                {
                    *samples.borrow_and_update()
                } else {
                    samples.borrow_and_update();
                    [None; uu_input::gamepad::SLOTS]
                };
                if version==1 {for pad in state.iter_mut().flatten(){pad.kind=uu_input::gamepad::Kind::DualShock4;}}
                if channel.ready_state()
                    != webrtc::data_channel::data_channel_state::RTCDataChannelState::Open
                {
                    continue;
                }
                // Keep at most a few small snapshots queued. A newer full state
                // repairs lost releases; stale sensor data is never replayed.
                if channel.buffered_amount().await > 4096 {
                    continue;
                }
                if input_epoch.load(Ordering::Acquire) != sample_epoch {
                    continue;
                }
                sequence = sequence.wrapping_add(1).max(1);
                if let Ok(bytes) = serde_json::to_vec(&Packet::State {
                    version,
                    sequence,
                    generation: grant,
                    state,
                }) {
                    let _ = tokio::time::timeout(
                        Duration::from_millis(20),
                        channel.send(&bytes.into()),
                    )
                    .await;
                }
            }
        });
        Self { stop, task }
    }
}
impl Drop for Viewer {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        self.task.abort();
    }
}
