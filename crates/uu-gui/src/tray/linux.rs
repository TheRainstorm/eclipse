//! A launcher-owned StatusNotifierItem. Only a registered icon permits hiding.
use super::Action;
use eframe::egui;
use ksni::TrayMethods;
use std::sync::{atomic::{AtomicU8, Ordering}, Arc, Mutex};
use std::time::Duration;

const STARTING: u8 = 0;
const ONLINE: u8 = 1;
const OFFLINE: u8 = 2;
const FINISHED: u8 = 3;

struct State {
    status: AtomicU8,
    action: Mutex<Option<Action>>,
    context: egui::Context,
}
impl State {
    fn send(&self, next: Action) {
        let mut action = self.action.lock().unwrap();
        if !matches!(*action, Some(Action::Exit)) { *action = Some(next); }
        drop(action);
        self.context.request_repaint();
    }
    fn offline(&self) {
        self.status.store(OFFLINE, Ordering::Release);
        // The only recovery entry must never disappear behind a hidden window.
        self.send(Action::Open);
    }
}
struct Item {
    state: Arc<State>,
    icon: Vec<u8>,
    width: i32,
    height: i32,
}
impl ksni::Tray for Item {
    fn id(&self) -> String { "io.github.TheRainstorm.Eclipse".into() }
    fn title(&self) -> String { crate::brand::NAME.into() }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![ksni::Icon { width: self.width, height: self.height, data: self.icon.clone() }]
    }
    fn activate(&mut self, _: i32, _: i32) { self.state.send(Action::Open); }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let entry = |label, action| ksni::menu::StandardItem {
            label: uu_i18n::text(label).into(),
            activate: Box::new(move |item: &mut Self| item.state.send(action)),
            ..Default::default()
        }.into();
        vec![entry("打开 Eclipse", Action::Open), entry("悬浮会话中心", Action::Sessions),
             ksni::MenuItem::Separator, entry("退出 Eclipse 并停止共享", Action::Exit)]
    }
    fn menu_about_to_show(&mut self) {}
    fn watcher_offline(&self, _: ksni::OfflineReason) -> bool {
        self.state.offline();
        // The desktop appearing again is not proof it registered our icon.
        // A fresh spawn will acknowledge registration before hiding is allowed.
        false
    }
}

pub struct Tray {
    state: Arc<State>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Tray {
    pub fn new(_: isize, context: &egui::Context) -> std::io::Result<Self> {
        let state = Arc::new(State { status: AtomicU8::new(STARTING),
            action: Mutex::new(None), context: context.clone() });
        let image = crate::brand::window_icon();
        let item = Item { state: state.clone(), width: image.width as i32, height: image.height as i32,
            icon: image.rgba.chunks_exact(4).flat_map(|p| [p[3], p[0], p[1], p[2]]).collect() };
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let observed = state.clone();
        let worker = std::thread::Builder::new().name("eclipse-tray".into()).spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
                    let handle = tokio::select! {
                        result = tokio::time::timeout(Duration::from_secs(5), item.spawn()) => result??,
                        _ = &mut stopped => return Ok(()),
                    };
                    observed.status.store(ONLINE, Ordering::Release);
                    observed.context.request_repaint();
                    loop {
                        tokio::select! {
                            _ = &mut stopped => break,
                            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                                if handle.is_closed() { break; }
                            }
                        }
                    }
                    let _ = tokio::time::timeout(Duration::from_secs(1), handle.shutdown()).await;
                    Ok(())
                })
            })();
            if let Err(error) = result { eprintln!("system tray: {error:#}"); }
            observed.offline();
            observed.status.store(FINISHED, Ordering::Release);
            observed.context.request_repaint();
        })?;
        Ok(Self { state, stop: Some(stop), worker: Some(worker) })
    }
    pub fn pending(&self) -> bool { self.state.status.load(Ordering::Acquire) == STARTING }
    pub fn finished(&self) -> bool { self.state.status.load(Ordering::Acquire) == FINISHED }
    pub fn ensure(&self) -> bool { self.state.status.load(Ordering::Acquire) == ONLINE }
    pub fn take_action(&self) -> Option<Action> { self.state.action.lock().unwrap().take() }
    pub fn set_display_options(&self, _: bool, _: bool) {}
}
impl Drop for Tray {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() { let _ = stop.send(()); }
        if let Some(worker) = self.worker.take() { let _ = worker.join(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_lost_icon_restores_the_window_and_never_overrides_exit() {
        let state = State { status: AtomicU8::new(ONLINE), action: Mutex::new(None), context: Default::default() };
        state.offline();
        assert_eq!(state.status.load(Ordering::Acquire), OFFLINE);
        assert_eq!(state.action.lock().unwrap().take(), Some(Action::Open));
        state.send(Action::Exit);
        state.offline();
        assert_eq!(state.action.lock().unwrap().take(), Some(Action::Exit));
    }

    struct Watcher(Arc<Mutex<Vec<String>>>);
    #[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
    impl Watcher {
        fn register_status_notifier_item(&self, service: String) {
            self.0.lock().unwrap().push(service);
        }
        #[zbus(property)]
        fn is_status_notifier_host_registered(&self) -> bool { true }
        #[zbus(property)]
        fn protocol_version(&self) -> i32 { 0 }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires dbus-run-session and ECLIPSE_OWNED_DBUS_TEST=1"]
    async fn private_session_bus_registers_clicks_and_recovers_its_tray_host() {
        assert_eq!(std::env::var("ECLIPSE_OWNED_DBUS_TEST").as_deref(), Ok("1"));
        let registered = Arc::new(Mutex::new(Vec::new()));
        let connection = zbus::connection::Builder::session().unwrap()
            .name("org.kde.StatusNotifierWatcher").unwrap()
            .serve_at("/StatusNotifierWatcher", Watcher(registered.clone())).unwrap()
            .build().await.unwrap();
        let tray = Tray::new(0, &egui::Context::default()).unwrap();
        let ready = || async {
            while !tray.ensure() { tokio::time::sleep(Duration::from_millis(10)).await; }
        };
        tokio::time::timeout(Duration::from_secs(5), ready()).await.unwrap();
        let service = registered.lock().unwrap()[0].clone();
        let proxy = zbus::Proxy::new(&connection, service.as_str(), "/StatusNotifierItem", "org.kde.StatusNotifierItem")
            .await.unwrap();
        proxy.call::<_, _, ()>("Activate", &(0i32, 0i32)).await.unwrap();
        assert_eq!(tray.take_action(), Some(Action::Open));
        connection.release_name("org.kde.StatusNotifierWatcher").await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while tray.ensure() { tokio::time::sleep(Duration::from_millis(10)).await; }
        }).await.unwrap();
        assert_eq!(tray.take_action(), Some(Action::Open), "losing the tray restores its window");
        drop(tray);
        connection.request_name("org.kde.StatusNotifierWatcher").await.unwrap();
        let replacement = Tray::new(0, &egui::Context::default()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !replacement.ensure() { tokio::time::sleep(Duration::from_millis(10)).await; }
        }).await.unwrap();
        assert!(registered.lock().unwrap().len() >= 2);
        drop(replacement);
    }
}
