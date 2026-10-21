//! Only owned loopback listeners and generated video. No desktop/audio/input.
use super::*;
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
        let (stop, rx) = watch::channel(false);
        let (events, event_rx) = mpsc::unbounded_channel();
        let config = Config {
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
            clipboard: None,
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
        self.handle.allow_pairing();
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
    assert!(!inspect(&device.endpoint).await.unwrap().pairing_open);
    let invitation = connect(&device, &id).await.unwrap();
    assert!(
        connect(&device, &id).await.is_err(),
        "one host cannot start conflicting capture sessions"
    );
    host.handle.revoke(&id).unwrap();
    timeout(Duration::from_secs(3), async {
        loop {
            if host
                .handle
                .state
                .active
                .lock()
                .unwrap()
                .as_ref()
                .is_none_or(|s| s.finished.load(Ordering::Acquire))
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
    assert!(host.handle.state.active.lock().unwrap().is_none());
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
