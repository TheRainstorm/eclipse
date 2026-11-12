//! One server list owns enrollment, local aliases and each endpoint's directory.
use super::{Directory, Join, Labels};
use eframe::egui;
use uu_core::Config;

#[derive(Clone)]
pub(crate) struct Server {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) address: String,
    pub(crate) host_enabled: bool,
}
impl From<&uu_core::device_book::Service> for Server {
    fn from(service: &uu_core::device_book::Service) -> Self {
        Self {
            id: service.id.clone(),
            name: service.name.clone(),
            address: service.address.clone(),
            host_enabled: service.host_enabled,
        }
    }
}
pub(crate) enum ServerAction {
    SetEnabled { id: String, enabled: bool },
    Remove { id: String },
}
struct AliasEditor {
    address: String,
    value: String,
    submitted: bool,
    saved_generation: u64,
}
#[derive(Default)]
pub(crate) struct Servers {
    join: Join,
    directory: Directory,
    labels: Labels,
    alias: Option<AliasEditor>,
    open: Option<crate::sessions::StreamTarget>,
    config_changed: bool,
}
impl Servers {
    pub(crate) fn is_open(&self) -> bool {
        self.join.is_open() || self.alias.is_some()
    }
    #[cfg(test)]
    pub(crate) fn fixture() -> (Self, Vec<Server>) {
        use uu_core::device_book::{Book, Service};
        let mut state = Self::default();
        let mut book = Book::default();
        let mut rows = Vec::new();
        let mut entries = std::collections::HashMap::new();
        let mut names = super::Names::new();
        for (index, group_name, remote) in
            [(1, "我的设备", "笔记本电脑"), (2, "工作设备", "办公室电脑")]
        {
            let address = uu_core::internet::enrollment::group_url(
                "wss://203.0.113.10:9443/v1/signal",
                &uuid::Uuid::from_u128(index),
            )
            .unwrap();
            let service = Service {
                id: uuid::Uuid::new_v4().to_string(),
                name: group_name.into(),
                address: address.clone(),
                token_file: "fixture-unused-token".into(),
                certificate: None,
                host_enabled: true,
                published_name: "own-device-route-id".into(),
            };
            let mut row = Server::from(&service);
            row.id = uuid::Uuid::new_v4().to_string();
            entries.insert(
                service.id.clone(),
                Ok(vec![
                    uu_core::internet::self_hosted::Device {
                        device: "own-device-route-id".into(),
                        name: "本机（不应显示）".into(),
                    },
                    uu_core::internet::self_hosted::Device {
                        device: format!("remote-{index}-route-id"),
                        name: remote.into(),
                    },
                ]),
            );
            if index == 1 {
                names.insert(super::address_key(&address), "家里的服务器".into());
            }
            book.services.push(service);
            rows.push(row);
        }
        state.directory.seed_fixture(book, entries);
        state.labels = Labels::fixture(names);
        (state, rows)
    }
    pub(crate) fn poll(&mut self, ctx: &egui::Context, config: Option<&Config>) {
        #[cfg(windows)]
        self.join.bind_config(config, ctx);
        self.join.poll(ctx);
        self.labels.poll(ctx, config);
        if let Some(target) = self.directory.take_open() {
            self.open = Some(target);
        }
    }
    pub(crate) fn take_changed(&mut self) -> bool {
        let joined = self.join.take_changed();
        if joined {
            self.directory.reset();
        }
        self.config_changed |= joined;
        self.labels.take_changed() || joined
    }
    pub(crate) fn take_config_changed(&mut self) -> bool {
        std::mem::take(&mut self.config_changed)
    }
    pub(crate) fn take_open(&mut self) -> Option<crate::sessions::StreamTarget> {
        self.open.take().or_else(|| self.directory.take_open())
    }
    pub(crate) fn refresh_directory(&mut self) {
        self.directory.reset();
    }
    pub(crate) fn join_mut(&mut self) -> &mut Join {
        &mut self.join
    }
    pub(crate) fn labels(&self) -> &Labels {
        &self.labels
    }
    pub(crate) fn labels_mut(&mut self) -> &mut Labels {
        &mut self.labels
    }
    pub(crate) fn dialogs(&mut self, ctx: &egui::Context, config: Option<&Config>) {
        self.join.show(ctx, config);
        if self.alias.as_ref().is_some_and(|editor| {
            editor.submitted && self.labels.saved_generation() > editor.saved_generation
        }) {
            self.alias = None;
        }
        if let Some(editor) = &mut self.alias {
            let mut open = true;
            let mut cancel = false;
            egui::Window::new(uu_i18n::text("服务器别名"))
                .id("connection-server-alias".into())
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(360.)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label(uu_i18n::text("仅在本机显示，留空使用地址和设备组编号。"));
                    ui.add(
                        crate::theme::Input::new(&mut editor.value)
                            .char_limit(64)
                            .desired_width(f32::INFINITY),
                    );
                    if !self.labels.error.is_empty() {
                        ui.colored_label(
                            crate::theme::palette(ui.visuals()).danger,
                            &self.labels.error,
                        );
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                config.is_some() && self.labels.ready(),
                                crate::theme::primary_button(uu_i18n::text("保存")),
                            )
                            .clicked()
                        {
                            self.labels.save(
                                ctx,
                                config.unwrap(),
                                editor.address.clone(),
                                editor.value.clone(),
                            );
                            editor.submitted = true;
                        }
                        if ui
                            .add(crate::theme::secondary_button(uu_i18n::text("取消")))
                            .clicked()
                        {
                            cancel = true;
                        }
                    });
                });
            if !open || cancel {
                self.alias = None;
            }
        }
    }
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        config: Option<&Config>,
        servers: &[Server],
        enabled: bool,
    ) -> Option<ServerAction> {
        self.poll(ui.ctx(), config);
        let mut action = None;
        crate::theme::card(ui.visuals()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong(uu_i18n::text("自建服务器"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            enabled && config.is_some(),
                            crate::theme::primary_button(format!(
                                "＋ {}",
                                uu_i18n::text("添加服务器")
                            )),
                        )
                        .clicked()
                    {
                        self.join.open();
                    }
                });
            });
            self.join.status(ui);
            if !self.labels.error.is_empty() {
                ui.colored_label(
                    crate::theme::palette(ui.visuals()).danger,
                    &self.labels.error,
                );
            }
            if servers.is_empty() {
                ui.weak(uu_i18n::text(
                    "粘贴接入码添加服务器，两台电脑使用同一设备组的接入码。",
                ));
            }
            for server in servers {
                ui.push_id(&server.id, |ui| {
                    ui.add_space(8.);
                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(self.labels.display_name(&server.address));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(
                                    enabled,
                                    crate::theme::secondary_button(uu_i18n::text("移除")).small(),
                                )
                                .clicked()
                            {
                                action = Some(ServerAction::Remove {
                                    id: server.id.clone(),
                                });
                            }
                            if ui
                                .add_enabled(
                                    enabled && self.labels.ready(),
                                    crate::theme::secondary_button(uu_i18n::text("修改别名"))
                                        .small(),
                                )
                                .clicked()
                            {
                                self.labels.clear_error();
                                self.alias = Some(AliasEditor {
                                    address: server.address.clone(),
                                    value: self.labels.alias(&server.address).into(),
                                    submitted: false,
                                    saved_generation: self.labels.saved_generation(),
                                });
                            }
                            if ui
                                .add(crate::theme::secondary_button(uu_i18n::text("刷新")).small())
                                .clicked()
                            {
                                self.directory.refresh();
                                self.labels.refresh();
                            }
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.weak(uu_i18n::text("设备组"));
                        ui.label(&server.name);
                    });
                    let mut host_enabled = server.host_enabled;
                    if ui
                        .add_enabled_ui(enabled, |ui| {
                            crate::theme::switch(
                                ui,
                                &mut host_enabled,
                                uu_i18n::text("允许通过此服务器连接本机"),
                            )
                        })
                        .inner
                        .changed()
                    {
                        action = Some(ServerAction::SetEnabled {
                            id: server.id.clone(),
                            enabled: host_enabled,
                        });
                    }
                    ui.weak(uu_i18n::text(if host_enabled {
                        "本机以设备名称自动上线；访问桌面仍需配对授权。"
                    } else {
                        "本机不在此服务器上线，仍可连接下方在线设备。"
                    }));
                    ui.add_space(6.);
                    ui.label(uu_i18n::text("在线设备"));
                    if let Some(target) = self.directory.show_service(ui, config, &server.address) {
                        self.open = Some(target);
                    }
                });
            }
        });
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn texts(output: egui::FullOutput) -> Vec<String> {
        fn collect(shape: &egui::Shape, values: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => values.push(text.galley.job.text.clone()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, values);
                    }
                }
                _ => {}
            }
        }
        let mut values = Vec::new();
        for shape in output.shapes {
            collect(&shape.shape, &mut values);
        }
        values
    }
    #[test]
    fn each_server_contains_only_its_own_directory_and_never_exposes_routing_ids() {
        let (mut state, servers) = Servers::fixture();
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let render = |ctx: &egui::Context, state: &mut Servers| {
            egui::CentralPanel::default().show(ctx, |ui| {
                state.show(ui, None, &servers, true);
            });
        };
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000., 1000.),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input(), |ctx| render(ctx, &mut state));
        let labels = texts(ctx.run(input(), |ctx| render(ctx, &mut state)));
        let index = |value: &str| {
            labels
                .iter()
                .position(|label| label == value)
                .unwrap_or_else(|| panic!("missing {value}: {labels:?}"))
        };
        assert!(index("家里的服务器") < index("我的设备"));
        assert!(index("我的设备") < index("笔记本电脑"));
        assert!(index("笔记本电脑") < index("工作设备"));
        assert!(index("工作设备") < index("办公室电脑"));
        assert!(!labels
            .iter()
            .any(|label| label.contains("route-id") || label.contains("本机（不应显示）")));
        assert_eq!(labels.iter().filter(|label| *label == "连接").count(), 2);
        assert!(state.directory.take_open().is_none());
    }
    #[test]
    fn server_addition_uses_one_invitation_without_manual_credentials_or_route_fields() {
        let mut state = Servers::default();
        state.join.open();
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let _ = ctx.run(Default::default(), |ctx| state.dialogs(ctx, None));
        let labels = texts(ctx.run(Default::default(), |ctx| state.dialogs(ctx, None)));
        assert!(labels.iter().any(|label| label == "添加服务器"));
        assert!(labels.iter().any(|label| label.contains("接入码")));
        assert!(!labels.iter().any(|label| matches!(
            label.as_str(),
            "访问令牌" | "证书与高级设置" | "本机在服务器的设备名称"
        )));
    }
    #[test]
    fn failed_alias_save_keeps_the_draft_when_a_later_refresh_succeeds() {
        let root =
            std::env::temp_dir().join(format!("eclipse-alias-draft-{}", uuid::Uuid::new_v4()));
        let config = Config {
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            signaling_host: String::new(),
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let path = config.at("connection-service-aliases.json");
        std::fs::write(&path, b"invalid fixture").unwrap();
        let (mut state, rows) = Servers::fixture();
        let ctx = egui::Context::default();
        state.alias = Some(AliasEditor {
            address: rows[0].address.clone(),
            value: "unsaved draft".into(),
            submitted: true,
            saved_generation: state.labels.saved_generation(),
        });
        state.labels.save(
            &ctx,
            &config,
            rows[0].address.clone(),
            "unsaved draft".into(),
        );
        let settle = |state: &mut Servers| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !state.labels.ready() {
                state.labels.poll(&ctx, Some(&config));
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        settle(&mut state);
        assert!(!state.labels.error.is_empty());
        std::fs::write(&path, b"{}").unwrap();
        state.labels.refresh();
        state.labels.poll(&ctx, Some(&config));
        settle(&mut state);
        let _ = ctx.run(Default::default(), |ctx| state.dialogs(ctx, Some(&config)));
        assert_eq!(state.alias.as_ref().unwrap().value, "unsaved draft");
        assert!(!state.labels.error.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
