//! Standalone self-hosted rendezvous process, without desktop access.
use anyhow::Result;
use clap::Parser;
use std::{net::SocketAddr, path::PathBuf};
use tokio::sync::watch;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8443")]
    bind: SocketAddr,
    #[arg(long)]
    certificate: PathBuf,
    #[arg(long)]
    private_key: PathBuf,
    #[arg(long)]
    token_file: PathBuf,
    #[arg(long)]
    stun: Vec<String>,
    #[arg(long)]
    stun_bind: Option<SocketAddr>,
}
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_ansi(false)
        .init();
    let args = Args::parse();
    let (stop, rx) = watch::channel(false);
    let service = uu_core::internet::server::serve(
        uu_core::internet::server::Config {
            bind: args.bind,
            certificate: args.certificate,
            private_key: args.private_key,
            token_file: args.token_file,
            stun: args.stun,
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
