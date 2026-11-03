//! One advertised, bounded port family for native streaming. The OS bind is
//! the allocation authority, including across processes and IPv4/IPv6.
use std::{io, net::{IpAddr, SocketAddr}};
use rand::Rng;

pub const FIRST: u16 = 49000;
pub const LAST: u16 = 49009;
pub const RANGE: &str = "49000-49009";

fn allocate<T>(first: u16, last: u16, mut bind: impl FnMut(u16) -> io::Result<T>) -> io::Result<T> {
    let count = last - first + 1;
    let start = rand::thread_rng().gen_range(0..count);
    for offset in 0..count {
        let port = first + (start + offset) % count;
        match bind(port) {
            Ok(socket) => return Ok(socket),
            Err(e) if matches!(e.kind(), io::ErrorKind::AddrInUse | io::ErrorKind::PermissionDenied) => {},
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AddrInUse,
        format!("streaming ports {first}-{last} are occupied or reserved; close unused streams or check port reservations")))
}
pub(crate) fn tcp(ip: IpAddr) -> io::Result<tokio::net::TcpListener> {
    allocate(FIRST, LAST, |port| crate::listen_socket::tcp(SocketAddr::new(ip, port)))
}
pub(crate) fn udp(ip: IpAddr) -> io::Result<tokio::net::UdpSocket> {
    allocate(FIRST, LAST, |port| crate::listen_socket::udp(SocketAddr::new(ip, port)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exhaustion_visits_every_port_without_falling_back() {
        let mut visited = std::collections::BTreeSet::new();
        let result = allocate(41000, 41003, |port| {
            assert!(visited.insert(port));
            Err::<(), _>(io::Error::from(io::ErrorKind::AddrInUse))
        });
        assert_eq!(visited.len(), 4);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AddrInUse);
        let e = allocate(41000, 41003, |_| Err::<(), _>(io::Error::from(io::ErrorKind::AddrNotAvailable)));
        assert_eq!(e.unwrap_err().kind(), io::ErrorKind::AddrNotAvailable);
    }
    #[tokio::test]
    async fn ten_occupied_ports_fail_then_released_slot_is_reused() {
        let ip="127.0.0.41".parse().unwrap();
        let mut sockets=Vec::new();
        for _ in FIRST..=LAST {sockets.push(udp(ip).unwrap());}
        assert_eq!(udp(ip).unwrap_err().kind(),io::ErrorKind::AddrInUse);
        let released=sockets.pop().unwrap().local_addr().unwrap();
        let replacement=udp(ip).unwrap();
        assert_eq!(replacement.local_addr().unwrap(),released);
        let mut tcp_sockets=Vec::new();
        for _ in FIRST..=LAST {tcp_sockets.push(tcp(ip).unwrap());}
        assert_eq!(tcp(ip).unwrap_err().kind(),io::ErrorKind::AddrInUse);
    }
    #[tokio::test]
    async fn concurrent_protocols_use_the_range_and_release_independently() {
        let ip = "127.0.0.1".parse().unwrap();
        let a = udp(ip).unwrap(); let b = udp(ip).unwrap();
        let tcp = tcp(ip).unwrap();
        for addr in [a.local_addr().unwrap(), b.local_addr().unwrap(), tcp.local_addr().unwrap()] {
            assert!((FIRST..=LAST).contains(&addr.port()));
        }
        assert_ne!(a.local_addr().unwrap(), b.local_addr().unwrap());
        let address = a.local_addr().unwrap(); drop(a);
        let _rebound = crate::listen_socket::udp(address).unwrap();
        b.send_to(b"live", address).await.unwrap();
        let mut data = [0; 8];
        assert_eq!(_rebound.recv_from(&mut data).await.unwrap().0, 4);
    }
}
