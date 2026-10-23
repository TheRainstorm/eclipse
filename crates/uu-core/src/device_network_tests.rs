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
    Config {video_policy:None,codec:Default::default(),rate_control:Default::default(),
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
async fn unified_device_wss_listener_republishes_and_reconnects_with_the_same_grant_after_stream_close(
) {
    use crate::{
        device_internet::EndpointStatus,
        internet::{self, Provider, Settings},
    };
    let root = std::env::temp_dir().join(format!("eclipse-device-wss-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert = root.join("certificate.pem");
    let key = root.join("private.pem");
    let token = root.join("token");
    std::fs::write(&cert, certificate.cert.pem()).unwrap();
    std::fs::write(&key, certificate.key_pair.serialize_pem()).unwrap();
    internet::AccessKey::create(&token).unwrap();
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reserved.local_addr().unwrap();
    drop(reserved);
    let (service_stop, service_rx) = watch::channel(false);
    let service = tokio::spawn(internet::server::serve(
        internet::server::Config {
            bind: address,
            certificate: cert.clone(),
            private_key: key,
            token_file: token.clone(),
            stun: vec![],
        },
        service_rx,
    ));
    let settings = Settings {
        provider: Provider::SelfHosted,
        device_auth: true,
        server: format!("localhost:{}", address.port()),
        server_ca: Some(cert),
        server_token: token,
        device: "generated-host".into(),
        ..Default::default()
    };
    let cfg = |part: &str| crate::Config {
        data_dir: root.join(part),
        state_dir: root.join(format!("{part}-state")),
        signaling_host: "unused.invalid".into(),
    };
    let (access, mut requests) = access(&root);
    access.pairing.open();
    let (host_stop, host_rx) = watch::channel(false);
    let (status, mut endpoint) = watch::channel(EndpointStatus::Starting);
    let (progress, _) = watch::channel(native::HostStatus::Starting);
    let sharing = native::SharingConfig {session:None,video_policy:None,codec:Default::default(),rate_control:Default::default(),
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
    let hosting = tokio::spawn(crate::device_internet::listen(
        settings.clone(),
        cfg("host"),
        sharing,
        access.clone(),
        host_rx,
        status,
        progress,
        None,
    ));
    for attempt in 0..2 {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if matches!(*endpoint.borrow_and_update(), EndpointStatus::Ready) {
                    break;
                }
                endpoint.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let stats = Arc::new(crate::media::MediaStats::default());
        stats.audio.output_enabled.store(false, Ordering::Release);
        let (frames, mut received) = crate::media::frame_channel();
        let (viewer_stop, viewer_rx) = watch::channel(false);
        let observed = stats.clone();
        let viewer_settings = settings.clone();
        let config = cfg("viewer");
        let viewing = tokio::spawn(async move {
            internet::view(viewer_settings, config, frames, observed, viewer_rx, None).await
        });
        if attempt == 0 {
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
        }
        tokio::time::timeout(Duration::from_secs(12), async {
            while stats.decoded_total.load(Ordering::Relaxed) < 12
                || stats.audio.packets.load(Ordering::Relaxed) == 0
            {
                while received.try_recv().is_ok() {}
                assert!(
                    !viewing.is_finished(),
                    "viewer ended before generated media"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(stats.errors.load(Ordering::Relaxed), 0);
        assert!(stats.pairing.code().is_none());
        let second_stats=Arc::new(crate::media::MediaStats::default());second_stats.audio.output_enabled.store(false,Ordering::Release);
        let (second_frames,mut second_received)=crate::media::frame_channel();
        let (second_stop,second_rx)=watch::channel(false);
        let second_settings=settings.clone();let second_cfg=cfg("viewer");let second_observed=second_stats.clone();
        let second=tokio::spawn(async move{internet::view(second_settings,second_cfg,second_frames,second_observed,second_rx,None).await});
        tokio::time::timeout(Duration::from_secs(12),async {
            while second_stats.decoded_total.load(Ordering::Acquire)<12 {
                while received.try_recv().is_ok(){}while second_received.try_recv().is_ok(){}
                assert!(!viewing.is_finished() && !second.is_finished(),"second WSS viewer must not replace the first");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        assert_eq!(access.sessions.sessions().len(),2);
        assert_eq!(second_stats.errors.load(Ordering::Acquire),0);
        second_stop.send_replace(true);
        tokio::time::timeout(Duration::from_secs(5),second).await.unwrap().unwrap().unwrap();
        let before=stats.decoded_total.load(Ordering::Acquire);
        tokio::time::timeout(Duration::from_secs(6),async {
            while stats.decoded_total.load(Ordering::Acquire)<before+12 {
                while received.try_recv().is_ok(){}assert!(!viewing.is_finished(),"leaving one WSS session cannot disconnect another");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        viewer_stop.send_replace(true);
        tokio::time::timeout(Duration::from_secs(5), viewing)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(7), async {
            while access.sessions.active_peer().is_some() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        access.pairing.close();
        assert_eq!(access.store.peers().unwrap().len(), 1);
    }
    assert!(
        requests.try_recv().is_err(),
        "trusted reconnect must not ask for another code"
    );
    host_stop.send_replace(true);
    tokio::time::timeout(Duration::from_secs(5), hosting)
        .await
        .unwrap()
        .unwrap();
    service_stop.send_replace(true);
    tokio::time::timeout(Duration::from_secs(5), service)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unified_device_tls_concurrent_streams_survive_revoking_another_viewer() {
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
    let c_connected=lan::connect_device(&endpoint,&c,"C",Some(&expected),None,true,&PairingDisplay::default()).await.unwrap();
    let c_invite=c_connected.invitation.unwrap();
    let c_stats=Arc::new(crate::media::MediaStats::default());
    let (c_frames,mut c_received)=crate::media::frame_channel();
    let (c_stop,c_rx)=watch::channel(false);let c_observed=c_stats.clone();
    let c_viewing=tokio::spawn(async move{native::view(&c_invite,c_frames,c_observed,c_rx).await});
    tokio::time::timeout(Duration::from_secs(12),async {
        while c_stats.decoded_total.load(Ordering::Acquire)<12 {
            while c_received.try_recv().is_ok(){}while receiver.try_recv().is_ok(){}
            assert!(!viewing.is_finished() && !c_viewing.is_finished());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    assert_eq!(access.sessions.sessions().len(),2);
    let c_before=c_stats.decoded_total.load(Ordering::Acquire);
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
    tokio::time::timeout(Duration::from_secs(6),async {
        while c_stats.decoded_total.load(Ordering::Acquire)<c_before+12 {
            while c_received.try_recv().is_ok(){}
            assert!(!c_viewing.is_finished(),"revoking A cannot stop C");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    assert_eq!(c_stats.errors.load(Ordering::Acquire),0);
    c_stop.send_replace(true);
    tokio::time::timeout(Duration::from_secs(5),c_viewing).await.unwrap().unwrap().unwrap();
    viewer_stop.send_replace(true);
    stop.send_replace(true);
    serving.await.unwrap().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unified_device_legacy_pinned_pairing_migrates_without_another_code_and_cannot_restore_revoked_access(
) {
    let root = std::env::temp_dir().join(format!("eclipse-migration-{}", uuid::Uuid::new_v4()));
    let (access, mut requests) = access(&root);
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
    access.pairing.open();
    let expected = access.store.card("Generated host").id();
    let approval = async {
        let request = requests.recv().await.unwrap();
        request
            .respond
            .send(Some(
                PairApproval::new(&display.code().unwrap(), Permissions::VIEW).unwrap(),
            ))
            .ok();
    };
    let (connected, _) = tokio::join!(
        lan::connect_device(
            &endpoint,
            &client,
            "Upgraded client",
            Some(&expected),
            None,
            false,
            &display
        ),
        approval
    );
    assert!(
        connected.unwrap().peer.is_some(),
        "a revoked migrated client can be explicitly paired again without replaying its old token"
    );
    assert_eq!(
        access.store.peer(&peer).unwrap().unwrap().inbound,
        Some(Permissions::VIEW)
    );
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
        let sharing = native::SharingConfig {session:None,video_policy:None,codec:Default::default(),rate_control:Default::default(),
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
