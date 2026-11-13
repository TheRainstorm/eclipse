//! Session-bus ownership and activation; no shared socket files or stale PID locks.
use std::{io, sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex, OnceLock}, time::Duration};

const PATH: &str = "/io/github/TheRainstorm/Eclipse/Launcher";
const INTERFACE: &str = "io.github.TheRainstorm.Eclipse.Launcher";
static CURRENT: OnceLock<Arc<State>> = OnceLock::new();

#[derive(Default)]
struct State {
    activate: AtomicBool,
    context: Mutex<Option<eframe::egui::Context>>,
}
struct Activation(Arc<State>);
enum Started { Owner, Notified, NoBus }
#[zbus::interface(name = "io.github.TheRainstorm.Eclipse.Launcher")]
impl Activation {
    fn open(&self) {
        self.0.activate.store(true, Ordering::Release);
        if let Some(context) = self.0.context.lock().unwrap().as_ref() { context.request_repaint(); }
    }
}
pub struct Launcher {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Launcher {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() { let _ = stop.send(()); }
        if let Some(worker) = self.worker.take() { let _ = worker.join(); }
    }
}
pub fn take_activation(context: &eframe::egui::Context) -> bool {
    let Some(state) = CURRENT.get() else { return false; };
    *state.context.lock().unwrap() = Some(context.clone());
    state.activate.swap(false, Ordering::AcqRel)
}

pub(super) fn claim(config: &uu_core::Config) -> io::Result<Option<Launcher>> {
    let name = format!("io.github.TheRainstorm.Eclipse.Launcher.p{}", super::profile_key(&config.data_dir)?);
    let state = Arc::new(State::default());
    let observed = state.clone();
    let (ready, started) = std::sync::mpsc::sync_channel(1);
    let (stop, mut stopped) = tokio::sync::oneshot::channel();
    let worker = std::thread::Builder::new().name("eclipse-launcher".into()).spawn(move || {
        let result = (|| -> anyhow::Result<()> {
            tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
                let connecting = async {
                    zbus::connection::Builder::session()?.serve_at(PATH, Activation(observed))?.build().await
                };
                let connection = match tokio::time::timeout(Duration::from_secs(2), connecting).await {
                    Ok(Ok(connection)) => connection,
                    _ => {
                        // A minimal X11 desktop may have no session bus. Keep
                        // its visible GUI usable; tray creation will also fail.
                        let _ = ready.send(Ok(Started::NoBus));
                        return Ok(());
                    }
                };
                let requested = connection.request_name_with_flags(name.as_str(), zbus::fdo::RequestNameFlags::DoNotQueue.into());
                match tokio::time::timeout(Duration::from_secs(2), requested).await? {
                    Ok(zbus::fdo::RequestNameReply::PrimaryOwner | zbus::fdo::RequestNameReply::AlreadyOwner) => {},
                    Ok(zbus::fdo::RequestNameReply::Exists) | Err(zbus::Error::NameTaken) => {
                        let proxy = zbus::Proxy::new(&connection, name.as_str(), PATH, INTERFACE).await?;
                        tokio::time::timeout(Duration::from_secs(2), proxy.call::<_, _, ()>("Open", &())).await??;
                        let _ = ready.send(Ok(Started::Notified));
                        return Ok(());
                    }
                    Ok(zbus::fdo::RequestNameReply::InQueue) => anyhow::bail!("launcher ownership unexpectedly queued"),
                    Err(error) => return Err(error.into()),
                }
                if ready.send(Ok(Started::Owner)).is_err() { return Ok(()); }
                let _ = (&mut stopped).await;
                drop(connection);
                Ok(())
            })
        })();
        if let Err(error) = result { let _ = ready.send(Err(io::Error::other(error.to_string()))); }
    })?;
    let guard = Launcher { stop: Some(stop), worker: Some(worker) };
    match started.recv_timeout(Duration::from_secs(7)) {
        Ok(Ok(Started::Owner)) => { let _ = CURRENT.set(state); Ok(Some(guard)) }
        Ok(Ok(Started::NoBus)) => Ok(Some(guard)),
        Ok(Ok(Started::Notified)) => Ok(None),
        Ok(Err(error)) => Err(error),
        Err(error) => Err(io::Error::new(io::ErrorKind::TimedOut, error.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires dbus-run-session and ECLIPSE_OWNED_DBUS_TEST=1"]
    fn private_session_bus_keeps_one_launcher_and_activates_the_owner() {
        assert_eq!(std::env::var("ECLIPSE_OWNED_DBUS_TEST").as_deref(), Ok("1"));
        let root = std::env::temp_dir().join(format!("eclipse-launcher-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let config = uu_core::Config { data_dir: root.join("profile/data"),
            state_dir: root.join("profile/state"), signaling_host: String::new() };
        let owner = claim(&config).unwrap().unwrap();
        assert!(claim(&config).unwrap().is_none());
        let context = eframe::egui::Context::default();
        assert!(take_activation(&context));
        assert!(!take_activation(&context));
        let other = uu_core::Config { data_dir: root.join("another/data"), ..config.clone() };
        assert!(claim(&other).unwrap().is_some());
        drop(owner);
        assert!(claim(&config).unwrap().is_some(), "the name must be released when its launcher ends");
        std::fs::remove_dir_all(root).unwrap();
    }
}
