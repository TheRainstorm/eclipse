//! Generated localhost hardware stream; no desktop, input, audio or accounts.
use anyhow::{ensure, Context, Result};
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tokio::{
    sync::watch,
    time::{sleep, timeout},
};
use uu_core::{
    media::{frame_channel, MediaStats},
    native,
    native_quality::{self, RateMode},
};

#[tokio::main]
async fn main() -> Result<()> {
    let codec = match std::env::args().nth(1).as_deref().unwrap_or("h264") {
        "h264" => native::VideoCodec::H264,
        "hevc" => native::VideoCodec::Hevc,
        "av1" => native::VideoCodec::Av1,
        _ => anyhow::bail!("expected h264, hevc or av1"),
    };
    let root = std::env::temp_dir().join(format!("eclipse-quality-probe-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    }
    struct Remove(std::path::PathBuf);
    impl Drop for Remove {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _remove = Remove(root.clone());
    let file = root.join("session.eclipse-invite.json");
    let config = native::HostConfig {
        bind: "127.0.0.1:0".parse()?,
        source: native::CaptureSource::Synthetic {
            width: 640,
            height: 360,
            fps: 60,
        },
        invitation_file: file.clone(),
        invitation_lifetime: Duration::from_secs(30),
        fps: 60,
        bitrate_kbps: 2000,
        encoder: native::EncoderPreference::Hardware,
        session_limit: Some(Duration::from_secs(25)),
        allow_input: false,
        audio: None,
        clipboard: None,
        encoder_tuning: Default::default(),
        session: None,
        codec,
        rate_control: Default::default(),
        video_policy: Some(native_quality::Policy {
            codecs: vec![codec],
            preferred_codec: None,
        }),
    };
    let (host_stop, rx) = watch::channel(false);
    let (status, host_status) = watch::channel(native::HostStatus::Starting);
    let host = tokio::spawn(native::serve_with_status(config, rx, Some(status)));
    let invitation = timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(invite) = uu_core::native_pairing::Invitation::load(&file) {
                return Ok(invite);
            }
            if host.is_finished() {
                anyhow::bail!("host ended before invitation");
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|v| v);
    let invitation = match invitation {
        Ok(invite) => invite,
        Err(error) => {
            host_stop.send_replace(true);
            let _ = host.await;
            return Err(error);
        }
    };
    let stats = Arc::new(MediaStats::default());
    stats.native_codec_capabilities.set(vec![codec]).unwrap();
    stats
        .native_quality_request
        .set(native_quality::Request {
            fps: 60,
            bounds: None,
            bitrate_kbps: Some(2000),
            codec: Some(codec),
        })
        .unwrap();
    stats
        .native_rate_request
        .set(native_quality::RateRequest { selection: None })
        .unwrap();
    let (frames, _images) = frame_channel();
    let (view_stop, rx) = watch::channel(false);
    let (input, receiver) = uu_core::native_input::channel(false);
    let observed = stats.clone();
    let viewer = tokio::spawn(async move {
        native::view_with_input(&invitation, frames, observed, rx, receiver).await
    });
    let result=timeout(Duration::from_secs(20),async {
        while stats.decoded_total.load(Ordering::Acquire)<8 {ensure!(!viewer.is_finished(),"viewer ended before first pictures");sleep(Duration::from_millis(5)).await;}
        let epoch=stats.native_media_epoch.load(Ordering::Acquire);let mut transitions=Vec::new();
        for (index,mode) in [RateMode::Cbr,RateMode::Vbr,RateMode::Cbr].into_iter().enumerate() {
            let request=index as u64+1;input.set_quality(request,native_quality::Update{fps:60,bitrate_kbps:Some(2000),rate_control:Some(mode)})?;
            loop {
                let applied=stats.native_quality_applied.lock().unwrap().clone();
                if let Some(applied)=applied.filter(|a|a.request==request){ensure!(applied.error.is_none(),"rate switch failed: {:?}",applied.error);ensure!(applied.settings.rate_control==Some(mode)&&applied.bitrate_kbps==2000,"wrong applied settings");break;}
                ensure!(!viewer.is_finished(),"viewer ended during rate switch");sleep(Duration::from_millis(5)).await;
            }
            let baseline=stats.decoded_total.load(Ordering::Acquire);
            while stats.decoded_total.load(Ordering::Acquire)<baseline+8 {ensure!(!viewer.is_finished(),"no pictures after rate switch");sleep(Duration::from_millis(5)).await;}
            ensure!(stats.native_media_epoch.load(Ordering::Acquire)==epoch&&stats.errors.load(Ordering::Acquire)==0,"media reconnected or decoder errors");
            let phase=host_status.borrow().clone();
            transitions.push(serde_json::json!({"mode":mode,"decoded":stats.decoded_total.load(Ordering::Acquire),"host":format!("{phase:?}")}));
        }
        Ok::<_,anyhow::Error>(transitions)
    }).await;
    view_stop.send_replace(true);
    host_stop.send_replace(true);
    let viewed = viewer.await;
    let hosted = host.await;
    let transitions = result.context("quality probe timed out")??;
    viewed??;
    let report = hosted??;
    println!(
        "{}",
        serde_json::json!({"quality":"passed","codec":codec,"transitions":transitions,"host_backend":report.encoder,"decoded":stats.decoded_total.load(Ordering::Acquire),"errors":stats.errors.load(Ordering::Acquire),"desktop_captured":false,"input_enabled":false,"clean_shutdown":true})
    );
    Ok(())
}
