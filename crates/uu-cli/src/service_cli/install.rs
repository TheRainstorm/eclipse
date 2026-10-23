use super::*;
use anyhow::{ensure, Context};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, os::windows::fs::MetadataExt, path::Path};
use windows_sys::Win32::Security::Authorization::*;
struct InstallationLock(Handle);
impl Drop for InstallationLock {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0 .0);
        }
    }
}
fn installation_lock() -> Result<InstallationLock> {
    let descriptor = Descriptor::administrators()?;
    let attributes = descriptor.attributes();
    let handle = Handle(unsafe {
        CreateMutexW(
            &attributes,
            0,
            wide("Global\\EclipseRemoteHost.install").as_ptr(),
        )
    });
    ensure!(!handle.0.is_null(), "无法锁定服务安装状态");
    let result = unsafe { WaitForSingleObject(handle.0, 0) };
    ensure!(
        result == WAIT_OBJECT_0 || result == WAIT_ABANDONED,
        "另一个服务安装操作正在进行"
    );
    Ok(InstallationLock(handle))
}
use windows_sys::Win32::{
    Foundation::*,
    Security::*,
    Storage::FileSystem::*,
    System::{
        RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId},
        Threading::*,
    },
};

fn verify_acl(path: &Path) -> Result<()> {
    let mut owner = ptr::null_mut();
    let mut dacl = ptr::null_mut();
    let mut descriptor = ptr::null_mut();
    let error = unsafe {
        GetNamedSecurityInfoW(
            wide(path.as_os_str()).as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    ensure!(
        error == ERROR_SUCCESS,
        "无法核验后台服务目录权限（{error}）"
    );
    let _descriptor = Descriptor(descriptor);
    let trusted = |sid: &str| sid == "S-1-5-18" || sid == "S-1-5-32-544";
    ensure!(
        trusted(&sid_string(owner)?),
        "后台服务路径由普通用户拥有，请移走该目录后重新安装"
    );
    ensure!(!dacl.is_null(), "后台服务路径缺少访问控制");
    let mut information: ACL_SIZE_INFORMATION = unsafe { std::mem::zeroed() };
    winok(unsafe {
        GetAclInformation(
            dacl,
            (&mut information as *mut ACL_SIZE_INFORMATION).cast(),
            std::mem::size_of_val(&information) as u32,
            AclSizeInformation,
        )
    })?;
    for index in 0..information.AceCount {
        let mut ace = ptr::null_mut();
        winok(unsafe { GetAce(dacl, index, &mut ace) })?;
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        ensure!(
            header.AceType
                == windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE as u8,
            "后台服务路径包含无法验证的权限规则"
        );
        let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        let sid = ptr::addr_of!(allowed.SidStart).cast_mut().cast();
        ensure!(
            trusted(&sid_string(sid)?),
            "后台服务路径仍允许普通用户访问，请移走该目录后重新安装"
        );
    }
    Ok(())
}

fn regular(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file()
            && meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
            && meta.len() <= limit,
        "迁移源必须是有界普通文件"
    );
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "迁移源文件过大");
    Ok(bytes)
}
pub fn protect(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "后台服务目录不能包含重解析点"
    );
    verify_acl(path)?;
    let descriptor = Descriptor::administrators()?;
    winok(unsafe {
        SetFileSecurityW(
            wide(path.as_os_str()).as_ptr(),
            OWNER_SECURITY_INFORMATION
                | GROUP_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor.0,
        )
    })?;
    Ok(())
}
pub fn directory(path: &Path) -> Result<()> {
    if !path.exists() {
        let descriptor = Descriptor::administrators()?;
        let attributes = descriptor.attributes();
        winok(unsafe { CreateDirectoryW(wide(path.as_os_str()).as_ptr(), &attributes) })?;
    }
    protect(path)
}
fn protect_tree(path: &Path) -> Result<()> {
    protect(path)?;
    if path.is_dir() {
        for item in fs::read_dir(path)? {
            protect_tree(&item?.path())?;
        }
    }
    Ok(())
}
fn copy(source: &Path, destination: &Path) -> Result<()> {
    let bytes = regular(source, 1024 * 1024)?;
    fs::write(destination, bytes)?;
    protect(destination)
}
pub fn install(source: PathBuf) -> Result<()> {
    administrator_file_owner()?;
    let package = std::env::current_exe()?
        .parent()
        .context("无法确定安装包目录")?
        .to_path_buf();
    install_from(source, package, true)
}
fn install_from(source: PathBuf, package: PathBuf, enforce_console: bool) -> Result<()> {
    let _lock = installation_lock()?;
    let mut session = 0;
    winok(unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) })?;
    ensure!(
        !enforce_console || session == unsafe { WTSGetActiveConsoleSessionId() },
        "请在需要共享的 Windows 控制台用户中安装后台服务"
    );
    let owner = process_user(unsafe { GetCurrentProcess() })?;
    ensure!(owner != "S-1-5-18", "安装需要从当前控制台用户发起");
    let paths = Paths::discover()?;
    directory(&paths.programs)?;
    directory(&paths.root)?;
    let existing = paths.root.join("service.json").try_exists()?;
    if existing {
        protect_tree(&paths.root)?;
        ensure!(
            paths.manifest()?.owner_sid == owner,
            "后台服务属于另一位 Windows 用户，请先移除原安装和配置"
        );
    }
    let manifest: serde_json::Value =
        serde_json::from_slice(&regular(&package.join("version.json"), 64 * 1024)?)?;
    let revision = manifest["source_commit"]
        .as_str()
        .context("安装包缺少源码版本")?
        .to_owned();
    let installed = Installed {
        version: 1,
        owner_sid: owner,
        revision,
        build: hex::encode(Sha256::digest(serde_json::to_vec(&manifest["sha256"])?)),
    };
    installed.validate()?;
    let hashes = manifest["sha256"]
        .as_object()
        .context("安装包缺少完整性记录")?;
    let payload = [
        "uu.exe",
        "uu-gui.exe",
        "libgcc_s_seh-1.dll",
        "libstdc++-6.dll",
        "libvpl.dll",
    ];
    // Validate the complete executable set before changing the active service.
    let mut files = Vec::new();
    for name in payload {
        let bytes = regular(&package.join(name), 512 * 1024 * 1024)?;
        ensure!(
            hashes.get(name).and_then(|v| v.as_str())
                == Some(hex::encode(Sha256::digest(&bytes)).as_str()),
            "安装包文件校验失败：{name}"
        );
        files.push((name, bytes));
    }
    if status().is_ok() {
        super::stop()?;
    }
    let versions = paths.programs.join("versions");
    directory(&versions)?;
    let destination = paths.binary_directory(&installed);
    if destination.exists() {
        protect_tree(&destination)?;
    } else {
        directory(&destination)?;
    }
    for (name, bytes) in files {
        let path = destination.join(name);
        if path.exists()
            && hex::encode(Sha256::digest(regular(&path, 512 * 1024 * 1024)?))
                == hashes[name].as_str().unwrap_or("")
        {
            continue;
        }
        ensure!(
            !path.exists(),
            "该版本的受保护程序已有不同内容，请保留原目录并重新获取有效安装包"
        );
        fs::write(&path, bytes)?;
        protect(&path)?;
    }
    fs::write(
        destination.join("version.json"),
        serde_json::to_vec(&manifest)?,
    )?;
    protect(&destination.join("version.json"))?;
    directory(&paths.root.join("data"))?;
    directory(&paths.root.join("state"))?;
    let config = paths.config();
    directory(&config.data_dir)?;
    directory(&config.state_dir)?;
    if !existing {
        let owner_directory = source.join("device-trust");
        let deadline = Instant::now() + Duration::from_secs(8);
        let _source_owner = loop {
            match uu_core::shared_host::HostOwner::claim(&owner_directory) {
                Ok(owner) => break owner,
                Err(error) => {
                    ensure!(
                        Instant::now() < deadline,
                        "请先停止前台共享再安装后台服务：{error}"
                    );
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        };
        let source_store = uu_core::device_book::Store::open(&owner_directory)?;
        let mut book = source_store.read()?;
        book.devices.clear();
        book.sharing.auto_start = false;
        for folder in ["device-trust", "native-lan", "service-credentials"] {
            directory(&config.at(folder))?;
        }
        for relative in [
            "state.json",
            "signing-key",
            "device-trust/identity.pk8",
            "device-trust/peers.json",
            "device-trust/pairing-password.json",
            "native-lan/server-identity.json",
            "native-lan/authorized-clients.json",
        ] {
            let from = source.join(relative);
            if from.try_exists()? {
                copy(&from, &config.at(relative))?;
            }
        }
        book.services.retain(|service| service.host_enabled);
        for service in &mut book.services {
            let destination = config
                .at("service-credentials")
                .join(format!("{}-token", service.id));
            copy(&service.token_file, &destination)?;
            service.token_file = destination;
            if let Some(from) = &service.certificate {
                let destination = config
                    .at("service-credentials")
                    .join(format!("{}-certificate", service.id));
                copy(from, &destination)?;
                service.certificate = Some(destination);
            }
        }
        uu_core::device_book::Store::open(config.at("device-trust"))?.edit(|saved| {
            *saved = book;
            Ok(())
        })?;
        protect_tree(&config.data_dir)?;
        source_store.edit(|book| {
            book.sharing.auto_start = false;
            Ok(())
        })?;
    }
    fs::write(
        paths.root.join("service.json"),
        serde_json::to_vec(&installed)?,
    )?;
    protect(&paths.root.join("service.json"))?;
    let manager = ServiceHandle(unsafe {
        OpenSCManagerW(
            ptr::null(),
            ptr::null(),
            SC_MANAGER_CREATE_SERVICE | SC_MANAGER_CONNECT,
        )
    });
    ensure!(!manager.0.is_null(), "无法打开 Windows 服务管理器");
    let executable = destination.join("uu.exe");
    let command = wide(format!("\"{}\" service daemon", executable.display()));
    let name = wide(SERVICE_NAME);
    let service = unsafe {
        OpenServiceW(
            manager.0,
            name.as_ptr(),
            SERVICE_CHANGE_CONFIG | SERVICE_START | SERVICE_QUERY_STATUS,
        )
    };
    if service.is_null() {
        let service = ServiceHandle(unsafe {
            CreateServiceW(
                manager.0,
                name.as_ptr(),
                wide("Eclipse Remote Host").as_ptr(),
                SERVICE_START | SERVICE_QUERY_STATUS,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_AUTO_START,
                SERVICE_ERROR_NORMAL,
                command.as_ptr(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
            )
        });
        ensure!(
            !service.0.is_null(),
            "无法注册 Windows 后台服务：{}",
            std::io::Error::last_os_error()
        );
    } else {
        let service = ServiceHandle(service);
        winok(unsafe {
            ChangeServiceConfigW(
                service.0,
                SERVICE_NO_CHANGE,
                SERVICE_AUTO_START,
                SERVICE_NO_CHANGE,
                command.as_ptr(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
            )
        })?;
    }
    super::start()
}

pub fn import_account(source: PathBuf) -> Result<()> {
    administrator_file_owner()?;
    let _lock = installation_lock()?;
    let paths = Paths::discover()?;
    let installed = paths.manifest()?;
    ensure!(
        process_user(unsafe { GetCurrentProcess() })? == installed.owner_sid,
        "只有安装服务的 Windows 用户可以同步账号"
    );
    let state = uu_core::ClientState::load(&source.join("state.json"))?;
    let key = source.join("signing-key");
    if key.try_exists()? {
        let _ = uu_core::signing_key::SigningKey::load_file(&key)?;
    }
    let was_running = status()? == SERVICE_RUNNING;
    if was_running {
        super::stop()?;
    }
    let result = (|| -> Result<()> {
        state.save(&paths.config().at("state.json"))?;
        if key.try_exists()? {
            copy(&key, &paths.config().at("signing-key"))?;
        }
        Ok(())
    })();
    if was_running {
        super::start()?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use uu_core::{
        device_trust::{Permissions, TrustStore},
        host_admin,
        service_admin::{self, Request, Response},
    };

    struct Fixture {
        paths: Paths,
        source: PathBuf,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if status().is_ok() {
                let _ = super::super::stop();
            }
            if let Ok(service) = open_service(DELETE) {
                unsafe {
                    DeleteService(service.0);
                }
            }
            // Do not remove configuration if a failed stop left our child alive.
            if !matches!(status(), Ok(SERVICE_RUNNING) | Ok(SERVICE_STOP_PENDING)) {
                for _ in 0..40 {
                    match fs::remove_dir_all(&self.paths.programs) {
                        Ok(()) => break,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                        Err(_) => std::thread::sleep(Duration::from_millis(50)),
                    }
                }
                let _ = fs::remove_dir_all(&self.paths.root);
            }
            let _ = fs::remove_dir_all(&self.source);
        }
    }
    async fn inspect() -> Response {
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            if let Ok(response @ Response::Status { .. }) =
                service_admin::windows::request(Request::Status).await
            {
                return response;
            }
            assert!(
                Instant::now() < deadline,
                "owned background host did not become manageable"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires an elevated Windows console owner, a verified package, and completely absent service directories; never captures desktop pixels"]
    async fn protected_service_migrates_authorization_restarts_and_releases_owned_child() {
        administrator_file_owner().unwrap();
        let _lock = installation_lock().unwrap();
        let paths = Paths::discover().unwrap();
        assert!(
            !paths.root.try_exists().unwrap() && !paths.programs.try_exists().unwrap(),
            "refuse to touch an existing installation"
        );
        let manager =
            ServiceHandle(unsafe { OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_CONNECT) });
        assert!(!manager.0.is_null());
        let service =
            unsafe { OpenServiceW(manager.0, wide(SERVICE_NAME).as_ptr(), SERVICE_QUERY_STATUS) };
        assert!(service.is_null(), "refuse to touch an existing service");
        assert_eq!(unsafe { GetLastError() }, ERROR_SERVICE_DOES_NOT_EXIST);
        let package = PathBuf::from(
            std::env::var_os("ECLIPSE_SERVICE_TEST_PACKAGE")
                .expect("explicit verified fixture package is required"),
        );
        assert!(package.join("version.json").is_file());
        let source =
            std::env::temp_dir().join(format!("eclipse-owned-service-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&source).unwrap();
        let fixture = Fixture { paths, source };
        // A same-name pipe owned by an administrator is still not the service.
        // Verify that no pairing code is written before server identity checks.
        let mut fake = pipe(true).unwrap();
        let fake = tokio::spawn(async move {
            fake.connect().await.unwrap();
            let mut bytes = [0u8; 4];
            tokio::time::timeout(
                Duration::from_secs(2),
                tokio::io::AsyncReadExt::read(&mut fake, &mut bytes),
            )
            .await
            .unwrap()
            .unwrap()
        });
        assert!(service_admin::windows::request(Request::Approve {
            id: "generated".into(),
            code: "12345678".into(),
            permissions: Permissions::VIEW
        })
        .await
        .is_err());
        assert_eq!(
            fake.await.unwrap(),
            0,
            "unverified local server received a pairing code"
        );
        let source_data = fixture.source.join("data");
        fs::create_dir(&source_data).unwrap();
        let trust = TrustStore::open(source_data.join("device-trust")).unwrap();
        let controller = TrustStore::open(fixture.source.join("controller"))
            .unwrap()
            .card("Generated controller");
        trust
            .authorize(controller.clone(), Permissions::VIEW, None)
            .unwrap();
        let identity = trust.card("Generated host").id();
        let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = tcp.local_addr().unwrap().port();
        let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let discovery = udp.local_addr().unwrap().port();
        drop((tcp, udp));
        let store = uu_core::device_book::Store::open(source_data.join("device-trust")).unwrap();
        store
            .edit(|book| {
                book.sharing.name = "Generated background fixture".into();
                book.sharing.monitor = "eclipse-owned-no-capture".into();
                book.sharing.service_port = port;
                book.sharing.discovery_port = discovery;
                book.sharing.ip_enabled = true;
                book.sharing.uu_enabled = false;
                book.sharing.control = false;
                book.sharing.audio = false;
                book.sharing.clipboard = false;
                book.sharing.auto_start = true;
                Ok(())
            })
            .unwrap();
        install_from(source_data.clone(), package, false).unwrap();
        let Response::Status { snapshot, .. } = inspect().await else {
            unreachable!()
        };
        assert!(snapshot.server.as_ref().unwrap().active.monitor == "eclipse-owned-no-capture");
        let protected = fixture.paths.config();
        let protected_trust = TrustStore::open(protected.at("device-trust")).unwrap();
        assert_eq!(protected_trust.card("Generated host").id(), identity);
        assert_eq!(
            protected_trust
                .peer(&controller.id())
                .unwrap()
                .unwrap()
                .inbound,
            Some(Permissions::VIEW)
        );
        assert!(!store.read().unwrap().sharing.auto_start);
        for path in [
            protected.at("device-trust/identity.pk8"),
            protected.at("device-trust/peers.json"),
            protected.at("device-trust/connections.json"),
        ] {
            verify_acl(&path).unwrap();
        }
        // The client verifies both the System token and exact installed binary.
        use std::os::windows::io::AsRawHandle;
        let client = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(PIPE_NAME)
            .unwrap();
        let mut pid = 0;
        winok(unsafe {
            windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId(
                client.as_raw_handle() as _,
                &mut pid,
            )
        })
        .unwrap();
        drop(client);
        let process = Handle(unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION
                    | windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE,
                0,
                pid,
            )
        });
        assert!(!process.0.is_null());
        let mut session = 0;
        winok(unsafe { ProcessIdToSessionId(pid, &mut session) }).unwrap();
        assert_eq!(session, unsafe { WTSGetActiveConsoleSessionId() });
        assert_eq!(process_user(process.0).unwrap(), "S-1-5-18");
        let server = snapshot.server.unwrap();
        let mut next = server.saved.clone();
        next.name = "Generated renamed fixture".into();
        let response = service_admin::windows::request(Request::Admin {
            request: host_admin::Request::Save {
                revision: server.revision,
                settings: next.clone(),
                restart: false,
            },
        })
        .await
        .unwrap();
        assert!(matches!(
            response,
            Response::Admin {
                response: host_admin::Response::Saved { restart: false }
            }
        ));
        let Response::Status { snapshot, .. } = inspect().await else {
            unreachable!()
        };
        let server = snapshot.server.unwrap();
        assert_ne!(server.active.name, next.name);
        assert_eq!(server.saved.name, next.name);
        service_admin::windows::request(Request::Restart)
            .await
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let Response::Status { snapshot, .. } = inspect().await else {
                unreachable!()
            };
            if snapshot.server.unwrap().active.name == next.name {
                break;
            }
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(
            unsafe { WaitForSingleObject(process.0, 0) },
            WAIT_TIMEOUT,
            "configuration restart retains its supervised process"
        );
        super::super::stop().unwrap();
        assert_eq!(
            unsafe { WaitForSingleObject(process.0, 2000) },
            WAIT_OBJECT_0
        );
        let _port = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
        assert_eq!(
            uu_core::device_book::Store::open(protected.at("device-trust"))
                .unwrap()
                .read()
                .unwrap()
                .sharing
                .name,
            next.name
        );
        drop(fixture);
        assert!(!Paths::discover().unwrap().root.exists());
        assert!(!Paths::discover().unwrap().programs.exists());
        assert!(status().is_err());
        println!("owned System console host, protected ACLs, identity and authorization migration, saved-only change, live restart, child stop and port release passed; desktop pixels were never captured");
    }
}
