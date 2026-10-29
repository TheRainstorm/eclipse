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
    System::{RemoteDesktop::*, Threading::*},
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
fn console_owner(session: u32) -> Result<String> {
    fn field(session: u32, kind: WTS_INFO_CLASS) -> Result<Vec<u16>> {
        let mut value = ptr::null_mut();
        let mut bytes = 0;
        winok(unsafe {
            WTSQuerySessionInformationW(
                WTS_CURRENT_SERVER_HANDLE,
                session,
                kind,
                &mut value,
                &mut bytes,
            )
        })
        .context("读取控制台用户身份失败")?;
        struct Buffer(*mut u16);
        impl Drop for Buffer {
            fn drop(&mut self) {
                unsafe {
                    WTSFreeMemory(self.0.cast());
                }
            }
        }
        let value = Buffer(value);
        ensure!(
            !value.0.is_null() && bytes >= 2 && bytes <= 65536 && bytes % 2 == 0,
            "控制台用户身份无效"
        );
        let data = unsafe { std::slice::from_raw_parts(value.0, bytes as usize / 2) };
        ensure!(data.last() == Some(&0), "控制台用户身份缺少结尾");
        Ok(data[..data.len() - 1].to_vec())
    }
    let user = field(session, WTSUserName)?;
    ensure!(!user.is_empty(), "请先登录需要共享的 Windows 控制台用户");
    let mut name = field(session, WTSDomainName)?;
    if !name.is_empty() {
        name.push(b'\\' as u16);
    }
    name.extend(user);
    name.push(0);
    let (mut size, mut domain_length, mut kind) = (0, 0, 0);
    unsafe {
        LookupAccountNameW(
            ptr::null(),
            name.as_ptr(),
            ptr::null_mut(),
            &mut size,
            ptr::null_mut(),
            &mut domain_length,
            &mut kind,
        );
    }
    ensure!(
        size > 0 && size < 65536 && domain_length < 32768,
        "无法解析控制台用户 SID"
    );
    let mut sid = vec![
        0usize;
        (size as usize + std::mem::size_of::<usize>() - 1)
            / std::mem::size_of::<usize>()
    ];
    let mut domain = vec![0u16; domain_length as usize];
    winok(unsafe {
        LookupAccountNameW(
            ptr::null(),
            name.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut size,
            domain.as_mut_ptr(),
            &mut domain_length,
            &mut kind,
        )
    })
    .context("解析控制台用户 SID 失败")?;
    ensure!(kind == SidTypeUser, "控制台身份不是 Windows 用户");
    sid_string(sid.as_mut_ptr().cast())
}
fn install_from(source: PathBuf, package: PathBuf, enforce_console: bool) -> Result<()> {
    let _lock = installation_lock()?;
    let mut session = 0;
    winok(unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) })?;
    ensure!(
        !enforce_console || session == unsafe { WTSGetActiveConsoleSessionId() },
        "请在需要共享的 Windows 控制台用户中安装后台服务"
    );
    // UAC may use a different administrator's credentials. The service belongs
    // to the logged-in console user whose desktop was selected by the caller.
    let owner = if enforce_console {
        console_owner(session)?
    } else {
        process_user(unsafe { GetCurrentProcess() })?
    };
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
    let previous = if existing {
        Some(paths.manifest()?)
    } else {
        None
    };
    let before = service_state()?;
    ensure!(
        before.is_none() || previous.is_some(),
        "同名 Windows 服务缺少本产品的受保护安装记录，拒绝覆盖"
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&regular(&package.join("version.json"), 64 * 1024)?)?;
    let revision = manifest["source_commit"]
        .as_str()
        .context("安装包缺少源码版本")?
        .to_owned();
    let installed = Installed {
        version: 2,
        owner_sid: owner,
        revision,
        build: hex::encode(Sha256::digest(serde_json::to_vec(&manifest["sha256"])?)),
    };
    installed.validate()?;
    let hashes = manifest["sha256"]
        .as_object()
        .context("安装包缺少完整性记录")?;
    let payload=uu_core::service_admin::package::executable_files(&manifest)?;
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
    }
    // Stage and verify the complete version before interrupting an old host.
    // Keep both SCM registration and the manifest recoverable on normal errors.
    if before.is_some() {
        super::stop()?;
    }
    let mut created = false;
    let mut switched = false;
    let result = (|| -> Result<()> {
        let (service, new) = registration(&paths, &installed)?;
        created = new;
        switched = true;
        configure_recovery(&service)?;
        paths.save_manifest(&installed)?;
        protect(&paths.root.join("service.json"))?;
        super::start()?;
        uu_core::device_book::Store::open(source.join("device-trust"))?.edit(|book| {
            book.sharing.auto_start = false;
            Ok(())
        })?;
        Ok(())
    })();
    if let Err(error) = result {
        let restored = (|| -> Result<()> {
            if switched && service_state()?.is_some() {
                super::stop()?;
            }
            if let Some(previous) = &previous {
                if fs::read(paths.root.join("service.json")).ok()
                    != Some(serde_json::to_vec(previous)?)
                {
                    paths.save_manifest(previous)?;
                    protect(&paths.root.join("service.json"))?;
                }
                if before.is_some() {
                    let _ = registration(&paths, previous)?;
                }
            } else if switched && paths.root.join("service.json").try_exists()? {
                fs::remove_file(paths.root.join("service.json"))?;
            }
            if created && before.is_none() {
                let service = open_service(DELETE)?;
                winok(unsafe { DeleteService(service.0) })?;
            }
            if before == Some(SERVICE_RUNNING) {
                super::start()?;
            }
            Ok(())
        })();
        return match restored {
            Ok(()) => Err(error.context("后台安装更新失败，已恢复原服务状态")),
            Err(restore) => Err(anyhow::anyhow!(
                "后台安装更新失败：{error:#}；恢复原状态也失败：{restore:#}"
            )),
        };
    }
    Ok(())
}
fn service_state() -> Result<Option<u32>> {
    match status() {
        Ok(state) => Ok(Some(state)),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .and_then(|e| e.raw_os_error())
                == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}
fn registration(paths: &Paths, installed: &Installed) -> Result<(ServiceHandle, bool)> {
    let manager = ServiceHandle(unsafe {
        OpenSCManagerW(
            ptr::null(),
            ptr::null(),
            SC_MANAGER_CREATE_SERVICE | SC_MANAGER_CONNECT,
        )
    });
    ensure!(!manager.0.is_null(), "无法打开 Windows 服务管理器");
    let executable = paths.binary_directory(installed).join(installed.cli_name());
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
        ensure!(
            unsafe { GetLastError() } == ERROR_SERVICE_DOES_NOT_EXIST,
            "无法读取现有后台服务配置"
        );
        let service = ServiceHandle(unsafe {
            CreateServiceW(
                manager.0,
                name.as_ptr(),
                wide("Eclipse Remote Host").as_ptr(),
                SERVICE_CHANGE_CONFIG | SERVICE_START | SERVICE_QUERY_STATUS,
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
        Ok((service, true))
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
        Ok((service, false))
    }
}

fn configure_recovery(service: &ServiceHandle) -> Result<()> {
    let mut actions = [
        SC_ACTION {
            Type: SC_ACTION_RESTART,
            Delay: 1000,
        },
        SC_ACTION {
            Type: SC_ACTION_RESTART,
            Delay: 5000,
        },
        SC_ACTION {
            Type: SC_ACTION_RESTART,
            Delay: 15000,
        },
    ];
    let mut empty = wide("");
    let recovery = SERVICE_FAILURE_ACTIONSW {
        dwResetPeriod: 86400,
        lpRebootMsg: empty.as_mut_ptr(),
        lpCommand: empty.as_mut_ptr(),
        cActions: actions.len() as u32,
        lpsaActions: actions.as_mut_ptr(),
    };
    winok(unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_FAILURE_ACTIONS,
            (&recovery as *const SERVICE_FAILURE_ACTIONSW).cast(),
        )
    })
    .context("设置后台服务异常恢复失败")?;
    let flag = SERVICE_FAILURE_ACTIONS_FLAG {
        fFailureActionsOnNonCrashFailures: 1,
    };
    winok(unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_FAILURE_ACTIONS_FLAG,
            (&flag as *const SERVICE_FAILURE_ACTIONS_FLAG).cast(),
        )
    })
    .context("设置后台服务错误退出恢复失败")
}

pub fn import_plugins(source: PathBuf) -> Result<()> {
    administrator_file_owner()?;
    let _lock = installation_lock()?;
    let paths = Paths::discover()?;
    let installed = paths.manifest()?;
    let session = unsafe { WTSGetActiveConsoleSessionId() };
    let mut caller_session = 0;
    winok(unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut caller_session) })?;
    ensure!(
        caller_session == session && console_owner(session)? == installed.owner_sid,
        "请在后台服务所属的 Windows 控制台用户中同步插件"
    );
    let target_program=paths.binary_directory(&installed);
    let package:serde_json::Value=serde_json::from_slice(&regular(&target_program.join("version.json"),1024*1024)?)?;
    ensure!(package["plugin_sdk_id"].as_str()==Some(uu_plugin_sdk::SDK_ID),"请先更新后台服务程序，再同步匹配的插件");
    let config=Config{data_dir:source.clone(),state_dir:source.join("state"),signaling_host:String::new()};
    let preferences=uu_core::plugins::Preferences::load(&config)?;
    let source_program=std::env::current_exe()?.parent().context("missing application directory")?.to_owned();
    let found=uu_plugin_sdk::loader::discover(&source_program.join("plugins"));
    ensure!(found.errors.is_empty(),"插件清单无效：{}",found.errors.join("; "));
    let mut writes:Vec<(PathBuf,Vec<u8>)>=Vec::new();
    let mut directories=vec![target_program.join("plugins"),paths.config().at("plugins")];
    let mut total=0usize;
    for plugin in found.installed.into_iter().filter(|p|preferences.get(&p.manifest.id).enabled) {
        ensure!(plugin.compatible(),"插件 {} 与当前程序 SDK 不匹配",plugin.manifest.id);
        for directory in [source.join("plugins"),source.join("plugins").join(&plugin.manifest.id),source_program.join("plugins"),plugin.directory.clone()] {
            let meta=fs::symlink_metadata(directory)?;
            ensure!(meta.is_dir()&&meta.file_attributes()&FILE_ATTRIBUTE_REPARSE_POINT==0,"插件目录不能是重解析点");
        }
        let library=regular(&plugin.directory.join(&plugin.manifest.library),256*1024*1024)?;
        ensure!(hex::encode(Sha256::digest(&library))==plugin.manifest.library_sha256.to_ascii_lowercase(),"插件 {} 完整性校验失败",plugin.manifest.id);
        let program=target_program.join("plugins").join(&plugin.manifest.id);
        let data=paths.config().at("plugins").join(&plugin.manifest.id);
        directories.extend([program.clone(),data.clone()]);
        total=total.checked_add(library.len()).context("plugin import size overflow")?;
        ensure!(total<=512*1024*1024,"插件同步超过 512 MiB");
        writes.push((program.join(&plugin.manifest.library),library));
        writes.push((program.join("plugin.json"),serde_json::to_vec_pretty(&plugin.manifest)?));
        for relative in &plugin.manifest.data_files {
            let from=source.join("plugins").join(&plugin.manifest.id).join(relative);
            if from.try_exists()? {let bytes=regular(&from,1024*1024)?;total+=bytes.len();ensure!(total<=512*1024*1024,"插件同步超过 512 MiB");writes.push((data.join(relative),bytes));}
        }
    }
    writes.push((paths.config().at(uu_core::plugins::Preferences::FILE),serde_json::to_vec_pretty(&preferences)?));
    // Validate every source before stopping a running host. Preserve old files
    // so a failed synchronization can restore its exact previous contents.
    let before=writes.iter().map(|(path,_)|Ok((path.clone(),if path.try_exists()? {Some(regular(path,256*1024*1024)?)}else{None}))).collect::<Result<Vec<_>>>()?;
    let was_running = status()? == SERVICE_RUNNING;
    if was_running {
        super::stop()?;
    }
    let result = (|| -> Result<()> {
        for path in directories {directory(&path)?;}
        for (path,bytes) in &writes {uu_core::plugin_backend::write_private_file(path,bytes)?;protect(path)?;}
        Ok(())
    })();
    if result.is_err() {
        for (path,bytes) in before.into_iter().rev() {
            if let Some(bytes)=bytes {uu_core::plugin_backend::write_private_file(&path,&bytes)?;protect(&path)?;}else if path.try_exists()? {fs::remove_file(path)?;}
        }
    }
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
            let reason = match service_admin::windows::request(Request::Status).await {
                Ok(response @ Response::Status { .. }) => return response,
                Ok(Response::Error { message }) => message,
                Ok(_) => "unexpected management response".into(),
                Err(error) => format!("{error:#}"),
            };
            assert!(
                Instant::now() < deadline,
                "owned background host did not become manageable: {reason}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires an elevated Windows console owner, a verified package, and completely absent service directories; never captures desktop pixels"]
    async fn protected_service_migrates_authorization_restarts_and_releases_owned_child() {
        administrator_file_owner().unwrap();
        assert_eq!(
            console_owner(unsafe { WTSGetActiveConsoleSessionId() }).unwrap(),
            process_user(unsafe { GetCurrentProcess() }).unwrap(),
            "fixture requires the logged-in console owner's identity"
        );
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
                book.sharing.plugin_services.clear();
                book.sharing.control = false;
                book.sharing.audio = false;
                book.sharing.clipboard = false;
                book.sharing.auto_start = true;
                Ok(())
            })
            .unwrap();
        install_from(source_data.clone(), package.clone(), false).unwrap();
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
        // Exercise a real OS activation failure after the protected manifest
        // and SCM command have changed, rather than a validation-only failure.
        let prior_manifest = fs::read(fixture.paths.root.join("service.json")).unwrap();
        let invalid_package = fixture.source.join("invalid-executable-package");
        fs::create_dir(&invalid_package).unwrap();
        let original:serde_json::Value=serde_json::from_slice(&fs::read(package.join("version.json")).unwrap()).unwrap();
        for name in uu_core::service_admin::package::executable_files(&original).unwrap() {
            fs::copy(package.join(name), invalid_package.join(name)).unwrap();
        }
        let invalid_bytes = b"owned fixture: intentionally invalid Windows executable";
        fs::write(invalid_package.join("eclipse-cli.exe"), invalid_bytes).unwrap();
        let mut invalid: serde_json::Value =
            serde_json::from_slice(&fs::read(package.join("version.json")).unwrap()).unwrap();
        invalid["source_commit"] = "1111111111111111111111111111111111111111".into();
        invalid["sha256"]["eclipse-cli.exe"] = hex::encode(Sha256::digest(invalid_bytes)).into();
        fs::write(
            invalid_package.join("version.json"),
            serde_json::to_vec(&invalid).unwrap(),
        )
        .unwrap();
        store
            .edit(|book| {
                book.sharing.auto_start = true;
                Ok(())
            })
            .unwrap();
        let error = install_from(source_data.clone(), invalid_package, false).unwrap_err();
        assert!(
            format!("{error:#}").contains("已恢复原服务状态"),
            "{error:#}"
        );
        assert_eq!(
            fs::read(fixture.paths.root.join("service.json")).unwrap(),
            prior_manifest
        );
        assert!(
            store.read().unwrap().sharing.auto_start,
            "failed activation changed the source profile"
        );
        let _ = inspect().await;
        install_from(source_data.clone(), package, false).unwrap();
        assert!(
            !store.read().unwrap().sharing.auto_start,
            "successful upgrades must disable frontend auto-start too"
        );
        let _ = inspect().await;
        super::super::supervisor::system_probe::run(&fixture.paths).unwrap();
        let _ = inspect().await;
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
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(
            status().unwrap(),
            SERVICE_STOPPED,
            "an explicit stop must not trigger recovery"
        );
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
        println!("owned System console host, protected ACLs, identity and authorization migration, failed activation rollback, successful upgrade, saved-only change, live restart, child stop and port release passed; desktop pixels were never captured");
    }
}
