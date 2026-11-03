//! An OS-lifetime claim is separate from the short topology mutation lock.
//! Each mutex is acquired and released by the same dedicated owner thread;
//! holding its guard across threads never calls ReleaseMutex from a non-owner.
use super::{Claim, Error, Result, TargetKey};
use std::{ptr::null_mut, sync::mpsc, thread::JoinHandle};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, GetLastError, LocalFree, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        SECURITY_ATTRIBUTES,
    },
    System::Threading::{CreateMutexExW, ReleaseMutex, WaitForSingleObject, MUTEX_MODIFY_STATE},
};

// Both the elevated background host and the ordinary interactive frontend
// participate. The DACL alone does not override mandatory integrity checks;
// label the synchronization object medium even when System creates it first.
pub(super) const MUTEX_SECURITY: &str =
    "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x00100001;;;IU)S:(ML;;NW;;;ME)";

struct OwnedMutex(HANDLE);
impl Drop for OwnedMutex {
    fn drop(&mut self) {
        // This value is constructed, kept and dropped only on its owner thread.
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}
fn acquire(name: &str) -> Result<OwnedMutex> {
    let name: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let sddl: Vec<u16> = MUTEX_SECURITY.encode_utf16().chain([0]).collect();
    let mut descriptor = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(Error::Platform(format!(
            "virtual display claim security: {}",
            std::io::Error::last_os_error()
        )));
    }
    let security = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let handle = unsafe {
        CreateMutexExW(
            &security,
            name.as_ptr(),
            0,
            0x0010_0000 | MUTEX_MODIFY_STATE,
        )
    };
    let error = unsafe { GetLastError() };
    unsafe {
        LocalFree(descriptor);
    }
    if handle.is_null() {
        return Err(Error::Platform(format!(
            "create virtual display claim: {}",
            std::io::Error::from_raw_os_error(error as i32)
        )));
    }
    let state = unsafe { WaitForSingleObject(handle, 0) };
    if state == WAIT_OBJECT_0 || state == WAIT_ABANDONED {
        return Ok(OwnedMutex(handle));
    }
    let wait_error = unsafe { GetLastError() };
    unsafe {
        CloseHandle(handle);
    }
    if state == WAIT_TIMEOUT {
        return Err(Error::Claimed);
    }
    Err(Error::Platform(format!(
        "wait for virtual display claim: {}",
        std::io::Error::from_raw_os_error(wait_error as i32)
    )))
}

struct Owner {
    release: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}
impl Claim for Owner {}
impl Drop for Owner {
    fn drop(&mut self) {
        self.release.take(); // Disconnection wakes exactly our owner thread.
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                tracing::error!("virtual display claim owner thread panicked");
            }
        }
    }
}
pub(super) fn claim(key: &TargetKey) -> Result<Box<dyn Claim>> {
    // The System media host is launched and checked in the active console
    // session, alongside the frontend. Session 0's SCM supervisor never claims
    // desktop targets; Local keeps unrelated desktop sessions isolated.
    let name = format!("Local\\EclipseVirtualDisplayLease.{}", key.slot_id());
    let (ready, answer) = mpsc::channel();
    let (release, closing) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("eclipse-vdd-claim".into())
        .spawn(move || {
            let owned = match acquire(&name) {
                Ok(owned) => owned,
                Err(error) => {
                    let _ = ready.send(Err(error));
                    return;
                }
            };
            if ready.send(Ok(())).is_ok() {
                let _ = closing.recv();
            }
            drop(owned);
        })
        .map_err(|error| Error::Platform(format!("start virtual display claim owner: {error}")))?;
    match answer.recv() {
        Ok(Ok(())) => Ok(Box::new(Owner {
            release: Some(release),
            thread: Some(thread),
        })),
        response => {
            drop(release);
            let _ = thread.join();
            Err(match response {
                Ok(Err(error)) => error,
                _ => Error::Platform(
                    "virtual display claim owner stopped before registration".into(),
                ),
            })
        }
    }
}
pub(super) fn is_claimed(key: &TargetKey) -> Result<bool> {
    match claim(key) {
        Ok(guard) => {
            drop(guard);
            Ok(false)
        }
        Err(Error::Claimed) => Ok(true),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::{
        Security::{
            Authorization::{
                ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo,
                SE_KERNEL_OBJECT,
            },
            LABEL_SECURITY_INFORMATION,
        },
        System::Threading::OpenMutexW,
    };

    #[test]
    fn claims_are_labeled_for_the_ordinary_frontend_even_when_created_elevated() {
        let name = format!(
            "Local\\EclipseVirtualDisplaySecurityTest-{}",
            uuid::Uuid::new_v4()
        );
        let _guard = acquire(&name).unwrap();
        let name: Vec<u16> = name.encode_utf16().chain([0]).collect();
        // READ_CONTROL is granted to the object owner and allows label query.
        let handle = unsafe { OpenMutexW(0x0002_0000, 0, name.as_ptr()) };
        assert!(!handle.is_null(), "{}", std::io::Error::last_os_error());
        let mut descriptor = null_mut();
        let result = unsafe {
            GetSecurityInfo(
                handle,
                SE_KERNEL_OBJECT,
                LABEL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                &mut descriptor,
            )
        };
        unsafe { CloseHandle(handle) };
        assert_eq!(result, 0);
        let mut label = null_mut();
        let converted = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                LABEL_SECURITY_INFORMATION,
                &mut label,
                null_mut(),
            )
        };
        unsafe { LocalFree(descriptor) };
        assert_ne!(converted, 0, "{}", std::io::Error::last_os_error());
        let mut length = 0;
        while unsafe { *label.add(length) } != 0 {
            length += 1;
        }
        let value = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(label, length) });
        unsafe { LocalFree(label.cast()) };
        // The kernel may add SACL control flags such as AI. They do not
        // change the one mandatory label ACE queried above.
        assert_eq!(value.split_once('(').unwrap().1, "ML;;NW;;;ME)");
    }

    fn key(identity: String) -> TargetKey {
        TargetKey {
            adapter: (17, -9),
            target: 3,
            monitor_path: identity,
        }
    }
    #[test]
    fn a_claim_blocks_other_threads_and_processes_until_its_guard_is_dropped() {
        let identity = format!("generated-owned-monitor-{}", uuid::Uuid::new_v4());
        let key = key(identity.clone());
        let guard = claim(&key).unwrap();
        assert!(is_claimed(&key).unwrap());
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "virtual_display::ownership_windows::tests::claim_child",
                "--nocapture",
            ])
            .env("ECLIPSE_VDD_TEST_CLAIM_KEY", &identity)
            .env("ECLIPSE_VDD_TEST_CLAIM_MODE", "blocked")
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stdout)
        );
        // Releasing from a different caller thread still routes ReleaseMutex to
        // the original owner. This is the Manager/worker handoff in production.
        std::thread::spawn(move || drop(guard)).join().unwrap();
        assert!(!is_claimed(&key).unwrap());
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "virtual_display::ownership_windows::tests::claim_child",
                "--nocapture",
            ])
            .env("ECLIPSE_VDD_TEST_CLAIM_KEY", &identity)
            .env("ECLIPSE_VDD_TEST_CLAIM_MODE", "abandon")
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stdout)
        );
        // The child deliberately exited while its guard was live. Windows
        // releases/abandons only that process's ownership; no stale PID guess.
        assert!(!is_claimed(&key).unwrap());
    }
    #[test]
    fn claim_child() {
        let Ok(identity) = std::env::var("ECLIPSE_VDD_TEST_CLAIM_KEY") else {
            return;
        };
        let key = key(identity);
        match std::env::var("ECLIPSE_VDD_TEST_CLAIM_MODE")
            .unwrap()
            .as_str()
        {
            "blocked" => assert!(matches!(claim(&key), Err(Error::Claimed))),
            "abandon" => {
                let _guard = claim(&key).unwrap();
                std::process::exit(0);
            }
            _ => panic!("unknown owned claim test mode"),
        }
    }
}
