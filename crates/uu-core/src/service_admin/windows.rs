//! Windows management boundaries: an owner data pipe in the SCM process and
//! an internal administrator/System pipe in the protected console media child.
//! Both clients verify their OS peer before sending pairing credentials.
use super::{Request, Response};
use anyhow::{ensure, Context, Result};
use std::{
    ffi::{OsStr, OsString},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::AsRawHandle,
    },
    path::{Path, PathBuf},
    ptr,
    time::Duration,
};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};
use windows_sys::{
    core::GUID,
    Win32::{
        Foundation::*,
        Security::{Authorization::*, *},
        System::{
            Com::CoTaskMemFree, Pipes::GetNamedPipeServerProcessId, Services::*, Threading::*,
        },
        UI::Shell::*,
    },
};

pub const SERVICE_NAME: &str = "EclipseRemoteHost";
pub const PIPE_NAME: &str = r"\\.\pipe\EclipseRemoteHost.admin.v1";
/// The SCM process, rather than the console media child, owns this endpoint.
pub const OWNER_PIPE_NAME: &str = r"\\.\pipe\EclipseRemoteHost.owner.v1";
const OWNER_PIPE_CLIENT_ACCESS:u32=0x0012019b;
#[path="process_query_windows.rs"]
mod process_query;
pub use process_query::enable_running_version_query;
pub fn configure_owner_control(service:&ServiceHandle,installed:&Installed)->Result<()> {process_query::configure_owner_control(service,installed)}
pub fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}
pub fn winok(value: i32) -> Result<()> {
    if value == 0 {
        Err(std::io::Error::last_os_error().into())
    } else {
        Ok(())
    }
}
pub struct Handle(pub HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
pub struct Descriptor(pub PSECURITY_DESCRIPTOR);
impl Descriptor {
    pub fn administrators() -> Result<Self> {
        Self::from_sddl("O:BAG:BAD:P(A;OICI;GA;;;SY)(A;OICI;GA;;;BA)")
    }
    fn from_sddl(sddl: &str) -> Result<Self> {
        let mut descriptor = ptr::null_mut();
        winok(unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide(sddl).as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        })?;
        Ok(Self(descriptor))
    }
    pub fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
fn folder(id: &GUID) -> Result<PathBuf> {
    let mut value = ptr::null_mut();
    let result = unsafe { SHGetKnownFolderPath(id, 0, ptr::null_mut(), &mut value) };
    ensure!(
        result >= 0 && !value.is_null(),
        "{}", uu_i18n::format!("无法确定 Windows 服务目录（{result:#x}）"));
    let mut length = 0;
    unsafe {
        while *value.add(length) != 0 {
            length += 1;
        }
    }
    let path = unsafe {
        PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(
            value, length,
        )))
    };
    unsafe {
        CoTaskMemFree(value.cast());
    }
    Ok(path)
}
#[derive(Clone)]
pub struct Paths {
    pub root: PathBuf,
    pub programs: PathBuf,
}
impl Paths {
    pub fn discover() -> Result<Self> {
        Ok(Self {
            root: folder(&FOLDERID_ProgramData)?.join("EclipseRemoteHost"),
            programs: folder(&FOLDERID_ProgramFiles)?.join("EclipseRemoteHost"),
        })
    }
    pub fn config(&self) -> crate::Config {
        crate::Config {
            data_dir: self.root.join("data/uu-remote"),
            state_dir: self.root.join("state/uu-remote"),
            signaling_host: String::new(),
        }
    }
    pub fn manifest(&self) -> Result<Installed> {
        let bytes = std::fs::read(self.root.join("service.json"))?;
        ensure!(bytes.len() < 16 * 1024, "{}", uu_i18n::format!("服务安装记录过大"));
        let value: Installed = serde_json::from_slice(&bytes)?;
        value.validate()?;
        Ok(value)
    }
    pub fn save_manifest(&self, value: &Installed) -> Result<()> {
        value.validate()?;
        crate::private_file::write_atomic(
            &self.root.join("service.json"),
            &serde_json::to_vec(value)?,
        )?;
        Ok(())
    }
    pub fn binary_directory(&self, installed: &Installed) -> PathBuf {
        self.programs
            .join("versions")
            .join(format!("{}-{}", installed.revision, installed.build))
    }
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Installed {
    pub version: u32,
    pub owner_sid: String,
    pub revision: String,
    pub build: String,
}
impl Installed {
    pub fn cli_name(&self)->&'static str {if self.version>=2 {"eclipse-cli.exe"}else{"uu.exe"}}
    pub fn gui_name(&self)->&'static str {if self.version>=2 {"eclipse.exe"}else{"uu-gui.exe"}}
    pub fn validate(&self) -> Result<()> {
        ensure!(
            matches!(self.version,1|2)
                && self.revision.len() == 40
                && self.revision.bytes().all(|v| v.is_ascii_hexdigit()),
            "{}", uu_i18n::format!("服务安装记录版本无效"));
        ensure!(
            self.build.len() == 64 && self.build.bytes().all(|v| v.is_ascii_hexdigit()),
            "{}", uu_i18n::format!("服务安装构建标识无效"));
        ensure!(
            self.owner_sid.starts_with("S-1-")
                && self.owner_sid.len() < 192
                && self
                    .owner_sid
                    .bytes()
                    .all(|v| v.is_ascii_digit() || matches!(v, b'S' | b'-')),
            "{}", uu_i18n::format!("服务所有者标识无效"));
        Ok(())
    }
}
pub fn sid_string(sid: PSID) -> Result<String> {
    let mut value = ptr::null_mut();
    winok(unsafe { ConvertSidToStringSidW(sid, &mut value) })?;
    let mut length = 0;
    unsafe {
        while *value.add(length) != 0 {
            length += 1;
        }
    }
    let sid = unsafe { String::from_utf16_lossy(std::slice::from_raw_parts(value, length)) };
    unsafe {
        LocalFree(value.cast());
    }
    Ok(sid)
}
pub fn token_user(token: HANDLE) -> Result<String> {
    let mut length = 0;
    unsafe {
        GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut length);
    }
    ensure!(length > 0 && length < 64 * 1024, "{}", uu_i18n::format!("无法读取进程身份"));
    let mut storage = vec![
        0usize;
        (length as usize + std::mem::size_of::<usize>() - 1)
            / std::mem::size_of::<usize>()
    ];
    winok(unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            storage.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    })?;
    let user = unsafe { &*storage.as_ptr().cast::<TOKEN_USER>() };
    sid_string(user.User.Sid)
}

pub fn process_user(process: HANDLE) -> Result<String> {
    let mut token = ptr::null_mut();
    winok(unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) })?;
    let token = Handle(token);
    token_user(token.0)
}
pub fn require_administrator() -> Result<()> {
    let mut token = ptr::null_mut();
    winok(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) })?;
    let token = Handle(token);
    let mut elevation: TOKEN_ELEVATION = unsafe { std::mem::zeroed() };
    let mut length = 0;
    winok(unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut length,
        )
    })?;
    ensure!(
        elevation.TokenIsElevated != 0,
        "{}", uu_i18n::format!("请以管理员身份管理 Windows 后台服务"));
    Ok(())
}
/// Service installer/manager processes create protected configuration on behalf
/// of Administrators, so a filtered token of the same user cannot change its ACL.
pub fn administrator_file_owner() -> Result<()> {
    require_administrator()?;
    let mut sid = ptr::null_mut();
    winok(unsafe { ConvertStringSidToSidW(wide("S-1-5-32-544").as_ptr(), &mut sid) })?;
    let result = (|| {
        let mut token = ptr::null_mut();
        winok(unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY | TOKEN_ADJUST_DEFAULT,
                &mut token,
            )
        })?;
        let token = Handle(token);
        let owner = TOKEN_OWNER { Owner: sid };
        winok(unsafe {
            SetTokenInformation(
                token.0,
                TokenOwner,
                (&owner as *const TOKEN_OWNER).cast(),
                std::mem::size_of_val(&owner) as u32,
            )
        })
    })();
    unsafe {
        LocalFree(sid);
    }
    result
}
pub fn pipe(first: bool) -> Result<NamedPipeServer> {
    let descriptor = Descriptor::administrators()?;
    let attributes = descriptor.attributes();
    Ok(unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .max_instances(8)
            .create_with_security_attributes_raw(
                PIPE_NAME,
                (&attributes as *const SECURITY_ATTRIBUTES)
                    .cast_mut()
                    .cast(),
            )
    }?)
}
pub fn owner_pipe(first: bool, installed: &Installed) -> Result<NamedPipeServer> {
    installed.validate()?;
    // Individual client rights deliberately exclude FILE_CREATE_PIPE_INSTANCE.
    // GENERIC_WRITE would also grant that server right on a named pipe.
    let descriptor = owner_pipe_descriptor(&installed.owner_sid)?;
    let attributes = descriptor.attributes();
    Ok(unsafe { ServerOptions::new().first_pipe_instance(first)
        .reject_remote_clients(true).max_instances(8)
        .create_with_security_attributes_raw(OWNER_PIPE_NAME,
            (&attributes as *const SECURITY_ATTRIBUTES).cast_mut().cast()) }?)
}
fn owner_pipe_descriptor(owner:&str)->Result<Descriptor> {
    Descriptor::from_sddl(&format!(
        "O:SYG:SYD:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;{OWNER_PIPE_CLIENT_ACCESS:#x};;;{owner})S:(ML;;NW;;;ME)"
    ))
}

fn verify_owner_server(handle: HANDLE) -> Result<()> {
    let mut pid = 0;
    winok(unsafe { GetNamedPipeServerProcessId(handle, &mut pid) })?;
    let service = open_service(SERVICE_QUERY_STATUS)?;
    let state = service_process(service.0)?;
    ensure!(pid != 0 && state.dwProcessId == pid && state.dwCurrentState == SERVICE_RUNNING,
        "后台管理通路不是已安装的 Windows 服务");
    Ok(())
}

/// Ordinary GUI clients never open the protected configuration or a System token.
/// SCM binds the pipe peer to the actual service process before sending secrets.
pub async fn request_owner(request: Request) -> Result<Response> {
    use windows_sys::Win32::Storage::FileSystem::*;
    let mut stream = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let handle = unsafe { CreateFileW(wide(OWNER_PIPE_NAME).as_ptr(), OWNER_PIPE_CLIENT_ACCESS, 0,
                ptr::null(), OPEN_EXISTING, FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                ptr::null_mut()) };
            if handle != INVALID_HANDLE_VALUE {
                let handle = Handle(handle);
                verify_owner_server(handle.0)?;
                // from_raw_handle takes ownership, including registration failure.
                let raw = handle.0;
                std::mem::forget(handle);
                return unsafe { tokio::net::windows::named_pipe::NamedPipeClient::from_raw_handle(raw) }
                    .map_err(anyhow::Error::from);
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_PIPE_BUSY as i32) {
                return Err(error).context("后台服务未启动或尚未更新，请在服务管理中启动或更新");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }).await.context("后台服务连接超时")??;
    let mutation = request.is_mutation();
    let result = async {
        super::write(&mut stream, &request).await?;
        let response:Response=if mutation {
            super::read(&mut stream).await?
        } else {
            tokio::time::timeout(Duration::from_secs(100),super::read(&mut stream)).await
                .context("后台服务响应超时，请刷新确认结果")??
        };
        match response {
            Response::SubmittedUnknown{message}=>Err(super::SubmittedUnknown(message).into()),
            response=>Ok(response),
        }
    }.await;
    if mutation { result.map_err(|error| super::SubmittedUnknown(format!("{error:#}")).into()) }
    else { result }
}
fn verify_server(handle: HANDLE) -> Result<()> {
    let mut pid = 0;
    winok(unsafe { GetNamedPipeServerProcessId(handle, &mut pid) })?;
    let process = Handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) });
    ensure!(!process.0.is_null(), "{}", uu_i18n::format!("无法验证后台服务进程"));
    ensure!(
        process_user(process.0)? == "S-1-5-18",
        uu_i18n::text("后台管理通路不是已安装的 System 服务")
    );
    let mut path = vec![0u16; 32768];
    let mut size = path.len() as u32;
    winok(unsafe { QueryFullProcessImageNameW(process.0, 0, path.as_mut_ptr(), &mut size) })?;
    let paths = Paths::discover()?;
    let installed = paths.manifest()?;
    let expected = paths
        .binary_directory(&installed)
        .join(installed.cli_name())
        .canonicalize()?;
    let actual = PathBuf::from(OsString::from_wide(&path[..size as usize])).canonicalize()?;
    ensure!(
        expected
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&actual.as_os_str().to_string_lossy()),
        "{}", uu_i18n::format!("后台服务程序位置不符合安装记录"));
    Ok(())
}
pub async fn request(request: Request) -> Result<Response> {
    let mutation = request.is_mutation();
    let mut pipe = tokio::time::timeout(Duration::from_secs(5), async {
        let mut pipe = loop {
            match ClientOptions::new().open(PIPE_NAME) {
                Ok(pipe) => break pipe,
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    tokio::time::sleep(Duration::from_millis(50)).await
                }
                Err(error) => return Err(error).context(uu_i18n::text("后台服务尚未在当前用户控制台会话中启动")),
            }
        };
        verify_server(pipe.as_raw_handle() as _)?;
        Ok::<_, anyhow::Error>(pipe)
    })
    .await
    .context(uu_i18n::text("后台服务管理请求超时"))??;
    let submitted = tokio::time::timeout(Duration::from_secs(5), super::write(&mut pipe, &request))
        .await.context(uu_i18n::text("后台服务管理请求超时"))
        .and_then(|result| result);
    if mutation {
        // A cancelled/failed write may already have delivered the complete
        // frame. Inspect before retrying even if write_all/flush did not return.
        submitted.map_err(|error| super::SubmittedUnknown(format!("{error:#}")))?;
    } else {
        submitted?;
    }
    // Once accepted, mutations run to an explicit result. Dropping a timeout
    // cannot cancel blocking OS work or a durable configuration write, and
    // must not encourage the frontend to replay a still-running operation.
    if mutation {
        super::read(&mut pipe).await.map_err(|error| super::SubmittedUnknown(format!("{error:#}")).into())
    } else {
        tokio::time::timeout(Duration::from_secs(5), super::read(&mut pipe))
            .await.context(uu_i18n::text("后台服务管理请求超时"))?
    }
}
pub struct ServiceHandle(pub SC_HANDLE);
impl Drop for ServiceHandle {
    fn drop(&mut self) {
        unsafe {
            CloseServiceHandle(self.0);
        }
    }
}
pub fn open_service(access: u32) -> Result<ServiceHandle> {
    let manager =
        ServiceHandle(unsafe { OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_CONNECT) });
    if manager.0.is_null() {
        return Err(std::io::Error::last_os_error()).context(uu_i18n::text("无法访问 Windows 服务管理器"));
    }
    let service =
        ServiceHandle(unsafe { OpenServiceW(manager.0, wide(SERVICE_NAME).as_ptr(), access) });
    if service.0.is_null() {
        return Err(std::io::Error::last_os_error())
            .context(uu_i18n::text("Windows 后台服务尚未安装或没有管理权限"));
    }
    Ok(service)
}
pub fn status() -> Result<u32> {
    let service = open_service(SERVICE_QUERY_STATUS)?;
    let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
    winok(unsafe { QueryServiceStatus(service.0, &mut status) })?;
    Ok(status.dwCurrentState)
}
/// The installer grants only start/stop/query rights to the configured owner.
/// Starting here does not read host data; the System service prepares itself.
pub fn control_owner(start: bool) -> Result<()> {
    let service = open_service(SERVICE_QUERY_STATUS | if start { SERVICE_START } else { SERVICE_STOP })?;
    let target=if start {SERVICE_RUNNING}else{SERVICE_STOPPED};
    if status()?==target {return Ok(());}
    if start {winok(unsafe{StartServiceW(service.0,0,ptr::null())})?;}
    else {
        let mut state: SERVICE_STATUS=unsafe{std::mem::zeroed()};
        winok(unsafe{ControlService(service.0,SERVICE_CONTROL_STOP,&mut state)})?;
    }
    let deadline=std::time::Instant::now()+Duration::from_secs(95);
    while status()?!=target {
        ensure!(std::time::Instant::now()<deadline,"后台服务仍在切换状态，请刷新确认");
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}
/// Read-only display metadata from SCM and the running supervisor image. This
/// is not an authorization check and never relaxes the protected file ACLs.
#[derive(Clone, serde::Serialize)]
pub struct BuildVersion {
    pub revision:String,
}
impl BuildVersion {
    pub fn label(&self)->String {
        self.revision[..12].into()
    }
}
#[derive(Clone, serde::Serialize)]
pub struct BuildStatus {
    pub state:u32,
    pub process_id:u32,
    pub installed:Option<BuildVersion>,
    pub running:Option<BuildVersion>,
    pub running_error:Option<String>,
}
fn image_build(paths:&Paths,image:&Path)->Option<BuildVersion> {
    if !matches!(image.file_name()?.to_str()?,"eclipse-cli.exe"|"uu.exe") {return None;}
    let directory=image.parent()?;
    let relative=directory.strip_prefix(paths.programs.join("versions")).ok()?;
    if relative.components().count()!=1 {return None;}
    let value=relative.to_str()?;
    let (revision,build)=value.split_once('-')?;
    if revision.len()!=40 || build.len()!=64 || !revision.bytes().chain(build.bytes()).all(|c|c.is_ascii_hexdigit()) {return None;}
    Some(BuildVersion{revision:revision.into()})
}
fn service_process(service:SC_HANDLE)->Result<SERVICE_STATUS_PROCESS> {
    let mut value:SERVICE_STATUS_PROCESS=unsafe{std::mem::zeroed()};
    let mut length=0;
    winok(unsafe{QueryServiceStatusEx(service,SC_STATUS_PROCESS_INFO,(&mut value as *mut SERVICE_STATUS_PROCESS).cast(),std::mem::size_of_val(&value) as u32,&mut length)})?;
    Ok(value)
}
pub fn build_status()->Result<BuildStatus> {
    let paths=Paths::discover()?;
    let service=open_service(SERVICE_QUERY_STATUS|SERVICE_QUERY_CONFIG)?;
    let state=service_process(service.0)?;
    let mut length=0;
    unsafe{QueryServiceConfigW(service.0,ptr::null_mut(),0,&mut length);}
    ensure!(length>0 && length<=8192,"Invalid service configuration size");
    let mut storage=vec![0usize;(length as usize+std::mem::size_of::<usize>()-1)/std::mem::size_of::<usize>()];
    let config=storage.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>();
    winok(unsafe{QueryServiceConfigW(service.0,config,length,&mut length)})?;
    let command=unsafe{(*config).lpBinaryPathName};
    let base=storage.as_ptr() as usize;
    let end=base+storage.len()*std::mem::size_of::<usize>();
    ensure!(!command.is_null() && (command as usize)>=base && (command as usize)<end,"Invalid service binary path");
    let capacity=(end-command as usize)/2;
    let command=unsafe{std::slice::from_raw_parts(command,capacity)};
    let command=&command[..command.iter().position(|c|*c==0).ok_or_else(||anyhow::anyhow!("Unterminated service binary path"))?];
    let mut argc=0;
    let argv=unsafe{CommandLineToArgvW(wide(OsString::from_wide(command)).as_ptr(),&mut argc)};
    ensure!(!argv.is_null(),"Invalid service command line");
    let installed=if argc>0 {
        let first=unsafe{*argv};let mut n=0;
        unsafe{while *first.add(n)!=0 {n+=1;}}
        image_build(&paths,&PathBuf::from(unsafe{OsString::from_wide(std::slice::from_raw_parts(first,n))}))
    } else {None};
    unsafe{LocalFree(argv.cast());}
    let mut running_error=None;
    let running=if state.dwProcessId!=0 && state.dwCurrentState!=SERVICE_STOPPED {
        let result=(||->Result<Option<BuildVersion>> {
            let process=Handle(unsafe{OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION,0,state.dwProcessId)});
            ensure!(!process.0.is_null(),"{}",std::io::Error::last_os_error());
            let mut image=vec![0u16;32768];let mut length=image.len() as u32;
            winok(unsafe{QueryFullProcessImageNameW(process.0,0,image.as_mut_ptr(),&mut length)})?;
            let mut exit_code=0;
            winok(unsafe{GetExitCodeProcess(process.0,&mut exit_code)})?;
            let after=service_process(service.0)?;
            ensure!(after.dwProcessId==state.dwProcessId && after.dwCurrentState==state.dwCurrentState
                && exit_code==STILL_ACTIVE as u32,"Service changed during version query");
            Ok(image_build(&paths,&PathBuf::from(OsString::from_wide(&image[..length as usize]))))
        })();
        match result {Ok(build)=>build,Err(error)=>{running_error=Some(error.to_string());None}}
    }else{None};
    Ok(BuildStatus{state:state.dwCurrentState,process_id:state.dwProcessId,installed,running,running_error})
}
/// Firewall repair must bind the exact protected executable, including build
/// directory and legacy filename, rather than comparing the revision label.
pub fn firewall_program(paths:&Paths,installed:&Installed)->Result<PathBuf> {
    let expected=paths.binary_directory(installed).join(installed.cli_name());
    let service=open_service(SERVICE_QUERY_STATUS)?;
    let state=service_process(service.0)?;
    if state.dwProcessId==0 || state.dwCurrentState==SERVICE_STOPPED {return Ok(expected);}
    let process=Handle(unsafe{OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION,0,state.dwProcessId)});
    ensure!(!process.0.is_null(),"cannot verify running service image: {}",std::io::Error::last_os_error());
    let mut image=vec![0u16;32768];let mut length=image.len() as u32;
    winok(unsafe{QueryFullProcessImageNameW(process.0,0,image.as_mut_ptr(),&mut length)})?;
    let actual=PathBuf::from(OsString::from_wide(&image[..length as usize]));
    ensure!(service_process(service.0)?.dwProcessId==state.dwProcessId,"service restarted during firewall inspection");
    ensure!(actual.canonicalize()?==expected.canonicalize()?,"running service image differs from the protected installation; firewall was not modified");
    Ok(actual)
}

#[cfg(test)]
mod build_version_tests {
    use super::*;
    #[tokio::test]
    async fn management_rejects_a_pipe_owned_by_a_process_other_than_the_scm_service() {
        let name=format!(r"\\.\pipe\eclipse-owned-peer-test-{}",uuid::Uuid::new_v4());
        let server=ServerOptions::new().first_pipe_instance(true).reject_remote_clients(true).create(&name).unwrap();
        let client=ClientOptions::new().open(&name).unwrap();
        assert!(verify_owner_server(client.as_raw_handle() as _).is_err());
        drop(client);drop(server);
    }
    #[test]
    fn owner_pipe_grants_data_access_without_instance_creation_or_acl_control() {
        use windows_sys::Win32::Storage::FileSystem::*;
        let own=process_user(unsafe{GetCurrentProcess()}).unwrap();
        let owner=if own=="S-1-5-18"{"S-1-0-0"}else{&own};
        let descriptor=owner_pipe_descriptor(owner).unwrap();
        let mut sid=ptr::null_mut();
        winok(unsafe{ConvertStringSidToSidW(wide(owner).as_ptr(),&mut sid)}).unwrap();
        let _sid=Descriptor(sid);
        let mut original=ptr::null_mut();
        winok(unsafe{OpenProcessToken(GetCurrentProcess(),TOKEN_QUERY|TOKEN_DUPLICATE,&mut original)}).unwrap();
        let original=Handle(original);
        let restriction=SID_AND_ATTRIBUTES{Sid:sid,Attributes:0};
        let mut restricted=ptr::null_mut();
        winok(unsafe{CreateRestrictedToken(original.0,DISABLE_MAX_PRIVILEGE,0,ptr::null(),0,ptr::null(),1,&restriction,&mut restricted)}).unwrap();
        let restricted=Handle(restricted);
        let mut token=ptr::null_mut();
        winok(unsafe{DuplicateTokenEx(restricted.0,TOKEN_QUERY,ptr::null(),SecurityImpersonation,TokenImpersonation,&mut token)}).unwrap();
        let token=Handle(token);
        let mapping=GENERIC_MAPPING{GenericRead:FILE_GENERIC_READ,GenericWrite:FILE_GENERIC_WRITE,GenericExecute:FILE_GENERIC_EXECUTE,GenericAll:FILE_ALL_ACCESS};
        for (rights,expected) in [(OWNER_PIPE_CLIENT_ACCESS,true),(0x4,false),(WRITE_DAC,false),(WRITE_OWNER,false)] {
            let mut mapping=mapping;
            let mut privileges=[0usize;64];let mut length=std::mem::size_of_val(&privileges) as u32;
            let mut granted=0;let mut allowed=0;
            winok(unsafe{AccessCheck(descriptor.0,token.0,rights,&mut mapping,privileges.as_mut_ptr().cast(),&mut length,&mut granted,&mut allowed)}).unwrap();
            assert_eq!(allowed!=0,expected,"unexpected owner pipe access {rights:#x}");
        }
    }
    #[test]
    fn service_build_metadata_requires_a_complete_version_directory_and_known_image() {
        let paths=Paths{root:std::env::temp_dir().join("generated-service-data"),programs:std::env::temp_dir().join("generated-service-programs")};
        let revision="a".repeat(40);let build="b".repeat(64);
        let directory=paths.programs.join("versions").join(format!("{revision}-{build}"));
        for image in ["eclipse-cli.exe","uu.exe"] {
            let value=image_build(&paths,&directory.join(image)).unwrap();
            assert_eq!(value.revision,revision);assert_eq!(value.label(),"a".repeat(12));
        }
        for image in [directory.join("eclipse.exe"),directory.join("nested/eclipse-cli.exe"),
            paths.programs.join("versions/development/eclipse-cli.exe"),
            paths.programs.join("other").join(format!("{revision}-{build}/eclipse-cli.exe"))] {
            assert!(image_build(&paths,&image).is_none());
        }
    }
}
/// Run from a GUI worker, never the render thread. No shell is involved.
pub fn elevated(exe: &Path, arguments: &str) -> Result<()> {
    let code=elevated_exit_code(exe,arguments)?;
    ensure!(code == 0, "{}", uu_i18n::format!("后台服务管理未成功（退出码 {code}）"));
    Ok(())
}
/// Some fixed local helpers return a small status in their exit code. They
/// still use the same UAC/process-handle boundary, never a writable result path.
pub fn elevated_exit_code(exe:&Path,arguments:&str)->Result<u32> {
    let verb = wide("runas");
    let file = wide(exe.as_os_str());
    let arguments = wide(arguments);
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of_val(&info) as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = arguments.as_ptr();
    info.nShow = 1;
    winok(unsafe { ShellExecuteExW(&mut info) })?;
    let process = Handle(info.hProcess);
    ensure!(!process.0.is_null(), "{}", uu_i18n::format!("后台服务管理程序未启动"));
    if unsafe { WaitForSingleObject(process.0, 180_000) } != WAIT_OBJECT_0 {
        return Err(super::SubmittedUnknown(uu_i18n::text("后台服务管理程序仍在执行").into()).into());
    }
    let mut code = 0;
    winok(unsafe { GetExitCodeProcess(process.0, &mut code) })?;
    Ok(code)
}
