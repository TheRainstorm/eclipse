//! Save both roles first, then apply the host entrance only at a reserved idle point.
use super::received;
use eframe::egui;
#[cfg(windows)]
use std::time::Instant;
use std::{sync::mpsc, time::Duration};
#[cfg(not(windows))]
use uu_core::device_book::Store;
use uu_core::{
    device_book::Service,
    internet::enrollment::{Enrollment, Publication},
    Config,
};
use zeroize::Zeroizing;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Activation {
    Applied,
    Deferred,
}
struct Joined {
    service: Service,
    activation: Activation,
    #[cfg(windows)]
    intent: Option<String>,
}
#[cfg(windows)]
const WAITING: &str = "已加入连接服务。保持应用运行，空闲后自动启用；关闭后下次打开继续。";
#[cfg(not(windows))]
const WAITING: &str = "已加入连接服务。保持应用运行，空闲后自动启用本机共享。";
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Apply {
    Applied,
    Deferred,
    Superseded,
}
#[cfg(any(windows, test))]
fn activation_state(actual: &str, restart_required: bool, intended: &str) -> Apply {
    if !restart_required {
        Apply::Applied
    } else if actual == intended {
        Apply::Deferred
    } else {
        Apply::Superseded
    }
}
#[derive(Default)]
pub(crate) struct Join {
    open: bool,
    code: Zeroizing<String>,
    error: String,
    notice: String,
    pending: Option<mpsc::Receiver<Result<Joined, String>>>,
    #[cfg(windows)]
    retry: Option<mpsc::Receiver<(String, Result<Apply, String>)>>,
    #[cfg(windows)]
    config: Option<Config>,
    #[cfg(windows)]
    restore: Option<mpsc::Receiver<Result<Option<String>, String>>>,
    #[cfg(windows)]
    intent: Option<String>,
    deferred: bool,
    #[cfg(windows)]
    next_retry: Option<Instant>,
    changed: bool,
}
impl Join {
    #[cfg(windows)]
    pub fn bind_config(&mut self, config: Option<&Config>, ctx: &egui::Context) {
        if self.config.is_some() {
            return;
        }
        let Some(config) = config.cloned() else {
            return;
        };
        self.config = Some(config.clone());
        let context = ctx.clone();
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = send.send(super::pending::load(&config).map_err(|error| format!("{error:#}")));
            context.request_repaint();
        });
        self.restore = Some(receive);
    }
    pub fn open(&mut self) {
        self.open = true;
        self.error.clear();
    }
    pub fn poll(&mut self, ctx: &egui::Context) {
        #[cfg(windows)]
        if let Some(result) = received(&self.restore).map(|result| result.and_then(|result| result))
        {
            self.restore = None;
            match result {
                Ok(Some(revision)) if self.intent.is_none() => {
                    self.intent = Some(revision);
                    self.deferred = true;
                    self.notice = uu_i18n::text(WAITING).into();
                }
                Ok(_) => {}
                Err(error) => {
                    self.error = format!(
                        "{}：{error}",
                        uu_i18n::text("未能读取待启用状态，请在服务管理中确认配置")
                    )
                }
            }
        }
        if let Some(result) = received(&self.pending).map(|result| result.and_then(|result| result))
        {
            self.pending = None;
            match result {
                Ok(joined) => {
                    let _ = joined.service;
                    self.open = false;
                    self.code.clear();
                    self.error.clear();
                    self.changed = true;
                    self.deferred = joined.activation == Activation::Deferred;
                    #[cfg(windows)]
                    {
                        self.intent = joined.intent;
                        self.next_retry = None;
                    }
                    self.notice = uu_i18n::text(if self.deferred {
                        WAITING
                    } else {
                        "已加入连接服务，本机共享已启用。"
                    })
                    .into();
                }
                Err(error) => self.error = error,
            }
        }
        #[cfg(windows)]
        {
            if let Some(result) = received(&self.retry) {
                self.retry = None;
                match result {
                    Ok((revision, result)) if self.intent.as_deref() == Some(&revision) => {
                        match result {
                            Ok(Apply::Applied) => {
                                self.deferred = false;
                                self.intent = None;
                                self.error.clear();
                                self.notice = uu_i18n::text("本机连接服务已启用。").into();
                            }
                            Ok(Apply::Superseded) => {
                                self.deferred = false;
                                self.intent = None;
                                self.notice = uu_i18n::text(
                                    "服务配置已再次修改，请在服务管理中保存并重启服务。",
                                )
                                .into();
                            }
                            Ok(Apply::Deferred) => {}
                            Err(error) => self.error = error,
                        }
                    }
                    Ok(_) => {}
                    Err(error) => self.error = error,
                }
                self.next_retry = Some(Instant::now() + Duration::from_secs(5));
            }
            if self.deferred
                && self.retry.is_none()
                && self.next_retry.is_none_or(|at| Instant::now() >= at)
            {
                if let (Some(config), Some(revision)) = (self.config.clone(), self.intent.clone()) {
                    let context = ctx.clone();
                    let (send, receive) = mpsc::channel();
                    std::thread::spawn(move || {
                        let result = activate_background(&config, &revision);
                        let _ = send.send((revision, result));
                        context.request_repaint();
                    });
                    self.retry = Some(receive);
                }
            }
        }
        let busy = self.pending.is_some();
        #[cfg(windows)]
        let busy = busy || self.retry.is_some() || self.restore.is_some();
        if busy {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else if self.deferred {
            ctx.request_repaint_after(Duration::from_secs(5));
        }
    }
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }
    #[cfg(not(windows))]
    pub fn activation_pending(&self) -> bool {
        self.deferred
    }
    #[cfg(not(windows))]
    pub fn require_activation(&mut self) {
        self.deferred = true;
        self.notice = uu_i18n::text(WAITING).into();
    }
    #[cfg(not(windows))]
    pub fn activation_started(&mut self) {
        self.deferred = false;
        self.notice = uu_i18n::text("已保存并请求启用本机共享，连接状态见下方。").into();
    }
    pub fn status(&self, ui: &mut egui::Ui) {
        if !self.notice.is_empty() {
            ui.weak(&self.notice);
        }
        if !self.error.is_empty() && !self.open {
            ui.colored_label(crate::theme::palette(ui.visuals()).danger, &self.error);
        }
    }
    pub fn show(&mut self, ctx: &egui::Context, config: Option<&Config>) {
        #[cfg(windows)]
        self.bind_config(config, ctx);
        self.poll(ctx);
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new(uu_i18n::text("加入连接服务"))
            .id("connection-service-enrollment".into())
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(440.)
            .show(ctx, |ui| {
                ui.label(uu_i18n::text("粘贴管理员提供的接入码或链接"));
                ui.add_enabled(
                    self.pending.is_none(),
                    crate::theme::Input::new(&mut *self.code)
                        .password(true)
                        .char_limit(128 * 1024)
                        .desired_width(f32::INFINITY)
                        .hint_text("eclipse://connect/…"),
                );
                ui.weak(uu_i18n::text(
                    "加入后可互相发现设备；优先直连，需要时自动中转。桌面访问仍需配对授权。",
                ));
                if !self.error.is_empty() {
                    ui.colored_label(crate::theme::palette(ui.visuals()).danger, &self.error);
                }
                let parsed = Enrollment::decode(&self.code);
                if let Ok(value) = &parsed {
                    ui.label(format!("{} · {}", value.name, value.url));
                }
                if self.pending.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(uu_i18n::text("正在加入连接服务…"));
                    });
                } else if ui
                    .add_enabled(
                        config.is_some() && !self.code.trim().is_empty(),
                        crate::theme::primary_button(uu_i18n::text("加入并启用")),
                    )
                    .clicked()
                {
                    match parsed {
                        Err(error) => self.error = uu_i18n::text(&error.to_string()).into(),
                        Ok(_) => {
                            let config = config.unwrap().clone();
                            let code = self.code.clone();
                            let context = ctx.clone();
                            let (send, receive) = mpsc::channel();
                            std::thread::spawn(move || {
                                let _ = send.send(join(&config, &code));
                                context.request_repaint();
                            });
                            self.pending = Some(receive);
                            self.error.clear();
                            self.notice.clear();
                        }
                    }
                }
            });
        self.open = open && self.open;
    }
}
fn join(config: &Config, code: &str) -> Result<Joined, String> {
    let invitation =
        Enrollment::decode(code).map_err(|error| uu_i18n::text(&error.to_string()).to_owned())?;
    let directory = config.at("device-trust");
    #[cfg(windows)]
    {
        use uu_core::service_admin::{connections, Request, Response};
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let response = runtime
            .block_on(uu_core::service_admin::windows::request_owner(
                Request::Connections {
                    request: connections::Request::Import {
                        invitation: Zeroizing::new(code.into()),
                    },
                },
            ))
            .map_err(|error| {
                format!(
                    "{}：{error:#}",
                    uu_i18n::text("未能保存本机服务配置，请在服务管理中启动或更新服务后重试")
                )
            })?;
        let snapshot = match response {
            Response::Connections { snapshot } => snapshot,
            Response::Error { message } => return Err(message),
            _ => return Err(uu_i18n::text("主机尚不支持接入码，请更新服务后重试。").into()),
        };
        let imported = snapshot
            .imported
            .ok_or_else(|| uu_i18n::text("主机没有确认导入结果，请刷新后重试。").to_owned())?;
        let service = uu_core::internet::enrollment::import(
            &directory,
            &invitation,
            Publication::Mirror(&imported.device),
            true,
        )
        .map_err(|error| {
            format!(
                "{}：{error:#}",
                uu_i18n::text("本机服务配置已保存，但客户端配置保存失败；重新加入即可补齐")
            )
        })?;
        let activation = if snapshot.restart_required {
            Activation::Deferred
        } else {
            Activation::Applied
        };
        let intent = if activation == Activation::Deferred {
            super::pending::save(config, &snapshot.revision).map_err(|error| format!("{}：{error:#}",
                uu_i18n::text("连接服务已保存，但待启用状态保存失败；请保持应用运行并重新加入，或在服务管理中重启服务")))?;
            Some(snapshot.revision)
        } else {
            None
        };
        Ok(Joined {
            service,
            activation,
            intent,
        })
    }
    #[cfg(not(windows))]
    {
        let trust =
            uu_core::device_trust::TrustStore::open(&directory).map_err(|e| e.to_string())?;
        let book = Store::open(&directory)
            .and_then(|store| store.read())
            .map_err(|e| e.to_string())?;
        let name = book.sharing.name;
        let device = trust.card(&name).id();
        let service = uu_core::internet::enrollment::import(
            &directory,
            &invitation,
            Publication::PreserveOrDefault(&device),
            true,
        )
        .map_err(|e| format!("{e:#}"))?;
        let changed = !book.services.iter().any(|old| old == &service);
        Ok(Joined {
            service,
            activation: if changed {
                Activation::Deferred
            } else {
                Activation::Applied
            },
        })
    }
}

#[cfg(windows)]
fn activate_background(config: &Config, revision: &str) -> Result<Apply, String> {
    use uu_core::service_admin::{
        windows::{control_owner, request_owner},
        Request, Response,
    };
    let apply = || -> anyhow::Result<Apply> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let inspect = || -> anyhow::Result<Apply> {
            match runtime.block_on(request_owner(Request::Connections {
                request: uu_core::service_admin::connections::Request::Inspect,
            }))? {
                Response::Connections { snapshot } => Ok(activation_state(
                    &snapshot.revision,
                    snapshot.restart_required,
                    revision,
                )),
                Response::Error { message } => anyhow::bail!("{message}"),
                _ => anyhow::bail!("unexpected connections inspection response"),
            }
        };
        let state = inspect()?;
        if state != Apply::Deferred {
            return Ok(state);
        }
        if !crate::service_ui::enrollment_restart_ready()? {
            return Ok(Apply::Deferred);
        }
        let token = uuid::Uuid::new_v4().to_string();
        if !matches!(
            runtime.block_on(request_owner(Request::ConfigurationSwitch {
                token: token.clone(),
                release: false
            }))?,
            Response::Applied
        ) {
            return Ok(Apply::Deferred);
        }
        let result = (|| {
            let state = inspect()?;
            if state != Apply::Deferred {
                return Ok(state);
            }
            if !crate::service_ui::enrollment_restart_ready()? {
                return Ok(Apply::Deferred);
            }
            control_owner(false)?;
            control_owner(true)?;
            // SCM running is not the media worker's configuration receipt.
            // The next Inspect must confirm it before clearing the intention.
            Ok(Apply::Deferred)
        })();
        let _ = runtime.block_on(request_owner(Request::ConfigurationSwitch {
            token,
            release: true,
        }));
        result
    };
    let result = apply().and_then(|state| {
        if state != Apply::Deferred {
            super::pending::clear(config, revision)?;
        }
        Ok(state)
    });
    result.map_err(|error| {
        format!(
            "{}：{error:#}",
            uu_i18n::text("服务配置已保存，自动启用暂未完成；请在服务管理中检查")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_activation_requires_its_saved_revision_and_accepts_already_applied_newer_config() {
        assert_eq!(activation_state("first", true, "first"), Apply::Deferred);
        assert_eq!(activation_state("second", true, "first"), Apply::Superseded);
        assert_eq!(activation_state("second", false, "first"), Apply::Applied);
        assert_eq!(activation_state("first", false, "first"), Apply::Applied);
    }
    #[test]
    fn lost_enrollment_worker_exposes_unknown_result_without_losing_the_pasted_code() {
        let (send, receive) = mpsc::channel();
        let mut join = Join {
            open: true,
            code: Zeroizing::new("owned fixture".into()),
            pending: Some(receive),
            ..Default::default()
        };
        drop(send);
        join.poll(&egui::Context::default());
        assert!(join.pending.is_none() && !join.error.is_empty());
        assert_eq!(join.code.as_str(), "owned fixture");
        assert!(join.open && !join.deferred);
    }
    #[cfg(not(windows))]
    #[test]
    fn local_enrollment_publishes_the_saved_identity_and_reimport_does_not_request_restart() {
        let root =
            std::env::temp_dir().join(format!("eclipse-enrollment-ui-{}", uuid::Uuid::new_v4()));
        let config = Config {
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            signaling_host: String::new(),
        };
        let invitation = Enrollment::new(
            "Fixture service".into(),
            "wss://signal.example/v1/signal".into(),
            Zeroizing::new("ab".repeat(32)),
            None,
        )
        .unwrap()
        .encode()
        .unwrap();
        let first = super::join(&config, &invitation).unwrap();
        assert!(first.activation == Activation::Deferred && first.service.host_enabled);
        let second = super::join(&config, &invitation).unwrap();
        assert!(second.activation == Activation::Applied);
        assert_eq!(first.service.id, second.service.id);
        let identity = uu_core::device_trust::TrustStore::open(config.at("device-trust"))
            .unwrap()
            .card("Fixture host")
            .id();
        assert_eq!(second.service.published_name, identity);
        std::fs::remove_dir_all(root).unwrap();
    }
}
