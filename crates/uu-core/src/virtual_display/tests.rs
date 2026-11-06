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
    fail_validate: AtomicBool,
    fail_apply: AtomicBool,
    write_before_apply_error: AtomicBool,
    fail_reconfigure_once: AtomicBool,
    write_before_reconfigure_error: AtomicBool,
    activated: Mutex<Vec<TargetKey>>,
    reconfigured: Mutex<Vec<(TargetKey,Spec)>>,
    removed: Mutex<Vec<TargetKey>>,
    claims: Mutex<BTreeSet<TargetKey>>,
    provision_calls: AtomicUsize,
    provisioned: Mutex<Option<Target>>,
    capacity: Mutex<Option<preparation::Observation>>,
    capacity_downloads: AtomicUsize,
    capacity_changes: AtomicUsize,
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
    fn recover_capacity(&self,checkpoint:&(dyn Fn()->preparation::Result<()>+Sync))->preparation::Result<()> {
        checkpoint()?;
        self.recover().map_err(|e|preparation::Failure::pending(e.to_string()))?;
        if let Some(state)=self.capacity.lock().unwrap().as_mut(){state.recovery_pending=false;}
        Ok(())
    }
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
        let targets=self.targets.lock().unwrap().clone();
        Ok(Inventory {
            supported: true,
            driver_present: !targets.is_empty() || self.capacity.lock().unwrap().as_ref().is_none_or(|state|state.driver_present),
            targets,
        })
    }
    fn preparation_observation(&self) -> Result<Option<preparation::Observation>> {
        let Some(mut observed) = *self.capacity.lock().unwrap() else {
            return Ok(None);
        };
        let targets = self.targets.lock().unwrap();
        observed.targets = targets.len() as u32;
        observed.available = targets
            .iter()
            .filter(|target| target.available && !target.active)
            .count() as u32;
        observed.active_driver_targets =
            targets.iter().filter(|target| target.active).count() as u32;
        observed.claimed_targets = self.claims.lock().unwrap().len() as u32;
        Ok(Some(observed))
    }
    fn preparation_authorized(&self) -> bool {
        self.capacity
            .lock()
            .unwrap()
            .is_some_and(|state| state.authority)
    }
    fn download_capacity_driver(&self) -> preparation::Result<Vec<(String, Vec<u8>)>> {
        self.capacity_downloads.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }
    fn prepare_capacity(
        &self,
        minimum: u32,
        package: Option<&[(String, Vec<u8>)]>,
        check: &(dyn Fn() -> preparation::Result<()> + Sync),
    ) -> preparation::Result<()> {
        check()?;
        self.capacity_changes.fetch_add(1, Ordering::SeqCst);
        let mut state = self.capacity.lock().unwrap();
        let state = state.as_mut().unwrap();
        if !state.driver_present {
            assert!(package.is_some());
        }
        let mut targets = self.targets.lock().unwrap();
        assert!(targets.iter().all(|target| !target.active));
        for id in targets.len() as u32 + 1..=minimum {
            targets.push(capacity_target(id));
        }
        state.driver_present = true;
        state.configured_capacity = Some(minimum);
        Ok(())
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
        let mut apply_submitted = false;
        let result = config_apply::run(&mut apply_submitted, |phase| {
            match phase {
                config_apply::Phase::Validate => {
                    if self.fail_validate.load(Ordering::Relaxed) {
                        return Err("validation rejected");
                    }
                }
                config_apply::Phase::Apply => {
                    let fail = self.fail_apply.load(Ordering::Relaxed);
                    if !fail || self.write_before_apply_error.load(Ordering::Relaxed) {
                        target.active = true;
                        target.actual = Some(spec);
                        self.activated.lock().unwrap().push(key.clone());
                    }
                    if fail {
                        return Err("driver error after APPLY submission");
                    }
                }
            }
            Ok(())
        });
        if let Err(message) = result {
            return Err(ActivationFailure {
                message: message.into(),
                activated: apply_submitted,
            });
        }
        if self.fail_after_activate.load(Ordering::Relaxed) {
            return Err(ActivationFailure {
                message: "mode readback mismatch".into(),
                activated: true,
            });
        }
        Ok(target.clone())
    }
    fn reconfigure(&self,key:&TargetKey,spec:Spec)->std::result::Result<Target,ActivationFailure> {
        let mut targets=self.targets.lock().unwrap();
        let Some(target)=targets.iter_mut().find(|target|&target.key==key&&target.active) else {
            return Err(ActivationFailure{message:"target is not active".into(),activated:false});
        };
        let mut apply_submitted=false;
        let result=config_apply::run(&mut apply_submitted,|phase|{
            match phase {
                config_apply::Phase::Validate=>if self.fail_validate.load(Ordering::Relaxed){return Err("validation rejected");},
                config_apply::Phase::Apply=>{
                    let fail=self.fail_reconfigure_once.swap(false,Ordering::Relaxed);
                    if !fail||self.write_before_reconfigure_error.load(Ordering::Relaxed){target.actual=Some(spec);self.reconfigured.lock().unwrap().push((key.clone(),spec));}
                    if fail{return Err("driver error after reconfigure APPLY submission");}
                }
            }
            Ok(())
        });
        if let Err(message)=result{return Err(ActivationFailure{message:message.into(),activated:apply_submitted});}
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
            preparation: preparation::Coordinator::default(),
            preparation_owner: AtomicBool::new(true),
            editing_mode: AtomicBool::new(false),
            hosts: Mutex::default(),
            policy:Mutex::default(),
            limit:std::sync::atomic::AtomicU16::new(DEFAULT_DISPLAY_LIMIT),
        }),
        fake,
    )
}

#[test]
fn manual_and_two_sessions_never_take_active_targets_or_each_others_leases() {
    let (manager, backend) = fixture();
    // This test exercises driver exhaustion and ownership independently of policy.
    manager.set_limit(3).unwrap();
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
fn rejected_validation_releases_claim_without_authorizing_target_cleanup() {
    let (manager, backend) = fixture();
    backend.fail_validate.store(true, Ordering::Relaxed);
    assert!(matches!(
        manager.create(Owner::Manual, Spec::default()),
        Err(Error::Platform(_))
    ));
    assert!(backend.activated.lock().unwrap().is_empty());
    assert!(backend.removed.lock().unwrap().is_empty());
    assert!(backend.claims.lock().unwrap().is_empty());
    assert!(manager.list().unwrap().is_empty());
    assert!(backend.targets.lock().unwrap()[0].active);
}

#[test]
fn apply_error_cleans_only_claimed_target_even_without_visible_driver_changes() {
    for driver_changed_target in [false, true] {
        let (manager, backend) = fixture();
        let target = backend.targets.lock().unwrap()[1].key.clone();
        backend.fail_apply.store(true, Ordering::Relaxed);
        backend
            .write_before_apply_error
            .store(driver_changed_target, Ordering::Relaxed);
        assert!(matches!(
            manager.create(Owner::Manual, Spec::default()),
            Err(Error::Platform(_))
        ));
        assert_eq!(*backend.removed.lock().unwrap(), [target]);
        assert!(backend.claims.lock().unwrap().is_empty());
        assert!(manager.list().unwrap().is_empty());
        assert!(backend.targets.lock().unwrap()[0].active);
    }
}

#[test]
fn apply_error_with_unproven_cleanup_retains_claim_until_exact_target_retires() {
    for driver_changed_target in [false, true] {
        let (manager, backend) = fixture();
        backend.targets.lock().unwrap().truncate(2);
        let key = backend.targets.lock().unwrap()[1].key.clone();
        let second = another_manager(&backend);
        backend.fail_apply.store(true, Ordering::Relaxed);
        backend.fail_remove.store(true, Ordering::Relaxed);
        backend
            .write_before_apply_error
            .store(driver_changed_target, Ordering::Relaxed);
        let id = match manager.create(Owner::Manual, Spec::default()) {
            Err(Error::CleanupPending { id, .. }) => id,
            result => panic!("unexpected activation result: {result:?}"),
        };
        assert!(manager.list().unwrap()[0].cleanup_pending);
        assert!(backend.is_claimed(&key).unwrap());
        assert_eq!(second.status().unwrap().available_slots, 0);
        assert!(matches!(
            second.create(Owner::Manual, Spec::default()),
            Err(Error::NoAvailableSlot)
        ));
        backend.fail_remove.store(false, Ordering::Relaxed);
        manager.remove(id).unwrap();
        assert!(!backend.is_claimed(&key).unwrap());
        assert!(manager.list().unwrap().is_empty());
        assert!(backend.targets.lock().unwrap()[0].active);
    }
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
        preparation: preparation::Coordinator::default(),
        preparation_owner: AtomicBool::new(true),
            editing_mode: AtomicBool::new(false),
        hosts: Mutex::default(),
        policy:Mutex::default(),
            limit:std::sync::atomic::AtomicU16::new(DEFAULT_DISPLAY_LIMIT),
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

fn capacity_target(id: u32) -> Target {
    Target {
        key: TargetKey {
            adapter: (17, 18),
            target: id,
            monitor_path: format!("prepared-monitor-{id}"),
        },
        name: "prepared virtual screen".into(),
        source: format!("prepared-source-{id}"),
        active: false,
        available: true,
        actual: None,
    }
}
fn capacity_fixture(count: u32) -> (Arc<Manager>, Arc<Fake>) {
    let (manager, backend) = fixture();
    *backend.targets.lock().unwrap() = (1..=count).map(capacity_target).collect();
    *backend.capacity.lock().unwrap() = Some(preparation::Observation {
        supported: true,
        driver_present: count != 0,
        authority: true,
        configured_capacity: (count != 0).then_some(count),
        ..Default::default()
    });
    (manager, backend)
}
#[test]
fn passive_catalog_reads_skip_an_owned_display_mutation_without_observing_its_pool() {
    let (manager,_)=capacity_fixture(0);
    let guard=manager.lock().unwrap();
    assert!(manager.try_read_display_configuration(||panic!("an intermediate pool must not be enumerated")).is_none());
    drop(guard);
    assert_eq!(manager.try_read_display_configuration(||3),Some(3));
}
#[test]
fn common_create_prepares_missing_driver_once_before_allocating_distinct_leases() {
    let (manager, backend) = capacity_fixture(0);
    // Inspect alone cannot install or accept preparation intent.
    assert_eq!(
        manager.status().unwrap().preparation.unwrap().phase,
        preparation::Phase::NeedsDriver
    );
    assert!(!manager.has_queued_preparation());
    let a = manager.create(Owner::Manual, Spec::default()).unwrap();
    let b = manager.create(Owner::Manual, Spec::default()).unwrap();
    assert_ne!(a.id, b.id);
    assert_ne!(a.source, b.source);
    assert_eq!(backend.capacity_downloads.load(Ordering::SeqCst), 1);
    assert_eq!(backend.capacity_changes.load(Ordering::SeqCst), 1);
    assert_eq!(manager.status().unwrap().available_slots, 0);
    assert_eq!(backend.targets.lock().unwrap().len(), 2, "default limit prepares two targets, never a hidden batch of sixteen");
    manager.remove(a.id).unwrap();
    manager.remove(b.id).unwrap();
}
#[test]
fn plus_can_install_a_missing_driver_and_lease_a_screen_while_another_session_streams() {
    let (manager,backend)=capacity_fixture(0);
    backend.capacity.lock().unwrap().as_mut().unwrap().live_install=true;
    let gate=Arc::new(crate::shared_host::SessionGate::default());manager.register_host(&gate);
    let physical=gate.claim("generated-physical").unwrap();
    physical.progress(crate::native::HostStatus::Streaming{width:1920,height:1080,fps:60,audio:"off".into(),encoder:"fake".into()});
    let pending=gate.claim("generated-virtual-tab").unwrap();pending.progress(crate::native::HostStatus::Preparing);
    let owner=Uuid::new_v4();
    let guard=manager.session_guard(owner);
    let created=manager.create_with_context(Owner::Session(owner),Spec::default(),Some(pending.id())).unwrap();
    assert!(created.active);
    assert_eq!(gate.sessions().len(),2);
    assert!(!*physical.stopped().borrow());
    assert_eq!(backend.capacity_changes.load(Ordering::SeqCst),1);
    assert_eq!(backend.targets.lock().unwrap().len(),2);
    // The second slot is consumed without reinstalling or ending either stream.
    let second=manager.create(Owner::Manual,Spec::default()).unwrap();
    assert_eq!(backend.capacity_changes.load(Ordering::SeqCst),1);
    drop(guard);
    assert!(manager.list().unwrap().iter().any(|d|d.id==second.id && d.active));
    assert!(!*physical.stopped().borrow());
    manager.remove(second.id).unwrap();
}
#[test]
fn existing_slot_reuse_does_not_expand_until_a_second_create_requests_capacity() {
    let (manager, backend) = capacity_fixture(1);
    let gate = Arc::new(crate::shared_host::SessionGate::default());
    manager.register_host(&gate);
    let physical = gate.claim("peer").unwrap();
    physical.progress(crate::native::HostStatus::Streaming {
        width: 1920,
        height: 1080,
        fps: 60,
        audio: "off".into(),
        encoder: "fake".into(),
    });
    let a = manager.create(Owner::Manual, Spec::default()).unwrap();
    assert_eq!(backend.capacity_changes.load(Ordering::SeqCst), 0);
    assert!(!manager.has_queued_preparation());
    let error = manager.create(Owner::Manual, Spec::default()).unwrap_err();
    assert!(matches!(
        error,
        Error::Preparation(preparation::Status {
            phase: preparation::Phase::WaitingForIdle,
            ..
        })
    ));
    manager.remove(a.id).unwrap();
    assert_eq!(
        manager.resume_preparation().unwrap().unwrap().phase,
        preparation::Phase::WaitingForIdle
    );
    drop(physical);
    assert_eq!(
        manager.resume_preparation().unwrap().unwrap().phase,
        preparation::Phase::Ready
    );
    assert_eq!(backend.capacity_changes.load(Ordering::SeqCst), 1);
    assert_eq!(backend.capacity_downloads.load(Ordering::SeqCst), 0);
    assert_eq!(manager.status().unwrap().available_slots, 2);
}
#[test]
fn initial_preparation_exempts_only_its_own_authenticated_pending_session() {
    let (manager, backend) = capacity_fixture(0);
    let gate = Arc::new(crate::shared_host::SessionGate::default());
    manager.register_host(&gate);
    let connection = gate.claim("peer").unwrap();
    connection.progress(crate::native::HostStatus::Preparing);
    assert!(matches!(
        manager.create_with_context(Owner::Manual, Spec::default(), Some("other-session")),
        Err(Error::Preparation(preparation::Status {
            phase: preparation::Phase::WaitingForIdle,
            ..
        }))
    ));
    assert_eq!(backend.capacity_changes.load(Ordering::SeqCst), 0);
    let display = manager
        .create_with_context(Owner::Manual, Spec::default(), Some(connection.id()))
        .unwrap();
    assert_eq!(backend.capacity_changes.load(Ordering::SeqCst), 1);
    manager.remove(display.id).unwrap();
}
#[test]
fn lowering_the_limit_retargets_or_cancels_a_waiting_preparation_without_installing_an_old_batch() {
    for limit in [0,1] {
        let (manager,backend)=capacity_fixture(0);
        let gate=Arc::new(crate::shared_host::SessionGate::default());manager.register_host(&gate);
        let stream=gate.claim("generated-physical-stream").unwrap();
        manager.set_limit(16).unwrap();
        assert!(matches!(manager.create(Owner::Manual,Spec::default()),Err(Error::Preparation(_))));
        assert!(manager.has_queued_preparation());
        manager.set_limit(limit).unwrap();drop(stream);
        manager.resume_preparation().unwrap();
        assert_eq!(backend.targets.lock().unwrap().len(),usize::from(limit));
        assert_eq!(backend.capacity_changes.load(Ordering::SeqCst),usize::from(limit));
        assert!(!manager.has_queued_preparation());
    }
}
#[test]
fn status_and_unrequested_worker_ticks_never_install_or_expand_a_driver() {
    for count in [0,1,2,16] {
        let (manager,backend)=capacity_fixture(count);
        manager.status().unwrap();manager.preparation_status().unwrap();
        manager.resume_preparation().unwrap();
        assert!(!manager.has_queued_preparation());
        assert_eq!(backend.capacity_downloads.load(Ordering::SeqCst),0);
        assert_eq!(backend.capacity_changes.load(Ordering::SeqCst),0);
        assert_eq!(backend.targets.lock().unwrap().len(),count as usize);
    }
}
#[test]
fn old_virtual_status_without_preparation_remains_readable() {
    let value = serde_json::json!({"supported":true,"driver_present":true,"available_slots":1,"leased":0,"explanation":"ready","install_url":"https://example.invalid"});
    let status: Status = serde_json::from_value(value).unwrap();
    assert!(status.preparation.is_none());
}

#[test]
fn consuming_an_existing_other_driver_slot_does_not_queue_an_unneeded_mtt_install() {
    let (manager, backend) = capacity_fixture(0);
    backend.targets.lock().unwrap().push(capacity_target(1));
    let display = manager.create(Owner::Manual, Spec::default()).unwrap();
    assert!(!manager.has_queued_preparation());
    assert_eq!(backend.capacity_downloads.load(Ordering::SeqCst), 0);
    manager.remove(display.id).unwrap();
}

#[test]
fn a_foreground_helper_can_reuse_a_slot_but_cannot_reload_an_existing_driver() {
    let (manager, backend) = capacity_fixture(1);
    manager.preparation_owner.store(false, Ordering::Release);
    let display = manager.create(Owner::Manual, Spec::default()).unwrap();
    assert!(!manager.has_queued_preparation());
    assert!(matches!(
        manager.create(Owner::Manual, Spec::default()),
        Err(Error::Preparation(preparation::Status {
            phase: preparation::Phase::NeedsElevation,
            ..
        }))
    ));
    manager.remove(display.id).unwrap();
    assert_eq!(backend.capacity_changes.load(Ordering::SeqCst), 0);
    assert_eq!(
        manager.resume_preparation().unwrap().unwrap().phase,
        preparation::Phase::NeedsElevation
    );
    assert_eq!(backend.capacity_changes.load(Ordering::SeqCst), 0);
}

#[test]
fn durable_preparation_pending_blocks_create_before_any_recovery_or_slot_reuse() {
    let (manager, backend) = capacity_fixture(1);
    backend
        .capacity
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .recovery_pending = true;
    assert!(matches!(
        manager.create(Owner::Manual, Spec::default()),
        Err(Error::Preparation(preparation::Status {
            phase: preparation::Phase::RecoveryPending,
            ..
        }))
    ));
    assert_eq!(backend.recover_calls.load(Ordering::SeqCst), 0);
    assert_eq!(backend.capacity_changes.load(Ordering::SeqCst), 0);
    assert!(backend.claims.lock().unwrap().is_empty());
}

#[test]
fn explicit_recovery_waits_for_streams_then_checks_once_without_reinstalling() {
    for fail in [false,true] {
        let (manager,backend)=capacity_fixture(0);
        backend.capacity.lock().unwrap().as_mut().unwrap().recovery_pending=true;
        backend.fail_recover.store(fail,Ordering::SeqCst);
        let gate=Arc::new(crate::shared_host::SessionGate::default());manager.register_host(&gate);
        let active=gate.claim("generated-streaming-peer").unwrap();
        let queued=manager.recover_preparation().unwrap().unwrap();
        assert_eq!(queued.phase,preparation::Phase::RecoveryPending);
        assert!(queued.queued && manager.has_queued_preparation());
        assert!(manager.preparation_status().unwrap().unwrap().queued);
        manager.resume_preparation().unwrap();
        assert_eq!(backend.recover_calls.load(Ordering::SeqCst),0,"streaming must never be interrupted for recovery");
        drop(active);
        let finished=manager.resume_preparation().unwrap().unwrap();
        assert_eq!(finished.phase,if fail {preparation::Phase::RecoveryPending}else{preparation::Phase::NeedsDriver});
        assert_eq!(finished.error.is_some(),fail);
        assert!(!finished.queued && !manager.has_queued_preparation());
        manager.resume_preparation().unwrap();
        assert_eq!(backend.recover_calls.load(Ordering::SeqCst),1,"a failed recovery needs a new explicit intent");
        assert_eq!(backend.capacity_downloads.load(Ordering::SeqCst),0);
        assert_eq!(backend.capacity_changes.load(Ordering::SeqCst),0,"recovery never repeats driver installation");
    }
}

#[test]
fn create_requires_separate_explicit_repair_before_installing_after_external_uninstall() {
    let (manager,backend)=capacity_fixture(0);
    {let mut capacity=backend.capacity.lock().unwrap();let state=capacity.as_mut().unwrap();state.driver_present=false;state.recovery_pending=true;}
    assert_eq!(manager.preparation_status().unwrap().unwrap().phase,preparation::Phase::RecoveryPending);
    assert_eq!(backend.recover_calls.load(Ordering::SeqCst),0,"status must not recover");
    assert!(matches!(manager.create(Owner::Manual,Spec::default()),Err(Error::Preparation(_))));
    assert_eq!(backend.recover_calls.load(Ordering::SeqCst),0,"create must not silently repair");
    assert_eq!(backend.capacity_downloads.load(Ordering::SeqCst),0);
    let repaired=manager.recover_preparation().unwrap().unwrap();
    assert_eq!(repaired.phase,preparation::Phase::NeedsDriver);
    assert_eq!(backend.capacity_changes.load(Ordering::SeqCst),0,"repair must not install");
    let display=manager.create(Owner::Manual,Spec::default()).unwrap();
    assert!(display.active);
    assert_eq!(backend.capacity_downloads.load(Ordering::SeqCst),1);
    assert_eq!(backend.capacity_changes.load(Ordering::SeqCst),1);
    assert_eq!(backend.targets.lock().unwrap().len(),2);
    assert!(!backend.capacity.lock().unwrap().unwrap().recovery_pending);
}

#[test]
fn removed_driver_recovery_failure_never_downloads_or_creates_a_replacement() {
    let (manager,backend)=capacity_fixture(0);
    {let mut capacity=backend.capacity.lock().unwrap();let state=capacity.as_mut().unwrap();state.driver_present=false;state.recovery_pending=true;}
    backend.fail_recover.store(true,Ordering::SeqCst);
    assert!(matches!(manager.create(Owner::Manual,Spec::default()),Err(Error::Preparation(_))));
    assert_eq!(backend.capacity_downloads.load(Ordering::SeqCst),0);
    assert_eq!(backend.capacity_changes.load(Ordering::SeqCst),0);
    assert!(backend.claims.lock().unwrap().is_empty());
}

fn saved_policy(manager:&Manager)->std::path::PathBuf {
    let path=std::env::temp_dir().join(format!("eclipse-saved-displays-{}",Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    manager.configure_saved_displays(&path).unwrap();path
}

#[test]
fn retained_session_becomes_durable_manual_ownership_and_restarts_without_duplicate_activation() {
    let (first,backend)=fixture();let path=saved_policy(&first);
    first.set_keep_after_stream(true).unwrap();
    let session=Uuid::new_v4();let guard=first.session_guard(session);
    let created=first.create(Owner::Session(session),Spec::default()).unwrap();
    first.release_session(session).unwrap();drop(guard);
    let retained=first.list().unwrap();assert_eq!(retained.len(),1);
    assert_eq!(retained[0].owner,Owner::Manual);assert!(retained[0].persistent&&retained[0].active);
    assert_eq!(backend.activated.lock().unwrap().len(),1);
    first.shutdown_manual().unwrap();
    let after=first.list().unwrap();assert_eq!(after.len(),1);assert_eq!(after[0].id,created.id);assert!(!after[0].active&&after[0].persistent);
    assert!(backend.removed.lock().unwrap().is_empty(),"saved output outlives process ownership");
    let second=another_manager(&backend);second.configure_saved_displays(&path).unwrap();
    assert!(second.keep_after_stream().unwrap());second.restore_saved_displays().unwrap();
    assert_eq!(second.list().unwrap()[0].id,created.id);
    assert_eq!(backend.activated.lock().unwrap().len(),1,"restoring a live recorded output is not another activation");
    let third=another_manager(&backend);third.configure_saved_displays(&path).unwrap();
    assert!(matches!(third.restore_saved_displays(),Err(Error::Claimed)));
    let pending=third.list().unwrap();assert_eq!(pending.len(),1);assert_eq!(pending[0].id,created.id);assert!(!pending[0].active);
    assert!(third.saved_restore_pending(),"known claim contention preserves the startup intent");
    second.shutdown_manual().unwrap();third.restore_saved_displays().unwrap();
    assert_eq!(third.list().unwrap()[0].id,created.id);
    third.remove(created.id).unwrap();
    let fourth=another_manager(&backend);fourth.configure_saved_displays(&path).unwrap();fourth.restore_saved_displays().unwrap();
    assert!(fourth.list().unwrap().is_empty(),"explicit removal also removes boot intent");
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn saved_manual_display_recovers_after_boot_luid_change_and_can_be_made_temporary() {
    let (first,backend)=fixture();let path=saved_policy(&first);
    let display=first.create_manual(Spec::default(),true).unwrap();
    let key=first.lock().unwrap()[&display.id].target.clone();
    first.shutdown_manual().unwrap();
    {let mut targets=backend.targets.lock().unwrap();let target=targets.iter_mut().find(|t|t.key==key).unwrap();target.active=false;target.actual=None;target.key.adapter.0+=100;}
    let second=another_manager(&backend);second.configure_saved_displays(&path).unwrap();
    second.restore_saved_displays().unwrap();assert_eq!(second.list().unwrap().len(),1);
    assert!(second.list().unwrap()[0].persistent);
    assert_eq!(backend.activated.lock().unwrap().len(),2);
    second.set_persistent(display.id,false).unwrap();
    second.shutdown_manual().unwrap();assert_eq!(backend.removed.lock().unwrap().len(),1);
    let third=another_manager(&backend);third.configure_saved_displays(&path).unwrap();third.restore_saved_displays().unwrap();
    assert!(third.list().unwrap().is_empty());std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn close_and_reset_touch_only_owned_outputs_and_remove_saved_creation_intent() {
    let (manager,backend)=fixture();let path=saved_policy(&manager);
    manager.create_manual(Spec::default(),true).unwrap();
    manager.create_manual(Spec::default(),false).unwrap();
    manager.set_keep_after_stream(true).unwrap();
    manager.reset_displays().unwrap();
    assert!(!manager.keep_after_stream().unwrap());
    assert!(manager.list().unwrap().is_empty());
    assert!(backend.targets.lock().unwrap()[0].active,"foreign original display is preserved");
    let next=another_manager(&backend);next.configure_saved_displays(&path).unwrap();next.restore_saved_displays().unwrap();
    assert!(next.list().unwrap().is_empty());std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn damaged_saved_configuration_never_changes_targets_or_overwrites_evidence() {
    let (manager,backend)=fixture();let path=saved_policy(&manager);
    let file=path.join("virtual-displays.json");let damaged=b"{not a saved policy}";
    std::fs::write(&file,damaged).unwrap();
    assert!(manager.configure_saved_displays(&path).is_err());
    assert_eq!(std::fs::read(file).unwrap(),damaged);
    assert!(backend.activated.lock().unwrap().is_empty());std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn host_limit_counts_manual_and_session_allocations_without_changing_existing_targets() {
    let (manager,backend)=fixture();
    let owner=Uuid::new_v4();let _guard=manager.session_guard(owner);
    assert_eq!(manager.status().unwrap().limit,2);
    let manual=manager.create(Owner::Manual,Spec::default()).unwrap();
    let session=manager.create_for_session(owner,Spec::default(),false,None).unwrap();
    let before=backend.inventory_calls.load(Ordering::SeqCst);
    assert!(matches!(manager.create(Owner::Manual,Spec::default()),Err(Error::LimitReached{limit:2})));
    assert_eq!(before,backend.inventory_calls.load(Ordering::SeqCst),"quota rejection precedes driver work");
    manager.set_limit(0).unwrap();
    assert_eq!(manager.list().unwrap().len(),2,"reducing the limit preserves existing outputs");
    assert!(matches!(manager.create(Owner::Manual,Spec::default()),Err(Error::LimitReached{limit:0})));
    manager.remove(manual.id).unwrap();manager.set_limit(2).unwrap();
    manager.create(Owner::Manual,Spec::default()).unwrap();
    manager.remove_session(owner,session.id).unwrap();
    manager.close_all().unwrap();
}

#[test]
fn remote_limit_change_persists_through_the_service_book() {
    let (manager,_backend)=fixture();
    let path=saved_policy(&manager);
    manager.set_limit_persisted(4).unwrap();
    let book=crate::device_book::Store::open(&path).unwrap().read().unwrap();
    assert_eq!(book.sharing.virtual_display_limit,4);
    let (next,_backend)=fixture();
    next.configure_saved_displays(&path).unwrap();
    assert_eq!(next.status().unwrap().limit,4);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn explicit_temporary_and_persistent_creation_have_distinct_disconnect_lifetimes() {
    let (manager,backend)=fixture();let path=saved_policy(&manager);
    manager.set_keep_after_stream(true).unwrap();
    let owner=Uuid::new_v4();let _guard=manager.session_guard(owner);
    let temporary=manager.create_for_session(owner,Spec::default(),false,None).unwrap();
    manager.release_session(owner).unwrap();
    assert!(!manager.list().unwrap().iter().any(|d|d.id==temporary.id),"explicit temporary bypasses the legacy keep policy");
    let owner=Uuid::new_v4();let _guard=manager.session_guard(owner);
    let durable=manager.create_for_session(owner,Spec::default(),true,None).unwrap();
    assert!(durable.persistent);
    manager.release_session(owner).unwrap();
    let displays=manager.list().unwrap();assert_eq!(displays.len(),1);
    assert_eq!(displays[0].owner,Owner::Manual);assert_eq!(displays[0].id,durable.id);
    assert_eq!(backend.removed.lock().unwrap().len(),1);
    manager.set_persistent(durable.id,false).unwrap();manager.close_all().unwrap();
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn failed_persistent_record_rolls_back_creation_and_preserves_corrupt_policy() {
    let (manager,backend)=fixture();let path=saved_policy(&manager);
    let file=path.join("virtual-displays.json");std::fs::write(&file,b"{broken policy}").unwrap();
    let owner=Uuid::new_v4();let _guard=manager.session_guard(owner);
    assert!(manager.create_for_session(owner,Spec::default(),true,None).is_err());
    assert!(manager.list().is_err());
    assert_eq!(backend.removed.lock().unwrap().len(),0,"invalid durable policy is rejected before target activation");
    assert!(backend.activated.lock().unwrap().is_empty());
    assert!(backend.claims.lock().unwrap().is_empty());
    assert_eq!(std::fs::read(file).unwrap(),b"{broken policy}");
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn display_rename_is_metadata_only_and_does_not_require_an_idle_host() {
    let (manager,backend)=fixture();let path=saved_policy(&manager);
    let display=manager.create_manual(Spec{hdr:true,..Spec::default()},true).unwrap();
    let target=manager.lock().unwrap()[&display.id].target.clone();
    let observed=Spec{hdr:false,..display.spec};
    backend.targets.lock().unwrap().iter_mut().find(|t|t.key==target).unwrap().actual=Some(observed);
    let gate=Arc::new(crate::shared_host::SessionGate::default());manager.register_host(&gate);
    let _active=gate.claim("streaming-peer").unwrap();
    let edited=manager.configure_display(display.id,"Desk".into(),display.spec).unwrap();
    assert_eq!(edited.name,"Desk");assert_eq!(edited.actual,Some(observed));assert_eq!(edited.spec,display.spec);
    assert!(backend.reconfigured.lock().unwrap().is_empty());
    assert_eq!(manager.saved_records().unwrap()[0].name,"Desk");
    manager.remove(display.id).unwrap();std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn display_mode_edit_requires_idle_host_then_reconfigures_and_updates_durable_intent() {
    let (manager,backend)=fixture();let path=saved_policy(&manager);
    let display=manager.create_manual(Spec::default(),true).unwrap();
    let target=manager.lock().unwrap()[&display.id].target.clone();
    let gate=Arc::new(crate::shared_host::SessionGate::default());manager.register_host(&gate);
    let active=gate.claim("streaming-peer").unwrap();
    let next=Spec{width:2560,height:1440,refresh_millihz:120_000,hdr:true};
    assert!(matches!(manager.configure_display(display.id,"Desk".into(),next),Err(Error::Platform(message)) if message.contains("结束")));
    assert!(backend.reconfigured.lock().unwrap().is_empty());
    drop(active);
    let edited=manager.configure_display(display.id,"Desk".into(),next).unwrap();
    assert_eq!(edited.spec,next);assert_eq!(edited.actual,Some(next));
    assert_eq!(*backend.reconfigured.lock().unwrap(),vec![(target,next)]);
    let saved=manager.saved_records().unwrap();assert_eq!(saved.len(),1);assert_eq!(saved[0].name,"Desk");assert_eq!(saved[0].spec,next);
    manager.remove(display.id).unwrap();std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn failed_mode_apply_rolls_back_to_the_exact_previous_mode() {
    let (manager,backend)=fixture();let path=saved_policy(&manager);
    let display=manager.create_manual(Spec::default(),true).unwrap();
    let target=manager.lock().unwrap()[&display.id].target.clone();
    let next=Spec{width:3840,height:2160,refresh_millihz:60_000,hdr:false};
    backend.fail_reconfigure_once.store(true,Ordering::SeqCst);
    backend.write_before_reconfigure_error.store(true,Ordering::SeqCst);
    assert!(manager.configure_display(display.id,"Desk".into(),next).is_err());
    assert_eq!(*backend.reconfigured.lock().unwrap(),vec![(target.clone(),next),(target.clone(),Spec::default())]);
    let listed=manager.list().unwrap();assert_eq!(listed[0].spec,Spec::default());assert_eq!(listed[0].actual,Some(Spec::default()));assert!(listed[0].last_error.is_some());
    assert_eq!(manager.saved_records().unwrap()[0].spec,Spec::default());
    assert!(backend.is_claimed(&target).unwrap());
    manager.remove(display.id).unwrap();std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn durable_write_failure_rolls_back_a_submitted_mode_change_without_overwriting_corruption() {
    let (manager,backend)=fixture();let path=saved_policy(&manager);
    let display=manager.create_manual(Spec::default(),true).unwrap();
    let policy=path.join("virtual-displays.json");let valid=std::fs::read(&policy).unwrap();
    std::fs::write(&policy,b"{broken policy}").unwrap();
    let next=Spec{width:2560,height:1600,refresh_millihz:75_000,hdr:false};
    assert!(manager.configure_display(display.id,"Desk".into(),next).is_err());
    assert_eq!(backend.reconfigured.lock().unwrap().len(),2);
    assert_eq!(backend.targets.lock().unwrap()[1].actual,Some(Spec::default()));
    assert!(manager.list().is_err(),"the corrupted durable record must not be silently hidden");
    assert_eq!(std::fs::read(&policy).unwrap(),b"{broken policy}");
    std::fs::write(&policy,valid).unwrap();
    let listed=manager.list().unwrap();assert_eq!(listed[0].spec,Spec::default());assert_eq!(listed[0].actual,Some(Spec::default()));assert!(listed[0].last_error.is_some());
    manager.remove(display.id).unwrap();std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn disconnected_saved_display_can_be_edited_without_hardware_then_restored_with_new_mode() {
    let (first,backend)=fixture();let path=saved_policy(&first);
    let display=first.create_manual(Spec::default(),true).unwrap();
    first.shutdown_manual().unwrap();
    let second=another_manager(&backend);second.configure_saved_displays(&path).unwrap();
    let saved=second.list().unwrap().into_iter().find(|d|d.id==display.id).unwrap();
    assert!(!saved.active&&saved.persistent&&saved.actual.is_none());
    let next=Spec{width:1920,height:1200,refresh_millihz:59_940,hdr:false};
    let edited=second.configure_display(display.id,"Office".into(),next).unwrap();
    assert_eq!(edited.name,"Office");assert_eq!(edited.spec,next);assert!(!edited.active);
    assert!(backend.reconfigured.lock().unwrap().is_empty());
    second.restore_saved_displays().unwrap();
    let restored=second.list().unwrap().into_iter().find(|d|d.id==display.id).unwrap();
    assert!(restored.active);assert_eq!(restored.name,"Office");assert_eq!(restored.spec,next);assert_eq!(restored.actual,Some(next));
    assert_eq!(backend.reconfigured.lock().unwrap().len(),1);
    second.shutdown_manual().unwrap();std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn saved_records_count_toward_limit_and_close_all_retires_them() {
    let (first,backend)=fixture();let path=saved_policy(&first);
    let saved_id=first.create_manual(Spec::default(),true).unwrap().id;
    first.shutdown_manual().unwrap();
    let second=another_manager(&backend);second.configure_saved_displays(&path).unwrap();
    let status=second.status().unwrap();assert_eq!(status.leased,0);assert_eq!(status.allocated,1);assert_eq!(status.allocation_count(),1);
    second.create_manual(Spec::default(),false).unwrap();
    assert!(matches!(second.create(Owner::Manual,Spec::default()),Err(Error::LimitReached{limit:2})));
    second.close_all().unwrap();
    assert!(second.list().unwrap().is_empty());
    assert!(!second.saved_records().unwrap().iter().any(|saved|saved.id==saved_id.0));
    assert!(backend.claims.lock().unwrap().is_empty());
    let third=another_manager(&backend);third.configure_saved_displays(&path).unwrap();third.restore_saved_displays().unwrap();
    assert!(third.list().unwrap().is_empty());
    std::fs::remove_dir_all(path).unwrap();
}
