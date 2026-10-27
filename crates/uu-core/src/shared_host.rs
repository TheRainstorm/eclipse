//! All listeners share one authorization context and bounded concurrent media sessions.
use crate::{
    device_auth::PairingControl,
    device_trust::{Permissions, TrustStore},
};
use anyhow::{ensure, Context, Result};
use std::{collections::HashMap, sync::{Arc, Mutex}};
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
            .context(uu_i18n::text("另一个窗口已开启本机共享，请在原窗口管理共享"))?;
        Ok(Self(file))
    }
}
impl Drop for HostOwner {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

pub const MAX_SESSIONS: usize = 4;
#[derive(Clone)]
pub struct SessionContext {
    pub access: Arc<Access>,
    pub peer: String,
    pub permissions: Permissions,
}
struct Active { peer: String, stop: watch::Sender<bool>, status: crate::native::HostStatus }
#[derive(Default)]
pub struct SessionGate {
    active: Mutex<HashMap<String, Active>>,
    pub input: Arc<crate::input_ownership::Authority>,
    pub administration: Mutex<Option<Arc<crate::host_admin::Service>>>,
}
impl SessionGate {
    pub fn claim(self: &Arc<Self>, peer: &str) -> Result<SessionLease> {
        let mut active = self.active.lock().unwrap();
        ensure!(active.len() < MAX_SESSIONS, "{}", uu_i18n::format!("主机最多同时接受 {MAX_SESSIONS} 个串流，请先断开一个设备"));
        let session = uuid::Uuid::new_v4().to_string();
        let (stop, receiver) = watch::channel(false);
        active.insert(session.clone(), Active { peer: peer.into(), stop, status: crate::native::HostStatus::Starting });
        Ok(SessionLease { gate: self.clone(), session, receiver,temporary:None })
    }
    pub fn disconnect(&self) {
        for active in self.active.lock().unwrap().values() { active.stop.send_replace(true); }
    }
    pub fn revoke(&self, peer: &str) {
        for active in self.active.lock().unwrap().values().filter(|s| s.peer == peer) { active.stop.send_replace(true); }
    }
    pub fn active_peer(&self) -> Option<String> { self.active.lock().unwrap().values().next().map(|v| v.peer.clone()) }
    pub fn sessions(&self) -> Vec<(String, String)> {
        let mut sessions: Vec<_> = self.active.lock().unwrap().iter().map(|(id,s)| (id.clone(),s.peer.clone())).collect();
        sessions.sort(); sessions
    }
    pub fn details(&self) -> Vec<(String, String, crate::native::HostStatus)> {
        let mut sessions: Vec<_> = self.active.lock().unwrap().iter().map(|(id,s)| (id.clone(),s.peer.clone(),s.status.clone())).collect();
        sessions.sort_by(|a,b|a.0.cmp(&b.0)); sessions
    }
    pub fn disconnect_session(&self, id: &str) {
        if let Some(active) = self.active.lock().unwrap().get(id) { active.stop.send_replace(true); }
    }
}
pub struct SessionLease { gate: Arc<SessionGate>, session: String, receiver: watch::Receiver<bool>,temporary:Option<crate::device_auth::TemporaryGrant> }
impl SessionLease {
    pub fn cancel(&self) { self.gate.disconnect_session(&self.session); }
    pub fn stopped(&self) -> watch::Receiver<bool> { self.receiver.clone() }
    pub fn progress(&self, status: crate::native::HostStatus) {
        if let Some(active)=self.gate.active.lock().unwrap().get_mut(&self.session) {active.status=status;}
    }
}
impl Drop for SessionLease { fn drop(&mut self) { self.gate.active.lock().unwrap().remove(&self.session); } }
#[derive(Clone)]
pub struct Access {
    pub store: TrustStore,
    pub name: String,
    pub pairing: Arc<PairingControl>,
    pub sessions: Arc<SessionGate>,
}
impl Access {
    pub fn peer_permissions(&self,peer:&str)->Result<Option<(crate::device_trust::PublicDevice,Permissions)>> {
        if let Some(record)=self.pairing.temporary_peer(peer) {return Ok(Some(record));}
        Ok(self.store.peer(peer)?.filter(|r|!r.revoked).and_then(|r|r.inbound.map(|p|(r.device,p))))
    }
    pub fn set_permissions(&self,peer:&str,permissions:Permissions)->Result<()> {
        permissions.validate()?;
        if self.pairing.revoke_temporary(peer) {
            self.sessions.revoke(peer);
            return Ok(());
        }
        let record=self.store.peer(peer)?.context(uu_i18n::text("设备授权不存在"))?;
        ensure!(!record.revoked && record.inbound.is_some(),"{}", uu_i18n::format!("设备授权已撤销"));
        self.store.authorize(record.device,permissions,None)?;
        self.sessions.revoke(peer);
        Ok(())
    }
    pub fn revoke(&self, peer: &str) -> Result<()> {
        let temporary=self.pairing.revoke_temporary(peer);
        if self.store.peer(peer)?.is_some() {self.store.revoke(peer)?;}
        else {ensure!(temporary,"{}", uu_i18n::format!("设备授权不存在"));}
        self.sessions.revoke(peer);
        Ok(())
    }
    pub fn claim(&self, peer: &str) -> Result<SessionLease> {
        let record = self.store.peer(peer)?.context(uu_i18n::text("控制端尚未配对"))?;
        ensure!(
            !record.revoked && record.inbound.is_some(),
            "{}", uu_i18n::format!("控制端授权已经撤销"));
        self.sessions.claim(peer)
    }
    pub(crate) fn claim_authenticated(&self,authorized:&mut crate::device_auth::AuthenticatedPeer)->Result<SessionLease> {
        if let Some(temporary)=authorized.temporary.take() {
            ensure!(self.pairing.valid_temporary(&temporary.probe()),"{}", uu_i18n::format!("临时授权已结束"));
            let mut lease=self.sessions.claim(&authorized.device.id())?;
            lease.temporary=Some(temporary);
            Ok(lease)
        }else{self.claim(&authorized.device.id())}
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
        let access = self.clone();
        let temporary=lease.temporary.as_ref().map(|grant|grant.probe());
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
                        if let Some(probe)=&temporary {
                            if !access.pairing.valid_temporary(probe) {break;}
                            if permission.is_some() && access.peer_permissions(&peer).ok().flatten().map(|(_,p)|p)!=permission {break;}
                        }else if permission.is_some() && access.store.peer(&peer).ok().flatten().and_then(|p|p.inbound)!=permission {break;}
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
    fn concurrent_sessions_are_bounded_and_revocation_only_stops_the_selected_peer() {
        let gate = Arc::new(SessionGate::default());
        let a = gate.claim("a").unwrap();
        let b = gate.claim("b").unwrap();
        let c = gate.claim("c").unwrap();
        let d = gate.claim("d").unwrap();
        assert!(gate.claim("e").is_err());
        gate.revoke("a");
        assert!(*a.receiver.borrow()); assert!(!*b.receiver.borrow());
        assert!(gate.claim("e").is_err(), "cancellation retains the slot until cleanup");
        drop(a); let e = gate.claim("e").unwrap();
        gate.disconnect_session(&b.session);
        assert!(*b.receiver.borrow()); assert!(!*e.receiver.borrow());
        gate.disconnect();
        for lease in [&b,&c,&d,&e] { assert!(*lease.receiver.borrow()); }
        drop((b,c,d,e)); assert!(gate.sessions().is_empty());
    }
}
