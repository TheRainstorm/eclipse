//! Standalone self-hosted rendezvous process, without desktop access.
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::{net::SocketAddr, path::PathBuf};
use tokio::sync::watch;

#[path = "uu-signal/groups.rs"]
mod groups;

#[derive(Parser)]
#[command(subcommand_negates_reqs = true)]
struct Args {
    #[command(subcommand)]
    command: Option<Action>,
    #[arg(long, default_value = "127.0.0.1:8443")]
    bind: SocketAddr,
    #[arg(long, required = true)]
    certificate: Option<PathBuf>,
    #[arg(long, required = true)]
    private_key: Option<PathBuf>,
    #[arg(long, required = true)]
    groups_file: Option<PathBuf>,
    #[arg(long)]
    stun: Vec<String>,
    #[arg(long)]
    stun_bind: Option<SocketAddr>,
    #[arg(long, requires = "turn_secret_file")]
    turn_url: Vec<String>,
    #[arg(long, requires = "turn_url")]
    turn_secret_file: Option<PathBuf>,
    #[arg(long, default_value_t = 86400)]
    turn_ttl_seconds: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn group_invitation_does_not_require_the_server_private_key_or_listener() {
        let args = Args::try_parse_from([
            "eclipse-signal",
            "group",
            "create",
            "--file",
            "groups.json",
            "--name",
            "Generated service",
            "--url",
            "wss://service.example:9443/v1/signal",
        ])
        .unwrap();
        assert!(matches!(args.command, Some(Action::Group { .. })));
        assert!(args.private_key.is_none() && args.certificate.is_none());
    }
    #[test]
    fn turn_requires_matching_secret_and_urls() {
        let base = [
            "eclipse-signal",
            "--certificate",
            "cert",
            "--private-key",
            "key",
            "--groups-file",
            "groups.json",
        ];
        assert!(Args::try_parse_from(base).is_ok());
        assert!(Args::try_parse_from(
            base.into_iter()
                .chain(["--turn-url", "turn:service.example:3478"])
        )
        .is_err());
        assert!(Args::try_parse_from(base.into_iter().chain([
            "--turn-url",
            "turn:service.example:3478",
            "--turn-secret-file",
            "secret"
        ]))
        .is_ok());
    }
}
#[derive(Subcommand)]
enum Action {
    /// Manage isolated private device groups on this server.
    Group {
        #[command(subcommand)]
        command: groups::Command,
    },
}
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_ansi(false)
        .init();
    let args = Args::parse();
    if let Some(Action::Group { command }) = args.command {
        return groups::run(command);
    }
    let turn = if args.turn_url.is_empty() {
        None
    } else {
        Some(uu_core::internet::server::TurnConfig {
            urls: args.turn_url,
            secret_file: args
                .turn_secret_file
                .context("--turn-url requires --turn-secret-file")?,
            credential_lifetime: std::time::Duration::from_secs(args.turn_ttl_seconds),
        })
    };
    let (stop, rx) = watch::channel(false);
    let service = uu_core::internet::server::serve(
        uu_core::internet::server::Config {
            bind: args.bind,
            certificate: args.certificate.context("--certificate is required")?,
            private_key: args.private_key.context("--private-key is required")?,
            groups_file: args.groups_file.context("--groups-file is required")?,
            stun: args.stun,
            turn,
        },
        rx.clone(),
    );
    let stun = async {
        if let Some(bind) = args.stun_bind {
            uu_core::internet::stun::serve(bind, rx).await
        } else {
            std::future::pending::<Result<()>>().await
        }
    };
    tokio::pin!(service);
    tokio::pin!(stun);
    tokio::select! {
        result=&mut service=>{stop.send_replace(true);result},
        result=&mut stun=>{stop.send_replace(true);result},
        _=tokio::signal::ctrl_c()=>{stop.send_replace(true);service.await?;if args.stun_bind.is_some(){stun.await?;}Ok(())}
    }
}
