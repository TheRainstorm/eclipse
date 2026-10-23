use super::*;
use crate::clipboard::Image;
use std::io::Write;
fn generated(width: u32, height: u32, mut seed: u32) -> Image {
    let mut raw = Vec::new();
    for _ in 0..height {
        raw.push(0);
        for _ in 0..width * 4 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            raw.push((seed >> 24) as u8);
        }
    }
    let mut zip = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    zip.write_all(&raw).unwrap();
    let data = zip.finish().unwrap();
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        let mut crc = crc32fast::Hasher::new();
        crc.update(kind);
        crc.update(data);
        png.extend_from_slice(&crc.finalize().to_be_bytes());
    };
    let mut header = width.to_be_bytes().to_vec();
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &header);
    chunk(b"IDAT", &data);
    chunk(b"IEND", &[]);
    Image::new(width, height, png).unwrap()
}
#[test]
fn image_chunks_complete_once_and_partial_invalid_or_expired_data_never_replaces_content() {
    let image = generated(64, 64, 1);
    assert!(image.png.len() > image::CHUNK);
    let mut assembly = image::Assembly::default();
    let now = Instant::now();
    let mut completed = None;
    for (offset, bytes) in image.png.chunks(image::CHUNK).enumerate() {
        let chunk = image::Chunk {
            revision: 3,
            width: image.width,
            height: image.height,
            total: image.png.len() as u32,
            offset: (offset * image::CHUNK) as u32,
            bytes,
        };
        let wire = encode_version(Packet::Image { epoch: 1, chunk }, 2).unwrap();
        assert!(parse_version(&wire, 1).is_none());
        let Some(Packet::Image { epoch: 1, chunk }) = parse_version(&wire, 2) else {
            panic!("chunk did not roundtrip")
        };
        if let Some(value) = assembly.push(chunk, now).unwrap() {
            assert!(completed.is_none());
            completed = Some(value);
        }
    }
    assert!(completed == Some(image.clone()));
    let start = image::Chunk {
        revision: 3,
        width: image.width,
        height: image.height,
        total: image.png.len() as u32,
        offset: 0,
        bytes: &image.png[..image::CHUNK],
    };
    assert!(assembly.push(start, now).unwrap().is_none());
    let next = image::Chunk {
        offset: image::CHUNK as u32,
        bytes: &image.png[image::CHUNK..image::CHUNK * 2],
        ..start
    };
    assert!(assembly
        .push(next, now + Duration::from_secs(11))
        .unwrap()
        .is_none());
    assembly.push(start, now).unwrap();
    assert!(assembly
        .push(image::Chunk { offset: 1, ..next }, now)
        .is_err());
    assert!(!image::Chunk {
        total: (crate::clipboard::MAX_IMAGE_BYTES + 1) as u32,
        ..start
    }
    .valid());
    assert!(!image::Chunk {
        width: 8193,
        ..start
    }
    .valid());
}
#[test]
fn content_revisions_order_text_and_images_and_resolve_simultaneous_copy() {
    let host = Shared::new();
    let viewer = Shared::new();
    assert_eq!(host.next_revision(false), Some(2));
    assert_eq!(viewer.next_revision(true), Some(3));
    assert!(!viewer.accept_revision(2, false));
    assert!(host.accept_revision(3, true));
    assert!(!host.accept_revision(3, true));
    assert_eq!(host.next_revision(false), Some(4));
    assert!(viewer.accept_revision(4, false));
    assert_eq!(viewer.next_revision(true), Some(7));
    assert!(!host.accept_revision(8, true));
    assert!(!host.accept_revision(u64::MAX, true));
    let control = Clipboard::default();
    *control.remote.lock().unwrap() = Some("old".into());
    let image = generated(64, 64, 2);
    let chunk = image::Chunk {
        revision: 9,
        width: image.width,
        height: image.height,
        total: image.png.len() as u32,
        offset: 0,
        bytes: &image.png[..image::CHUNK],
    };
    receive_content(&host, &control, Packet::Image { epoch: 1, chunk }, true).unwrap();
    assert_eq!(
        control
            .remote
            .lock()
            .unwrap()
            .as_ref()
            .and_then(Content::as_text),
        Some("old")
    );
    receive_content(
        &host,
        &control,
        Packet::RichText {
            epoch: 1,
            revision: 11,
            value: "new text",
        },
        true,
    )
    .unwrap();
    receive_content(
        &host,
        &control,
        Packet::Image {
            epoch: 1,
            chunk: image::Chunk {
                offset: image::CHUNK as u32,
                bytes: &image.png[image::CHUNK..image::CHUNK * 2],
                ..chunk
            },
        },
        true,
    )
    .unwrap();
    assert_eq!(
        control
            .remote
            .lock()
            .unwrap()
            .as_ref()
            .and_then(Content::as_text),
        Some("new text")
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn encrypted_native_png_is_bidirectional_readonly_and_focus_cancellation_drops_partial_data()
{
    use crate::{media::frame_channel, native};
    use std::sync::atomic::Ordering::{Acquire, Release};
    let root = std::env::temp_dir().join(format!("eclipse-image-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let invitation = root.join("generated.eclipse-invite.json");
    let source = Arc::new(Clipboard::default());
    source.available.store(true, Release);
    source.enabled.store(true, Release);
    let (host_stop, host_rx) = tokio::sync::watch::channel(false);
    let config = native::HostConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        source: native::CaptureSource::Synthetic {
            width: 160,
            height: 96,
            fps: 30,
        },
        invitation_file: invitation.clone(),
        invitation_lifetime: Duration::from_secs(15),
        fps: 30,
        bitrate_kbps: 500,
        encoder: native::EncoderPreference::Software,
        session_limit: Some(Duration::from_secs(15)),
        allow_input: false,
        audio: None,
        clipboard: Some(source.clone()),
        video_policy: None,
        codec: Default::default(),
        rate_control: Default::default(),
        encoder_tuning: Default::default(),
        session: None,
    };
    let host = tokio::spawn(native::serve(config, host_rx));
    let stats = Arc::new(MediaStats::default());
    stats.clipboard.available.store(true, Release);
    stats.clipboard.enabled.store(true, Release);
    stats.clipboard.active.store(true, Release);
    stats.clipboard.generation.store(1, Release);
    let _ = stats
        .decoder_preference
        .set(uu_media::DecoderKind::Software);
    async fn wait(condition: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(8), async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    wait(|| invitation.exists()).await;
    let invite = crate::native_pairing::Invitation::load(&invitation).unwrap();
    let (frames, _images) = frame_channel();
    let (stop, rx) = tokio::sync::watch::channel(false);
    let observed = stats.clone();
    let viewer = tokio::spawn(async move { native::view(&invite, frames, observed, rx).await });
    wait(|| stats.clipboard.images_supported.load(Acquire) && source.active.load(Acquire)).await;
    let image = generated(128, 128, 3);
    *stats.clipboard.local.lock().unwrap() = Some(Content::Image(image.clone()));
    wait(|| source.remote.lock().unwrap().as_ref() == Some(&Content::Image(image.clone()))).await;
    *stats.clipboard.local.lock().unwrap() = Some("after image".into());
    wait(|| {
        source
            .remote
            .lock()
            .unwrap()
            .as_ref()
            .and_then(Content::as_text)
            == Some("after image")
    })
    .await;
    source.remote.lock().unwrap().take();
    let large = generated(512, 2048, 4);
    *stats.clipboard.local.lock().unwrap() = Some(Content::Image(large));
    tokio::time::sleep(Duration::from_millis(30)).await;
    stats.clipboard.active.store(false, Release);
    stats.clipboard.generation.fetch_add(1, Ordering::AcqRel);
    wait(|| !source.active.load(Acquire)).await;
    assert!(
        source.remote.lock().unwrap().is_none(),
        "partial PNG must never publish clipboard content"
    );
    stats.clipboard.active.store(true, Release);
    stats.clipboard.generation.fetch_add(1, Ordering::AcqRel);
    stats.clipboard.read_only.store(true, Release);
    wait(|| source.active.load(Acquire)).await;
    let before = source.received.load(Acquire);
    *stats.clipboard.local.lock().unwrap() = Some(Content::Image(image));
    let reply = generated(32, 32, 5);
    *source.local.lock().unwrap() = Some(Content::Image(reply.clone()));
    wait(|| {
        stats.clipboard.remote.lock().unwrap().as_ref() == Some(&Content::Image(reply.clone()))
    })
    .await;
    assert_eq!(source.received.load(Acquire), before);
    assert_eq!(stats.errors.load(Acquire), 0);
    assert!(stats.decoded_total.load(Acquire) > 0);
    stop.send_replace(true);
    tokio::time::timeout(Duration::from_secs(5), viewer)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    host_stop.send_replace(true);
    tokio::time::timeout(Duration::from_secs(5), host)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
