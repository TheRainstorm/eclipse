//! Native remote streaming, device authorization and optional runtime extensions.
//!
//! Both roles live in one binary:
//! * `uu native-host`  — expose an explicitly selected source
//! * `uu plugin`       — invoke an installed optional extension
//!
//! The protocol work is in `uu-proto`/`uu-core`; the platform work is in
//! `uu-capture`/`uu-input`.

mod native_cli;
mod lan_cli;
mod internet_cli;
#[cfg(windows)]
mod service_cli;

use anyhow::{Context, Result};
use std::io::IsTerminal;
use clap::{Parser, Subcommand};
use uu_core::{Config, Role, Session, SessionState};

#[derive(clap::ValueEnum,Clone,Copy,Debug)]
enum NativeEncoder { Software, Auto, Hardware, Vaapi }
impl NativeEncoder {
    fn preference(self)->uu_core::native::EncoderPreference {
        use uu_core::native::EncoderPreference as P;
        match self {Self::Software=>P::Software,Self::Auto=>P::Auto,Self::Hardware=>P::Hardware,Self::Vaapi=>P::Vaapi}
    }
}

#[derive(clap::ValueEnum,Clone,Copy,Debug)]
enum NativeCodec {H264,#[value(alias="h265")] Hevc,Av1,Pyrowave}
impl NativeCodec {
    fn codec(self)->uu_core::native::VideoCodec {
        use uu_core::native::VideoCodec as V;
        match self{Self::H264=>V::H264,Self::Hevc=>V::Hevc,Self::Av1=>V::Av1,Self::Pyrowave=>V::Pyrowave}
    }
}
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
        let codec=self.codec.codec();
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
    fn native_host_quality_negotiation_remains_explicit() {
        let base=["uu","native-host","--invitation-file","test.eclipse-invite.json"];
        assert!(matches!(Cli::try_parse_from(base).unwrap().command,Command::NativeHost{client_quality:false,..}));
        let mut args=base.to_vec();args.push("--client-quality");
        assert!(matches!(Cli::try_parse_from(args).unwrap().command,Command::NativeHost{client_quality:true,..}));
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
    name = "eclipse-cli",
    version,
    about = "Native remote streaming client (Windows/Linux)",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Install or manage the Windows background host for lock-screen streaming.
    #[cfg(windows)]
    Service {
        #[command(subcommand)]
        command: service_cli::Command,
    },
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
    /// List native capture monitors without reading desktop pixels.
    NativeSources {
        /// Machine-readable physical output and color inventory (no desktop pixels).
        #[arg(long)]
        json: bool,
    },
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
        /// Explicitly accept client quality requests, restricted to --codec.
        #[arg(long)]
        client_quality: bool,
    },
    /// Decode native video without saving pictures or opening a GUI.
    NativeProbe {
        /// Explicit native codec selection; PyroWave requires the pinned runtime on both peers.
        #[arg(long,value_enum)]
        codec:Option<NativeCodec>,
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
    /// List installed extensions or explicitly enable/disable one.
    Plugins {
        id:Option<String>,
        #[arg(long,action=clap::ArgAction::Set,requires="id")]
        enabled:Option<bool>,
    },
    /// Invoke an installed, enabled extension's own command interface.
    Plugin {
        id:String,
        #[arg(trailing_var_arg=true,allow_hyphen_values=true)]
        arguments:Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    #[cfg(windows)]
    let background=matches!(&cli.command,Command::Service{command:service_cli::Command::Daemon|service_cli::Command::SessionHost{..}});
    #[cfg(not(windows))]
    let background=false;
    #[cfg(windows)]
    if background {if let Err(error)=service_cli::init_background_logging(){service_cli::record_bootstrap_error(&error);return Err(error);}}
    if !background {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .with_ansi(std::io::stdout().is_terminal())
        .init();
    }

    uu_core::install_crypto_provider();
    #[cfg(windows)]
    let config = if background {uu_core::service_admin::windows::Paths::discover()?.config()}else{Config::from_env()?};
    #[cfg(not(windows))]
    let config = Config::from_env()?;
    config.ensure_dirs().await?;
    #[cfg(windows)]
    if matches!(&cli.command,Command::Service{..}) {
        let Command::Service{command}=cli.command else{unreachable!()};
        return service_cli::run(command,config).await;
    }

    let extensions=uu_plugin_sdk::runtime::shared(&config);
    match cli.command {
        #[cfg(windows)]
        Command::Service{command}=>service_cli::run(command,config).await,
        Command::Internet{command}=>internet_cli::run(command,config).await,
        Command::Lan {command} => lan_cli::run(command,config.at("native-lan")).await,
        Command::Probe { h264, frame } => probe(&config, h264.as_deref(), frame.as_deref()),
        Command::NativeSources {json} => native_cli::sources(json),
        Command::NativeHost { video,bind, invitation_file, monitor, all_monitors, synthetic, synthetic_width, synthetic_height, allow_input, audio, synthetic_audio, fps, bitrate_kbps, encoder, pairing_seconds, max_seconds, client_quality } => {
            let source=if all_monitors {uu_capture::CaptureSource::Desktop{initial_monitor:None}}else if synthetic {uu_capture::CaptureSource::Synthetic{width:synthetic_width,height:synthetic_height,fps}}else if let Some(name)=monitor {uu_capture::CaptureSource::Monitor(name)}else{uu_capture::CaptureSource::Primary};
            let (codec,rate_control)=video.policy(bitrate_kbps)?;
            native_cli::host(uu_core::native::HostConfig{encoder_tuning:Default::default(),session:None,video_policy:client_quality.then(||uu_core::native_quality::Policy{codecs:vec![codec],preferred_codec:Some(codec)}),codec,rate_control,bind,source,invitation_file,invitation_lifetime:std::time::Duration::from_secs(pairing_seconds),fps,bitrate_kbps,encoder:encoder.preference(),session_limit:max_seconds.map(std::time::Duration::from_secs),allow_input,clipboard:None,audio:if audio{Some(uu_core::native::AudioSource::System)}else if synthetic_audio{Some(uu_core::native::AudioSource::Synthetic)}else{None}}).await
        },
        Command::NativeProbe {codec,invitation_file,frames,timeout_seconds,report,audio_loss_every,progress_seconds,max_frame_gap_seconds} => native_cli::probe(invitation_file,frames,timeout_seconds,report,audio_loss_every.unwrap_or(0),progress_seconds,max_frame_gap_seconds,codec.map(NativeCodec::codec)).await,
        Command::Plugins{id,enabled}=> {
            let preferences=uu_core::plugins::Preferences::load(&config)?;
            let mut runtime=extensions.lock().unwrap();
            if let Some(id)=id {
                if let Some(enabled)=enabled {let expected=preferences.get(&id);let mut value=expected.clone();value.enabled=enabled;runtime.save(&config,&id,&expected,value)?;}
            }
            for (id,record) in &runtime.records {println!("{id}: {} v{}, enabled={}, compatible={}{}",record.manifest.name,record.manifest.version,record.enabled,record.compatible,if record.error.is_empty(){String::new()}else{format!(", error={}",record.error)});}
            for error in &runtime.errors {eprintln!("{error}");}
            Ok(())
        },
        Command::Plugin{id,arguments}=> {
            let loaded=extensions.lock().unwrap().get(&id)?;
            tokio::task::spawn_blocking(move||loaded.plugin.cli(arguments)).await??;Ok(())
        },
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
    println!("plugin SDK        {}", uu_plugin_sdk::SDK_ID);
    Ok(())
}

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
        // An access unit can contain AUD, SPS, PPS and
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
