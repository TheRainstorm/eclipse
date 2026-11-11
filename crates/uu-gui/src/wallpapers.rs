//! Optional previews never gate the device list or retain full desktop pixels.
use anyhow::Context as _;
use std::{
    io::Cursor,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

const MAX_DOWNLOAD: usize = 4 * 1024 * 1024;
pub(super) const MAX_WALLPAPER_BYTES: usize = 16 * 1024 * 1024;
#[derive(Clone, Copy)]
enum Rendition { Preview, Wallpaper(u32), Favorite }
impl Rendition {
    fn bytes(self) -> usize { match self { Self::Preview => MAX_DOWNLOAD, _ => MAX_WALLPAPER_BYTES } }
    fn allocation(self) -> u64 { match self { Self::Preview => MAX_DECODE_ALLOC, _ => 256 * 1024 * 1024 } }
    fn size(self) -> (u32,u32) { match self { Self::Wallpaper(side) => (side,side), Self::Favorite => (256,144), Self::Preview => MAX_PREVIEW } }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Original { pub width:u32, pub height:u32, pub format:image::ImageFormat }
impl Original {
    pub fn extension(self) -> &'static str { match self.format { image::ImageFormat::Jpeg=>"jpg", image::ImageFormat::Png=>"png", image::ImageFormat::WebP=>"webp", image::ImageFormat::Bmp=>"bmp", image::ImageFormat::Ico=>"ico", _=>"image" } }
}
// Random wallpaper APIs also return 5K/8K originals. Bound source decoding
// separately from the small, fixed-size texture we retain after downsampling.
const MAX_DECODE_ALLOC: u64 = 96 * 1024 * 1024;
const MAX_SOURCE_DIMENSION: u32 = 8192;
const MAX_PREVIEW: (u32, u32) = (960, 540);
const CONCURRENCY: usize = 2;
const ATTEMPTS: usize = 3;
const DOWNLOAD_DEADLINE: Duration = Duration::from_secs(30);

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
    pub encoded: Vec<u8>,
    pub original: Original,
}

pub(super) struct Failure {
    pub device_id: String,
    pub url: String,
    pub error: String,
}

pub(super) struct Worker {
    rx: tokio::sync::mpsc::Receiver<Result<Thumbnail, Failure>>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    finished: Arc<AtomicBool>,
}

impl Worker {
    pub fn start(requests: Vec<Request>, context: eframe::egui::Context) -> anyhow::Result<Self> {
        Self::start_for(requests, context, Rendition::Preview)
    }
    pub fn start_wallpaper(request:Request, context:eframe::egui::Context)->anyhow::Result<Self> {
        let side=context.input(|i|i.max_texture_side).clamp(1,4096) as u32;
        Self::start_for(vec![request], context, Rendition::Wallpaper(side))
    }
    fn start_for(requests:Vec<Request>, context:eframe::egui::Context, rendition:Rendition)->anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(12))
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()?;
        Self::with_rendition(requests, context, client, rendition)
    }

    #[cfg(test)]
    fn with_client(
        requests: Vec<Request>,
        context: eframe::egui::Context,
        client: reqwest::Client,
    ) -> anyhow::Result<Self> {
        Self::with_rendition(requests, context, client, Rendition::Preview)
    }
    fn with_rendition(requests:Vec<Request>, context:eframe::egui::Context, client:reqwest::Client, rendition:Rendition)->anyhow::Result<Self> {
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
                            _ = download_all(requests, client, tx, context.clone(), rendition) => {},
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

    pub fn try_recv(&mut self) -> Option<Result<Thumbnail, Failure>> {
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
    tx: tokio::sync::mpsc::Sender<Result<Thumbnail, Failure>>,
    context: eframe::egui::Context,
    rendition:Rendition,
) {
    let mut requests = requests.into_iter();
    let mut jobs = tokio::task::JoinSet::new();
    loop {
        while jobs.len() < CONCURRENCY {
            let Some(request) = requests.next() else {
                break;
            };
            let client = client.clone();
            jobs.spawn(async move {
                let identity=(request.device_id.clone(),request.url.clone());
                let result=tokio::time::timeout(DOWNLOAD_DEADLINE,download_for(client,request,rendition)).await
                    .unwrap_or_else(|_|Err(anyhow::anyhow!("image download exceeded 30 s")));
                result.map_err(|error|Failure{device_id:identity.0,url:identity.1,error:format!("{error:#}")})
            });
        }
        let Some(result) = jobs.join_next().await else {
            break;
        };
        if let Ok(outcome) = result {
            if tx.send(outcome).await.is_err() {
                break;
            }
            context.request_repaint();
        }
    }
}

#[cfg(test)]
async fn download(client:reqwest::Client,request:Request)->anyhow::Result<Thumbnail> { download_for(client,request,Rendition::Preview).await }
async fn download_for(client: reqwest::Client, request: Request, rendition:Rendition) -> anyhow::Result<Thumbnail> {
    let url = reqwest::Url::parse(&request.url).context("invalid image URL")?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
            && url.username().is_empty() && url.password().is_none(),
        "unsupported image URL"
    );
    let mut last_error=None;
    for attempt in 1..=ATTEMPTS {
        match download_once(&client,&url,Request{device_id:request.device_id.clone(),url:request.url.clone()},rendition).await {
            Ok(image)=>return Ok(image),
            Err(error)=> {
                // Auth/permission errors are actionable, not transient. Random
                // image endpoints can choose an unavailable or oversized image;
                // retry their request, never guess a different CDN filename.
                let permanent=error.downcast_ref::<reqwest::Error>().and_then(|e|e.status())
                    .is_some_and(|s|s.is_client_error());
                last_error=Some(error.context(format!("attempt {attempt}/{ATTEMPTS}")));
                if permanent {break;}
                if attempt<ATTEMPTS {tokio::time::sleep(Duration::from_millis(200)).await;}
            }
        }
    }
    Err(last_error.unwrap())
}

async fn download_once(client:&reqwest::Client,url:&reqwest::Url,request:Request,rendition:Rendition)->anyhow::Result<Thumbnail> {
    // Refresh must reach random-image APIs rather than reusing a cached 302.
    // Keep the configured URL unchanged for source ownership and disk caching.
    let mut response=client.get(url.clone())
        .header(reqwest::header::CACHE_CONTROL,"no-cache")
        .header(reqwest::header::ACCEPT,"image/webp,image/png,image/jpeg,image/*;q=0.8")
        .header(reqwest::header::USER_AGENT,"Eclipse")
        .send().await.map_err(reqwest::Error::without_url).context("image request failed")?
        .error_for_status().map_err(reqwest::Error::without_url)?;
    if let Some(length)=response.content_length() {
        anyhow::ensure!(length<=rendition.bytes() as u64,"image download exceeds byte limit");
    }
    let mut bytes=Vec::new();
    while let Some(chunk)=response.chunk().await.map_err(reqwest::Error::without_url).context("image response interrupted")? {
        anyhow::ensure!(chunk.len()<=rendition.bytes()-bytes.len(),"image download exceeds byte limit");
        bytes.extend_from_slice(&chunk);
    }
    tokio::task::spawn_blocking(move||decode_for(request,bytes,rendition)).await.context("image decoder task failed")?
}

pub(super) fn decode(request: Request, bytes: Vec<u8>) -> anyhow::Result<Thumbnail> { decode_for(request,bytes,Rendition::Preview) }
pub(super) fn decode_wallpaper(request:Request,bytes:Vec<u8>)->anyhow::Result<Thumbnail> { decode_wallpaper_with_limit(request,bytes,4096) }
pub(super) fn decode_wallpaper_with_limit(request:Request,bytes:Vec<u8>,side:usize)->anyhow::Result<Thumbnail> { decode_for(request,bytes,Rendition::Wallpaper(side.clamp(1,4096) as u32)) }
pub(super) fn decode_favorite(request:Request,bytes:Vec<u8>)->anyhow::Result<Thumbnail> { decode_for(request,bytes,Rendition::Favorite) }
fn decode_for(request:Request,bytes:Vec<u8>,rendition:Rendition)->anyhow::Result<Thumbnail> {
    anyhow::ensure!(bytes.len() <= rendition.bytes(), "image exceeds byte limit");
    let mut limits=image::Limits::default();
    limits.max_image_width=Some(MAX_SOURCE_DIMENSION);
    limits.max_image_height=Some(MAX_SOURCE_DIMENSION);
    limits.max_alloc=Some(rendition.allocation());
    let mut reader=image::ImageReader::new(Cursor::new(bytes.as_slice())).with_guessed_format()?;
    reader.limits(limits.clone());
    let format=reader.format();
    let (width,height)=reader.into_dimensions().context("invalid image response or source dimensions exceed 8192 pixels")?;
    let mut reader=image::ImageReader::new(Cursor::new(bytes.as_slice())).with_guessed_format()?;
    reader.limits(limits);
    let decoded=reader.decode().with_context(||format!("cannot decode {format:?} {width}x{height}, {} bytes (allocation limit {} MiB)",bytes.len(),rendition.allocation()/1024/1024))?;
    let target=rendition.size();
    let image = if decoded.width() > target.0 || decoded.height() > target.1 {
        decoded.resize(target.0, target.1, image::imageops::FilterType::Lanczos3)
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
        encoded: bytes,
        original:Original{width,height,format:format.context("unknown image encoding")?},
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
    fn worker_reports_http_failure_without_exposing_query_values() {
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();
        let address=listener.local_addr().unwrap();
        let server=std::thread::spawn(move|| {
            let (mut stream,_)=listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request=[0;4096];let _=stream.read(&mut request).unwrap();
            write!(stream,"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let mut worker=Worker::with_client(vec![request(format!("http://{address}/api?token=private-fixture"))],Default::default(),reqwest::Client::builder().no_proxy().build().unwrap()).unwrap();
        let deadline=Instant::now()+Duration::from_secs(5);
        let failure=loop {
            if let Some(result)=worker.try_recv() {break result.err().expect("HTTP failure missing");}
            assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(failure.device_id,"fixture-device");
        assert!(failure.error.contains("403"));
        assert!(!failure.error.contains("private-fixture"));
        drop(worker);server.join().unwrap();
    }

    #[test]
    fn retries_transient_http_and_invalid_random_image_before_publishing() {
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();
        let address=listener.local_addr().unwrap();
        let server=std::thread::spawn(move|| {
            for step in 0..3 {
                let (mut stream,_)=listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut request=[0;4096];let n=stream.read(&mut request).unwrap();
                let header=String::from_utf8_lossy(&request[..n]).to_ascii_lowercase();
                assert!(header.starts_with("get /api "));
                assert!(header.contains("cache-control: no-cache"));
                if step==0 {
                    write!(stream,"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                }else{
                    let bytes=if step==1 {b"unavailable image".to_vec()} else {png(32,18)};
                    write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len()).unwrap();stream.write_all(&bytes).unwrap();
                }
            }
        });
        let url=format!("http://{address}/api");
        let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let thumbnail=runtime.block_on(download(reqwest::Client::builder().no_proxy().build().unwrap(),request(url.clone()))).unwrap();
        assert_eq!(thumbnail.url,url);assert_eq!((thumbnail.width,thumbnail.height),(32,18));
        server.join().unwrap();
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
    fn redirected_webp_api_is_decoded_and_keeps_the_configured_source() {
        // Pipw redirects to a WebP CDN. Exercise the complete HTTP redirect and
        // format-sniffing path without relying on the public service uptime.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let mut bytes = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(64, 36, image::Rgba([20, 100, 180, 255]))
            .write_to(&mut bytes, image::ImageFormat::WebP).unwrap();
        let bytes = bytes.into_inner();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            for step in 0..2 {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "redirect request missing");
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("{error}"),
                    }
                };
                // Windows inherits the listener's nonblocking mode on accept.
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut buffer = [0; 4096];
                let count = stream.read(&mut buffer).unwrap();
                if step == 0 {
                    assert!(buffer[..count].starts_with(b"GET /api "));
                    write!(stream, "HTTP/1.1 302 Found\r\nLocation: http://{address}/image.webp\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                } else {
                    assert!(buffer[..count].starts_with(b"GET /image.webp "));
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: image/webp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
                    stream.write_all(&bytes).unwrap();
                }
            }
        });
        let url = format!("http://{address}/api");
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let thumbnail = runtime.block_on(download(
            reqwest::Client::builder().no_proxy().build().unwrap(), request(url.clone())
        )).unwrap();
        server.join().unwrap();
        assert_eq!(thumbnail.url, url);
        assert_eq!((thumbnail.width, thumbnail.height), (64, 36));
        assert_eq!(&thumbnail.rgba[..4], &[20, 100, 180, 255]);
    }

    #[test]
    #[ignore = "explicit public image-service network smoke test"]
    fn public_wallpaper_sources_deliver_decodable_images() {
        uu_core::install_crypto_provider();
        let samples=std::env::var("ECLIPSE_WALLPAPER_TEST_SAMPLES").ok().and_then(|v|v.parse::<usize>().ok()).unwrap_or(3).clamp(1,30);
        let mut failures=Vec::new();
        for url in ["https://img-api.pipw.top","https://picsum.photos/1920/1080"] {
            for sample in 0..samples {
                let mut worker=Worker::start(vec![request(url.into())],Default::default()).unwrap();
                let deadline=Instant::now()+Duration::from_secs(35);
                let result=loop {
                    if let Some(result)=worker.try_recv(){break result;}
                    assert!(Instant::now()<deadline,"wallpaper worker exceeded deadline");
                    std::thread::sleep(Duration::from_millis(10));
                };
                match result {
                    Ok(image)=>eprintln!("wallpaper source {url} sample {sample}: decoded {}x{}",image.width,image.height),
                    Err(failure)=>{eprintln!("wallpaper source {url} sample {sample}: {}",failure.error);failures.push(failure.error);},
                }
            }
        }
        assert!(failures.is_empty(),"{} public image requests failed: {failures:?}",failures.len());
    }

    #[test]
    fn high_resolution_wallpaper_is_downsampled_with_bounded_output() {
        let preview=decode(request("https://example.invalid/5k".into()),png(5120,2880)).unwrap();
        assert_eq!((preview.width,preview.height),MAX_PREVIEW);
        assert_eq!(preview.rgba.len(),960*540*4);
        // Dimensions alone are insufficient: a compact PNG can expand beyond
        // the decode allocation budget while remaining inside the edge limit.
        let original=png(8192,4096);
        assert!(decode(request(String::new()),original.clone()).is_err());
        let wallpaper=decode_wallpaper(request(String::new()),original.clone()).unwrap();
        assert_eq!((wallpaper.width,wallpaper.height),(4096,2048));
        assert_eq!((wallpaper.original.width,wallpaper.original.height),(8192,4096));
        assert_eq!(wallpaper.encoded,original);
    }

    #[test]
    fn wallpaper_and_favorite_keep_source_pixels_and_encoding_separate_from_previews() {
        let bytes=png(2560,1440);
        let wallpaper=decode_wallpaper(request("https://example.invalid/original".into()),bytes.clone()).unwrap();
        assert_eq!((wallpaper.width,wallpaper.height),(2560,1440));
        assert_eq!((wallpaper.original.width,wallpaper.original.height),(2560,1440));
        assert_eq!(wallpaper.original.extension(),"png");
        assert_eq!(wallpaper.encoded,bytes);
        let favorite=decode_favorite(request("favorite:owned".into()),bytes.clone()).unwrap();
        assert_eq!((favorite.width,favorite.height),(256,144));
        assert_eq!(favorite.original,wallpaper.original);
        assert_eq!(favorite.encoded,bytes);
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
                break preview.unwrap_or_else(|failure|panic!("{}",failure.error));
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
                break preview.unwrap_or_else(|failure|panic!("{}",failure.error));
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
