//! Account-independent LAN diagnostics. Pairing always needs host confirmation.
use anyhow::{ensure, Context, Result};
use clap::Subcommand;
use std::{io::BufRead, net::SocketAddr, path::PathBuf, time::Duration};
use tokio::sync::{mpsc, watch};
use uu_core::{
    native, native_discovery as discovery, native_lan as lan, native_lan_store::Devices,
};

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Query public device metadata; does not pair or start capture.
    Discover {
        #[arg(long, default_value = "255.255.255.255:48999")]
        target: Vec<SocketAddr>,
    },
    /// Check one IP/hostname and print public metadata only.
    Inspect { endpoint: String },
    /// Pair with a host; enter the displayed comparison code on that host.
    Pair { endpoint: String },
    /// List this client's saved devices, without credentials.
    Devices,
    /// Forget a local pairing; revoke its grant separately on the host.
    Forget { id: String },
    /// Decode generated/authorized remote video without playback or input.
    Probe {
        id: String,
        #[arg(long, default_value_t = 120)]
        frames: u64,
        #[arg(long, default_value_t = 15)]
        timeout_seconds: u64,
        #[arg(long)]
        report: Option<PathBuf>,
    },
    /// Explicit foreground server; stdin commands confirm pairing and stop it.
    Host {
        #[command(flatten)]
        video:super::NativeVideoOptions,
        #[arg(long, default_value = "127.0.0.1:48998")]
        bind: SocketAddr,
        /// Enable UDP discovery on this explicit local address.
        #[arg(long)]
        discovery_bind: Option<SocketAddr>,
        #[arg(long, default_value = "Eclipse server")]
        name: String,
        /// Use generated pixels only, without desktop capture or input.
        #[arg(long)]
        synthetic: bool,
        #[arg(long,requires="synthetic",default_value_t=1280,value_parser=super::synthetic_dimension)]
        synthetic_width: u32,
        #[arg(long,requires="synthetic",default_value_t=720,value_parser=super::synthetic_dimension)]
        synthetic_height: u32,
        #[arg(long, conflicts_with = "synthetic")]
        monitor: Option<String>,
        #[arg(long, conflicts_with = "synthetic")]
        allow_input: bool,
        #[arg(long, conflicts_with = "synthetic")]
        audio: bool,
        #[arg(long, default_value_t = 60)]
        fps: u32,
        #[arg(long, default_value_t = 8000)]
        bitrate_kbps: u32,
        #[arg(long, value_enum, default_value = "software")]
        encoder: super::NativeEncoder,
    },
}
pub async fn run(command: Command, dir: PathBuf) -> Result<()> {
    let path = dir.join("paired-devices.json");
    match command {
        Command::Discover { target } => {
            for device in discovery::discover(&target, Duration::from_millis(1200)).await? {
                println!(
                    "{}",
                    serde_json::json!({"id":device.info.id,"name":device.info.name,"address":device.address.to_string(),"fingerprint":device.info.fingerprint})
                );
            }
        }
        Command::Inspect { endpoint } => {
            let device = lan::inspect(&endpoint).await?;
            println!(
                "{}",
                serde_json::json!({"id":device.info.id,"name":device.info.name,"address":device.address.to_string(),"fingerprint":device.info.fingerprint,"pairing_open":device.pairing_open})
            );
        }
        Command::Devices => {
            let book = Devices::open(&path)?;
            for device in &book.devices {
                println!(
                    "{}",
                    serde_json::json!({"id":device.id,"name":device.name,"address":device.endpoint,"fingerprint":device.fingerprint()})
                );
            }
        }
        Command::Forget { id } => Devices::open(&path)?.forget(&path, &id)?,
        Command::Pair { endpoint } => {
            let mut book = Devices::open(&path)?;
            let (progress, mut codes) = mpsc::unbounded_channel();
            let client_id = book.client_id.clone();
            let pairing = lan::pair(&endpoint, &client_id, "Eclipse CLI", progress);
            tokio::pin!(pairing);
            let device = loop {
                tokio::select! {
                    _=tokio::signal::ctrl_c()=>anyhow::bail!("pairing cancelled"),
                    Some(code)=codes.recv()=>println!("Enter this code on the host to approve pairing: {code}"),
                    result=&mut pairing=>break result?,
                }
            };
            let id = device.id.clone();
            book.remember(&path, device)?;
            println!("paired {id}");
        }
        Command::Probe {
            id,
            frames,
            timeout_seconds,
            report,
        } => {
            ensure!(
                (1..=100_000).contains(&frames) && (1..=3600).contains(&timeout_seconds),
                "invalid probe limits"
            );
            let book = Devices::open(&path)?;
            let device = book.paired(&id).context("device is not paired")?;
            let invitation = lan::connect(device, &book.client_id).await?;
            super::native_cli::probe_invitation(
                invitation,
                frames,
                timeout_seconds,
                report,
                0,
                None,
                None,
            )
            .await?;
        }
        Command::Host {
            video,
            bind,
            discovery_bind,
            name,
            synthetic,
            synthetic_width,
            synthetic_height,
            monitor,
            allow_input,
            audio,
            fps,
            bitrate_kbps,
            encoder,
        } => {
            let (codec,rate_control)=video.policy(bitrate_kbps)?;
            let source = if synthetic {
                native::CaptureSource::Synthetic {
                    width: synthetic_width,
                    height: synthetic_height,
                    fps,
                }
            } else if let Some(monitor) = monitor {
                native::CaptureSource::Monitor(monitor)
            } else {
                native::CaptureSource::Primary
            };
            host(lan::Config {encoder_tuning:Default::default(),video_policy:None,codec,rate_control,
                name,
                bind,
                discovery_bind,
                data_dir: dir,
                source,
                fps,
                bitrate_kbps,
                encoder: encoder.preference(),
                allow_input,
                audio: audio.then_some(native::AudioSource::System),
                clipboard: None,
            })
            .await?;
        }
    }
    Ok(())
}
async fn host(config: lan::Config) -> Result<()> {
    let (stop, rx) = watch::channel(false);
    let (events, mut incoming) = mpsc::unbounded_channel();
    let (server, handle) = lan::Server::bind(config, rx, events).await?;
    println!("LAN server listening on {}", server.address()?);
    println!("Commands: pair | code <8 digits from client> | deny | close-pairing | revoke <client-id> | disconnect | stop");
    let (lines, mut input) = mpsc::unbounded_channel();
    // A regular foreground stdin reader cannot keep the async runtime alive on
    // shutdown; it never reads anything outside this command's input stream.
    std::thread::Builder::new()
        .name("eclipse-console".into())
        .spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if lines.send(line).is_err() {
                    break;
                }
            }
        })?;
    let serving = tokio::spawn(server.run());
    tokio::pin!(serving);
    let mut pending: Option<lan::PairingRequest> = None;
    let result = loop {
        tokio::select! {
            _=tokio::signal::ctrl_c()=>break Ok(()),
            result=&mut serving=>return result.context("LAN server task failed")?,
            event=incoming.recv()=>match event {
                Some(lan::Event::Pairing(request))=>{
                    println!("Pairing requested by {:?}; read the client code and enter: code <8 digits>",request.client_name);
                    pending=Some(request);
                }
                Some(lan::Event::PairingEnded(id))=>{if pending.as_ref().is_some_and(|p|p.id==id){pending=None;println!("Pairing request ended");}}
                Some(lan::Event::Warning(message))=>eprintln!("LAN warning: {message}"),
                Some(lan::Event::SessionStarted { .. })=>println!("Native session started"),
                Some(lan::Event::SessionProgress { status, .. })=>{
                    if let native::HostStatus::Streaming { width,height,fps,audio,encoder }=status {
                        println!("Native streaming: {width}x{height} @ {fps} FPS; encoder={encoder}; audio={audio}");
                    }
                },
                Some(lan::Event::SessionEnded(_))=>println!("Native session ended"),
                None=>break Ok(()),
            },
            line=input.recv()=>{
                let Some(line)=line else {break Ok(())};
                let (command,value)=line.trim().split_once(' ').unwrap_or((line.trim(),""));
                match command {
                    "pair"=>{handle.allow_pairing();println!("New pairing requests enabled");}
                    "close-pairing"=>handle.close_pairing(),
                    "code"=>{
                        let code:String=value.chars().filter(|c|c.is_ascii_digit()).collect();
                        if pending.as_ref().is_some_and(|p|code.len()==8&&p.code.replace(' ',"")==code){
                            let _=pending.take().unwrap().decision.send(true);
                        }else{eprintln!("Pairing code does not match; no authorization granted");}
                    }
                    "deny"=>{if let Some(request)=pending.take(){let _=request.decision.send(false);}}
                    "disconnect"=>{handle.stop_session();println!("Disconnect requested; server remains available");}
                    "revoke"=>{match handle.revoke(value.trim()){Ok(())=>println!("Device authorization revoked"),Err(error)=>eprintln!("Cannot revoke device: {error}")}}
                    "stop"=>break Ok(()),
                    _=>eprintln!("Unknown command"),
                }
            }
        }
    };
    stop.send_replace(true);
    drop(pending);
    match tokio::time::timeout(Duration::from_secs(5), &mut serving).await {
        Ok(joined) => joined.context("LAN server task failed")??,
        Err(_) => {
            serving.abort();
            let _ = serving.await;
            anyhow::bail!("LAN shutdown timed out");
        }
    }
    result
}
#[cfg(test)]
mod tests {
    use clap::Parser;
    #[test]
    fn generated_host_cannot_enable_desktop_input_and_addresses_are_explicit() {
        let parsed = super::super::Cli::try_parse_from([
            "uu",
            "lan",
            "host",
            "--synthetic",
            "--allow-input",
        ]);
        assert!(parsed.is_err());
        assert!(
            super::super::Cli::try_parse_from(["uu", "lan", "inspect", "example.invalid"]).is_ok()
        );
        assert!(super::super::Cli::try_parse_from(["uu", "lan", "host", "--synthetic"]).is_ok());
    }
}
