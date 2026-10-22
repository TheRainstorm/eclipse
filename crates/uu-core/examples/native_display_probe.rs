//! Renew authenticated native media using only the owned generated portal fixture.
use anyhow::{ensure, Context};
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tokio::sync::watch;
use uu_core::{
    media::{frame_channel, MediaStats},
    native_input::{self, InputCommand},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    ensure!(
        std::env::var("ECLIPSE_PORTAL_FIXTURE").as_deref() == Ok("1")
            && std::env::var("WAYLAND_DISPLAY").as_deref() == Ok("owned-fixture"),
        "requires tools/native/portal_smoke.py --case stream-displays"
    );
    let invitation = std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR")?)
        .join("generated.eclipse-invite.json");
    let host_clipboard = Arc::new(uu_core::clipboard::Clipboard::default());
    host_clipboard.available.store(true, Ordering::Release);
    host_clipboard.enabled.store(true, Ordering::Release);
    let config = uu_core::native::HostConfig {
        bind: "127.0.0.1:0".parse()?,
        source: uu_core::native::CaptureSource::Desktop,
        invitation_file: invitation.clone(),
        invitation_lifetime: Duration::from_secs(30),
        fps: 30,
        bitrate_kbps: 2000,
        encoder: uu_core::native::EncoderPreference::Software,
        session_limit: Some(Duration::from_secs(25)),
        allow_input: true,
        audio: Some(uu_core::native::AudioSource::Synthetic),
        clipboard: Some(host_clipboard.clone()),
    };
    let (host_stop, rx) = watch::channel(false);
    let host = tokio::spawn(uu_core::native::serve(config, rx));
    let invitation = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(invite) = uu_core::native_pairing::Invitation::load(&invitation) {
                break invite;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let stats = Arc::new(MediaStats::default());
    stats.audio.output_enabled.store(false, Ordering::Release);
    let clipboard = &stats.clipboard;
    clipboard.available.store(true, Ordering::Release);
    clipboard.enabled.store(true, Ordering::Release);
    clipboard.active.store(true, Ordering::Release);
    let (frames, mut images) = frame_channel();
    let (input, rx) = native_input::channel(true);
    let (viewer_stop, stop) = watch::channel(false);
    let view_stats = stats.clone();
    let viewer = tokio::spawn(async move {
        uu_core::native::view_with_input(&invitation, frames, view_stats, stop, rx).await
    });
    let result=async{
        for round in 0..3 {
            let wanted=round+1;
            tokio::time::timeout(Duration::from_secs(7),async{
                let mut frames=0;
                loop {
                    if images.generation()==wanted && stats.native_displays.catalog_epoch()==wanted {
                        while let Ok((_,w,h,_))=images.try_recv(){ensure!((w,h)==(320,240),"unexpected fixture size");frames+=1;}
                    }
                    if frames>=8 && stats.capabilities.allows(uu_core::capabilities::Capability::Input){break Ok::<_,anyhow::Error>(());}
                    if viewer.is_finished(){anyhow::bail!("viewer ended before generation {wanted}");}
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.context("new display generation did not deliver media")??;
            // Frontend focus becomes active again only after the new source is shown.
            stats.clipboard.active.store(true,Ordering::Release);
            tokio::time::timeout(Duration::from_secs(3),async{
                loop{if host_clipboard.active.load(Ordering::Acquire)&&stats.clipboard.supported.load(Ordering::Acquire){break;}
                    tokio::time::sleep(Duration::from_millis(20)).await;}
            }).await.context("clipboard did not resume after source renewal")?;
            ensure!(!host_clipboard.stopped.load(Ordering::Acquire)&&!stats.clipboard.stopped.load(Ordering::Acquire),"media renewal stopped the connection clipboard");
            *host_clipboard.local.lock().unwrap()=Some(format!("generated-screen-{round}"));
            tokio::time::timeout(Duration::from_secs(2),async{
                loop{if stats.clipboard.remote.lock().unwrap().take().as_deref()==Some(&format!("generated-screen-{round}")){break;}
                    tokio::time::sleep(Duration::from_millis(10)).await;}
            }).await.context("clipboard text missing after source renewal")?;
            input.send(InputCommand::Key{usage:4,pressed:true})?;
            tokio::time::sleep(Duration::from_millis(100)).await;
            println!("{}",serde_json::json!({"generation":wanted,"decoded":stats.decoded_total.load(Ordering::Relaxed),"errors":stats.errors.load(Ordering::Relaxed),"clipboard_resumed":true}));
            if round<2 {input.select_display(100+round,1)?;}
        }
        ensure!(stats.errors.load(Ordering::Relaxed)==0,"decode errors during renewal");
        ensure!(stats.audio.packets.load(Ordering::Relaxed)>0,"generated audio missing");
        Ok::<_,anyhow::Error>(())
    }.await;
    viewer_stop.send_replace(true);
    host_stop.send_replace(true);
    let viewed = tokio::time::timeout(Duration::from_secs(4), viewer).await??;
    let hosted = tokio::time::timeout(Duration::from_secs(4), host).await???;
    result?;
    viewed?;
    ensure!(
        host_clipboard.stopped.load(Ordering::Acquire),
        "final host shutdown must stop clipboard"
    );
    println!(
        "{}",
        serde_json::json!({"ok":true,"generations":3,"sent":hosted.sent,"input_events":hosted.input_events,"input_errors":hosted.input_errors,"no_user_desktop":true})
    );
    Ok(())
}
