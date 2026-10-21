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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_lan_page_pairs_persists_and_decodes_generated_video_without_account() {
    let fixture = Fixture::new();
    let (stop, rx) = watch::channel(false);
    let (events, mut incoming) = async_mpsc::unbounded_channel();
    let (server, host) = lan::Server::bind(
        lan::Config {
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
    host.allow_pairing();
    let ctx = egui::Context::default();
    let dir = fixture.0.join("viewer");
    let mut page = LanUi::new(Some(dir.clone()));
    page.address = endpoint;
    page.add();
    page.connect(page.rows().remove(0), &ctx);
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
