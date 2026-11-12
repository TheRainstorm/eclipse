//! A service-scoped authenticated directory is a route source, never desktop trust.
use super::received;
use eframe::egui;
use std::{
    collections::HashMap,
    sync::mpsc,
    time::{Duration, Instant},
};
use uu_core::{
    device_book::{Book, Destination, Profile, Service, Store},
    Config,
};

type Devices = Vec<uu_core::internet::self_hosted::Device>;
enum Update {
    Service { id: String, result: Result<Devices, String> },
    Finished,
}
#[derive(Default)]
pub(crate) struct Directory {
    local_book: Option<Book>,
    local_read: Option<mpsc::Receiver<Result<Book, String>>>,
    read_at: Option<Instant>,
    entries: HashMap<String, Result<Devices, String>>,
    pending: Option<mpsc::Receiver<Update>>,
    awaiting: Vec<String>,
    open: Option<mpsc::Receiver<Result<crate::sessions::StreamTarget, String>>>,
    key: String,
    next: Option<Instant>,
    error: String,
}
impl Directory {
    fn poll_results(&mut self) {
        while let Some(message) = received(&self.pending) {
            match message {
                Ok(Update::Service { id, result }) => {
                    if self.awaiting.iter().any(|pending| pending == &id) {
                        self.awaiting.retain(|pending| pending != &id);
                        self.entries.insert(id, result);
                    }
                }
                result => {
                    let error = match result {
                        Err(error) => Some(error),
                        _ if !self.awaiting.is_empty() => Some(uu_i18n::text("后台操作未返回结果，请刷新确认状态。").into()),
                        _ => None,
                    };
                    if let Some(error) = error {
                        for id in self.awaiting.drain(..) { self.entries.insert(id, Err(error.clone())); }
                        self.error = error;
                    }
                    self.pending = None;
                    self.next = Some(Instant::now() + Duration::from_secs(5));
                }
            }
        }
    }
    #[cfg(test)]
    pub(crate) fn seed_fixture(&mut self, book: Book, entries: HashMap<String, Result<Devices, String>>) {
        self.key = serde_json::to_string(&book.services).unwrap();
        self.entries = entries;
        self.local_book = Some(book);
        self.next = Some(Instant::now() + Duration::from_secs(3600));
        self.read_at = self.next;
    }
    pub(crate) fn take_open(&mut self) -> Option<crate::sessions::StreamTarget> {
        let result = received(&self.open)?.and_then(|result| result);
        self.open = None;
        match result {
            Ok(target) => Some(target),
            Err(error) => { self.error = error; None }
        }
    }
    fn start_open(
        &mut self,
        config: Config,
        service: Service,
        device: uu_core::internet::self_hosted::Device,
        context: egui::Context,
    ) -> bool {
        if self.open.is_some() { return false; }
        // One accepted click owns one immutable request and one result slot.
        // Saving routes and publishing the launch snapshot stay off the UI.
        let (send, receive) = mpsc::sync_channel(1);
        match std::thread::Builder::new().name("eclipse-connect-prepare".into()).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                save_device(&config, &service, &device.device, &device.name)
                    .and_then(|id| prepare_profile(&config, &id))
                    .map_err(|error| format!("{error:#}"))
            })).unwrap_or_else(|_| Err(uu_i18n::text("后台操作未返回结果，请刷新确认状态。").into()));
            if let Err(error) = send.send(result) {
                if let Ok(crate::sessions::StreamTarget { connection: crate::sessions::ConnectionTarget::NativeProfile { snapshot, .. }, .. }) = error.0 {
                    let _ = std::fs::remove_file(snapshot);
                }
            }
            context.request_repaint();
        }) {
            Ok(_) => {
                self.open = Some(receive);
                self.error.clear();
                true
            }
            Err(error) => {
                self.error = format!("{} {error}", uu_i18n::text("后台操作未返回结果，请刷新确认状态。"));
                false
            }
        }
    }
    pub(crate) fn reset(&mut self) {
        self.key.clear();
        self.next = None;
        self.read_at = None;
        self.local_read = None;
    }
    pub(crate) fn refresh(&mut self) {
        self.pending = None;
        self.awaiting.clear();
        self.next = None;
        self.read_at = None;
    }
    fn read_local(&mut self, ctx: &egui::Context, config: Option<&Config>) {
        if let Some(result) =
            received(&self.local_read).map(|result| result.and_then(|result| result))
        {
            self.local_read = None;
            match result {
                Ok(book) => self.local_book = Some(book),
                Err(error) => self.error = error,
            }
            self.read_at = Some(Instant::now() + Duration::from_secs(5));
        }
        if self.local_read.is_none() && self.read_at.is_none_or(|at| Instant::now() >= at) {
            if let Some(config) = config {
                let directory = config.at("device-trust");
                let context = ctx.clone();
                let (send, receive) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ = send.send(
                        Store::open(directory)
                            .and_then(|s| s.read())
                            .map_err(|e| e.to_string()),
                    );
                    context.request_repaint();
                });
                self.local_read = Some(receive);
            }
        }
    }
    pub(crate) fn show_local(&mut self, ui: &mut egui::Ui, config: Option<&Config>) -> Option<crate::sessions::StreamTarget> {
        self.read_local(ui.ctx(), config);
        let book = self.local_book.clone().unwrap_or_default();
        self.show(ui, config, &book)
    }
    pub(crate) fn show_service(&mut self, ui: &mut egui::Ui, config: Option<&Config>, address: &str) -> Option<crate::sessions::StreamTarget> {
        self.read_local(ui.ctx(), config);
        let book = self.local_book.clone().unwrap_or_default();
        self.show_filtered(ui, config, &book, Some(address))
    }
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        config: Option<&Config>,
        book: &Book,
    ) -> Option<crate::sessions::StreamTarget> {
        self.show_filtered(ui, config, book, None)
    }
    fn show_filtered(&mut self, ui: &mut egui::Ui, config: Option<&Config>, book: &Book, address: Option<&str>) -> Option<crate::sessions::StreamTarget> {
        let open = self.take_open();
        let key = serde_json::to_string(&book.services).unwrap_or_default();
        if key != self.key {
            self.key = key;
            self.entries.clear();
            self.pending = None;
            self.awaiting.clear();
            self.next = None;
        }
        self.poll_results();
        if self.pending.is_none()
            && !book.services.is_empty()
            && self.next.is_none_or(|at| Instant::now() >= at)
        {
            let services = book.services.clone();
            self.awaiting = services.iter().map(|service| service.id.clone()).collect();
            let context = ui.ctx().clone();
            let (send, receive) = mpsc::channel();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                if let Ok(runtime) = runtime {
                    runtime.block_on(async {
                        let mut tasks = tokio::task::JoinSet::new();
                        for service in services {
                            tasks.spawn(async move {
                                let settings = uu_core::internet::Settings {
                                    server: service.address,
                                    server_token: service.token_file,
                                    server_ca: service.certificate,
                                    ..Default::default()
                                };
                                let result = match settings.self_hosted() {
                                    Ok(config) => {
                                        uu_core::internet::self_hosted::directory(&config)
                                            .await
                                            .map_err(|error| format!("{error:#}"))
                                    }
                                    Err(error) => Err(error.to_string()),
                                };
                                (service.id, result)
                            });
                        }
                        while let Some(result) = tasks.join_next().await {
                            if let Ok((id, result)) = result {
                                if send.send(Update::Service { id, result }).is_err() { return; }
                                context.request_repaint();
                            }
                        }
                        let _ = send.send(Update::Finished);
                    });
                }
                context.request_repaint();
            });
            self.pending = Some(receive);
        }
        if address.is_none() { ui.horizontal(|ui| {
            ui.strong(uu_i18n::text("在线设备"));
            if ui
                .add(crate::theme::secondary_button(uu_i18n::text("刷新")).small())
                .clicked()
            {
                self.next = None;
            }
        }); }
        if !self.error.is_empty() {
            ui.colored_label(crate::theme::palette(ui.visuals()).danger, &self.error);
        }
        if book.services.is_empty() && address.is_none() {
            ui.weak(uu_i18n::text(
                "加入同一连接服务后，可在这里选择另一台设备。",
            ));
        }
        let mut requested = None;
        let services: Vec<_> = book.services.iter().filter(|service| address.is_none_or(|address| super::address_key(&service.address) == super::address_key(address))).collect();
        if address.is_some() && services.is_empty() {
            ui.weak(uu_i18n::text(if self.local_read.is_some() { "正在读取在线设备…" } else { "本机尚未保存此服务器的连接配置，请重新导入接入码。" }));
        }
        for service in services {
            ui.push_id(&service.id, |ui| {
                if address.is_none() { ui.label(&service.name); }
                match self.entries.get(&service.id) {
                    Some(Ok(devices)) => {
                        let devices: Vec<_> = devices
                            .iter()
                            .filter(|device| device.device != service.published_name)
                            .collect();
                        if devices.is_empty() {
                            ui.weak(uu_i18n::text(
                                "暂无其他在线设备，请让另一台电脑加入同一服务。",
                            ));
                        }
                        for device in devices {
                            ui.horizontal(|ui| {
                                ui.label(&device.name);
                                if ui
                                    .add_enabled(config.is_some() && self.open.is_none(), crate::theme::primary_button(uu_i18n::text("连接")))
                                    .clicked()
                                {
                                    if let Some(config) = config {
                                        requested = Some((config.clone(), service.clone(), device.clone()));
                                    }
                                }
                            });
                        }
                    }
                    Some(Err(error)) => {
                        ui.weak(uu_i18n::text("在线设备暂不可用"));
                        ui.collapsing(uu_i18n::text("诊断详情"), |ui| {
                            ui.label(error);
                        });
                    }
                    None => {
                        ui.weak(uu_i18n::text("正在读取在线设备…"));
                    }
                }
            });
        }
        if let Some((config, service, device)) = requested {
            self.start_open(config, service, device, ui.ctx().clone());
        }
        if self.open.is_some() { ui.horizontal(|ui| { ui.spinner(); ui.label(uu_i18n::text("正在准备连接…")); }); }
        ui.ctx()
            .request_repaint_after(Duration::from_millis(if self.pending.is_some() || self.open.is_some() {
                100
            } else {
                1000
            }));
        open
    }
}
fn save_device(
    config: &Config,
    service: &Service,
    device: &str,
    name: &str,
) -> anyhow::Result<String> {
    let store = Store::open(config.at("device-trust"))?;
    let mut saved = None;
    store.edit(|book| {
        if let Some((index, route)) = book
            .devices
            .iter()
            .enumerate()
            .filter(|(_, p)| p.redirect.is_none())
            .find_map(|(index, profile)| {
                profile
                    .routes
                    .iter()
                    .find(|r| {
                        matches!(&r.destination, Destination::Vps { service: id, device: target }
                if id == &service.id && target == device)
                    })
                    .map(|route| (index, route.id.clone()))
            })
        {
            book.devices[index].selected_route = route;
            saved = Some(book.devices[index].id.clone());
        } else {
            let profile = Profile::new(
                name,
                Destination::Vps {
                    service: service.id.clone(),
                    device: device.into(),
                },
            );
            saved = Some(profile.id.clone());
            book.devices.push(profile);
        }
        Ok(())
    })?;
    saved.ok_or_else(|| anyhow::anyhow!("设备未保存"))
}

fn prepare_profile(config: &Config, id: &str) -> anyhow::Result<crate::sessions::StreamTarget> {
    let store = Store::open(config.at("device-trust"))?;
    let defaults = crate::settings::Settings::load(&config.at("gui-settings.json"))?.client;
    let launch = store.launch(id)?;
    let snapshot = store.publish_launch_with_defaults(id, &defaults)?;
    Ok(crate::sessions::StreamTarget {
            connection: crate::sessions::ConnectionTarget::NativeProfile {
                id: launch.profile,
                snapshot,
            },
            alias: launch.name,
            view_only: false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uu_core::internet::enrollment::{self, Enrollment};
    #[test]
    fn completed_services_are_visible_before_slow_services_and_stale_batches_are_discarded() {
        let (send, receive) = mpsc::channel();
        let mut directory = Directory { pending: Some(receive), awaiting: vec!["fast".into(), "slow".into()], ..Default::default() };
        send.send(Update::Service { id: "fast".into(), result: Ok(vec![uu_core::internet::self_hosted::Device { device: "fast-device".into(), name: "Fast device".into() }]) }).unwrap();
        directory.poll_results();
        assert_eq!(directory.entries["fast"].as_ref().unwrap()[0].name, "Fast device");
        assert!(directory.pending.is_some() && !directory.entries.contains_key("slow"), "a stalled service must not hide an already completed directory");
        send.send(Update::Service { id: "slow".into(), result: Err("controlled timeout".into()) }).unwrap();
        send.send(Update::Finished).unwrap();
        directory.poll_results();
        assert!(directory.pending.is_none() && directory.awaiting.is_empty());
        assert_eq!(directory.entries["slow"].as_ref().err().unwrap(), "controlled timeout");
        assert!(directory.entries["fast"].is_ok());

        let (lost, receive) = mpsc::channel();
        directory.pending = Some(receive); directory.awaiting = vec!["unfinished".into()];
        drop(lost); directory.poll_results();
        assert!(directory.pending.is_none() && directory.entries["unfinished"].is_err());
        assert!(directory.entries["fast"].is_ok(), "worker failure keeps completed service results");

        let (old, receive) = mpsc::channel();
        directory.pending = Some(receive); directory.awaiting = vec!["old-service".into()]; directory.key = "old configuration".into();
        old.send(Update::Service { id: "old-service".into(), result: Ok(vec![]) }).unwrap();
        let context = egui::Context::default();
        let _ = context.run(Default::default(), |ctx| { egui::CentralPanel::default().show(ctx, |ui| { directory.show(ui, None, &Book::default()); }); });
        assert!(directory.entries.is_empty() && directory.pending.is_none() && directory.awaiting.is_empty());
        assert!(old.send(Update::Finished).is_err(), "reconfigured UI retires the old result receiver");
    }
    struct Fixture {
        root: std::path::PathBuf,
        config: Config,
        service: Service,
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("eclipse-directory-ui-{}", uuid::Uuid::new_v4()));
            let config = Config {
                data_dir: root.join("data"),
                state_dir: root.join("state"),
                signaling_host: String::new(),
            };
            let invitation = Enrollment::new(
                "Fixture service".into(),
                enrollment::group_url("wss://signal.example/v1/signal", &uuid::Uuid::new_v4()).unwrap(),
                zeroize::Zeroizing::new("ab".repeat(32)),
                None,
            )
            .unwrap();
            let service = enrollment::import(
                &config.at("device-trust"),
                &invitation,
                enrollment::Publication::PreserveOrDefault("custom-room"),
                true,
            )
            .unwrap();
            Self {
                root,
                config,
                service,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn directory_hides_the_published_host_identity_instead_of_the_separate_client_identity() {
        let fixture = Fixture::new();
        let book = Store::open(fixture.config.at("device-trust"))
            .unwrap()
            .read()
            .unwrap();
        let mut directory = Directory {
            key: serde_json::to_string(&book.services).unwrap(),
            next: Some(Instant::now() + Duration::from_secs(60)),
            ..Default::default()
        };
        directory.entries.insert(
            fixture.service.id.clone(),
            Ok(vec![
                uu_core::internet::self_hosted::Device {
                    device: "custom-room".into(),
                    name: "Own host".into(),
                },
                uu_core::internet::self_hosted::Device {
                    device: "another-host".into(),
                    name: "Other host".into(),
                },
            ]),
        );
        let context = egui::Context::default();
        crate::theme::apply(&context);
        let draw = |context: &egui::Context, directory: &mut Directory| {
            egui::CentralPanel::default().show(context, |ui| {
                directory.show(ui, Some(&fixture.config), &book);
            });
        };
        let _ = context.run(Default::default(), |context| draw(context, &mut directory));
        let output = context.run(Default::default(), |context| draw(context, &mut directory));
        fn text(shape: &egui::Shape, values: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(value) => values.push(value.galley.job.text.clone()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        text(shape, values);
                    }
                }
                _ => {}
            }
        }
        let mut labels = Vec::new();
        for shape in output.shapes {
            text(&shape.shape, &mut labels);
        }
        assert!(labels.iter().any(|s| s == "Other host"));
        assert!(!labels.iter().any(|s| s == "Own host"));
        assert!(
            directory.pending.is_none(),
            "the fixture must not query any real server"
        );
    }
    #[test]
    fn preparing_a_directory_target_freezes_one_request_and_reports_worker_exit() {
        fn finish(directory: &mut Directory) -> Option<crate::sessions::StreamTarget> {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let target = directory.take_open();
                if directory.open.is_none() { return target; }
                assert!(Instant::now() < deadline, "directory worker did not return");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        let fixture = Fixture::new();
        let context = egui::Context::default();
        let mut directory = Directory::default();
        let device = uu_core::internet::self_hosted::Device {
            device: "frozen-remote".into(), name: "Frozen target".into(),
        };
        assert!(directory.start_open(fixture.config.clone(), fixture.service.clone(), device, context.clone()));
        assert!(!directory.start_open(fixture.config.clone(), fixture.service.clone(),
            uu_core::internet::self_hosted::Device { device: "ignored-duplicate".into(), name: "Later target".into() }, context.clone()));
        let target = finish(&mut directory).expect("worker must return a prepared stream target");
        assert_eq!(target.alias, "Frozen target");
        let crate::sessions::ConnectionTarget::NativeProfile { id, snapshot } = target.connection else {
            panic!("directory must prepare a native profile");
        };
        let launch = uu_core::device_book::Launch::read(&snapshot).unwrap();
        assert_eq!(launch.profile, id);
        assert!(matches!(launch.route.destination, Destination::Vps { service, device }
            if service == fixture.service.id && device == "frozen-remote"));
        assert!(launch.client.is_some(), "client defaults must be frozen into the worker's snapshot");
        let book = Store::open(fixture.config.at("device-trust")).unwrap().read().unwrap();
        assert_eq!(book.devices.len(), 1, "busy clicks cannot create extra routes");
        assert!(directory.take_open().is_none(), "a prepared target is delivered once");

        // A real preparation error also releases the slot, without launching.
        std::fs::write(fixture.config.at("gui-settings.json"), "invalid settings fixture").unwrap();
        assert!(directory.start_open(fixture.config.clone(), fixture.service.clone(),
            uu_core::internet::self_hosted::Device { device: "frozen-remote".into(), name: "Existing target".into() }, context));
        assert!(finish(&mut directory).is_none());
        assert!(!directory.error.is_empty());

        // A worker which exits before returning must never leave the UI busy.
        let (send, receive) = mpsc::sync_channel(1);
        directory.open = Some(receive);
        drop(send);
        assert!(directory.take_open().is_none());
        assert!(directory.open.is_none());
        assert!(!directory.error.is_empty());
    }
    #[test]
    fn repeated_directory_selection_reuses_routes_without_replacing_user_preferences() {
        let fixture = Fixture::new();
        let first = save_device(
            &fixture.config,
            &fixture.service,
            "remote-host",
            "First label",
        )
        .unwrap();
        let store = Store::open(fixture.config.at("device-trust")).unwrap();
        store
            .edit(|book| {
                let profile = book.devices.iter_mut().find(|p| p.id == first).unwrap();
                profile.name = "Saved alias".into();
                Ok(())
            })
            .unwrap();
        let second = save_device(
            &fixture.config,
            &fixture.service,
            "remote-host",
            "New online label",
        )
        .unwrap();
        assert_eq!(first, second);
        let book = store.read().unwrap();
        assert_eq!(book.devices.len(), 1);
        assert_eq!(book.devices[0].name, "Saved alias");
        assert!(
            book.devices[0].peer.is_none(),
            "directory lookup does not grant or manufacture pairing trust"
        );
    }
}
