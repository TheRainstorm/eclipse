//! The host's output mute is leased across captures, never owned by one tab.
use anyhow::{ensure, Result};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
};

pub(super) trait Endpoint {
    fn key(&self) -> &str;
    fn muted(&mut self) -> Result<bool>;
    fn set_muted(&mut self, value: bool) -> Result<()>;
}
struct State {
    users: usize,
    original: bool,
    overridden: bool,
}
fn registry() -> &'static Mutex<HashMap<String, State>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, State>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}
pub(super) struct Lease {
    key: String,
    overridden: Arc<AtomicBool>,
}
impl Lease {
    pub fn acquire(endpoint: &mut impl Endpoint, overridden: Arc<AtomicBool>) -> Result<Self> {
        let key = endpoint.key().to_owned();
        let mut leases = registry().lock().unwrap();
        if let Some(state) = leases.get_mut(&key) {
            state.users = state
                .users
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("too many host audio mute leases"))?;
        } else {
            let original = endpoint.muted()?;
            if !original {
                endpoint.set_muted(true)?;
            }
            leases.insert(
                key.clone(),
                State {
                    users: 1,
                    original,
                    overridden: false,
                },
            );
        }
        Ok(Self { key, overridden })
    }
    pub fn release(self, endpoint: &mut impl Endpoint) -> Result<()> {
        ensure!(
            self.key == endpoint.key(),
            "host audio endpoint changed before mute restoration"
        );
        let mut leases = registry().lock().unwrap();
        let Some(state) = leases.get_mut(&self.key) else {
            return Ok(());
        };
        state.overridden |= self.overridden.load(Ordering::Acquire);
        state.users -= 1;
        if state.users != 0 {
            return Ok(());
        }
        let state = leases.remove(&self.key).unwrap();
        // Serialize the last restore with a concurrent new lease. An explicit
        // user change wins; never replay a saved volume or unmute another device.
        if !state.original && !state.overridden && endpoint.muted()? {
            endpoint.set_muted(false)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Device {
        key: String,
        muted: bool,
        writes: Vec<bool>,
        fail: bool,
    }
    impl Endpoint for Device {
        fn key(&self) -> &str {
            &self.key
        }
        fn muted(&mut self) -> Result<bool> {
            Ok(self.muted)
        }
        fn set_muted(&mut self, value: bool) -> Result<()> {
            ensure!(!self.fail, "owned failure");
            self.muted = value;
            self.writes.push(value);
            Ok(())
        }
    }
    fn device(key: &str, muted: bool) -> Device {
        Device {
            key: key.into(),
            muted,
            writes: vec![],
            fail: false,
        }
    }
    #[test]
    fn multiple_captures_restore_only_after_the_last_lease() {
        let mut device = device("owned-multiple", false);
        let a = Lease::acquire(&mut device, Arc::default()).unwrap();
        let b = Lease::acquire(&mut device, Arc::default()).unwrap();
        a.release(&mut device).unwrap();
        assert!(device.muted);
        assert_eq!(device.writes, [true]);
        b.release(&mut device).unwrap();
        assert!(!device.muted);
        assert_eq!(device.writes, [true, false]);
    }
    #[test]
    fn original_mute_and_explicit_user_changes_are_preserved() {
        let mut device = device("owned-preserve", true);
        Lease::acquire(&mut device, Arc::default())
            .unwrap()
            .release(&mut device)
            .unwrap();
        assert!(device.muted);
        assert!(device.writes.is_empty());
        device.muted = false;
        let changed = Arc::new(AtomicBool::new(false));
        let a = Lease::acquire(&mut device, changed.clone()).unwrap();
        let b = Lease::acquire(&mut device, Arc::default()).unwrap();
        changed.store(true, Ordering::Release);
        a.release(&mut device).unwrap();
        b.release(&mut device).unwrap();
        assert!(device.muted);
        assert_eq!(device.writes, [true]);
    }
    #[test]
    fn failed_acquisition_does_not_claim_or_restore_an_output() {
        let mut device = device("owned-failed", false);
        device.fail = true;
        assert!(Lease::acquire(&mut device, Arc::default()).is_err());
        device.fail = false;
        Lease::acquire(&mut device, Arc::default())
            .unwrap()
            .release(&mut device)
            .unwrap();
        assert_eq!(device.writes, [true, false]);
    }
}
