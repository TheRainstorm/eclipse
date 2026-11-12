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
struct AliasDraft {
    value: String,
    baseline: String,
    saving: Option<(u64, String)>,
}
impl AliasDraft {
    fn sync(&mut self, saved: &str, generation: u64, ready: bool) {
        if let Some((before, submitted)) = &self.saving {
            if generation > *before {
                if self.value.trim() == submitted.trim() {
                    self.value = saved.into();
                }
                self.baseline = saved.into();
                self.saving = None;
            } else if ready {
                self.saving = None;
            }
        }
        if self.value == self.baseline && self.baseline != saved {
            self.value = saved.into();
            self.baseline = saved.into();
        }
    }
    fn dirty(&self, saved: &str) -> bool {
        self.value.trim() != saved
    }
}
#[derive(Default)]
pub(crate) struct Servers {
    join: Join,
    directory: Directory,
    labels: Labels,
    drafts: std::collections::HashMap<String, AliasDraft>,
    open: Option<crate::sessions::StreamTarget>,
    config_changed: bool,
}
impl Servers {
    pub(crate) fn is_open(&self) -> bool {
        self.join.is_open()
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
                let key = super::address_key(&server.address);
                let saved = self.labels.alias(&server.address).to_owned();
                let draft = self
                    .drafts
                    .entry(key.clone())
                    .or_insert_with(|| AliasDraft {
                        value: saved.clone(),
                        baseline: saved.clone(),
                        saving: None,
                    });
                draft.sync(&saved, self.labels.saved_generation(), self.labels.ready());
                ui.push_id(&key, |ui| {
                    ui.add_space(6.);
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let mut host_enabled = server.host_enabled;
                            let switched = ui
                                .add_enabled_ui(enabled, |ui| {
                                    crate::theme::switch(ui, &mut host_enabled, "")
                                })
                                .inner
                                .on_hover_text(uu_i18n::text("允许通过此服务器连接本机"));
                            ui.label(uu_i18n::text("启用"));
                            if switched.changed() {
                                action = Some(ServerAction::SetEnabled {
                                    id: server.id.clone(),
                                    enabled: host_enabled,
                                });
                            }
                            if ui
                                .add_enabled(
                                    enabled,
                                    crate::theme::secondary_button(uu_i18n::text("删除")).small(),
                                )
                                .clicked()
                            {
                                action = Some(ServerAction::Remove {
                                    id: server.id.clone(),
                                });
                            }
                            if ui
                                .add_enabled(
                                    enabled
                                        && config.is_some()
                                        && self.labels.ready()
                                        && draft.saving.is_none()
                                        && draft.dirty(&saved),
                                    crate::theme::secondary_button(uu_i18n::text("保存")).small(),
                                )
                                .clicked()
                            {
                                self.labels.clear_error();
                                draft.saving =
                                    Some((self.labels.saved_generation(), draft.value.clone()));
                                self.labels.save(
                                    ui.ctx(),
                                    config.unwrap(),
                                    server.address.clone(),
                                    draft.value.clone(),
                                );
                            }
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.label(uu_i18n::text("别名"));
                                    ui.add_enabled(
                                        enabled,
                                        crate::theme::Input::new(&mut draft.value)
                                            .char_limit(64)
                                            .desired_width(ui.available_width())
                                            .hint_text(super::display_name(
                                                &Default::default(),
                                                &server.address,
                                            )),
                                    )
                                    .on_hover_text(
                                        uu_i18n::text("仅在本机显示，留空使用地址和设备组编号。"),
                                    );
                                },
                            );
                        });
                    });
                    egui::CollapsingHeader::new(uu_i18n::text("服务器详情"))
                        .id_salt(("connection-server-details", &key))
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.weak("WSS");
                                ui.label(&server.address);
                                if ui
                                    .add(
                                        crate::theme::secondary_button(uu_i18n::text("复制"))
                                            .small(),
                                    )
                                    .clicked()
                                {
                                    ui.ctx().copy_text(server.address.clone());
                                }
                            });
                            ui.horizontal_wrapped(|ui| {
                                ui.weak(uu_i18n::text("设备组"));
                                ui.label(&server.name);
                            });
                            if let Ok(url) = url::Url::parse(&server.address) {
                                if let Ok(group) = uu_core::internet::server::groups::query_group(
                                    url.query().unwrap_or(""),
                                ) {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.weak("Group ID");
                                        ui.monospace(group.to_string());
                                        if ui
                                            .add(
                                                crate::theme::secondary_button(uu_i18n::text(
                                                    "复制",
                                                ))
                                                .small(),
                                            )
                                            .clicked()
                                        {
                                            ui.ctx().copy_text(group.to_string());
                                        }
                                    });
                                }
                            }
                        });
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
    struct FormHarness {
        state: Servers,
        rows: Vec<Server>,
        context: egui::Context,
        config: Config,
        root: std::path::PathBuf,
        time: f64,
        painted: Vec<(String, egui::Pos2)>,
    }
    impl FormHarness {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("eclipse-inline-servers-{}", uuid::Uuid::new_v4()));
            let config = Config {
                data_dir: root.join("data"),
                state_dir: root.join("state"),
                signaling_host: String::new(),
            };
            let (state, rows) = Servers::fixture();
            let context = egui::Context::default();
            crate::theme::apply(&context);
            context.style_mut(|style| style.animation_time = 0.);
            let mut harness = Self {
                state,
                rows,
                context,
                config,
                root,
                time: 0.,
                painted: Vec::new(),
            };
            harness.frame(vec![]);
            harness.frame(vec![]);
            harness
        }
        fn frame(&mut self, events: Vec<egui::Event>) {
            self.time += 0.2;
            let output = self.context.run(
                egui::RawInput {
                    time: Some(self.time),
                    focused: true,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000., 1000.),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        self.state.show(ui, Some(&self.config), &self.rows, true);
                    });
                    self.state.dialogs(ctx, Some(&self.config));
                },
            );
            fn collect(shape: &egui::Shape, values: &mut Vec<(String, egui::Pos2)>) {
                match shape {
                    egui::Shape::Text(text) => values.push((
                        text.galley.job.text.clone(),
                        text.pos + text.galley.size() * 0.5,
                    )),
                    egui::Shape::Vec(shapes) => {
                        for shape in shapes {
                            collect(shape, values);
                        }
                    }
                    _ => {}
                }
            }
            self.painted.clear();
            for shape in output.shapes {
                collect(&shape.shape, &mut self.painted);
            }
        }
        fn click(&mut self, text: &str, index: usize) {
            let pos = self
                .painted
                .iter()
                .filter(|(label, _)| label == text)
                .nth(index)
                .unwrap_or_else(|| panic!("missing {text}: {:?}", self.painted))
                .1;
            let button = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            self.frame(vec![egui::Event::PointerMoved(pos), button(true)]);
            self.frame(vec![button(false)]);
        }
        fn replace_alias(&mut self, current: &str, replacement: &str) {
            self.click(current, 0);
            let key = |key, pressed, modifiers| egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed,
                repeat: false,
                modifiers,
            };
            let command = egui::Modifiers { ctrl: true, command: true, ..Default::default() };
            let mut events = vec![key(egui::Key::A, true, command), key(egui::Key::A, false, command)];
            if replacement.is_empty() {
                events.extend([
                    key(egui::Key::Backspace, true, Default::default()),
                    key(egui::Key::Backspace, false, Default::default()),
                ]);
            } else {
                events.push(egui::Event::Text(replacement.into()));
            }
            self.frame(events);
            self.frame(vec![]);
        }
        fn wait_saved(&mut self, generation: u64) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while self.state.labels.saved_generation() < generation || !self.state.labels.ready() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "inline alias save did not finish"
                );
                self.frame(vec![]);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            self.frame(vec![]);
        }
    }
    impl Drop for FormHarness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
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
        assert!(index("家里的服务器") < index("笔记本电脑"));
        assert!(index("笔记本电脑") < index("办公室电脑"));
        assert!(!labels
            .iter()
            .any(|label| label == "我的设备" || label == "工作设备" || label.contains("wss://")));
        assert_eq!(
            labels
                .iter()
                .filter(|label| *label == "在线设备（1）")
                .count(),
            2
        );
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
    fn inline_alias_drafts_keep_unsaved_edits_across_refresh_and_confirm_only_a_successful_save() {
        let mut draft = AliasDraft {
            value: "unsaved draft".into(),
            baseline: "saved alias".into(),
            saving: Some((4, "unsaved draft".into())),
        };
        draft.sync("saved alias", 4, true);
        assert_eq!(draft.value, "unsaved draft");
        assert!(draft.dirty("saved alias") && draft.saving.is_none());
        draft.sync("alias changed in another window", 4, true);
        assert_eq!(draft.value, "unsaved draft");
        draft.saving = Some((4, "unsaved draft".into()));
        draft.sync("unsaved draft", 5, true);
        assert_eq!(draft.baseline, "unsaved draft");
        assert!(!draft.dirty("unsaved draft"));
        draft.value.clear();
        assert!(draft.dirty("unsaved draft"));
    }
    #[test]
    fn inline_save_requires_an_edit_and_clearing_an_alias_restores_the_default() {
        let mut harness = FormHarness::new();
        harness.click("保存", 0);
        assert_eq!(harness.state.labels.saved_generation(), 0);
        assert!(!harness
            .config
            .at("connection-service-aliases.json")
            .exists());
        harness.replace_alias("家里的服务器", "新的别名");
        harness.click("保存", 0);
        harness.wait_saved(1);
        assert_eq!(
            harness.state.labels.alias(&harness.rows[0].address),
            "新的别名"
        );
        harness.replace_alias("新的别名", "");
        harness.click("保存", 0);
        harness.wait_saved(2);
        assert!(harness
            .state
            .labels
            .alias(&harness.rows[0].address)
            .is_empty());
        assert!(harness
            .state
            .labels
            .display_name(&harness.rows[0].address)
            .starts_with("203.0.113.10:9443"));
    }
    #[test]
    fn server_details_start_collapsed_and_remember_each_servers_expansion() {
        let mut harness = FormHarness::new();
        let contains =
            |h: &FormHarness, text: &str| h.painted.iter().any(|(label, _)| label == text);
        assert!(!contains(&harness, "我的设备") && !contains(&harness, "工作设备"));
        harness.click("服务器详情", 0);
        harness.frame(vec![]);
        assert!(contains(&harness, "我的设备") && !contains(&harness, "工作设备"));
        assert!(contains(&harness, &harness.rows[0].address));
        harness.frame(vec![]);
        assert!(contains(&harness, "我的设备") && !contains(&harness, "工作设备"));
        harness.click("服务器详情", 1);
        harness.frame(vec![]);
        harness.click("服务器详情", 0);
        harness.frame(vec![]);
        assert!(!contains(&harness, "我的设备") && contains(&harness, "工作设备"));
    }
}
