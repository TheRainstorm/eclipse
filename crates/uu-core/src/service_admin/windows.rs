//! Windows OS boundary for service management. Only elevated local
//! administrators and System can access the pipe. Clients verify System and
//! the protected installed executable before sending any pairing code.
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
        let mut descriptor = ptr::null_mut();
        winok(unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide("O:BAG:BAD:P(A;OICI;GA;;;SY)(A;OICI;GA;;;BA)").as_ptr(),
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
        "无法确定 Windows 服务目录（{result:#x}）"
    );
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
            signaling_host: uu_proto::SIGNALING_HOST.into(),
        }
    }
    pub fn manifest(&self) -> Result<Installed> {
        let bytes = std::fs::read(self.root.join("service.json"))?;
        ensure!(bytes.len() < 16 * 1024, "服务安装记录过大");
        let value: Installed = serde_json::from_slice(&bytes)?;
        value.validate()?;
        Ok(value)
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
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1
                && self.revision.len() == 40
                && self.revision.bytes().all(|v| v.is_ascii_hexdigit()),
            "服务安装记录版本无效"
        );
        ensure!(
            self.build.len() == 64 && self.build.bytes().all(|v| v.is_ascii_hexdigit()),
            "服务安装构建标识无效"
        );
        ensure!(
            self.owner_sid.starts_with("S-1-")
                && self.owner_sid.len() < 192
                && self
                    .owner_sid
                    .bytes()
                    .all(|v| v.is_ascii_digit() || matches!(v, b'S' | b'-')),
            "服务所有者标识无效"
        );
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
    ensure!(length > 0 && length < 64 * 1024, "无法读取进程身份");
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
        "请以管理员身份管理 Windows 后台服务"
    );
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
fn verify_server(handle: HANDLE) -> Result<()> {
    let mut pid = 0;
    winok(unsafe { GetNamedPipeServerProcessId(handle, &mut pid) })?;
    let process = Handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) });
    ensure!(!process.0.is_null(), "无法验证后台服务进程");
    ensure!(
        process_user(process.0)? == "S-1-5-18",
        "后台管理通路不是已安装的 System 服务"
    );
    let mut path = vec![0u16; 32768];
    let mut size = path.len() as u32;
    winok(unsafe { QueryFullProcessImageNameW(process.0, 0, path.as_mut_ptr(), &mut size) })?;
    let paths = Paths::discover()?;
    let installed = paths.manifest()?;
    let expected = paths
        .binary_directory(&installed)
        .join("uu.exe")
        .canonicalize()?;
    let actual = PathBuf::from(OsString::from_wide(&path[..size as usize])).canonicalize()?;
    ensure!(
        expected
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&actual.as_os_str().to_string_lossy()),
        "后台服务程序位置不符合安装记录"
    );
    Ok(())
}
pub async fn request(request: Request) -> Result<Response> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut pipe = loop {
            match ClientOptions::new().open(PIPE_NAME) {
                Ok(pipe) => break pipe,
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    tokio::time::sleep(Duration::from_millis(50)).await
                }
                Err(error) => return Err(error).context("后台服务尚未在当前用户控制台会话中启动"),
            }
        };
        verify_server(pipe.as_raw_handle() as _)?;
        super::write(&mut pipe, &request).await?;
        super::read(&mut pipe).await
    })
    .await
    .context("后台服务管理请求超时")?
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
    ensure!(!manager.0.is_null(), "无法访问 Windows 服务管理器");
    let service =
        ServiceHandle(unsafe { OpenServiceW(manager.0, wide(SERVICE_NAME).as_ptr(), access) });
    ensure!(
        !service.0.is_null(),
        "Windows 后台服务尚未安装或没有管理权限"
    );
    Ok(service)
}
pub fn status() -> Result<u32> {
    let service = open_service(SERVICE_QUERY_STATUS)?;
    let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
    winok(unsafe { QueryServiceStatus(service.0, &mut status) })?;
    Ok(status.dwCurrentState)
}
/// Run from a GUI worker, never the render thread. No shell is involved.
pub fn elevated(exe: &Path, arguments: &str) -> Result<()> {
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
    ensure!(!process.0.is_null(), "后台服务管理程序未启动");
    ensure!(
        unsafe { WaitForSingleObject(process.0, 180_000) } == WAIT_OBJECT_0,
        "后台服务管理程序仍在执行"
    );
    let mut code = 0;
    winok(unsafe { GetExitCodeProcess(process.0, &mut code) })?;
    ensure!(code == 0, "后台服务管理未成功（退出码 {code}）");
    Ok(())
}
