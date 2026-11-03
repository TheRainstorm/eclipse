//! All listeners share authorization, session ownership and revocation.
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

#[derive(Clone)]
pub struct SessionContext {
    pub access: Arc<Access>,
    pub peer: String,
    pub permissions: Permissions,
    /// The authenticated session lease, independent of peer/device identity.
    pub session_id: Option<String>,
}
struct Active { peer: String, stop: watch::Sender<bool>, status: crate::native::HostStatus }
#[derive(Default)]
pub struct SessionGate {
    active: Mutex<HashMap<String, Active>>,
    pub input: Arc<crate::input_ownership::Authority>,
    pub administration: Mutex<Option<Arc<crate::host_admin::Service>>>,
}
impl SessionGate {
    pub(crate) fn preparation_activity(&self, initial:Option<&str>)->u32 {
        self.active.lock().map(|sessions|sessions.iter().filter(|(id,active)| {
            !(Some(id.as_str())==initial && !*active.stop.borrow() && matches!(active.status,crate::native::HostStatus::Starting|crate::native::HostStatus::Preparing))
        }).count().try_into().unwrap_or(u32::MAX)).unwrap_or(u32::MAX)
    }
    /// Authenticated sessions have no fixed application count limit. Capture,
    /// encoder and transport admission failures remain local to each session.
    pub fn claim(self: &Arc<Self>, peer: &str) -> Result<SessionLease> {
        let mut active = self.active.lock().unwrap();
        // This check shares the activity mutex with the preparation checkpoint:
        // either the new session is observed, or admission waits for the host's
        // accepted machine-wide setup transaction. Existing streams continue.
        anyhow::ensure!(!crate::virtual_display::Manager::shared().preparation_in_flight(),
            "{}",uu_i18n::text("源机正在准备虚拟副屏，请在准备完成后连接。"));
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
    pub fn id(&self)->&str {&self.session}
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
    fn sessions_have_no_fixed_count_limit_and_revocation_is_scoped_until_cleanup() {
        let gate = Arc::new(SessionGate::default());
        let a = gate.claim("a").unwrap();
        let b = gate.claim("b").unwrap();
        let others: Vec<_> = (0..8).map(|_| gate.claim("other").unwrap()).collect();
        assert_eq!(gate.sessions().len(), 10);
        gate.revoke("a");
        assert!(*a.receiver.borrow()); assert!(!*b.receiver.borrow());
        assert_eq!(gate.sessions().len(), 10, "cancellation retains ownership until cleanup");
        let e = gate.claim("e").unwrap();
        drop(a);
        assert_eq!(gate.sessions().len(), 10);
        gate.disconnect_session(&b.session);
        assert!(*b.receiver.borrow()); assert!(!*e.receiver.borrow());
        gate.disconnect();
        for lease in others.iter().chain([&b,&e]) { assert!(*lease.receiver.borrow()); }
        drop((b,others,e)); assert!(gate.sessions().is_empty());
    }
    #[test]
    fn preparation_exempts_only_the_exact_pending_connection_never_streaming_or_other_sessions() {
        let gate=Arc::new(SessionGate::default());
        let initial=gate.claim("same-peer").unwrap();
        assert_eq!(gate.preparation_activity(Some(initial.id())),0);
        assert_eq!(gate.preparation_activity(None),1);
        assert_eq!(gate.preparation_activity(Some("unknown")),1);
        initial.progress(crate::native::HostStatus::Preparing);
        assert_eq!(gate.preparation_activity(Some(initial.id())),0);
        let parallel=gate.claim("same-peer").unwrap();
        assert_eq!(gate.preparation_activity(Some(initial.id())),1);
        drop(parallel);
        initial.progress(crate::native::HostStatus::Streaming {width:1920,height:1080,fps:60,audio:"off".into(),encoder:"fake".into()});
        assert_eq!(gate.preparation_activity(Some(initial.id())),1);
        drop(initial);assert_eq!(gate.preparation_activity(None),0);
    }

    #[test]
    fn a_cancelled_initial_connection_is_not_exempt_from_idle_admission() {
        let gate=Arc::new(SessionGate::default());let initial=gate.claim("peer").unwrap();
        assert_eq!(gate.preparation_activity(Some(initial.id())),0);
        initial.cancel();assert_eq!(gate.preparation_activity(Some(initial.id())),1);
        drop(initial);assert_eq!(gate.preparation_activity(None),0);
    }

}
