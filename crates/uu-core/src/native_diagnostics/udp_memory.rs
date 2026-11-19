//! Linux SO_MEMINFO accounts for the entire peer's bound UDP mux (all protocols).
//! ICE's independent ephemeral and relay sockets are not exposed by that mux.
use super::*;
use std::sync::OnceLock;
use tokio::net::UdpSocket;

type Connection = dyn Conn + Send + Sync;

pub(super) struct Bound {
    socket: Weak<Connection>,
    family: &'static str,
}

impl Bound {
    pub(super) fn register<C: Conn + Send + Sync + 'static>(
        slot: &OnceLock<Self>,
        socket: &Arc<C>,
    ) {
        let Some(udp) = socket.as_any().downcast_ref::<UdpSocket>() else {
            return;
        };
        let Ok(address) = udp.local_addr() else {
            return;
        };
        let connection: Arc<Connection> = socket.clone();
        let _ = slot.set(Self {
            socket: Arc::downgrade(&connection),
            family: if address.is_ipv4() { "ipv4" } else { "ipv6" },
        });
    }

    pub(super) fn read(&self) -> std::result::Result<crate::media::UdpReceiveMemory, &'static str> {
        use std::os::fd::AsRawFd;

        let connection = self.socket.upgrade().ok_or("socket_retired")?;
        let socket = connection
            .as_any()
            .downcast_ref::<UdpSocket>()
            .ok_or("socket_unavailable")?;
        let mut words = [0u32; libc::SK_MEMINFO_DROPS as usize + 1];
        let mut length = std::mem::size_of_val(&words) as libc::socklen_t;
        // The upgraded owner keeps this exact FD alive across the call, even on teardown.
        let result = unsafe {
            libc::getsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_MEMINFO,
                words.as_mut_ptr().cast(),
                &mut length,
            )
        };
        if result != 0 || length as usize != std::mem::size_of_val(&words) {
            return Err("kernel_meminfo_unavailable");
        }
        Ok(crate::media::UdpReceiveMemory {
            family: self.family,
            allocated_bytes: words[libc::SK_MEMINFO_RMEM_ALLOC as usize],
            capacity_bytes: words[libc::SK_MEMINFO_RCVBUF as usize],
            drops: words[libc::SK_MEMINFO_DROPS as usize],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[tokio::test]
    async fn socket_memory_tracks_real_overflow_and_does_not_follow_port_reuse() {
        for ip in [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ] {
            let receiver = UdpSocket::bind((ip, 0)).await.unwrap();
            let address = receiver.local_addr().unwrap();
            let sender = UdpSocket::bind((ip, 0)).await.unwrap();
            let socket = socket2::SockRef::from(&receiver);
            socket.set_recv_buffer_size(4096).unwrap();
            let capacity = socket.recv_buffer_size().unwrap();
            let diagnostics = Diagnostics::new(None, false);
            assert_eq!(
                diagnostics.udp_kernel_sample()["unavailable"],
                "ice_managed_sockets"
            );
            // Hold the real production mux parameters, without starting a read worker.
            let params = udp_params(receiver, None, diagnostics.clone());
            let idle = diagnostics.udp_kernel_sample();
            assert_eq!(idle["sample"]["drops"], 0);
            assert_eq!(idle["sample"]["allocated_bytes"], 0);
            assert_eq!(idle["sample"]["capacity_bytes"], capacity);
            assert_eq!(
                idle["sample"]["family"],
                if ip.is_ipv4() { "ipv4" } else { "ipv6" }
            );

            for _ in 0..256 {
                sender.send_to(&[0; 1200], address).await.unwrap();
            }
            let loaded = diagnostics.udp_kernel_sample();
            assert!(loaded["sample"]["drops"].as_u64().unwrap() > 0, "{loaded}");
            assert!(loaded["sample"]["allocated_bytes"].as_u64().unwrap() > 0);
            assert_eq!(
                diagnostics.latest_udp_memory().unwrap().drops,
                loaded["sample"]["drops"]
            );

            drop(params);
            let next = UdpSocket::bind(address).await.unwrap();
            assert_eq!(
                diagnostics.udp_kernel_sample()["unavailable"],
                "socket_retired"
            );
            assert!(diagnostics.latest_udp_memory().is_none());
            let next_diagnostics = Diagnostics::new(None, false);
            let next_params = udp_params(next, None, next_diagnostics.clone());
            sender.send_to(b"baseline", address).await.unwrap();
            let healthy = next_diagnostics.udp_kernel_sample();
            assert_eq!(healthy["sample"]["drops"], 0);
            assert!(healthy["sample"]["allocated_bytes"].as_u64().unwrap() > 0);
            next_diagnostics.closing();
            assert!(next_diagnostics.latest_udp_memory().is_none());
            drop(next_params);
        }
    }
}
