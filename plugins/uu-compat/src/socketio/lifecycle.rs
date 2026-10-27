//! Room lifetime is independent of media sessions and receive-future polling.
use anyhow::{ensure, Context, Result};
use serde_json::Value;
use std::{collections::VecDeque, time::Duration};
use tokio::time::Instant;

const FIRST_REFRESH: u64 = 1_000_000;
const RETRY: Duration = Duration::from_secs(30);
const RENEW: Duration = Duration::from_secs(300);
const MAX_PENDING: usize = 16;

pub(super) struct Timers {
    pub heartbeat_window: Duration,
    pub heartbeat_deadline: Instant,
    pub refresh: Option<Refresh>,
}
pub(super) struct Refresh {
    pub next: Instant,
    retry: Duration,
    renew: Duration,
    last_sent: u64,
    last_accepted: u64,
    pending: VecDeque<u64>,
}
impl Timers {
    pub fn open(json: &str) -> Result<Self> {
        let open: Value = serde_json::from_str(json).context("invalid Engine.IO handshake")?;
        ensure!(open.is_object(), "invalid Engine.IO handshake");
        let period = |name: &str, default| -> Result<Duration> {
            let ms = match open.get(name) {
                Some(value) => value
                    .as_u64()
                    .context("invalid Engine.IO heartbeat interval")?,
                None => default,
            };
            ensure!(
                (1..=300_000).contains(&ms),
                "invalid Engine.IO heartbeat interval"
            );
            Ok(Duration::from_millis(ms))
        };
        let heartbeat_window = period("pingInterval", 25_000)? + period("pingTimeout", 20_000)?;
        Ok(Self {
            heartbeat_window,
            heartbeat_deadline: Instant::now() + heartbeat_window,
            refresh: None,
        })
    }
    pub fn ping(&mut self) {
        self.heartbeat_deadline = Instant::now() + self.heartbeat_window;
    }
    pub fn host(&mut self) {
        if self.refresh.is_none() {
            self.refresh = Some(Refresh::new(Instant::now(), RETRY, RENEW));
        }
    }
}
impl Refresh {
    pub fn new(now: Instant, retry: Duration, renew: Duration) -> Self {
        Self {
            next: now + retry,
            retry,
            renew,
            last_sent: FIRST_REFRESH - 1,
            last_accepted: 0,
            pending: VecDeque::new(),
        }
    }
    pub fn request(&mut self, now: Instant) -> u64 {
        self.last_sent += 1;
        self.pending.push_back(self.last_sent);
        if self.pending.len() > MAX_PENDING {
            self.pending.pop_front();
        }
        // Advance before yielding to a cancellable websocket send. An omitted
        // or rejected ACK retries; incoming traffic cannot postpone this clock.
        self.next = now + self.retry;
        self.last_sent
    }
    pub fn ack(&mut self, id: u64, values: &Value, now: Instant) -> bool {
        if !(FIRST_REFRESH..=self.last_sent).contains(&id) {
            return false;
        }
        let pending = self.pending.contains(&id);
        self.pending.retain(|old| *old != id);
        if pending && id > self.last_accepted && valid_ack(values) {
            self.last_accepted = id;
            self.next = now + self.renew;
        }
        // Late replies are private keepalive traffic too. Never forward a key
        // into application events or diagnostics. Fresh reconnect creates a new
        // room, so no reconnect key needs to be stored or persisted here.
        true
    }
}
fn valid_ack(values: &Value) -> bool {
    values.get(0).and_then(Value::as_str) == Some("success")
        && values
            .get(1)
            .and_then(|v| v.get("reconnect_key"))
            .and_then(Value::as_str)
            .is_some_and(|key| !key.is_empty() && key.len() <= 16 * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn heartbeat_uses_server_handshake_and_rejects_invalid_intervals() {
        let timers = Timers::open(r#"{"pingInterval":25000,"pingTimeout":20000}"#).unwrap();
        assert_eq!(timers.heartbeat_window, Duration::from_secs(45));
        assert_eq!(
            Timers::open("{}").unwrap().heartbeat_window,
            Duration::from_secs(45)
        );
        for open in [
            r#"{"pingInterval":0}"#,
            r#"{"pingTimeout":-1}"#,
            r#"{"pingInterval":"25000"}"#,
            r#"{"pingTimeout":300001}"#,
            "null",
        ] {
            assert!(Timers::open(open).is_err());
        }
    }
    #[test]
    fn only_matching_new_success_renews_and_late_ack_cannot_replace_newer_schedule() {
        let now = Instant::now();
        let mut refresh = Refresh::new(now, RETRY, RENEW);
        let old = refresh.request(now);
        let newer = refresh.request(now + RETRY);
        let success = json!(["success",{"reconnect_key":"generated-fixture-key"}]);
        assert!(!refresh.ack(1, &success, now));
        assert!(refresh.ack(newer, &success, now + RETRY));
        let deadline = refresh.next;
        assert!(refresh.ack(old, &success, now + RETRY + Duration::from_secs(10)));
        assert_eq!(refresh.next, deadline);
        assert!(refresh.ack(newer, &success, now + RETRY + Duration::from_secs(20)));
        assert_eq!(refresh.next, deadline);
        for invalid in [
            json!(["failure",{"reconnect_key":"x"}]),
            json!(["success"]),
            json!(["success",{"reconnect_key":""}]),
            json!(["success",{"reconnect_key":"x".repeat(16385)}]),
        ] {
            let id = refresh.request(now);
            assert!(refresh.ack(id, &invalid, now));
            assert_eq!(refresh.next, now + RETRY);
        }
    }
    #[test]
    fn missing_ack_keeps_retry_bounded_and_does_not_extend_room_lifetime() {
        let now = Instant::now();
        let mut refresh = Refresh::new(now, RETRY, RENEW);
        let first = refresh.request(now);
        for n in 1..100 {
            refresh.request(now + RETRY * n);
        }
        assert_eq!(refresh.pending.len(), MAX_PENDING);
        let deadline = refresh.next;
        assert!(refresh.ack(first, &json!(["success",{"reconnect_key":"obsolete"}]), now));
        assert_eq!(refresh.next, deadline);
    }
}
