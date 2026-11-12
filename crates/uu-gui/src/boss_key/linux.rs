//! A session-bus owner holds the desktop shortcut; every local process follows
//! its visibility signal. If the owner exits, a remaining client takes over.
use super::observe;
use futures_util::StreamExt;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex, OnceLock,
};
use tokio::sync::{mpsc, oneshot};
use uu_core::hotkeys::{Action, Bindings, Chord};
#[path = "linux/x11.rs"]
mod x11;

const NAME: &str = "io.github.TheRainstorm.Eclipse.BossKey";
const PATH: &str = "/io/github/TheRainstorm/Eclipse/BossKey";
const INTERFACE: &str = "io.github.TheRainstorm.Eclipse.BossKey";
static READY: AtomicBool = AtomicBool::new(false);
static RECORDING: AtomicBool = AtomicBool::new(false);
pub(super) fn available() -> bool {
    READY.load(Ordering::Acquire)
}
static WORKER: OnceLock<mpsc::UnboundedSender<Command>> = OnceLock::new();
enum Command {
    Toggle,
    Configure(Vec<Chord>),
    Recording(Option<String>, bool),
    Focused,
}
enum Event {
    Ready,
    Activated(String),
    Failed,
}
fn keys() -> &'static Mutex<Vec<Chord>> {
    static KEYS: OnceLock<Mutex<Vec<Chord>>> = OnceLock::new();
    KEYS.get_or_init(|| Mutex::new(Bindings::default().chords(Action::BossKey).to_vec()))
}
pub fn configure(value: &[Chord]) {
    let mut keys = keys().lock().unwrap();
    if *keys == value {
        return;
    }
    *keys = value.to_vec();
    if let Some(worker) = WORKER.get() {
        let _ = worker.send(Command::Configure(value.to_vec()));
    }
}
pub fn recording(active: bool) {
    if RECORDING.swap(active, Ordering::AcqRel) != active {
        if let Some(worker) = WORKER.get() { let _ = worker.send(Command::Recording(None, active)); }
    }
}
pub fn start() {
    WORKER.get_or_init(|| {
        let (send, receive) = mpsc::unbounded_channel();
        let _ = std::thread::Builder::new()
            .name("eclipse-boss-key".into())
            .spawn(move || {
                let result = (|| -> anyhow::Result<()> {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?
                        .block_on(run(receive))
                })();
                if let Err(error) = result {
                    READY.store(false, Ordering::Release);
                    observe(false);
                    eprintln!("boss key: {error:#}");
                }
            });
        send
    });
}
pub fn toggle() -> bool {
    READY.load(Ordering::Acquire)
        && WORKER
            .get()
            .is_some_and(|worker| worker.send(Command::Toggle).is_ok())
}
struct Service {
    last_active: std::sync::Arc<Mutex<Option<String>>>,
    commands: mpsc::UnboundedSender<Command>,
}
#[zbus::interface(name = "io.github.TheRainstorm.Eclipse.BossKey")]
impl Service {
    fn status(&self) -> (bool, bool) {
        (super::hidden(), READY.load(Ordering::Acquire))
    }
    async fn toggle(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        if !READY.load(Ordering::Acquire) {
            return Err(zbus::fdo::Error::Failed(
                "desktop shortcut is unavailable".into(),
            ));
        }
        toggle_visibility(connection, &self.last_active, String::new())
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }
    fn focused(&self, #[zbus(header)] header: zbus::message::Header<'_>) -> zbus::fdo::Result<()> {
        let sender = header.sender().ok_or_else(|| zbus::fdo::Error::InvalidArgs("missing sender".into()))?;
        *self.last_active.lock().unwrap() = Some(sender.to_string());
        Ok(())
    }
    fn recording(&self, active: bool, #[zbus(header)] header: zbus::message::Header<'_>) -> zbus::fdo::Result<()> {
        let sender = header.sender().ok_or_else(|| zbus::fdo::Error::InvalidArgs("missing sender".into()))?;
        self.commands.send(Command::Recording(Some(sender.to_string()), active))
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }
    fn configure(&self, encoded: &str) -> zbus::fdo::Result<()> {
        let value: Vec<Chord> = serde_json::from_str(encoded)
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        if value.len() > 3 || value.iter().any(|c| !c.valid()) {
            return Err(zbus::fdo::Error::InvalidArgs("invalid shortcut".into()));
        }
        self.commands
            .send(Command::Configure(value))
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }
}
pub fn focused() {
    if let Some(worker) = WORKER.get() { let _ = worker.send(Command::Focused); }
}
fn activation_request() -> &'static Mutex<Option<String>> {
    static REQUEST: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    REQUEST.get_or_init(Default::default)
}
pub fn take_activation() -> Option<String> { activation_request().lock().unwrap().take() }
fn request_activation(token: String) {
    *activation_request().lock().unwrap() = Some(token);
    observe(super::hidden());
}
async fn toggle_visibility(connection: &zbus::Connection, last_active: &Mutex<Option<String>>, token: String) -> anyhow::Result<()> {
    let hidden = !super::hidden();
    if !hidden {
        let own = connection.unique_name().unwrap().as_str();
        let target = last_active.lock().unwrap().clone().unwrap_or_else(|| own.to_owned());
        if target == own { request_activation(token.clone()); }
        connection.emit_signal(None::<&str>, PATH, INTERFACE, "Activate", &(target, token)).await?;
    }
    publish(connection, hidden, true).await
}
async fn publish(connection: &zbus::Connection, hidden: bool, ready: bool) -> anyhow::Result<()> {
    if hidden || !ready { activation_request().lock().unwrap().take(); }
    READY.store(ready, Ordering::Release);
    observe(hidden);
    connection
        .emit_signal(None::<&str>, PATH, INTERFACE, "Changed", &(hidden, ready))
        .await?;
    Ok(())
}
async fn run(mut commands: mpsc::UnboundedReceiver<Command>) -> anyhow::Result<()> {
    let (owner_send, mut owner_commands) = mpsc::unbounded_channel();
    let last_active = std::sync::Arc::new(Mutex::new(None));
    let connection = zbus::connection::Builder::session()?
        .serve_at(
            PATH,
            Service {
                last_active: last_active.clone(),
                commands: owner_send,
            },
        )?
        .build()
        .await?;
    loop {
        use zbus::fdo::{RequestNameFlags, RequestNameReply};
        let leader = match connection
            .request_name_with_flags(NAME, RequestNameFlags::DoNotQueue.into())
            .await
        {
            Ok(reply) => matches!(
                reply,
                RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner
            ),
            Err(zbus::Error::NameTaken) => false,
            Err(error) => return Err(error.into()),
        };
        if leader {
            return owner(&connection, &mut commands, &mut owner_commands, &last_active).await;
        }
        let proxy = zbus::Proxy::new(&connection, NAME, PATH, INTERFACE).await?;
        // A single stream preserves ordering between visibility and activation.
        let mut signals = proxy.receive_all_signals().await?;
        if let Ok((hidden, ready)) = proxy.call::<_, _, (bool, bool)>("Status", &()).await {
            READY.store(ready, Ordering::Release);
            observe(hidden);
        }
        let encoded = serde_json::to_string(&*keys().lock().unwrap())?;
        let _: Result<(), _> = proxy.call("Configure", &(encoded,)).await;
        let _: Result<(), _> = proxy.call("Recording", &(RECORDING.load(Ordering::Acquire),)).await;
        let mut check = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tokio::select! {
                command=commands.recv()=>match command {
                    Some(Command::Toggle)=>{let _:Result<(),_>=proxy.call("Toggle",&()).await;},
                    Some(Command::Focused)=>{let _:Result<(),_>=proxy.call("Focused",&()).await;},
                    Some(Command::Configure(value))=>{let _:Result<(),_>=proxy.call("Configure",&(serde_json::to_string(&value)?,)).await;},
                    Some(Command::Recording(_, active))=>{let _:Result<(),_>=proxy.call("Recording",&(active,)).await;},
                    None=>return Ok(()),
                },
                signal=signals.next()=>{
                    if let Some(signal)=signal {
                        match signal.header().member().map(|name|name.as_str()) {
                            Some("Activate")=>if let Ok((target,token))=signal.body().deserialize::<(String,String)>() {
                                if connection.unique_name().is_some_and(|name|name.as_str()==target) { request_activation(token); }
                            },
                            Some("Changed")=>if let Ok((hidden,ready))=signal.body().deserialize::<(bool,bool)>() {
                                if hidden || !ready { activation_request().lock().unwrap().take(); }
                                READY.store(ready,Ordering::Release); observe(hidden);
                            },
                            _=>{},
                        }
                    }else{break;}
                },
                _=check.tick()=>match proxy.call::<_,_,(bool,bool)>("Status",&()).await {
                    Ok((hidden,ready))=>if hidden!=super::hidden() || ready!=READY.load(Ordering::Acquire){READY.store(ready,Ordering::Release);observe(hidden);},
                    Err(_)=>break,
                },
            }
        }
    }
}
struct Hotkey {
    cancel: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}
impl Hotkey {
    fn start(keys: Vec<Chord>, events: mpsc::UnboundedSender<Event>) -> Self {
        let (cancel, receive) = oneshot::channel();
        let task = tokio::spawn(async move {
            let result = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
                portal(keys, events.clone(), receive).await
            } else {
                x11::listen(keys, events.clone(), receive).await
            };
            if let Err(error) = result {
                eprintln!("boss key registration: {error:#}");
                let _ = events.send(Event::Failed);
            }
        });
        Self { cancel, task }
    }
    async fn stop(self) {
        let _ = self.cancel.send(());
        let _ = self.task.await;
    }
}
async fn owner(
    connection: &zbus::Connection,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    remote: &mut mpsc::UnboundedReceiver<Command>,
    last_active: &Mutex<Option<String>>,
) -> anyhow::Result<()> {
    let (send, mut events) = mpsc::unbounded_channel();
    let mut current = keys().lock().unwrap().clone();
    let own_name = connection.unique_name().unwrap().to_string();
    let mut recorders = std::collections::HashSet::new();
    if RECORDING.load(Ordering::Acquire) { recorders.insert(own_name.clone()); }
    let mut hotkey = recorders.is_empty().then(|| Hotkey::start(current.clone(), send.clone()));
    let bus = zbus::fdo::DBusProxy::new(connection).await?;
    let mut names = bus.receive_name_owner_changed().await?;
    loop {
        let command = tokio::select! {
            command=commands.recv()=>command,
            command=remote.recv()=>command,
            change=names.next()=>{
                let Some(change)=change else { continue; };
                let args=change.args()?;
                if args.new_owner().is_some() { continue; }
                {
                    let mut active = last_active.lock().unwrap();
                    if active.as_deref() == Some(args.name().as_str()) { *active = None; }
                }
                Some(Command::Recording(Some(args.name().to_string()),false))
            },
            event=events.recv()=>{
                match event {
                    Some(Event::Ready) if hotkey.is_some()=>publish(connection,super::hidden(),true).await?,
                    Some(Event::Activated(token)) if READY.load(Ordering::Acquire)=>toggle_visibility(connection,last_active,token).await?,
                    Some(Event::Failed)=>publish(connection,false,false).await?,
                    _=>{},
                }continue;
            }
        };
        let rebind = match command {
            Some(Command::Toggle) if READY.load(Ordering::Acquire) => {
                toggle_visibility(connection, last_active, String::new()).await?; false
            }
            Some(Command::Focused) => { *last_active.lock().unwrap()=Some(own_name.clone()); false },
            Some(Command::Configure(value)) if current != value => {
                current = value;
                *keys().lock().unwrap() = current.clone(); true
            }
            Some(Command::Recording(sender, active)) => {
                let sender=sender.unwrap_or_else(||own_name.clone());
                if active {recorders.insert(sender)} else {recorders.remove(&sender)}
            }
            Some(_) => false,
            None => {
                if let Some(hotkey)=hotkey.take() {hotkey.stop().await;}
                return Ok(());
            }
        };
        if rebind {
            if let Some(hotkey)=hotkey.take() {hotkey.stop().await;}
            while events.try_recv().is_ok() {}
            READY.store(false,Ordering::Release);
            if current.is_empty() || !recorders.is_empty() {
                publish(connection,false,false).await?;
            } else {
                hotkey=Some(Hotkey::start(current.clone(),send.clone()));
            }
        }
    }
}

async fn portal(
    keys: Vec<Chord>,
    events: mpsc::UnboundedSender<Event>,
    mut cancel: oneshot::Receiver<()>,
) -> anyhow::Result<()> {
    use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
    if keys.is_empty() {
        return Ok(());
    }
    let portal = GlobalShortcuts::new().await?;
    let session = portal.create_session().await?;
    let session_path = serde_json::to_value(&session)?;
    let result = async {
        let mut activated = portal.receive_activated().await?;
        let mut deactivated = portal.receive_deactivated().await?;
        let mut closed = session.receive_closed().await?;
        let mut held = std::collections::HashSet::new();
        let shortcuts: Vec<_> = keys
            .iter()
            .enumerate()
            .map(|(i, key)| {
                let trigger = portal_trigger(*key);
                NewShortcut::new(format!("boss-{i}"), "Eclipse: hide or restore stream windows")
                    .preferred_trigger(Some(trigger.as_str()))
            })
            .collect();
        let bound = portal
            .bind_shortcuts(&session, &shortcuts, None)
            .await?
            .response()?;
        anyhow::ensure!(
            !bound.shortcuts().is_empty(),
            "desktop did not bind a boss key"
        );
        let _ = events.send(Event::Ready);
        loop {
            tokio::select! {
                event=activated.next()=>if let Some(event)=event {
                    if Some(event.session_handle().as_str())==session_path.as_str() && event.shortcut_id().starts_with("boss-") && held.insert(event.shortcut_id().to_owned()){let token=event.options().get("activation_token").and_then(|v|<&str>::try_from(v).ok()).unwrap_or_default().to_owned();let _=events.send(Event::Activated(token));}
                }else{break;},
                event=deactivated.next()=>if let Some(event)=event {
                    if Some(event.session_handle().as_str())==session_path.as_str(){held.remove(event.shortcut_id());}
                }else{break;},
                _=closed.next()=>break,
            }
        }
        anyhow::bail!("global shortcut session ended")
    };
    let result = tokio::select! {result=result=>result,_=&mut cancel=>Ok(())};
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), session.close()).await;
    result
}
fn portal_trigger(chord: Chord) -> String {
    let mut value = String::new();
    for (active, name) in [
        (chord.modifiers.ctrl, "CTRL+"),
        (chord.modifiers.alt, "ALT+"),
        (chord.modifiers.shift, "SHIFT+"),
        (chord.modifiers.meta, "LOGO+"),
    ] {
        if active {
            value.push_str(name);
        }
    }
    let key = if (0x41..=0x5a).contains(&chord.key) {
        char::from_u32(chord.key + 32).unwrap().to_string()
    } else {
        x11::keysym_name(chord.key)
    };
    value.push_str(&key);
    value
}
