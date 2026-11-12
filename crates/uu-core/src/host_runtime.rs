//! The shared host lifecycle for GUI and background services. Every entrance
//! uses the same identity, authorization, session gate and saved configuration.
use crate::{
    device_auth::{PairRequest, PairingControl},
    device_book::{Book, Store},
    device_internet::EndpointStatus,
    device_trust::TrustStore,
    internet::{Provider, Settings},
    native::HostStatus,
    native_lan as lan,
    shared_host::{Access, HostOwner, SessionGate},
    Config,
};
use anyhow::{ensure, Result};
use std::{sync::Arc, time::Duration};
use tokio::sync::{mpsc, watch};

pub enum Event {
    Listening(lan::Handle),
    Legacy(lan::Event),
    Notice(String),
    Connected(crate::connection_history::Record),
}
pub struct Endpoint {
    pub name: String,
    pub provider:Option<String>,
    pub identity: EndpointIdentity,
    pub state: watch::Receiver<EndpointStatus>,
}
/// Runtime identity is independent of labels and local aliases. No credential
/// paths or settings are exposed in the read-only management snapshot.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EndpointIdentity {
    Ip,
    Plugin { id: String },
    Server { id: String, address: String },
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointSnapshot {
    pub identity: EndpointIdentity,
    pub name: String,
    pub status: EndpointStatus,
}
impl Endpoint {
    pub fn snapshot(&self) -> EndpointSnapshot {
        EndpointSnapshot { identity:self.identity.clone(),name:self.name.clone(),status:self.state.borrow().clone() }
    }
}
pub struct Prepared {
    pub access: Arc<Access>,
    pub requests: mpsc::UnboundedReceiver<PairRequest>,
    pub endpoints: Vec<Endpoint>,
    pub runner: Runner,
}
pub struct Runner {
    config: Config,
    book: Book,
    access: Arc<Access>,
    factory: Option<lan::ClipboardFactory>,
    ip_status: watch::Sender<EndpointStatus>,
    services: Vec<(Settings, watch::Sender<EndpointStatus>)>,
    emit: Arc<dyn Fn(Event) + Send + Sync>,
    _owner: HostOwner,
    _input_activity: Option<uu_input::local_activity::Monitor>,
    persistent: bool,
}

pub fn prepare(
    config: Config,
    book: Book,
    trust: TrustStore,
    factory: Option<lan::ClipboardFactory>,
    restart: impl Fn() -> Result<()> + Send + Sync + 'static,
    emit: impl Fn(Event) + Send + Sync + 'static,
) -> Result<Prepared> {
    prepare_inner(config,book,trust,factory,restart,emit,false)
}

/// Installed hosts retain their identity and management plane even when no
/// entrance can listen. A local administrator can repair configuration in place.
pub fn prepare_service(
    config: Config,
    book: Book,
    trust: TrustStore,
    factory: Option<lan::ClipboardFactory>,
    restart: impl Fn() -> Result<()> + Send + Sync + 'static,
    emit: impl Fn(Event) + Send + Sync + 'static,
) -> Result<Prepared> {
    prepare_inner(config,book,trust,factory,restart,emit,true)
}

fn prepare_inner(
    config: Config,
    book: Book,
    trust: TrustStore,
    factory: Option<lan::ClipboardFactory>,
    restart: impl Fn() -> Result<()> + Send + Sync + 'static,
    emit: impl Fn(Event) + Send + Sync + 'static,
    persistent: bool,
) -> Result<Prepared> {
    book.sharing.validate()?;
    // A missing, damaged or unavailable optional extension must not prevent an
    // otherwise configured native IP/self-hosted entrance from starting.
    let enabled=crate::plugins::Preferences::load(&config).unwrap_or_default();
    let providers:Vec<_>=crate::plugins::registry().host_services(&enabled,&book.sharing.plugin_services).into_iter()
        .map(|service|(service.id.to_owned(),service.name.to_owned()))
        .collect();
    ensure!(
        persistent || book.sharing.ip_enabled
            || !providers.is_empty()
            || book.services.iter().any(|s| s.host_enabled),
        "{}", uu_i18n::format!("请先在连接服务中启用至少一个入口"));
    let owner = HostOwner::claim(&config.at("device-trust"))?;
    let (requests, receive) = mpsc::unbounded_channel();
    let access = Arc::new(Access {
        store: trust,
        name: book.sharing.name.clone(),
        pairing: Arc::new(PairingControl::new(requests)),
        sessions: Arc::new(SessionGate::with_history(config.at("device-trust/connection-history.json"))),
    });
    access.sessions.configure_input_policy(book.sharing.host_input);
    #[cfg(all(windows,not(test)))]
    let input_activity=match uu_input::local_activity::start() {
        Ok(observer)=>Some(observer),
        Err(error)=>{tracing::warn!(%error,"physical input observation unavailable; manual pause remains available");None}
    };
    #[cfg(any(not(windows),test))]
    let input_activity=None;
    if let Err(error)=crate::virtual_display::Manager::shared().configure_saved_displays(config.at("device-trust")) {
        tracing::warn!(%error,"saved virtual display settings could not be loaded; sharing continues");
    }
    crate::virtual_display::Manager::shared().register_host(&access.sessions);
    crate::virtual_display::Manager::shared().set_limit(book.sharing.virtual_display_limit)?;
    *access.sessions.administration.lock().unwrap() =
        Some(Arc::new(crate::host_admin::Service::new(
            Store::open(config.at("device-trust"))?,
            book.sharing.clone(),
            restart,
        )));
    let (ip_status, state) = watch::channel(EndpointStatus::Starting);
    let mut endpoints = Vec::new();
    if book.sharing.ip_enabled {
        endpoints.push(Endpoint {
            name: uu_i18n::text("IP 直连").into(),
            provider:None,
            identity: EndpointIdentity::Ip,
            state,
        });
    }
    let mut services = Vec::new();
    let mut add = |name: String, identity: EndpointIdentity, settings: Settings| {
        let (status, state) = watch::channel(EndpointStatus::Starting);
        let provider=match &settings.provider {Provider::Plugin(id)=>Some(id.clone()),Provider::SelfHosted=>None};
        endpoints.push(Endpoint { name, state,provider,identity });
        services.push((settings, status));
    };
    for (id,name) in providers {
        add(
            name,
            EndpointIdentity::Plugin { id:id.clone() },
            Settings {
                provider: Provider::Plugin(id),
                device_auth: true,
                ..Default::default()
            },
        );
    }
    for service in book.services.iter().filter(|s| s.host_enabled) {
        add(
            service.name.clone(),
            EndpointIdentity::Server { id:service.id.clone(),address:service.address.clone() },
            Settings {
                provider: Provider::SelfHosted,
                device_auth: true,
                server: service.address.clone(),
                device: service.published_name.clone(),
                server_token: service.token_file.clone(),
                server_ca: service.certificate.clone(),
                ..Default::default()
            },
        );
    }
    Ok(Prepared {
        access: access.clone(),
        requests: receive,
        endpoints,
        runner: Runner {
            config,
            book,
            access,
            factory,
            ip_status,
            services,
            emit: Arc::new(emit),
            _owner: owner,
            _input_activity: input_activity,
            persistent,
        },
    })
}

impl Runner {
    pub async fn run(self, stop: watch::Receiver<bool>) -> Result<()> {
        self.run_with_startup(stop, |service,discovery| {
            #[cfg(windows)] {
                let exe=std::env::current_exe()?;
                let report=if crate::service_admin::windows::require_administrator().is_ok() {
                    crate::firewall::ensure(&exe,service,discovery)?
                } else {crate::firewall::inspect(&exe,service,discovery)?};
                return Ok(Some(report.message()));
            }
            #[cfg(not(windows))] {let _=(service,discovery);Ok(None)}
        }).await
    }
    pub async fn run_with_startup(self, mut stop: watch::Receiver<bool>, startup:impl FnOnce(u16,u16)->Result<Option<String>> + Send + 'static) -> Result<()> {
        let Self {
            config,
            book,
            access,
            factory,
            ip_status,
            services,
            emit,
            _owner,
            _input_activity,
            persistent,
        } = self;
        let sharing = book.sharing;
        let mut connections = access.sessions.history.subscribe();
        let service=sharing.service_port; let discovery=sharing.discovery_port;
        match tokio::task::spawn_blocking(move||startup(service,discovery)).await {
            Ok(Ok(Some(message)))=>emit(Event::Notice(message)),
            Ok(Ok(None))=>{},
            Ok(Err(error))=>emit(Event::Notice(uu_i18n::format!("防火墙配置未完成：{error:#}。共享仍会启动，请检查网络权限。"))),
            Err(error)=>emit(Event::Notice(format!("Firewall worker failed: {error}"))),
        }
        if *stop.borrow() {return Ok(());}
        // Driver setup has its own owner: network-task cancellation must never
        // abandon an accepted PnP transaction or release its rollback claims.
        let (preparation_shutdown, preparation_stop)=watch::channel(false);
        let preparation=tokio::spawn(preparation_worker(stop.clone(),preparation_stop));
        let (stream, _) = watch::channel(HostStatus::Starting);
        let (lan_events, mut receive) = mpsc::unbounded_channel();
        let forward = emit.clone();
        let progress = stream.clone();
        let forwarding = tokio::spawn(async move {
            while let Some(event) = receive.recv().await {
                if let lan::Event::SessionProgress { status, .. } = &event {
                    progress.send_replace(status.clone());
                }
                forward(Event::Legacy(event));
            }
        });
        let mut tasks = tokio::task::JoinSet::new();
        if sharing.ip_enabled {
            let defaults = sharing.native();
            let lan_config = lan::Config {
                encoder_tuning: sharing.encoder_tuning,
                video_policy: defaults.video_policy,
                codec: defaults.codec,
                rate_control: defaults.rate_control,
                name: sharing.name.clone(),
                bind: ([0, 0, 0, 0], sharing.service_port).into(),
                discovery_bind: Some(([0, 0, 0, 0], sharing.discovery_port).into()),
                data_dir: config.at("native-lan"),
                source: defaults.source,
                fps: defaults.fps,
                bitrate_kbps: defaults.bitrate_kbps,
                encoder: sharing.encoding.native(),
                allow_input: sharing.control,
                audio: sharing.audio_source(),
                clipboard: factory.clone(),
            };
            match lan::Server::bind_all_interfaces(
                lan_config,
                stop.clone(),
                lan_events,
                access.clone(),
                sharing.ipv6_enabled,
            )
            .await
            {
                Ok((server, handle)) => {
                    ip_status.send_replace(EndpointStatus::Ready);
                    emit(Event::Listening(handle));
                    tasks.spawn(async move {
                        let result = server.run().await;
                        ip_status.send_replace(match &result {
                            Ok(()) => EndpointStatus::Stopped,
                            Err(e) => EndpointStatus::Error(uu_i18n::format!("IP 服务异常：{e:#}")),
                        });
                        result
                    });
                }
                Err(error) => {
                    let text = uu_i18n::format!("IP 共享不可用：{error:#}");
                    ip_status.send_replace(EndpointStatus::Error(text.clone()));
                    emit(Event::Notice(text));
                }
            }
        }
        for (settings, status) in services {
            let config = config.clone();
            let defaults = sharing.native();
            let access = access.clone();
            let stop = stop.clone();
            let phase = stream.clone();
            let factory = factory.clone();
            tasks.spawn(async move {
                crate::device_internet::listen(
                    settings, config, defaults, access, stop, status, phase, factory,
                )
                .await;
                Ok(())
            });
        }
        while !*stop.borrow() && (persistent || !tasks.is_empty()) {
            tokio::select! {
                _=stop.changed()=>break,
                Ok(record)=connections.recv()=>emit(Event::Connected(record)),
                result=tasks.join_next(),if !tasks.is_empty()=>match result {
                    Some(Err(error))=>emit(Event::Notice(format!("共享线程异常：{error}"))),
                    Some(Ok(Err(error)))=>emit(Event::Notice(format!("共享服务异常：{error:#}"))),
                    _=>{},
                }
            }
        }
        preparation_shutdown.send_replace(true);
        access.sessions.disconnect();
        if tokio::time::timeout(Duration::from_secs(5), async {
            while tasks.join_next().await.is_some() {}
        })
        .await
        .is_err()
        {
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        }
        forwarding.abort();
        let _ = forwarding.await;
        // No timeout/abort here: an accepted driver change must settle or retain
        // its journal before the shared-host owner can finish shutting down.
        if let Err(error)=preparation.await {tracing::warn!(%error,"virtual display preparation worker failed");}
        let history = access.sessions.history.clone();
        let _ = tokio::task::spawn_blocking(move || history.close()).await;
        drop(_input_activity);
        drop(_owner);
        Ok(())
    }
}

/// Poll only a previously accepted intent. Status/peer inspection never creates
/// one, and a missing driver is not installed merely because sharing started.
async fn preparation_worker(mut stop:watch::Receiver<bool>, mut shutdown:watch::Receiver<bool>) {
    let manager=crate::virtual_display::Manager::shared();
    let mut previous_error=None;
    let mut previous_restore_error=None;
    loop {
        if *stop.borrow() || *shutdown.borrow() {break;}
        if manager.has_queued_preparation() && !manager.preparation_in_flight() {
            let operation=manager.clone();
            match tokio::task::spawn_blocking(move||operation.resume_preparation()).await {
                Ok(Ok(_))=>{previous_error=None;},
                Ok(Err(error))=>{
                    let message=error.to_string();
                    if previous_error.as_ref()!=Some(&message) {tracing::warn!(%error,"virtual display queued preparation remains incomplete");}
                    previous_error=Some(message);
                },
                Err(error)=>{tracing::warn!(%error,"virtual display preparation worker failed");break;},
            }
        }
        if manager.saved_restore_pending() && !manager.preparation_in_flight() {
            let operation=manager.clone();
            match tokio::task::spawn_blocking(move||operation.restore_saved_displays()).await {
                Ok(Ok(()))=>{previous_restore_error=None;},
                Ok(Err(error))=>{let message=error.to_string();if previous_restore_error.as_ref()!=Some(&message){tracing::warn!(%error,"saved virtual display restoration remains incomplete");previous_restore_error=Some(message);}},
                Err(error)=>tracing::warn!(%error,"saved virtual display restoration worker failed"),
            }
        }
        tokio::select! {
            _=stop.changed()=>break,
            _=shutdown.changed()=>break,
            _=tokio::time::sleep(Duration::from_millis(500))=>{},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn ipv6_setting_controls_the_listener_while_preserving_ipv4() {
        for ipv6 in [false, true] {
            let root = std::env::temp_dir().join(format!("eclipse-ipv6-setting-{}", uuid::Uuid::new_v4()));
            let config = Config { data_dir: root.join("data"), state_dir: root.join("state"), signaling_host: String::new() };
            let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = tcp.local_addr().unwrap().port();
            let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            let mut book = Book::default();
            book.sharing.ipv6_enabled = ipv6;
            book.sharing.service_port = port;
            book.sharing.discovery_port = udp.local_addr().unwrap().port();
            let store = Store::open(config.at("device-trust")).unwrap();
            store.sharing(book.sharing).unwrap();
            let book = store.read().unwrap();
            let trust = TrustStore::open(config.at("device-trust")).unwrap();
            let prepared = prepare_service(config, book, trust, None, || Ok(()), |_| {}).unwrap();
            let state = prepared.endpoints[0].state.clone();
            drop((tcp, udp));
            let (stop, receive) = watch::channel(false);
            let running = tokio::spawn(prepared.runner.run_with_startup(receive, |_, _| Ok(None)));
            tokio::time::timeout(Duration::from_secs(3), async {
                while matches!(*state.borrow(), EndpointStatus::Starting) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }).await.unwrap();
            assert!(matches!(*state.borrow(), EndpointStatus::Ready));
            assert!(lan::inspect(&format!("127.0.0.1:{port}")).await.is_ok());
            let v6 = lan::inspect(&format!("[::1]:{port}")).await;
            assert_eq!(v6.is_ok(), ipv6, "the saved setting must control actual IPv6 acceptance");
            stop.send_replace(true);
            tokio::time::timeout(Duration::from_secs(5), running).await.unwrap().unwrap().unwrap();
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    #[tokio::test]
    async fn installed_host_keeps_management_and_identity_when_entrances_are_unavailable() {
        for conflict in [false,true] {
            let root=std::env::temp_dir().join(format!("eclipse-persistent-host-{}",uuid::Uuid::new_v4()));
            let config=Config{data_dir:root.join("data"),state_dir:root.join("state"),signaling_host:String::new()};
            let mut book=Book::default();book.sharing.audio=false;book.sharing.control=false;
            let occupied=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            book.sharing.service_port=occupied.local_addr().unwrap().port();
            book.sharing.ip_enabled=conflict;
            if !conflict {book.sharing.plugin_services=vec!["missing-generated-addon".into()];}
            let store=Store::open(config.at("device-trust")).unwrap();store.sharing(book.sharing.clone()).unwrap();
            let trust=TrustStore::open(config.at("device-trust")).unwrap();
            let identity=trust.card("Generated host").id();
            let prepared=prepare_service(config.clone(),book.clone(),trust.clone(),None,||Ok(()),|_|{}).unwrap();
            let access=prepared.access.clone();
            let endpoints=prepared.endpoints;
            let admin=access.sessions.administration.lock().unwrap().as_ref().unwrap().clone();
            let (stop,receive)=watch::channel(false);
            let running=tokio::spawn(prepared.runner.run_with_startup(receive,|_,_|Ok(None)));
            tokio::time::timeout(Duration::from_secs(3),async {
                while endpoints.iter().any(|e|matches!(*e.state.borrow(),EndpointStatus::Starting)) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }).await.unwrap();
            assert!(!running.is_finished(),"an unavailable listener must not destroy service management");
            if conflict {
                assert!(endpoints.iter().all(|e|matches!(*e.state.borrow(),EndpointStatus::Error(_))));
            } else {
                assert!(endpoints.is_empty(),"an unavailable configured plugin must not synthesize an endpoint");
            }
            let crate::host_admin::Response::Snapshot{snapshot}=admin.execute_local_manager(&access,crate::host_admin::Request::Inspect).unwrap() else {panic!()};
            let mut settings=book.sharing.clone();settings.ip_enabled=true;
            let revision=snapshot.server.unwrap().revision;
            admin.execute_local_manager(&access,crate::host_admin::Request::Save{revision,settings,restart:false}).unwrap();
            assert!(store.read().unwrap().sharing.ip_enabled);
            assert_eq!(trust.card("Generated host").id(),identity);
            assert!(HostOwner::claim(&config.at("device-trust")).is_err(),"the service remains the one host owner during recovery");
            stop.send_replace(true);tokio::time::timeout(Duration::from_secs(5),running).await.unwrap().unwrap().unwrap();
            drop(HostOwner::claim(&config.at("device-trust")).unwrap());
            drop(occupied);drop(admin);drop(access);drop(trust);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    fn unavailable_configured_plugin_does_not_hide_or_shadow_ip_entrance() {
        let root=std::env::temp_dir().join(format!("eclipse-phantom-plugin-{}",uuid::Uuid::new_v4()));
        let config=Config{data_dir:root.join("data"),state_dir:root.join("state"),signaling_host:String::new()};
        let mut book=Book::default();book.sharing.audio=false;book.sharing.control=false;
        book.sharing.ip_enabled=true;book.sharing.plugin_services=vec!["missing-generated-addon".into()];
        let trust=TrustStore::open(config.at("device-trust")).unwrap();
        let prepared=prepare_service(config.clone(),book,trust,None,||Ok(()),|_|{}).unwrap();
        assert_eq!(prepared.endpoints.len(),1,"missing plugins must not publish phantom entrances");
        assert_eq!(prepared.endpoints[0].name,uu_i18n::text("IP 直连"));
        assert!(prepared.endpoints[0].provider.is_none());
        drop(prepared);
        std::fs::remove_dir_all(root).unwrap();
    }
}
