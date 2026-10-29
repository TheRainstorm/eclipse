use anyhow::{ensure, Context, Result};
use clap::Subcommand;
use std::{
    path::PathBuf,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tokio::sync::watch;
use uu_core::{internet, media::MediaStats};

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Create a new private 256-bit access key (never overwrites a file).
    Keygen {
        #[arg(long)]
        file: PathBuf,
    },
    /// Run the self-hosted TLS signaling service. Media is never forwarded.
    Serve {
        #[arg(long, default_value = "127.0.0.1:8443")]
        bind: std::net::SocketAddr,
        #[arg(long)]
        certificate: PathBuf,
        #[arg(long)]
        private_key: PathBuf,
        #[arg(long)]
        token_file: PathBuf,
        #[arg(long)]
        stun: Vec<String>,
    },
    /// Share a native source using an installed provider or self-hosted service.
    Host {
        #[command(flatten)]
        video:super::NativeVideoOptions,
        #[arg(long)]
        settings: PathBuf,
        #[arg(long, conflicts_with = "synthetic")]
        monitor: Option<String>,
        #[arg(long)]
        synthetic: bool,
        #[arg(long,requires="synthetic",default_value_t=1280,value_parser=super::synthetic_dimension)]
        width: u32,
        #[arg(long,requires="synthetic",default_value_t=720,value_parser=super::synthetic_dimension)]
        height: u32,
        #[arg(long, conflicts_with = "synthetic")]
        allow_input: bool,
        #[arg(long, conflicts_with = "synthetic")]
        audio: bool,
        #[arg(long, requires = "synthetic")]
        synthetic_audio: bool,
        #[arg(long, default_value_t = 60)]
        fps: u32,
        #[arg(long, default_value_t = 8000)]
        bitrate_kbps: u32,
        #[arg(long, value_enum, default_value = "auto")]
        encoder: super::NativeEncoder,
        #[arg(long)]
        max_seconds: Option<u64>,
    },
    /// Receive native video and write metadata only, without desktop input.
    Probe {
        #[arg(long)]
        settings: PathBuf,
        #[arg(long, default_value_t = 120)]
        frames: u64,
        #[arg(long, default_value_t = 45)]
        timeout_seconds: u64,
        #[arg(long)]
        report: Option<PathBuf>,
    },
}
fn settings(path: &std::path::Path) -> Result<internet::Settings> {
    ensure!(
        std::fs::metadata(path)?.len() < 64 * 1024,
        "connection settings file too large"
    );
    serde_json::from_slice(&std::fs::read(path)?).context("invalid internet settings")
}
pub async fn run(command: Command, config: uu_core::Config) -> Result<()> {
    match command {
        Command::Keygen { file } => {
            internet::AccessKey::create(&file)?;
            println!("Private key created; transfer it to the intended device securely.");
            Ok(())
        }
        Command::Serve {
            bind,
            certificate,
            private_key,
            token_file,
            stun,
        } => {
            let (stop, rx) = watch::channel(false);
            let service = internet::server::serve(
                internet::server::Config {
                    bind,
                    certificate,
                    private_key,
                    token_file,
                    stun,
                },
                rx,
            );
            tokio::pin!(service);
            tokio::select! {result=&mut service=>result,_=tokio::signal::ctrl_c()=>{stop.send_replace(true);service.await}}
        }
        Command::Host {
            video,
            settings: path,
            monitor,
            synthetic,
            width,
            height,
            allow_input,
            audio,
            synthetic_audio,
            fps,
            bitrate_kbps,
            encoder,
            max_seconds,
        } => {
            let settings = settings(&path)?;
            let (codec,rate_control)=video.policy(bitrate_kbps)?;
            let source = if synthetic {
                uu_core::native::CaptureSource::Synthetic { width, height, fps }
            } else if let Some(name) = monitor {
                uu_core::native::CaptureSource::Monitor(name)
            } else {
                uu_core::native::CaptureSource::Primary
            };
            let sharing = uu_core::native::SharingConfig {encoder_tuning:Default::default(),session:None,video_policy:None,codec,rate_control,
                source,
                fps,
                bitrate_kbps,
                encoder: encoder.preference(),
                session_limit: max_seconds.map(Duration::from_secs),
                allow_input,
                clipboard: None,
                audio: if audio {
                    Some(uu_core::native::AudioSource::System)
                } else if synthetic_audio {
                    Some(uu_core::native::AudioSource::Synthetic)
                } else {
                    None
                },
            };
            let (stop, rx) = watch::channel(false);
            let session = internet::host(settings, config, sharing, rx, None);
            tokio::pin!(session);
            let report = tokio::select! {result=&mut session=>result?,_=tokio::signal::ctrl_c()=>{stop.send_replace(true);session.await?}};
            println!("Native internet host: sent={} captured={} bytes={} encoder={} input_errors={} audio_packets={}",report.sent,report.captured,report.bytes,report.encoder,report.input_errors,report.audio_packets);
            Ok(())
        }
        Command::Probe {
            settings: path,
            frames,
            timeout_seconds,
            report,
        } => {
            ensure!(
                frames > 0 && timeout_seconds > 0,
                "probe limits must be positive"
            );
            let settings = settings(&path)?;
            let mut report = report
                .map(|path| {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(path)
                })
                .transpose()?;
            let stats = Arc::new(MediaStats::default());
            stats
                .diagnostics
                .start([config.state_dir.join("diagnostics")]);
            let (tx, mut rx) = uu_core::media::frame_channel();
            let (stop, stop_rx) = watch::channel(false);
            let task = tokio::spawn(internet::view(
                settings,
                config,
                tx,
                stats.clone(),
                stop_rx,
                None,
            ));
            let started = std::time::Instant::now();
            let mut observed = 0u64;
            let result = tokio::time::timeout(Duration::from_secs(timeout_seconds), async {
                loop {
                    while rx.try_recv().is_ok() {
                        observed += 1;
                    }
                    if stats.decoded_total.load(Ordering::Relaxed) >= frames && observed > 0 {
                        return Ok::<_, anyhow::Error>(());
                    }
                    ensure!(
                        !task.is_finished(),
                        "native internet connection ended before requested frames"
                    );
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .context("internet video probe timed out");
            stop.send_replace(true);
            let closed = task.await?;
            let (width, height) = stats.res();
            let metrics = serde_json::json!({"completed":matches!(&result,Ok(Ok(())))&&closed.is_ok(),
                "decoded":stats.decoded_total.load(Ordering::Relaxed),"observed":observed,"errors":stats.errors.load(Ordering::Relaxed),
                "width":width,"height":height,"elapsed_ms":started.elapsed().as_millis() as u64,"backend":stats.backend(),
                "audio_packets":stats.audio.packets.load(Ordering::Relaxed)});
            if let Some(file) = &mut report {
                serde_json::to_writer_pretty(&mut *file, &metrics)?;
                file.sync_all()?;
            }
            println!("{metrics}");
            closed?;
            result??;
            ensure!(
                stats.errors.load(Ordering::Relaxed) == 0,
                "native decoder reported errors"
            );
            Ok(())
        }
    }
}
