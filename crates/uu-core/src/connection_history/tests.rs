use super::*;
use crate::shared_host::SessionGate;

fn root() -> PathBuf {
    let path = std::env::temp_dir().join(format!("eclipse-history-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap(); path
}

#[test]
fn latest_two_hundred_records_are_bounded_and_failed_attempts_remain_visible() {
    let history = History::default();
    let mut notices = history.subscribe();
    for index in 0..205 {
        let id = uuid::Uuid::new_v4().to_string();
        let mut record = history.begin(&id, "peer", &format!("Device {index}"));
        history.source(&mut record, Source::Ip, Some("192.0.2.10".parse().unwrap()));
        history.end(&mut record, false);
    }
    let snapshot = history.snapshot();
    assert_eq!(snapshot.records.len(), LIMIT);
    assert_eq!(snapshot.records.front().unwrap().name, "Device 204");
    assert_eq!(snapshot.records.back().unwrap().name, "Device 5");
    assert!(snapshot.records.iter().all(|r| r.outcome == Outcome::Failed && r.connected_at.is_none()));
    assert!(notices.try_recv().is_err(), "pending or failed attempts never claim a stream connected");
    assert!(history.changed_since(Some(snapshot.revision)).is_none());
    assert!(serde_json::to_vec(&snapshot).unwrap().len() < crate::service_admin::MAX_MESSAGE);
}

#[test]
fn multiple_screens_have_independent_records_but_one_notice_per_connected_device() {
    let gate = Arc::new(SessionGate::default());
    let mut notices = gate.history.subscribe();
    let first = gate.claim("same-device").unwrap();
    let second = gate.claim("same-device").unwrap();
    gate.connected(first.id());
    assert_eq!(notices.try_recv().unwrap().peer, "same-device");
    for _ in 0..3 { gate.connected(first.id()); gate.connected(second.id()); }
    assert!(notices.try_recv().is_err());
    assert_eq!(gate.history.snapshot().records.iter().filter(|r| r.connected_at.is_some()).count(), 2);
    drop(first);
    let third = gate.claim("same-device").unwrap(); gate.connected(third.id());
    assert!(notices.try_recv().is_err(), "another screen still owns this device's connected group");
    let other = gate.claim("another-device").unwrap(); gate.connected(other.id());
    assert_eq!(notices.try_recv().unwrap().peer, "another-device");
    drop((second, third));
    let reconnect = gate.claim("same-device").unwrap(); gate.connected(reconnect.id());
    assert_eq!(notices.try_recv().unwrap().peer, "same-device");
    reconnect.finish(false); drop(reconnect);
    assert_eq!(gate.history.snapshot().records.front().unwrap().outcome, Outcome::Failed);
    drop(other);
    assert!(gate.history.snapshot().records.iter().all(|r| r.ended_at.is_some()));
}

#[test]
fn cancellation_before_media_does_not_announce_and_repeated_status_cannot_reopen_history() {
    let gate = Arc::new(SessionGate::default());
    let mut notices = gate.history.subscribe();
    let lease = gate.claim("cancelled-device").unwrap();
    let id = lease.id().to_owned(); lease.cancel(); gate.connected(&id); drop(lease);
    gate.connected(&id);
    assert!(notices.try_recv().is_err());
    assert_eq!(gate.history.snapshot().records[0].outcome, Outcome::Failed);
}

#[test]
fn final_snapshot_is_flushed_and_restart_marks_unfinished_connections_without_a_fake_end_time() {
    let root = root(); let path = root.join("connection-history.json");
    let history = History::open(path.clone());
    let ended = uuid::Uuid::new_v4().to_string();
    let mut record = history.begin(&ended, "peer-a", "Alpha"); history.connected(&mut record, true); history.end(&mut record, true);
    let live = uuid::Uuid::new_v4().to_string();
    let mut record = history.begin(&live, "peer-b", "Beta"); history.connected(&mut record, false);
    drop(history);
    let saved = History::read(&path).unwrap();
    assert_eq!(saved.records[1].outcome, Outcome::Disconnected);
    assert_eq!(saved.records[0].outcome, Outcome::Connected);
    let history = History::open(path.clone()); let snapshot = history.snapshot();
    assert_eq!(snapshot.records[0].outcome, Outcome::Interrupted);
    assert!(snapshot.records[0].ended_at.is_none());
    assert!(snapshot.records[1].ended_at.is_some());
    assert!(snapshot.revision > saved.revision);
    drop(history);
    assert_eq!(History::read(&path).unwrap().records[0].outcome, Outcome::Interrupted);
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_history_is_preserved_without_preventing_live_sessions() {
    let root = root(); let path = root.join("connection-history.json");
    std::fs::write(&path, b"damaged existing history").unwrap();
    let gate = Arc::new(SessionGate::with_history(path.clone()));
    assert!(gate.history.snapshot().error.is_some());
    let lease = gate.claim("peer").unwrap(); gate.connected(lease.id()); drop(lease);
    assert_eq!(gate.history.snapshot().records.len(), 1); drop(gate);
    assert_eq!(std::fs::read(&path).unwrap(), b"damaged existing history");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn history_retention_does_not_own_live_notifications_and_close_flushes_with_readers_alive() {
    let root = root(); let path = root.join("connection-history.json");
    let gate = Arc::new(SessionGate::with_history(path.clone()));
    let pending = gate.claim("long-preparing").unwrap();
    for _ in 0..205 { drop(gate.claim("failed-before-media").unwrap()); }
    let mut notices = gate.history.subscribe();
    gate.connected(pending.id());
    assert_eq!(notices.try_recv().unwrap().peer, "long-preparing");
    assert!(gate.history.snapshot().records.iter().all(|r| r.id != pending.id()));
    drop(pending);
    gate.history.close();
    assert_eq!(History::read(&path).unwrap().records.len(), LIMIT);
    assert!(gate.history.snapshot().records.iter().all(|r| r.ended_at.is_some()));
    drop(gate); std::fs::remove_dir_all(root).unwrap();
}
