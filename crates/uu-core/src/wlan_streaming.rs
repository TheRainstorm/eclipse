//! Per-connection, best-effort WLAN media requests. Never writes FALSE.
use serde::Serialize;
use std::sync::Arc;
#[cfg(windows)]
mod windows;

#[derive(Clone)]
struct Interface {
    id: [u8; 16],
    state: u32,
}
trait Api: Send + Sync {
    fn open(&self) -> Result<(usize, u32), u32>;
    fn interfaces(&self, handle: usize) -> Result<Vec<Interface>, u32>;
    fn query(&self, handle: usize, interface: &Interface) -> Result<bool, u32>;
    /// The only supported write is requesting TRUE on this client's handle.
    fn request(&self, handle: usize, interface: &Interface) -> u32;
    fn close(&self, handle: usize) -> u32;
}
#[derive(Clone, Debug, Serialize)]
pub struct Readback {
    pub result: u32,
    pub enabled: Option<bool>,
}
impl From<Result<bool, u32>> for Readback {
    fn from(value: Result<bool, u32>) -> Self {
        match value {
            Ok(enabled) => Self {
                result: 0,
                enabled: Some(enabled),
            },
            Err(result) => Self {
                result,
                enabled: None,
            },
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct InterfaceReport {
    pub index: usize,
    pub state: u32,
    pub before: Readback,
    pub request_result: Option<u32>,
    pub observed: Readback,
}
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub session: String,
    pub pid: u32,
    pub requested: bool,
    pub open_result: Option<u32>,
    pub negotiated_version: Option<u32>,
    pub enumerate_result: Option<u32>,
    pub reason: Option<String>,
    pub interfaces: Vec<InterfaceReport>,
}
/// No callbacks are registered. A guard owns exactly one client handle and DLL
/// reference. Move it to a blocking boundary for open/close; never share it as
/// a global request or restore a global snapshot when it leaves scope.
pub struct Guard {
    api: Option<Arc<dyn Api>>,
    handle: Option<usize>,
    interfaces: Vec<Interface>,
    report: Report,
}
fn emit(action: &str, report: &impl Serialize) {
    eprintln!(
        "{}",
        serde_json::json!({"event":"wlan_streaming","action":action,"report":report})
    );
}
/// Unset means no API calls at all. Explicit 0 observes without requesting;
/// explicit 1 requests media mode. Invalid experimental values fail closed.
pub fn experiment_request() -> Option<bool> {
    match std::env::var("UU_WLAN_STREAMING_MODE").as_deref() {
        Ok("0") => Some(false),
        Ok("1") => Some(true),
        Err(_) => None,
        Ok(_) => {
            emit(
                "disabled",
                &serde_json::json!({"reason":"invalid_experiment_value"}),
            );
            None
        }
    }
}
impl Guard {
    pub fn acquire(session: impl Into<String>, requested: bool) -> Self {
        #[cfg(windows)]
        let api = windows::load();
        #[cfg(not(windows))]
        let api: Result<Arc<dyn Api>, u32> = Err(50);
        Self::using(session.into(), requested, api)
    }
    fn using(session: String, requested: bool, api: Result<Arc<dyn Api>, u32>) -> Self {
        let mut guard = Self {
            api: None,
            handle: None,
            interfaces: Vec::new(),
            report: Report {
                session,
                pid: std::process::id(),
                requested,
                open_result: None,
                negotiated_version: None,
                enumerate_result: None,
                reason: None,
                interfaces: Vec::new(),
            },
        };
        match api {
            Err(code) => guard.report.reason = Some(format!("WLAN API unavailable: {code}")),
            Ok(api) => {
                match api.open() {
                    Err(code) => {
                        guard.report.open_result = Some(code);
                        guard.report.reason = Some("client_open_failed".into());
                    }
                    Ok((handle, version)) => {
                        guard.handle = Some(handle);
                        guard.report.open_result = Some(0);
                        guard.report.negotiated_version = Some(version);
                        match api.interfaces(handle) {
                            Err(code) => {
                                guard.report.enumerate_result = Some(code);
                                guard.report.reason = Some("interface_enumeration_failed".into());
                            }
                            Ok(interfaces) => {
                                guard.report.enumerate_result = Some(0);
                                if !interfaces.iter().any(|i| i.state == 1) {
                                    guard.report.reason =
                                        Some("no_connected_wlan_interface".into());
                                }
                                for (index, interface) in interfaces.iter().enumerate() {
                                    let before = Readback::from(api.query(handle, interface));
                                    let request_result = (requested && interface.state == 1)
                                        .then(|| api.request(handle, interface));
                                    let observed = Readback::from(api.query(handle, interface));
                                    guard.report.interfaces.push(InterfaceReport {
                                        index,
                                        state: interface.state,
                                        before,
                                        request_result,
                                        observed,
                                    });
                                }
                                guard.interfaces = interfaces;
                            }
                        }
                    }
                }
                guard.api = Some(api);
            }
        }
        emit("acquire", &guard.report);
        guard
    }
    pub fn report(&self) -> &Report {
        &self.report
    }
    /// Read-only observation on this handle; callers choose a low-frequency
    /// boundary. It cannot reverse another connection's request.
    pub fn snapshot(&self) -> Report {
        let mut report = self.report.clone();
        if let (Some(api), Some(handle)) = (&self.api, self.handle) {
            for (entry, interface) in report.interfaces.iter_mut().zip(&self.interfaces) {
                entry.observed = Readback::from(api.query(handle, interface));
            }
        }
        report
    }
    pub fn close(&mut self) -> Option<u32> {
        let handle = self.handle.take()?;
        let result = self.api.as_ref().unwrap().close(handle);
        emit(
            "close",
            &serde_json::json!({"session":self.report.session,"pid":self.report.pid,"result":result}),
        );
        Some(result)
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashSet, sync::Mutex};
    #[derive(Default)]
    struct Fake {
        state: Mutex<(usize, HashSet<usize>, HashSet<usize>, Vec<usize>)>,
        fail_open: bool,
        fail_enum: bool,
        partial: bool,
    }
    impl Api for Fake {
        fn open(&self) -> Result<(usize, u32), u32> {
            if self.fail_open {
                return Err(5);
            }
            let mut s = self.state.lock().unwrap();
            s.0 += 1;
            let h = s.0;
            s.1.insert(h);
            Ok((h, 2))
        }
        fn interfaces(&self, _: usize) -> Result<Vec<Interface>, u32> {
            if self.fail_enum {
                Err(50)
            } else {
                Ok(vec![
                    Interface {
                        id: [0; 16],
                        state: 1,
                    },
                    Interface {
                        id: [1; 16],
                        state: 1,
                    },
                    Interface {
                        id: [2; 16],
                        state: 4,
                    },
                ])
            }
        }
        fn query(&self, _: usize, i: &Interface) -> Result<bool, u32> {
            if self.partial && i.id[0] == 1 {
                Err(13)
            } else {
                Ok(!self.state.lock().unwrap().2.is_empty())
            }
        }
        fn request(&self, h: usize, i: &Interface) -> u32 {
            if self.partial && i.id[0] == 1 {
                5
            } else {
                self.state.lock().unwrap().2.insert(h);
                0
            }
        }
        fn close(&self, h: usize) -> u32 {
            let mut s = self.state.lock().unwrap();
            assert!(s.1.remove(&h));
            s.2.remove(&h);
            s.3.push(h);
            0
        }
    }
    #[test]
    fn unavailable_open_and_enumeration_failures_release_only_owned_resources() {
        drop(Guard::using("no-api".into(), true, Err(126)));
        for fake in [
            Fake {
                fail_open: true,
                ..Default::default()
            },
            Fake {
                fail_enum: true,
                ..Default::default()
            },
        ] {
            let api = Arc::new(fake);
            let g = Guard::using("failure".into(), true, Ok(api.clone()));
            drop(g);
            let s = api.state.lock().unwrap();
            assert!(s.1.is_empty());
            assert!(s.2.is_empty());
            assert_eq!(s.3.len(), usize::from(!api.fail_open));
        }
    }
    #[test]
    fn independent_connections_and_read_only_observer_do_not_revoke_each_other() {
        let api = Arc::new(Fake::default());
        let observer = Guard::using("observer".into(), false, Ok(api.clone()));
        let mut a = Guard::using("a".into(), true, Ok(api.clone()));
        let b = Guard::using("b".into(), true, Ok(api.clone()));
        assert_eq!(a.close(), Some(0));
        assert_eq!(a.close(), None);
        assert_eq!(
            observer.snapshot().interfaces[0].observed.enabled,
            Some(true)
        );
        drop(b);
        assert_eq!(
            observer.snapshot().interfaces[0].observed.enabled,
            Some(false)
        );
        drop(observer);
        assert_eq!(api.state.lock().unwrap().3.len(), 3);
    }
    #[test]
    fn partial_interface_failure_is_reported_and_other_requests_remain_owned() {
        let api = Arc::new(Fake {
            partial: true,
            ..Default::default()
        });
        let guard = Guard::using("partial".into(), true, Ok(api.clone()));
        assert_eq!(guard.report.interfaces[0].request_result, Some(0));
        assert_eq!(guard.report.interfaces[1].request_result, Some(5));
        assert_eq!(guard.report.interfaces[1].observed.enabled, None);
        assert_eq!(guard.report.interfaces[2].request_result, None);
        drop(guard);
        assert!(api.state.lock().unwrap().1.is_empty());
    }
    #[tokio::test]
    async fn connection_failure_and_cancelled_owner_release_the_request() {
        let api = Arc::new(Fake::default());
        let failed: Result<(), ()> = (|| {
            let _guard = Guard::using("failed-connect".into(), true, Ok(api.clone()));
            Err(())
        })();
        assert!(failed.is_err());
        let (ready, started) = tokio::sync::oneshot::channel();
        let cloned = api.clone();
        let task = tokio::spawn(async move {
            let _guard = Guard::using("cancel".into(), true, Ok(cloned));
            let _ = ready.send(());
            std::future::pending::<()>().await;
        });
        started.await.unwrap();
        task.abort();
        let _ = task.await;
        let s = api.state.lock().unwrap();
        assert!(s.1.is_empty());
        assert!(s.2.is_empty());
        assert_eq!(s.3.len(), 2);
    }
}
