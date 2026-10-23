//! End-to-end video and control only inside an explicit owned X11 fixture.
#[cfg(target_os = "linux")]
#[path = "../../../tools/native/owned_display.rs"]
mod owned_display;
#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use anyhow::{ensure, Context};
    use std::{
        sync::{atomic::Ordering, Arc},
        time::{Duration, Instant},
    };
    use tokio::sync::watch;
    use uu_core::{
        capabilities::Capability,
        media::{frame_channel, MediaStats},
        native_input::{self, InputCommand as I},
    };
    use uu_input::{InputEvent, InputInjector, KeyCode};
    use x11rb::{
        connection::Connection,
        protocol::xproto::{
            ConnectionExt as _, CreateWindowAux, EventMask, InputFocus, WindowClass,
        },
        COPY_FROM_PARENT,
    };
    let mut args = std::env::args().skip(1);
    ensure!(
        args.next().as_deref() == Some("--x11-display"),
        "explicit owned --x11-display is required"
    );
    let display = args.next().context("missing display")?;
    owned_display::check(&display)?;
    ensure!(
        std::env::var("DISPLAY").ok().as_deref() == Some(&display),
        "capture and fixture must use the same owned display"
    );
    let readonly = match args.next().as_deref() {
        None => false,
        Some("--read-only") => true,
        _ => anyhow::bail!("unknown fixture argument"),
    };
    ensure!(args.next().is_none(), "unexpected fixture argument");
    let (conn, index) = x11rb::connect(Some(&display))?;
    let screen = &conn.setup().roots[index];
    let root = screen.root;
    let window = conn.generate_id()?;
    conn.create_window(
        COPY_FROM_PARENT as u8,
        window,
        root,
        10,
        10,
        600,
        300,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new()
            .background_pixel(screen.white_pixel)
            .event_mask(
                EventMask::KEY_PRESS
                    | EventMask::KEY_RELEASE
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE,
            ),
    )?
    .check()?;
    conn.map_window(window)?.check()?;
    conn.set_input_focus(InputFocus::PARENT, window, 0u32)?
        .check()?;
    ensure!(
        conn.get_input_focus()?.reply()?.focus == window,
        "owned fixture must have focus"
    );
    let mut foreign = uu_input::linux::XTestInjector::connect(Some(&display))?;
    foreign.inject(InputEvent::Key {
        key: KeyCode::ESC,
        pressed: true,
    })?;
    let invitation_path = std::env::temp_dir().join(format!(
        "uu-native-input-{}.eclipse-invite.json",
        uuid::Uuid::new_v4()
    ));
    struct InvitationFile(std::path::PathBuf);
    impl Drop for InvitationFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let invitation_file = InvitationFile(invitation_path.clone());
    let config = uu_core::native::HostConfig {session:None,video_policy:None,codec:Default::default(),rate_control:Default::default(),
        bind: "127.0.0.1:0".parse()?,
        source: uu_capture::CaptureSource::Primary,
        invitation_file: invitation_path.clone(),
        invitation_lifetime: Duration::from_secs(30),
        fps: 30,
        bitrate_kbps: 4000,
        encoder: uu_core::native::EncoderPreference::Software,
        session_limit: Some(Duration::from_secs(15)),
        allow_input: !readonly,
        audio:None, clipboard:None,
    };
    let (host_stop, host_stop_rx) = watch::channel(false);
    let host = tokio::spawn(uu_core::native::serve(config, host_stop_rx));
    let invitation = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(invitation) = uu_core::native_pairing::Invitation::load(&invitation_path) {
                break invitation;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("fixture invitation not ready")?;
    let stats = Arc::new(MediaStats::default());
    let worker_stats = stats.clone();
    let (frames, mut images) = frame_channel();
    let (input, input_rx) = native_input::channel(true);
    let (stop, stop_rx) = watch::channel(false);
    let viewer = tokio::spawn(async move {
        uu_core::native::view_with_input(&invitation, frames, worker_stats, stop_rx, input_rx).await
    });
    // A command queued before permission must never be replayed as a late key.
    input.send(I::Key {
        usage: 5,
        pressed: true,
    })?;
    let started = Instant::now();
    let mut observed = false;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            while let Ok((_, width, _, rgba)) = images.try_recv() {
                let p = (50 * width as usize + 50) * 4;
                ensure!(
                    rgba[p..p + 3].iter().all(|v| *v >= 240),
                    "decoded frame is not the owned white fixture"
                );
                observed = true;
            }
            if observed
                && stats.rtt_us.load(Ordering::Acquire) > 0
                && stats.decoded_total.load(Ordering::Acquire) >= 5
                && stats.native_timing.snapshot().host.is_some_and(|host|host.video.is_some())
            {
                break;
            }
            ensure!(!viewer.is_finished(), "fixture viewer ended early");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    ensure!(
        stats.capabilities.allows(Capability::Input) == !readonly,
        "host control permission mismatch"
    );
    let b = KeyCode(5).mapping()?.xkb as usize;
    ensure!(
        conn.query_keymap()?.reply()?.keys[b / 8] & (1 << (b % 8)) == 0,
        "pre-permission key was replayed"
    );
    let mut input_ack_us=None;
    if !readonly {
        input.send(I::Absolute {
            x: (70u32 * 65535 / 1279) as u16,
            y: (80u32 * 65535 / 719) as u16,
        })?;
        input.send(I::Button {
            button: uu_input::MouseButton::Left,
            pressed: true,
        })?;
        input.send(I::Button {
            button: uu_input::MouseButton::Left,
            pressed: false,
        })?;
        input.send(I::Key {
            usage: 0xe1,
            pressed: true,
        })?;
        input.send(I::Key {
            usage: 0x33,
            pressed: true,
        })?;
        input.send(I::Key {
            usage: 0x33,
            pressed: false,
        })?;
        // Deliberately leave a letter, modifier and drag held until disconnect.
        input.send(I::Key {
            usage: 4,
            pressed: true,
        })?;
        input.send(I::Button {
            button: uu_input::MouseButton::Middle,
            pressed: true,
        })?;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let map = conn.query_keymap().unwrap().reply().unwrap().keys;
                let key = KeyCode(4).mapping().unwrap().xkb as usize;
                if map[key / 8] & (1 << (key % 8)) != 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        tokio::time::timeout(Duration::from_secs(2),async {
            loop {
                if let Some(timing)=stats.native_timing.snapshot().input {input_ack_us=Some(timing.confirmation_rtt_us);break;}
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await?;
        // Hold beyond the control lease: heartbeats, rather than repeated
        // key packets, must keep a deliberate drag/hold alive.
        tokio::time::sleep(Duration::from_millis(5500)).await;
        let pointer = conn.query_pointer(root)?.reply()?;
        ensure!(
            (pointer.root_x, pointer.root_y) == (70, 80),
            "normalized pointer missed selected capture"
        );
        ensure!(pointer.mask.contains(x11rb::protocol::xproto::KeyButMask::BUTTON2),"control lease interrupted a live drag");
        let key=KeyCode(4).mapping()?.xkb as usize;
        ensure!(conn.query_keymap()?.reply()?.keys[key/8]&(1<<(key%8))!=0,"control lease interrupted a live key hold");
        let before=stats.decoded_total.load(Ordering::Relaxed);
        input.set_enabled(false);
        tokio::time::timeout(Duration::from_secs(2),async{
            loop {
                let map=conn.query_keymap().unwrap().reply().unwrap().keys;
                if !stats.capabilities.allows(Capability::Input)&&map[key/8]&(1<<(key%8))==0
                    &&!conn.query_pointer(root).unwrap().reply().unwrap().mask.contains(x11rb::protocol::xproto::KeyButMask::BUTTON2)
                    &&stats.decoded_total.load(Ordering::Relaxed)>before+3 {break;}
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await?;
        input.set_enabled(true);
        tokio::time::timeout(Duration::from_secs(2),async{
            while !stats.capabilities.allows(Capability::Input){tokio::time::sleep(Duration::from_millis(10)).await;}
        }).await?;
        input.send(I::Key{usage:4,pressed:true})?;
        input.send(I::Button{button:uu_input::MouseButton::Middle,pressed:true})?;
        tokio::time::timeout(Duration::from_secs(2),async{
            while conn.query_keymap().unwrap().reply().unwrap().keys[key/8]&(1<<(key%8))==0 {tokio::time::sleep(Duration::from_millis(10)).await;}
        }).await?;
    } else {
        ensure!(
            stats.input_messages_sent.load(Ordering::Relaxed) == 0,
            "readonly viewer forwarded input"
        );
    }
    let _ = stop.send(true);
    tokio::time::timeout(Duration::from_secs(3), viewer).await???;
    let report = tokio::time::timeout(Duration::from_secs(3), host).await???;
    let _ = host_stop.send(true);
    let map = conn.query_keymap()?.reply()?.keys;
    ensure!(!conn.query_pointer(root)?.reply()?.mask.contains(x11rb::protocol::xproto::KeyButMask::BUTTON2),"disconnect left a drag held");
    for usage in [4, 0xe1, 0x33] {
        let key = KeyCode(usage).mapping()?.xkb as usize;
        ensure!(
            map[key / 8] & (1 << (key % 8)) == 0,
            "disconnect left a session key held"
        );
    }
    let escape = KeyCode::ESC.mapping()?.xkb as usize;
    ensure!(
        map[escape / 8] & (1 << (escape % 8)) != 0,
        "disconnect released an unrelated key"
    );
    foreign.release_all()?;
    ensure!(
        report.input_errors == 0 && stats.errors.load(Ordering::Relaxed) == 0,
        "native fixture had input/video errors"
    );
    ensure!(
        (report.input_events > 0) == !readonly,
        "host input ownership was not enforced"
    );
    conn.destroy_window(window)?.check()?;
    drop(invitation_file);
    println!("{}",serde_json::json!({"input":"passed","readonly":readonly,"decoded":stats.decoded_total.load(Ordering::Relaxed),
        "input_events":report.input_events,"rtt_us":stats.rtt_us.load(Ordering::Relaxed),"input_ack_rtt_us":input_ack_us,
        "elapsed_ms":started.elapsed().as_millis(),"clean_disconnect":true,"native_timing":stats.native_timing.snapshot()}));
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This fixture requires an owned X11 test server");
    std::process::exit(2);
}
