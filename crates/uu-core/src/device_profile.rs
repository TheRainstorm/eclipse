//! Resolve a saved device route once; every route shares device trust and the
//! existing media controller, without placing credentials in child arguments.
use crate::{
    device_book::{Destination, Launch, Store},
    device_trust::TrustStore,
    internet::{Provider, Settings},
    media::{FrameSender, MediaStats},
    Config,
};
use anyhow::{ensure, Context, Result};
use std::sync::Arc;
use tokio::sync::watch;

async fn cancelled(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow_and_update() {
            return;
        }
        if stop.changed().await.is_err() {
            return;
        }
    }
}
pub async fn view(
    launch: Launch,
    config: Config,
    frames: FrameSender,
    stats: Arc<MediaStats>,
    mut stop: watch::Receiver<bool>,
    input: Option<crate::native_input::NativeInputReceiver>,
) -> Result<()> {
    let store = TrustStore::open(config.at("device-trust"))?;
    let profiles = Store::open(config.at("device-trust"))?;
    let name = profiles.read()?.sharing.name;
    let settings = match &launch.route.destination {
        Destination::Ip {
            address,
            legacy_device,
        } => {
            let legacy = legacy_device
                .as_ref()
                .map(|id| {
                    let book = crate::native_lan_store::Devices::open(
                        &config.at("native-lan/paired-devices.json"),
                    )?;
                    let device = book
                        .paired(id)
                        .context("原有 IP 配对已经移除，请重新配对")?
                        .clone();
                    Ok::<_, anyhow::Error>((device, book.client_id))
                })
                .transpose()?;
            let connection = tokio::select! {
                result=crate::native_lan::connect_device(address,&store,&name,launch.expected_peer.as_deref(),
                    legacy.as_ref().map(|(d,id)|(d,id.as_str())),true,&stats.pairing)=>result?,
                _=cancelled(&mut stop)=>return Ok(()),
            };
            if let Some(peer) = connection.peer {
                profiles.bind(&launch.profile, &peer.id())?;
            }
            let invitation = connection.invitation.context("被控端未签发串流许可")?;
            return if let Some(input) = input {
                crate::native::view_with_input(&invitation, frames, stats, stop, input).await
            } else {
                crate::native::view(&invitation, frames, stats, stop).await
            };
        }
        Destination::Uu { device } => Settings {
            provider: Provider::Uu,
            device: device.clone(),
            device_auth: true,
            expected_peer: launch.expected_peer.clone(),
            profile: Some(launch.profile.clone()),
            ..Default::default()
        },
        Destination::Vps { service, device } => {
            let server = launch
                .service
                .as_ref()
                .filter(|s| s.id == *service)
                .context("缺少服务器配置快照")?;
            Settings {
                provider: Provider::SelfHosted,
                server: server.address.clone(),
                device: device.clone(),
                server_token: server.token_file.clone(),
                server_ca: server.certificate.clone(),
                device_auth: true,
                expected_peer: launch.expected_peer.clone(),
                profile: Some(launch.profile.clone()),
                ..Default::default()
            }
        }
        Destination::LegacyInternet { settings } => {
            ensure!(
                launch.expected_peer.is_none(),
                "已升级公钥配对的设备不能降级使用旧共享密钥"
            );
            ensure!(
                std::fs::metadata(settings)?.len() <= 64 * 1024,
                "旧连接配置过大"
            );
            serde_json::from_slice(&std::fs::read(settings)?)
                .map_err(|_| anyhow::anyhow!("旧连接配置格式错误，原文件已保留"))?
        }
    };
    crate::internet::view(settings, config, frames, stats, stop, input).await
}
