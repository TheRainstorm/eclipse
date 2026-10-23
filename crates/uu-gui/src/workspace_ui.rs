//! Devices are the primary object. Sharing settings and connection services
//! have their own pages and never ask for a per-device shared key file.
use super::theme;
use eframe::egui::{self, RichText};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use uu_core::{
    device_book::{RateMode,Book, Destination, Encoding, Profile, Route, Service, Sharing, Store},
    device_internet::EndpointStatus,
    device_trust::TrustStore,
    native::{CaptureMonitor, HostStatus},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(super) enum Page {
    #[default]
    Devices,
    Services,
    Compatibility,
    Settings,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(super) enum DeviceView {
    #[default]
    List,
    Details(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(super) enum CompatibilityView {
    #[default]
    Devices,
    Assistance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(super) enum SettingsView {
    #[default]
    Software,
    About,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(super) struct HomeRoute {
    pub page: Page,
    pub device: DeviceView,
    pub compatibility: CompatibilityView,
    pub settings: SettingsView,
    pub settings_return: Page,
}
pub(super) enum Action {
    Open(String),
    Login,
    RenameUu,
    ManageUu,
    CancelLogin,
    StartSharing,
    SaveClientDefaults(uu_core::client_settings::ClientSettings),
    Assist(uu_core::assistance::Request),
}
pub(super) struct Info {
    pub logged_in: bool,
    pub uu_id: Option<String>,
    pub qr: Option<egui::TextureId>,
    pub login_status: String,
    pub login_busy: bool,
    pub nearby: Vec<(String, String)>,
    pub uu_devices: Vec<(String, String)>,
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
struct ClientEditor {
    device:String,
    value:uu_core::client_settings::NamedClientSettings,
    section:super::settings_page::Section,
    global:bool,
    error:String,
}
#[derive(Clone)]
struct ServiceEditor {
    error: String,
    value: Service,
    token: String,
    credential: String,
    certificate: String,
}

struct PreviewSlot {revision:String,texture:Option<egui::TextureHandle>,loaded:bool}
struct PreviewJob {device:String,revision:String,rx:std::sync::mpsc::Receiver<Result<super::wallpapers::Thumbnail,String>>}
pub(super) struct Workspace {
    pub route: HomeRoute,
    pub book: Book,
    pub host: super::sharing_ui::HostUi,
    pub store: Option<Store>,
    pub trust: Option<TrustStore>,
    pub feedback: super::ui_feedback::Feedback,
    draft: Sharing,
    monitors: Vec<CaptureMonitor>,
    monitor_error: String,
    host_section:u8,
    service_tab:u8,
    encoder_vendor_tab:u8,
    editor: Option<DeviceEditor>,
    service_editor: Option<ServiceEditor>,
    client_editor: Option<ClientEditor>,
    pub client_defaults: uu_core::client_settings::ClientSettings,
    dark_background:bool,
    assist_text: String,
    assist_id: String,
    assist_code: String,
    assist_error: String,
    last_read: Instant,
    pub resume_connect: Option<String>,
    pub pairing_visible: bool,
    autostart_checked: bool,
    previews:std::collections::HashMap<String,PreviewSlot>,
    preview_jobs:Vec<PreviewJob>,
    uu_choices: Vec<(String,String)>,
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
        logged_in: bool,
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
            assert!(workspace.store.is_some(), "{}", workspace.feedback.text());
            let ctx = egui::Context::default();
            theme::apply(&ctx);
            Self {
                workspace,
                ctx,
                root,
                time: 0.,
                texts: Vec::new(),
                logged_in: false,
            }
        }
        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<Action> {
            self.time += 0.02;
            let mut actions = Vec::new();
            let info = Info {
                logged_in: self.logged_in,
                uu_id: None,
                qr: None,
                login_status: String::new(),
                login_busy: false,
                nearby: Vec::new(),
                uu_devices: Vec::new(),
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
    fn uu_name_is_automatic_and_does_not_change_native_host_preferences() {
        let mut h = Harness::new();
        h.workspace.route.page = Page::Services;
        h.workspace.draft.name = "My native host".into();
        let name = h.workspace.prepare_device_name().unwrap();
        assert_eq!(name, uu_core::config::uu_device_name());
        assert!(name.ends_with("-Eclipse"));
        assert!(matches!(&h.click("登录 UU 账号")[..], [Action::Login]));
        h.logged_in = true;
        assert!(matches!(&h.click("更新注册信息")[..], [Action::RenameUu]));
        assert!(matches!(&h.click("管理 UU 设备")[..], [Action::ManageUu]));
        assert_eq!(h.workspace.draft.name, "My native host");
    }

    #[test]
    fn device_editor_retains_failed_input_and_saves_multiple_routes_without_duplicates_or_key_files(
    ) {
        let mut h = Harness::new();
        h.workspace.new_device(None,None);
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
        h.workspace.route.page = Page::Services;
        h.click("＋ 添加服务器");
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
    fn final_remote_image_is_persisted_and_loaded_into_the_device_card() {
        let mut h=Harness::new();
        let profile=Profile::new("Generated desktop",Destination::Ip{address:"127.0.0.1:47998".into(),legacy_device:None});
        let id=profile.id.clone();let store=h.workspace.store.clone().unwrap();store.save_device(profile).unwrap();
        let writer=super::super::device_preview::Writer::default();
        writer.submit(store.clone(),id.clone(),None);
        writer.submit(store.clone(),id.clone(),Some(egui::ColorImage::new([160,90],egui::Color32::from_rgb(40,110,190))));
        drop(writer);
        let book=store.read().unwrap();let profile=book.device(&id).unwrap();
        assert!(profile.last_connected.is_some());let preview=profile.preview.as_ref().unwrap();
        let bytes=store.preview_bytes(preview).unwrap();assert!(bytes.starts_with(&[0xff,0xd8]));
        h.workspace.refresh();let until=Instant::now()+Duration::from_secs(3);
        loop {
            h.workspace.poll_previews(&h.ctx);
            if h.workspace.previews.get(&id).and_then(|p|p.texture.as_ref()).is_some(){break;}
            assert!(Instant::now()<until,"private preview cache failed to load");std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(h.workspace.previews[&id].texture.as_ref().unwrap().size(),[160,90]);
    }
    #[test]
    fn device_preset_editor_saves_an_isolated_selected_config() {
        let mut h=Harness::new();
        let profile=Profile::new("Desk",Destination::Ip {address:"127.0.0.1:47998".into(),legacy_device:None});
        let id=profile.id.clone();
        h.workspace.store.as_ref().unwrap().save_device(profile).unwrap(); h.workspace.refresh();
        h.workspace.client_defaults.fps=120;
        let profile=h.workspace.book.device(&id).unwrap().clone();
        h.workspace.edit_client_config(&profile,None,false);
        let editor=h.workspace.client_editor.as_mut().unwrap();
        editor.value.name="移动网络".into();editor.value.settings.fps=30;editor.value.settings.bitrate=3;
        h.click("保存配置");
        assert!(h.workspace.client_editor.is_none());
        let book=h.workspace.store.as_ref().unwrap().read().unwrap();
        let profile=book.device(&id).unwrap();
        assert_eq!(profile.client_configs.len(),1);
        assert_eq!(profile.selected_client.as_deref(),Some(profile.client_configs[0].id.as_str()));
        assert_eq!(profile.client_configs[0].settings.fps,30);
        assert_eq!(h.workspace.client_defaults.fps,120);
        assert_eq!(book.sharing.fps,60);
        h.workspace.route.device = DeviceView::Details(id);
        h.click("复制");
        assert!(h.workspace.client_editor.as_ref().unwrap().value.name.contains("副本"));
    }
    #[test]
    fn media_draft_does_not_restore_old_connection_ports_or_enable_flags() {
        let mut h=Harness::new();
        h.workspace.draft.bitrate_mbps=25;
        h.workspace.update_connections(|s|{s.ip_enabled=false;s.uu_enabled=true;s.service_port=51001;s.discovery_port=51002;});
        assert!(h.workspace.save_sharing());
        assert!(!h.workspace.server_dirty());
        let book=h.workspace.store.as_ref().unwrap().read().unwrap();
        assert_eq!(book.sharing.bitrate_mbps,25);
        assert!(!book.sharing.ip_enabled && book.sharing.uu_enabled);
        assert_eq!((book.sharing.service_port,book.sharing.discovery_port),(51001,51002));
    }
    #[test]
    fn feedback_does_not_leak_between_home_pages() {
        let mut h = Harness::new();
        h.workspace.feedback.success(
            crate::ui_feedback::Scope::Devices,
            "设备页保存成功",
        );
        h.frame(Vec::new());
        assert!(h.texts.iter().any(|(text, _)| text == "设备页保存成功"));
        h.workspace.route.page = Page::Services;
        h.frame(Vec::new());
        assert!(!h.texts.iter().any(|(text, _)| text == "设备页保存成功"));
        h.workspace.route.page = Page::Devices;
        h.frame(Vec::new());
        assert!(h.texts.iter().any(|(text, _)| text == "设备页保存成功"));
    }

    #[test]
    fn device_placeholders_use_a_stable_color_and_readable_initial() {
        assert_eq!(device_initial("  remote desktop"), "R");
        assert_eq!(device_initial("远程工作站"), "远");
        assert_eq!(device_initial("  "), "?");
        assert_eq!(
            device_placeholder_color("stable-device-id"),
            device_placeholder_color("stable-device-id")
        );
        assert_ne!(
            device_placeholder_color("device-a"),
            device_placeholder_color("device-b")
        );
    }

    #[test]
    fn assistance_import_uses_its_own_invitation_and_never_creates_a_device_grant() {
        let mut h = Harness::new();
        h.workspace.route.page = Page::Compatibility; h.workspace.route.compatibility = CompatibilityView::Assistance;
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
fn muted(ui:&mut egui::Ui,text:impl Into<String>)->egui::Response {
    ui.label(RichText::new(text).size(12.).color(theme::MUTED))
}
fn device_placeholder_color(id: &str) -> egui::Color32 {
    let palette = [
        egui::Color32::from_rgb(45, 93, 155),
        egui::Color32::from_rgb(74, 119, 80),
        egui::Color32::from_rgb(150, 92, 61),
        egui::Color32::from_rgb(105, 82, 151),
        egui::Color32::from_rgb(38, 124, 137),
        egui::Color32::from_rgb(151, 82, 108),
        egui::Color32::from_rgb(74, 101, 133),
        egui::Color32::from_rgb(128, 104, 53),
    ];
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in id.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    palette[(hash as usize) % palette.len()]
}

fn device_initial(name: &str) -> String {
    name.trim()
        .chars()
        .next()
        .map(|ch| {
            if ch.is_ascii_alphabetic() {
                ch.to_ascii_uppercase().to_string()
            } else {
                ch.to_string()
            }
        })
        .unwrap_or_else(|| "?".into())
}

fn last_connection(time:Option<u64>)->String {
    let Some(time)=time else{return "尚未连接".into();};
    let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d|d.as_secs()).unwrap_or(time);
    let seconds=now.saturating_sub(time);
    let relative=if seconds<60 {"刚刚".into()}else if seconds<3600{format!("{} 分钟前",seconds/60)}else if seconds<86400{format!("{} 小时前",seconds/3600)}else{format!("{} 天前",seconds/86400)};
    format!("上次连接 · {relative}")
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
                    book.sharing.name = uu_core::config::local_device_name();
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
        let mut feedback = super::ui_feedback::Feedback::default();
        if !notice.is_empty() {
            feedback.error(super::ui_feedback::Scope::Devices, notice);
        }
        Self {
            route: HomeRoute {
                page: if std::env::var_os("ECLIPSE_SHOW_SHARING").is_some() {
                    Page::Services
                } else {
                    Page::Devices
                },
                ..Default::default()
            },
            book,
            host: Default::default(),
            store,
            trust,
            feedback,
            draft,
            monitors: Vec::new(),
            monitor_error: String::new(),
            host_section:0,
            service_tab:0,
            encoder_vendor_tab:0,
            editor: None,
            service_editor: None,
            client_editor: None,
            client_defaults:Default::default(),
            dark_background:false,
            assist_text: String::new(),
            assist_id: String::new(),
            assist_code: String::new(),
            assist_error: String::new(),
            last_read: Instant::now(),
            resume_connect: None,
            pairing_visible: false,
            autostart_checked: false,
            previews:Default::default(),preview_jobs:Vec::new(),uu_choices:Vec::new(),
        }
    }
    pub fn poll(&mut self, ctx: &egui::Context, window: isize) {
        self.feedback.tick(ctx);
        self.host.poll(ctx, window);
        self.poll_previews(ctx);
        if self.last_read.elapsed() >= Duration::from_millis(500) {
            self.last_read = Instant::now();
            self.refresh();
        }
    }
    fn poll_previews(&mut self,ctx:&egui::Context) {
        self.previews.retain(|id,_|self.book.devices.iter().any(|p|p.id==*id&&p.redirect.is_none()));
        self.preview_jobs.retain(|job| {
            match job.rx.try_recv() {
                Ok(result)=>{
                    if let Some(slot)=self.previews.get_mut(&job.device).filter(|slot|slot.revision==job.revision) {
                        slot.loaded=true;
                        if let Ok(image)=result {
                            slot.texture=Some(ctx.load_texture(format!("device-preview-{}",job.device),egui::ColorImage::from_rgba_unmultiplied([image.width as usize,image.height as usize],&image.rgba),egui::TextureOptions::LINEAR));
                        }
                    }
                    false
                },
                Err(std::sync::mpsc::TryRecvError::Empty)=>true,
                Err(_)=>{if let Some(slot)=self.previews.get_mut(&job.device){slot.loaded=true;}false},
            }
        });
        for profile in self.book.devices.iter().filter(|p|p.redirect.is_none()) {
            let Some(preview)=&profile.preview else{continue;};
            let slot=self.previews.entry(profile.id.clone()).or_insert_with(||PreviewSlot{revision:preview.id.clone(),texture:None,loaded:false});
            if slot.revision!=preview.id {slot.revision=preview.id.clone();slot.loaded=false;}
            if slot.loaded || self.preview_jobs.iter().any(|j|j.device==profile.id) || self.preview_jobs.len()>=2 {continue;}
            let Some(store)=self.store.clone() else{continue;};
            let preview=preview.clone();let device=profile.id.clone();let revision=preview.id.clone();
            let (tx,rx)=std::sync::mpsc::channel();let context=ctx.clone();let identity=device.clone();
            std::thread::spawn(move|| {
                let result=store.preview_bytes(&preview).and_then(|bytes|super::wallpapers::decode(super::wallpapers::Request{device_id:identity,url:String::new()},bytes)).map_err(|e|format!("{e:#}"));
                let _=tx.send(result);context.request_repaint();
            });
            self.preview_jobs.push(PreviewJob{device,revision,rx});
        }
        if !self.preview_jobs.is_empty(){ctx.request_repaint_after(Duration::from_millis(100));}
    }
    fn device_scope(&self) -> super::ui_feedback::Scope {
        match &self.route.device {
            DeviceView::Details(id) => super::ui_feedback::Scope::DeviceDetails(id.clone()),
            DeviceView::List => super::ui_feedback::Scope::Devices,
        }
    }

    fn service_scope(&self) -> super::ui_feedback::Scope {
        super::ui_feedback::Scope::Services(self.service_tab)
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
                Err(e) => self.feedback.error(
                    super::ui_feedback::Scope::Devices,
                    format!("无法读取设备配置：{e:#}，原文件已保留"),
                ),
            }
        }
    }
    pub fn prepare_device_name(&self) -> anyhow::Result<String> {
        Ok(uu_core::config::uu_device_name())
    }
    fn update_connections(&mut self, edit: impl FnOnce(&mut Sharing)) {
        let result = self.store.as_ref().ok_or_else(|| anyhow::anyhow!("设备配置不可用"))
            .and_then(|store| store.edit(|book| { edit(&mut book.sharing); Ok(()) }));
        if let Err(error) = result {
            self.feedback.error(
                super::ui_feedback::Scope::Services(self.service_tab),
                format!("无法保存连接服务：{error:#}"),
            );
        }
        self.refresh();
    }
    fn endpoint_state(&self, ui: &mut egui::Ui, name: &str) {
        for (endpoint, status) in self.host.statuses().into_iter().filter(|(n,_)| n==name) {
            let color = match status { EndpointStatus::Ready|EndpointStatus::Streaming=>theme::SUCCESS,EndpointStatus::Error(_)=>theme::DANGER,_=>theme::MUTED };
            ui.colored_label(color, format!("{endpoint} · {}",status.label()));
        }
    }
    pub(super) fn discard_server_draft(&mut self){self.draft=self.book.sharing.clone();}
    pub(super) fn server_dirty(&self)->bool {self.draft!=self.book.sharing}
    pub(super) fn save_sharing(&mut self) -> bool {
        let result = self
            .store
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("设备配置不可用"))
            .and_then(|s| s.edit(|book| {
                let mut media = self.draft.clone();
                media.ip_enabled = book.sharing.ip_enabled;
                media.uu_enabled = book.sharing.uu_enabled;
                media.service_port = book.sharing.service_port;
                media.discovery_port = book.sharing.discovery_port;
                book.sharing = media;
                Ok(())
            }));
        let saved = result.is_ok();
        match result {
            Ok(()) => self.feedback.success(
                super::ui_feedback::Scope::Services(self.service_tab),
                "服务端设置已保存",
            ),
            Err(e) => self.feedback.error(
                super::ui_feedback::Scope::Services(self.service_tab),
                format!("无法保存：{e:#}"),
            ),
        }
        self.refresh();
        if saved {self.draft=self.book.sharing.clone();}
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
        self.uu_choices = info.uu_devices.clone();
        let mut actions = Vec::new();
        match self.route.page {
            Page::Devices => self.devices(ui, info, &mut actions),
            Page::Services => self.service_workspace(ui, info, &mut actions),
            Page::Compatibility => {
                if self.route.compatibility == CompatibilityView::Assistance {
                    ui.heading("远程协助");
                    muted(ui, "通过临时邀请帮助另一台设备");
                    ui.add_space(18.);
                    self.assistance(ui, &mut actions);
                }
            }
            Page::Settings => {}
        }
        actions
    }
    fn devices(&mut self, ui: &mut egui::Ui, info: &Info, actions: &mut Vec<Action>) {
        if let DeviceView::Details(id) = self.route.device.clone() {
            self.device_details(ui, &id, actions);
            return;
        }
        self.feedback.show(ui, super::ui_feedback::Scope::Devices);
        let blank=ui.interact(ui.max_rect().intersect(ui.clip_rect()),ui.id().with("device-wallpaper"),egui::Sense::click());
        blank.context_menu(|ui| {
            if ui.button("添加设备").clicked(){self.new_device(None,None);ui.close_menu();}
            if !info.nearby.is_empty() {
                ui.menu_button("添加附近设备",|ui| {
                    for (name,address) in &info.nearby {
                        if ui.button(name).on_hover_text(address).clicked() {
                            if let Some(store)=&self.store {
                                let result=store.save_device(Profile::new(name,Destination::Ip {address:address.clone(),legacy_device:None}));
                                if let Err(error)=result {
                                    self.feedback.error(
                                        super::ui_feedback::Scope::Devices,
                                        format!("无法添加设备：{error:#}"),
                                    );
                                }
                                self.refresh();
                            }
                            ui.close_menu();
                        }
                    }
                });
            }
        });
        let profiles:Vec<_>=self.book.devices.iter().filter(|p|p.redirect.is_none()).cloned().collect();
        let columns=((ui.available_width()+22.)/252.).floor().clamp(1.,8.) as usize;
        for row in profiles.chunks(columns) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x=22.;
                for profile in row {self.device_tile(ui,profile,actions);}
            });
            ui.add_space(22.);
        }
    }
    fn device_tile(&mut self,ui:&mut egui::Ui,profile:&Profile,actions:&mut Vec<Action>) {
        ui.push_id(&profile.id,|ui| {
            let width=230_f32.min(ui.available_width());
            let (rect,response)=ui.allocate_exact_size(egui::vec2(width,width*9./16.+42.),egui::Sense::click());
            response.widget_info(||egui::WidgetInfo::labeled(egui::WidgetType::Button,ui.is_enabled(),format!("连接 {}",profile.name)));
            if response.hovered()||response.has_focus(){ui.painter().rect_filled(rect.expand(7.),10.,egui::Color32::from_rgba_unmultiplied(80,140,220,55));}
            let picture=egui::Rect::from_min_size(rect.min,egui::vec2(width,width*9./16.));
            self.paint_device_picture(ui.painter(),picture,profile);
            let color=if self.dark_background {egui::Color32::WHITE}else{theme::INK};
            let text=ui.painter().layout(profile.name.clone(),egui::FontId::proportional(17.),color,width);
            let pos=egui::pos2(rect.center().x-text.size().x/2.,picture.bottom()+12.);
            ui.painter().with_clip_rect(egui::Rect::from_min_max(egui::pos2(rect.left(),picture.bottom()+8.),rect.max)).galley(pos,text,color);
            if response.clicked(){actions.push(Action::Open(profile.id.clone()));}
            response.context_menu(|ui| {
                ui.set_min_width(170.);
                if ui.button("详情").clicked(){self.route.device=DeviceView::Details(profile.id.clone());ui.close_menu();}
                ui.menu_button("连接方式",|ui| {
                    for route in &profile.routes {
                        if ui.selectable_label(profile.selected_route==route.id,format!("{} · {}",route.destination.label(),route.destination.target())).clicked() {
                            let mut updated=profile.clone();updated.selected_route=route.id.clone();
                            if let Some(store)=&self.store {if let Err(e)=store.save_device(updated){
                                let scope=self.device_scope();
                                self.feedback.error(scope,format!("无法保存连接方式：{e:#}"));
                            }}
                            self.refresh();ui.close_menu();
                        }
                    }
                });
                ui.menu_button("配置选择",|ui| {
                    for (id,name) in std::iter::once((None,"默认配置")).chain(profile.client_configs.iter().map(|v|(Some(v.id.clone()),v.name.as_str()))) {
                        if ui.selectable_label(profile.selected_client==id,name).clicked() {
                            if let Some(store)=&self.store {if let Err(e)=store.select_client_config(&profile.id,id){
                                let scope=self.device_scope();
                                self.feedback.error(scope,format!("无法选择配置：{e:#}"));
                            }}
                            self.refresh();ui.close_menu();
                        }
                    }
                });
                if ui.button("配置编辑").clicked(){
                    let selected=profile.client_configs.iter().find(|v|Some(&v.id)==profile.selected_client.as_ref()).cloned();
                    self.edit_client_config(profile,selected,false);ui.close_menu();
                }
            });
        });
    }
    fn route_picker(&mut self,ui:&mut egui::Ui,profile:&Profile) {
                let mut selected=profile.selected_route.clone();
                ui.horizontal(|ui| {
                    ui.label("连接方式");
                    egui::ComboBox::from_id_salt("device-route").selected_text(profile.route().map(|r|r.destination.label()).unwrap_or("请选择")).width(150.).show_ui(ui,|ui| {
                        for route in &profile.routes {ui.selectable_value(&mut selected,route.id.clone(),format!("{} · {}",route.destination.label(),route.destination.target()));}
                    });
                });
                if selected!=profile.selected_route {
                    let mut updated=profile.clone();updated.selected_route=selected;
                    if let Some(store)=&self.store {if let Err(e)=store.save_device(updated){
                        let scope=self.device_scope();
                        self.feedback.error(scope,format!("无法保存连接方式：{e:#}"));
                    }}
                    self.refresh();
                }
    }
    fn device_picture(&mut self,ui:&mut egui::Ui,profile:&Profile,width:f32) {
        let (rect,_)=ui.allocate_exact_size(egui::vec2(width,width*9./16.),egui::Sense::hover());
        self.paint_device_picture(ui.painter(),rect,profile);
    }
    fn paint_device_picture(&self,painter:&egui::Painter,rect:egui::Rect,profile:&Profile) {
        painter.rect_filled(rect,8.,theme::SIDEBAR);
        if let Some(texture)=self.previews.get(&profile.id).and_then(|p|p.texture.as_ref()) {
            let size=texture.size_vec2();let scale=(rect.width()/size.x).min(rect.height()/size.y);
            painter.image(texture.id(),egui::Rect::from_center_size(rect.center(),size*scale),egui::Rect::from_min_max(egui::Pos2::ZERO,egui::pos2(1.,1.)),egui::Color32::WHITE);
        }else{
            painter.rect_filled(rect, 8.0, device_placeholder_color(&profile.id));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                device_initial(&profile.name),
                egui::FontId::proportional((rect.height() * 0.40).clamp(34.0, 76.0)),
                egui::Color32::WHITE,
            );
        }
    }
    fn edit_client_config(&mut self,profile:&Profile, value:Option<uu_core::client_settings::NamedClientSettings>, duplicate:bool) {
        let mut value=value.unwrap_or_else(||uu_core::client_settings::NamedClientSettings::new("新配置",self.client_defaults.clone()));
        if duplicate { value.id=uuid::Uuid::new_v4().to_string(); value.name=format!("{} 副本",value.name); }
        self.client_editor=Some(ClientEditor {device:profile.id.clone(),value,section:super::settings_page::Section::Video,global:false,error:String::new()});
    }
    fn edit_global_client_config(&mut self) {
        self.client_editor=Some(ClientEditor {
            device:String::new(),
            value:uu_core::client_settings::NamedClientSettings::new("全局默认",self.client_defaults.clone()),
            section:super::settings_page::Section::Video,
            global:true,
            error:String::new(),
        });
    }
    fn client_config_picker(&mut self,ui:&mut egui::Ui,profile:&Profile) {
        ui.horizontal_wrapped(|ui| {
            ui.label("客户端配置");
            let mut selected=profile.selected_client.clone();
            let title=profile.client_configs.iter().find(|v|Some(&v.id)==selected.as_ref()).map(|v|v.name.as_str()).unwrap_or("默认配置");
            egui::ComboBox::from_id_salt(("client-config",&profile.id)).selected_text(title).width(160.).show_ui(ui,|ui| {
                ui.selectable_value(&mut selected,None,"默认配置");
                for value in &profile.client_configs {ui.selectable_value(&mut selected,Some(value.id.clone()),&value.name);}
            });
            if selected!=profile.selected_client {
                if let Some(store)=&self.store {
                    match store.select_client_config(&profile.id,selected){
                        Ok(())=>self.refresh(),
                        Err(e)=>{
                            let scope=self.device_scope();
                            self.feedback.error(scope,format!("无法选择配置：{e:#}"));
                        }
                    }
                }
            }
            if ui.small_button("配置…").clicked() {
                let value=profile.client_configs.iter().find(|v|Some(&v.id)==profile.selected_client.as_ref()).cloned();
                self.edit_client_config(profile,value,false);
            }
            if matches!(self.route.device, DeviceView::Details(_)) && ui.small_button("新建").clicked(){self.edit_client_config(profile,None,false);}
            if ui.small_button("编辑全局默认").clicked(){self.edit_global_client_config();}
        });
    }
    fn device_details(&mut self, ui: &mut egui::Ui, id: &str, actions: &mut Vec<Action>) {
        let profile = match self.book.device(id) {
            Ok(p) => p.clone(),
            Err(e) => {
                self.feedback.error(super::ui_feedback::Scope::Devices, e.to_string());
                self.route.device = DeviceView::List;
                return;
            }
        };
        ui.scope(|ui| {
            ui.set_max_width(880.0);
            self.feedback.show(
                ui,
                super::ui_feedback::Scope::DeviceDetails(id.to_owned()),
            );
            ui.horizontal_top(|ui| {
                let picture_width = (ui.available_width() * 0.30).clamp(220.0, 300.0);
                self.device_picture(ui, &profile, picture_width);
                ui.add_space(22.0);
                ui.vertical(|ui| {
                    ui.heading(&profile.name);
                    ui.add_space(4.0);
                    muted(ui, last_connection(profile.last_connected));
                    let route = profile.route().map(|route| {
                        format!("{} · {}", route.destination.label(), route.destination.target())
                    }).unwrap_or_else(|_| "未选择连接方式".into());
                    muted(ui, route);
                    if profile.peer.is_some() {
                        ui.label(RichText::new("已完成设备配对").color(theme::SUCCESS));
                    } else {
                        ui.label(RichText::new("首次连接需要配对").color(theme::MUTED));
                    }
                    ui.add_space(8.0);
                    if primary(ui, "连接此设备").clicked() {
                        actions.push(Action::Open(profile.id.clone()));
                    }
                });
            });
            ui.add_space(18.0);
            card(ui, |ui| {
                ui.strong("设备身份");
                ui.add_space(8.0);
                if let Some(peer) = &profile.peer {
                    ui.label(RichText::new("已完成配对").color(theme::SUCCESS));
                    muted(ui,"设备公钥指纹");
                    ui.monospace(peer);
                    if ui.small_button("复制指纹").clicked() {
                        ui.ctx().copy_text(peer.clone());
                    }
                } else {
                    ui.label("首次连接时，被控端输入控制端显示的短码即可保存授权。");
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
                        Err(e) => self.feedback.error(self.device_scope(), format!("无法移除地址：{e}")),
                    }
                }
            }
        });
        card(ui,|ui| {
            ui.strong("客户端配置");
            muted(ui,"每台设备可保存多个配置；选择全局默认会跟随应用设置。修改配置只影响下次连接。");
            self.client_config_picker(ui,&profile);
            for value in &profile.client_configs {
                ui.push_id(&value.id,|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(&value.name);
                        muted(ui,format!("{} FPS · {}",value.settings.fps,value.settings.codec.label()));
                        if ui.small_button("编辑").clicked(){self.edit_client_config(&profile,Some(value.clone()),false);}
                        if ui.small_button("复制").clicked(){self.edit_client_config(&profile,Some(value.clone()),true);}
                        if ui.small_button("删除").clicked(){
                            if let Some(store)=&self.store {
                                match store.remove_client_config(&profile.id,&value.id){
                                    Ok(())=>self.refresh(),
                                    Err(e)=>{
                                        let scope=self.device_scope();
                                        self.feedback.error(scope,format!("无法删除配置：{e:#}"));
                                    }
                                }
                            }
                        }
                    });
                });
            }
        });
        ui.add_space(12.);
        if ui.button("移除本地设备记录").clicked() {
            if let Some(store) = &self.store {
                match store.forget(&profile.id) {
                    Ok(()) => {
                        self.route.device = DeviceView::List;
                        self.refresh();
                    }
                    Err(e) => self.feedback.error(self.device_scope(), format!("无法移除：{e}")),
                }
            }
        }
        muted(ui,"移除本地记录不会撤销被控端已授予的权限。授权在被控端的“共享本机”中管理。");
        });
    }
    fn refresh_monitors(&mut self) {
        if uu_core::native::uses_desktop_portal() {
            self.monitors.clear();
            self.monitor_error.clear();
            if self.draft.monitor != uu_core::native_displays::ALL_MONITORS {self.draft.monitor.clear();}
            return;
        }
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
    fn service_workspace(&mut self, ui:&mut egui::Ui, info:&Info, actions:&mut Vec<Action>) {
        ui.horizontal_wrapped(|ui| {
            for (tab,label) in [(0,"连接服务"),(1,"输入"),(2,"音频 / 视频"),(3,"编码器")] {
                ui.selectable_value(&mut self.service_tab,tab,label);
            }
        });
        ui.add_space(14.);
        let scope = self.service_scope();
        self.feedback.show(ui, scope);
        if self.service_tab==0 {
            self.service_control(ui,actions);
            self.services(ui,info,actions);
        } else {
            ui.horizontal(|ui| {
                let locked=self.host.running();
                if ui.add_enabled(!locked && self.server_dirty(),egui::Button::new(RichText::new("保存设置").color(egui::Color32::WHITE)).fill(theme::ACCENT)).clicked(){self.save_sharing();}
                if ui.add_enabled(self.server_dirty(),egui::Button::new("还原修改")).clicked(){self.discard_server_draft();}
                muted(ui,if locked {"停止服务后可修改"}else if self.server_dirty(){"有未保存的修改"}else{"已保存"});
            });
            ui.add_space(12.);
            self.host_section=match self.service_tab {1=>2,2=>0,_=>1};
            if self.service_tab==3 {
                ui.horizontal_wrapped(|ui| {
                    for (vendor,label) in [(0,"自动"),(1,"NVIDIA"),(2,"Intel"),(3,"AMD")] {
                        ui.selectable_value(&mut self.encoder_vendor_tab,vendor,label);
                    }
                });
                muted(ui,match self.encoder_vendor_tab {
                    1=>"NVIDIA：Windows 通过驱动提供的 Media Foundation 硬编使用 H.264。当前尚未接入 NVENC 专有参数。",
                    2=>"Intel：H.264 使用系统硬件接口，HEVC / AV1 和 VBR 使用 Intel VPL；驱动需要支持所选组合。",
                    3=>"AMD：Windows 通过驱动提供的 Media Foundation 硬编使用 H.264。当前尚未接入 AMF 专有参数。",
                    _=>"编码参数应用于当前主机。自动模式按已安装驱动选择可用后端；客户端仍需支持所选格式。",
                });
                let choice=match self.encoder_vendor_tab {1=>Some(Encoding::Nvidia),2=>Some(Encoding::Intel),3=>Some(Encoding::Amd),_=>None};
                if let Some(choice)=choice {
                    let mut selected=self.draft.encoding==choice;
                    if ui.add_enabled(!self.host.running(),egui::Checkbox::new(&mut selected,"使用此厂商硬件编码")).changed() {
                        self.draft.encoding=if selected {choice}else{Encoding::Auto};
                    }
                }
                ui.add_space(10.);
            }
            self.server_settings(ui,info,actions);
        }
    }
    fn service_control(&mut self, ui:&mut egui::Ui, actions:&mut Vec<Action>) {
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(if self.host.running() {
                        if self.host.stopping() {
                            "正在停止服务…"
                        } else {
                            "本机服务运行中"
                        }
                    } else {
                        "本机服务已停止"
                    })
                    .size(18.)
                    .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.host.running() {
                        if ui.add_enabled(!self.host.stopping(),egui::Button::new("停止服务")).clicked(){self.host.stop();}
                    } else if ui.add_enabled(self.store.is_some(),egui::Button::new(RichText::new("启动服务").color(egui::Color32::WHITE)).fill(theme::ACCENT)).clicked() {
                        if !self.server_dirty() || self.save_sharing() { actions.push(Action::StartSharing); }
                    }
                });
            });
            if !self.host.notice.is_empty() {
                let error = self.host.notice_is_error();
                let mut dismiss = false;
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(&self.host.notice).color(if error {
                            theme::DANGER
                        } else {
                            theme::SUCCESS
                        }),
                    );
                    if error && ui.small_button("关闭").clicked() {
                        dismiss = true;
                    }
                });
                if dismiss {
                    self.host.clear_notice();
                }
            }
            let mut auto = self.book.sharing.auto_start;
            if ui.checkbox(&mut auto, "打开软件时自动启动服务").changed() {
                self.update_connections(|s| s.auto_start = auto);
                self.draft.auto_start = auto;
            }
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
                        && ui.button("断开全部串流").clicked()
                    {
                        self.host.disconnect();
                    }
                });
            }
        });
        if self.monitors.is_empty() && self.monitor_error.is_empty() {
            self.refresh_monitors();
        }
        let sessions=self.host.sessions();
        if !sessions.is_empty() {
            card(ui,|ui| {
                ui.strong(format!("已连接 {} / {} 个会话",sessions.len(),uu_core::shared_host::MAX_SESSIONS));
                muted(ui,"多台设备可同时观看；键鼠由当前取得控制权的一台设备操作，释放或离线后交接。");
                for session in sessions {
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(&session.name);
                        let mut control=session.control;
                        if ui.checkbox(&mut control,"允许控制键鼠").changed() {
                            match self.host.set_session_control(&session.peer_id,control) {
                                Ok(())=>self.feedback.success(
                                    super::ui_feedback::Scope::Services(0),
                                    "已更新设备控制权限；该设备需要重新连接",
                                ),
                                Err(error)=>self.feedback.error(
                                    super::ui_feedback::Scope::Services(0),
                                    format!("无法更新设备控制权限：{error:#}"),
                                ),
                            }
                        }
                        if ui.small_button("断开此会话").clicked(){self.host.disconnect_session(&session.id);}
                    });
                    muted(ui,match session.status {
                        HostStatus::Starting|HostStatus::Preparing=>"正在准备串流…".into(),
                        HostStatus::Waiting{..}=>"等待控制端建立媒体连接…".into(),
                        HostStatus::Streaming{width,height,fps,encoder,audio}=>format!("{width}×{height} · {fps} FPS · {encoder} · {audio}"),
                    });
                }
            });
        }
    }
    pub(super) fn server_settings(&mut self, ui: &mut egui::Ui, info: &Info, _actions: &mut Vec<Action>) {
        if self.monitors.is_empty() && self.monitor_error.is_empty() {
            self.refresh_monitors();
        }
        let locked = self.host.running();
        card(ui, |ui| {
            ui.strong(match self.host_section {0=>"采集与声音",1=>"编码与串流限制",_=>"访问权限"});
            ui.add_space(10.);
            ui.add_enabled_ui(!locked, |ui| {
                if self.host_section==0 {
                ui.add_enabled_ui(!info.login_busy, |ui| field(ui, "本机名称", &mut self.draft.name, "设备名称"));
                ui.label("共享屏幕");
                let portal=uu_core::native::uses_desktop_portal();
                egui::ComboBox::from_id_salt("shared-monitor")
                    .selected_text(if self.draft.monitor==uu_core::native_displays::ALL_MONITORS {
                        if portal {"系统选择（允许远程请求重选）"}else{"所有显示器（允许远程切换）"}
                    }else if self.draft.monitor.is_empty(){if portal{"系统选择一个屏幕"}else{"主显示器"}}else{&self.draft.monitor})
                    .show_ui(ui,|ui|{
                        ui.selectable_value(&mut self.draft.monitor,String::new(),if portal{"系统选择一个屏幕"}else{"主显示器"});
                        ui.selectable_value(&mut self.draft.monitor,uu_core::native_displays::ALL_MONITORS.into(),if portal{"系统选择（允许远程请求重选）"}else{"所有显示器（允许远程切换）"});
                        if !portal {for monitor in &self.monitors {
                            ui.selectable_value(&mut self.draft.monitor,monitor.name.clone(),format!("{} · {}×{}",monitor.name,monitor.width,monitor.height));
                        }}
                    });
                if portal {muted(ui,"Wayland 由系统授权屏幕；远程重选仍需本机确认，控制权限与屏幕使用同一会话。");}
                if !self.monitor_error.is_empty(){ui.colored_label(theme::DANGER,&self.monitor_error);}
                if !portal && ui.small_button("刷新屏幕").clicked(){self.refresh_monitors();}
                ui.checkbox(&mut self.draft.audio,"共享系统声音");
                }
                if self.host_section==1 {
                ui.horizontal(|ui| {
                    ui.label("最大帧率");
                    ui.add(
                        egui::DragValue::new(&mut self.draft.fps)
                            .range(1..=60)
                            .suffix(" FPS"),
                    );
                    ui.add_space(18.);
                    ui.label("默认码率");
                    ui.add(
                        egui::DragValue::new(&mut self.draft.bitrate_mbps)
                            .range(1..=100)
                            .suffix(" Mbps"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("最大码率");
                    ui.add(egui::DragValue::new(&mut self.draft.max_bitrate_mbps).range(1..=100).suffix(" Mbps"));
                });
                muted(ui,"客户端可请求更低的帧率、码率和输出分辨率；当前原生链路最高 60 FPS。");
                ui.horizontal_wrapped(|ui| {
                    ui.label("允许协商的编码");
                    for (codec,label) in [(uu_core::native::VideoCodec::H264,"H.264"),(uu_core::native::VideoCodec::Hevc,"HEVC"),(uu_core::native::VideoCodec::Av1,"AV1")] {
                        let mut enabled=self.draft.allowed_codecs.contains(&codec);
                        if ui.checkbox(&mut enabled,label).changed(){if enabled{self.draft.allowed_codecs.push(codec);}else{self.draft.allowed_codecs.retain(|c|*c!=codec);}}
                    }
                });
                muted(ui,"仅通报实际初始化成功的编码。HEVC / AV1 需要可用硬件编码，允许不代表驱动一定支持。");
                ui.horizontal_wrapped(|ui|{
                    ui.label("默认编码");
                    egui::ComboBox::from_id_salt("shared-codec").selected_text(match self.draft.codec {None=>"自动协商",Some(uu_core::native::VideoCodec::H264)=>"H.264",Some(uu_core::native::VideoCodec::Hevc)=>"H.265 / HEVC",Some(uu_core::native::VideoCodec::Av1)=>"AV1"}).show_ui(ui,|ui|{
                        ui.selectable_value(&mut self.draft.codec,None,"自动协商");
                        ui.selectable_value(&mut self.draft.codec,Some(uu_core::native::VideoCodec::H264),"H.264");
                        ui.selectable_value(&mut self.draft.codec,Some(uu_core::native::VideoCodec::Hevc),"H.265 / HEVC");
                        ui.selectable_value(&mut self.draft.codec,Some(uu_core::native::VideoCodec::Av1),"AV1");
                    });
                    ui.label("码率模式");
                    egui::ComboBox::from_id_salt("shared-rate-control").selected_text(match self.draft.rate_control {RateMode::Cbr=>"CBR",RateMode::Vbr=>"VBR"}).show_ui(ui,|ui|{
                        ui.selectable_value(&mut self.draft.rate_control,RateMode::Cbr,"CBR · 稳定预算");
                        ui.selectable_value(&mut self.draft.rate_control,RateMode::Vbr,"VBR · 允许峰值");
                    });
                });
                muted(ui,"服务器明确选择时优先使用服务器格式；服务器自动时遵循客户端选择；双方自动按 AV1 → HEVC → H.264 尝试。明确请求不可用时提示原因。");
                self.draft.peak_bitrate_mbps=self.draft.peak_bitrate_mbps.max(self.draft.bitrate_mbps);
                if self.draft.rate_control==RateMode::Vbr {ui.horizontal(|ui|{ui.label("峰值码率");ui.add(egui::DragValue::new(&mut self.draft.peak_bitrate_mbps).range(self.draft.bitrate_mbps..=200).suffix(" Mbps"));});}
                let needs_hardware=self.draft.codec.is_some_and(|c|c!=uu_core::native::VideoCodec::H264) || self.draft.rate_control==RateMode::Vbr;
                if needs_hardware && self.draft.encoding==Encoding::Software {self.draft.encoding=Encoding::Auto;}
                ui.horizontal(|ui| {
                    ui.label("编码加速");
                    egui::ComboBox::from_id_salt("shared-encoder")
                        .selected_text(match self.draft.encoding {
                            Encoding::Auto => "自动",
                            Encoding::Hardware => "硬件",
                            Encoding::Intel => "Intel QSV",
                            Encoding::Nvidia => "NVIDIA 硬件",
                            Encoding::Amd => "AMD 硬件",
                            Encoding::Software => "软件",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.draft.encoding, Encoding::Auto, "自动");
                            ui.selectable_value(
                                &mut self.draft.encoding,
                                Encoding::Hardware,
                                "硬件",
                            );
                            for (vendor,label) in [(Encoding::Intel,"Intel QSV"),(Encoding::Nvidia,"NVIDIA 硬件"),(Encoding::Amd,"AMD 硬件")] {ui.selectable_value(&mut self.draft.encoding,vendor,label);}
                            ui.add_enabled_ui(!needs_hardware,|ui|{ui.selectable_value(&mut self.draft.encoding,Encoding::Software,"软件");});
                        });
                });
                if needs_hardware {
                    muted(ui, if self.draft.codec==Some(uu_core::native::VideoCodec::H264) {
                        "H.264 VBR 需要硬件编码；Windows 自动模式可使用 Media Foundation，显式 Intel 模式使用 VPL。控制端需支持相同格式。"
                    } else {
                        "HEVC / AV1 当前需要 Intel VPL 硬件编码；控制端需支持相同格式，连接时自动核验。"
                    });
                }
                }
                if self.host_section==2 {
                ui.checkbox(&mut self.draft.control, "允许已授权设备控制键盘、鼠标和手柄");
                ui.checkbox(&mut self.draft.clipboard, "允许双方启用文本剪贴板");
                }
            });
            if locked {
                muted(ui,"停止共享后可修改设置。");
            }
        });
        ui.add_space(14.);
        if self.host_section==2 {self.grants(ui);}
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
            muted(ui,"这些设备通过 IP、UU 或 VPS 连接时，都使用相同的身份和权限。");
            ui.add_space(10.);
            let active: Vec<_> = peers.iter().filter(|p| p.inbound.is_some()).collect();
            if active.is_empty() {
                muted(ui,"还没有已授权的控制端。开启共享后，允许新设备配对即可添加。");
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
                            match result {
                                Ok(()) => self.feedback.success(
                                    super::ui_feedback::Scope::Services(self.service_tab),
                                    "权限已保存，重新连接后生效",
                                ),
                                Err(e) => self.feedback.error(
                                    super::ui_feedback::Scope::Services(self.service_tab),
                                    format!("无法修改权限：{e}"),
                                ),
                            }
                        }
                        if ui.small_button("撤销授权").clicked() {
                            let result = if let Some(access) = &self.host.access {
                                access.revoke(&peer.device.id())
                            } else {
                                self.trust.as_ref().unwrap().revoke(&peer.device.id())
                            };
                            match result {
                                Ok(()) => self.feedback.success(
                                    super::ui_feedback::Scope::Services(self.service_tab),
                                    "已撤销设备授权",
                                ),
                                Err(e) => self.feedback.error(
                                    super::ui_feedback::Scope::Services(self.service_tab),
                                    format!("撤销失败：{e}"),
                                ),
                            }
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
                    muted(ui,"旧版 IP 配对");
                    if ui.small_button("撤销授权").clicked() {
                        self.host.revoke_legacy(&legacy.id);
                    }
                });
            }
        });
    }
    fn services(&mut self, ui: &mut egui::Ui, info: &Info, actions: &mut Vec<Action>) {
        let locked = self.host.running();
        if locked { muted(ui,"服务正在运行；停止服务后可修改连接入口。"); }
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("IP 直连");
                let mut enabled = self.book.sharing.ip_enabled;
                if ui.add_enabled(!locked, egui::Checkbox::new(&mut enabled,"启用")).changed() {
                    self.update_connections(|s| s.ip_enabled=enabled);
                }
            });
            muted(ui,"同一网络或已有路由时直接连接，默认开启。");
            let mut tcp=self.book.sharing.service_port;
            let mut udp=self.book.sharing.discovery_port;
            ui.add_enabled_ui(!locked, |ui| {
                egui::Grid::new("connection-ip-ports").spacing([20.,10.]).show(ui,|ui| {
                    ui.label("服务端口 · TCP"); ui.add(egui::DragValue::new(&mut tcp).range(1..=65535)); ui.end_row();
                    ui.label("发现端口 · UDP"); ui.add(egui::DragValue::new(&mut udp).range(1..=65535)); ui.end_row();
                });
            });
            if tcp!=self.book.sharing.service_port || udp!=self.book.sharing.discovery_port {
                self.update_connections(|s| {s.service_port=tcp; s.discovery_port=udp;});
            }
            self.endpoint_state(ui,"IP 直连");
            ui.collapsing("本机地址",|ui| {
                if let Ok(addresses)=super::native_interfaces::enumerate() {
                    for address in addresses.into_iter().filter(|a| a.ip.is_ipv4()) {
                        let endpoint=format!("{}:{}",address.ip,self.book.sharing.service_port);
                        ui.horizontal(|ui| {ui.monospace(&endpoint); if ui.small_button("复制").clicked(){ui.ctx().copy_text(endpoint);}});
                    }
                }
                muted(ui,"手动输入可达地址支持跨网段；自动发现通常不跨路由。");
            });
        });
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("UU 账号连接服务");
                let mut enabled=self.book.sharing.uu_enabled;
                if ui.add_enabled(!locked,egui::Checkbox::new(&mut enabled,"启用")).changed(){self.update_connections(|s|s.uu_enabled=enabled);}
            });
            self.endpoint_state(ui,"UU 账号");
            ui.add_space(8.);
            ui.horizontal(|ui| {
                ui.label("UU 本机名称");
                ui.monospace(uu_core::config::uu_device_name());
                if info.logged_in && ui.add_enabled(!info.login_busy, egui::Button::new("更新注册信息")).clicked() {
                    actions.push(Action::RenameUu);
                }
            });
            muted(ui,"自动使用电脑名加 -Eclipse，便于与官方客户端区分；原生共享名称独立设置。");
            if info.logged_in {
                ui.label(RichText::new("已登录").color(theme::SUCCESS));
                if ui.button("管理 UU 设备").clicked() { actions.push(Action::ManageUu); }
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
            if info.login_busy { ui.spinner(); }
            if !info.login_status.is_empty() { ui.label(&info.login_status); }
            ui.add_space(8.);
            muted(ui,"启动服务后，启用的入口会发布本机。连接仍需经过设备配对授权。");
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("自建服务器").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui| {
                if ui.add_enabled(!locked,egui::Button::new(RichText::new("＋ 添加服务器").color(theme::ACCENT))).clicked(){self.edit_service(None);}
            });
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
                            muted(ui,&service.address);
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
                                        Err(e) => self.feedback.error(self.device_scope(), format!("无法移除：{e}")),
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
                    let mut enabled=service.host_enabled;
                    if ui.add_enabled(!locked,egui::Checkbox::new(&mut enabled,"启用此连接服务")).changed() {
                        if let Some(store)=&self.store {
                            let result=store.edit(|book| {
                                let item=book.services.iter_mut().find(|s|s.id==service.id).ok_or_else(||anyhow::anyhow!("服务器已移除"))?;
                                item.host_enabled=enabled; Ok(())
                            });
                            if let Err(e)=result {
                                self.feedback.error(
                                    super::ui_feedback::Scope::Services(0),
                                    format!("无法保存：{e:#}"),
                                );
                            }
                        }
                        self.refresh();
                    }
                    self.endpoint_state(ui,&service.name);
                    muted(ui,if service.host_enabled {
                        "共享开启时，会在此服务器发布本机"
                    } else {
                        "仅用于连接其它设备"
                    });
                });
            });
        }
        if self.host.running() {
            muted(ui,"停止共享后可修改服务器配置。");
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
    pub(super) fn assistance_page(&mut self, ui: &mut egui::Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        self.feedback
            .show(ui, super::ui_feedback::Scope::Compatibility);
        self.assistance(ui, &mut actions);
        actions
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
            muted(ui,"需登录 UU 账号。验证码只用于本次连接，不保存为设备授权。");
        });
        ui.collapsing("连接原生设备", |ui| {
            ui.label("被控端开启“允许新设备配对”，控制端添加 IP、UU 或 VPS 设备并连接，再把八位配对码交给被控端输入。");
            if primary(ui, "添加设备").clicked() {self.route.page=Page::Devices;self.new_device(None,None);}
            if ui.button("让别人连接本机").clicked(){self.route.page=Page::Services;}
        });
    }
    pub fn dialogs(&mut self, ctx: &egui::Context) -> Vec<Action> {
        let mut actions = Vec::new();
        self.host.dialogs(ctx);
        if let Some(mut editor)=self.client_editor.take() {
            let mut open=true; let mut save=false; let mut cancel=false;
            egui::Window::new(if editor.global {"全局默认客户端配置"} else {"设备客户端配置"}).id("device-client-editor".into()).open(&mut open)
                .default_width(720.).default_height(600.).max_height((ctx.screen_rect().height()-70.).max(200.)).max_width((ctx.screen_rect().width()-40.).max(280.))
                .collapsible(false).show(ctx,|ui| {
                    if !editor.error.is_empty(){ui.colored_label(theme::DANGER,&editor.error);}
                    if !editor.global && ui.small_button("编辑全局默认").clicked() {
                        editor.global = true;
                        editor.device.clear();
                        editor.value = uu_core::client_settings::NamedClientSettings::new(
                            "全局默认",
                            self.client_defaults.clone(),
                        );
                    }
                    if editor.global {
                        muted(ui,"全局默认会用于未指定独立配置的设备；修改只影响下一次连接。");
                    } else {
                        field(ui,"配置名称",&mut editor.value.name,"例如：桌面 4K / 游戏 120 FPS");
                        muted(ui,"仅用于这台设备，不改变本机共享设置和其它设备。");
                    }
                    ui.separator();
                    egui::ScrollArea::vertical().id_salt("device-client-fields").max_height((ui.available_height()-70.).max(80.).min((ctx.screen_rect().height()-260.).max(80.))).show(ui,|ui| {
                        super::settings_page::client_editor(ui,&mut editor.value.settings,&mut editor.section);
                    });
                    ui.separator();
                    ui.horizontal(|ui| {save=primary(ui,"保存配置").clicked();cancel=ui.button("取消").clicked();});
                });
            if save {
                if editor.global {
                    actions.push(Action::SaveClientDefaults(editor.value.settings.clone()));
                } else {
                    let result=self.store.as_ref().ok_or_else(||anyhow::anyhow!("设备配置不可用")).and_then(|store| {
                        store.save_and_select_client_config(&editor.device,editor.value.clone())
                    });
                    match result {
                        Ok(())=>{
                            let scope=self.device_scope();
                            self.feedback.success(scope,"设备配置已保存；下次连接生效");
                            self.refresh();
                        }
                        Err(e)=>{editor.error=format!("无法保存：{e:#}");self.client_editor=Some(editor);}
                    }
                }
            }else if open && !cancel {self.client_editor=Some(editor);}
        }
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
                    1 => {
                        if !self.uu_choices.is_empty() {
                            ui.label("选择账号中的设备");
                            let selected = self
                                .uu_choices
                                .iter()
                                .find(|(id, _)| id == &editor.device)
                                .map(|(_, name)| name.as_str())
                                .unwrap_or("手动输入设备编号");
                            egui::ComboBox::from_id_salt("route-uu-device")
                                .selected_text(selected)
                                .width(360.0)
                                .show_ui(ui, |ui| {
                                    for (id, name) in &self.uu_choices {
                                        if ui
                                            .selectable_label(editor.device == *id, format!("{name} · {id}"))
                                            .clicked()
                                        {
                                            editor.device = id.clone();
                                            if editor.name.trim().is_empty() {
                                                editor.name = name.clone();
                                            }
                                        }
                                    }
                                });
                            ui.collapsing("手动输入设备编号", |ui| {
                                field(ui, "目标 UU 设备编号", &mut editor.device, "被控端“连接服务”中的本机 UU 编号");
                            });
                        } else {
                            field(
                                ui,
                                "目标 UU 设备编号",
                                &mut editor.device,
                                "被控端“连接服务”中的本机 UU 编号",
                            );
                            muted(ui, "登录 UU 账号后可在这里选择设备列表。");
                        }
                    }
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
                muted(ui,"首次连接时确认设备身份，以后不再重复配对。");
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
                    anyhow::ensure!(!self.host.running(),"请先停止本机服务，再修改连接服务器");
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
                        self.feedback.success(
                            super::ui_feedback::Scope::Services(0),
                            "服务器配置已保存",
                        );
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
    pub(super) fn toggle_shared_host(&mut self, ctx:&egui::Context) {
        if self.workspace.host.running() { self.workspace.host.stop(); }
        else { self.workspace_actions(vec![Action::StartSharing],ctx); }
    }
    pub(super) fn autostart_shared_host(&mut self, ctx:&egui::Context) {
        if self.workspace.autostart_checked {return;}
        self.workspace.autostart_checked=true;
        if self.stream_target.is_none() && self.workspace.book.sharing.auto_start && !self.exit_requested {
            self.workspace_actions(vec![Action::StartSharing],ctx);
        }
    }
    pub(super) fn select_workspace_page(&mut self, page: Page) {
        let previous = self.workspace.route.page;
        if previous == Page::Settings && page != Page::Settings {
            self.discard_home_settings();
        }
        if page == Page::Settings && previous != Page::Settings {
            self.workspace.route.settings_return = previous;
            self.prepare_home_settings();
        }
        self.workspace.route.page = page;
        self.workspace.route.device = DeviceView::List;
        self.workspace.editor = None;
        self.workspace.service_editor = None;
        self.context.request_repaint();
    }
    pub(super) fn leave_settings(&mut self) {
        let return_page = self.workspace.route.settings_return;
        self.discard_home_settings();
        self.workspace.route.page = return_page;
        self.workspace.route.device = DeviceView::List;
        self.context.request_repaint();
    }
    pub(super) fn select_uu_compatibility(&mut self, assistance: bool) {
        if self.workspace.route.page == Page::Settings {
            self.discard_home_settings();
        }
        self.workspace.route.page = Page::Compatibility;
        self.workspace.route.compatibility = if assistance {
            CompatibilityView::Assistance
        } else {
            CompatibilityView::Devices
        };
        self.workspace.route.device = DeviceView::List;
        if !assistance && self.logged_in && self.devices.is_empty() {
            self.start_load();
        }
        self.workspace.editor = None;
        self.workspace.service_editor = None;
        self.context.request_repaint();
    }
    pub(super) fn compatibility_active(&self) -> bool {
        self.workspace.route.page == Page::Compatibility
    }
    pub(super) fn workspace_info(&self)->Info {
        Info {
            logged_in:self.logged_in,
            uu_id:self.config.as_ref().and_then(|c|uu_core::ClientState::read(&c.at("state.json")).ok().flatten()).and_then(|s|s.device_id),
            qr:self.login_texture.as_ref().map(|t|t.id()),
            login_status:self.account_error.clone().unwrap_or_else(||self.login_status.clone()),
            login_busy:self.login_rx.is_some(), nearby:self.lan.nearby(self.workspace.host.server_id()),
            uu_devices:self.devices.iter().map(|device|(device.device_id.clone(),device.alias.clone())).collect(),
        }
    }
    pub(super) fn workspace_home(&mut self, ctx: &egui::Context) {
        let info = self.workspace_info();
        let wallpaper_root = self.workspace.route.page == Page::Devices
            && matches!(self.workspace.route.device, DeviceView::List)
            && self.preferences.wallpaper_enabled;
        self.workspace.client_defaults = self.preferences.client.clone();
        self.workspace.dark_background = wallpaper_root;
        let mut actions = Vec::new();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(if wallpaper_root { egui::Color32::TRANSPARENT } else { theme::CANVAS })
                    .inner_margin(24.),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    actions = self.workspace.show(ui, &info);
                    if wallpaper_root && !self.background.notice.is_empty() {
                        muted(ui, &self.background.notice);
                    }
                });
            });
        self.workspace_actions(actions, ctx);
    }
    pub(super) fn workspace_actions(&mut self, actions: Vec<Action>, ctx: &egui::Context) {
        for action in actions {
            match action {
                Action::Open(id) => self.open_device_profile(&id),
                Action::SaveClientDefaults(settings) => {
                    self.preferences.client = settings;
                    self.workspace.client_defaults = self.preferences.client.clone();
                    self.persist_preferences();
                    let scope = self.workspace.device_scope();
                    self.workspace.feedback.success(scope, "全局默认客户端配置已保存；下次连接生效");
                }
                Action::Login => self.start_login(),
                Action::RenameUu => self.rename_uu_device(),
                Action::ManageUu => {
                    if let (Some(config), Some(token)) = (self.config.clone(), self.account_token.clone()) {
                        self.uu_devices.open(config, token);
                    }
                }
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
                        self.workspace.feedback.error(
                            super::ui_feedback::Scope::Services(0),
                            format!("无法开始共享：{e:#}"),
                        );
                    }
                }
                Action::Assist(request) => {
                    if !self.logged_in {
                        self.workspace.assist_error =
                            "请先在 UU 兼容页登录账号，再发起远程协助。".into();
                        self.workspace.route.page = Page::Compatibility;
                        self.workspace.route.compatibility = CompatibilityView::Devices;
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
                                self.workspace.feedback.success(
                                    super::ui_feedback::Scope::Compatibility,
                                    "已打开远程协助窗口",
                                );
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
                self.workspace.route.page = Page::Services;
                self.start_login();
                return Ok(());
            }
            let snapshot = store.publish_launch_with_defaults(id,&self.preferences.client)?;
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
            self.workspace.feedback.error(
                self.workspace.device_scope(),
                format!("无法连接设备：{e:#}"),
            );
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
                muted(ui,"完整公钥会自动交换。配对完成后，以后连接无需重复输入。");
                ui.spinner();
                ui.label("等待被控端确认…");
            });
        if !open {
            self.disconnect();
        }
    }
}
