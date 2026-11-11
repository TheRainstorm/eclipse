//! Blocking service management transport. The UI submits jobs and consumes
//! events; this owner holds the worker stop flag and bounded channels.
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};
use uu_core::service_admin::{
    windows::{build_status, control_owner, request_owner, status, BuildStatus},
    PluginRequest, Request, Response, TrustRequest,
};
use windows_sys::Win32::System::Services::SERVICE_RUNNING;

pub(super) enum Job {
    Refresh,
    Start,
    Stop,
    HistoryRefresh,
    Rpc(Request),
}
impl Job {
    /// Background refresh has no completion event; locking the whole service
    /// form for it leaves stale busy state after a successful deployment.
    pub(super) fn foreground(&self) -> bool {
        !matches!(self, Self::Refresh | Self::HistoryRefresh)
    }
}
pub(super) enum Event {
    State {
        state: u32,
        build: Option<BuildStatus>,
        response: Result<Response, String>,
    },
    Done(Result<Response, String>),
    History(Result<Response, String>),
    Trust {
        completed: bool,
        result: Result<Response, String>,
    },
    Connections {
        completed: bool,
        result: Result<Response, String>,
    },
    Plugins {
        completed: bool,
        result: Result<Response, String>,
    },
}

pub(super) struct Worker {
    pub sender: mpsc::SyncSender<Job>,
    pub events: mpsc::Receiver<Event>,
    stop: Arc<AtomicBool>,
}
impl Worker {
    pub(super) fn start() -> Self {
        let (sender, jobs) = mpsc::sync_channel(8);
        let (send, events) = mpsc::sync_channel(16);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(r) => r,
                Err(e) => {
                    let _ = send.send(Event::Done(Err(e.to_string())));
                    return;
                }
            };
            let mut trust_at = Instant::now();
            let mut plugins_at = Instant::now();
            let mut history_at = Instant::now();
            let mut history_revision = None;
            while !stopped.load(Ordering::Acquire) {
                match jobs.recv_timeout(Duration::from_millis(750)) {
                    Ok(Job::Refresh) => {}
                    Ok(Job::HistoryRefresh) => { history_at = Instant::now(); history_revision = None; }
                    Ok(job @ (Job::Start | Job::Stop)) => {
                        let result = control_owner(matches!(job, Job::Start))
                            .map(|_| Response::Applied)
                            .map_err(|e| format!("{e:#}"));
                        let _ = send.send(Event::Done(result));
                    }
                    Ok(Job::Rpc(request)) => {
                        let trust = matches!(request, Request::Trust { .. });
                        let connections = matches!(request, Request::Connections { .. });
                        let plugins = matches!(request, Request::Plugins { .. });
                        let result = runtime
                            .block_on(request_owner(request))
                            .map_err(|e| format!("{e:#}"));
                        let _ = send.send(if trust {
                            Event::Trust {
                                completed: true,
                                result,
                            }
                        } else if connections {
                            Event::Connections {
                                completed: true,
                                result,
                            }
                        } else if plugins {
                            Event::Plugins {
                                completed: true,
                                result,
                            }
                        } else {
                            Event::Done(result)
                        });
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                let build = build_status().ok();
                let state = build
                    .as_ref()
                    .map(|b| b.state)
                    .unwrap_or_else(|| match status() {
                        Ok(state) => state,
                        Err(error)
                            if error.downcast_ref::<std::io::Error>().is_some_and(|e| {
                                e.raw_os_error()
                                    == Some(
                                        windows_sys::Win32::Foundation::ERROR_SERVICE_DOES_NOT_EXIST
                                            as i32,
                                    )
                            }) =>
                        {
                            0
                        }
                        Err(_) => u32::MAX,
                    });
                let response = if state == SERVICE_RUNNING {
                    runtime
                        .block_on(request_owner(Request::Status))
                        .map_err(|e| format!("{e:#}"))
                } else {
                    Err(uu_i18n::text("后台服务未启动；启动服务后读取和修改同一份配置。").into())
                };
                if send
                    .send(Event::State {
                        state,
                        build,
                        response,
                    })
                    .is_err()
                {
                    break;
                }
                if state == SERVICE_RUNNING && Instant::now() >= trust_at {
                    let result = runtime
                        .block_on(request_owner(Request::Trust {
                            request: TrustRequest::Inspect,
                        }))
                        .map_err(|e| format!("{e:#}"));
                    if send
                        .send(Event::Trust {
                            completed: false,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    trust_at = Instant::now() + Duration::from_secs(5);
                    let result = runtime
                        .block_on(request_owner(Request::Connections {
                            request: uu_core::service_admin::connections::Request::Inspect,
                        }))
                        .map_err(|e| format!("{e:#}"));
                    if send
                        .send(Event::Connections {
                            completed: false,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                if state == SERVICE_RUNNING && Instant::now() >= plugins_at {
                    let result = runtime
                        .block_on(request_owner(Request::Plugins {
                            request: PluginRequest::Inspect,
                        }))
                        .map_err(|e| format!("{e:#}"));
                    if send
                        .send(Event::Plugins {
                            completed: false,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    plugins_at = Instant::now() + Duration::from_secs(5);
                }
                if Instant::now() >= history_at {
                    let result = if state == SERVICE_RUNNING {
                        runtime.block_on(request_owner(Request::ConnectionHistory { revision: history_revision }))
                            .map_err(|error| format!("{error:#}"))
                    } else {
                        history_revision = None;
                        uu_core::Config::from_env().map_err(|error| error.to_string()).and_then(|config|
                            uu_core::connection_history::History::read(&config.at("server/device-trust/connection-history.json"))
                                .map(|snapshot| Response::ConnectionHistory { snapshot: Some(snapshot.inactive()) })
                                .map_err(|error| format!("{error:#}")))
                    };
                    if let Ok(Response::ConnectionHistory { snapshot: Some(snapshot) }) = &result {
                        history_revision = Some(snapshot.revision);
                    }
                    if send.send(Event::History(result)).is_err() { break; }
                    history_at = Instant::now() + Duration::from_secs(5);
                }
            }
        });

        Self {
            sender,
            events,
            stop,
        }
    }
    #[cfg(test)]
    pub(super) fn from_channels(
        sender: mpsc::SyncSender<Job>,
        events: mpsc::Receiver<Event>,
        stop: Arc<AtomicBool>,
    ) -> Self {
        Self {
            sender,
            events,
            stop,
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}
