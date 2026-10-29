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
}
pub struct Endpoint {
    pub name: String,
    pub provider:Option<String>,
    pub state: watch::Receiver<EndpointStatus>,
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
}

pub fn prepare(
    config: Config,
    book: Book,
    trust: TrustStore,
    factory: Option<lan::ClipboardFactory>,
    restart: impl Fn() -> Result<()> + Send + Sync + 'static,
    emit: impl Fn(Event) + Send + Sync + 'static,
) -> Result<Prepared> {
    book.sharing.validate()?;
    // A missing, damaged or unavailable optional extension must not prevent an
    // otherwise configured native IP/self-hosted entrance from starting.
    let enabled=crate::plugins::Preferences::load(&config).unwrap_or_default();
    let providers:Vec<_>=crate::plugins::registry().host_services(&enabled,&book.sharing.plugin_services).into_iter()
        .map(|service|(service.id.to_owned(),service.name.to_owned()))
        .collect();
    ensure!(
        book.sharing.ip_enabled
            || !providers.is_empty()
            || book.services.iter().any(|s| s.host_enabled),
        "{}", uu_i18n::format!("请先在连接服务中启用至少一个入口"));
    let owner = HostOwner::claim(&config.at("device-trust"))?;
    let (requests, receive) = mpsc::unbounded_channel();
    let access = Arc::new(Access {
        store: trust,
        name: book.sharing.name.clone(),
        pairing: Arc::new(PairingControl::new(requests)),
        sessions: Arc::new(SessionGate::default()),
    });
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
            state,
        });
    }
    let mut services = Vec::new();
    let mut add = |name: String, settings: Settings| {
        let (status, state) = watch::channel(EndpointStatus::Starting);
        let provider=match &settings.provider {Provider::Plugin(id)=>Some(id.clone()),Provider::SelfHosted=>None};
        endpoints.push(Endpoint { name, state,provider });
        services.push((settings, status));
    };
    for (id,name) in providers {
        add(
            name,
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
        },
    })
}

impl Runner {
    pub async fn run(self, mut stop: watch::Receiver<bool>) -> Result<()> {
        let Self {
            config,
            book,
            access,
            factory,
            ip_status,
            services,
            emit,
            _owner,
        } = self;
        let sharing = book.sharing;
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
                audio: sharing.audio.then_some(crate::native::AudioSource::System),
                clipboard: factory.clone(),
            };
            match lan::Server::bind_with_access(
                lan_config,
                stop.clone(),
                lan_events,
                access.clone(),
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
        while !*stop.borrow() && !tasks.is_empty() {
            tokio::select! {
                _=stop.changed()=>break,
                result=tasks.join_next()=>match result {
                    Some(Err(error))=>emit(Event::Notice(format!("共享线程异常：{error}"))),
                    Some(Ok(Err(error)))=>emit(Event::Notice(format!("共享服务异常：{error:#}"))),
                    _=>{},
                }
            }
        }
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
        drop(_owner);
        Ok(())
    }
}
