//! Account-independent LAN discovery. Replies contain public metadata only.
//! Addresses come from the UDP sender, never an address supplied in JSON.
use anyhow::{ensure, Context, Result};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};
use tokio::{
    net::UdpSocket,
    sync::watch,
    time::{timeout_at, Instant as Deadline},
};

pub const SERVICE_PORT: u16 = 48998;
pub const DISCOVERY_PORT: u16 = 48999;
const MAGIC: &str = "eclipse-lan";
const MAX_PACKET: usize = 1200;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServerInfo {
    pub id: String,
    pub name: String,
    pub port: u16,
    /// SHA-256 of the public TLS certificate, not an authentication token.
    pub fingerprint: String,
}
impl ServerInfo {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            uuid::Uuid::parse_str(&self.id).is_ok() && self.id.len() <= 36,
            "invalid server identity"
        );
        ensure!(
            !self.name.trim().is_empty()
                && self.name.chars().count() <= 64
                && !self.name.chars().any(char::is_control),
            "invalid server name"
        );
        ensure!(
            self.port != 0
                && self.fingerprint.len() == 64
                && self.fingerprint.bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid server endpoint"
        );
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    service: String,
    version: u8,
    nonce: [u8; 16],
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    service: String,
    version: u8,
    nonce: [u8; 16],
    server: ServerInfo,
}
#[derive(Clone, Debug)]
pub struct Device {
    pub info: ServerInfo,
    pub address: SocketAddr,
}
fn local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || (ip.segments()[0] & 0xfe00 == 0xfc00)
                || (ip.segments()[0] & 0xffc0 == 0xfe80)
        }
    }
}

/// Serve a caller-owned socket. Enabling a server is explicit; discovery alone
/// does not start capture, input, pairing or a listening media session.
pub async fn respond(
    socket: UdpSocket,
    info: ServerInfo,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    info.validate()?;
    let mut bytes = [0u8; MAX_PACKET + 1];
    let mut recent = HashMap::<IpAddr, Instant>::new();
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        let (n, address) = tokio::select! {
            _=stop.changed()=>return Ok(()),
            received=socket.recv_from(&mut bytes)=>received?,
        };
        if n > 256 || !local(address.ip()) || address.port() == 0 {
            continue;
        }
        let Ok(query) = serde_json::from_slice::<Query>(&bytes[..n]) else {
            continue;
        };
        if query.service != MAGIC || query.version != 1 {
            continue;
        }
        let now = Instant::now();
        recent.retain(|_, at| now.duration_since(*at) < Duration::from_secs(1));
        if recent.len() >= 64
            || recent
                .get(&address.ip())
                .is_some_and(|at| now.duration_since(*at) < Duration::from_millis(100))
        {
            continue;
        }
        recent.insert(address.ip(), now);
        let reply = serde_json::to_vec(&Reply {
            service: MAGIC.into(),
            version: 1,
            nonce: query.nonce,
            server: info.clone(),
        })?;
        ensure!(reply.len() <= MAX_PACKET, "discovery reply exceeds limit");
        // A disappearing requester must not shut down the host's responder.
        let _ = socket.send_to(&reply, address).await;
    }
}

/// IPv4 broadcast by default; callers may include directed broadcasts for
/// multiple interfaces. Tests provide only an explicitly owned loopback socket.
pub async fn discover(targets: &[SocketAddr], wait: Duration) -> Result<Vec<Device>> {
    ensure!(
        !targets.is_empty() && targets.len() <= 32,
        "invalid discovery targets"
    );
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .context("cannot open LAN discovery socket")?;
    socket.set_broadcast(true)?;
    let mut nonce = [0; 16];
    OsRng.fill_bytes(&mut nonce);
    let query = serde_json::to_vec(&Query {
        service: MAGIC.into(),
        version: 1,
        nonce,
    })?;
    let mut sent = false;
    for address in targets {
        if address.is_ipv4() && address.port() != 0 {
            sent |= socket.send_to(&query, address).await.is_ok();
        }
    }
    ensure!(sent, "cannot send LAN discovery request");
    let deadline = Deadline::now() + wait.clamp(Duration::from_millis(100), Duration::from_secs(3));
    let mut bytes = [0; MAX_PACKET + 1];
    let mut found = HashMap::new();
    while let Ok(received) = timeout_at(deadline, socket.recv_from(&mut bytes)).await {
        let (n, address) = received?;
        if n > MAX_PACKET || !local(address.ip()) {
            continue;
        }
        let Ok(reply) = serde_json::from_slice::<Reply>(&bytes[..n]) else {
            continue;
        };
        if reply.service != MAGIC
            || reply.version != 1
            || reply.nonce != nonce
            || reply.server.validate().is_err()
        {
            continue;
        }
        // Public metadata is not trust. Connection code must still verify the
        // server certificate and the saved identity before sending credentials.
        let key = (reply.server.id.clone(), reply.server.fingerprint.clone());
        let address = SocketAddr::new(address.ip(), reply.server.port);
        if found.len() < 64 || found.contains_key(&key) {
            found.insert(
                key,
                Device {
                    info: reply.server,
                    address,
                },
            );
        }
    }
    let mut devices: Vec<_> = found.into_values().collect();
    devices.sort_by(|a, b| {
        a.info
            .name
            .cmp(&b.info.name)
            .then(a.info.id.cmp(&b.info.id))
    });
    Ok(devices)
}

/// Parse a manual endpoint without accepting URL credentials, paths or flags.
/// Hostname lookup happens later with its own bounded connection timeout.
pub fn endpoint(input: &str) -> Result<String> {
    let input = input.trim();
    ensure!(
        !input.is_empty()
            && input.len() <= 255
            && !input.chars().any(char::is_whitespace)
            && !input.contains(['/', '@', '\\']),
        "enter an IP address or hostname, optionally with a port"
    );
    if let Ok(ip) = input.parse::<IpAddr>() {
        ensure!(
            !ip.is_unspecified() && !ip.is_multicast(),
            "use a unicast host address"
        );
        return Ok(SocketAddr::new(ip, SERVICE_PORT).to_string());
    }
    if let Ok(address) = input.parse::<SocketAddr>() {
        ensure!(
            !address.ip().is_unspecified() && !address.ip().is_multicast() && address.port() != 0,
            "invalid host endpoint"
        );
        return Ok(address.to_string());
    }
    let (name, port) = if let Some((name, port)) = input.rsplit_once(':') {
        (name, port.parse::<u16>().context("invalid port")?)
    } else {
        (input, SERVICE_PORT)
    };
    ensure!(
        !name.is_empty()
            && name.len() <= 253
            && port != 0
            && name.split('.').all(|label| !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')),
        "invalid hostname"
    );
    Ok(format!("{name}:{port}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn info() -> ServerInfo {
        ServerInfo {
            id: "01234567-89ab-4cde-8fab-0123456789ab".into(),
            name: "Generated server".into(),
            port: SERVICE_PORT,
            fingerprint: "ab".repeat(32),
        }
    }
    #[tokio::test]
    async fn own_loopback_discovery_preserves_public_identity_and_sender_endpoint() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = socket.local_addr().unwrap();
        let expected = info();
        let (stop, rx) = watch::channel(false);
        let server = tokio::spawn(respond(socket, expected.clone(), rx));
        let devices = discover(&[target], Duration::from_millis(150))
            .await
            .unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].info, expected);
        assert_eq!(devices[0].address, "127.0.0.1:48998".parse().unwrap());
        stop.send_replace(true);
        server.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn wrong_nonce_unknown_protocol_and_duplicate_responses_do_not_poison_scan() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = socket.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut bytes = [0; 512];
            let (n, peer) = socket.recv_from(&mut bytes).await.unwrap();
            let q: Query = serde_json::from_slice(&bytes[..n]).unwrap();
            let mut reply = Reply {
                service: MAGIC.into(),
                version: 1,
                nonce: [99; 16],
                server: info(),
            };
            socket
                .send_to(&serde_json::to_vec(&reply).unwrap(), peer)
                .await
                .unwrap();
            reply.nonce = q.nonce;
            reply.version = 2;
            socket
                .send_to(&serde_json::to_vec(&reply).unwrap(), peer)
                .await
                .unwrap();
            reply.version = 1;
            for _ in 0..2 {
                socket
                    .send_to(&serde_json::to_vec(&reply).unwrap(), peer)
                    .await
                    .unwrap();
            }
        });
        let devices = discover(&[target], Duration::from_millis(150))
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(devices.len(), 1);
    }
    #[test]
    fn manual_endpoints_handle_ipv4_ipv6_hostnames_and_reject_urls_or_empty_ports() {
        for (input, expected) in [
            ("127.0.0.1", "127.0.0.1:48998"),
            ("127.0.0.1:43210", "127.0.0.1:43210"),
            ("::1", "[::1]:48998"),
            ("[::1]:43210", "[::1]:43210"),
            ("generated-host.local", "generated-host.local:48998"),
        ] {
            assert_eq!(endpoint(input).unwrap(), expected);
        }
        for invalid in [
            "",
            "http://localhost",
            "name@localhost",
            "localhost/stream",
            "localhost:0",
            "localhost:",
            "0.0.0.0",
            "224.0.0.1",
            "bad host",
            "-host.local",
            "a..local",
            "[::1]:0",
        ] {
            assert!(endpoint(invalid).is_err(), "{invalid}");
        }
        let mut bad = info();
        bad.name = "x\ny".into();
        assert!(bad.validate().is_err());
        bad = info();
        bad.fingerprint = "token".into();
        assert!(bad.validate().is_err());
    }
}
