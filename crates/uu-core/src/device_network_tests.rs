//! Real TLS pairing and generated-video regression. No system desktop/input.
use crate::{
    device_auth::{PairApproval, PairingControl, PairingDisplay},
    device_trust::{Permissions, TrustStore},
    native,
    native_lan::{self as lan, Config},
    shared_host::{Access, SessionGate},
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{mpsc, watch};

fn config(root: &std::path::Path) -> Config {
    Config {
        name: "Generated host".into(),
        bind: "127.0.0.1:0".parse().unwrap(),
        discovery_bind: None,
        data_dir: root.join("lan"),
        source: native::CaptureSource::Synthetic {
            width: 320,
            height: 180,
            fps: 30,
        },
        fps: 30,
        bitrate_kbps: 400,
        encoder: native::EncoderPreference::Software,
        allow_input: false,
        audio: None,
        clipboard: None,
    }
}
fn access(
    root: &std::path::Path,
) -> (
    Arc<Access>,
    mpsc::UnboundedReceiver<crate::device_auth::PairRequest>,
) {
    let (send, receive) = mpsc::unbounded_channel();
    (
        Arc::new(Access {
            store: TrustStore::open(root.join("host-identity")).unwrap(),
            name: "Generated host".into(),
            pairing: Arc::new(PairingControl::new(send)),
            sessions: Arc::new(SessionGate::default()),
        }),
        receive,
    )
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unified_device_tls_pairing_saves_multiple_clients_and_revoke_stops_only_the_active_peer() {
    let root = std::env::temp_dir().join(format!("eclipse-network-{}", uuid::Uuid::new_v4()));
    let (access, mut requests) = access(&root);
    access.pairing.open();
    let (stop, rx) = watch::channel(false);
    let (events, _) = mpsc::unbounded_channel();
    let (server, _) = lan::Server::bind_with_access(config(&root), rx, events, access.clone())
        .await
        .unwrap();
    let endpoint = server.address().unwrap().to_string();
    let serving = tokio::spawn(server.run());
    let a = TrustStore::open(root.join("a")).unwrap();
    let c = TrustStore::open(root.join("c")).unwrap();
    for (store, name, permission) in [
        (&a, "A", Permissions::CONTROL),
        (&c, "C", Permissions::VIEW),
    ] {
        let display = PairingDisplay::default();
        let approve = async {
            let request = requests.recv().await.unwrap();
            request
                .respond
                .send(Some(
                    PairApproval::new(&display.code().unwrap(), permission).unwrap(),
                ))
                .ok();
        };
        let (connected, _) = tokio::join!(
            lan::connect_device(&endpoint, store, name, None, None, false, &display),
            approve
        );
        let connected = connected.unwrap();
        assert!(connected.invitation.is_none());
        assert!(display.code().is_none());
        assert_eq!(
            connected.peer.unwrap().id(),
            access.store.card("Generated host").id()
        );
    }
    assert_eq!(access.store.peers().unwrap().len(), 2);
    assert!(access.sessions.active_peer().is_none());
    access.pairing.close();
    let display = PairingDisplay::default();
    let expected = access.store.card("Generated host").id();
    let connected = lan::connect_device(&endpoint, &a, "A", Some(&expected), None, true, &display)
        .await
        .unwrap();
    assert!(display.code().is_none());
    let invite = connected.invitation.unwrap();
    let stats = Arc::new(crate::media::MediaStats::default());
    let (frames, mut receiver) = crate::media::frame_channel();
    let (viewer_stop, viewer_rx) = watch::channel(false);
    let observed = stats.clone();
    let viewing =
        tokio::spawn(async move { native::view(&invite, frames, observed, viewer_rx).await });
    tokio::time::timeout(Duration::from_secs(12), async {
        while stats.decoded_total.load(Ordering::Relaxed) < 12 {
            while receiver.try_recv().is_ok() {}
            assert!(!viewing.is_finished());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    access.revoke(&a.card("A").id()).unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(5), viewing)
        .await
        .unwrap();
    ended.unwrap().unwrap();
    assert_eq!(stats.errors.load(Ordering::Relaxed), 0);
    assert!(access
        .store
        .peer(&c.card("C").id())
        .unwrap()
        .unwrap()
        .inbound
        .is_some());
    let denied =
        lan::connect_device(&endpoint, &a, "A", Some(&expected), None, false, &display).await;
    assert!(denied.is_err());
    viewer_stop.send_replace(true);
    stop.send_replace(true);
    serving.await.unwrap().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unified_device_legacy_pinned_pairing_migrates_without_another_code_and_cannot_restore_revoked_access(
) {
    let root = std::env::temp_dir().join(format!("eclipse-migration-{}", uuid::Uuid::new_v4()));
    let (access, _requests) = access(&root);
    let (stop, rx) = watch::channel(false);
    let (events, mut incoming) = mpsc::unbounded_channel();
    let (server, handle) = lan::Server::bind_with_access(config(&root), rx, events, access.clone())
        .await
        .unwrap();
    handle.allow_pairing();
    let endpoint = server.address().unwrap().to_string();
    let serving = tokio::spawn(server.run());
    let approved = Arc::new(AtomicBool::new(false));
    let permit = approved.clone();
    let user = tokio::spawn(async move {
        while let Some(event) = incoming.recv().await {
            if let lan::Event::Pairing(request) = event {
                permit.store(true, Ordering::Release);
                request.decision.send(true).ok();
                break;
            }
        }
    });
    let id = uuid::Uuid::new_v4().to_string();
    let (progress, _codes) = mpsc::unbounded_channel();
    let old = lan::pair(&endpoint, &id, "Legacy client", progress)
        .await
        .unwrap();
    user.await.unwrap();
    assert!(approved.load(Ordering::Acquire));
    let client = TrustStore::open(root.join("client-identity")).unwrap();
    let display = PairingDisplay::default();
    let connected = lan::connect_device(
        &endpoint,
        &client,
        "Upgraded client",
        None,
        Some((&old, &id)),
        false,
        &display,
    )
    .await
    .unwrap();
    assert!(connected.peer.is_some());
    assert!(display.code().is_none());
    let peer = client.card("Upgraded client").id();
    access.revoke(&peer).unwrap();
    assert!(lan::connect(&old, &id).await.is_err());
    assert!(lan::connect_device(
        &endpoint,
        &client,
        "Upgraded client",
        None,
        Some((&old, &id)),
        false,
        &display
    )
    .await
    .is_err());
    stop.send_replace(true);
    serving.await.unwrap().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unified_device_internet_bootstrap_reuses_native_video_audio_after_short_code_authorization(
) {
    use crate::internet::{Connection, Settings, Transport};
    use anyhow::{Context, Result};
    use serde_json::Value;
    struct Memory {
        tx: mpsc::Sender<Value>,
        rx: mpsc::Receiver<Value>,
    }
    #[async_trait::async_trait]
    impl Transport for Memory {
        async fn send(&mut self, v: Value) -> Result<()> {
            self.tx.send(v).await.map_err(Into::into)
        }
        async fn receive(&mut self) -> Result<Value> {
            self.rx.recv().await.context("closed")
        }
        async fn close(&mut self) {
            self.rx.close();
        }
    }
    let root = std::env::temp_dir().join(format!("eclipse-internet-auth-{}", uuid::Uuid::new_v4()));
    let (access, mut requests) = access(&root);
    access.pairing.open();
    let (tx, rx) = mpsc::channel(8);
    let (tx2, rx2) = mpsc::channel(8);
    let viewer = Connection {
        transport: Box::new(Memory { tx, rx: rx2 }),
        session: "generated".into(),
        ice: crate::direct_ice::InternetIce::default(),
    };
    let host = Connection {
        transport: Box::new(Memory { tx: tx2, rx }),
        session: "generated".into(),
        ice: crate::direct_ice::InternetIce::default(),
    };
    let stats = Arc::new(crate::media::MediaStats::default());
    stats.audio.output_enabled.store(false, Ordering::Release);
    let (frames, mut received) = crate::media::frame_channel();
    let (viewer_stop, viewer_rx) = watch::channel(false);
    let (host_stop, host_rx) = watch::channel(false);
    let config = crate::Config {
        data_dir: root.join("viewer"),
        state_dir: root.join("viewer-state"),
        signaling_host: "unused.invalid".into(),
    };
    let observed = stats.clone();
    let viewing = tokio::spawn(async move {
        let settings = Settings {
            device_auth: true,
            ..Default::default()
        };
        crate::device_internet::view(
            viewer, &settings, &config, frames, observed, viewer_rx, None,
        )
        .await
    });
    let hosting = tokio::spawn(async move {
        let sharing = native::SharingConfig {
            source: native::CaptureSource::Synthetic {
                width: 320,
                height: 180,
                fps: 30,
            },
            fps: 30,
            bitrate_kbps: 500,
            encoder: native::EncoderPreference::Software,
            session_limit: Some(Duration::from_secs(15)),
            allow_input: false,
            audio: Some(native::AudioSource::Synthetic),
            clipboard: None,
        };
        crate::device_internet::serve_connection(host, sharing, &access, host_rx, None, None).await
    });
    let request = tokio::time::timeout(Duration::from_secs(8), requests.recv())
        .await
        .unwrap()
        .unwrap();
    request
        .respond
        .send(Some(
            PairApproval::new(&stats.pairing.code().unwrap(), Permissions::VIEW).unwrap(),
        ))
        .ok();
    let observation = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            while received.try_recv().is_ok() {}
            if stats.decoded_total.load(Ordering::Relaxed) >= 24
                && stats.audio.packets.load(Ordering::Relaxed) > 0
            {
                break;
            }
            if viewing.is_finished() || hosting.is_finished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    viewer_stop.send_replace(true);
    host_stop.send_replace(true);
    let viewed = viewing.await.unwrap();
    let hosted = hosting.await.unwrap();
    observation.unwrap();
    viewed.unwrap();
    let report = hosted.unwrap();
    assert!(report.sent >= 24);
    assert!(stats.decoded_total.load(Ordering::Relaxed) >= 24);
    assert!(stats.audio.packets.load(Ordering::Relaxed) > 0);
    assert_eq!(stats.errors.load(Ordering::Relaxed), 0);
    assert!(stats.pairing.code().is_none());
    std::fs::remove_dir_all(root).unwrap();
}
