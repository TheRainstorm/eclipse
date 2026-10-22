//! Fixed service listeners own their requested interfaces and port. Windows
//! otherwise permits overlapping wildcard/specific binds under one account.
use socket2::{Domain, Protocol, Socket, Type};
use std::{io, net::SocketAddr};

fn socket(address: SocketAddr, kind: Type, protocol: Protocol) -> io::Result<Socket> {
    let socket = Socket::new(Domain::for_address(address), kind, Some(protocol))?;
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        use windows_sys::Win32::Networking::WinSock::{
            setsockopt, WSAGetLastError, SOCKET_ERROR, SOL_SOCKET, SO_EXCLUSIVEADDRUSE,
        };
        let exclusive = 1i32;
        // Set before bind. SO_REUSEADDR has different Windows semantics and
        // may silently exclude an already-owned interface from a wildcard.
        let result = unsafe {
            setsockopt(
                socket.as_raw_socket() as _,
                SOL_SOCKET,
                SO_EXCLUSIVEADDRUSE,
                (&exclusive as *const i32).cast(),
                std::mem::size_of_val(&exclusive) as i32,
            )
        };
        if result == SOCKET_ERROR {
            return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
        }
    }
    #[cfg(not(windows))]
    if kind == Type::STREAM {
        socket.set_reuse_address(true)?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&address.into())?;
    Ok(socket)
}
pub(crate) fn tcp(address: SocketAddr) -> io::Result<tokio::net::TcpListener> {
    let socket = socket(address, Type::STREAM, Protocol::TCP)?;
    socket.listen(128)?;
    tokio::net::TcpListener::from_std(socket.into())
}
pub(crate) fn udp(address: SocketAddr) -> io::Result<tokio::net::UdpSocket> {
    tokio::net::UdpSocket::from_std(socket(address, Type::DGRAM, Protocol::UDP)?.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, TcpListener, UdpSocket};
    #[tokio::test]
    async fn fixed_listeners_reject_overlapping_wildcard_and_interface_ports_in_both_directions() {
        let first = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = first.local_addr().unwrap().port();
        assert!(tcp((Ipv4Addr::UNSPECIFIED, port).into()).is_err());
        drop(first);
        let first = tcp((Ipv4Addr::UNSPECIFIED, port).into()).unwrap();
        assert!(TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err());
        drop(first);
        let first = tcp((Ipv4Addr::UNSPECIFIED, port).into()).unwrap();
        drop(first);
        let first = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = first.local_addr().unwrap().port();
        assert!(udp((Ipv4Addr::UNSPECIFIED, port).into()).is_err());
        drop(first);
        let first = udp((Ipv4Addr::UNSPECIFIED, port).into()).unwrap();
        assert!(UdpSocket::bind((Ipv4Addr::LOCALHOST, port)).is_err());
        drop(first);
        let first = udp((Ipv4Addr::UNSPECIFIED, port).into()).unwrap();
        drop(first);
    }
}
