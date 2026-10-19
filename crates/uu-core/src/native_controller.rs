//! Native viewer adapter for the shared GUI session handle. No interoperability API.
use crate::{
    controller::{ControllerEvent, ControllerHandle},
    media::{frame_channel, FrameSender, MediaStats},
    native_pairing::Invitation,
};
use anyhow::{Context, Result};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{
    mpsc::{unbounded_channel, UnboundedReceiver},
    watch,
};

pub fn spawn(invitation_file: PathBuf, stats: Arc<MediaStats>) -> Result<ControllerHandle> {
    stats.capabilities.set(0);
    stats.clipboard.enabled.store(false, Ordering::Release);
    stats.clipboard.available.store(false, Ordering::Release);
    let (events, event_rx) = std::sync::mpsc::channel();
    let (frames, frame_rx) = frame_channel();
    let (input, input_rx) = unbounded_channel();
    let (settings, settings_rx) = unbounded_channel();
    let (close, close_rx) = unbounded_channel();
    let stopped = Arc::new(AtomicBool::new(false));
    let worker = Worker {
        invitation_file,
        events,
        frames,
        input: input_rx,
        settings: settings_rx,
        close: close_rx,
        stats: stats.clone(),
        stopped: stopped.clone(),
    };
    std::thread::Builder::new()
        .name("native-viewer".into())
        .spawn(move || worker.run())
        .context("cannot start native viewing thread")?;
    Ok(ControllerHandle {
        stopped,
        events: event_rx,
        frames: frame_rx,
        input,
        settings,
        close,
        stats,
    })
}

struct Worker {
    invitation_file: PathBuf,
    events: std::sync::mpsc::Sender<ControllerEvent>,
    frames: FrameSender,
    input: UnboundedReceiver<Vec<u8>>,
    settings: UnboundedReceiver<Vec<u8>>,
    close: UnboundedReceiver<()>,
    stats: Arc<MediaStats>,
    stopped: Arc<AtomicBool>,
}
impl Worker {
    fn run(mut self) {
        let result = match tokio::runtime::Runtime::new() {
            Ok(runtime) => {
                let result = runtime.block_on(self.network());
                self.stop_local();
                runtime.shutdown_timeout(Duration::from_secs(2));
                result
            }
            Err(error) => Err(anyhow::anyhow!(
                "cannot start native viewing runtime: {error}"
            )),
        };
        match result {
            Err(error) => {
                let _ = self
                    .events
                    .send(ControllerEvent::Error(format!("原生连接失败：{error}")));
            }
            Ok(()) => {
                let _ = self
                    .events
                    .send(ControllerEvent::Status("原生会话已结束".into()));
            }
        }
        self.stop_local();
        self.stopped.store(true, Ordering::Release);
    }
    fn stop_local(&self) {
        self.stats.audio.stop();
        self.stats.clipboard.stopped.store(true, Ordering::Release);
    }
    async fn network(&mut self) -> Result<()> {
        let _ = self
            .events
            .send(ControllerEvent::Status("正在验证原生邀请…".into()));
        let invitation = Invitation::load(&self.invitation_file)?;
        let (stop, stop_rx) = watch::channel(false);
        let session = crate::native::view(
            &invitation,
            self.frames.clone(),
            self.stats.clone(),
            stop_rx,
        );
        tokio::pin!(session);
        loop {
            tokio::select! {
                result=&mut session=>return result,
                _=self.close.recv()=>{let _=stop.send(true);return session.await;},
                data=self.input.recv()=>{if data.is_none(){let _=stop.send(true);return session.await;}},
                data=self.settings.recv()=>{if data.is_none(){let _=stop.send(true);return session.await;}},
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn closing_during_tls_setup_stops_without_waiting_for_the_auth_timeout() {
        use tokio::{io::AsyncReadExt, net::TcpListener, time::timeout};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let credentials = crate::native_pairing::HostCredentials::new(
            listener.local_addr().unwrap(),
            Duration::from_secs(60),
        )
        .unwrap();
        let path = std::env::temp_dir().join(format!(
            "uu-native-cancel-{}.eclipse-invite.json",
            uuid::Uuid::new_v4()
        ));
        credentials.invitation().save_new(&path).unwrap();
        let stats = Arc::new(MediaStats::default());
        let handle = spawn(path.clone(), stats.clone()).unwrap();
        let outcome = async {
            let (mut socket, _) = timeout(Duration::from_secs(2), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut first = [0u8; 1];
            timeout(Duration::from_secs(2), socket.read_exact(&mut first))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                first[0], 22,
                "expected a TLS handshake before any credential"
            );
            handle.input.send(vec![1, 2, 3]).unwrap();
            handle.settings.send(vec![4, 5, 6]).unwrap();
            handle.close.send(()).unwrap();
            timeout(Duration::from_secs(2), async {
                while !handle.stopped.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            let mut remaining = Vec::new();
            timeout(Duration::from_secs(1), socket.read_to_end(&mut remaining))
                .await
                .unwrap()
                .unwrap();
            assert!(!stats
                .capabilities
                .allows(crate::capabilities::Capability::Input));
            assert_eq!(stats.input_messages_sent.load(Ordering::Relaxed), 0);
        }
        .await;
        std::fs::remove_file(path).unwrap();
        outcome
    }
}
