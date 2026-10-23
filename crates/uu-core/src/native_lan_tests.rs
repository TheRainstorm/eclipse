//! Only owned loopback listeners and generated video. No desktop/audio/input.
use super::*;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn native_lan_port_conflicts_are_actionable_and_custom_discovery_advertises_service_port() {
    let root = std::env::temp_dir().join(format!("eclipse-port-test-{}", uuid::Uuid::new_v4()));
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = tcp.local_addr().unwrap();
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let (stop, rx) = watch::channel(false);
    let (events, mut incoming) = mpsc::unbounded_channel();
    let mut config = Config {video_policy:None,codec:Default::default(),rate_control:Default::default(),
        name: "Port fixture".into(), bind: address,
        discovery_bind: Some(udp.local_addr().unwrap()), data_dir: root.clone(),
        source: native::CaptureSource::Synthetic { width:64, height:64, fps:30 },
        fps:30, bitrate_kbps:300, encoder:native::EncoderPreference::Software,
        allow_input:false, audio:None, clipboard:None,
    };
    let error = Server::bind(config.clone(), rx.clone(), events.clone()).await.err().unwrap();
    let message = format!("{error:#}");
    assert!(message.contains(&format!("TCP {}", address.port())));
    assert!(message.contains("更换服务端口"));
    drop(tcp);
    let (server, handle) = Server::bind(config.clone(), rx.clone(), events.clone()).await.unwrap();
    assert_eq!(server.address().unwrap(), address);
    assert!(server.discovery.is_none(), "UDP conflict must keep manual TCP connection available");
    match incoming.recv().await.unwrap() {
        Event::Warning(message) => {
            assert!(message.contains(&format!("UDP {}", udp.local_addr().unwrap().port())));
            assert!(message.contains(&format!("IP:{}", address.port())));
        }
        _ => panic!("discovery failure must be visible"),
    }
    drop(server); drop(handle); drop(udp);
    config.discovery_bind = Some("127.0.0.1:0".parse().unwrap());
    let (server, handle) = Server::bind(config, rx, events).await.unwrap();
    let discovery = server.discovery.as_ref().unwrap().local_addr().unwrap();
    let task = tokio::spawn(server.run());
    let found = crate::native_discovery::discover(&[discovery], Duration::from_millis(150)).await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].address.port(), address.port());
    stop.send_replace(true);
    timeout(Duration::from_secs(3), task).await.unwrap().unwrap().unwrap();
    drop(handle);
    std::fs::remove_dir_all(root).unwrap();
}

struct Fixture {
    root: Option<PathBuf>,
    handle: Handle,
    stop: watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<Result<()>>>,
    events: mpsc::UnboundedReceiver<Event>,
    endpoint: String,
}
impl Fixture {
    async fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("eclipse-lan-fixture-{}", uuid::Uuid::new_v4()));
        Self::at(root).await
    }
    async fn at(root: PathBuf) -> Self {
        Self::configured(root, None).await
    }
    async fn configured(root: PathBuf, clipboard: Option<ClipboardFactory>) -> Self {
        let (stop, rx) = watch::channel(false);
        let (events, event_rx) = mpsc::unbounded_channel();
        let config = Config {video_policy:None,codec:Default::default(),rate_control:Default::default(),
            name: "Generated fixture".into(),
            bind: "127.0.0.1:0".parse().unwrap(),
            discovery_bind: None,
            data_dir: root.clone(),
            source: native::CaptureSource::Synthetic {
                width: 64,
                height: 64,
                fps: 30,
            },
            fps: 30,
            bitrate_kbps: 300,
            encoder: native::EncoderPreference::Software,
            allow_input: false,
            audio: None,
            clipboard,
        };
        let (server, handle) = Server::bind(config, rx, events).await.unwrap();
        let endpoint = server.address().unwrap().to_string();
        let task = tokio::spawn(server.run());
        Self {
            root: Some(root),
            handle,
            stop,
            task: Some(task),
            events: event_rx,
            endpoint,
        }
    }
    async fn pairing(&mut self) -> PairingRequest {
        timeout(Duration::from_secs(3), async {
            loop {
                if let Some(Event::Pairing(request)) = self.events.recv().await {
                    return request;
                }
            }
        })
        .await
        .unwrap()
    }
    async fn pair(&mut self, id: &str) -> Device {
        assert!(inspect(&self.endpoint).await.unwrap().pairing_open);
        let target = self.endpoint.clone();
        let id = id.to_owned();
        let (progress, mut codes) = mpsc::unbounded_channel();
        let client =
            tokio::spawn(async move { pair(&target, &id, "Generated viewer", progress).await });
        let request = self.pairing().await;
        let client_code = timeout(Duration::from_secs(3), codes.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            request.code, client_code,
            "TLS exporters must agree before a human confirms"
        );
        request.decision.send(true).unwrap();
        client.await.unwrap().unwrap()
    }
    async fn finish(mut self) {
        self.stop.send_replace(true);
        if let Some(task) = self.task.take() {
            timeout(Duration::from_secs(5), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        if let Some(task) = self.task.take() {
            task.abort();
        }
        if let Some(root) = self.root.take() {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

#[tokio::test]
async fn matching_tls_comparison_pairs_then_saved_authorization_starts_generated_stream() {
    let mut host = Fixture::new().await;
    let client_id = uuid::Uuid::new_v4().to_string();
    let device = host.pair(&client_id).await;
    assert_eq!(host.handle.clients().len(), 1);
    let invitation = connect(&device, &client_id).await.unwrap();
    assert!(invitation.address().ip().is_loopback());
    assert_eq!(
        invitation.observed_client_ip(),
        Some("127.0.0.1".parse().unwrap())
    );
    let (frames, _receiver) = crate::media::frame_channel();
    let stats = Arc::new(crate::media::MediaStats::default());
    let (stop, rx) = watch::channel(false);
    let measured = stats.clone();
    let viewing =
        tokio::spawn(async move { native::view(&invitation, frames, measured, rx).await });
    timeout(Duration::from_secs(10), async {
        while stats.decoded_total.load(Ordering::Acquire) < 8 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(stats.errors.load(Ordering::Acquire), 0);
    let session_id = timeout(Duration::from_secs(3), async {
        let mut session_id = None;
        loop {
            match host.events.recv().await.unwrap() {
                Event::SessionStarted { id, client } => {
                    assert_eq!(client, client_id);
                    session_id = Some(id);
                }
                Event::SessionProgress {
                    id,
                    status:
                        native::HostStatus::Streaming {
                            width,
                            height,
                            fps,
                            encoder,
                            ..
                        },
                } => {
                    assert_eq!(session_id.as_deref(), Some(id.as_str()));
                    assert_eq!((width, height, fps), (64, 64, 30));
                    assert!(encoder.starts_with("openh264"));
                    break id;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    host.handle.stop_session();
    timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Event::SessionEnded(id)) = host.events.recv().await {
                assert_eq!(id, session_id);
                break;
            }
        }
    })
    .await
    .unwrap();
    // Stopping capture retains the persistent listener and pairing grant.
    assert_eq!(host.handle.clients().len(), 1);
    assert!(connect(&device, &client_id).await.is_ok());
    stop.send_replace(true);
    timeout(Duration::from_secs(5), viewing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    host.finish().await;
}
#[tokio::test]
async fn pairing_requires_host_approval_and_rejection_does_not_create_authorization() {
    let mut host = Fixture::new().await;
    host.handle.close_pairing();
    let id = uuid::Uuid::new_v4().to_string();
    let (progress, _codes) = mpsc::unbounded_channel();
    assert!(pair(&host.endpoint, &id, "Generated", progress)
        .await
        .is_err());
    host.handle.allow_pairing();
    let target = host.endpoint.clone();
    let (progress, _codes) = mpsc::unbounded_channel();
    let client = tokio::spawn(async move { pair(&target, &id, "Generated", progress).await });
    let request = host.pairing().await;
    request.decision.send(false).unwrap();
    assert!(client.await.unwrap().is_err());
    assert!(host.handle.clients().is_empty());
    host.finish().await;
}
#[tokio::test]
async fn wrong_tokens_revocation_and_changed_server_identity_are_rejected() {
    let mut host = Fixture::new().await;
    let id = uuid::Uuid::new_v4().to_string();
    let device = host.pair(&id).await;
    let mut wrong = device.clone();
    wrong.token = store::fresh_token();
    assert!(connect(&wrong, &id).await.is_err());
    let other = Fixture::new().await;
    let mut changed = device.clone();
    changed.endpoint = other.endpoint.clone();
    let error = connect(&changed, &id).await.unwrap_err().to_string();
    assert!(error.contains("身份已变化"));
    assert!(other.handle.clients().is_empty());
    host.handle.revoke(&id).unwrap();
    assert!(connect(&device, &id).await.is_err());
    assert!(host.handle.clients().is_empty());
    other.finish().await;
    host.finish().await;
}
#[tokio::test]
async fn stopping_host_cancels_pending_confirmation_promptly() {
    let mut host = Fixture::new().await;
    host.handle.allow_pairing();
    let endpoint = host.endpoint.clone();
    let id = uuid::Uuid::new_v4().to_string();
    let (progress, _codes) = mpsc::unbounded_channel();
    let client = tokio::spawn(async move { pair(&endpoint, &id, "Generated", progress).await });
    let request = host.pairing().await;
    host.stop.send_replace(true);
    let result = timeout(Duration::from_secs(3), client)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_err());
    drop(request);
    host.finish().await;
}

#[tokio::test]
async fn saved_pairing_survives_server_restart_and_sessions_are_bounded() {
    let mut host = Fixture::new().await;
    let id = uuid::Uuid::new_v4().to_string();
    let mut device = host.pair(&id).await;
    let root = host.root.take().unwrap();
    host.finish().await;
    let host = Fixture::at(root).await;
    device = verify_address(&device, &host.endpoint).await.unwrap();
    assert!(inspect(&device.endpoint).await.unwrap().pairing_open);
    let invitation = connect(&device, &id).await.unwrap();
    for _ in 1..crate::shared_host::MAX_SESSIONS { connect(&device, &id).await.unwrap(); }
    assert!(connect(&device, &id).await.is_err(), "capture concurrency must be bounded");
    host.handle.revoke(&id).unwrap();
    timeout(Duration::from_secs(3), async {
        loop {
            if host
                .handle
                .state
                .active
                .lock()
                .unwrap()
                .is_empty()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(crate::native_pairing::connect(&invitation).await.is_err());
    host.finish().await;
}

#[tokio::test]
async fn new_address_must_prove_the_saved_identity_without_starting_a_session() {
    let mut host = Fixture::new().await;
    let id = uuid::Uuid::new_v4().to_string();
    let device = host.pair(&id).await;
    let other = Fixture::new().await;
    assert!(verify_address(&device, &other.endpoint).await.is_err());
    let updated = verify_address(&device, &host.endpoint).await.unwrap();
    assert_eq!(updated.certificate, device.certificate);
    assert_eq!(updated.token, device.token);
    assert!(host.handle.state.active.lock().unwrap().is_empty());
    assert!(other.handle.clients().is_empty());
    other.finish().await;
    host.finish().await;
}

#[tokio::test]
async fn human_confirmation_is_not_limited_by_the_short_rpc_read_timeout() {
    let mut host = Fixture::new().await;
    host.handle.allow_pairing();
    let endpoint = host.endpoint.clone();
    let id = uuid::Uuid::new_v4().to_string();
    let (progress, mut codes) = mpsc::unbounded_channel();
    let client =
        tokio::spawn(async move { pair(&endpoint, &id, "Generated viewer", progress).await });
    let request = host.pairing().await;
    assert_eq!(codes.recv().await.unwrap(), request.code);
    tokio::time::sleep(IO_WAIT + Duration::from_millis(200)).await;
    request.decision.send(true).unwrap();
    assert!(client.await.unwrap().is_ok());
    host.finish().await;
}

#[tokio::test]
async fn clipboard_is_opt_in_and_reconnect_creates_an_isolated_fresh_backend() {
    use crate::{capabilities::Capability, clipboard::Clipboard};
    let created = Arc::new(Mutex::new(Vec::<Arc<Clipboard>>::new()));
    let instances = created.clone();
    let factory: ClipboardFactory = Arc::new(move |_id| {
        let state = Arc::new(Clipboard::default());
        state.enabled.store(true, Ordering::Release);
        state.available.store(true, Ordering::Release);
        instances.lock().unwrap().push(state.clone());
        Ok(state)
    });
    let root = std::env::temp_dir().join(format!("eclipse-lan-clipboard-{}", uuid::Uuid::new_v4()));
    let mut host = Fixture::configured(root, Some(factory)).await;
    let id = uuid::Uuid::new_v4().to_string();
    let device = host.pair(&id).await;
    for round in 0..2 {
        let invite = connect(&device, &id).await.unwrap();
        let source = created.lock().unwrap()[round].clone();
        if round > 0 {
            assert!(!Arc::ptr_eq(&source, &created.lock().unwrap()[round - 1]));
        }
        let stats = Arc::new(crate::media::MediaStats::default());
        stats.clipboard.available.store(true, Ordering::Release);
        let (frames, _receiver) = crate::media::frame_channel();
        let (stop, rx) = watch::channel(false);
        let measured = stats.clone();
        let client = tokio::spawn(async move { native::view(&invite, frames, measured, rx).await });
        timeout(Duration::from_secs(6), async {
            while !stats.capabilities.allows(Capability::Clipboard)
                || stats.decoded_total.load(Ordering::Acquire) < 8
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            !source.ready(),
            "host permission alone cannot activate clipboard"
        );
        stats.clipboard.enabled.store(true, Ordering::Release);
        stats.clipboard.active.store(true, Ordering::Release);
        timeout(Duration::from_secs(5), async {
            while !source.ready() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            loop {
                *stats.clipboard.local.lock().unwrap() = Some("generated viewer text 中文".into());
                if source.remote.lock().unwrap().as_deref() == Some("generated viewer text 中文")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            loop {
                *source.local.lock().unwrap() = Some("generated host text".into());
                if stats.clipboard.remote.lock().unwrap().as_deref() == Some("generated host text")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(stats.errors.load(Ordering::Acquire), 0);
        stop.send_replace(true);
        timeout(Duration::from_secs(5), client)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        timeout(Duration::from_secs(5), async {
            loop {
                if let Some(Event::SessionEnded(_)) = host.events.recv().await {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(source.stopped.load(Ordering::Acquire));
        assert!(source.local.lock().unwrap().is_none() && source.remote.lock().unwrap().is_none());
    }
    assert_eq!(created.lock().unwrap().len(), 2);
    host.finish().await;
}

#[tokio::test]
async fn clipboard_backend_failure_releases_the_session_slot_without_creating_a_media_listener() {
    let root = std::env::temp_dir().join(format!(
        "eclipse-lan-clipboard-fail-{}",
        uuid::Uuid::new_v4()
    ));
    let factory: ClipboardFactory = Arc::new(|_| anyhow::bail!("generated backend error"));
    let mut host = Fixture::configured(root, Some(factory)).await;
    let id = uuid::Uuid::new_v4().to_string();
    let device = host.pair(&id).await;
    for _ in 0..2 {
        assert!(connect(&device, &id)
            .await
            .unwrap_err()
            .to_string()
            .contains("generated backend error"));
        assert!(host
            .handle
            .state
            .active
            .lock()
            .unwrap()
            .is_empty());
    }
    host.finish().await;
}
