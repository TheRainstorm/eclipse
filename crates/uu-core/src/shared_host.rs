//! All listeners share one authorization context and one active media owner.
use crate::{
    device_auth::PairingControl,
    device_trust::{Permissions, TrustStore},
};
use anyhow::{ensure, Context, Result};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// One running host per private configuration, including separate launcher
/// processes. This lock is independent of the short-lived store update lock.
pub struct HostOwner(std::fs::File);
impl HostOwner {
    pub fn claim(directory: &std::path::Path) -> Result<Self> {
        std::fs::create_dir_all(directory)?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(directory.join("host.lock"))?;
        fs2::FileExt::try_lock_exclusive(&file)
            .context("另一个窗口已开启本机共享，请在原窗口管理共享")?;
        Ok(Self(file))
    }
}
impl Drop for HostOwner {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

struct Active {
    session: String,
    peer: String,
    stop: watch::Sender<bool>,
}
#[derive(Default)]
pub struct SessionGate(Mutex<Option<Active>>);
impl SessionGate {
    pub fn claim(self: &Arc<Self>, peer: &str) -> Result<SessionLease> {
        let mut active = self.0.lock().unwrap();
        ensure!(active.is_none(), "主机正在串流，请先断开当前设备");
        let session = uuid::Uuid::new_v4().to_string();
        let (stop, receiver) = watch::channel(false);
        *active = Some(Active {
            session: session.clone(),
            peer: peer.into(),
            stop,
        });
        Ok(SessionLease {
            gate: self.clone(),
            session,
            receiver,
        })
    }
    pub fn disconnect(&self) {
        if let Some(active) = &*self.0.lock().unwrap() {
            active.stop.send_replace(true);
        }
    }
    pub fn revoke(&self, peer: &str) {
        if let Some(active) = &*self.0.lock().unwrap() {
            if active.peer == peer {
                active.stop.send_replace(true);
            }
        }
    }
    pub fn active_peer(&self) -> Option<String> {
        self.0.lock().unwrap().as_ref().map(|v| v.peer.clone())
    }
}
pub struct SessionLease {
    gate: Arc<SessionGate>,
    session: String,
    receiver: watch::Receiver<bool>,
}
impl SessionLease {
    pub fn stopped(&self) -> watch::Receiver<bool> {
        self.receiver.clone()
    }
}
impl Drop for SessionLease {
    fn drop(&mut self) {
        let mut active = self.gate.0.lock().unwrap();
        if active.as_ref().is_some_and(|v| v.session == self.session) {
            *active = None;
        }
    }
}
#[derive(Clone)]
pub struct Access {
    pub store: TrustStore,
    pub name: String,
    pub pairing: Arc<PairingControl>,
    pub sessions: Arc<SessionGate>,
}
impl Access {
    pub fn revoke(&self, peer: &str) -> Result<()> {
        self.store.revoke(peer)?;
        self.sessions.revoke(peer);
        Ok(())
    }
    pub fn claim(&self, peer: &str) -> Result<SessionLease> {
        let record = self.store.peer(peer)?.context("控制端尚未配对")?;
        ensure!(
            !record.revoked && record.inbound.is_some(),
            "控制端授权已经撤销"
        );
        self.sessions.claim(peer)
    }
    /// A separate process can revoke a grant. Poll only the small private
    /// authorization record, never run this work on the media packet path.
    pub fn cancellation(
        &self,
        lease: &SessionLease,
        peer: &str,
        permission: Option<Permissions>,
        mut application: watch::Receiver<bool>,
    ) -> Cancellation {
        let (stop, receiver) = watch::channel(false);
        let mut session = lease.stopped();
        let store = self.store.clone();
        let peer = peer.to_owned();
        let task = tokio::spawn(async move {
            let mut timer = tokio::time::interval(std::time::Duration::from_millis(250));
            loop {
                if *application.borrow() || *session.borrow() {
                    break;
                }
                tokio::select! {
                    _=application.changed()=>break,
                    _=session.changed()=>break,
                    _=timer.tick()=>{
                        if permission.is_some() && store.peer(&peer).ok().flatten().and_then(|p|p.inbound)!=permission {break;}
                    }
                }
            }
            stop.send_replace(true);
        });
        Cancellation { receiver, task }
    }
}
pub struct Cancellation {
    pub receiver: watch::Receiver<bool>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Cancellation {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_one_host_owns_a_configuration_until_shutdown_finishes() {
        let root =
            std::env::temp_dir().join(format!("eclipse-host-owner-{}", uuid::Uuid::new_v4()));
        let owner = HostOwner::claim(&root).unwrap();
        assert!(HostOwner::claim(&root).is_err());
        drop(owner);
        let next = HostOwner::claim(&root).unwrap();
        drop(next);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn all_listeners_share_single_media_ownership_and_revocation_is_per_peer() {
        let gate = Arc::new(SessionGate::default());
        let a = gate.claim("a").unwrap();
        assert!(gate.claim("b").is_err());
        gate.revoke("b");
        assert!(!*a.receiver.borrow());
        gate.revoke("a");
        assert!(*a.receiver.borrow());
        assert!(
            gate.claim("b").is_err(),
            "stop request does not release ownership before cleanup"
        );
        drop(a);
        let b = gate.claim("b").unwrap();
        assert_eq!(gate.active_peer().as_deref(), Some("b"));
        gate.disconnect();
        assert!(*b.receiver.borrow());
        drop(b);
        assert!(gate.active_peer().is_none());
    }
}
