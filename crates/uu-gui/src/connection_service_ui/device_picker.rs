//! One selected service owns one directory query. Changing that service cancels
//! the old query and invalidates its selection; only fresh results may be saved.
use eframe::egui;
use std::{path::PathBuf, time::Duration};
use tokio::sync::watch;
use uu_core::{device_book::Service, internet::self_hosted::Device};

#[derive(Clone, PartialEq, Eq)]
struct Key {
    id: String,
    address: String,
    token: PathBuf,
    certificate: Option<PathBuf>,
    published: String,
}
impl From<&Service> for Key {
    fn from(service: &Service) -> Self {
        Self {
            id: service.id.clone(),
            address: service.address.clone(),
            token: service.token_file.clone(),
            certificate: service.certificate.clone(),
            published: service.published_name.clone(),
        }
    }
}
#[derive(Clone)]
struct Request {
    epoch: u64,
    key: Key,
    service: Service,
}
#[derive(Clone)]
enum Reply {
    Devices {
        epoch: u64,
        key: Key,
        result: Result<Vec<Device>, String>,
    },
    Failed(String),
}
struct Worker {
    request: watch::Sender<Option<Request>>,
    results: watch::Receiver<Option<Reply>>,
}
impl Worker {
    fn new(context: egui::Context) -> std::io::Result<Self> {
        let (request, input) = watch::channel(None);
        let (output, results) = watch::channel(None);
        std::thread::Builder::new()
            .name("eclipse-device-directory".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| e.to_string())?;
                    runtime.block_on(run(input, &output, &context));
                    Ok::<(), String>(())
                }))
                .unwrap_or_else(|_| {
                    Err(uu_i18n::text("后台操作未返回结果，请刷新确认状态。").into())
                });
                if let Err(error) = result {
                    output.send_replace(Some(Reply::Failed(error)));
                }
                context.request_repaint();
            })?;
        Ok(Self { request, results })
    }
}
async fn run(
    mut input: watch::Receiver<Option<Request>>,
    output: &watch::Sender<Option<Reply>>,
    context: &egui::Context,
) {
    let mut next = None;
    loop {
        if next.is_none() {
            if input.changed().await.is_err() {
                break;
            }
            next = input.borrow_and_update().clone();
        }
        let Some(request) = next.take() else {
            continue;
        };
        let query = async {
            let settings = uu_core::internet::Settings {
                server: request.service.address.clone(),
                server_token: request.service.token_file.clone(),
                server_ca: request.service.certificate.clone(),
                ..Default::default()
            };
            let endpoint = settings.self_hosted()?;
            uu_core::internet::self_hosted::directory(&endpoint).await
        };
        tokio::select! {
            changed = input.changed() => {
                if changed.is_err() { break; }
                next = input.borrow_and_update().clone();
            },
            result = query => {
                output.send_replace(Some(Reply::Devices { epoch: request.epoch, key: request.key,
                    result: result.map_err(|error| format!("{error:#}")) }));
                context.request_repaint();
            },
        }
    }
}

#[derive(Default)]
pub(crate) struct DevicePicker {
    key: Option<Key>,
    epoch: u64,
    worker: Option<Worker>,
    devices: Vec<Device>,
    loading: bool,
    error: String,
    preferred: String,
}
impl DevicePicker {
    pub fn reset(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        if let Some(worker) = &self.worker {
            worker.request.send_replace(None);
        }
        self.key = None;
        self.devices.clear();
        self.error.clear();
        self.preferred.clear();
        self.loading = false;
    }
    fn query(&mut self, service: &Service, context: &egui::Context) {
        self.epoch = self.epoch.wrapping_add(1);
        self.devices.clear();
        self.error.clear();
        self.loading = true;
        if self.worker.is_none() {
            match Worker::new(context.clone()) {
                Ok(worker) => self.worker = Some(worker),
                Err(error) => {
                    self.loading = false;
                    self.error = error.to_string();
                    return;
                }
            }
        }
        if let Some(worker) = &self.worker {
            worker.request.send_replace(Some(Request {
                epoch: self.epoch,
                key: service.into(),
                service: service.clone(),
            }));
        }
    }
    fn accept(&mut self, reply: Reply, selected: &mut String) {
        match reply {
            Reply::Devices { epoch, key, result }
                if epoch == self.epoch && self.key.as_ref() == Some(&key) =>
            {
                self.loading = false;
                match result {
                    Ok(devices) => {
                        self.devices = devices
                            .into_iter()
                            .filter(|device| device.device != key.published)
                            .collect();
                        let wanted = if selected.is_empty() {
                            self.preferred.as_str()
                        } else {
                            selected.as_str()
                        };
                        *selected = self
                            .devices
                            .iter()
                            .find(|device| device.device == wanted)
                            .map(|device| device.device.clone())
                            .unwrap_or_default();
                        self.preferred.clear();
                        self.error.clear();
                    }
                    Err(error) => {
                        self.devices.clear();
                        selected.clear();
                        self.error = error;
                    }
                }
            }
            Reply::Failed(error) => {
                self.loading = false;
                self.devices.clear();
                selected.clear();
                self.error = error;
            }
            _ => {}
        }
    }
    pub fn valid(&self, service: Option<&Service>, selected: &str) -> bool {
        !self.loading
            && self.error.is_empty()
            && service.is_some_and(|service| self.key.as_ref() == Some(&Key::from(service)))
            && self.devices.iter().any(|device| device.device == selected)
    }
    fn select_service(
        &mut self,
        service: Option<&Service>,
        selected: &mut String,
        context: &egui::Context,
    ) {
        let key = service.map(Key::from);
        if key != self.key {
            let preferred = if self.key.is_none() {
                selected.clone()
            } else {
                String::new()
            };
            self.reset();
            self.key = key;
            self.preferred = preferred;
            selected.clear();
            if let Some(service) = service {
                self.query(service, context);
            }
        }
    }
    fn poll(&mut self, selected: &mut String) {
        let event = self
            .worker
            .as_mut()
            .and_then(|worker| match worker.results.has_changed() {
                Ok(true) => worker.results.borrow_and_update().clone(),
                Ok(false) => None,
                Err(_) => Some(Reply::Failed(
                    uu_i18n::text("后台操作未返回结果，请刷新确认状态。").into(),
                )),
            });
        if let Some(reply) = event {
            let stopped = matches!(reply, Reply::Failed(_));
            self.accept(reply, selected);
            if stopped {
                self.worker = None;
            }
        }
    }
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        service: Option<&Service>,
        selected: &mut String,
    ) -> Option<String> {
        self.select_service(service, selected, ui.ctx());
        self.poll(selected);
        let selected_label = self
            .devices
            .iter()
            .find(|device| device.device == *selected)
            .map(|device| label(device, &self.devices))
            .unwrap_or_else(|| uu_i18n::text("请选择在线设备").into());
        let mut chosen = None;
        ui.horizontal(|ui| {
            crate::theme::Select::from_id_salt("route-online-device")
                .selected_text(selected_label)
                .width((ui.available_width() - 68.).max(160.))
                .show_ui(ui, |ui| {
                    for device in &self.devices {
                        if crate::theme::selectable_label(
                            ui,
                            device.device == *selected,
                            label(device, &self.devices),
                        )
                        .on_hover_text(&device.device)
                        .clicked()
                        {
                            chosen = Some(device.clone());
                        }
                    }
                });
            if ui
                .add_enabled(
                    service.is_some() && !self.loading,
                    crate::theme::secondary_button(uu_i18n::text("刷新")),
                )
                .clicked()
            {
                self.preferred = selected.clone();
                selected.clear();
                if let Some(service) = service {
                    self.query(service, ui.ctx());
                }
            }
        });
        if self.loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak(uu_i18n::text("正在读取在线设备…"));
            });
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        } else if !self.error.is_empty() {
            ui.colored_label(
                crate::theme::palette(ui.visuals()).danger,
                uu_i18n::text("在线设备暂不可用"),
            );
            ui.collapsing(uu_i18n::text("诊断详情"), |ui| {
                ui.label(&self.error);
            });
        } else if service.is_some() && self.devices.is_empty() {
            ui.weak(uu_i18n::text(
                "暂无其他在线设备，请让另一台电脑加入同一服务。",
            ));
        }
        if let Some(device) = chosen {
            *selected = device.device;
            return Some(device.name);
        }
        None
    }
    #[cfg(test)]
    pub(crate) fn seed(&mut self, service: &Service, devices: Vec<Device>, selected: &mut String) {
        self.key = Some(service.into());
        self.accept(
            Reply::Devices {
                epoch: self.epoch,
                key: service.into(),
                result: Ok(devices),
            },
            selected,
        );
    }
}
fn label(device: &Device, devices: &[Device]) -> String {
    if devices
        .iter()
        .filter(|other| other.name == device.name)
        .count()
        < 2
    {
        return device.name.clone();
    }
    let mut length = device.device.len().min(8);
    while length < device.device.len()
        && devices.iter().any(|other| {
            other.device != device.device && other.device.starts_with(&device.device[..length])
        })
    {
        length += 1;
    }
    format!(
        "{} · {}{}",
        device.name,
        &device.device[..length],
        if length < device.device.len() {
            "…"
        } else {
            ""
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn service() -> Service {
        Service {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Generated group".into(),
            address: format!("wss://127.0.0.1:1/v1/signal?group={}", uuid::Uuid::new_v4()),
            token_file: "missing-generated-token".into(),
            certificate: None,
            host_enabled: false,
            published_name: "owned-local-device".into(),
        }
    }
    fn device(id: &str) -> Device {
        Device {
            device: id.into(),
            name: "Generated computer".into(),
        }
    }

    #[test]
    fn switching_services_discards_old_selection_and_rejects_late_replies() {
        let context = egui::Context::default();
        let first = service();
        let second = service();
        let mut picker = DevicePicker::default();
        let mut selected = "same-device-id".to_owned();
        picker.seed(
            &first,
            vec![device(&selected), device(&first.published_name)],
            &mut selected,
        );
        assert_eq!(
            picker.devices.len(),
            1,
            "the actual published identity is hidden"
        );
        assert!(picker.valid(Some(&first), &selected));
        let old_epoch = picker.epoch;
        picker.select_service(Some(&second), &mut selected, &context);
        assert!(selected.is_empty());
        assert!(
            picker.preferred.is_empty(),
            "the new server must not inherit an old device ID"
        );
        picker.accept(
            Reply::Devices {
                epoch: old_epoch,
                key: (&first).into(),
                result: Ok(vec![device("same-device-id")]),
            },
            &mut selected,
        );
        assert!(picker.devices.is_empty());
        assert!(!picker.valid(Some(&first), "same-device-id"));
        assert!(!picker.valid(Some(&second), "same-device-id"));
        picker.accept(
            Reply::Devices {
                epoch: picker.epoch,
                key: (&second).into(),
                result: Ok(vec![device("same-device-id")]),
            },
            &mut selected,
        );
        assert!(
            selected.is_empty(),
            "a new directory requires an explicit new selection"
        );
        selected = "same-device-id".into();
        assert!(picker.valid(Some(&second), &selected));
        let mut rotated = second.clone();
        rotated.token_file = "different-generated-token".into();
        assert!(
            !picker.valid(Some(&rotated), &selected),
            "credential changes invalidate cached results"
        );
    }

    #[test]
    fn existing_route_is_restored_only_after_a_matching_fresh_directory() {
        let current = service();
        let mut picker = DevicePicker {
            key: Some((&current).into()),
            epoch: 4,
            preferred: "saved-route-id".into(),
            loading: true,
            ..Default::default()
        };
        let mut selected = String::new();
        picker.accept(
            Reply::Devices {
                epoch: 3,
                key: (&current).into(),
                result: Ok(vec![device("saved-route-id")]),
            },
            &mut selected,
        );
        assert!(!picker.valid(Some(&current), "saved-route-id"));
        picker.accept(
            Reply::Devices {
                epoch: 4,
                key: (&current).into(),
                result: Ok(vec![device("saved-route-id")]),
            },
            &mut selected,
        );
        assert_eq!(selected, "saved-route-id");
        assert!(picker.valid(Some(&current), &selected));
        picker.accept(
            Reply::Devices {
                epoch: 4,
                key: (&current).into(),
                result: Err("generated directory failure".into()),
            },
            &mut selected,
        );
        assert!(selected.is_empty());
        assert!(!picker.valid(Some(&current), "saved-route-id"));
    }

    #[test]
    fn duplicate_friendly_names_get_distinct_short_identity_labels() {
        let devices = vec![
            device("abcdefgh-one"),
            device("abcdefgh-two"),
            Device {
                device: "third-device".into(),
                name: "Other generated computer".into(),
            },
        ];
        assert_ne!(label(&devices[0], &devices), label(&devices[1], &devices));
        assert_eq!(
            label(&devices[0], &devices),
            "Generated computer · abcdefgh-o…"
        );
        assert_eq!(label(&devices[2], &devices), "Other generated computer");
        assert_eq!(
            devices[0].device, "abcdefgh-one",
            "labels never replace the saved route ID"
        );
    }

    #[test]
    fn lost_worker_unlocks_refresh_and_invalidates_selection() {
        let current = service();
        let (request, _input) = watch::channel(None);
        let (output, results) = watch::channel(None);
        let mut picker = DevicePicker {
            key: Some((&current).into()),
            worker: Some(Worker { request, results }),
            loading: true,
            ..Default::default()
        };
        let mut selected = "saved-route-id".into();
        drop(output);
        picker.poll(&mut selected);
        assert!(!picker.loading && picker.worker.is_none());
        assert!(!picker.error.is_empty() && selected.is_empty());
        assert!(!picker.valid(Some(&current), "saved-route-id"));
    }

    #[tokio::test]
    async fn newer_request_cancels_stalled_tls_and_worker_exits_when_dropped() {
        use tokio::{io::AsyncReadExt, net::TcpListener, time::timeout};
        let root =
            std::env::temp_dir().join(format!("eclipse-device-picker-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let token = root.join("generated.token");
        std::fs::write(&token, "ab".repeat(32)).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut first = service();
        first.address = format!(
            "wss://127.0.0.1:{}/v1/signal?group={}",
            listener.local_addr().unwrap().port(),
            uuid::Uuid::new_v4()
        );
        first.token_file = token;
        let mut worker = Worker::new(egui::Context::default()).unwrap();
        worker.request.send_replace(Some(Request {
            epoch: 1,
            key: (&first).into(),
            service: first,
        }));
        // This owned socket accepts TCP and deliberately never completes TLS.
        let (mut stalled, _) = timeout(Duration::from_secs(3), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let second = Service {
            token_file: root.join("deliberately-missing-token"),
            ..service()
        };
        worker.request.send_replace(Some(Request {
            epoch: 2,
            key: (&second).into(),
            service: second,
        }));
        timeout(Duration::from_secs(3), worker.results.changed())
            .await
            .unwrap()
            .unwrap();
        let reply = worker.results.borrow_and_update().clone();
        assert!(
            matches!(
                reply,
                Some(Reply::Devices {
                    epoch: 2,
                    result: Err(_),
                    ..
                })
            ),
            "the latest request completes without waiting for the first TLS deadline"
        );
        let mut discarded_client_hello = Vec::new();
        timeout(
            Duration::from_secs(3),
            stalled.read_to_end(&mut discarded_client_hello),
        )
        .await
        .unwrap()
        .unwrap();
        let mut stopped = worker.results.clone();
        stopped.borrow_and_update();
        drop(worker);
        assert!(
            timeout(Duration::from_secs(3), stopped.changed())
                .await
                .unwrap()
                .is_err(),
            "dropping the picker closes its idle worker"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
