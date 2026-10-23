//! Generated text only, on Xvfb servers owned by the companion smoke script.
#[path = "../src/clipboard.rs"]
mod clipboard;
#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use anyhow::{ensure, Context};
    use std::{
        path::{Path, PathBuf},
        sync::{atomic::Ordering, Arc},
        time::Duration,
    };
    use uu_core::{
        clipboard::Clipboard,
        media::{frame_channel, MediaStats},
        native::{self, CaptureSource, HostConfig},
    };
    fn owned() -> anyhow::Result<()> {
        let display = std::env::var("DISPLAY")?;
        let number = display
            .strip_prefix(':')
            .context("owned local display required")?;
        ensure!(
            !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()),
            "owned local display required"
        );
        let pid = std::env::var("UU_CLIPBOARD_TEST_SERVER_PID")?.parse::<u32>()?;
        ensure!(
            std::fs::read_to_string(format!("/tmp/.X{number}-lock"))?.trim() == pid.to_string(),
            "test does not own this X server"
        );
        ensure!(
            std::fs::read_to_string(format!("/proc/{pid}/comm"))?.trim() == "Xvfb",
            "test requires its own Xvfb"
        );
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            std::fs::metadata(std::env::var("XAUTHORITY")?)?
                .permissions()
                .mode()
                & 0o077
                == 0,
            "private X authority required"
        );
        Ok(())
    }
    async fn until(mut condition: impl FnMut() -> bool) -> anyhow::Result<()> {
        tokio::time::timeout(Duration::from_secs(8), async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .context("owned clipboard state did not converge")?;
        Ok(())
    }
    fn picture(width: usize, height: usize, mut seed: u32) -> arboard::ImageData<'static> {
        let mut bytes = Vec::with_capacity(width * height * 4);
        for index in 0..width * height {
            for _ in 0..3 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                bytes.push((seed >> 24) as u8);
            }
            bytes.push(if index % 7 == 0 { 128 } else { 255 });
        }
        arboard::ImageData {
            width,
            height,
            bytes: std::borrow::Cow::Owned(bytes),
        }
    }
    fn is_picture(system: &mut arboard::Clipboard, width: usize, height: usize, seed: u32) -> bool {
        let expected = picture(width, height, seed);
        system.get_image().is_ok_and(|image| {
            image.width == width
                && image.height == height
                && image.bytes.as_ref() == expected.bytes.as_ref()
        })
    }
    const LOCAL: &str = "Generated controller text 中文🙂";
    const REPLY: &str = "Generated host reply\r\nsecond line";
    const PAUSED_REPLY: &str = "Generated host copy while controller inactive";
    let args: Vec<_> = std::env::args().collect();
    ensure!(
        args.len() == 4,
        "usage: native_clipboard_probe host|viewer INVITATION COORDINATION_DIRECTORY"
    );
    owned()?;
    let invitation = PathBuf::from(&args[2]);
    let phases = PathBuf::from(&args[3]);
    ensure!(phases.is_dir(), "own coordination directory required");
    let mut system = arboard::Clipboard::new()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    if args[1] == "host" {
        system.set_text("Generated host baseline")?;
        let state = Arc::new(Clipboard::default());
        state.enabled.store(true, Ordering::Release);
        clipboard::start_host(state.clone(), 0)?;
        let config = HostConfig {
            encoder_tuning: Default::default(),
            session: None,
            video_policy: None,
            codec: Default::default(),
            rate_control: Default::default(),
            bind: "127.0.0.1:0".parse()?,
            source: CaptureSource::Primary,
            invitation_file: invitation,
            invitation_lifetime: Duration::from_secs(30),
            fps: 30,
            bitrate_kbps: 2000,
            encoder: uu_core::native::EncoderPreference::Software,
            session_limit: Some(Duration::from_secs(25)),
            allow_input: false,
            audio: None,
            clipboard: Some(state.clone()),
        };
        runtime.block_on(async {
            let (stop,rx)=tokio::sync::watch::channel(false);let host=tokio::spawn(native::serve(config,rx));
            let result=async {
                until(||system.get_text().is_ok_and(|s|s==LOCAL)).await?;
                ensure!(state.sent.load(Ordering::Relaxed)==0,"host baseline was broadcast on activation");
                system.set_text(REPLY)?;
                until(||Path::new(&phases.join("inactive")).exists()&&!state.active.load(Ordering::Acquire)).await?;
                system.set_text(PAUSED_REPLY)?;
                std::fs::write(phases.join("inactive-host-copy"),b"ready")?;
                until(||Path::new(&phases.join("resumed")).exists()&&system.get_text().is_ok_and(|s|s==PAUSED_REPLY)).await?;
                std::fs::write(phases.join("host-converged"),b"ready")?;
                until(||phases.join("images-start").exists()&&state.images_supported.load(Ordering::Acquire)).await?;
                system.set_image(picture(192,128,10))?;
                until(||is_picture(&mut system,128,192,20)).await?;
                system.set_text("Generated text after images")?;
                until(||phases.join("readonly-ready").exists()).await?;
                system.set_image(picture(96,96,30))?;
                until(||phases.join("readonly-local-copy").exists()).await?;
                tokio::time::sleep(Duration::from_millis(700)).await;
                ensure!(is_picture(&mut system,96,96,30),"readonly image was forwarded to host");
                std::fs::write(phases.join("readonly-verified"),b"ready")?;
                until(||host.is_finished()).await?;
                Ok::<_,anyhow::Error>(())
            }.await;
            stop.send_replace(true);let ended=tokio::time::timeout(Duration::from_secs(5),host).await;
            state.stopped.store(true,Ordering::Release);result?;let report=ended???;
            ensure!(state.remote.lock().unwrap().is_none()&&!state.active.load(Ordering::Acquire),"host clipboard did not release pending state");
            println!("{}",serde_json::json!({"role":"host","native_clipboard":true,"frames":report.sent,"sent":state.sent.load(Ordering::Relaxed),"received":state.received.load(Ordering::Relaxed),"initial_baseline_stayed_local":true,"clean_stop":true}));Ok::<_,anyhow::Error>(())
        })?;
    } else if args[1] == "viewer" {
        system.set_text(LOCAL)?;
        let stats = Arc::new(MediaStats::default());
        clipboard::start(stats.clipboard.clone(), 0)?;
        runtime.block_on(async {
            until(||invitation.exists()).await?;let invite=uu_core::native_pairing::Invitation::load(&invitation)?;
            let (frames,_images)=frame_channel();let (stop,rx)=tokio::sync::watch::channel(false);let client_stats=stats.clone();let client=tokio::spawn(async move{native::view(&invite,frames,client_stats,rx).await});
            let result=async {
                until(||stats.capabilities.allows(uu_core::capabilities::Capability::Clipboard)&&stats.decoded_total.load(Ordering::Relaxed)>=30).await?;
                ensure!(system.get_text()?==LOCAL,"clipboard changed before consent");
                stats.clipboard.generation.fetch_add(1,Ordering::AcqRel);stats.clipboard.enabled.store(true,Ordering::Release);stats.clipboard.active.store(true,Ordering::Release);
                until(||system.get_text().is_ok_and(|s|s==REPLY)).await?;
                stats.clipboard.generation.fetch_add(1,Ordering::AcqRel);stats.clipboard.active.store(false,Ordering::Release);stats.clipboard.local.lock().unwrap().take();stats.clipboard.remote.lock().unwrap().take();
                std::fs::write(phases.join("inactive"),b"ready")?;
                until(||phases.join("inactive-host-copy").exists()).await?;
                let decoded=stats.decoded_total.load(Ordering::Relaxed);until(||stats.decoded_total.load(Ordering::Relaxed)>=decoded+30).await?;
                ensure!(system.get_text()?==REPLY,"inactive host text reached the controller");
                stats.clipboard.generation.fetch_add(1,Ordering::AcqRel);stats.clipboard.active.store(true,Ordering::Release);std::fs::write(phases.join("resumed"),b"ready")?;
                until(||phases.join("host-converged").exists()).await?;
                until(||system.get_text().is_ok_and(|s|s==PAUSED_REPLY)).await?;
                ensure!(stats.clipboard.images_supported.load(Ordering::Acquire),"image protocol was not negotiated");
                std::fs::write(phases.join("images-start"),b"ready")?;
                until(||is_picture(&mut system,192,128,10)).await?;
                system.set_image(picture(128,192,20))?;
                until(||system.get_text().is_ok_and(|s|s=="Generated text after images")).await?;
                stats.clipboard.read_only.store(true,Ordering::Release);
                stats.clipboard.generation.fetch_add(1,Ordering::AcqRel);
                std::fs::write(phases.join("readonly-ready"),b"ready")?;
                until(||is_picture(&mut system,96,96,30)).await?;
                system.set_image(picture(64,64,40))?;
                std::fs::write(phases.join("readonly-local-copy"),b"ready")?;
                until(||phases.join("readonly-verified").exists()).await?;
                ensure!(stats.errors.load(Ordering::Relaxed)==0,"clipboard disturbed video");Ok::<_,anyhow::Error>(())
            }.await;
            stop.send_replace(true);let ended=tokio::time::timeout(Duration::from_secs(5),client).await;
            stats.clipboard.stopped.store(true,Ordering::Release);result?;ended???;
            println!("{}",serde_json::json!({"role":"viewer","native_clipboard":true,"decoded":stats.decoded_total.load(Ordering::Relaxed),"default_off":true,"bidirectional_text":true,"bidirectional_png":true,"text_image_switch":true,"readonly_image_suppressed":true,"inactive_suppressed":true,"resume_converged":true,"clean_stop":true}));Ok::<_,anyhow::Error>(())
        })?;
    } else {
        anyhow::bail!("role must be host or viewer");
    }
    // Allow the actual OS worker to observe stop before its private X server exits.
    std::thread::sleep(Duration::from_millis(150));
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This fixture requires two private Linux Xvfb servers");
    std::process::exit(2);
}
