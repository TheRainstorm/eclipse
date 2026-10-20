//! QR login flow for the GUI.

use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};

use anyhow::{Context, Result};
use uu_core::{Config, SignalClient, UuApi};

pub enum LoginEvent {
    Qr(String),
    Status(String),
    Done {
        token: String,
        user_id: Option<String>,
    },
    Error(String),
}

pub struct Worker {
    events: Receiver<LoginEvent>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    pub fn try_recv(&self) -> std::result::Result<LoginEvent, TryRecvError> {
        self.events.try_recv()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn spawn(config: Config) -> Worker {
    spawn_task(move |tx| async move { run(config, &tx).await })
}

fn spawn_task<F: std::future::Future<Output = Result<()>> + 'static>(
    task: impl FnOnce(Sender<LoginEvent>) -> F + Send + 'static,
) -> Worker {
    let (tx, rx) = channel();
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let thread = std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                let _ = tx.send(LoginEvent::Error(e.to_string()));
                return;
            }
        };
        rt.block_on(async {
            tokio::select! {
                biased;
                _ = cancelled => {},
                result = task(tx.clone()) => {
                    if let Err(error) = result { let _ = tx.send(LoginEvent::Error(error.to_string())); }
                }
            }
        });
        // DNS/background tasks must not hold the UI's cancellation join open.
        rt.shutdown_background();
    });
    Worker {
        events: rx,
        cancel: Some(cancel),
        thread: Some(thread),
    }
}

async fn run(config: Config, tx: &std::sync::mpsc::Sender<LoginEvent>) -> Result<()> {
    let client = SignalClient::new(&config)?;
    let mut api = UuApi::new(client, config.at("state.json"))?;
    if !api.has_identity() {
        let _ = tx.send(LoginEvent::Status("注册设备…".into()));
        let dev = api.register("uu-rs-gui").await?;
        let _ = dev;
    }
    loop {
        let _ = tx.send(LoginEvent::Status("获取二维码…".into()));
        let qr = api.gen_qr_login().await?;
        anyhow::ensure!(
            !qr.jump_url.is_empty() && !qr.token.is_empty(),
            "服务未返回有效登录二维码"
        );
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
                            anyhow::ensure!(!tok.is_empty(), "登录响应缺少凭据");
                            let uid = v["data"]["user_id"]
                                .as_str()
                                .map(str::to_string)
                                .or_else(|| v["data"]["user_id"].as_u64().map(|id| id.to_string()));
                            // Only the active UI receiver may persist credentials. A cancelled
                            // or replaced QR flow can never sign the user back in later.
                            let _ = tx.send(LoginEvent::Done {
                                token: tok.into(),
                                user_id: uid,
                            });
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[test]
    fn cancelling_qr_worker_drops_inflight_poll_before_returning() {
        struct Cancelled(Arc<AtomicBool>);
        impl Drop for Cancelled {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let worker = spawn_task(move |tx| async move {
            let _guard = Cancelled(flag);
            tx.send(LoginEvent::Status("poll started".into())).unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            panic!("cancelled login must never finish");
        });
        assert!(matches!(
            worker
                .events
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            LoginEvent::Status(_)
        ));
        let started = std::time::Instant::now();
        drop(worker);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert!(cancelled.load(Ordering::SeqCst));
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
