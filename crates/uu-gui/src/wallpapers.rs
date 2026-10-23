//! Optional previews never gate the device list or retain full desktop pixels.
use std::{
    io::Cursor,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

const MAX_DOWNLOAD: usize = 4 * 1024 * 1024;
const MAX_DECODE_ALLOC: u64 = 40 * 1024 * 1024;
const MAX_SOURCE_DIMENSION: u32 = 4096;
const MAX_PREVIEW: (u32, u32) = (960, 540);
const CONCURRENCY: usize = 2;

pub(super) struct Request {
    pub device_id: String,
    pub url: String,
}

pub(super) struct Thumbnail {
    pub device_id: String,
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub(super) struct Worker {
    rx: tokio::sync::mpsc::Receiver<Thumbnail>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    finished: Arc<AtomicBool>,
}

impl Worker {
    pub fn start(requests: Vec<Request>, context: eframe::egui::Context) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(12))
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()?;
        Self::with_client(requests, context, client)
    }

    fn with_client(
        requests: Vec<Request>,
        context: eframe::egui::Context,
        client: reqwest::Client,
    ) -> anyhow::Result<Self> {
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let finished = Arc::new(AtomicBool::new(false));
        let done = finished.clone();
        let thread = std::thread::Builder::new()
            .name("uu-wallpapers".into())
            .spawn(move || {
                if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    runtime.block_on(async {
                        tokio::select! {
                            biased;
                            _ = cancelled => {},
                            _ = download_all(requests, client, tx, context.clone()) => {},
                        }
                    });
                    // A cancelled image decode may finish on a blocking worker; it
                    // has bounded input/allocation and cannot publish after cancel.
                    runtime.shutdown_background();
                }
                done.store(true, Ordering::Release);
                context.request_repaint();
            })?;
        Ok(Self {
            rx,
            cancel: Some(cancel),
            thread: Some(thread),
            finished,
        })
    }

    pub fn try_recv(&mut self) -> Option<Thumbnail> {
        self.rx.try_recv().ok()
    }

    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire) && self.rx.is_empty()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn download_all(
    requests: Vec<Request>,
    client: reqwest::Client,
    tx: tokio::sync::mpsc::Sender<Thumbnail>,
    context: eframe::egui::Context,
) {
    let mut requests = requests.into_iter();
    let mut jobs = tokio::task::JoinSet::new();
    loop {
        while jobs.len() < CONCURRENCY {
            let Some(request) = requests.next() else {
                break;
            };
            let client = client.clone();
            jobs.spawn(download(client, request));
        }
        let Some(result) = jobs.join_next().await else {
            break;
        };
        if let Ok(Ok(thumbnail)) = result {
            if tx.send(thumbnail).await.is_err() {
                break;
            }
            context.request_repaint();
        }
    }
}

async fn download(client: reqwest::Client, request: Request) -> anyhow::Result<Thumbnail> {
    let url = reqwest::Url::parse(&request.url)?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none(),
        "unsupported preview URL"
    );
    let mut response = client.get(url).send().await?.error_for_status()?;
    if let Some(length) = response.content_length() {
        anyhow::ensure!(length <= MAX_DOWNLOAD as u64, "preview too large");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(
            chunk.len() <= MAX_DOWNLOAD - bytes.len(),
            "preview too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    tokio::task::spawn_blocking(move || decode(request, bytes)).await?
}

pub(super) fn decode(request: Request, bytes: Vec<u8>) -> anyhow::Result<Thumbnail> {
    anyhow::ensure!(bytes.len() <= MAX_DOWNLOAD, "preview too large");
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_DIMENSION);
    limits.max_image_height = Some(MAX_SOURCE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let decoded = reader.decode()?;
    let image = if decoded.width() > MAX_PREVIEW.0 || decoded.height() > MAX_PREVIEW.1 {
        decoded.thumbnail(MAX_PREVIEW.0, MAX_PREVIEW.1)
    } else {
        decoded
    }
    .to_rgba8();
    Ok(Thumbnail {
        device_id: request.device_id,
        url: request.url,
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        time::Instant,
    };

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba([20, 100, 180, 255]));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    fn request(url: String) -> Request {
        Request {
            device_id: "fixture-device".into(),
            url,
        }
    }

    #[test]
    fn generated_preview_has_bounded_pixels_and_retains_device_identity() {
        let preview = decode(
            request("https://example.invalid/preview".into()),
            png(1920, 1080),
        )
        .unwrap();
        assert_eq!((preview.width, preview.height), MAX_PREVIEW);
        assert_eq!(preview.rgba.len(), 960 * 540 * 4);
        assert_eq!(preview.device_id, "fixture-device");
    }

    #[test]
    fn rejects_oversized_source_dimensions_and_compressed_payload() {
        assert!(decode(request(String::new()), png(1, MAX_SOURCE_DIMENSION + 1)).is_err());
        assert!(decode(request(String::new()), vec![0; MAX_DOWNLOAD + 1]).is_err());
    }

    #[test]
    fn real_http_worker_publishes_generated_thumbnail() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let bytes = png(32, 18);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut buffer = [0; 4096];
            let _ = stream.read(&mut buffer).unwrap();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
            stream.write_all(&bytes).unwrap();
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut worker = Worker::with_client(
            vec![request(format!("http://{address}/preview"))],
            Default::default(),
            client,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let preview = loop {
            if let Some(preview) = worker.try_recv() {
                break preview;
            }
            assert!(
                Instant::now() < deadline,
                "generated preview was not delivered"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!((preview.width, preview.height), (32, 18));
        drop(worker);
        server.join().unwrap();
    }

    #[test]
    fn cancelling_stalled_http_does_not_wait_for_request_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (ready, accepted) = std::sync::mpsc::channel();
        let (release, stop) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut buffer = [0; 4096];
            let _ = stream.read(&mut buffer).unwrap();
            ready.send(()).unwrap();
            let _ = stop.recv_timeout(Duration::from_secs(5));
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let worker = Worker::with_client(
            vec![request(format!("http://{address}/stalled"))],
            Default::default(),
            client,
        )
        .unwrap();
        accepted.recv_timeout(Duration::from_secs(3)).unwrap();
        let start = Instant::now();
        drop(worker);
        let elapsed = start.elapsed();
        let _ = release.send(());
        server.join().unwrap();
        assert!(
            elapsed < Duration::from_secs(2),
            "cancel waited for network timeout: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn preview_requests_reject_local_files_and_embedded_authentication() {
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        for url in [
            "file:///tmp/fixture-preview.png",
            "http://fixture:password@127.0.0.1:9/preview",
        ] {
            assert!(download(client.clone(), request(url.into())).await.is_err());
        }
    }

    #[test]
    fn fast_preview_is_delivered_while_another_server_response_is_stalled() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let bytes = png(32, 18);
        let (release, stop) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut stalled = None;
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut buffer = [0; 4096];
                let count = stream.read(&mut buffer).unwrap();
                if buffer[..count].starts_with(b"GET /stalled ") {
                    stalled = Some(stream);
                } else {
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    )
                    .unwrap();
                    stream.write_all(&bytes).unwrap();
                }
            }
            let _ = stop.recv_timeout(Duration::from_secs(5));
            drop(stalled);
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let mut worker = Worker::with_client(
            vec![
                request(format!("http://{address}/stalled")),
                request(format!("http://{address}/fast")),
            ],
            Default::default(),
            client,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let preview = loop {
            if let Some(preview) = worker.try_recv() {
                break preview;
            }
            assert!(
                Instant::now() < deadline,
                "fast preview waited for the stalled response"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(preview.url.ends_with("/fast"));
        drop(worker);
        let _ = release.send(());
        server.join().unwrap();
    }
}
