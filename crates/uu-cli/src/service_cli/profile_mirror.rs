//! Protected host state is exported using the console owner's restricted user
//! token. A portable directory never becomes a privileged output pathname.
use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use windows_sys::Win32::{
    Foundation::*,
    Security::*,
    System::{RemoteDesktop::*, Threading::*},
};

pub struct Mirror {
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Mirror {
    pub fn start(paths: &Paths, installed: &Installed) -> Result<Option<Self>> {
        let Some(binding) = paths.profile_for(installed)? else {
            return Ok(None);
        };
        let config = binding.config(&paths.root);
        let owner = installed.owner_sid.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = std::thread::Builder::new().name("eclipse-profile-save".into()).spawn(move || {
            let mut digest = None;
            let mut last_error = String::new();
            loop {
                let result = (|| -> Result<()> {
                    let snapshot = uu_core::service_profile::DataSnapshot::capture(&config.data_dir)?;
                    let next = snapshot.digest();
                    if digest == Some(next) { return Ok(()); }
                    if publish(&binding, &owner, snapshot)? { digest = Some(next); }
                    Ok(())
                })();
                match result {
                    Ok(()) => { last_error.clear(); },
                    Err(error) => {
                        let error = format!("{error:#}");
                        if error != last_error { tracing::warn!(%error, "host profile copy has not been saved to its application directory"); last_error=error; }
                    },
                }
                if stopping.load(Ordering::Acquire) { break; }
                std::thread::park_timeout(Duration::from_secs(1));
            }
        })?;
        Ok(Some(Self {
            stop,
            worker: Some(worker),
        }))
    }
}
impl Drop for Mirror {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}
fn publish(
    binding: &uu_core::service_profile::Binding,
    owner: &str,
    snapshot: uu_core::service_profile::DataSnapshot,
) -> Result<bool> {
    let session = unsafe { WTSGetActiveConsoleSessionId() };
    if session == u32::MAX {
        return Ok(false);
    }
    let mut token = ptr::null_mut();
    if unsafe { WTSQueryUserToken(session, &mut token) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_NO_TOKEN as i32) {
            return Ok(false);
        }
        return Err(error.into());
    }
    let token = Handle(token);
    let mut size = 0;
    unsafe {
        GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut size);
    }
    anyhow::ensure!(size > 0 && size < 65536, "Invalid console user token");
    let mut bytes = vec![
        0usize;
        (size as usize + std::mem::size_of::<usize>() - 1)
            / std::mem::size_of::<usize>()
    ];
    winok(unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            bytes.as_mut_ptr().cast(),
            size,
            &mut size,
        )
    })?;
    let user = unsafe { &*bytes.as_ptr().cast::<TOKEN_USER>() };
    if sid_string(user.User.Sid)? != owner {
        return Ok(false);
    }
    winok(unsafe { ImpersonateLoggedOnUser(token.0) })?;
    struct User;
    impl Drop for User {
        fn drop(&mut self) {
            unsafe {
                RevertToSelf();
            }
        }
    }
    let _user = User;
    // Moving/deleting the application folder is not permission to recreate
    // its old path. The protected copy remains available for recovery.
    anyhow::ensure!(
        binding.user_data.is_dir(),
        "The selected application data directory has moved or is unavailable"
    );
    snapshot.publish(&binding.source())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires explicit SYSTEM fixture and a disposable logged-in user's directory"]
    fn system_exports_profile_as_its_console_owner_and_returns_to_system() {
        assert_eq!(
            process_user(unsafe { GetCurrentProcess() }).unwrap(),
            "S-1-5-18"
        );
        super::super::supervisor::enable("SeTcbPrivilege").unwrap();
        let target = PathBuf::from(
            std::env::var_os("ECLIPSE_MIRROR_TEST_DATA").expect("explicit fixture directory"),
        );
        let owner = std::env::var("ECLIPSE_MIRROR_TEST_OWNER").expect("fixture owner SID");
        assert!(target.is_absolute() && target.join("fixture-marker").is_file());
        assert!(!target.join("server").exists());
        let root = Paths::discover()
            .unwrap()
            .root
            .with_file_name(format!("EclipseProfileTest-{}", uuid::Uuid::new_v4()));
        install::directory(&root).unwrap();
        let source = root.join("data");
        install::directory(&source).unwrap();
        let trust = uu_core::device_trust::TrustStore::open(source.join("device-trust")).unwrap();
        uu_core::device_book::Store::open(source.join("device-trust"))
            .unwrap()
            .read()
            .unwrap();
        std::fs::write(source.join("deleted-secret"), b"fixture secret").unwrap();
        let binding =
            uu_core::service_profile::Binding::new(target.clone(), "a".repeat(40), "b".repeat(64))
                .unwrap();
        assert!(publish(
            &binding,
            &owner,
            uu_core::service_profile::DataSnapshot::capture(&source).unwrap()
        )
        .unwrap());
        let copied =
            uu_core::device_trust::TrustStore::open(target.join("server/device-trust")).unwrap();
        assert_eq!(copied.card("copy").id(), trust.card("original").id());
        assert!(target.join("server/deleted-secret").is_file());
        std::fs::remove_file(source.join("deleted-secret")).unwrap();
        assert!(publish(
            &binding,
            &owner,
            uu_core::service_profile::DataSnapshot::capture(&source).unwrap()
        )
        .unwrap());
        assert!(!target.join("server/deleted-secret").exists());
        let mut thread_token = ptr::null_mut();
        assert_eq!(
            unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut thread_token) },
            0
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(ERROR_NO_TOKEN as i32)
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
