//! `uu` — a native reimplementation of the NetEase UU Remote client.
//!
//! Both roles live in one binary:
//! * `uu host`      — expose this machine (screen, input) to a controller
//! * `uu control`   — view and drive another machine
//!
//! The protocol work is in `uu-proto`/`uu-core`; the platform work is in
//! `uu-capture`/`uu-input`.

mod native_cli;
mod lan_cli;
mod internet_cli;

use anyhow::{Context, Result};
use std::io::IsTerminal;
use clap::{Parser, Subcommand};
use uu_core::signal::SignedRequest;
use uu_core::UuApi;
use uu_core::{Config, Role, Session, SessionState, SignalClient};

#[derive(clap::ValueEnum,Clone,Copy,Debug)]
enum NativeEncoder { Software, Auto, Hardware, Vaapi }
impl NativeEncoder {
    fn preference(self)->uu_core::native::EncoderPreference {
        use uu_core::native::EncoderPreference as P;
        match self {Self::Software=>P::Software,Self::Auto=>P::Auto,Self::Hardware=>P::Hardware,Self::Vaapi=>P::Vaapi}
    }
}

#[derive(clap::ValueEnum,Clone,Copy,Debug)]
enum NativeCodec {H264,#[value(alias="h265")] Hevc,Av1}
#[derive(clap::ValueEnum,Clone,Copy,Debug)]
enum NativeRate {Cbr,Vbr}
#[derive(clap::Args,Clone,Debug)]
struct NativeVideoOptions {
    #[arg(long,value_enum,default_value="h264")]
    codec:NativeCodec,
    #[arg(long,value_enum,default_value="cbr")]
    rate_control:NativeRate,
    #[arg(long)]
    peak_kbps:Option<u32>,
}
impl NativeVideoOptions {
    fn policy(&self,target:u32)->Result<(uu_core::native::VideoCodec,uu_media::encoder::RateControl)>{
        let codec=match self.codec {NativeCodec::H264=>uu_core::native::VideoCodec::H264,NativeCodec::Hevc=>uu_core::native::VideoCodec::Hevc,NativeCodec::Av1=>uu_core::native::VideoCodec::Av1};
        let rate=match self.rate_control {NativeRate::Cbr=>{anyhow::ensure!(self.peak_kbps.is_none(),"--peak-kbps requires --rate-control vbr");uu_media::encoder::RateControl::Cbr},NativeRate::Vbr=>uu_media::encoder::RateControl::Vbr{peak_kbps:self.peak_kbps.unwrap_or(target.saturating_mul(2))}};
        Ok((codec,rate))
    }
}

fn synthetic_dimension(value: &str) -> std::result::Result<u32, String> {
    let n: u32 = value.parse().map_err(|_| "test dimension must be an integer")?;
    if (2..=8192).contains(&n) && n % 2 == 0 { Ok(n) }
    else { Err("test dimension must be even and within 2..=8192".into()) }
}

#[cfg(test)]
mod native_encoder_options {
    use super::*;
    #[test]
    fn synthetic_dimensions_are_explicit_bounded_and_do_not_affect_desktop_capture() {
        let base=["uu","native-host","--invitation-file","generated.eclipse-invite.json"];
        assert!(Cli::try_parse_from(base).is_ok());
        let mut args=base.to_vec();args.extend(["--synthetic-width","1920"]);
        assert!(Cli::try_parse_from(args).is_err());
        let mut args=base.to_vec();args.extend(["--synthetic","--synthetic-width","1920","--synthetic-height","1080"]);
        assert!(matches!(Cli::try_parse_from(args).unwrap().command,Command::NativeHost {synthetic_width:1920,synthetic_height:1080,..}));
        for value in ["0","17","8194"] {
            let mut args=base.to_vec();args.extend(["--synthetic","--synthetic-width",value]);
            assert!(Cli::try_parse_from(args).is_err());
        }
    }
    #[test]
    fn native_probe_progress_and_gap_limits_are_explicit_and_bounded() {
        let base=["uu","native-probe","--invitation-file","generated.eclipse-invite.json"];
        for option in ["--progress-seconds","--max-frame-gap-seconds"] {
            let mut invalid=base.to_vec();invalid.extend([option,"0"]);
            assert!(Cli::try_parse_from(invalid).is_err());
        }
        let mut args=base.to_vec();args.extend(["--progress-seconds","5","--max-frame-gap-seconds","10"]);
        assert!(matches!(Cli::try_parse_from(args).unwrap().command,Command::NativeProbe {progress_seconds:Some(5),max_frame_gap_seconds:Some(10),..}));
    }
    #[test]
    fn native_sharing_defaults_to_software_and_accepts_explicit_choices() {
        let base=["uu","native-host","--synthetic","--invitation-file","generated.eclipse-invite.json"];
        let command=Cli::try_parse_from(base).unwrap().command;
        assert!(matches!(command,Command::NativeHost {encoder:NativeEncoder::Software,..}));
        for (name,expected) in [("auto",uu_core::native::EncoderPreference::Auto),("hardware",uu_core::native::EncoderPreference::Hardware),("vaapi",uu_core::native::EncoderPreference::Vaapi)] {
            let mut args=base.to_vec();args.extend(["--encoder",name]);
            let Command::NativeHost {encoder,..}=Cli::try_parse_from(args).unwrap().command else {panic!("native-host expected")};
            assert_eq!(encoder.preference(),expected);
        }
        let mut invalid=base.to_vec();invalid.extend(["--encoder","unknown"]);
        assert!(Cli::try_parse_from(invalid).is_err());
    }
}

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
    /// Native streaming through replaceable internet connection services.
    Internet {
        #[command(subcommand)]
        command:internet_cli::Command,
    },
    /// Eclipse LAN discovery, persistent pairing and foreground sharing.
    Lan {
        #[command(subcommand)]
        command:lan_cli::Command,
    },
    /// Report what this machine can do (backends, session type, paths).
    Probe {
        /// Decode an Annex-B H.264 file with the decoder this machine selects
        /// and report what came out. Checks the hardware path without
        /// connecting to a device.
        #[arg(long, value_name = "FILE")]
        h264: Option<std::path::PathBuf>,
        /// Save the first decoded frame as a PPM image for visual inspection.
        #[arg(long, value_name = "FILE", requires = "h264")]
        frame: Option<std::path::PathBuf>,
    },
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
    /// List native capture monitors without reading desktop pixels.
    NativeSources,
    /// Native LAN host; desktop input requires an explicit --allow-input.
    NativeHost {
        #[command(flatten)]
        video:NativeVideoOptions,
        #[arg(long, default_value = "127.0.0.1:0")]
        bind: std::net::SocketAddr,
        /// NEW private .eclipse-invite.json file to share with the controller.
        #[arg(long)]
        invitation_file: std::path::PathBuf,
        #[arg(long, conflicts_with = "synthetic")]
        monitor: Option<String>,
        /// Permit remote selection among desktop monitors (Wayland asks locally again).
        #[arg(long, conflicts_with_all=["synthetic","monitor"])]
        all_monitors:bool,
        /// Use a moving test pattern instead of a desktop (default 1280x720).
        #[arg(long)]
        synthetic: bool,
        /// Generated test-source width; never changes a real monitor.
        #[arg(long, requires="synthetic", default_value_t=1280, value_parser=synthetic_dimension)]
        synthetic_width: u32,
        /// Generated test-source height; never changes a real monitor.
        #[arg(long, requires="synthetic", default_value_t=720, value_parser=synthetic_dimension)]
        synthetic_height: u32,
        /// Allow an invited viewer to request keyboard and mouse control.
        #[arg(long, conflicts_with = "synthetic")]
        allow_input: bool,
        /// Explicitly share system output audio for a real monitor source.
        #[arg(long, conflicts_with="synthetic")]
        audio:bool,
        /// Add generated audio to a synthetic test host.
        #[arg(long, requires="synthetic", conflicts_with="audio")]
        synthetic_audio:bool,
        #[arg(long, default_value_t = 60)]
        fps: u32,
        #[arg(long, default_value_t = 8000)]
        bitrate_kbps: u32,
        /// Native host encoder; hardware requires a supported platform GPU driver.
        #[arg(long,value_enum,default_value="software")]
        encoder: NativeEncoder,
        #[arg(long, default_value_t = 600)]
        pairing_seconds: u64,
        #[arg(long)]
        max_seconds: Option<u64>,
    },
    /// Decode native video without saving pictures or opening a GUI.
    NativeProbe {
        #[arg(long)]
        invitation_file: std::path::PathBuf,
        #[arg(long, default_value_t = 120)]
        frames: u64,
        #[arg(long, default_value_t = 15)]
        timeout_seconds: u64,
        /// New private JSON report containing metrics, never desktop images.
        #[arg(long)]
        report: Option<std::path::PathBuf>,
        /// Receive-only diagnostic loss injection; the network is unchanged.
        #[arg(long,value_parser=clap::value_parser!(u32).range(2..=10000))]
        audio_loss_every:Option<u32>,
        /// Print metadata-only progress periodically; disabled by default.
        #[arg(long,value_parser=clap::value_parser!(u64).range(1..=60))]
        progress_seconds:Option<u64>,
        /// Fail with a report if no visible frame arrives for this long after the first frame.
        #[arg(long,value_parser=clap::value_parser!(u64).range(1..=60))]
        max_frame_gap_seconds:Option<u64>,
    },
    /// Register this machine (once) and log in by scanning a QR code.
    Login {
        /// Registration name; defaults to the computer name with -Eclipse.
        #[arg(long, default_value_t = uu_core::config::uu_device_name())]
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
        .with_ansi(std::io::stdout().is_terminal())
        .init();

    uu_core::install_crypto_provider();
    let cli = Cli::parse();
    let config = Config::from_env()?;
    config.ensure_dirs().await?;

    match cli.command {
        Command::Internet{command}=>internet_cli::run(command,config).await,
        Command::Lan {command} => lan_cli::run(command,config.at("native-lan")).await,
        Command::Probe { h264, frame } => probe(&config, h264.as_deref(), frame.as_deref()),
        Command::NativeSources => native_cli::sources(),
        Command::NativeHost { video,bind, invitation_file, monitor, all_monitors, synthetic, synthetic_width, synthetic_height, allow_input, audio, synthetic_audio, fps, bitrate_kbps, encoder, pairing_seconds, max_seconds } => {
            let source=if all_monitors {uu_capture::CaptureSource::Desktop}else if synthetic {uu_capture::CaptureSource::Synthetic{width:synthetic_width,height:synthetic_height,fps}}else if let Some(name)=monitor {uu_capture::CaptureSource::Monitor(name)}else{uu_capture::CaptureSource::Primary};
            let (codec,rate_control)=video.policy(bitrate_kbps)?;
            native_cli::host(uu_core::native::HostConfig{video_policy:None,codec,rate_control,bind,source,invitation_file,invitation_lifetime:std::time::Duration::from_secs(pairing_seconds),fps,bitrate_kbps,encoder:encoder.preference(),session_limit:max_seconds.map(std::time::Duration::from_secs),allow_input,clipboard:None,audio:if audio{Some(uu_core::native::AudioSource::System)}else if synthetic_audio{Some(uu_core::native::AudioSource::Synthetic)}else{None}}).await
        },
        Command::NativeProbe {invitation_file,frames,timeout_seconds,report,audio_loss_every,progress_seconds,max_frame_gap_seconds} => native_cli::probe(invitation_file,frames,timeout_seconds,report,audio_loss_every.unwrap_or(0),progress_seconds,max_frame_gap_seconds).await,
        Command::ReleaseCheck => release_check(&config).await,
        Command::Devices => {
            let client = SignalClient::new(&config)?;
            let api = UuApi::new(client, config.at("state.json"))?;
            let body = api
                .device_list()
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{}", serde_json::to_string_pretty(&body).unwrap_or_default());
            Ok(())
        }
        Command::Control { device } => {
            let client = SignalClient::new(&config)?;
            let api = UuApi::new(client, config.at("state.json"))?;
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
                "geo_exit_ip": uu_core::config::geo_exit_ip(),
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
            let mut api = UuApi::new(client, config.at("state.json"))?;
            let dev = api.register(&name).await?;
            println!("registered device {} ({})", dev.alias, dev.device_id);
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
            let saved = uu_core::ClientState::load(&config.at("state.json"))?;
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
            let geo = uu_core::config::geo_exit_ip();
            let uu_core::controller::ControllerHandle { events, frames, input, close, settings: _, stats: _, stopped: _, native_input:_ } =
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
            let geo = uu_core::config::geo_exit_ip();
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
            let geo = uu_core::config::geo_exit_ip();
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

fn probe(config: &Config, h264: Option<&std::path::Path>, frame: Option<&std::path::Path>) -> Result<()> {
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

    println!("video decoder     {}", uu_media::decoder_backend_name());
    if let Some(path) = h264 {
        println!();
        decode_selftest(path, frame)?;
    }

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

/// Decode an Annex-B H.264 file with the decoder this machine selects.
///
/// This exists because the hardware path is otherwise only reachable through a
/// live session: it reports whether a machine really gets a hardware decoder,
/// how many frames the file produced, and how fast — which is also how the
/// software and hardware paths get compared on the same input.
fn decode_selftest(path: &std::path::Path, output: Option<&std::path::Path>) -> Result<()> {
    let data = std::fs::read(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let units = split_access_units(&data);
    if units.is_empty() {
        anyhow::bail!("{} holds no H.264 NAL units", path.display());
    }

    let mut decoder = uu_media::default_decoder()?;
    println!("decoder           {}", decoder.backend_name());
    println!(
        "input             {} ({} bytes, {} access units)",
        path.display(),
        data.len(),
        units.len()
    );

    let mut decoded = 0u64;
    let mut errors = 0u64;
    let mut shape = None;
    let mut first_error = None;
    let started = std::time::Instant::now();
    for (index, unit) in units.iter().enumerate() {
        let keyframe = uu_media::EncodedFrame::detect_h264_keyframe(unit);
        let frame = uu_media::EncodedFrame {
            is_keyframe: keyframe,
            data: bytes::Bytes::from(unit.clone()),
            pts: uu_media::Timestamp(index as u64 * 33_333),
        };
        match decoder.decode(&frame) {
            Ok(Some(decoded_frame)) => {
                anyhow::ensure!(decoded_frame.is_consistent(), "decoder returned a truncated frame");
                decoded += 1;
                shape = Some(decoded_frame.format);
                if decoded <= 3 {
                    println!(
                        "  au {:>4}  {}-> {:?} {}x{} stride {}",
                        index,
                        if keyframe { "key  " } else { "delta" },
                        decoded_frame.format,
                        decoded_frame.width,
                        decoded_frame.height,
                        decoded_frame.stride,
                    );
                }
                if decoded == 1 {
                    if let Some(output) = output {
                        write_probe_frame(output, &decoded_frame, uu_media::color::from_h264(&data).unwrap_or_default())?;
                        println!("first frame       {}", output.display());
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                errors += 1;
                first_error.get_or_insert_with(|| e.to_string());
            }
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    println!(
        "decoded           {decoded} frames in {:.0} ms ({:.1} fps){}",
        elapsed * 1000.0,
        decoded as f64 / elapsed.max(1e-9),
        shape.map(|f| format!(", {f:?}")).unwrap_or_default()
    );
    if errors > 0 {
        println!(
            "decode errors     {errors}{}",
            first_error
                .map(|e| format!(" (first: {e})"))
                .unwrap_or_default()
        );
    }
    // Report the backend again: a hardware decoder that failed mid-file
    // demotes itself, and that shows up only in the name.
    println!("decoder after     {}", decoder.backend_name());
    anyhow::ensure!(decoded > 0, "decoder produced no frames");
    anyhow::ensure!(errors == 0, "decoder reported {errors} errors");
    Ok(())
}

fn write_probe_frame(path: &std::path::Path, frame: &uu_media::RawFrame, color:uu_media::color::YuvColor) -> Result<()> {
    use std::io::Write;
    let rgba = uu_media::convert::frame_to_rgba_with_color(frame,color);
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)
        .with_context(|| format!("creating {}", path.display()))?);
    write!(file, "P6\n{} {}\n255\n", frame.width, frame.height)?;
    let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| p[..3].iter().copied()).collect();
    file.write_all(&rgb)?;
    file.flush()?;
    Ok(())
}

/// Split an Annex-B H.264 byte stream into access units.
///
/// An access unit ends when the next picture's data starts. Both markers
/// matter: a slice whose `first_mb_in_slice` is zero begins a picture, and the
/// leading non-VCL NALs (AUD, SPS, PPS, SEI) that follow a picture belong to
/// the *next* access unit, not to the one they came after.
fn split_access_units(data: &[u8]) -> Vec<Vec<u8>> {
    let mut units = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    let mut has_slice = false;
    for nal in annex_b_nals(data) {
        let kind = nal.first().copied().unwrap_or(0) & 0x1f;
        let is_slice = (1..=5).contains(&kind);
        let starts_picture = is_slice && first_mb_in_slice(nal) == Some(0);
        if has_slice && (starts_picture || !is_slice) {
            units.push(std::mem::take(&mut current));
            has_slice = false;
        }
        current.extend_from_slice(&[0, 0, 0, 1]);
        current.extend_from_slice(nal);
        has_slice |= is_slice;
    }
    if !current.is_empty() {
        units.push(current);
    }
    units
}

/// NAL units of an Annex-B stream (start codes stripped).
fn annex_b_nals(data: &[u8]) -> Vec<&[u8]> {
    // (start of the start code, first byte after it)
    let mut marks: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i + 3 <= data.len() {
        let (length, body) = if data[i..].starts_with(&[0, 0, 0, 1]) {
            (4usize, i + 4)
        } else if data[i..].starts_with(&[0, 0, 1]) {
            (3usize, i + 3)
        } else {
            i += 1;
            continue;
        };
        let _ = length;
        marks.push((i, body));
        i = body;
    }
    marks
        .iter()
        .enumerate()
        .filter_map(|(index, (_, body))| {
            // A NAL ends where the *next* start code begins, whatever its
            // length: guessing three bytes here silently appends padding.
            let end = marks
                .get(index + 1)
                .map(|(start, _)| *start)
                .unwrap_or(data.len());
            data.get(*body..end).filter(|nal| !nal.is_empty())
        })
        .collect()
}

/// The first `ue(v)` of a slice header: `first_mb_in_slice`.
fn first_mb_in_slice(nal: &[u8]) -> Option<u32> {
    let mut bit = 0usize;
    let mut zeros = 0u32;
    loop {
        let byte = *nal.get(1 + bit / 8)?;
        let one = (byte >> (7 - (bit % 8))) & 1;
        bit += 1;
        if one == 1 {
            break;
        }
        zeros += 1;
        if zeros > 31 {
            return None;
        }
    }
    let mut value = 0u32;
    for _ in 0..zeros {
        let byte = *nal.get(1 + bit / 8)?;
        value = (value << 1) | u32::from((byte >> (7 - (bit % 8))) & 1);
        bit += 1;
    }
    Some(((1u32 << zeros) - 1) + value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an Annex-B stream from NAL payloads.
    fn stream_of(nals: &[&[u8]]) -> Vec<u8> {
        let mut stream = Vec::new();
        for nal in nals {
            stream.extend_from_slice(&[0, 0, 0, 1]);
            stream.extend_from_slice(nal);
        }
        stream
    }

    fn kinds(unit: &[u8]) -> Vec<u8> {
        annex_b_nals(unit)
            .iter()
            .map(|nal| nal[0] & 0x1f)
            .collect()
    }

    #[test]
    fn parameter_sets_lead_the_picture_they_belong_to() {
        // This is the layout UU sends: every access unit is AUD, SPS, PPS and
        // one slice, so the trailing parameter sets of a keyframe must not be
        // left behind with the previous picture.
        let aud = [0x09u8, 0x10];
        let sps = [0x67u8, 0x42, 0x00];
        let pps = [0x68u8, 0xce, 0x38];
        let idr = [0x65u8, 0x88, 0x84];
        let p = [0x41u8, 0x9a, 0x11];
        let stream = stream_of(&[&aud, &sps, &pps, &idr, &aud, &sps, &pps, &idr, &aud, &p]);
        let units = split_access_units(&stream);
        assert_eq!(units.len(), 3);
        assert_eq!(kinds(&units[0]), vec![9, 7, 8, 5]);
        assert_eq!(kinds(&units[1]), vec![9, 7, 8, 5]);
        assert_eq!(kinds(&units[2]), vec![9, 1]);
    }

    #[test]
    fn multi_slice_pictures_stay_in_one_unit() {
        let aud = [0x09u8, 0x10];
        let first = [0x65u8, 0x88, 0x84]; // first_mb_in_slice = 0
        let second = [0x65u8, 0x41, 0x11]; // first_mb_in_slice = 1
        let stream = stream_of(&[&aud, &first, &second, &aud, &first]);
        let units = split_access_units(&stream);
        assert_eq!(units.len(), 2);
        assert_eq!(kinds(&units[0]), vec![9, 5, 5]);
    }

    #[test]
    fn first_mb_in_slice_reads_the_leading_ue_v() {
        // 0x88 = 1 0001000: ue(v) "1" then "0001000" -> 0.
        assert_eq!(first_mb_in_slice(&[0x65, 0x88]), Some(0));
        // 0x9a starts with a 1 bit, so it is the shortest ue(v) too.
        assert_eq!(first_mb_in_slice(&[0x41, 0x9a]), Some(0));
        // 0x41 = 0100 0001: ue(v) "01" + suffix "0" -> 1, i.e. a second slice.
        assert_eq!(first_mb_in_slice(&[0x41, 0x41]), Some(1));
    }
}
