//! Desktop notification delivery stays outside media and GUI render threads.
pub(super) fn show(record: &uu_core::connection_history::Record) {
    #[cfg(windows)]
    uu_core::connection_notice::show(record);
    #[cfg(target_os = "linux")]
    {
        let name = record.name.clone();
        let _ = std::thread::Builder::new().name("host-notice".into()).spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread().enable_all().build()
                .map_err(anyhow::Error::from).and_then(|runtime| runtime.block_on(notify(&name)));
            if let Err(error) = result { eprintln!("desktop connection notification unavailable: {error:#}"); }
        });
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    let _ = record;
}

#[cfg(target_os = "linux")]
async fn notify(name: &str) -> anyhow::Result<()> {
    use std::time::Duration;
    let connection = tokio::time::timeout(Duration::from_secs(2), zbus::Connection::session()).await??;
    notify_on(&connection, name).await
}
#[cfg(target_os = "linux")]
async fn notify_on(connection: &zbus::Connection, name: &str) -> anyhow::Result<()> {
    use std::{collections::HashMap, time::Duration};
    let proxy = zbus::Proxy::new(&connection, "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications", "org.freedesktop.Notifications").await?;
    // Names belong in the plain summary, never in markup-bearing body text.
    let summary = uu_i18n::format!("{name} 已连接");
    let hints = HashMap::from([("transient", zbus::zvariant::Value::from(true))]);
    let id: u32 = tokio::time::timeout(Duration::from_secs(2), proxy.call("Notify", &(
        "Eclipse", 0u32, "eclipse", summary, "", Vec::<String>::new(), hints,
        uu_core::connection_notice::DURATION.as_millis() as i32,
    ))).await??;
    tokio::time::sleep(uu_core::connection_notice::DURATION).await;
    // Explicit close also handles desktop servers that ignore expire_timeout.
    tokio::time::timeout(Duration::from_secs(2), proxy.call::<_, _, ()>("CloseNotification", &(id,))).await??;
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::{io::BufRead, process::{Command, Stdio}, sync::{Arc, Mutex}, time::Instant};
    struct Bus(std::process::Child);
    impl Drop for Bus { fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); } }
    struct Notifications(Arc<Mutex<Vec<(Instant, i32, String)>>>);
    #[zbus::interface(name = "org.freedesktop.Notifications")]
    impl Notifications {
        async fn notify(&self, app: String, replaces: u32, _icon: String, summary: String, body: String,
            actions: Vec<String>, hints: std::collections::HashMap<String, zbus::zvariant::OwnedValue>, timeout: i32) -> u32 {
            assert_eq!(app, "Eclipse"); assert_eq!(replaces, 0);
            assert!(body.is_empty() && actions.is_empty());
            assert_eq!(bool::try_from(hints.get("transient").unwrap()).unwrap(), true);
            self.0.lock().unwrap().push((Instant::now(), timeout, summary));
            42
        }
        async fn close_notification(&self, id: u32) {
            assert_eq!(id, 42);
            self.0.lock().unwrap().push((Instant::now(), 0, String::new()));
        }
    }
    #[tokio::test]
    async fn private_desktop_bus_receives_one_passive_notice_and_explicit_close_after_three_seconds() {
        let mut bus = Bus(Command::new("dbus-daemon").args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap());
        let mut address = String::new();
        std::io::BufReader::new(bus.0.stdout.take().unwrap()).read_line(&mut address).unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let server = zbus::connection::Builder::address(address.trim()).unwrap()
            .name("org.freedesktop.Notifications").unwrap()
            .serve_at("/org/freedesktop/Notifications", Notifications(events.clone())).unwrap().build().await.unwrap();
        let client = zbus::connection::Builder::address(address.trim()).unwrap().build().await.unwrap();
        notify_on(&client, "<b>Owned fixture</b>").await.unwrap();
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].1, 3000);
        assert!(events[0].2.contains("<b>Owned fixture</b>"), "names remain plain summary text");
        let elapsed = events[1].0.duration_since(events[0].0);
        assert!(elapsed.as_millis() >= 2990 && elapsed.as_secs_f32() < 4.0, "{elapsed:?}");
        drop(events); drop((client, server));
    }
}
