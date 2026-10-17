//! QR login flow for the GUI.

use std::sync::mpsc::{channel, Receiver};

use anyhow::{Context, Result};
use uu_core::{Config, SignalClient, UuApi};

pub enum LoginEvent {
    Qr(String),
    Status(String),
    Done,
    Error(String),
}

pub fn spawn(config: Config) -> Receiver<LoginEvent> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                let _ = tx.send(LoginEvent::Error(e.to_string()));
                return;
            }
        };
        if let Err(e) = rt.block_on(run(config, &tx)) {
            let _ = tx.send(LoginEvent::Error(e.to_string()));
        }
    });
    rx
}

async fn run(config: Config, tx: &std::sync::mpsc::Sender<LoginEvent>) -> Result<()> {
    let client = SignalClient::new(&config)?;
    let mut api = UuApi::new(client, config.at("state.json"));
    if !api.has_identity() {
        let _ = tx.send(LoginEvent::Status("注册设备…".into()));
        let dev = api.register("uu-rs-gui").await?;
        let _ = dev;
    }
    loop {
        let _ = tx.send(LoginEvent::Status("获取二维码…".into()));
        let qr = api.gen_qr_login().await?;
        let _ = tx.send(LoginEvent::Qr(qr.jump_url.clone()));
        let mut status = 1i32;
        let mut last = None;
        loop {
            let full = api.poll_qr_login(&qr, status).await?;
            let body = &full.body;
            let data = &body["data"];
            let state = data["login_status"]
                .as_i64()
                .or_else(|| data["status"].as_i64())
                .unwrap_or(0);
            if Some(state) != last {
                let msg = match state {
                    2 => "已扫码，等待确认…",
                    3 => "已确认，正在登录…",
                    _ => "等待扫码…",
                };
                let _ = tx.send(LoginEvent::Status(msg.into()));
                last = Some(state);
            }
            if state == 4 {
                match api.exchange_qr(&qr).await {
                    Ok(v) => {
                        if let Some(tok) = v["data"]["token"].as_str() {
                            let uid = v["data"]["user_id"].as_str().map(str::to_string);
                            api.store_token(tok, uid)?;
                            let _ = tx.send(LoginEvent::Done);
                            return Ok(());
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(LoginEvent::Status(format!("登录失败: {e}")));
                    }
                }
                break;
            }
            if state > status as i64 {
                status = state as i32;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }
}

/// Render a QR code as RGBA pixels (with a 4-module quiet zone).
pub fn qr_rgba(data: &str, scale: usize) -> Result<(usize, usize, Vec<u8>)> {
    let code = qrcode::QrCode::new(data.as_bytes()).context("encoding QR")?;
    let n = code.width();
    let quiet = 4usize;
    let size = (n + quiet * 2) * scale;
    let mut rgba = vec![255u8; size * size * 4];
    let colors = code.to_colors();
    for y in 0..n {
        for x in 0..n {
            if colors[y * n + x] == qrcode::Color::Dark {
                let px = (x + quiet) * scale;
                let py = (y + quiet) * scale;
                for dy in 0..scale {
                    for dx in 0..scale {
                        let o = ((py + dy) * size + px + dx) * 4;
                        rgba[o] = 0;
                        rgba[o + 1] = 0;
                        rgba[o + 2] = 0;
                        rgba[o + 3] = 255;
                    }
                }
            }
        }
    }
    Ok((size, size, rgba))
}
