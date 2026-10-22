//! Devices are the primary object. Sharing settings and connection services
//! have their own pages and never ask for a per-device shared key file.
use super::theme;
use eframe::egui::{self, RichText};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use uu_core::{
    device_book::{Book, Destination, Encoding, Profile, Route, Service, Sharing, Store},
    device_internet::EndpointStatus,
    device_trust::TrustStore,
    native::{CaptureMonitor, HostStatus},
};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Page {
    #[default]
    Devices,
    Sharing,
    Services,
    Assistance,
}
pub(super) enum Action {
    Open(String),
    Login,
    CancelLogin,
    StartSharing,
    Assist(uu_core::assistance::Request),
}
pub(super) struct Info {
    pub logged_in: bool,
    pub uu_id: Option<String>,
    pub qr: Option<egui::TextureId>,
    pub login_status: String,
    pub login_busy: bool,
    pub nearby: Vec<(String, String)>,
}
#[derive(Clone)]
struct DeviceEditor {
    error: String,
    profile: Option<String>,
    route: Option<String>,
    name: String,
    kind: u8,
    address: String,
    device: String,
    service: String,
}
#[derive(Clone)]
struct ServiceEditor {
    error: String,
    value: Service,
    token: String,
    credential: String,
    certificate: String,
}

pub(super) struct Workspace {
    pub page: Page,
    pub book: Book,
    pub host: super::sharing_ui::HostUi,
    pub store: Option<Store>,
    pub trust: Option<TrustStore>,
    pub notice: String,
    draft: Sharing,
    monitors: Vec<CaptureMonitor>,
    monitor_error: String,
    editor: Option<DeviceEditor>,
    service_editor: Option<ServiceEditor>,
    details: Option<String>,
    search: String,
    assist_text: String,
    assist_id: String,
    assist_code: String,
    assist_error: String,
    last_read: Instant,
    pub resume_connect: Option<String>,
    pub pairing_visible: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Harness {
        workspace: Workspace,
        ctx: egui::Context,
        root: PathBuf,
        time: f64,
        texts: Vec<(String, egui::Pos2)>,
    }
    impl Harness {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("eclipse-workspace-ui-{}", uuid::Uuid::new_v4()));
            let config = uu_core::Config {
                data_dir: root.join("data"),
                state_dir: root.join("state"),
                signaling_host: "unreachable.example.invalid".into(),
            };
            let workspace = Workspace::load(Some(&config), (47998, 47999));
            assert!(workspace.store.is_some(), "{}", workspace.notice);
            let ctx = egui::Context::default();
            theme::apply(&ctx);
            Self {
                workspace,
                ctx,
                root,
                time: 0.,
                texts: Vec::new(),
            }
        }
        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<Action> {
            self.time += 0.02;
            let mut actions = Vec::new();
            let info = Info {
                logged_in: false,
                uu_id: None,
                qr: None,
                login_status: String::new(),
                login_busy: false,
                nearby: Vec::new(),
            };
            let output = self.ctx.run(
                egui::RawInput {
                    time: Some(self.time),
                    focused: true,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200., 1000.),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        actions.extend(self.workspace.show(ui, &info));
                    });
                    actions.extend(self.workspace.dialogs(ctx));
                },
            );
            fn collect(shape: &egui::Shape, texts: &mut Vec<(String, egui::Pos2)>) {
                match shape {
                    egui::Shape::Text(text) => texts.push((
                        text.galley.text().to_owned(),
                        text.pos + text.galley.size() * 0.5,
                    )),
                    egui::Shape::Vec(shapes) => {
                        for s in shapes {
                            collect(s, texts);
                        }
                    }
                    _ => {}
                }
            }
            self.texts.clear();
            for shape in output.shapes {
                collect(&shape.shape, &mut self.texts);
            }
            actions
        }
        fn click(&mut self, label: &str) -> Vec<Action> {
            for _ in 0..3 {
                self.frame(Vec::new());
            }
            let position = self
                .texts
                .iter()
                .find(|(text, _)| text == label)
                .unwrap_or_else(|| panic!("missing button {label}: {:?}", self.texts))
                .1;
            let event = |pressed| egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            self.frame(vec![egui::Event::PointerMoved(position), event(true)]);
            self.frame(vec![event(false)])
        }
    }
    impl Drop for Harness {
        fn drop(&mut self) {
            self.workspace.host.stop();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn device_editor_retains_failed_input_and_saves_multiple_routes_without_duplicates_or_key_files(
    ) {
        let mut h = Harness::new();
        h.click("添加第一台设备");
        let editor = h.workspace.editor.as_mut().unwrap();
        editor.name = "Generated host".into();
        editor.address = "invalid:::".into();
        h.click("仅保存");
        let editor = h
            .workspace
            .editor
            .as_ref()
            .expect("invalid save keeps the form open");
        assert_eq!(editor.address, "invalid:::");
        assert!(!editor.error.is_empty());
        assert!(h.workspace.book.devices.is_empty());
        h.workspace.editor.as_mut().unwrap().address = "192.0.2.50:47998".into();
        h.click("仅保存");
        assert!(h.workspace.editor.is_none());
        let id = h.workspace.book.devices[0].id.clone();
        for _ in 0..2 {
            h.workspace.new_device(Some(id.clone()), None);
            let editor = h.workspace.editor.as_mut().unwrap();
            editor.kind = 1;
            editor.device = "generated-uu-device".into();
            h.click("仅保存");
        }
        assert_eq!(h.workspace.book.device(&id).unwrap().routes.len(), 2);
        let service = Service {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Generated server".into(),
            address: "signal.example.invalid:8443".into(),
            token_file: "generated.token".into(),
            certificate: None,
            host_enabled: false,
            published_name: "generated-host".into(),
        };
        h.workspace
            .store
            .as_ref()
            .unwrap()
            .save_service(service.clone())
            .unwrap();
        h.workspace.refresh();
        h.workspace.new_device(Some(id.clone()), None);
        let editor = h.workspace.editor.as_mut().unwrap();
        editor.kind = 2;
        editor.service = service.id;
        editor.device = "other-host".into();
        let actions = h.click("保存并连接");
        assert!(matches!(&actions[..],[Action::Open(opened)] if opened==&id));
        assert_eq!(h.workspace.book.device(&id).unwrap().routes.len(), 3);
        assert!(!h
            .texts
            .iter()
            .any(|(s, _)| s.contains("配对文件") || s.contains("配对 key")));
    }
    #[test]
    fn server_form_reports_missing_credentials_without_discarding_fields() {
        let mut h = Harness::new();
        h.workspace.page = Page::Services;
        h.click("添加服务器");
        let editor = h.workspace.service_editor.as_mut().unwrap();
        editor.value.name = "Home".into();
        editor.value.address = "signal.example.invalid:8443".into();
        h.click("保存服务器");
        let editor = h.workspace.service_editor.as_ref().unwrap();
        assert_eq!(editor.value.name, "Home");
        assert!(!editor.error.is_empty());
        assert!(h.workspace.book.services.is_empty());
        h.workspace.service_editor.as_mut().unwrap().credential = "ab".repeat(32);
        h.click("保存服务器");
        assert!(h.workspace.service_editor.is_none());
        assert_eq!(h.workspace.book.services.len(), 1);
        let path = &h.workspace.book.services[0].token_file;
        assert_eq!(std::fs::read_to_string(path).unwrap(), "ab".repeat(32));
        h.frame(Vec::new());
        assert!(!h.texts.iter().any(|(s, _)| s.contains(&"ab".repeat(32))));
    }
    #[test]
    fn assistance_import_uses_its_own_invitation_and_never_creates_a_device_grant() {
        let mut h = Harness::new();
        h.workspace.page = Page::Assistance;
        h.workspace.assist_text = "设备ID：123456789\n设备验证码：TEST79".into();
        h.click("识别分享文本");
        assert_eq!(h.workspace.assist_id, "123456789");
        assert!(h.workspace.assist_text.is_empty());
        let actions = h.click("发起远程协助");
        assert!(
            matches!(&actions[..],[Action::Assist(request)] if request.id=="123456789" && request.code()=="TEST79")
        );
        assert!(h.workspace.book.devices.is_empty());
        assert!(h
            .workspace
            .trust
            .as_ref()
            .unwrap()
            .peers()
            .unwrap()
            .is_empty());
        assert!(!h.texts.iter().any(|(s, _)| s.contains("TEST79")));
    }
}
fn primary(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).color(egui::Color32::WHITE)).fill(theme::ACCENT))
}
fn card(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(theme::SURFACE)
        .stroke(egui::Stroke::new(1., theme::BORDER))
        .rounding(12.)
        .inner_margin(18.)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            contents(ui);
        });
    ui.add_space(12.);
}
fn field(ui: &mut egui::Ui, label: &str, text: &mut String, hint: &str) {
    ui.label(label);
    ui.add(
        egui::TextEdit::singleline(text)
            .desired_width(f32::INFINITY)
            .hint_text(hint),
    );
    ui.add_space(6.);
}
impl Workspace {
    pub fn load(config: Option<&uu_core::Config>, ports: (u16, u16)) -> Self {
        let loaded = (|| -> anyhow::Result<_> {
            let config = config.ok_or_else(|| anyhow::anyhow!("无法确定设备配置目录"))?;
            let store = Store::open(config.at("device-trust"))?;
            let trust = TrustStore::open(config.at("device-trust"))?;
            let old = store.read()?;
            if !old.legacy_imported {
                let legacy_path = config.at("native-lan/paired-devices.json");
                let legacy = if legacy_path.exists() {
                    Some(uu_core::native_lan_store::Devices::open(&legacy_path)?)
                } else {
                    None
                };
                let internet_path = config.at("internet/settings.json");
                let internet = if internet_path.exists() {
                    Some(super::internet_ui::read_profile(&internet_path)?)
                } else {
                    None
                };
                store.edit(|book| {
                    if book.legacy_imported {
                        return Ok(());
                    }
                    book.sharing.service_port = ports.0;
                    book.sharing.discovery_port = ports.1;
                    book.sharing.name = std::env::var("COMPUTERNAME")
                        .ok()
                        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
                        .map(|s| {
                            s.trim()
                                .chars()
                                .filter(|c| !c.is_control())
                                .take(64)
                                .collect::<String>()
                        })
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "Eclipse".into());
                    if let Some(legacy) = &legacy {
                        for device in &legacy.devices {
                            book.devices.push(Profile::new(
                                &device.name,
                                Destination::Ip {
                                    address: device.endpoint.clone(),
                                    legacy_device: Some(device.id.clone()),
                                },
                            ));
                        }
                        for address in &legacy.manual {
                            if !legacy.devices.iter().any(|d| &d.endpoint == address) {
                                book.devices.push(Profile::new(
                                    "手动设备",
                                    Destination::Ip {
                                        address: address.clone(),
                                        legacy_device: None,
                                    },
                                ));
                            }
                        }
                    }
                    if let Some(settings) = &internet {
                        match settings.provider {
                            uu_core::internet::Provider::Uu => {
                                book.sharing.uu_enabled = true;
                                if !settings.device.is_empty() {
                                    book.devices.push(Profile::new(
                                        "UU 设备",
                                        Destination::Uu {
                                            device: settings.device.clone(),
                                        },
                                    ));
                                }
                            }
                            uu_core::internet::Provider::SelfHosted => {
                                if !settings.server.trim().is_empty() {
                                    let service = Service {
                                        id: uuid::Uuid::new_v4().to_string(),
                                        name: "已导入的服务器".into(),
                                        address: settings.server.clone(),
                                        token_file: settings.server_token.clone(),
                                        certificate: settings.server_ca.clone(),
                                        host_enabled: false,
                                        published_name: format!(
                                            "device-{}",
                                            &trust.card("Eclipse").id()[..12]
                                        ),
                                    };
                                    if !settings.device.is_empty() {
                                        book.devices.push(Profile::new(
                                            "服务器设备",
                                            Destination::Vps {
                                                service: service.id.clone(),
                                                device: settings.device.clone(),
                                            },
                                        ));
                                    }
                                    book.services.push(service);
                                }
                            }
                        }
                    }
                    book.legacy_imported = true;
                    Ok(())
                })?;
            }
            let book = store.read()?;
            Ok((store, trust, book))
        })();
        let (store, trust, book, notice) = match loaded {
            Ok((s, t, b)) => (Some(s), Some(t), b, String::new()),
            Err(e) => (
                None,
                None,
                Book::default(),
                format!("无法读取设备配置：{e:#}。原文件已保留。"),
            ),
        };
        let draft = book.sharing.clone();
        Self {
            page: if std::env::var_os("ECLIPSE_SHOW_SHARING").is_some() {
                Page::Sharing
            } else {
                Page::Devices
            },
            book,
            host: Default::default(),
            store,
            trust,
            notice,
            draft,
            monitors: Vec::new(),
            monitor_error: String::new(),
            editor: None,
            service_editor: None,
            details: None,
            search: String::new(),
            assist_text: String::new(),
            assist_id: String::new(),
            assist_code: String::new(),
            assist_error: String::new(),
            last_read: Instant::now(),
            resume_connect: None,
            pairing_visible: false,
        }
    }
    pub fn poll(&mut self, ctx: &egui::Context, window: isize) {
        self.host.poll(ctx, window);
        if self.last_read.elapsed() >= Duration::from_millis(500) {
            self.last_read = Instant::now();
            self.refresh();
        }
    }
    fn refresh(&mut self) {
        if let Some(store) = &self.store {
            match store.read() {
                Ok(book) => {
                    if serde_json::to_vec(&self.draft).ok()
                        == serde_json::to_vec(&self.book.sharing).ok()
                    {
                        self.draft = book.sharing.clone();
                    }
                    self.book = book;
                }
                Err(e) => self.notice = format!("无法读取设备配置：{e:#}，原文件已保留"),
            }
        }
    }
    fn save_sharing(&mut self) -> bool {
        let result = self
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("设备配置不可用"))
            .and_then(|s| s.sharing(self.draft.clone()));
        let saved = result.is_ok();
        self.notice = match result {
            Ok(()) => "共享设置已保存".into(),
            Err(e) => format!("无法保存：{e:#}"),
        };
        self.refresh();
        saved
    }
    fn new_device(&mut self, profile: Option<String>, route: Option<String>) {
        let mut editor = DeviceEditor {
            error: String::new(),
            profile,
            route,
            name: String::new(),
            kind: 0,
            address: String::new(),
            device: String::new(),
            service: self
                .book
                .services
                .first()
                .map(|s| s.id.clone())
                .unwrap_or_default(),
        };
        if let Some(id) = &editor.profile {
            if let Ok(profile) = self.book.device(id) {
                editor.name = profile.name.clone();
                if let Some(id) = &editor.route {
                    if let Some(route) = profile.routes.iter().find(|r| &r.id == id) {
                        match &route.destination {
                            Destination::Ip { address, .. } => editor.address = address.clone(),
                            Destination::Uu { device } => {
                                editor.kind = 1;
                                editor.device = device.clone();
                            }
                            Destination::Vps { service, device } => {
                                editor.kind = 2;
                                editor.service = service.clone();
                                editor.device = device.clone();
                            }
                            Destination::LegacyInternet { .. } => {}
                        }
                    }
                }
            }
        }
        self.editor = Some(editor);
    }
    fn save_device(&mut self, editor: DeviceEditor) -> anyhow::Result<String> {
        let destination = match editor.kind {
            0 => Destination::Ip {
                address: uu_core::native_discovery::endpoint(&editor.address)?,
                legacy_device: None,
            },
            1 => Destination::Uu {
                device: editor.device.trim().into(),
            },
            _ => Destination::Vps {
                service: editor.service,
                device: editor.device.trim().into(),
            },
        };
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("设备配置不可用"))?;
        let mut profile = if let Some(id) = editor.profile {
            store.read()?.device(&id)?.clone()
        } else {
            let name = if editor.name.trim().is_empty() {
                "远端设备"
            } else {
                editor.name.trim()
            };
            Profile::new(name, destination.clone())
        };
        if !editor.name.trim().is_empty() {
            profile.name = editor.name.trim().into();
        }
        if let Some(id) = editor.route {
            anyhow::ensure!(
                !profile
                    .routes
                    .iter()
                    .any(|r| r.id != id && r.destination.same_target(&destination)),
                "这个连接地址已经存在"
            );
            let route = profile
                .routes
                .iter_mut()
                .find(|r| r.id == id)
                .ok_or_else(|| anyhow::anyhow!("连接地址已移除"))?;
            // A previously paired IP may change address but retains its old
            // pinned identity until the v2 migration has been verified.
            let mut updated = destination;
            if let (
                Destination::Ip {
                    legacy_device: old, ..
                },
                Destination::Ip {
                    legacy_device: new, ..
                },
            ) = (&route.destination, &mut updated)
            {
                *new = old.clone();
            }
            route.destination = updated;
        } else if let Some(route) = profile
            .routes
            .iter()
            .find(|r| r.destination.same_target(&destination))
        {
            profile.selected_route = route.id.clone();
        } else {
            let route = Route::new(destination);
            profile.selected_route = route.id.clone();
            profile.routes.push(route);
        }
        let id = profile.id.clone();
        store.save_device(profile)?;
        self.refresh();
        Ok(id)
    }
    pub fn show(&mut self, ui: &mut egui::Ui, info: &Info) -> Vec<Action> {
        let mut actions = Vec::new();
        if !self.notice.is_empty() {
            card(ui, |ui| {
                ui.label(&self.notice);
            });
        }
        if !self.host.notice.is_empty() {
            card(ui, |ui| {
                ui.label(&self.host.notice);
            });
        }
        match self.page {
            Page::Devices => self.devices(ui, info, &mut actions),
            Page::Sharing => self.sharing(ui, &mut actions),
            Page::Services => self.services(ui, info, &mut actions),
            Page::Assistance => {
                ui.heading("远程协助");
                ui.weak("通过临时邀请帮助另一台设备");
                ui.add_space(18.);
                self.assistance(ui, &mut actions);
            }
        }
        actions
    }
    fn devices(&mut self, ui: &mut egui::Ui, info: &Info, actions: &mut Vec<Action>) {
        if let Some(id) = self.details.clone() {
            self.device_details(ui, &id, actions);
            return;
        }
        ui.horizontal(|ui| {
            ui.heading("设备");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if primary(ui, "添加设备").clicked() {
                    self.new_device(None, None);
                }
            });
        });
        ui.weak("配对一次，以后选择设备即可连接。");
        ui.add_space(16.);
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text("搜索设备")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(12.);
        let profiles: Vec<_> = self
            .book
            .devices
            .iter()
            .filter(|p| {
                p.redirect.is_none() && p.name.to_lowercase().contains(&self.search.to_lowercase())
            })
            .cloned()
            .collect();
        if profiles.is_empty() {
            card(ui, |ui| {
                ui.heading("把你的设备放在一起");
                ui.label("添加 IP、UU 或服务器中的设备，连接地址和配对会自动保存。");
                ui.add_space(12.);
                if primary(ui, "添加第一台设备").clicked() {
                    self.new_device(None, None);
                }
                if ui.button("共享这台电脑").clicked() {
                    self.page = Page::Sharing;
                }
            });
        }
        for mut profile in profiles {
            let mut changed = false;
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&profile.name).size(18.).strong());
                    let paired = profile.peer.is_some();
                    ui.label(
                        RichText::new(if paired {
                            "已配对"
                        } else {
                            "首次连接需确认"
                        })
                        .small()
                        .color(if paired {
                            theme::SUCCESS
                        } else {
                            theme::MUTED
                        }),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if primary(ui, "连接").clicked() {
                            actions.push(Action::Open(profile.id.clone()));
                        }
                        if ui.button("详情").clicked() {
                            self.details = Some(profile.id.clone());
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.label("连接方式");
                    egui::ComboBox::from_id_salt(("route", &profile.id))
                        .selected_text(
                            profile
                                .route()
                                .map(|r| r.destination.label())
                                .unwrap_or("请选择"),
                        )
                        .show_ui(ui, |ui| {
                            for route in &profile.routes {
                                changed |= ui
                                    .selectable_value(
                                        &mut profile.selected_route,
                                        route.id.clone(),
                                        format!(
                                            "{} · {}",
                                            route.destination.label(),
                                            route.destination.target()
                                        ),
                                    )
                                    .changed();
                            }
                        });
                    if let Ok(route) = profile.route() {
                        ui.weak(route.destination.target());
                    }
                });
            });
            if changed {
                if let Some(store) = &self.store {
                    if let Err(e) = store.save_device(profile) {
                        self.notice = format!("无法保存连接方式：{e}");
                    }
                }
                self.refresh();
            }
        }
        let nearby: Vec<_> = info
            .nearby
            .iter()
            .filter(|(_, address)| {
                !self.book.devices.iter().any(|p| {
                    p.routes.iter().any(
                        |r| matches!(&r.destination,Destination::Ip{address:a,..} if a==address),
                    )
                })
            })
            .cloned()
            .collect();
        if !nearby.is_empty() {
            ui.add_space(12.);
            ui.label(RichText::new("附近的设备").strong());
            ui.weak("发现设备不代表已经授权，首次连接仍需被控端确认。");
            ui.add_space(8.);
            for (name, address) in nearby {
                card(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.strong(&name);
                            ui.weak(&address);
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("添加").clicked() {
                                let profile = Profile::new(
                                    &name,
                                    Destination::Ip {
                                        address,
                                        legacy_device: None,
                                    },
                                );
                                if let Some(store) = &self.store {
                                    match store.save_device(profile) {
                                        Ok(()) => self.refresh(),
                                        Err(e) => self.notice = format!("无法添加：{e}"),
                                    }
                                }
                            }
                        });
                    });
                });
            }
        }
    }
    fn device_details(&mut self, ui: &mut egui::Ui, id: &str, actions: &mut Vec<Action>) {
        if ui.button("‹ 返回设备").clicked() {
            self.details = None;
            return;
        }
        let profile = match self.book.device(id) {
            Ok(p) => p.clone(),
            Err(e) => {
                self.notice = e.to_string();
                self.details = None;
                return;
            }
        };
        ui.heading(&profile.name);
        ui.add_space(14.);
        card(ui, |ui| {
            ui.strong("设备身份");
            ui.add_space(8.);
            if let Some(peer) = &profile.peer {
                ui.label(RichText::new("已完成配对").color(theme::SUCCESS));
                ui.weak("设备公钥指纹");
                ui.monospace(peer);
                if ui.small_button("复制指纹").clicked() {
                    ui.ctx().copy_text(peer.clone());
                }
            } else {
                ui.label("首次连接时，被控端输入控制端显示的短码即可保存授权。");
            }
            ui.add_space(10.);
            if primary(ui, "连接此设备").clicked() {
                actions.push(Action::Open(profile.id.clone()));
            }
        });
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("连接地址");
                if ui.button("添加地址").clicked() {
                    self.new_device(Some(profile.id.clone()), None);
                }
            });
            ui.add_space(8.);
            let mut remove = None;
            for route in &profile.routes {
                ui.push_id(&route.id, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "{} · {}",
                            route.destination.label(),
                            route.destination.target()
                        ));
                        if ui.small_button("编辑").clicked() {
                            self.new_device(Some(profile.id.clone()), Some(route.id.clone()));
                        }
                        if ui
                            .add_enabled(
                                profile.routes.len() > 1,
                                egui::Button::new("移除").small(),
                            )
                            .clicked()
                        {
                            remove = Some(route.id.clone());
                        }
                    });
                });
            }
            if let Some(id) = remove {
                let mut updated = profile.clone();
                updated.routes.retain(|r| r.id != id);
                if updated.selected_route == id {
                    updated.selected_route = updated.routes[0].id.clone();
                }
                if let Some(store) = &self.store {
                    match store.save_device(updated) {
                        Ok(()) => self.refresh(),
                        Err(e) => self.notice = format!("无法移除地址：{e}"),
                    }
                }
            }
        });
        ui.add_space(12.);
        if ui.button("移除本地设备记录").clicked() {
            if let Some(store) = &self.store {
                match store.forget(&profile.id) {
                    Ok(()) => {
                        self.details = None;
                        self.refresh();
                    }
                    Err(e) => self.notice = format!("无法移除：{e}"),
                }
            }
        }
        ui.weak("移除本地记录不会撤销被控端已授予的权限。授权在被控端的“共享本机”中管理。");
    }
    fn refresh_monitors(&mut self) {
        match uu_core::native::monitors() {
            Ok(monitors) => {
                self.monitors = monitors;
                self.monitor_error = if self.monitors.is_empty() {
                    "当前会话没有可共享的屏幕".into()
                } else {
                    String::new()
                };
            }
            Err(e) => self.monitor_error = format!("无法读取屏幕：{e}"),
        }
    }
    fn sharing(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        if self.monitors.is_empty() && self.monitor_error.is_empty() {
            self.refresh_monitors();
        }
        ui.heading("共享本机");
        ui.weak("一套画面与权限设置，供所有连接入口使用。");
        ui.add_space(16.);
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(if self.host.running() {
                        if self.host.stopping() {
                            "正在停止共享…"
                        } else {
                            "本机共享已开启"
                        }
                    } else {
                        "本机共享已关闭"
                    })
                    .size(18.)
                    .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.host.running() {
                        if ui
                            .add_enabled(!self.host.stopping(), egui::Button::new("停止共享"))
                            .clicked()
                        {
                            self.host.stop();
                        }
                    } else if ui
                        .add_enabled(
                            self.store.is_some() && !self.monitors.is_empty(),
                            egui::Button::new(
                                RichText::new("开始共享").color(egui::Color32::WHITE),
                            )
                            .fill(theme::ACCENT),
                        )
                        .clicked()
                    {
                        if self.save_sharing() {
                            actions.push(Action::StartSharing);
                        }
                    }
                });
            });
            if self.host.running() {
                ui.add_space(12.);
                ui.horizontal(|ui| {
                    if self.host.pairing_open() {
                        ui.label(RichText::new("正在允许新设备配对").color(theme::SUCCESS));
                        if ui.button("停止接收配对").clicked() {
                            self.host.close_pairing();
                        }
                    } else if primary(ui, "允许新设备配对").clicked() {
                        self.host.allow_pairing();
                    }
                    if self
                        .host
                        .access
                        .as_ref()
                        .is_some_and(|a| a.sessions.active_peer().is_some())
                        && ui.button("断开当前串流").clicked()
                    {
                        self.host.disconnect();
                    }
                });
                if self
                    .host
                    .access
                    .as_ref()
                    .is_some_and(|a| a.sessions.active_peer().is_some())
                {
                    if let Some(HostStatus::Streaming {
                        width,
                        height,
                        fps,
                        encoder,
                        audio,
                    }) = self.host.stream_status()
                    {
                        ui.weak(format!(
                            "{width}×{height} · {fps} FPS · {encoder} · {audio}"
                        ));
                    }
                }
            }
        });
        if self.monitors.is_empty() && self.monitor_error.is_empty() {
            self.refresh_monitors();
        }
        let locked = self.host.running();
        card(ui, |ui| {
            ui.strong("画面与声音");
            ui.add_space(10.);
            ui.add_enabled_ui(!locked, |ui| {
                field(ui, "本机名称", &mut self.draft.name, "设备名称");
                ui.label("共享屏幕");
                egui::ComboBox::from_id_salt("shared-monitor")
                    .selected_text(if self.draft.monitor.is_empty() {
                        "主显示器"
                    } else {
                        &self.draft.monitor
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.draft.monitor, String::new(), "主显示器");
                        for monitor in &self.monitors {
                            ui.selectable_value(
                                &mut self.draft.monitor,
                                monitor.name.clone(),
                                format!("{} · {}×{}", monitor.name, monitor.width, monitor.height),
                            );
                        }
                    });
                if !self.monitor_error.is_empty() {
                    ui.colored_label(theme::DANGER, &self.monitor_error);
                }
                if ui.small_button("刷新屏幕").clicked() {
                    self.refresh_monitors();
                }
                ui.horizontal(|ui| {
                    ui.label("帧率");
                    ui.add(
                        egui::DragValue::new(&mut self.draft.fps)
                            .range(1..=60)
                            .suffix(" FPS"),
                    );
                    ui.add_space(18.);
                    ui.label("码率");
                    ui.add(
                        egui::DragValue::new(&mut self.draft.bitrate_mbps)
                            .range(1..=100)
                            .suffix(" Mbps"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("编码加速");
                    egui::ComboBox::from_id_salt("shared-encoder")
                        .selected_text(match self.draft.encoding {
                            Encoding::Auto => "自动",
                            Encoding::Hardware => "硬件",
                            Encoding::Software => "软件",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.draft.encoding, Encoding::Auto, "自动");
                            ui.selectable_value(
                                &mut self.draft.encoding,
                                Encoding::Hardware,
                                "硬件",
                            );
                            ui.selectable_value(
                                &mut self.draft.encoding,
                                Encoding::Software,
                                "软件",
                            );
                        });
                });
                ui.checkbox(&mut self.draft.audio, "共享系统声音");
                ui.checkbox(&mut self.draft.control, "允许已授权设备控制键盘和鼠标");
                ui.checkbox(&mut self.draft.clipboard, "允许双方启用文本剪贴板");
            });
            if locked {
                ui.weak("停止共享后可修改设置。");
            }
        });
        card(ui, |ui| {
            ui.strong("连接入口");
            ui.weak("IP 直连默认启用，其它入口按需开启。");
            ui.add_space(10.);
            if self.host.running() {
                for (name, status) in self.host.statuses() {
                    ui.horizontal(|ui| {
                        ui.label(name);
                        let color = match status {
                            EndpointStatus::Ready | EndpointStatus::Streaming => theme::SUCCESS,
                            EndpointStatus::Error(_) => theme::DANGER,
                            _ => theme::MUTED,
                        };
                        ui.label(RichText::new(status.label()).color(color));
                    });
                }
            }
            ui.add_enabled_ui(!locked, |ui| {
                ui.checkbox(&mut self.draft.uu_enabled, "使用 UU 账号连接服务");
            });
            for service in &self.book.services {
                ui.horizontal(|ui| {
                    ui.label(&service.name);
                    ui.weak(if service.host_enabled {
                        format!("已启用 · 本机名称 {}", service.published_name)
                    } else {
                        "未用于共享本机".into()
                    });
                });
            }
            if ui.button("管理连接服务").clicked() {
                self.page = Page::Services;
            }
            ui.collapsing("IP 地址与高级网络", |ui| {
                ui.add_enabled_ui(!locked, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("服务端口 TCP");
                        ui.add(egui::DragValue::new(&mut self.draft.service_port).range(1..=65535));
                        ui.label("发现端口 UDP");
                        ui.add(
                            egui::DragValue::new(&mut self.draft.discovery_port).range(1..=65535),
                        );
                    });
                });
                if let Ok(addresses) = super::native_interfaces::enumerate() {
                    for address in addresses.into_iter().filter(|a| a.ip.is_ipv4()) {
                        let endpoint = format!("{}:{}", address.ip, self.book.sharing.service_port);
                        ui.horizontal(|ui| {
                            ui.monospace(&endpoint);
                            if ui.small_button("复制").clicked() {
                                ui.ctx().copy_text(endpoint);
                            }
                        });
                    }
                }
                ui.weak(
                    "不同网段可手动填写可达地址；自动发现通常不跨路由。应用不会自动修改防火墙。",
                );
            });
        });
        if !locked && primary(ui, "保存共享设置").clicked() {
            self.save_sharing();
        }
        ui.add_space(14.);
        self.grants(ui);
    }
    fn grants(&mut self, ui: &mut egui::Ui) {
        let peers = match &self.trust {
            Some(store) => match store.peers() {
                Ok(peers) => peers,
                Err(e) => {
                    ui.colored_label(theme::DANGER, format!("无法读取授权：{e}"));
                    return;
                }
            },
            None => return,
        };
        card(ui, |ui| {
            ui.strong("已授权的控制端");
            ui.weak("这些设备通过 IP、UU 或 VPS 连接时，都使用相同的身份和权限。");
            ui.add_space(10.);
            let active: Vec<_> = peers.iter().filter(|p| p.inbound.is_some()).collect();
            if active.is_empty() {
                ui.weak("还没有已授权的控制端。开启共享后，允许新设备配对即可添加。");
            }
            for peer in active {
                ui.push_id(peer.device.id(), |ui| {
                    ui.horizontal(|ui| {
                        ui.label(&peer.device.name);
                        let mut control = peer.inbound.is_some_and(|p| p.control);
                        if ui
                            .checkbox(&mut control, "允许控制")
                            .on_hover_text(
                                "关闭后仅能观看。修改权限会断开这台设备的当前串流，重连后生效。",
                            )
                            .changed()
                        {
                            let permission = if control {
                                uu_core::device_trust::Permissions::CONTROL
                            } else {
                                uu_core::device_trust::Permissions::VIEW
                            };
                            let result = self.trust.as_ref().unwrap().authorize(
                                peer.device.clone(),
                                permission,
                                None,
                            );
                            if result.is_ok() {
                                if let Some(access) = &self.host.access {
                                    access.sessions.revoke(&peer.device.id());
                                }
                            }
                            self.notice = match result {
                                Ok(()) => "权限已保存，重新连接后生效".into(),
                                Err(e) => format!("无法修改权限：{e}"),
                            };
                        }
                        if ui.small_button("撤销授权").clicked() {
                            let result = if let Some(access) = &self.host.access {
                                access.revoke(&peer.device.id())
                            } else {
                                self.trust.as_ref().unwrap().revoke(&peer.device.id())
                            };
                            self.notice = match result {
                                Ok(()) => "已撤销设备授权".into(),
                                Err(e) => format!("撤销失败：{e}"),
                            };
                        }
                    });
                });
            }
            for legacy in self
                .host
                .legacy_clients()
                .into_iter()
                .filter(|p| !peers.iter().any(|peer| peer.legacy_ids.contains(&p.id)))
            {
                ui.horizontal(|ui| {
                    ui.label(&legacy.name);
                    ui.weak("旧版 IP 配对");
                    if ui.small_button("撤销授权").clicked() {
                        self.host.revoke_legacy(&legacy.id);
                    }
                });
            }
        });
    }
    fn services(&mut self, ui: &mut egui::Ui, info: &Info, actions: &mut Vec<Action>) {
        ui.heading("连接服务");
        ui.weak("服务配置保存一次，供多台设备复用。");
        ui.add_space(16.);
        card(ui, |ui| {
            ui.strong("UU 账号");
            ui.add_space(8.);
            if info.logged_in {
                ui.label(RichText::new("已登录").color(theme::SUCCESS));
                if let Some(id) = &info.uu_id {
                    ui.horizontal(|ui| {
                        ui.label(format!("本机 UU 编号：{id}"));
                        if ui.small_button("复制").clicked() {
                            ui.ctx().copy_text(id.clone());
                        }
                    });
                }
            } else {
                ui.label("登录后可通过 UU 寻址原生设备。");
                if let Some(qr) = info.qr {
                    ui.image((qr, egui::vec2(180., 180.)));
                }
                if info.login_busy {
                    ui.spinner();
                }
                if !info.login_status.is_empty() {
                    ui.label(&info.login_status);
                }
                if primary(
                    ui,
                    if info.qr.is_some() {
                        "刷新登录二维码"
                    } else {
                        "登录 UU 账号"
                    },
                )
                .clicked()
                {
                    actions.push(Action::Login);
                }
                if info.login_busy && ui.button("取消登录").clicked() {
                    actions.push(Action::CancelLogin);
                }
            }
            ui.add_space(8.);
            ui.weak("本机是否接受 UU 连接，在“共享本机”中启用。对方仍需经过设备配对授权。");
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("自建服务器").strong());
            if ui
                .add_enabled(!self.host.running(), egui::Button::new("添加服务器"))
                .clicked()
            {
                self.edit_service(None);
            }
        });
        ui.add_space(10.);
        if self.book.services.is_empty() {
            card(ui, |ui| {
                ui.label("还没有服务器。你可以使用自己的 VPS 运行连接服务，再添加地址和访问凭据。");
            });
        }
        for service in self.book.services.clone() {
            ui.push_id(&service.id, |ui| {
                card(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&service.name).size(17.).strong());
                            ui.weak(&service.address);
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(!self.host.running(), egui::Button::new("编辑"))
                                .clicked()
                            {
                                self.edit_service(Some(service.clone()));
                            }
                            if ui
                                .add_enabled(!self.host.running(), egui::Button::new("移除"))
                                .clicked()
                            {
                                if let Some(store) = &self.store {
                                    match store.remove_service(&service.id) {
                                        Ok(()) => self.refresh(),
                                        Err(e) => self.notice = format!("无法移除：{e}"),
                                    }
                                }
                            }
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.label("本机在此服务的设备名称");
                        ui.monospace(&service.published_name);
                        if ui.small_button("复制").clicked() {
                            ui.ctx().copy_text(service.published_name.clone());
                        }
                    });
                    ui.weak(if service.host_enabled {
                        "共享开启时，会在此服务器发布本机"
                    } else {
                        "仅用于连接其它设备"
                    });
                });
            });
        }
        if self.host.running() {
            ui.weak("停止共享后可修改服务器配置。");
        }
    }
    fn edit_service(&mut self, service: Option<Service>) {
        let value = service.unwrap_or_else(|| Service {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            address: String::new(),
            token_file: PathBuf::new(),
            certificate: None,
            host_enabled: false,
            published_name: self
                .trust
                .as_ref()
                .map(|t| format!("device-{}", &t.card("Eclipse").id()[..12]))
                .unwrap_or_else(|| "my-desktop".into()),
        });
        self.service_editor = Some(ServiceEditor {
            error: String::new(),
            token: value.token_file.display().to_string(),
            credential: String::new(),
            certificate: value
                .certificate
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            value,
        });
    }
    fn assistance(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        card(ui, |ui| {
            ui.strong("UU 官方远程协助");
            ui.label("让对方在 UU 官方被控端开启远程协助，再发送设备 ID 和验证码。");
            ui.add_space(10.);
            ui.label("粘贴对方分享的完整文本");
            ui.add(
                egui::TextEdit::multiline(&mut self.assist_text)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text("设备ID：…\n设备验证码：…"),
            );
            if ui.button("识别分享文本").clicked() {
                match uu_core::assistance::Request::parse(&self.assist_text) {
                    Ok(request) => {
                        self.assist_id = request.id.clone();
                        self.assist_code = request.code().into();
                        self.assist_text.clear();
                        self.assist_error.clear();
                    }
                    Err(e) => self.assist_error = e.to_string(),
                }
            }
            ui.add_space(10.);
            field(ui, "设备 ID", &mut self.assist_id, "对方分享的数字 ID");
            ui.label("设备验证码");
            ui.add(
                egui::TextEdit::singleline(&mut self.assist_code)
                    .password(true)
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(10.);
            if !self.assist_error.is_empty() {
                ui.colored_label(theme::DANGER, &self.assist_error);
            }
            if primary(ui, "发起远程协助").clicked() {
                match uu_core::assistance::Request::new(&self.assist_id, &self.assist_code) {
                    Ok(request) => actions.push(Action::Assist(request)),
                    Err(e) => self.assist_error = e.to_string(),
                }
            }
            ui.weak("需登录 UU 账号。验证码只用于本次连接，不保存为设备授权。");
        });
        ui.collapsing("连接原生设备", |ui| {
            ui.label("被控端开启“允许新设备配对”，控制端添加 IP、UU 或 VPS 设备并连接，再把八位配对码交给被控端输入。");
            if primary(ui, "添加设备").clicked() {self.page=Page::Devices;self.new_device(None,None);}
            if ui.button("让别人连接本机").clicked(){self.page=Page::Sharing;}
        });
    }
    pub fn dialogs(&mut self, ctx: &egui::Context) -> Vec<Action> {
        let mut actions = Vec::new();
        self.host.dialogs(ctx);
        if let Some(mut editor) = self.editor.take() {
            let mut open = true;
            let mut save = false;
            let mut connect = false;
            let mut cancel = false;
            egui::Window::new(if editor.profile.is_none() {
                "添加设备"
            } else if editor.route.is_some() {
                "修改连接地址"
            } else {
                "添加连接地址"
            })
            .id("device-editor".into())
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.)
            .default_pos(ctx.screen_rect().center() - egui::vec2(240., 200.))
            .show(ctx, |ui| {
                if !editor.error.is_empty() {
                    ui.colored_label(theme::DANGER, &editor.error);
                }
                field(ui, "设备名称（可选）", &mut editor.name, "例如：工作电脑");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut editor.kind, 0, "IP 直连");
                    ui.selectable_value(&mut editor.kind, 1, "UU 账号");
                    ui.selectable_value(&mut editor.kind, 2, "自建服务器");
                });
                ui.add_space(12.);
                match editor.kind {
                    0 => field(ui, "目标地址", &mut editor.address, "主机 IP:端口"),
                    1 => field(
                        ui,
                        "目标 UU 设备编号",
                        &mut editor.device,
                        "被控端“连接服务”中的本机 UU 编号",
                    ),
                    _ => {
                        ui.label("连接服务器");
                        egui::ComboBox::from_id_salt("route-service")
                            .selected_text(
                                self.book
                                    .services
                                    .iter()
                                    .find(|s| s.id == editor.service)
                                    .map(|s| s.name.as_str())
                                    .unwrap_or("请先添加服务器"),
                            )
                            .show_ui(ui, |ui| {
                                for service in &self.book.services {
                                    ui.selectable_value(
                                        &mut editor.service,
                                        service.id.clone(),
                                        &service.name,
                                    );
                                }
                            });
                        field(
                            ui,
                            "目标设备名称",
                            &mut editor.device,
                            "被控端在该服务器发布的名称",
                        );
                    }
                }
                ui.weak("首次连接时确认设备身份，以后不再重复配对。");
                ui.add_space(12.);
                ui.horizontal(|ui| {
                    if primary(ui, "保存并连接").clicked() {
                        save = true;
                        connect = true;
                    }
                    if ui.button("仅保存").clicked() {
                        save = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel = true;
                    }
                });
            });
            if save {
                match self.save_device(editor.clone()) {
                    Ok(id) => {
                        if connect {
                            actions.push(Action::Open(id));
                        }
                    }
                    Err(e) => {
                        editor.error = format!("无法保存设备：{e:#}");
                        self.editor = Some(editor);
                    }
                }
            } else if open && !cancel {
                self.editor = Some(editor);
            }
        }
        if let Some(mut editor) = self.service_editor.take() {
            let mut open = true;
            let mut save = false;
            egui::Window::new("连接服务器")
                .id("service-editor".into())
                .open(&mut open)
                .collapsible(false)
                .default_width(480.)
                .resizable(false)
                .default_pos(ctx.screen_rect().center() - egui::vec2(250., 210.))
                .show(ctx, |ui| {
                    if !editor.error.is_empty() {
                        ui.colored_label(theme::DANGER, &editor.error);
                    }
                    field(ui, "名称", &mut editor.value.name, "例如：家庭服务器");
                    field(
                        ui,
                        "服务器地址",
                        &mut editor.value.address,
                        "signal.example:8443 或 wss://…/v1/signal",
                    );
                    ui.label("访问令牌");
                    ui.add(
                        egui::TextEdit::singleline(&mut editor.credential)
                            .password(true)
                            .desired_width(f32::INFINITY)
                            .hint_text(if editor.token.is_empty() {
                                "粘贴服务器提供的访问令牌"
                            } else {
                                "留空则保留已保存的凭据"
                            }),
                    );
                    ui.add_space(6.);
                    ui.collapsing("证书与高级设置", |ui| {
                        field(
                            ui,
                            "使用现有访问凭据文件（可选）",
                            &mut editor.token,
                            "server.token",
                        );
                        field(
                            ui,
                            "自签名或私有 CA 证书（可选）",
                            &mut editor.certificate,
                            "公开可信证书可留空",
                        );
                    });
                    ui.checkbox(&mut editor.value.host_enabled, "共享本机时使用此服务器");
                    if editor.value.host_enabled {
                        field(
                            ui,
                            "本机在服务器的设备名称",
                            &mut editor.value.published_name,
                            "my-desktop（字母、数字、-、_）",
                        );
                    }
                    ui.add_space(12.);
                    if primary(ui, "保存服务器").clicked() {
                        save = true;
                    }
                });
            if save {
                editor.value.token_file = editor.token.trim().trim_matches('"').into();
                editor.value.certificate = (!editor.certificate.trim().is_empty())
                    .then(|| editor.certificate.trim().trim_matches('"').into());
                let result = (|| -> anyhow::Result<()> {
                    let store = self
                        .store
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("设备配置不可用"))?;
                    if !editor.credential.trim().is_empty() {
                        editor.value.token_file =
                            store.store_service_credential(&editor.value.id, &editor.credential)?;
                        editor.token = editor.value.token_file.display().to_string();
                    }
                    let settings = uu_core::internet::Settings {
                        server: editor.value.address.clone(),
                        server_token: editor.value.token_file.clone(),
                        server_ca: editor.value.certificate.clone(),
                        ..Default::default()
                    };
                    settings.self_hosted()?.validate_local()?;
                    store.save_service(editor.value.clone())
                })();
                match result {
                    Ok(()) => {
                        self.notice = "服务器配置已保存".into();
                        self.refresh();
                    }
                    Err(e) => {
                        editor.error = format!("无法保存：{e:#}");
                        self.service_editor = Some(editor);
                    }
                }
            } else if open {
                self.service_editor = Some(editor);
            }
        }
        actions
    }
}

impl super::App {
    pub(super) fn workspace_navigation(&mut self, ctx: &egui::Context) {
        let mut compatibility = false;
        egui::SidePanel::left("workspace-navigation")
            .exact_width(170.)
            .frame(egui::Frame::none().fill(theme::SIDEBAR).inner_margin(16.))
            .show(ctx, |ui| {
                ui.add_space(12.);
                for (page, label) in [
                    (Page::Devices, "设备"),
                    (Page::Sharing, "共享本机"),
                    (Page::Services, "连接服务"),
                    (Page::Assistance, "远程协助"),
                ] {
                    if ui
                        .add_sized(
                            [138., 38.],
                            egui::SelectableLabel::new(
                                !self.lan.compatibility && self.workspace.page == page,
                                label,
                            ),
                        )
                        .clicked()
                    {
                        self.workspace.page = page;
                        self.lan.compatibility = false;
                        self.workspace.editor = None;
                        self.workspace.service_editor = None;
                    }
                    ui.add_space(4.);
                }
                ui.add_space(22.);
                ui.separator();
                ui.add_space(10.);
                if ui
                    .selectable_label(self.lan.compatibility, "UU 兼容设备")
                    .clicked()
                {
                    compatibility = true;
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    if ui.button("应用设置").clicked() {
                        self.open_settings();
                    }
                    ui.weak("Eclipse 设备工作区");
                });
            });
        if compatibility {
            self.lan.compatibility = true;
            self.workspace.editor = None;
            self.workspace.service_editor = None;
            if self.logged_in && self.devices.is_empty() {
                self.start_load();
            }
        }
    }
    pub(super) fn workspace_home(&mut self, ctx: &egui::Context) {
        let info = Info {
            logged_in: self.logged_in,
            uu_id: self
                .config
                .as_ref()
                .and_then(|c| {
                    uu_core::ClientState::read(&c.at("state.json"))
                        .ok()
                        .flatten()
                })
                .and_then(|s| s.device_id),
            qr: self.login_texture.as_ref().map(|t| t.id()),
            login_status: self
                .account_error
                .clone()
                .unwrap_or_else(|| self.login_status.clone()),
            login_busy: self.login_rx.is_some(),
            nearby: self.lan.nearby(self.workspace.host.server_id()),
        };
        let mut actions = Vec::new();
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::CANVAS).inner_margin(24.))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    actions = self.workspace.show(ui, &info);
                });
            });
        self.workspace_actions(actions, ctx);
    }
    pub(super) fn workspace_actions(&mut self, actions: Vec<Action>, ctx: &egui::Context) {
        for action in actions {
            match action {
                Action::Open(id) => self.open_device_profile(&id),
                Action::Login => self.start_login(),
                Action::CancelLogin => {
                    self.login_rx = None;
                    self.login_texture = None;
                    self.login_status = "已取消登录".into();
                    self.workspace.resume_connect = None;
                }
                // The start button is dispatched after rendering to keep one
                // owner of the runtime and immutable sharing configuration.
                Action::StartSharing => {
                    let result = (|| -> anyhow::Result<()> {
                        let config = self
                            .config
                            .clone()
                            .ok_or_else(|| anyhow::anyhow!("设备目录不可用"))?;
                        let book = self
                            .workspace
                            .store
                            .as_ref()
                            .ok_or_else(|| anyhow::anyhow!("设备配置不可用"))?
                            .read()?;
                        let trust = self
                            .workspace
                            .trust
                            .clone()
                            .ok_or_else(|| anyhow::anyhow!("设备身份不可用"))?;
                        self.workspace.host.start(config, book, trust, ctx)
                    })();
                    if let Err(e) = result {
                        self.workspace.notice = format!("无法开始共享：{e:#}");
                    }
                }
                Action::Assist(request) => {
                    if !self.logged_in {
                        self.workspace.assist_error =
                            "请先在连接服务中登录 UU 账号，再返回此页发起协助。".into();
                        self.workspace.page = Page::Services;
                        self.start_login();
                    } else {
                        match std::env::current_exe().and_then(|exe| {
                            self.sessions
                                .open_assistance(&exe, &request, self.platform.origin())
                        }) {
                            Ok(_) => {
                                self.workspace.assist_code.clear();
                                self.workspace.assist_text.clear();
                                self.workspace.assist_error.clear();
                                self.workspace.notice = "已打开远程协助窗口".into();
                            }
                            Err(e) => {
                                self.workspace.assist_error = format!("无法打开协助窗口：{e}")
                            }
                        }
                    }
                }
            }
        }
    }
    fn open_device_profile(&mut self, id: &str) {
        let result = (|| -> anyhow::Result<()> {
            let store = self
                .workspace
                .store
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("设备配置不可用"))?;
            let launch = store.launch(id)?;
            let book = store.read()?;
            let aliases: Vec<_> = book
                .devices
                .iter()
                .filter(|p| book.device(&p.id).is_ok_and(|p| p.id == launch.profile))
                .map(|p| p.id.clone())
                .collect();
            if self.sessions.focus_profiles(&aliases) {
                return Ok(());
            }
            if matches!(launch.route.destination, Destination::Uu { .. }) && !self.logged_in {
                self.workspace.resume_connect = Some(id.into());
                self.workspace.page = Page::Services;
                self.start_login();
                return Ok(());
            }
            let snapshot = store.publish_launch(id)?;
            let target = super::sessions::StreamTarget {
                connection: super::sessions::ConnectionTarget::NativeProfile {
                    id: launch.profile,
                    snapshot,
                },
                alias: launch.name,
                view_only: false,
            };
            self.sessions
                .open(&std::env::current_exe()?, &target, self.platform.origin())?;
            Ok(())
        })();
        if let Err(e) = result {
            self.workspace.notice = format!("无法连接设备：{e:#}");
        }
    }
    pub(super) fn workspace_poll(&mut self, ctx: &egui::Context) {
        if self.stream_target.is_some() {
            return;
        }
        self.workspace.poll(ctx, self.platform.window_id());
        if self.logged_in {
            if let Some(id) = self.workspace.resume_connect.take() {
                self.open_device_profile(&id);
            }
        }
    }
    pub(super) fn workspace_dialogs(&mut self, ctx: &egui::Context) {
        let actions = self.workspace.dialogs(ctx);
        self.workspace_actions(actions, ctx);
    }
    pub(super) fn device_pairing_ui(&mut self, ctx: &egui::Context) {
        let code = self
            .controller
            .as_ref()
            .and_then(|h| h.stats.pairing.code());
        self.workspace.pairing_visible = code.is_some();
        let Some(code) = code else {
            return;
        };
        let mut open = true;
        egui::Window::new("首次连接 · 设备配对")
            .id("device-pair-code".into())
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .default_width(420.)
            .show(ctx, |ui| {
                ui.label("请在被控端输入下面的配对码，并批准本设备：");
                ui.add_space(14.);
                let formatted = format!("{} {}", &code[..4], &code[4..]);
                ui.label(
                    RichText::new(formatted)
                        .size(34.)
                        .monospace()
                        .color(theme::ACCENT),
                );
                ui.add_space(10.);
                if ui.button("复制配对码").clicked() {
                    ctx.copy_text(code.clone());
                }
                ui.weak("完整公钥会自动交换。配对完成后，以后连接无需重复输入。");
                ui.spinner();
                ui.label("等待被控端确认…");
            });
        if !open {
            self.disconnect();
        }
    }
}
