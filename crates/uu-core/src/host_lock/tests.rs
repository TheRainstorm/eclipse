use super::*;
use crate::shared_host::SessionGate;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub(crate) struct Fake {
    pub locked: AtomicBool,
    pub locks: AtomicUsize,
    pub queries: AtomicUsize,
    pub fail: AtomicBool,
}
impl Fake {
    fn new(locked: bool) -> Arc<Self> {
        Arc::new(Self {
            locked: AtomicBool::new(locked),
            locks: AtomicUsize::new(0),
            queries: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
        })
    }
}
impl Backend for Fake {
    fn desktop(&self) -> Result<Desktop> {
        self.queries.fetch_add(1, Ordering::Relaxed);
        Ok(Desktop {
            session: 7,
            logon: 10,
            locked: self.locked.load(Ordering::Relaxed),
        })
    }
    fn lock(&self, _: Desktop) -> Result<()> {
        anyhow::ensure!(!self.fail.load(Ordering::Relaxed), "simulated lock failure");
        self.locks.fetch_add(1, Ordering::Relaxed);
        self.locked.store(true, Ordering::Relaxed);
        Ok(())
    }
}
fn fixture(locked: bool, policy: Policy) -> (Arc<SessionGate>, Arc<Fake>) {
    let gate = Arc::new(SessionGate::default());
    let backend = Fake::new(locked);
    gate.desktop.enable(backend.clone(), policy);
    (gate, backend)
}
#[test]
fn only_last_session_restores_the_first_admission_snapshot() {
    let (gate, backend) = fixture(true, Policy::Restore);
    let a = gate.claim("a").unwrap();
    backend.locked.store(false, Ordering::Relaxed);
    gate.connected(a.id());
    let b = gate.claim("b").unwrap();
    gate.connected(b.id());
    assert_eq!(backend.queries.load(Ordering::Relaxed), 1);
    gate.session_ending(a.id(), None);
    drop(a);
    gate.expire_desktop_lock(true);
    assert_eq!(
        backend.locks.load(Ordering::Relaxed),
        0,
        "one screen/client cannot lock another"
    );
    gate.session_ending(b.id(), None);
    drop(b);
    gate.expire_desktop_lock(false);
    assert_eq!(backend.locks.load(Ordering::Relaxed), 1);
    assert!(gate
        .desktop_status(None)
        .unwrap()
        .originally_locked
        .is_none());
}
#[test]
fn unlocked_and_failed_connections_do_not_arm_restore() {
    for (locked, connected, policy, expected) in [
        (false, true, Policy::Restore, 0),
        (true, false, Policy::Always, 0),
        (true, true, Policy::Never, 0),
        (false, true, Policy::Always, 1),
    ] {
        let (gate, backend) = fixture(locked, policy);
        let lease = gate.claim("client").unwrap();
        if connected {
            gate.connected(lease.id());
        }
        gate.session_ending(lease.id(), None);
        drop(lease);
        gate.expire_desktop_lock(false);
        assert_eq!(backend.locks.load(Ordering::Relaxed), expected);
    }
}
#[test]
fn unlocked_desktop_finishes_its_cycle_before_the_next_admission() {
    let (gate, backend) = fixture(false, Policy::Restore);
    let first = gate.claim("client").unwrap();
    gate.connected(first.id());
    drop(first);
    backend.locked.store(true, Ordering::Relaxed);
    let second = gate.claim("client").unwrap();
    assert_eq!(
        gate.desktop_status(None).unwrap().originally_locked,
        Some(true)
    );
    assert_eq!(backend.queries.load(Ordering::Relaxed), 2);
    drop(second);
}
#[test]
fn reconnect_grace_retains_original_state_and_failed_retry_does_not_extend_it() {
    let (gate, backend) = fixture(true, Policy::Restore);
    let first = gate.claim("client").unwrap();
    gate.connected(first.id());
    backend.locked.store(false, Ordering::Relaxed);
    drop(first);
    gate.expire_desktop_lock(false);
    assert_eq!(backend.locks.load(Ordering::Relaxed), 0);
    let due = gate.desktop.0.lock().unwrap().due.unwrap();
    let retry = gate.claim("client").unwrap();
    assert_eq!(
        gate.desktop_status(None).unwrap().originally_locked,
        Some(true)
    );
    gate.expire_desktop_lock(true);
    assert_eq!(backend.locks.load(Ordering::Relaxed), 0);
    drop(retry);
    assert_eq!(gate.desktop.0.lock().unwrap().due, Some(due));
    gate.desktop.expire(due.at - Duration::from_millis(1), false);
    assert_eq!(backend.locks.load(Ordering::Relaxed), 0);
    gate.desktop.expire(due.at, false);
    assert_eq!(backend.locks.load(Ordering::Relaxed), 1);
}
#[test]
fn successful_reconnect_cancels_expiry_and_starts_a_new_grace_only_after_exit() {
    let (gate, backend) = fixture(true, Policy::Restore);
    let lease = gate.claim("client").unwrap();
    gate.connected(lease.id());
    drop(lease);
    let next = gate.claim("client").unwrap();
    gate.connected(next.id());
    assert!(gate.desktop.0.lock().unwrap().due.is_none());
    assert_eq!(backend.queries.load(Ordering::Relaxed), 1);
    gate.session_ending(next.id(), None);
    drop(next);
    gate.expire_desktop_lock(false);
    assert_eq!(backend.locks.load(Ordering::Relaxed), 1);
}
#[test]
fn policy_changes_and_lock_errors_are_reported_without_automatic_retry_loops() {
    let (gate, backend) = fixture(true, Policy::Restore);
    let lease = gate.claim("client").unwrap();
    gate.connected(lease.id());
    backend.fail.store(true, Ordering::Relaxed);
    gate.session_ending(lease.id(), None);
    drop(lease);
    gate.expire_desktop_lock(false);
    assert!(gate.desktop_status(None).unwrap().error.is_some());
    backend.fail.store(false, Ordering::Relaxed);
    gate.expire_desktop_lock(true);
    assert_eq!(backend.locks.load(Ordering::Relaxed), 0);
    let lease = gate.claim("client").unwrap();
    gate.connected(lease.id());
    gate.configure_lock_policy(Policy::Never);
    gate.session_ending(lease.id(), None);
    drop(lease);
    gate.expire_desktop_lock(false);
    assert_eq!(backend.locks.load(Ordering::Relaxed), 0);
    assert!(gate.lock_desktop(Some("nonexistent")).is_err());
}

#[test]
fn a_session_can_choose_lock_or_keep_desktop_on_explicit_exit() {
    for (policy, choice, locks) in [(Policy::Never, true, 1), (Policy::Always, false, 0)] {
        let (gate, backend) = fixture(false, policy);
        let desktop = &gate.desktop;
        desktop.begin();
        desktop.connected();
        desktop.end(true, Some(choice), Instant::now());
        desktop.expire(Instant::now(), false);
        assert_eq!(backend.locks.load(Ordering::Relaxed), locks);
    }
    let (gate, backend) = fixture(false, Policy::Never);
    let view_only = gate.claim("viewer").unwrap();
    gate.connected(view_only.id());
    gate.session_ending(view_only.id(), Some(true));
    drop(view_only);
    gate.expire_desktop_lock(false);
    assert_eq!(backend.locks.load(Ordering::Relaxed), 0);
}
