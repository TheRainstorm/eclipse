use super::*;
use anyhow::ensure;
use std::sync::{atomic::Ordering, Arc, Mutex, Weak};
use tokio::{
    net::windows::named_pipe::NamedPipeServer,
    sync::{mpsc, watch},
};
use uu_core::{
    device_auth::{PairApproval, PairRequest, PairStatus},
    device_trust::TrustStore,
    host_admin,
    service_admin::{self, Request, Response},
    shared_host::Access,
};
use windows_sys::Win32::{
    Foundation::*,
    System::{
        RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId},
        Threading::*,
    },
};

struct Clipboards(Arc<Mutex<Vec<Weak<uu_core::clipboard::Clipboard>>>>);
impl Drop for Clipboards {
    fn drop(&mut self) {
        for state in self.0.lock().unwrap().iter().filter_map(Weak::upgrade) {
            state.stopped.store(true, Ordering::Release);
        }
    }
}
pub async fn run(expected_session: u32, event: &str) -> Result<()> {
    ensure!(
        process_user(unsafe { GetCurrentProcess() })? == "S-1-5-18",
        "后台媒体进程必须由 Windows 服务启动"
    );
    let mut session = 0;
    winok(unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) })?;
    ensure!(
        session == expected_session
            && session == unsafe { WTSGetActiveConsoleSessionId() }
            && session != 0,
        "后台媒体进程不在指定控制台会话"
    );
    let suffix = event
        .strip_prefix("Global\\EclipseRemoteHost.stop.")
        .context("后台停止事件无效")?;
    uuid::Uuid::parse_str(suffix).context("后台停止事件无效")?;
    let stop_event = Handle(unsafe {
        OpenEventW(
            windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE,
            0,
            wide(event).as_ptr(),
        )
    });
    ensure!(!stop_event.0.is_null(), "无法连接后台服务生命周期");
    let paths = Paths::discover()?;
    let installed = paths.manifest()?;
    ensure!(
        std::env::current_exe()?.canonicalize()?
            == paths
                .binary_directory(&installed)
                .join(installed.cli_name())
                .canonicalize()?,
        "后台媒体进程必须从受保护的安装位置启动"
    );
    // Driver reconfiguration belongs to this protected management host's
    // session gate. Ordinary UAC helpers may only perform a fresh install.
    uu_core::virtual_display::Manager::shared().enable_service_preparation()?;
    let config = paths.runtime_config()?;
    config.ensure_dirs().await?;
    let result=async {loop {
        if unsafe { WaitForSingleObject(stop_event.0, 0) } == WAIT_OBJECT_0 {
            return Ok(());
        }
        let store = uu_core::device_book::Store::open(config.at("device-trust"))?;
        let book = store.read()?;
        let active_services = Arc::new(book.services.clone());
        let trust = TrustStore::open(config.at("device-trust"))?;
        let states = Clipboards(Arc::new(Mutex::new(Vec::new())));
        let factory = Some({
            let states = states.0.clone();
            Arc::new(move |_id: &str| {
                let state = Arc::new(uu_core::clipboard::Clipboard::default());
                state.enabled.store(true, Ordering::Release);
                uu_clipboard::start_host_headless(state.clone())?;
                let mut states = states.lock().unwrap();
                states.retain(|state| {
                    state
                        .upgrade()
                        .is_some_and(|s| !s.stopped.load(Ordering::Acquire))
                });
                states.push(Arc::downgrade(&state));
                Ok(state)
            }) as uu_core::native_lan::ClipboardFactory
        });
        let (restart, mut restarts) = mpsc::channel(1);
        let extensions=uu_plugin_sdk::runtime::shared(&config);
        if let Ok(preferences)=uu_core::plugins::Preferences::load(&config){extensions.lock().unwrap().apply(&preferences);}
        let _extensions=extensions;
        let prepared = uu_core::host_runtime::prepare_service(
            config.clone(),
            book,
            trust,
            factory,
            move || match restart.try_send(()) {
                Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => Ok(()),
                Err(_) => anyhow::bail!("后台服务正在退出"),
            },
            |event| {
                match event {
                    uu_core::host_runtime::Event::Notice(message) => tracing::warn!(%message,"background host entrance"),
                    uu_core::host_runtime::Event::Connected(record) => uu_core::connection_notice::show(&record),
                    _ => {},
                }
            },
        )?;
        let administration = prepared
            .access
            .sessions
            .administration
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .clone();
        let listener = pipe(true)?;
        let (stop, receiver) = watch::channel(false);
        let manager = tokio::spawn(serve(
            listener,
            prepared.access.clone(),
            administration,
            prepared.requests,
            prepared.endpoints,
            active_services,
            receiver.clone(),
        ));
        let running = prepared.runner.run(receiver);
        tokio::pin!(running);
        let mut completed = false;
        let mut failure = None;
        let restart = tokio::select! {
            result=&mut running=>{completed=true;failure=result.err();false},
            restart=restarts.recv()=>restart.is_some(),
            _=async{loop{if manager.is_finished()||unsafe{WaitForSingleObject(stop_event.0,0)}==WAIT_OBJECT_0{break;}tokio::time::sleep(Duration::from_millis(50)).await;}}=>false,
        };
        if manager.is_finished() && !completed {
            failure = Some(anyhow::anyhow!("后台管理通道已结束"));
        }
        stop.send_replace(true);
        if !completed {
            match tokio::time::timeout(uu_core::native_virtual_display::OPERATION_TIMEOUT, &mut running).await {
                Ok(result) => failure = result.err(),
                Err(_) => failure = Some(anyhow::anyhow!("后台共享停止超时")),
            }
        }
        // Stop accepting IPC first; join every accepted display mutation before
        // releasing the process's manual leases. Aborting an async wrapper does
        // not stop its blocking Windows worker.
        let _ = manager.await;
        drop(states);
        if let Some(error) = failure {
            return Err(error);
        }
        if !restart {
            return Ok(());
        }
    }}.await;
    match tokio::task::spawn_blocking(||uu_core::virtual_display::Manager::shared().shutdown_manual()).await {
        Ok(Ok(()))=>{},
        Ok(Err(error))=>tracing::warn!(%error,"manual virtual display cleanup remains pending"),
        Err(error)=>tracing::warn!(%error,"manual virtual display cleanup worker failed"),
    }
    result
}

type Pending = Arc<Mutex<Vec<PairRequest>>>;
async fn serve(
    mut listener: NamedPipeServer,
    access: Arc<Access>,
    admin: Arc<host_admin::Service>,
    mut requests: mpsc::UnboundedReceiver<PairRequest>,
    endpoints: Vec<uu_core::host_runtime::Endpoint>,
    active_services: Arc<Vec<uu_core::device_book::Service>>,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let pending: Pending = Arc::default();
    let endpoints = Arc::new(endpoints);
    let mut tasks = tokio::task::JoinSet::new();
    let result = async { loop {
        tokio::select! {
            biased;
            _=stop.changed()=>break,
            request=requests.recv()=>if let Some(request)=request {
                let mut pending=pending.lock().unwrap();pending.retain(|r|matches!(*r.status.borrow(),PairStatus::Waiting|PairStatus::Verifying));pending.push(request);
            }else{break;},
            connected=listener.connect()=>{
                connected?;let next=pipe(false)?;let connected=std::mem::replace(&mut listener,next);
                if tasks.len()>=4 {drop(connected);continue;}
                let access=access.clone();let admin=admin.clone();let pending=pending.clone();let endpoints=endpoints.clone();let active_services=active_services.clone();
                tasks.spawn(async move {
                    let _=connection(connected,access,admin,pending,endpoints,active_services).await;
                });
            },
            _=tasks.join_next(),if !tasks.is_empty()=>{},
        }
    } Ok(()) }.await;
    drop(listener);
    while tasks.join_next().await.is_some() {}
    result
}
async fn connection(
    mut stream: NamedPipeServer,
    access: Arc<Access>,
    admin: Arc<host_admin::Service>,
    pending: Pending,
    endpoints: Arc<Vec<uu_core::host_runtime::Endpoint>>,
    active_services: Arc<Vec<uu_core::device_book::Service>>,
) -> Result<()> {
    let request: Request = tokio::time::timeout(Duration::from_secs(5), service_admin::read(&mut stream)).await??;
    let reply_admin = admin.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<(Response, bool)> {
        let profile = Paths::discover()?.active_profile()?.map(|p|p.info);
        let request = request.for_profile(profile.as_ref().map(|p|p.id.as_str()))?;
        ensure!(!access.sessions.configuration_switching() || !request.is_mutation()
            || matches!(request, Request::ConfigurationSwitch { .. }), "服务正在切换配置，请稍后重试。");
        let snapshot = || -> Result<host_admin::Snapshot> {
            match admin.execute_local_manager(&access, host_admin::Request::Inspect)? {
                host_admin::Response::Snapshot { snapshot } => Ok(snapshot),
                _ => anyhow::bail!("后台状态读取失败"),
            }
        };
        match request {
            Request::Endpoints => Ok((Response::Endpoints {
                endpoints: endpoints.iter().map(|endpoint|endpoint.snapshot()).collect(),
            }, false)),
            Request::ConfigurationSwitch { token, release } => {
                access.sessions.reserve_configuration_switch(&token, release)?;
                Ok((Response::Applied, false))
            },
            Request::Status => {
                let mut pending = pending.lock().unwrap();
                pending.retain(|r| {
                    matches!(
                        *r.status.borrow(),
                        PairStatus::Waiting | PairStatus::Verifying
                    )
                });
                let pending = pending
                    .iter()
                    .map(|r| service_admin::Pairing {
                        id: r.id.clone(),
                        device: r.device.clone(),
                        method: r.method,
                        waiting: matches!(*r.status.borrow(), PairStatus::Waiting),
                    })
                    .collect();
                let response = Response::Status {
                        ready: endpoints.iter().any(|e| matches!(*e.state.borrow(), uu_core::device_internet::EndpointStatus::Ready)),
                        keep_virtual_displays: uu_core::virtual_display::Manager::shared().keep_after_stream().ok(),
                        endpoints: endpoints
                            .iter()
                            .map(|e| (e.name.clone(), e.state.borrow().label()))
                            .collect(),
                        pending,
                        snapshot: snapshot()?,
                    };
                let response = if let Some(profile) = profile { Response::ProfileStatus { profile, response:Box::new(response) } } else { response };
                Ok((response, false))
            }
            Request::Approve {
                id,
                code,
                permissions,
            } => {
                let approval = PairApproval::new(&code, permissions)?;
                let mut pending = pending.lock().unwrap();
                let index = pending
                    .iter()
                    .position(|r| r.id == id)
                    .context("配对请求已结束")?;
                let request = pending.remove(index);
                ensure!(
                    matches!(*request.status.borrow(), PairStatus::Waiting),
                    "配对请求已结束"
                );
                request
                    .respond
                    .send(Some(approval))
                    .map_err(|_| anyhow::anyhow!("配对请求已结束"))?;
                Ok((Response::Applied, false))
            }
            Request::Reject { id } => {
                let mut pending = pending.lock().unwrap();
                let index = pending
                    .iter()
                    .position(|r| r.id == id)
                    .context("配对请求已结束")?;
                let request = pending.remove(index);
                let _ = request.respond.send(None);
                Ok((Response::Applied, false))
            }
            Request::Admin { request } => {
                let response = admin.execute_local_manager(&access, request)?;
                let restart = matches!(response, host_admin::Response::Saved { restart: true });
                Ok((Response::Admin { response }, restart))
            }
            Request::ConnectionHistory { revision } => Ok((Response::ConnectionHistory {
                snapshot: access.sessions.history.changed_since(revision),
            }, false)),
            Request::VirtualDisplay{request}=>{
                let response=uu_core::native_virtual_display::execute_local_owner(request).unwrap_or_else(|error|uu_core::native_virtual_display::Response::Error{message:uu_core::native_virtual_display::error_message(&error)});
                Ok((Response::VirtualDisplay{response},false))
            },
            Request::Trust { request } => {
                use service_admin::TrustRequest;
                match request {
                    TrustRequest::Inspect => {},
                    TrustRequest::Password { password, permissions } => {
                        if let Some(password) = password {
                            access.store.set_pairing_password(&password, permissions)?;
                        } else { access.store.clear_pairing_password()?; }
                    },
                    TrustRequest::Authorize { device, permissions } => {
                        access.store.authorize(device.clone(), permissions, None)?;
                        access.sessions.revoke(&device.id());
                    },
                    TrustRequest::Revoke { peer } => { access.revoke(&peer)?; },
                }
                Ok((Response::Trust { snapshot: service_admin::TrustSnapshot {
                    device: access.store.card(&access.name), peers: access.store.peers()?,
                    password_permissions: access.store.pairing_password_permissions()?,
                } }, false))
            },
            Request::Firewall { repair } => {
                let paths = Paths::discover()?;
                let installed = paths.manifest()?;
                let executable = firewall_program(&paths, &installed)?;
                        let sharing = uu_core::device_book::Store::open(paths.runtime_config()?.at("device-trust"))?.read()?.sharing;
                let report = if repair { uu_core::firewall::repair(&executable, sharing.service_port, sharing.discovery_port)? }
                    else { uu_core::firewall::inspect(&executable, sharing.service_port, sharing.discovery_port)? };
                Ok((Response::Firewall { message: report.message() }, false))
            },
            Request::Connections { request } => {
                let directory = Paths::discover()?.runtime_config()?.at("device-trust");
                let snapshot = service_admin::connections::execute(&directory, &active_services, request)?;
                Ok((Response::Connections { snapshot }, false))
            },
            Request::Plugins{request}=>{
                use service_admin::PluginRequest;
                let config=Paths::discover()?.runtime_config()?;
                let runtime=uu_plugin_sdk::runtime::shared(&config);
                let restart=matches!(&request,PluginRequest::Save{..});
                match request {
                    PluginRequest::Inspect=>{},
                    PluginRequest::Rescan=>{
                        let preferences=uu_core::plugins::Preferences::load(&config)?;
                        let mut runtime=runtime.lock().unwrap();runtime.rescan();runtime.apply(&preferences);
                    },
                    PluginRequest::Save{id,expected,value}=>{runtime.lock().unwrap().save(&config,&id,&expected,value)?;},
                }
                Ok((Response::Plugins{snapshot:plugin_snapshot(&config)?},restart))
            },
            Request::Restart => Ok((Response::Applied, true)),
            Request::Scoped { .. } => unreachable!("nested profile requests are rejected before dispatch"),
        }
    })
    .await?;
    let (response, restart) = match result {
        Ok(result) => result,
        Err(error) => (
            Response::Error {
                message: error.to_string(),
            },
            false,
        ),
    };
    tokio::time::timeout(Duration::from_secs(5), service_admin::write(&mut stream, &response)).await??;
    if restart {
        reply_admin.restart_local_manager()?;
    }
    Ok(())
}

fn plugin_snapshot(config:&uu_core::Config)->Result<service_admin::PluginSnapshot>{
    let preferences=uu_core::plugins::Preferences::load(config)?;
    let runtime=uu_plugin_sdk::runtime::shared(config);
    let runtime=runtime.lock().unwrap();
    let plugins=runtime.records.iter().map(|(id,record)|service_admin::PluginInfo{
        id:id.clone(),name:record.manifest.name.clone(),version:record.manifest.version.clone(),
        enabled:record.enabled,compatible:record.compatible,error:record.error.clone(),
        preferences:preferences.get(id),
    }).collect();
    Ok(service_admin::PluginSnapshot{directory:runtime.directory.display().to_string(),plugins})
}
