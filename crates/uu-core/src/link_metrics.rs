//! On-demand ICMP and interface counters, owned by the connection monitor.
//! No endpoint or interface name leaves this module in the numeric snapshot.
use std::{
    net::IpAddr,
    sync::{Mutex, RwLock},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default)]
pub struct Snapshot {
    pub ping_ms: Option<f32>,
    pub rx_mbps: Option<f32>,
    pub tx_mbps: Option<f32>,
}
#[derive(Default)]
pub struct Metrics {
    requested: Mutex<Option<Instant>>,
    sample: RwLock<Option<(Instant, Snapshot)>>,
}
impl Metrics {
    pub fn request(&self) -> Option<Snapshot> {
        *self.requested.lock().unwrap() = Some(Instant::now());
        self.sample
            .read()
            .unwrap()
            .filter(|(at, _)| at.elapsed() < Duration::from_secs(3))
            .map(|(_, s)| s)
    }
    pub(crate) fn wanted(&self) -> bool {
        self.requested
            .lock()
            .unwrap()
            .is_some_and(|at| at.elapsed() < Duration::from_secs(3))
    }
    pub(crate) fn publish(&self, sample: Snapshot) {
        *self.sample.write().unwrap() = Some((Instant::now(), sample));
    }
}

#[derive(Default)]
pub(crate) struct Sampler {
    previous: Option<(String, Instant, [u64; 2])>,
}
impl Sampler {
    fn rates(&mut self, key: String, now: Instant, bytes: Option<[u64; 2]>) -> [Option<f32>; 2] {
        let previous = self.previous.take();
        let Some(bytes) = bytes else {
            return [None; 2];
        };
        self.previous = Some((key.clone(), now, bytes));
        let Some((old_key, at, old)) = previous else {
            return [None; 2];
        };
        let seconds = now.saturating_duration_since(at).as_secs_f64();
        if old_key != key || !(0.25..=3.).contains(&seconds) {
            return [None; 2];
        }
        [0, 1].map(|i| {
            bytes[i]
                .checked_sub(old[i])
                .map(|v| (v as f64 * 8. / seconds / 1_000_000.) as f32)
        })
    }
    pub(crate) fn sample(
        &mut self,
        path: Option<crate::connection_info::SelectedPath>,
    ) -> Snapshot {
        let Some(path) = path.filter(|p| !p.relayed()) else {
            self.previous = None;
            return Snapshot::default();
        };
        let local = path.local.base.unwrap_or(path.local.address).ip();
        let remote = path.remote.address.ip();
        let interface = if_addrs::get_if_addrs()
            .ok()
            .and_then(|v| v.into_iter().find(|v| v.ip() == local));
        let (key, bytes) = interface.map_or_else(
            || (String::new(), None),
            |i| {
                let bytes = interface_bytes(&i.name, i.index);
                (format!("{}:{:?}:{local}:{remote}", i.name, i.index), bytes)
            },
        );
        let [rx_mbps, tx_mbps] = self.rates(key, Instant::now(), bytes);
        let ping_ms = ping(local, remote);
        Snapshot {
            ping_ms,
            rx_mbps,
            tx_mbps,
        }
    }
}
#[cfg(target_os = "linux")]
fn interface_bytes(name: &str, _index: Option<u32>) -> Option<[u64; 2]> {
    if name.contains('/') || name == "." || name == ".." {
        return None;
    }
    let base = std::path::Path::new("/sys/class/net")
        .join(name)
        .join("statistics");
    Some([
        std::fs::read_to_string(base.join("rx_bytes"))
            .ok()?
            .trim()
            .parse()
            .ok()?,
        std::fs::read_to_string(base.join("tx_bytes"))
            .ok()?
            .trim()
            .parse()
            .ok()?,
    ])
}
#[cfg(windows)]
fn interface_bytes(_name: &str, index: Option<u32>) -> Option<[u64; 2]> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{GetIfEntry2, MIB_IF_ROW2};
    let mut row: MIB_IF_ROW2 = unsafe { std::mem::zeroed() };
    row.InterfaceIndex = index?;
    (unsafe { GetIfEntry2(&mut row) } == 0).then_some([row.InOctets, row.OutOctets])
}
#[cfg(not(any(windows, target_os = "linux")))]
fn interface_bytes(_: &str, _: Option<u32>) -> Option<[u64; 2]> {
    None
}

#[cfg(target_os = "linux")]
fn ping(local: IpAddr, remote: IpAddr) -> Option<f32> {
    use socket2::{Domain, Protocol, SockAddr, Socket, Type};
    use std::{mem::MaybeUninit, net::SocketAddr};
    let v4 = local.is_ipv4();
    if v4 != remote.is_ipv4() {
        return None;
    }
    // Datagram ICMP obeys ping_group_range; never request raw-socket privilege.
    let socket = Socket::new(
        if v4 { Domain::IPV4 } else { Domain::IPV6 },
        Type::DGRAM,
        Some(if v4 {
            Protocol::ICMPV4
        } else {
            Protocol::ICMPV6
        }),
    )
    .ok()?;
    socket
        .bind(&SockAddr::from(SocketAddr::new(local, 0)))
        .ok()?;
    socket
        .connect(&SockAddr::from(SocketAddr::new(remote, 0)))
        .ok()?;
    socket
        .set_read_timeout(Some(Duration::from_millis(200)))
        .ok()?;
    socket
        .set_write_timeout(Some(Duration::from_millis(200)))
        .ok()?;
    let mut request = [0u8; 24];
    request[0] = if v4 { 8 } else { 128 };
    request[7] = 1;
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut request[8..]);
    if v4 {
        let sum = request
            .chunks_exact(2)
            .map(|v| u32::from(u16::from_be_bytes([v[0], v[1]])))
            .sum::<u32>();
        let sum = (sum & 0xffff) + (sum >> 16);
        let checksum = !((sum & 0xffff) + (sum >> 16)) as u16;
        request[2..4].copy_from_slice(&checksum.to_be_bytes());
    }
    let start = Instant::now();
    if socket.send(&request).ok()? != request.len() {
        return None;
    }
    let mut buffer = [MaybeUninit::<u8>::uninit(); 256];
    let n = socket.recv(&mut buffer).ok()?;
    let reply = unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), n) };
    (n == request.len()
        && reply[0] == if v4 { 0 } else { 129 }
        && reply[1] == 0
        && reply[6..] == request[6..])
        .then(|| start.elapsed().as_secs_f32() * 1000.)
}
#[cfg(windows)]
fn ping(local: IpAddr, remote: IpAddr) -> Option<f32> {
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE, NetworkManagement::IpHelper::*, Networking::WinSock::*,
    };
    let v4 = local.is_ipv4();
    if v4 != remote.is_ipv4() {
        return None;
    }
    if matches!(local,IpAddr::V6(v) if v.is_unicast_link_local())
        || matches!(remote,IpAddr::V6(v) if v.is_unicast_link_local())
    {
        return None;
    }
    let handle = unsafe {
        if v4 {
            IcmpCreateFile()
        } else {
            Icmp6CreateFile()
        }
    };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }
    let request = [0x75u8; 16];
    let mut reply = [0u64; 64];
    let count = match (local, remote) {
        (IpAddr::V4(local), IpAddr::V4(remote)) => unsafe {
            IcmpSendEcho2Ex(
                handle,
                std::ptr::null_mut(),
                None,
                std::ptr::null(),
                u32::from_ne_bytes(local.octets()),
                u32::from_ne_bytes(remote.octets()),
                request.as_ptr().cast(),
                request.len() as u16,
                std::ptr::null(),
                reply.as_mut_ptr().cast(),
                std::mem::size_of_val(&reply) as u32,
                200,
            )
        },
        (IpAddr::V6(local), IpAddr::V6(remote)) => {
            let address = |ip: std::net::Ipv6Addr| {
                let mut v: SOCKADDR_IN6 = unsafe { std::mem::zeroed() };
                v.sin6_family = AF_INET6;
                v.sin6_addr.u.Byte = ip.octets();
                v
            };
            unsafe {
                Icmp6SendEcho2(
                    handle,
                    std::ptr::null_mut(),
                    None,
                    std::ptr::null(),
                    &address(local),
                    &address(remote),
                    request.as_ptr().cast(),
                    request.len() as u16,
                    std::ptr::null(),
                    reply.as_mut_ptr().cast(),
                    std::mem::size_of_val(&reply) as u32,
                    200,
                )
            }
        }
        _ => 0,
    };
    unsafe { IcmpCloseHandle(handle) };
    if count == 0 {
        return None;
    }
    let (status, rtt) = if v4 {
        let r = unsafe { &*reply.as_ptr().cast::<ICMP_ECHO_REPLY>() };
        (r.Status, r.RoundTripTime)
    } else {
        let r = unsafe { &*reply.as_ptr().cast::<ICMPV6_ECHO_REPLY_LH>() };
        (r.Status, r.RoundTripTime)
    };
    (status == 0).then_some(rtt as f32)
}
#[cfg(not(any(windows, target_os = "linux")))]
fn ping(_: IpAddr, _: IpAddr) -> Option<f32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "explicit local ICMP capability check; no privilege changes"]
    fn icmp_on_owned_loopback() {
        #[cfg(target_os="linux")]
        if let Err(error)=socket2::Socket::new(socket2::Domain::IPV4,socket2::Type::DGRAM,Some(socket2::Protocol::ICMPV4)) {
            if error.kind()==std::io::ErrorKind::PermissionDenied {
                assert!(ping("127.0.0.1".parse().unwrap(),"127.0.0.1".parse().unwrap()).is_none());
                eprintln!("datagram ICMP denied by host policy; unknown result verified without privilege changes");return;
            }
        }
        let v4 = ping("127.0.0.1".parse().unwrap(), "127.0.0.1".parse().unwrap());
        assert!(
            v4.is_some_and(|v| v.is_finite() && v >= 0.),
            "IPv4 loopback ICMP unavailable"
        );
        let v6 = ping("::1".parse().unwrap(), "::1".parse().unwrap());
        assert!(
            v6.is_some_and(|v| v.is_finite() && v >= 0.),
            "IPv6 loopback ICMP unavailable"
        );
    }
    #[test]
    fn throughput_uses_actual_interval_and_resets_on_route_counter_or_sample_gap() {
        let mut s = Sampler::default();
        let now = Instant::now();
        assert_eq!(s.rates("a".into(), now, Some([0, 0])), [None; 2]);
        assert_eq!(
            s.rates(
                "a".into(),
                now + Duration::from_secs(2),
                Some([2_000_000, 500_000])
            ),
            [Some(8.), Some(2.)]
        );
        assert_eq!(
            s.rates(
                "b".into(),
                now + Duration::from_secs(3),
                Some([3_000_000, 900_000])
            ),
            [None; 2]
        );
        assert_eq!(
            s.rates("b".into(), now + Duration::from_secs(4), Some([0, 0])),
            [None; 2]
        );
        assert_eq!(
            s.rates("b".into(), now + Duration::from_secs(10), Some([99, 99])),
            [None; 2]
        );
        assert_eq!(
            s.rates("b".into(), now + Duration::from_secs(11), None),
            [None; 2]
        );
    }
    #[test]
    fn no_selected_path_never_probes_or_reuses_a_previous_rate() {
        assert_eq!(Sampler::default().sample(None).ping_ms, None);
        let metrics = Metrics::default();
        assert!(!metrics.wanted());
        assert!(metrics.request().is_none());
        assert!(metrics.wanted());
    }
}
