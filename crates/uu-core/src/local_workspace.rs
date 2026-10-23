//! Bounded, authenticated local handoff to a live streaming workspace. Only an
//! ephemeral capability is stored; connection requests and passwords stay in RAM.
use anyhow::{ensure, Result};
use fs2::FileExt;
use ring::{
    hmac,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

const MAX_MESSAGE: usize = 64 * 1024;
const MAGIC: &[u8; 8] = b"ECWS0001";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Endpoint {
    port: u16,
    pid: u32,
    token: [u8; 32],
}

pub enum Join {
    Attached,
    Owner(Owner),
}
pub struct Request {
    pub bytes: Zeroizing<Vec<u8>>,
    reply: mpsc::SyncSender<bool>,
}
impl Request {
    pub fn finish(self, accepted: bool) {
        let _ = self.reply.try_send(accepted);
    }
}
pub struct Owner {
    requests: mpsc::Receiver<Request>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    wake: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>,
    directory: PathBuf,
    _lock: fs::File,
}
impl Owner {
    pub fn try_recv(&self) -> Option<Request> {
        self.requests.try_recv().ok()
    }
    pub fn set_waker(&self, wake: impl Fn() + Send + Sync + 'static) {
        *self.wake.lock().unwrap() = Some(Arc::new(wake));
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
        let _ = fs::remove_file(self.directory.join("endpoint.json"));
    }
}

/// The first process owns the listener until all its stream windows close.
/// Concurrent starts wait for that listener rather than creating duplicates.
pub fn join(directory: &Path, bytes: &[u8]) -> Result<Join> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_MESSAGE,
        "本地串流请求过大"
    );
    private_directory(directory)?;
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(directory.join("workspace.lock"))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match FileExt::try_lock_exclusive(&lock) {
            Ok(()) => return owner(directory, lock).map(Join::Owner),
            Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {}
            Err(e) => return Err(e.into()),
        }
        if let Ok(endpoint) = crate::device_trust::read_private(&directory.join("endpoint.json"))
            .and_then(|bytes| Ok(serde_json::from_slice::<Endpoint>(&bytes)?))
        {
            match attach(&endpoint, bytes) {
                Ok(()) => return Ok(Join::Attached),
                Err(error) if Instant::now() >= deadline => return Err(error),
                Err(_) => {}
            }
        }
        ensure!(
            Instant::now() < deadline,
            "串流窗口正在启动或退出，请稍后重试"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn nonce() -> Result<[u8; 32]> {
    let mut value = [0; 32];
    SystemRandom::new()
        .fill(&mut value)
        .map_err(|_| anyhow::anyhow!("无法创建本地串流随机数"))?;
    Ok(value)
}
fn transcript(side: u8, client: &[u8; 32], server: &[u8; 32], bytes: &[u8]) -> Zeroizing<Vec<u8>> {
    let mut value = Zeroizing::new(Vec::with_capacity(65 + bytes.len()));
    value.push(side);
    value.extend(client);
    value.extend(server);
    value.extend(bytes);
    value
}
fn socket(stream: &TcpStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(750)))?;
    stream.set_write_timeout(Some(Duration::from_millis(750)))?;
    stream.set_nodelay(true)?;
    Ok(())
}
fn attach(endpoint: &Endpoint, bytes: &[u8]) -> Result<()> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(500))?;
    socket(&stream)?;
    let key = hmac::Key::new(hmac::HMAC_SHA256, &endpoint.token);
    let client = nonce()?;
    stream.write_all(MAGIC)?;
    stream.write_all(&client)?;
    let mut server = [0; 32];
    let mut proof = [0; 32];
    stream.read_exact(&mut server)?;
    stream.read_exact(&mut proof)?;
    hmac::verify(&key, &transcript(0, &client, &server, &[]), &proof)
        .map_err(|_| anyhow::anyhow!("本地串流窗口身份验证失败"))?;
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(bytes)?;
    stream.write_all(hmac::sign(&key, &transcript(1, &client, &server, bytes)).as_ref())?;
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(endpoint.pid);
    }
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut response = [0];
    stream.read_exact(&mut response)?;
    ensure!(response == [1], "串流窗口未接受连接请求");
    Ok(())
}
fn owner(directory: &Path, lock: fs::File) -> Result<Owner> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let token = nonce()?;
    let endpoint = Endpoint {
        port: listener.local_addr()?.port(),
        pid: std::process::id(),
        token,
    };
    crate::private_file::write_atomic(
        &directory.join("endpoint.json"),
        &serde_json::to_vec(&endpoint)?,
    )?;
    let (send, requests) = mpsc::sync_channel(8);
    let stop = Arc::new(AtomicBool::new(false));
    let wake: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>> = Default::default();
    let stopped = stop.clone();
    let notifier = wake.clone();
    let thread = std::thread::Builder::new()
        .name("stream-workspace".into())
        .spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, address)) if address.ip().is_loopback() => {
                        let _ = receive(&mut stream, &token, &send, &notifier, &stopped);
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::park_timeout(Duration::from_millis(20))
                    }
                    Err(_) => break,
                }
            }
        })?;
    Ok(Owner {
        requests,
        stop,
        thread: Some(thread),
        wake,
        directory: directory.into(),
        _lock: lock,
    })
}
fn receive(
    stream: &mut TcpStream,
    token: &[u8; 32],
    send: &mpsc::SyncSender<Request>,
    wake: &Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    stop: &AtomicBool,
) -> Result<()> {
    socket(stream)?;
    let mut magic = [0; 8];
    let mut client = [0; 32];
    stream.read_exact(&mut magic)?;
    ensure!(&magic == MAGIC, "本地协议不匹配");
    stream.read_exact(&mut client)?;
    let server = nonce()?;
    let key = hmac::Key::new(hmac::HMAC_SHA256, token);
    stream.write_all(&server)?;
    stream.write_all(hmac::sign(&key, &transcript(0, &client, &server, &[])).as_ref())?;
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    ensure!(length > 0 && length <= MAX_MESSAGE, "本地串流请求过大");
    let mut bytes = Zeroizing::new(vec![0; length]);
    stream.read_exact(&mut bytes)?;
    let mut proof = [0; 32];
    stream.read_exact(&mut proof)?;
    hmac::verify(&key, &transcript(1, &client, &server, &bytes), &proof)
        .map_err(|_| anyhow::anyhow!("本地请求身份验证失败"))?;
    let (reply, receipt) = mpsc::sync_channel(1);
    send.try_send(Request { bytes, reply })
        .map_err(|_| anyhow::anyhow!("本地串流请求队列已满"))?;
    if let Some(wake) = wake.lock().unwrap().clone() {
        wake();
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    while !stop.load(Ordering::Acquire) && Instant::now() < deadline {
        match receipt.recv_timeout(Duration::from_millis(50)) {
            Ok(accepted) => {
                stream.write_all(&[u8::from(accepted)])?;
                return Ok(());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
    }
    anyhow::bail!("本地串流请求已取消")
}

fn private_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        let meta = fs::symlink_metadata(path)?;
        ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "串流工作区目录无效"
        );
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::{
            Foundation::LocalFree,
            Security::{
                Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW,
                SetFileSecurityW, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
            },
        };
        fs::create_dir_all(path)?;
        let meta = fs::symlink_metadata(path)?;
        ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "串流工作区目录无效"
        );
        // Owner Rights only, inherited by the private endpoint and lock files.
        // https://learn.microsoft.com/windows/win32/secauthz/security-descriptor-string-format
        let descriptor: Vec<u16> = "D:P(A;OICI;FA;;;OW)\0".encode_utf16().collect();
        let mut raw = std::ptr::null_mut();
        ensure!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    descriptor.as_ptr(),
                    1,
                    &mut raw,
                    std::ptr::null_mut(),
                )
            } != 0,
            "无法保护本地串流目录"
        );
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let ok = unsafe {
            SetFileSecurityW(
                wide.as_ptr(),
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                raw,
            )
        };
        unsafe {
            LocalFree(raw);
        }
        ensure!(
            ok != 0,
            "无法设置本地串流目录权限：{}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            Self(
                std::env::temp_dir()
                    .join(format!("eclipse-workspace-test-{}", uuid::Uuid::new_v4())),
            )
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn request(owner: &Owner) -> Request {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(request) = owner.try_recv() {
                return request;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn simultaneous_launch_attaches_only_after_the_live_workspace_accepts() {
        let directory = Directory::new();
        let owner = match join(&directory.0, b"first").unwrap() {
            Join::Owner(owner) => owner,
            _ => panic!("first owner"),
        };
        let path = directory.0.clone();
        let pending = std::thread::spawn(move || {
            join(&path, b"ephemeral-generated-assistance-secret").unwrap()
        });
        let message = request(&owner);
        assert_eq!(&message.bytes[..], b"ephemeral-generated-assistance-secret");
        assert!(!pending.is_finished());
        message.finish(true);
        assert!(matches!(pending.join().unwrap(), Join::Attached));
        let endpoint = fs::read(directory.0.join("endpoint.json")).unwrap();
        assert!(!String::from_utf8(endpoint)
            .unwrap()
            .contains("ephemeral-generated"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&directory.0).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(directory.0.join("endpoint.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        drop(owner);
        assert!(!directory.0.join("endpoint.json").exists());
        assert!(matches!(
            join(&directory.0, b"next").unwrap(),
            Join::Owner(_)
        ));
    }
    #[test]
    fn invalid_capability_never_delivers_a_request_and_ui_can_reject_an_attachment() {
        let directory = Directory::new();
        let owner = match join(&directory.0, b"first").unwrap() {
            Join::Owner(owner) => owner,
            _ => panic!("first owner"),
        };
        let mut endpoint: Endpoint =
            serde_json::from_slice(&fs::read(directory.0.join("endpoint.json")).unwrap()).unwrap();
        endpoint.token[0] ^= 1;
        assert!(attach(&endpoint, b"must-not-arrive")
            .unwrap_err()
            .to_string()
            .contains("身份验证"));
        assert!(owner.try_recv().is_none());
        endpoint.token[0] ^= 1;
        let task = std::thread::spawn(move || attach(&endpoint, b"rejected"));
        request(&owner).finish(false);
        assert!(task
            .join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("未接受"));
        assert!(join(&directory.0, &vec![0; MAX_MESSAGE + 1]).is_err());
    }
}
