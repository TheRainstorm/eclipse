//! Test-only ownership of the original kernel socket. A closed ephemeral port
//! can be reused by another parallel test; that is not a leak of this socket.
use super::*;
use std::{os::fd::AsRawFd, sync::{OnceLock, Weak}};

#[derive(Clone)]
pub(crate) struct Original {
    address: SocketAddr,
    fd: i32,
    inode: u64,
}
#[derive(Clone)]
pub(crate) struct Observer {
    original: Original,
    mux: Weak<UDPMuxDefault>,
}
fn observers() -> &'static Mutex<Vec<Observer>> {
    static OBSERVERS: OnceLock<Mutex<Vec<Observer>>> = OnceLock::new();
    OBSERVERS.get_or_init(Default::default)
}
pub(crate) fn capture(socket: &UdpSocket) -> Result<Original> {
    let fd = socket.as_raw_fd();
    let target = std::fs::read_link(format!("/proc/self/fd/{fd}"))?;
    let target = target.to_str().context("non-UTF8 socket FD link")?;
    let inode = target.strip_prefix("socket:[").and_then(|value| value.strip_suffix(']'))
        .context("UDP FD is not a kernel socket")?.parse()?;
    Ok(Original { address: socket.local_addr()?, fd, inode })
}
pub(crate) fn record(original: Original, mux: &Arc<UDPMuxDefault>) {
    let mut observers = observers().lock().unwrap();
    observers.retain(|observer| observer.mux.strong_count() != 0);
    observers.push(Observer { original, mux: Arc::downgrade(mux) });
}
pub(crate) fn observe(address: SocketAddr) -> Option<Observer> {
    observers().lock().unwrap().iter().rev().find(|observer| observer.original.address == address
        && observer.mux.strong_count() != 0).cloned()
}
impl Observer {
    fn original_fds(&self) -> std::io::Result<Vec<String>> {
        let target = format!("socket:[{}]", self.original.inode);
        let mut fds = Vec::new();
        for entry in std::fs::read_dir("/proc/self/fd")? {
            let entry = entry?;
            match std::fs::read_link(entry.path()) {
                Ok(path) if path.to_string_lossy() == target => fds.push(entry.file_name().to_string_lossy().into_owned()),
                Ok(_) => {},
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => return Err(error),
            }
        }
        Ok(fds)
    }
    pub(crate) fn retired(&self) -> bool {
        self.mux.strong_count() == 0 && self.original_fds().is_ok_and(|fds| fds.is_empty())
    }
    pub(crate) fn failure(&self) -> serde_json::Value {
        let port = format!(":{:04X}", self.original.address.port());
        let rows: Vec<_> = ["/proc/self/net/udp", "/proc/self/net/udp6"].into_iter()
            .filter_map(|path| std::fs::read_to_string(path).ok())
            .flat_map(|table| table.lines().skip(1).map(str::to_owned).collect::<Vec<_>>())
            .filter(|row| row.split_whitespace().nth(1).is_some_and(|address| address.ends_with(&port)))
            .collect();
        serde_json::json!({"address":self.original.address.to_string(),"original_fd":self.original.fd,
            "original_inode":self.original.inode,"mux_strong":self.mux.strong_count(),
            "original_fd_now":std::fs::read_link(format!("/proc/self/fd/{}",self.original.fd)).ok().map(|path|path.to_string_lossy().into_owned()),
            "original_inode_fds":self.original_fds().map_err(|error|error.to_string()),"same_port_rows":rows})
    }
}
