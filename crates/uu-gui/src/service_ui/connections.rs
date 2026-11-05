use super::*;
use uu_core::service_admin::connections::{CertificateChange, Entry, Request as Command, Snapshot};

#[derive(Default)]
pub(super) struct State {
    pub snapshot: Option<Snapshot>,
    editor: Option<Editor>,
}
struct Editor {
    revision: String,
    value: Entry,
    token: zeroize::Zeroizing<String>,
    certificate_mode: u8,
    pem: String,
    error: String,
}
impl Editor {
    fn new(revision: String, value: Entry) -> Self {
        Self {
            revision,
            value,
            token: Default::default(),
            certificate_mode: 0,
            pem: String::new(),
            error: String::new(),
        }
    }
    fn command(&self) -> anyhow::Result<Command> {
        anyhow::ensure!(
            self.value.token_configured || !self.token.trim().is_empty(),
            "{}",
            uu_i18n::text("请填写服务器访问令牌")
        );
        Ok(Command::Save {
            revision: self.revision.clone(),
            entry: self.value.clone(),
            token: (!self.token.trim().is_empty()).then(|| self.token.clone()),
            certificate: match self.certificate_mode {
                1 => CertificateChange::Remove,
                2 => CertificateChange::Set {
                    pem: self.pem.clone(),
                },
                _ => CertificateChange::Keep,
            },
        })
    }
}
impl State {
    pub fn apply(&mut self, completed: bool, result: Result<Response, String>) -> Option<String> {
        match result {
            Ok(Response::Connections { snapshot }) => {
                self.snapshot = Some(snapshot);
                if completed {
                    self.editor = None;
                    Some(uu_i18n::text("服务器配置已保存；重启服务后使用新入口。").into())
                } else {
                    None
                }
            }
            Ok(Response::Error { message }) | Err(message) if completed => {
                if let Some(editor) = &mut self.editor {
                    editor.error = message.clone();
                }
                Some(message)
            }
            _ => None,
        }
    }
    pub fn show(&mut self, ui: &mut egui::Ui, enabled: bool) -> Option<Command> {
        let mut command = None;
        ui.add_space(12.);
        ui.strong(uu_i18n::text("自建服务器"));
        let snapshot = self.snapshot.clone();
        ui.add_enabled_ui(enabled && snapshot.is_some(), |ui| {
            if let Some(snapshot) = snapshot {
                for value in snapshot.entries {
                    crate::theme::card(ui.visuals()).show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.strong(&value.name);
                            ui.weak(if value.host_enabled {
                                uu_i18n::text("已启用")
                            } else {
                                uu_i18n::text("未启用")
                            });
                            if ui
                                .add(crate::theme::secondary_button(uu_i18n::text("编辑")))
                                .clicked()
                            {
                                self.editor =
                                    Some(Editor::new(snapshot.revision.clone(), value.clone()));
                            }
                            if ui
                                .add(crate::theme::secondary_button(uu_i18n::text("删除")))
                                .clicked()
                            {
                                command = Some(Command::Remove {
                                    revision: snapshot.revision.clone(),
                                    id: value.id.clone(),
                                });
                            }
                        });
                        ui.label(&value.address);
                    });
                }
                if snapshot.restart_required {
                    ui.weak(uu_i18n::text("服务器配置已保存；重启服务后使用新入口。"));
                }
                if ui
                    .add(crate::theme::secondary_button(uu_i18n::text(
                        "＋ 添加服务器",
                    )))
                    .clicked()
                {
                    self.editor = Some(Editor::new(
                        snapshot.revision,
                        Entry {
                            published_name: "eclipse-desktop".into(),
                            ..Default::default()
                        },
                    ));
                }
            }
        });
        if let Some(editor) = &mut self.editor {
            let mut open = true;
            let mut cancel = false;
            egui::Window::new(uu_i18n::text("连接服务器"))
                .id("service-owner-connection-editor".into())
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(480.)
                .show(ui.ctx(), |ui| {
                    ui.add_enabled_ui(enabled, |ui| {
                        if !editor.error.is_empty() {
                            ui.colored_label(
                                crate::theme::palette(ui.visuals()).danger,
                                &editor.error,
                            );
                        }
                        field(ui, "名称", &mut editor.value.name, "例如：家庭服务器");
                        field(
                            ui,
                            "服务器地址",
                            &mut editor.value.address,
                            "signal.example:8443 或 wss://…/v1/signal",
                        );
                        ui.label(uu_i18n::text("访问令牌"));
                        ui.add(
                            crate::theme::Input::new(&mut *editor.token)
                                .password(true)
                                .char_limit(128)
                                .desired_width(f32::INFINITY)
                                .hint_text(uu_i18n::text(if editor.value.token_configured {
                                    "留空则保留已保存的凭据"
                                } else {
                                    "粘贴服务器提供的访问令牌"
                                })),
                        );
                        ui.collapsing(uu_i18n::text("证书与高级设置"), |ui| {
                            ui.horizontal_wrapped(|ui| {
                                crate::theme::tab_value(
                                    ui,
                                    &mut editor.certificate_mode,
                                    0,
                                    uu_i18n::text("保留证书"),
                                );
                                crate::theme::tab_value(
                                    ui,
                                    &mut editor.certificate_mode,
                                    1,
                                    uu_i18n::text("公共可信证书"),
                                );
                                crate::theme::tab_value(
                                    ui,
                                    &mut editor.certificate_mode,
                                    2,
                                    uu_i18n::text("设置私有 CA"),
                                );
                            });
                            if editor.certificate_mode == 2 {
                                ui.add(
                                    crate::theme::Input::multiline(&mut editor.pem)
                                        .desired_rows(4)
                                        .char_limit(65536)
                                        .desired_width(f32::INFINITY)
                                        .hint_text(uu_i18n::text("粘贴 PEM 格式的 CA 证书内容")),
                                );
                            }
                        });
                        crate::theme::switch(
                            ui,
                            &mut editor.value.host_enabled,
                            uu_i18n::text("共享本机时使用此服务器"),
                        );
                        field(
                            ui,
                            "本机在服务器的设备名称",
                            &mut editor.value.published_name,
                            "my-desktop（字母、数字、-、_）",
                        );
                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .add(crate::theme::primary_button(uu_i18n::text("保存服务器")))
                                .clicked()
                            {
                                match editor.command() {
                                    Ok(value) => command = Some(value),
                                    Err(error) => editor.error = error.to_string(),
                                }
                            }
                            if ui
                                .add(crate::theme::secondary_button(uu_i18n::text("取消")))
                                .clicked()
                            {
                                cancel = true;
                            }
                        });
                    });
                });
            if !open || cancel {
                self.editor = None;
            }
        }
        command
    }
}
fn field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    ui.label(uu_i18n::text(label));
    ui.add(
        crate::theme::Input::new(value)
            .desired_width(f32::INFINITY)
            .char_limit(2048)
            .hint_text(uu_i18n::text(hint)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn periodic_service_refresh_and_failed_save_preserve_connection_drafts_and_credentials() {
        let mut state = State {
            editor: Some(Editor::new(
                "old-revision".into(),
                Entry {
                    name: "Home".into(),
                    address: "signal.example:8443".into(),
                    ..Default::default()
                },
            )),
            ..Default::default()
        };
        assert!(state.editor.as_ref().unwrap().command().is_err());
        state.editor.as_mut().unwrap().token = zeroize::Zeroizing::new("ab".repeat(32));
        state.apply(
            false,
            Ok(Response::Connections {
                snapshot: Snapshot {
                    revision: "new-revision".into(),
                    entries: vec![],
                    restart_required: false,
                },
            }),
        );
        state.apply(true, Err("generated CAS conflict".into()));
        let editor = state.editor.as_ref().unwrap();
        assert_eq!(editor.value.name, "Home");
        assert_eq!(editor.value.address, "signal.example:8443");
        assert_eq!(editor.error, "generated CAS conflict");
        let Command::Save {
            revision, token, ..
        } = editor.command().unwrap()
        else {
            panic!("save")
        };
        assert_eq!(revision, "old-revision");
        assert_eq!(&**token.as_ref().unwrap(), &"ab".repeat(32));
        state.apply(
            true,
            Ok(Response::Connections {
                snapshot: Snapshot {
                    revision: "saved-revision".into(),
                    entries: vec![],
                    restart_required: true,
                },
            }),
        );
        assert!(state.editor.is_none());
    }
}
