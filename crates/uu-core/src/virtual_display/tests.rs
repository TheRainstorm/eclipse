use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
struct Fake {
    targets: Mutex<Vec<Target>>,
    fail_remove: AtomicBool,
    fail_after_activate: AtomicBool,
    activated: Mutex<Vec<TargetKey>>,
    removed: Mutex<Vec<TargetKey>>,
}

impl Backend for Arc<Fake> {
    fn inventory(&self) -> Result<Inventory> {
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
}
