use super::*;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Default)]
struct Fake {
    targets: Mutex<Vec<Target>>,
    fail_recover: AtomicBool,
    recover_calls: AtomicUsize,
    inventory_calls: AtomicUsize,
    fail_remove: AtomicBool,
    fail_after_activate: AtomicBool,
    activated: Mutex<Vec<TargetKey>>,
    removed: Mutex<Vec<TargetKey>>,
    claims: Mutex<BTreeSet<TargetKey>>,
    provision_calls: AtomicUsize,
    provisioned: Mutex<Option<Target>>,
}

struct FakeClaim {
    backend: Arc<Fake>,
    key: TargetKey,
}
impl Claim for FakeClaim {}
impl Drop for FakeClaim {
    fn drop(&mut self) {
        assert!(self.backend.claims.lock().unwrap().remove(&self.key));
    }
}
impl Backend for Arc<Fake> {
    fn recover(&self) -> Result<()> {
        self.recover_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_recover.load(Ordering::SeqCst) {
            Err(Error::Platform("adapter rollback remains pending".into()))
        } else {
            Ok(())
        }
    }
    fn claim(&self, key: &TargetKey) -> Result<Box<dyn Claim>> {
        if !self.claims.lock().unwrap().insert(key.clone()) {
            return Err(Error::Claimed);
        }
        Ok(Box::new(FakeClaim {
            backend: self.clone(),
            key: key.clone(),
        }))
    }
    fn is_claimed(&self, key: &TargetKey) -> Result<bool> {
        Ok(self.claims.lock().unwrap().contains(key))
    }
    fn provision(&self) -> Result<()> {
        self.provision_calls.fetch_add(1, Ordering::SeqCst);
        let target = self
            .provisioned
            .lock()
            .unwrap()
            .take()
            .ok_or(Error::NoAvailableSlot)?;
        self.targets.lock().unwrap().push(target);
        Ok(())
    }
    fn inventory(&self) -> Result<Inventory> {
        self.inventory_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Inventory {
            supported: true,
            driver_present: true,
            targets: self.targets.lock().unwrap().clone(),
        })
    }
    fn activate(
        &self,
        key: &TargetKey,
        spec: Spec,
    ) -> std::result::Result<Target, ActivationFailure> {
        let mut targets = self.targets.lock().unwrap();
        let Some(target) = targets
            .iter_mut()
            .find(|target| &target.key == key && !target.active)
        else {
            return Err(ActivationFailure {
                message: "target was activated externally".into(),
                activated: false,
            });
        };
        target.active = true;
        target.actual = Some(spec);
        self.activated.lock().unwrap().push(key.clone());
        if self.fail_after_activate.load(Ordering::Relaxed) {
            return Err(ActivationFailure {
                message: "mode readback mismatch".into(),
                activated: true,
            });
        }
        Ok(target.clone())
    }
    fn deactivate(&self, key: &TargetKey) -> Result<()> {
        if self.fail_remove.load(Ordering::Relaxed) {
            return Err(Error::Platform("busy".into()));
        }
        self.removed.lock().unwrap().push(key.clone());
        if let Some(target) = self
            .targets
            .lock()
            .unwrap()
            .iter_mut()
            .find(|target| &target.key == key)
        {
            target.active = false;
        }
        Ok(())
    }
    fn prepare(&self, key: &TargetKey) -> Result<()> {
        {
            let targets = self.targets.lock().unwrap();
            if targets
                .iter()
                .any(|target| target.active && &target.key == key)
                && targets.iter().filter(|target| target.active).count() <= 1
            {
                return Err(Error::LastActiveOutput);
            }
        }
        self.deactivate(key)
    }
}

fn fixture() -> (Arc<Manager>, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    for id in 1..=3 {
        fake.targets.lock().unwrap().push(Target {
            key: TargetKey {
                adapter: (7, 8),
                target: id,
                monitor_path: format!("owned-adapter-monitor-{id}"),
            },
            name: "virtual".into(),
            source: format!("\\\\.\\DISPLAY{id}"),
            active: id == 1,
            available: true,
            actual: None,
        });
    }
    (
        Arc::new(Manager {
            backend: Box::new(fake.clone()),
            entries: Mutex::new(BTreeMap::new()),
            sessions: Mutex::new(BTreeMap::new()),
            manual_closing: AtomicBool::new(false),
        }),
        fake,
    )
}

#[test]
fn manual_and_two_sessions_never_take_active_targets_or_each_others_leases() {
    let (manager, backend) = fixture();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let _a = manager.session_guard(a);
    let _b = manager.session_guard(b);
    let manual = manager.create(Owner::Manual, Spec::default()).unwrap();
    let session = manager.create(Owner::Session(a), Spec::default()).unwrap();
    assert_eq!(manual.source, "\\\\.\\DISPLAY2");
    assert_eq!(session.source, "\\\\.\\DISPLAY3");
    assert!(matches!(
        manager.create(Owner::Session(b), Spec::default()),
        Err(Error::NoAvailableSlot)
    ));
    assert!(matches!(
        manager.remove_session(b, session.id),
        Err(Error::WrongOwner)
    ));
    assert!(matches!(
        manager.remove_session(a, manual.id),
        Err(Error::WrongOwner)
    ));
    manager.release_session(a).unwrap();
    assert_eq!(manager.list().unwrap().len(), 1);
    assert_eq!(manager.list().unwrap()[0].id, manual.id);
    assert!(backend.targets.lock().unwrap()[0].active);
    manager.remove(manual.id).unwrap();
    manager.remove(manual.id).unwrap();
    manager.release_session(a).unwrap();
    assert_eq!(backend.removed.lock().unwrap().len(), 2);
}

#[test]
fn failed_cleanup_retains_exact_target_for_retry_without_reusing_it() {
    let (manager, backend) = fixture();
    let display = manager.create(Owner::Manual, Spec::default()).unwrap();
    backend.fail_remove.store(true, Ordering::Relaxed);
    assert!(
        matches!(manager.remove(display.id), Err(Error::CleanupPending { id, .. }) if id == display.id)
    );
    let listed = manager.list().unwrap();
    assert!(listed[0].cleanup_pending);
    assert!(listed[0].active);
    assert_eq!(manager.status().unwrap().available_slots, 1);
    backend.fail_remove.store(false, Ordering::Relaxed);
    manager.remove(display.id).unwrap();
    assert!(manager.list().unwrap().is_empty());
}

#[test]
fn readback_failure_rolls_back_only_the_new_target_and_retains_failed_rollback() {
    let (manager, backend) = fixture();
    backend.fail_after_activate.store(true, Ordering::Relaxed);
    assert!(matches!(
        manager.create(Owner::Manual, Spec::default()),
        Err(Error::Platform(_))
    ));
    assert!(manager.list().unwrap().is_empty());
    assert!(backend.targets.lock().unwrap()[0].active);
    backend.fail_remove.store(true, Ordering::Relaxed);
    let id = match manager.create(Owner::Manual, Spec::default()) {
        Err(Error::CleanupPending { id, .. }) => id,
        other => panic!("unexpected result: {other:?}"),
    };
    assert!(manager.list().unwrap()[0].cleanup_pending);
    backend.fail_remove.store(false, Ordering::Relaxed);
    manager.remove(id).unwrap();
}

#[test]
fn monitor_number_reuse_is_not_identity_reuse() {
    let (manager, backend) = fixture();
    let display = manager.create(Owner::Manual, Spec::default()).unwrap();
    // Driver re-enumeration reuses the numeric target, but its full monitor
    // identity has changed. Cleanup must never deactivate the replacement.
    backend.targets.lock().unwrap()[1].key.monitor_path = "external-replacement".into();
    manager.remove(display.id).unwrap();
    assert!(backend.targets.lock().unwrap()[1].active);
}

#[test]
fn guard_cleans_only_its_authenticated_connection() {
    let (manager, _) = fixture();
    let session_id = Uuid::new_v4();
    let manual = manager.create(Owner::Manual, Spec::default()).unwrap();
    {
        let _guard = manager.session_guard(session_id);
        manager
            .create(Owner::Session(session_id), Spec::default())
            .unwrap();
    }
    assert_eq!(manager.list().unwrap(), vec![manual]);
}

#[test]
fn invalid_spec_does_not_enumerate_or_mutate_a_display() {
    let (manager, backend) = fixture();
    assert!(matches!(
        manager.create(
            Owner::Manual,
            Spec {
                width: 0,
                ..Spec::default()
            }
        ),
        Err(Error::InvalidSpec)
    ));
    assert!(backend.activated.lock().unwrap().is_empty());
    assert_eq!(backend.recover_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn pending_recovery_blocks_every_mutation_before_inventory_or_target_reuse() {
    let (manager, backend) = fixture();
    let slot = backend.targets.lock().unwrap()[1].key.slot_id();
    backend.fail_recover.store(true, Ordering::SeqCst);
    assert!(matches!(
        manager.create(Owner::Manual, Spec::default()),
        Err(Error::Platform(_))
    ));
    assert!(matches!(
        manager.prepare_slot(&slot),
        Err(Error::Platform(_))
    ));
    assert!(matches!(manager.prepare_driver(), Err(Error::Platform(_))));
    assert_eq!(backend.recover_calls.load(Ordering::SeqCst), 3);
    assert_eq!(backend.inventory_calls.load(Ordering::SeqCst), 0);
    assert_eq!(backend.provision_calls.load(Ordering::SeqCst), 0);
    assert!(backend.activated.lock().unwrap().is_empty());
    assert!(backend.removed.lock().unwrap().is_empty());
    // Once exact rollback succeeds, the same admission path can reuse capacity.
    backend.fail_recover.store(false, Ordering::SeqCst);
    let display = manager.create(Owner::Manual, Spec::default()).unwrap();
    assert_eq!(display.source, "\\\\.\\DISPLAY2");
    manager.remove(display.id).unwrap();
}

#[test]
fn disconnected_session_cannot_create_a_late_display() {
    let (manager, _) = fixture();
    let id = Uuid::new_v4();
    assert!(matches!(
        manager.create(Owner::Session(id), Spec::default()),
        Err(Error::SessionClosed)
    ));
    let guard = manager.session_guard(id);
    manager.create(Owner::Session(id), Spec::default()).unwrap();
    manager.release_session(id).unwrap();
    assert!(matches!(
        manager.create(Owner::Session(id), Spec::default()),
        Err(Error::SessionClosed)
    ));
    drop(guard);
    assert!(manager.list().unwrap().is_empty());
}

#[test]
fn explicit_slot_preparation_cannot_disconnect_an_active_lease() {
    let (manager, backend) = fixture();
    let lease = manager.create(Owner::Manual, Spec::default()).unwrap();
    let slots = manager.slots().unwrap();
    let leased = slots
        .iter()
        .find(|slot| slot.source == lease.source)
        .unwrap();
    assert!(matches!(
        manager.prepare_slot(&leased.id),
        Err(Error::UnknownSlot)
    ));
    // An explicit local choice is required to disconnect the existing active
    // screen; ordinary create has deliberately skipped it throughout.
    let existing = slots
        .iter()
        .find(|slot| slot.active && !slot.leased)
        .unwrap();
    manager.prepare_slot(&existing.id).unwrap();
    assert!(!backend.targets.lock().unwrap()[0].active);
    assert!(manager.list().unwrap()[0].active);
}

#[test]
fn embedded_driver_package_only_pins_the_official_signed_x64_payload() {
    let package = driver_package();
    assert_eq!(package.release, "25.7.23");
    assert_eq!(package.architecture, "x86_64");
    assert!(package.url.starts_with(
        "https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/download/"
    ));
    assert_eq!(package.zip_sha256.len(), 64);
    assert_eq!(package.files.len(), 4);
    for file in &package.files {
        assert!(!file.name.contains(['/', '\\']));
        assert_eq!(file.sha256.len(), 64);
        assert!(hex::decode(&file.sha256).is_ok());
        assert!(file.bytes > 0 && file.bytes < 1024 * 1024);
    }
}

#[test]
fn manual_shutdown_keeps_other_sessions_and_failed_cleanup_pending() {
    let (manager, backend) = fixture();
    let id = Uuid::new_v4();
    let _guard = manager.session_guard(id);
    let manual = manager.create(Owner::Manual, Spec::default()).unwrap();
    let session = manager.create(Owner::Session(id), Spec::default()).unwrap();
    backend.fail_remove.store(true, Ordering::Relaxed);
    assert!(matches!(
        manager.release_manual(),
        Err(Error::CleanupPending { .. })
    ));
    backend.fail_remove.store(false, Ordering::Relaxed);
    manager.release_manual().unwrap();
    manager.release_manual().unwrap();
    assert_eq!(manager.list().unwrap(), vec![session]);
    assert!(backend
        .removed
        .lock()
        .unwrap()
        .iter()
        .all(|key| key.target != 3));
    manager.remove(manual.id).unwrap();
}

#[test]
fn elevated_helper_rejects_modified_input_before_handing_paths_to_windows() {
    let directory =
        std::env::temp_dir().join(format!("eclipse-vdd-package-test-{}", Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("mttvdd.cat"), b"modified catalog").unwrap();
    let error = read_pinned_package(&directory).unwrap_err().to_string();
    assert!(error.contains("does not match the pinned official release"));
    std::fs::remove_file(directory.join("mttvdd.cat")).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

#[test]
fn final_manual_shutdown_retires_late_creates_and_keeps_cleanup_retryable() {
    let (manager, backend) = fixture();
    manager.create(Owner::Manual, Spec::default()).unwrap();
    backend.fail_remove.store(true, Ordering::Relaxed);
    assert!(manager.shutdown_manual().is_err());
    assert!(matches!(
        manager.create(Owner::Manual, Spec::default()),
        Err(Error::HostClosing)
    ));
    backend.fail_remove.store(false, Ordering::Relaxed);
    manager.shutdown_manual().unwrap();
    assert!(manager.list().unwrap().is_empty());
    assert!(matches!(
        manager.create(Owner::Manual, Spec::default()),
        Err(Error::HostClosing)
    ));
}

#[test]
fn explicit_preparation_cannot_blank_the_last_output_or_run_after_shutdown() {
    let (manager, backend) = fixture();
    let slot = manager
        .slots()
        .unwrap()
        .into_iter()
        .find(|slot| slot.active)
        .unwrap();
    assert!(matches!(
        manager.prepare_slot(&slot.id),
        Err(Error::LastActiveOutput)
    ));
    assert!(backend.removed.lock().unwrap().is_empty());
    manager.shutdown_manual().unwrap();
    assert!(matches!(
        manager.prepare_slot(&slot.id),
        Err(Error::HostClosing)
    ));
    assert!(matches!(manager.prepare_driver(), Err(Error::HostClosing)));
}

fn another_manager(backend: &Arc<Fake>) -> Arc<Manager> {
    Arc::new(Manager {
        backend: Box::new(backend.clone()),
        entries: Mutex::new(BTreeMap::new()),
        sessions: Mutex::new(BTreeMap::new()),
        manual_closing: AtomicBool::new(false),
    })
}
#[test]
fn independent_managers_keep_claims_while_an_owned_output_is_temporarily_inactive() {
    let (first, backend) = fixture();
    backend
        .targets
        .lock()
        .unwrap()
        .retain(|target| target.key.target <= 2);
    let second = another_manager(&backend);
    let display = first.create(Owner::Manual, Spec::default()).unwrap();
    let key = first
        .lock()
        .unwrap()
        .get(&display.id)
        .unwrap()
        .target
        .clone();
    let slot = key.slot_id();
    assert!(
        second
            .slots()
            .unwrap()
            .iter()
            .find(|value| value.id == slot)
            .unwrap()
            .leased
    );
    assert!(matches!(second.prepare_slot(&slot), Err(Error::Claimed)));
    assert!(backend.removed.lock().unwrap().is_empty());
    // The user can disable an output in Windows without ending its Eclipse
    // lease. Another process must not adopt that same exact target meanwhile.
    backend
        .targets
        .lock()
        .unwrap()
        .iter_mut()
        .find(|target| target.key == key)
        .unwrap()
        .active = false;
    assert_eq!(second.status().unwrap().available_slots, 0);
    assert!(matches!(
        second.create(Owner::Manual, Spec::default()),
        Err(Error::NoAvailableSlot)
    ));
    assert!(matches!(second.prepare_slot(&slot), Err(Error::Claimed)));
    first.remove(display.id).unwrap();
    assert!(!backend.is_claimed(&key).unwrap());
    let replacement = second.create(Owner::Manual, Spec::default()).unwrap();
    first.remove(display.id).unwrap(); // A stale remove is idempotent, never B's lease.
    assert!(second.list().unwrap()[0].active);
    second.remove(replacement.id).unwrap();
}
#[test]
fn failed_rollback_retains_the_cross_process_claim_until_cleanup_really_succeeds() {
    let (first, backend) = fixture();
    backend
        .targets
        .lock()
        .unwrap()
        .retain(|target| target.key.target <= 2);
    let second = another_manager(&backend);
    backend.fail_after_activate.store(true, Ordering::SeqCst);
    backend.fail_remove.store(true, Ordering::SeqCst);
    let id = match first.create(Owner::Manual, Spec::default()) {
        Err(Error::CleanupPending { id, .. }) => id,
        value => panic!("unexpected activation: {value:?}"),
    };
    let key = first.lock().unwrap().get(&id).unwrap().target.clone();
    assert!(backend.is_claimed(&key).unwrap());
    backend
        .targets
        .lock()
        .unwrap()
        .iter_mut()
        .find(|target| target.key == key)
        .unwrap()
        .active = false;
    assert_eq!(second.status().unwrap().available_slots, 0);
    assert!(matches!(
        second.prepare_slot(&key.slot_id()),
        Err(Error::Claimed)
    ));
    backend.fail_remove.store(false, Ordering::SeqCst);
    first.remove(id).unwrap();
    assert!(!backend.is_claimed(&key).unwrap());
}
#[test]
fn two_managers_racing_for_one_target_cannot_both_activate_it() {
    let (first, backend) = fixture();
    backend
        .targets
        .lock()
        .unwrap()
        .retain(|target| target.key.target <= 2);
    let second = another_manager(&backend);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = [first.clone(), second.clone()]
        .into_iter()
        .map(|manager| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                manager.create(Owner::Manual, Spec::default())
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(backend.activated.lock().unwrap().len(), 1);
    first.release_manual().unwrap();
    second.release_manual().unwrap();
    assert!(backend.claims.lock().unwrap().is_empty());
}
#[test]
fn missing_capacity_provisions_once_then_claims_only_a_fresh_inactive_target() {
    let (manager, backend) = fixture();
    backend
        .targets
        .lock()
        .unwrap()
        .retain(|target| target.active);
    let target = Target {
        key: TargetKey {
            adapter: (9, 10),
            target: 7,
            monitor_path: "new-owned-instance-monitor".into(),
        },
        name: "fresh".into(),
        source: "generated-fresh-source".into(),
        active: false,
        available: true,
        actual: None,
    };
    *backend.provisioned.lock().unwrap() = Some(target.clone());
    let display = manager.create(Owner::Manual, Spec::default()).unwrap();
    assert_eq!(display.source, target.source);
    assert_eq!(backend.provision_calls.load(Ordering::SeqCst), 1);
    assert_eq!(*backend.activated.lock().unwrap(), vec![target.key]);
    assert!(backend.targets.lock().unwrap()[0].active);
    manager.remove(display.id).unwrap();
}
#[test]
fn provisioning_that_does_not_publish_inactive_capacity_is_not_retried_or_adopted() {
    let (manager, backend) = fixture();
    backend
        .targets
        .lock()
        .unwrap()
        .retain(|target| target.active);
    let target = Target {
        key: TargetKey {
            adapter: (9, 10),
            target: 7,
            monitor_path: "new-active-target".into(),
        },
        name: "external".into(),
        source: "generated-other-source".into(),
        active: true,
        available: true,
        actual: None,
    };
    *backend.provisioned.lock().unwrap() = Some(target);
    assert!(matches!(
        manager.create(Owner::Manual, Spec::default()),
        Err(Error::NoAvailableSlot)
    ));
    assert_eq!(backend.provision_calls.load(Ordering::SeqCst), 1);
    assert!(backend.activated.lock().unwrap().is_empty());
    assert!(backend.removed.lock().unwrap().is_empty());
}
