//! Measure first-frame preparation on the owned generated portal fixture.
use anyhow::{ensure, Context, Result};
use std::{
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};
use tokio::sync::watch;
use uu_core::{
    media::{frame_channel, MediaStats},
    native, native_quality,
};

#[tokio::main]
async fn main() -> Result<()> {
    let runtime = std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR")?);
    let owned_compositor=std::env::var("UU_OWNED_WAYLAND_TEST").as_deref()==Ok("1")
        &&runtime.file_name().is_some_and(|name|name.to_string_lossy().starts_with("uu-wayland-input-"));
    ensure!(
        std::env::var("ECLIPSE_PORTAL_FIXTURE").as_deref() == Ok("1")
            && (std::env::var("WAYLAND_DISPLAY").as_deref() == Ok("owned-fixture")||owned_compositor),
        "requires tools/native/portal_smoke.py --case stream-startup"
    );
    let expected=std::env::var("ECLIPSE_STARTUP_EXPECTED_SIZE").unwrap_or_else(|_|"320x240".into());
    let (width,height)=expected.split_once('x').context("expected size must be WxH")?;
    let expected=(width.parse::<u32>()?,height.parse::<u32>()?);
    ensure!((2..=8192).contains(&expected.0)&&(2..=8192).contains(&expected.1),"invalid fixture size");
    let target_frames=std::env::var("ECLIPSE_STARTUP_FRAMES").unwrap_or_else(|_|"60".into()).parse::<usize>()?;
    let rounds=std::env::var("ECLIPSE_STARTUP_ROUNDS").unwrap_or_else(|_|"2".into()).parse::<usize>()?;
    let exercise_quality=std::env::var("ECLIPSE_STARTUP_EXERCISE_QUALITY").as_deref()==Ok("1");
    let exercise_input=std::env::var("ECLIPSE_STARTUP_EXERCISE_INPUT").as_deref()==Ok("1");
    let exercise_displays=std::env::var("ECLIPSE_STARTUP_EXERCISE_DISPLAYS").as_deref()==Ok("1");
    let orientation=std::env::var("ECLIPSE_STARTUP_VERIFY_ORIENTATION").as_deref()==Ok("1");
    ensure!(!exercise_input||owned_compositor,"input requires the owned compositor");
    ensure!(!exercise_displays||owned_compositor,"display switching requires the owned compositor");
    let source=if let Ok(monitor)=std::env::var("ECLIPSE_STARTUP_MONITOR") {
        ensure!(owned_compositor&&!uu_core::native::uses_desktop_portal(),"requires native capture in the owned compositor");
        let mut sharing=uu_core::device_book::Sharing::default();sharing.monitor=monitor;
        sharing.native().source
    } else {native::CaptureSource::Primary};
    ensure!((60..=18000).contains(&target_frames)&&(1..=8).contains(&rounds)
        &&target_frames*rounds<=108000,"invalid owned portal test duration");
    let session_duration=Duration::from_secs((target_frames as u64/24+20).max(40));
    for round in 0..rounds {
        let started = Instant::now();
        let invitation_path = runtime.join(format!("startup-{round}.eclipse-invite.json"));
        let config = native::HostConfig {
            bind: "127.0.0.1:0".parse()?,
            source: source.clone(),
            invitation_file: invitation_path.clone(),
            invitation_lifetime: Duration::from_secs(60),
            fps: 30,
            bitrate_kbps: 2000,
            encoder: native::EncoderPreference::Auto,
            session_limit: Some(session_duration),
            allow_input: true,
            audio: None,
            clipboard: None,
            encoder_tuning: Default::default(),
            session: None,
            codec: Default::default(),
            rate_control: Default::default(),
            video_policy: Some(native_quality::Policy {
                codecs: vec![
                    native::VideoCodec::H264,
                    native::VideoCodec::Hevc,
                    native::VideoCodec::Av1,
                ],
                preferred_codec: None,
            }),
        };
        let (host_stop, stop) = watch::channel(false);
        let host = tokio::spawn(native::serve(config, stop));
        let invitation = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(value) = uu_core::native_pairing::Invitation::load(&invitation_path) {
                    break value;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        let stats = Arc::new(MediaStats::default());
        stats.audio.output_enabled.store(false, Ordering::Release);
        let diagnostics = runtime.join(format!("startup-diagnostics-{round}"));
        stats.diagnostics.start([diagnostics.clone()]);
        stats
            .native_quality_request
            .set(native_quality::Request {
                fps: 30,
                bounds: None,
                bitrate_kbps: Some(2000),
                codec: None,
            })
            .unwrap();
        let (frames, mut images) = frame_channel();
        let (viewer_stop, stop) = watch::channel(false);
        let view_stats = stats.clone();
        let (quality, input)=uu_core::native_input::channel(exercise_input);
        let viewer =
            tokio::spawn(async move { native::view_with_input(&invitation, frames, view_stats, stop,input).await });
        let mut first_frame_ms = None;
        let mut applied_quality=Vec::new();
        let mut input_sent=false;
        let mut display_switches=Vec::new();
        let received = tokio::time::timeout(session_duration+Duration::from_secs(5), async {
            let mut count = 0;
            let mut requested=0;
            while count < target_frames || (exercise_quality && applied_quality.len()<2) {
                while let Ok((_, width, height, rgba)) = images.try_recv() {
                    ensure!(
                        (width, height) == expected,
                        "unexpected generated frame geometry"
                    );
                    first_frame_ms.get_or_insert_with(|| started.elapsed().as_millis());
                    count += 1;
                    if orientation && count>=10 {
                        for (x,y,wanted) in [(width/4,height/4,[255u8,0,0]),(width*3/4,height/4,[0,255,0]),
                            (width/4,height*3/4,[0,0,255]),(width*3/4,height*3/4,[255,255,255])] {
                            let index=((y as usize*width as usize)+x as usize)*4;
                            let actual=&rgba[index..index+3];
                            ensure!(actual.iter().zip(wanted).all(|(a,b)|a.abs_diff(b)<70),"captured output orientation/color mismatch: {actual:?} expected {wanted:?}");
                        }
                    }
                }
                if exercise_quality {
                    if let Some(applied)=stats.native_quality_applied.lock().unwrap().clone() {
                        if applied.request==requested && applied_quality.len()<requested as usize {
                            ensure!(applied.error.is_none(),"runtime quality failed: {:?}",applied.error);
                            ensure!(applied.settings.fps==if requested==1{60}else{24},"wrong applied frame rate");
                            applied_quality.push(applied);
                        }
                    }
                    if (count>=20&&requested==0)||(count>=40&&requested==1&&applied_quality.len()==1) {
                        requested+=1;
                        quality.set_quality(requested,native_quality::Update{fps:if requested==1{60}else{24},bitrate_kbps:Some(2000),rate_control:None})?;
                    }
                }
                if exercise_input && !input_sent && count>=10
                    &&stats.capabilities.allows(uu_core::capabilities::Capability::Input) {
                    use uu_core::native_input::{InputCommand as I,MouseButton};
                    for command in [I::Absolute{x:32768,y:32768},
                        I::Button{button:MouseButton::Left,pressed:true},
                        I::Button{button:MouseButton::Left,pressed:false},
                        I::Key{usage:4,pressed:true},I::Key{usage:4,pressed:false}] {
                        quality.send(command)?;
                    }
                    input_sent=true;
                }
                ensure!(
                    !viewer.is_finished(),
                    "viewer ended before generated frames"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            if exercise_displays {
                let catalog=stats.remote_screens.lock().unwrap().clone();
                ensure!(catalog.screens.len()==2,"native host must list both real outputs");
                let initial=catalog.current_id;
                let other=catalog.screens.iter().find(|s|s.id!=initial).context("missing alternate display")?.id;
                for (request,target) in [(100,other),(101,initial)] {
                    let output=catalog.screens.iter().find(|s|s.id==target).unwrap();
                    let before=images.generation();
                    quality.select_display(request,target)?;
                    tokio::time::timeout(Duration::from_secs(7),async {
                        let mut frames=0;
                        loop {
                            if images.generation()>before && stats.native_displays.catalog_epoch()==images.generation() {
                                while let Ok((_,w,h,_))=images.try_recv(){
                                    ensure!((w,h)==(output.width,output.height),"new output geometry mismatch");frames+=1;
                                }
                            }
                            if frames>=8 {break Ok::<_,anyhow::Error>(());}
                            ensure!(!viewer.is_finished(),"viewer ended during display switch");
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    }).await.context("native display switch timed out")??;
                    let fps=stats.configured_fps.load(Ordering::Acquire);
                    if exercise_quality {ensure!(fps==24,"display switching reset the live frame rate");}
                    display_switches.push(serde_json::json!({"name":output.display_name,"width":output.width,"height":output.height,"fps":fps,"generation":images.generation()}));
                }
            }
            Ok::<_, anyhow::Error>(count)
        })
        .await
        .context("generated first-frame probe timed out");
        viewer_stop.send_replace(true);
        host_stop.send_replace(true);
        let viewed = tokio::time::timeout(Duration::from_secs(4), viewer).await??;
        let hosted = tokio::time::timeout(Duration::from_secs(4), host).await??;
        // Preserve the underlying portal/negotiation error if the viewer ends
        // before a frame. The frame-count guard alone hides the useful cause.
        viewed.context("owned portal viewer failed")?;
        let hosted=hosted.context("owned portal host failed")?;
        let frames = received??;
        ensure!(!exercise_input||(input_sent&&hosted.input_events>=5&&hosted.input_errors==0),"owned input was not delivered");
        ensure!(stats.errors.load(Ordering::Relaxed) == 0, "decode errors");
        // The diagnostic writer is bounded and asynchronous. Wait for its final
        // negotiation metadata; only numeric phase events enter this report.
        let phases = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let mut values = Vec::new();
                for entry in std::fs::read_dir(&diagnostics)?.flatten() {
                    let text = std::fs::read_to_string(entry.path())?;
                    for line in text.lines() {
                        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                            continue;
                        };
                        if matches!(
                            value["event"].as_str(),
                            Some(
                                "native_local_ice_gather"
                                    | "native_answer_wait"
                                    | "native_host_preparation"
                            )
                        ) {
                            values.push(value);
                        }
                    }
                }
                if values
                    .iter()
                    .any(|v| v["event"] == "native_host_preparation")
                {
                    break Ok::<_, anyhow::Error>(values);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        println!(
            "{}",
            serde_json::json!({"ok":true,"round":round,"frames":frames,"errors":0,
            "first_frame_ms":first_frame_ms,"phases":phases,"applied_quality":applied_quality,"input_sent":input_sent,"display_switches":display_switches,"generated_video":true,"no_user_desktop":true})
        );
    }
    Ok(())
}
