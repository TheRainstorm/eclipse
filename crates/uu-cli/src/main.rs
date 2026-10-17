//! `uu` — a native reimplementation of the NetEase UU Remote client.
//!
//! Both roles live in one binary:
//! * `uu host`      — expose this machine (screen, input) to a controller
//! * `uu control`   — view and drive another machine
//!
//! The protocol work is in `uu-proto`/`uu-core`; the platform work is in
//! `uu-capture`/`uu-input`.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use uu_core::signal::SignedRequest;
use uu_core::UuApi;
use uu_core::{Config, Role, Session, SessionState, SignalClient};

#[derive(Parser, Debug)]
#[command(
    name = "uu",
    version,
    about = "Native UU Remote client (Windows/Linux)",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Report what this machine can do (backends, session type, paths).
    Probe,
    /// Query NetEase's public release feed (platform ids 1..=5).
    ReleaseCheck,
    /// List devices on the signed-in account.
    Devices,
    /// Control another machine.
    Control {
        /// Device id reported by `uu devices`.
        device: String,
    },
    /// Allow this machine to be controlled.
    Host,
    /// Register this machine (once) and log in by scanning a QR code.
    Login {
        /// Device name shown on the account's device list.
        #[arg(long, default_value = "uu-rs-linux")]
        name: String,
    },
    /// Send one signed API request and print the JSON response.
    ///
    /// Useful for mapping the remaining endpoints: the server names the
    /// business parameters it is missing.
    Api {
        #[arg(long, default_value = "GET")]
        method: String,
        /// Path including query string, e.g. /api/v1/device/list
        path: String,
        /// JSON body (POST/PUT).
        #[arg(long)]
        body: Option<String>,
        /// Bearer token from a completed login.
        #[arg(long)]
        token: Option<String>,
        /// Device id; generated if omitted.
        #[arg(long)]
        device_id: Option<String>,
        #[arg(long)]
        user_id: Option<String>,
        #[arg(long, default_value = "en")]
        language: String,
    },
    /// Send a series of keyboard-encoding variants (diagnostic).
    KbdProbe {
        device: String,
    },
    /// Connect and type text through the CONTROL input channel.
    KbdTest {
        device: String,
        #[arg(long, default_value = "hello")]
        text: String,
    },
    /// Connect and send a single absolute mouse move (diagnostic).
    MouseMove {
        device: String,
        #[arg(long)]
        x: f64,
        #[arg(long)]
        y: f64,
        /// Ignored: retained for CLI compatibility.
        #[arg(long, default_value_t = 7)]
        kind: u32,
        /// Ignored: retained for CLI compatibility.
        #[arg(long, default_value_t = 0)]
        channel: u16,
        /// Also send a left-button press/release after the move.
        #[arg(long, default_value_t = false)]
        click: bool,
    },
    /// End-to-end pipeline check: capture -> H.264 -> UDP -> decode -> PSNR.
    ///
    /// Runs entirely on loopback and never contacts NetEase, so it validates
    /// the media path independently of the authentication work.
    SelfLoop {
        /// Number of frames to push through the pipeline.
        #[arg(long, default_value_t = 120)]
        frames: u32,
        /// Capture width (even numbers only; rounded down for 4:2:0).
        #[arg(long, default_value_t = 640)]
        width: u32,
        /// Capture height.
        #[arg(long, default_value_t = 360)]
        height: u32,
        #[arg(long, default_value_t = 30)]
        fps: u32,
        #[arg(long, default_value_t = 4000)]
        bitrate_kbps: u32,
        /// Datagram payload budget.
        #[arg(long, default_value_t = uu_proto::pkt::DEFAULT_MTU)]
        mtu: usize,
        /// Fail the run if average decoded PSNR falls below this.
        #[arg(long, default_value_t = 30.0)]
        min_psnr: f64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    uu_core::install_crypto_provider();
    let cli = Cli::parse();
    let config = Config::from_env()?;
    config.ensure_dirs().await?;

    match cli.command {
        Command::Probe => probe(&config),
        Command::ReleaseCheck => release_check(&config).await,
        Command::Devices => {
            let client = SignalClient::new(&config)?;
            let api = UuApi::new(client, config.at("state.json"));
            let body = api
                .device_list()
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{}", serde_json::to_string_pretty(&body).unwrap_or_default());
            Ok(())
        }
        Command::Control { device } => {
            let client = SignalClient::new(&config)?;
            let api = UuApi::new(client, config.at("state.json"));
            let force = std::env::var("UU_FORCE_JOIN").map(|v| v == "1").unwrap_or(false);
            let room = api
                .room_join(&device, force)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let data = room.get("data").cloned().unwrap_or_default();
            let token = data["token"].as_str().context("room/join: missing token")?.to_string();
            let server = data["signaling_server"]
                .as_str()
                .context("room/join: missing signaling_server")?;
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            let url = format!(
                "{}/socket.io/?EIO=4&transport=websocket&t={ts}",
                server.trim_end_matches('/')
            );
            println!("signaling {url}");
            let mut sio = uu_core::socketio::SocketIo::connect(&url, &token).await?;
            let ctrl = uuid::Uuid::new_v4().to_string();
            let dev_id = api.state.device_id.clone().unwrap_or_default();
            let app = uu_core::socketio::encode_app_data(&dev_id);
            let streamer = serde_json::json!({
                "control_id": ctrl,
                "device_capability": {
                    "display_info": [{"id":0,"fps":60,"type":0,"hdr":-2}],
                    "video_codec_capability": [{
                        "video_codec":1,"width":1920,"height":1080,
                        "chroma_sampling":1,"bit_depth":8,"codec_impl":32
                    }],
                    "ice_id": ""
                }
            })
            .to_string();
            let body = serde_json::json!({
                "app_control_id": ctrl,
                "app_data": {"_placeholder": true, "num": 0},
                "geo_exit_ip": std::env::var("UU_GEO_IP").unwrap_or_else(|_| "36.159.243.209".into()),
                "streamer_data": streamer,
            });
            sio.emit("control", body, &[app]).await?;
            println!("control sent");

            let mut success = None;
            let mut forwarded = 0;
            while success.is_none() || forwarded < 2 {
                let (ev, data, _) = sio.recv_event().await?;
                println!("event {ev}");
                match ev.as_str() {
                    "success" => success = Some(data),
                    "forward_setting" => forwarded += 1,
                    "fail" => anyhow::bail!("control failed: {data}"),
                    _ => {}
                }
            }
            let success = success.context("missing success event")?;
            eprintln!("iceServers={}", success["iceServers"]);
            let ice = uu_core::media::ice_servers_from_json(&success["iceServers"]);
            let media = uu_core::media::ControllerMedia::new(ice).await?;
            let sdp = media.offer().await?;
            std::fs::write("/tmp/offer_rust.sdp", &sdp).ok();
            let gz = uu_core::socketio::gzip_sdp04(&sdp)?;
            let ice_id = success["ice_id"].as_str().unwrap_or_default().to_string();
            let target = success["client_id"].as_str().unwrap_or_default().to_string();
            sio.emit(
                "soac",
                serde_json::json!({
                    "client_id": target,
                    "data": {
                        "app_control_id": ctrl,
                        "gzip_sdp": {"_placeholder": true, "num": 0},
                        "ice_id": ice_id,
                        "ice_network_type": 0,
                        "sdp": "",
                        "type": "offer"
                    }
                }),
                &[gz],
            )
            .await?;
            println!("offer sent ({} bytes sdp)", sdp.len());

            let mut answered = false;
            let mut answer_at: Option<tokio::time::Instant> = None;
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(45);
            while tokio::time::Instant::now() < deadline {
                if let Some(t) = answer_at {
                    if t.elapsed() > std::time::Duration::from_secs(5) {
                        break;
                    }
                }
                let next = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    sio.recv_event(),
                )
                .await;
                let (ev, data, atts) = match next {
                    Ok(Ok(v)) => v,
                    Ok(Err(e)) => {
                        println!("recv error: {e}");
                        break;
                    }
                    Err(_) => {
                        println!("signaling idle");
                        continue;
                    }
                };
                if ev != "soac" {
                    println!("event {ev}");
                    continue;
                }
                let d = &data["data"];
                match d["type"].as_str() {
                    Some("answer") if !atts.is_empty() => {
                        let answer = uu_core::socketio::gunzip_sdp04(&atts[0])?;
                        std::fs::write("/tmp/answer_rust.sdp", &answer).ok();
                        for line in answer.lines().filter(|l| l.contains("candidate")) {
                            println!("ANS {}", line);
                        }
                        media.set_answer(&answer).await?;
                        println!("answer set ({} bytes)", answer.len());
                        answered = true;
                        answer_at = Some(tokio::time::Instant::now());
                    }
                    Some("candidate") => {
                        if let Some(c) = d.get("candidate") {
                            let cand = c["candidate"].as_str().unwrap_or_default().to_string();
                            let mid = c["sdpMid"].as_str().unwrap_or("0").to_string();
                            let idx = c["sdpMLineIndex"].as_u64().unwrap_or(0) as u16;
                            if !cand.is_empty() {
                                println!("candidate {}", cand);
                                media.add_candidate(cand, mid, idx).await?;
                            }
                        }
                    }
                    _ => {}
                }
            }

            if answered {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                sio.emit(
                    "ice_finished_log",
                    serde_json::json!({"app_control_id": ctrl, "ice_id": ice_id}),
                    &[],
                )
                .await?;
                println!("ice_finished_log sent; receiving media for 30s...");
                let end = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
                while tokio::time::Instant::now() < end {
                    if let Ok(Ok((ev, data, _))) = tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        sio.recv_event(),
                    )
                    .await
                    {
                        if ev == "soac" && data["data"]["type"] == "candidate" {
                            if let Some(c) = data["data"].get("candidate") {
                                let cand = c["candidate"].as_str().unwrap_or_default().to_string();
                                let mid = c["sdpMid"].as_str().unwrap_or("0").to_string();
                                let idx = c["sdpMLineIndex"].as_u64().unwrap_or(0) as u16;
                                if !cand.is_empty() {
                                    media.add_candidate(cand, mid, idx).await?;
                                }
                            }
                        }
                    }
                }
            }
            Ok(())
        }
        Command::Host => {
            let session = Session::new(Role::Host);
            tracing::info!(state = session.state().as_str(), "host requested");
            anyhow::bail!("host path not implemented yet (see docs/roadmap.md)")
        }
        Command::Login { name } => {
            let client = SignalClient::new(&config)?;
            let mut api = UuApi::new(client, config.at("state.json"));
            if !api.has_identity() {
                let dev = api.register(&name).await?;
                println!("registered device {} ({})", dev.alias, dev.device_id);
            } else {
                println!("using existing identity {}", api.state.identity.client_id);
            }
            // The QR is short-lived, so regenerate it on expiry. While the
            // QR is alive we keep polling the *same* code: status 2 means
            // "scanned, waiting for the phone to confirm", not "start over".
            loop {
                let qr = api.gen_qr_login().await?;
                println!();
                println!("{}", render_qr(&qr.jump_url)?);
                println!();
                println!("扫码，或把链接发到手机打开：");
                println!("  {}", qr.jump_url);
                println!();
                println!("waiting for scan...");
                // The server advances one step per poll: we must echo back
                // the state it last returned (1 -> 2 -> 3 ...). Sending a
                // constant 1 leaves the flow stuck at "scanned".
                let mut status: i64 = 1;
                let mut last_state: Option<i64> = None;
                loop {
                    let full = api.poll_qr_login(&qr, status as i32).await?;
                    let resp = &full.body;
                    let data = &resp["data"];
                    if let Some(token) = data["token"]
                        .as_str()
                        .or_else(|| data["access_token"].as_str())
                        .filter(|t| !t.is_empty() && *t != qr.token)
                    {
                        let uid = data["user_id"].as_str().or_else(|| data["uid"].as_str());
                        api.store_token(token, uid.map(str::to_string))?;
                        println!(
                            "logged in{}",
                            uid.map(|u| format!(" as {u}")).unwrap_or_default()
                        );
                        return Ok(());
                    }
                    let state = data["login_status"]
                        .as_i64()
                        .or_else(|| data["status"].as_i64());
                    if state != last_state {
                        println!(
                            "status={:?}  body={}",
                            state,
                            serde_json::to_string(&resp).unwrap_or_default()
                        );
                        for (k, v) in &full.headers {
                            if k.contains("token") || k.contains("auth") || k.contains("cookie") {
                                println!("  header {k}: {v}");
                            }
                        }
                        last_state = state;
                    }
                    match state {
                        Some(4) => {
                            // The QR was consumed. This is the moment to
                            // exchange it for an access token; doing it any
                            // earlier returns 1194.
                            match api.exchange_qr(&qr).await {
                                Ok(v) => {
                                    if let Some(tok) = v["data"]["token"].as_str() {
                                        let uid = v["data"]["user_id"].as_str();
                                        api.store_token(tok, uid.map(str::to_string))?;
                                        println!(
                                            "logged in as {} ({})",
                                            uid.unwrap_or("?"),
                                            v["data"]["nickname"].as_str().unwrap_or("")
                                        );
                                        return Ok(());
                                    }
                                    println!("exchange: {}", serde_json::to_string(&v).unwrap_or_default());
                                }
                                Err(e) => println!("exchange failed: {e}"),
                            }
                            println!("二维码已过期，正在刷新…");
                            break;
                        }
                        Some(s) if s > status => status = s,
                        _ => tokio::time::sleep(std::time::Duration::from_millis(200)).await,
                    }
                }
            }
        }
        Command::Api { method, path, body, token, device_id, user_id, language } => {
            let client = SignalClient::new(&config)?;
            let saved = uu_core::ClientState::load(&config.at("state.json"));
            let mut identity = saved.identity.clone();
            identity.language = language.clone();
            identity.country = language;
            identity.device_id = device_id
                .or(saved.device_id.clone())
                .unwrap_or_default();
            identity.user_id = user_id.or(saved.user_id.clone());
            let parsed = match body {
                Some(raw) => Some(
                    serde_json::from_str(&raw)
                        .with_context(|| format!("--body is not valid JSON: {raw}"))?,
                ),
                None => None,
            };
            let resp = client
                .call(
                    &identity,
                    token.as_deref().or(saved.token.as_deref()),
                    SignedRequest { method: &method, path_with_query: &path, body: parsed },
                )
                .await?;
            println!("HTTP {}", resp.status);
            println!("{}", serde_json::to_string_pretty(&resp.body).unwrap_or_default());
            Ok(())
        }
        Command::KbdProbe { device } => {
            let geo = std::env::var("UU_GEO_IP").unwrap_or_else(|_| "36.159.243.209".into());
            let uu_core::controller::ControllerHandle { events, frames, input, close } =
                uu_core::controller::spawn(config, device, geo)?;
            std::thread::spawn(move || {
                let mut frames = frames;
                while frames.blocking_recv().is_some() {}
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
            while std::time::Instant::now() < deadline {
                match events.recv_timeout(std::time::Duration::from_millis(500)) {
                    Ok(uu_core::controller::ControllerEvent::Status(s)) => {
                        println!("status: {s}");
                        if s.contains("已连接") { break; }
                    }
                    Ok(uu_core::controller::ControllerEvent::Error(e)) => anyhow::bail!("{e}"),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => break,
                }
            }
            let send = |label: &str, payload: serde_json::Value| {
                println!("variant {label}");
                let _ = input.send(payload.to_string().into_bytes());
            };
            for (label, _vk, _kind, _ptype, payload) in [
                ("k8-key", 65u32, 8u32, 0x51u8, serde_json::json!({"action":"kbd_press","interrept":"i","key":65,"kseq":1})),
                ("k8-i-p", 66, 8, 0x51, serde_json::json!({"action":"kbd_press","interrept":"p","key":66,"kseq":2})),
                ("k15-key", 67, 15, 0x51, serde_json::json!({"action":"kbd_press","interrept":"i","key":67,"kseq":3})),
                ("k16-key", 68, 16, 0x51, serde_json::json!({"action":"kbd_press","interrept":"i","key":68,"kseq":4})),
                ("k8-p55", 69, 8, 0x55, serde_json::json!({"action":"kbd_press","interrept":"i","key":69,"kseq":5})),
                ("k8-vk", 70, 8, 0x51, serde_json::json!({"action":"kbd_press","interrept":"i","vk":70,"kseq":6})),
                ("k8-code", 71, 8, 0x51, serde_json::json!({"action":"kbd_press","interrept":"i","code":71,"kseq":7})),
                ("k8-keypress", 72, 8, 0x51, serde_json::json!({"action":"key_press","interrept":"i","key":72,"kseq":8})),
            ] {
                send(label, payload);
                std::thread::sleep(std::time::Duration::from_millis(700));
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
            let _ = close.send(());
            Ok(())
        }
        Command::KbdTest { device, text } => {
            let geo = std::env::var("UU_GEO_IP").unwrap_or_else(|_| "36.159.243.209".into());
            let h = uu_core::controller::spawn(config, device, geo)?;
            {
                let mut frames = h.frames;
                std::thread::spawn(move || { while frames.blocking_recv().is_some() {} });
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            let mut ready = false;
            while std::time::Instant::now() < deadline {
                match h.events.recv_timeout(std::time::Duration::from_millis(500)) {
                    Ok(uu_core::controller::ControllerEvent::Status(s)) => {
                        println!("status: {s}");
                        if s.contains("已连接") { ready = true; break; }
                    }
                    Ok(uu_core::controller::ControllerEvent::Error(e)) => {
                        anyhow::bail!("controller error: {e}");
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => break,
                }
            }
            if !ready {
                anyhow::bail!("controller did not connect");
            }
            let keys: Vec<(u32, bool)> = text
                .chars()
                .flat_map(|c| {
                    let vk = match c {
                        'a'..='z' => Some(c.to_ascii_uppercase() as u32),
                        'A'..='Z' => Some(c as u32),
                        '0'..='9' => Some(c as u32),
                        ' ' => Some(0x20),
                        '\n' => Some(0x0D),
                        _ => None,
                    };
                    vk.into_iter().flat_map(|v| [(v, true), (v, false)])
                })
                .collect();
            for (i, (vk, down)) in keys.into_iter().enumerate() {
                let payload = serde_json::json!({
                    "action": if down { "kbd_press" } else { "kbd_release" },
                    "interrept": "i", "key": vk, "kseq": i + 1
                });
                h.input.send(payload.to_string().into_bytes()).ok();
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
            println!("typed {} chars via CONTROL", text.chars().count());
            let hold = std::env::var("UU_HOLD_SECS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(8);
            std::thread::sleep(std::time::Duration::from_secs(hold));
            let _ = h.close.send(());
            Ok(())
        }
        Command::MouseMove { device, x, y, kind: _kind, channel: _channel, click } => {
            let geo = std::env::var("UU_GEO_IP").unwrap_or_else(|_| "36.159.243.209".into());
            let h = uu_core::controller::spawn(config, device, geo)?;
            {
                let mut frames = h.frames;
                std::thread::spawn(move || { while frames.blocking_recv().is_some() {} });
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(70);
            let mut ready = false;
            while std::time::Instant::now() < deadline {
                match h.events.recv_timeout(std::time::Duration::from_millis(500)) {
                    Ok(uu_core::controller::ControllerEvent::Status(s)) => {
                        println!("status: {s}");
                        if s.contains("已连接") { ready = true; }
                    }
                    Ok(uu_core::controller::ControllerEvent::Error(e)) => anyhow::bail!("{e}"),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => break,
                }
                if ready { break; }
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
            let payload = serde_json::json!({
                "abs_x": x, "abs_y": y,
                "action": "mouse_move_absolute", "screen_id": 1
            });
            h.input.send(payload.to_string().into_bytes()).ok();
            println!("sent mouse_move x={x} y={y}");
            if click {
                std::thread::sleep(std::time::Duration::from_millis(400));
                for action in ["mouse_press", "mouse_release"] {
                    let p = serde_json::json!({"action": action, "button": 1, "screen_id": 1});
                    h.input.send(p.to_string().into_bytes()).ok();
                    std::thread::sleep(std::time::Duration::from_millis(150));
                }
                println!("sent click");
            }
            let hold = std::env::var("UU_HOLD_SECS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(6);
            std::thread::sleep(std::time::Duration::from_secs(hold));
            let _ = h.close.send(());
            Ok(())
        }
        Command::SelfLoop { frames, width, height, fps, bitrate_kbps, mtu, min_psnr } => {
            let cfg = uu_core::SelfLoopConfig {
                frames,
                width,
                height,
                fps,
                bitrate_kbps,
                mtu,
                min_psnr_db: min_psnr,
            };
            uu_core::selfloop::run(cfg).await
        }
    }
}

fn probe(config: &Config) -> Result<()> {
    println!("uu {} — environment report\n", env!("CARGO_PKG_VERSION"));
    println!("platform          {} / {}", std::env::consts::OS, std::env::consts::ARCH);
    println!("data dir          {}", config.data_dir.display());
    println!("state dir         {}", config.state_dir.display());
    println!("signaling host    {}", config.signaling_host);

    let backends = uu_capture::available_backends();
    println!(
        "capture backends  {}",
        if backends.is_empty() { "none".to_string() } else { backends.join(", ") }
    );

    match uu_input::default_injector() {
        Ok(_) => println!("input injector    available"),
        Err(e) => println!("input injector    unavailable ({e})"),
    }

    let display = std::env::var("WAYLAND_DISPLAY")
        .map(|w| format!("wayland ({w})"))
        .or_else(|_| std::env::var("DISPLAY").map(|d| format!("x11 ({d})")))
        .unwrap_or_else(|_| "headless".into());
    println!("display           {display}");

    // Round out the report with the protocol revision so bug reports are
    // self-describing.
    let mut session = Session::new(Role::Controller);
    session.transition(SessionState::Authenticating)?;
    println!("protocol          {}", uu_proto::PROTOCOL_REVISION);
    Ok(())
}

async fn release_check(config: &Config) -> Result<()> {
    let client = SignalClient::new(config)?;
    println!("official release feed ({})\n", config.signaling_host);
    for target in uu_core::signal::RELEASE_TARGETS {
        match client.release_url(target.id).await {
            Ok(url) => {
                let name = url.rsplit('/').next().unwrap_or(&url);
                println!("  {:>2} {:<12} {}", target.id, target.name, name);
            }
            Err(e) => println!("  {:>2} {:<12} error: {e}", target.id, target.name),
        }
    }
    Ok(())
}

/// Render a URL as a terminal QR code.
fn render_qr(data: &str) -> Result<String> {
    use qrcode::render::unicode;
    let code = qrcode::QrCode::new(data.as_bytes()).context("encoding QR")?;
    Ok(code.render::<unicode::Dense1x2>().quiet_zone(true).build())
}
