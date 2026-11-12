use super::*;
use uu_core::service_admin::connections::{CertificateChange, Request as Command, Snapshot};

#[derive(Default)]
pub(crate) struct State {
    pub(crate) snapshot: Option<Snapshot>,
    pub(crate) servers: crate::connection_service_ui::Servers,
    removal: Option<Removal>,
    removing: Option<
        std::sync::mpsc::Receiver<
            Result<Option<Snapshot>, crate::connection_service_ui::removal::Failure>,
        >,
    >,
    error: String,
}
struct Removal {
    id: String,
    address: String,
    revision: String,
    owner_removed: bool,
}
impl State {
    pub(crate) fn poll(&mut self, ctx: &egui::Context, config: Option<&uu_core::Config>) {
        self.servers.poll(ctx, config);
        if let Some(result) = crate::connection_service_ui::received(&self.removing) {
            self.removing = None;
            match result {
                Ok(Ok(snapshot)) => {
                    if let Some(snapshot) = snapshot {
                        self.snapshot = Some(snapshot);
                    }
                    self.removal = None;
                    self.error.clear();
                    self.servers.refresh_directory();
                }
                Ok(Err(failure)) => {
                    if let Some(snapshot) = failure.owner_snapshot {
                        self.snapshot = Some(snapshot);
                        if let Some(removal) = &mut self.removal {
                            removal.owner_removed = true;
                        }
                    }
                    let partial = self
                        .removal
                        .as_ref()
                        .is_some_and(|removal| removal.owner_removed);
                    self.error = if partial {
                        format!(
                            "{}：{}",
                            uu_i18n::text(
                                "主机已移除服务器，客户端配置仍保留；请处理错误后重试清理"
                            ),
                            failure.message
                        )
                    } else {
                        failure.message
                    };
                }
                Err(error) => self.error = error,
            }
        }
        if self.removing.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    fn start_removal(&mut self, ctx: &egui::Context, config: &uu_core::Config) {
        if self.removing.is_some() {
            return;
        }
        let Some(removal) = &self.removal else {
            return;
        };
        let (id, address, revision, owner_removed) = (
            removal.id.clone(),
            removal.address.clone(),
            removal.revision.clone(),
            removal.owner_removed,
        );
        let config = config.clone();
        let context = ctx.clone();
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = crate::connection_service_ui::removal::remove(&config, &address, || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                if owner_removed {
                    match runtime.block_on(uu_core::service_admin::windows::request_owner(
                        Request::Connections {
                            request: Command::Inspect,
                        },
                    ))? {
                        Response::Connections { snapshot } => {
                            crate::connection_service_ui::removal::confirm_owner_absent(
                                &snapshot, &address,
                            )?
                        }
                        Response::Error { message } => anyhow::bail!("{message}"),
                        _ => anyhow::bail!(
                            "{}",
                            uu_i18n::text("主机没有确认移除结果，请刷新服务状态。")
                        ),
                    }
                    return Ok(None);
                }
                match runtime.block_on(uu_core::service_admin::windows::request_owner(
                    Request::Connections {
                        request: Command::Remove { revision, id },
                    },
                ))? {
                    Response::Connections { snapshot } => Ok(Some(snapshot)),
                    Response::Error { message } => anyhow::bail!("{message}"),
                    _ => anyhow::bail!(
                        "{}",
                        uu_i18n::text("主机没有确认移除结果，请刷新服务状态。")
                    ),
                }
            });
            let _ = send.send(result);
            context.request_repaint();
        });
        self.removing = Some(receive);
        self.error.clear();
    }
    pub(crate) fn apply(
        &mut self,
        completed: bool,
        result: Result<Response, String>,
    ) -> Option<Result<String, String>> {
        match result {
            Ok(Response::Connections { snapshot }) => {
                if self.removing.is_none() {
                    if let Some(removal) = &mut self.removal {
                        if !snapshot.entries.iter().any(|entry| entry.id == removal.id) {
                            removal.owner_removed = true;
                        }
                    }
                }
                let message = if snapshot.restart_required {
                    "服务器配置已保存；重启服务后使用新入口。"
                } else {
                    "服务器配置已保存。"
                };
                self.snapshot = Some(snapshot);
                completed.then(|| Ok(uu_i18n::text(message).into()))
            }
            Ok(Response::Error { message }) | Err(message) if completed => Some(Err(message)),
            _ => None,
        }
    }
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        enabled: bool,
        config: Option<&uu_core::Config>,
    ) -> Option<Command> {
        self.poll(ui.ctx(), config);
        let servers: Vec<_> = self
            .snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .entries
                    .iter()
                    .map(|entry| crate::connection_service_ui::Server {
                        id: entry.id.clone(),
                        name: entry.name.clone(),
                        address: entry.address.clone(),
                        host_enabled: entry.host_enabled,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let action = self.servers.show(
            ui,
            config,
            &servers,
            enabled && self.snapshot.is_some() && self.removing.is_none(),
        );
        if self.removing.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(uu_i18n::text("正在移除服务器…"));
            });
        }
        if !self.error.is_empty() {
            ui.colored_label(crate::theme::palette(ui.visuals()).danger, &self.error);
            if self
                .removal
                .as_ref()
                .is_some_and(|removal| removal.owner_removed)
                && self.removing.is_none()
            {
                if ui
                    .add_enabled(
                        config.is_some(),
                        crate::theme::secondary_button(uu_i18n::text("重试清理客户端配置")),
                    )
                    .clicked()
                {
                    self.start_removal(ui.ctx(), config.unwrap());
                }
            }
        }
        if self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.restart_required)
        {
            ui.weak(uu_i18n::text("服务器配置已保存；重启服务后使用新入口。"));
        }
        match action? {
            crate::connection_service_ui::ServerAction::Remove { id } => {
                let snapshot = self.snapshot.as_ref()?;
                let entry = snapshot.entries.iter().find(|entry| entry.id == id)?;
                self.removal = Some(Removal {
                    id,
                    address: entry.address.clone(),
                    revision: snapshot.revision.clone(),
                    owner_removed: false,
                });
                if let Some(config) = config {
                    self.start_removal(ui.ctx(), config);
                }
                None
            }
            action => self.command(action),
        }
    }
    fn command(&self, action: crate::connection_service_ui::ServerAction) -> Option<Command> {
        let snapshot = self.snapshot.as_ref()?;
        match action {
            crate::connection_service_ui::ServerAction::Remove { .. } => None,
            crate::connection_service_ui::ServerAction::SetEnabled { id, enabled } => {
                let mut entry = snapshot
                    .entries
                    .iter()
                    .find(|entry| entry.id == id)?
                    .clone();
                entry.host_enabled = enabled;
                Some(Command::Save {
                    revision: snapshot.revision.clone(),
                    entry,
                    token: None,
                    certificate: CertificateChange::Keep,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn toggling_host_publication_preserves_group_identity_and_write_only_credentials() {
        let mut state = State::default();
        state.apply(
            false,
            Ok(Response::Connections {
                snapshot: Snapshot {
                    imported: None,
                    revision: "fixture-revision".into(),
                    restart_required: false,
                    entries: vec![uu_core::service_admin::connections::Entry {
                        id: "fixture-service".into(),
                        name: "Private group".into(),
                        address: "wss://signal.example/v1/signal".into(),
                        published_name: "internal-routing-id".into(),
                        host_enabled: true,
                        token_configured: true,
                        certificate_configured: true,
                    }],
                },
            }),
        );
        let command = state
            .command(crate::connection_service_ui::ServerAction::SetEnabled {
                id: "fixture-service".into(),
                enabled: false,
            })
            .unwrap();
        let Command::Save {
            revision,
            entry,
            token,
            certificate,
        } = command
        else {
            panic!("save");
        };
        assert_eq!(revision, "fixture-revision");
        assert_eq!(entry.name, "Private group");
        assert_eq!(entry.published_name, "internal-routing-id");
        assert!(!entry.host_enabled && entry.token_configured && entry.certificate_configured);
        assert!(token.is_none() && matches!(certificate, CertificateChange::Keep));
    }
}
