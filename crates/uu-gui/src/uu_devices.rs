//! Account registrations are separate from native device profiles and trust.
use anyhow::{ensure, Context, Result};
use eframe::egui;
use std::sync::mpsc::{channel, Receiver};
use uu_core::{api::AccountDevice, Config, SignalClient, UuApi};

enum Operation {
    List,
    Unbind(String),
}
enum Event {
    Listed(Vec<AccountDevice>),
    Unbound { id: String, current: bool },
    Error(String),
}

struct Worker {
    events: Receiver<Event>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Worker {
    fn start(config: Config, token: String, operation: Operation) -> Self {
        let (tx, events) = channel();
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let thread = std::thread::spawn(move || {
            let runtime = match tokio::runtime::Runtime::new() {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = tx.send(Event::Error(error.to_string()));
                    return;
                }
            };
            runtime.block_on(async {
                tokio::select! {
                    biased;
                    _ = cancelled => {},
                    result = tokio::time::timeout(std::time::Duration::from_secs(15), async {
                        let api = UuApi::new(SignalClient::new(&config)?, config.at("state.json"))?;
                        ensure!(api.state.token.as_deref() == Some(token.as_str()), "账号已变化，请重新打开设备管理");
                        let devices = api.account_devices().await?;
                        match operation {
                            Operation::List => Ok(Event::Listed(devices)),
                            Operation::Unbind(id) => {
                                let device = devices.iter().find(|d| d.device_id == id)
                                    .context("设备已不在本账号列表，请刷新")?;
                                api.unbind_account_device(&id).await?;
                                Ok(Event::Unbound { id, current: device.current })
                            }
                        }
                    }) => {
                        let result: Result<Event> = result.context("请求超时，请刷新设备列表确认状态").and_then(|r|r);
                        let _ = tx.send(result.unwrap_or_else(|error| Event::Error(error.to_string())));
                    }
                }
            });
            runtime.shutdown_background();
        });
        Self {
            events,
            cancel: Some(cancel),
            thread: Some(thread),
        }
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

#[derive(Default)]
pub(super) struct Manager {
    open: bool,
    account: Option<String>,
    worker: Option<Worker>,
    devices: Vec<AccountDevice>,
    loaded: bool,
    confirmation: Option<AccountDevice>,
    notice: String,
}
impl Manager {
    pub fn open(&mut self, config: Config, token: String) {
        *self = Self::default();
        self.open = true;
        self.account = Some(token.clone());
        self.worker = Some(Worker::start(config, token, Operation::List));
    }

    /// Some(true) means this client's registration was explicitly unbound.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        config: Option<&Config>,
        token: Option<&str>,
    ) -> Option<bool> {
        if self.account.as_deref() != token {
            *self = Self::default();
        }
        if !self.open {
            return None;
        }
        let mut unbound = None;
        if let Some(event) = self.worker.as_ref().and_then(|w| w.events.try_recv().ok()) {
            self.worker = None;
            match event {
                Event::Listed(devices) => {
                    self.devices = devices;
                    self.loaded = true;
                    self.notice.clear();
                }
                Event::Unbound { id, current } => {
                    self.devices.retain(|d| d.device_id != id);
                    self.notice = "已解除 UU 账号绑定；原生配对和授权保持不变。".into();
                    unbound = Some(current);
                    if current {
                        self.open = false;
                        return unbound;
                    }
                }
                Event::Error(error) => self.notice = format!("操作失败：{error}"),
            }
        }
        let mut open = self.open;
        let mut operation = None;
        egui::Window::new("管理 UU 设备").open(&mut open).default_width(580.).resizable(true)
            .show(ctx, |ui| {
                ui.label("这里显示当前 UU 账号绑定的设备，包含本客户端和官方客户端。");
                ui.weak("“解绑”移除账号绑定，不删除系统上的软件、本机注册身份或原生设备授权；重新登录后设备可能再次出现在账号中。");
                ui.add_space(10.);
                let busy = self.worker.is_some();
                ui.horizontal(|ui| {
                    if ui.add_enabled(!busy, egui::Button::new("刷新列表")).clicked() { operation = Some(Operation::List); }
                    if busy { ui.spinner(); }
                });
                if !self.notice.is_empty() { ui.label(&self.notice); }
                egui::ScrollArea::vertical().max_height(360.).show(ui, |ui| {
                    for device in &self.devices {
                        ui.push_id(&device.device_id, |ui| {
                            ui.separator();
                            ui.horizontal(|ui| {
                                ui.strong(&device.alias);
                                if device.current { ui.weak("本客户端"); }
                                if !device.status.is_empty() { ui.weak(&device.status); }
                                if ui.add_enabled(!busy, egui::Button::new("解绑")).clicked() {
                                    self.confirmation = Some(device.clone());
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.monospace(&device.device_id);
                                if ui.small_button("复制编号").clicked() { ui.ctx().copy_text(device.device_id.clone()); }
                            });
                        });
                    }
                    if self.loaded && self.devices.is_empty() && !busy { ui.weak("当前列表没有绑定设备。"); }
                });
                if let Some(device) = self.confirmation.clone() {
                    ui.separator();
                    ui.label(format!("确认解绑“{}”（{}）？", device.alias, device.device_id));
                    if device.current { ui.label("解绑本客户端会退出 UU 登录并停止本机共享。"); }
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!busy, egui::Button::new("确认解绑")).clicked() {
                            operation = Some(Operation::Unbind(device.device_id));
                            self.confirmation = None;
                        }
                        if ui.button("取消").clicked() { self.confirmation = None; }
                    });
                }
            });
        self.open = open;
        if !open {
            self.worker = None;
            self.confirmation = None;
        } else if let (Some(operation), Some(config), Some(token)) = (operation, config, token) {
            self.notice.clear();
            self.worker = Some(Worker::start(config.clone(), token.into(), operation));
        }
        if self.worker.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        unbound
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending_event(event: Event) -> Worker {
        let (tx, events) = channel();
        tx.send(event).unwrap();
        Worker {
            events,
            cancel: None,
            thread: None,
        }
    }

    #[test]
    fn switching_accounts_discards_device_rows_confirmation_and_pending_unbind_events() {
        let device = AccountDevice {
            device_id: "old-device".into(),
            alias: "Previous account".into(),
            status: String::new(),
            current: true,
        };
        let mut manager = Manager {
            open: true,
            account: Some("old-account".into()),
            worker: Some(pending_event(Event::Unbound {
                id: device.device_id.clone(),
                current: true,
            })),
            devices: vec![device.clone()],
            loaded: true,
            confirmation: Some(device),
            notice: String::new(),
        };
        assert_eq!(
            manager.show(&egui::Context::default(), None, Some("new-account")),
            None
        );
        assert!(
            !manager.open
                && manager.devices.is_empty()
                && manager.confirmation.is_none()
                && manager.worker.is_none()
        );
    }

    #[test]
    fn successful_self_unbind_requests_signout_only_for_the_active_account() {
        let mut manager = Manager {
            open: true,
            account: Some("active-account".into()),
            worker: Some(pending_event(Event::Unbound {
                id: "current-device".into(),
                current: true,
            })),
            ..Default::default()
        };
        assert_eq!(
            manager.show(&egui::Context::default(), None, Some("active-account")),
            Some(true)
        );
        assert!(!manager.open && manager.worker.is_none());
    }
}
