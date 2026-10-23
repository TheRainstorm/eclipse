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
    const LOCAL: &str = "Generated controller text 中文🙂";
    const REPLY: &str = "Generated host reply\r\nsecond line";
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
        let config = HostConfig {encoder_tuning:Default::default(),session:None,video_policy:None,codec:Default::default(),rate_control:Default::default(),
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
                system.set_text("Generated private text while controller inactive")?;
                std::fs::write(phases.join("inactive-host-copy"),b"ready")?;
                until(||Path::new(&phases.join("resumed")).exists()&&system.get_text().is_ok_and(|s|s==REPLY)).await?;
                std::fs::write(phases.join("host-converged"),b"ready")?;
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
                ensure!(stats.errors.load(Ordering::Relaxed)==0,"clipboard disturbed video");Ok::<_,anyhow::Error>(())
            }.await;
            stop.send_replace(true);let ended=tokio::time::timeout(Duration::from_secs(5),client).await;
            stats.clipboard.stopped.store(true,Ordering::Release);result?;ended???;
            println!("{}",serde_json::json!({"role":"viewer","native_clipboard":true,"decoded":stats.decoded_total.load(Ordering::Relaxed),"default_off":true,"bidirectional_text":true,"inactive_suppressed":true,"resume_converged":true,"clean_stop":true}));Ok::<_,anyhow::Error>(())
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
