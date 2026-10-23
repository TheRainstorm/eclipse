//! Exercise page state with real loopback pairing and generated video only.
use super::*;
use std::sync::{atomic::Ordering, Arc};
use uu_core::{media, native};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("eclipse-page-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn native_lan_page_preserves_manual_addresses_and_reports_invalid_configuration() {
    let fixture = Fixture::new();
    let mut page = LanUi::new(Some(fixture.0.clone()));
    assert!(!page.compatibility);
    page.address = "127.0.0.1".into();
    page.add();
    assert_eq!(page.rows().len(), 1);
    drop(page);
    let mut page = LanUi::new(Some(fixture.0.clone()));
    assert_eq!(page.rows()[0].endpoint, "127.0.0.1:47998");
    page.address = "https://example.invalid".into();
    page.add();
    assert!(page.notice.contains("无法添加设备"));
    assert_eq!(page.rows().len(), 1);
    std::fs::write(fixture.0.join("paired-devices.json"), b"damaged fixture").unwrap();
    let page = LanUi::new(Some(fixture.0.clone()));
    assert!(page.book.is_none());
    assert!(page.notice.contains("原文件已保留"));
}

async fn wait_for_client_error(page: &mut LanUi, ctx: &egui::Context) {
    tokio::time::timeout(Duration::from_secs(8),async {
        loop {
            page.poll(ctx,false);
            if page.client.is_none() && page.client_error.is_some() {break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("client must report a bounded failure");
    assert!(page.client_dialog,"failures must remain visible after the worker exits");
}

#[test]
fn native_lan_page_worker_exit_cannot_silently_remove_progress() {
    let fixture=Fixture::new();
    let mut page=LanUi::new(Some(fixture.0.clone()));
    let (stop,_)=watch::channel(false);
    let task=std::thread::spawn(||{});
    while !task.is_finished() {std::thread::yield_now();}
    page.client=Some(Job {stop,task:Some(task)});
    page.client_dialog=true;
    page.poll(&egui::Context::default(),false);
    assert!(page.client.is_none());
    assert!(page.client_dialog);
    assert!(page.client_error.as_ref().unwrap().contains("线程意外退出"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_lan_page_refused_and_unresponsive_endpoints_keep_actionable_error() {
    for silent in [false,true] {
        let fixture=Fixture::new();
        let ctx=egui::Context::default();
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint=listener.local_addr().unwrap().to_string();
        let server=if silent {
            Some(tokio::spawn(async move {
                let (_socket,_)=listener.accept().await.unwrap();
                std::future::pending::<()>().await;
            }))
        } else {drop(listener);None};
        let mut page=LanUi::new(Some(fixture.0.clone()));
        page.address=endpoint.clone();page.add();
        page.connect(page.rows().remove(0),&ctx);
        assert!(page.client_dialog && page.client.is_some());
        assert!(page.pair_target.contains(&endpoint));
        wait_for_client_error(&mut page,&ctx).await;
        let error=page.client_error.as_ref().unwrap();
        assert!(error.contains(&endpoint),"endpoint must survive error context: {error}");
        if silent {assert!(error.contains("超时"));}
        else {assert!(error.contains("无法连接"));assert!(error.contains("TCP"));}
        assert!(page.open_device.is_none());
        // Later discovery/page polls must not dismiss a connection failure.
        page.poll(&ctx,false);
        assert!(page.client_dialog && page.client_error.is_some());
        if let Some(server)=server {server.abort();let _=server.await;}
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_lan_page_pairs_persists_and_decodes_generated_video_without_account() {
    let fixture = Fixture::new();
    let (stop, rx) = watch::channel(false);
    let (events, mut incoming) = async_mpsc::unbounded_channel();
    let (server, host) = lan::Server::bind(
        lan::Config {video_policy:None,codec:Default::default(),rate_control:Default::default(),
            name: "Generated page fixture".into(),
            bind: "127.0.0.1:0".parse().unwrap(),
            discovery_bind: None,
            data_dir: fixture.0.join("host"),
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
        },
        rx,
        events,
    )
    .await
    .unwrap();
    let endpoint = server.address().unwrap().to_string();
    let task = tokio::spawn(server.run());
    let ctx = egui::Context::default();
    let dir = fixture.0.join("viewer");
    let mut page = LanUi::new(Some(dir.clone()));
    page.address = endpoint;
    page.add();
    page.connect(page.rows().remove(0), &ctx);
    wait_for_client_error(&mut page,&ctx).await;
    assert!(page.client_error.as_ref().unwrap().contains("允许新设备配对"));
    host.allow_pairing();
    page.connect(page.rows().remove(0), &ctx);
    assert!(page.client_error.is_none(),"retry clears the previous failure");
    let mut request = None;
    let deadline = Instant::now() + Duration::from_secs(8);
    while page.open_device.is_none() {
        assert!(
            Instant::now() < deadline,
            "GUI pairing did not complete: {}",
            page.notice
        );
        page.poll(&ctx, false);
        while let Ok(event) = incoming.try_recv() {
            if let lan::Event::Pairing(pairing) = event {
                request = Some(pairing);
            }
        }
        if let (Some(code), Some(pending)) = (&page.pair_code, &request) {
            assert_eq!(
                code, &pending.code,
                "fixture must verify both TLS comparison codes"
            );
            request.take().unwrap().decision.send(true).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!page.client_dialog,"successful pairing closes the progress dialog");
    let device = page.open_device.take().unwrap();
    let book = Devices::open(&dir.join("paired-devices.json")).unwrap();
    assert_eq!(book.devices.len(), 1);
    assert!(matches!(page.rows()[0].kind, RowKind::Paired(_)));
    assert_eq!(
        page.rows().len(),
        1,
        "manual and paired rows must not duplicate"
    );
    let invitation = lan::connect(&device, &book.client_id).await.unwrap();
    let stats = Arc::new(media::MediaStats::default());
    let (frames, _receiver) = media::frame_channel();
    let (end, end_rx) = watch::channel(false);
    let measured = stats.clone();
    let viewing =
        tokio::spawn(async move { native::view(&invitation, frames, measured, end_rx).await });
    tokio::time::timeout(Duration::from_secs(8), async {
        while stats.decoded_total.load(Ordering::Acquire) < 8 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(stats.errors.load(Ordering::Acquire), 0);
    end.send_replace(true);
    tokio::time::timeout(Duration::from_secs(4), viewing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    stop.send_replace(true);
    tokio::time::timeout(Duration::from_secs(4), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(page);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_disable_cancels_owned_udp_and_discards_old_results_without_losing_devices() {
    let fixture = Fixture::new();
    let ctx = egui::Context::default();
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut page = LanUi::new(Some(fixture.0.clone()));
    page.address = "127.0.0.1:51000".into();
    page.add();
    let previous = page.scan_epoch;
    page.scan = Some(spawn(&ctx, page.send.clone(), Work::Scan(previous, vec![socket.local_addr().unwrap()])).unwrap());
    let mut bytes = [0; 1500];
    tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut bytes)).await.unwrap().unwrap();
    let device = discovery::Device {
        info: discovery::ServerInfo { id: uuid::Uuid::new_v4().to_string(), name: "Generated discovery fixture".into(), port: 51001, fingerprint: "a".repeat(64) },
        address: "127.0.0.1:51001".parse().unwrap(),
    };
    page.found.insert((device.info.id.clone(), device.info.fingerprint.clone()), Seen { device: device.clone(), at: Instant::now() });
    page.set_discovery(false);
    assert!(page.scan.is_none() && page.found.is_empty());
    page.send.send(Event::Scan(previous, Ok(vec![device.clone()]))).unwrap();
    page.poll(&ctx, true);
    assert!(page.nearby(None).is_empty());
    assert_eq!(page.rows().len(), 1, "manual devices survive disabling discovery");
    tokio::time::timeout(Duration::from_secs(2), async {
        loop { page.poll(&ctx, true); if page.retired_scans.is_empty() { break; } tokio::time::sleep(Duration::from_millis(10)).await; }
    }).await.expect("cancellation must finish the worker");
    assert!(tokio::time::timeout(Duration::from_millis(100), socket.recv_from(&mut bytes)).await.is_err(), "disabled polling must not send queries");
    page.set_discovery(true);
    page.send.send(Event::Scan(previous, Ok(vec![device.clone()]))).unwrap();
    page.poll(&ctx, false);
    assert!(page.found.is_empty(), "re-enabling cannot revive an old scan");
    page.send.send(Event::Scan(page.scan_epoch, Ok(vec![device]))).unwrap();
    page.poll(&ctx, false);
    assert_eq!(page.nearby(None).len(), 1);
}
