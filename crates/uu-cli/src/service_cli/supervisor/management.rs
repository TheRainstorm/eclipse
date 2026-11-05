//! One local management endpoint owned by the SCM process. Console media
//! restarts do not change the GUI's authority or configuration location.
use super::*;
use tokio::{net::windows::named_pipe::NamedPipeServer, sync::watch};
use uu_core::service_admin::{self, Request, Response};

pub(super) struct Management {
    stop: watch::Sender<bool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Management {
    pub fn is_finished(&self) -> bool {
        self.worker
            .as_ref()
            .is_none_or(|worker| worker.is_finished())
    }
    pub fn start(installed: Installed) -> Result<Self> {
        // Create the first instance synchronously: no unprotected fallback if
        // another process has occupied the name or the owner ACL cannot be set.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let listener = {
            let _entered = runtime.enter();
            owner_pipe(true, &installed)?
        };
        let (stop, receive) = watch::channel(false);
        let worker = std::thread::spawn(move || {
            if let Err(error) = runtime.block_on(serve(listener, installed, receive)) {
                tracing::error!(%error, "local service management stopped");
            }
        });
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}
impl Drop for Management {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
async fn serve(
    mut listener: NamedPipeServer,
    installed: Installed,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let mut tasks = tokio::task::JoinSet::new();
    let result = async {
        loop {
            tokio::select! {
                biased;
                _ = stop.changed() => break,
                connected = listener.connect() => {
                    connected?;
                    let next = owner_pipe(false, &installed)?;
                    let connected = std::mem::replace(&mut listener, next);
                    if tasks.len() >= 4 { drop(connected); continue; }
                    tasks.spawn(async move { let _ = connection(connected).await; });
                },
                _ = tasks.join_next(), if !tasks.is_empty() => {},
            }
        }
        Ok(())
    }
    .await;
    drop(listener);
    // In particular, do not abandon an accepted blocking display mutation.
    while tasks.join_next().await.is_some() {}
    result
}
async fn connection(mut stream: NamedPipeServer) -> Result<()> {
    let request: Request =
        tokio::time::timeout(Duration::from_secs(5), service_admin::read(&mut stream)).await??;
    // This is a closed, bounded protocol. No command, executable, configuration
    // path, plugin payload or private-key export can be supplied through it.
    let response = match uu_core::service_admin::windows::request(request).await {
        Ok(response) => response,
        Err(error)
            if error
                .downcast_ref::<service_admin::SubmittedUnknown>()
                .is_some() =>
        {
            Response::SubmittedUnknown {
                message: format!("{error:#}"),
            }
        }
        Err(error) => Response::Error {
            message: format!("{error:#}"),
        },
    };
    tokio::time::timeout(
        Duration::from_secs(5),
        service_admin::write(&mut stream, &response),
    )
    .await??;
    Ok(())
}
