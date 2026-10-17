//! TURN-over-TLS transport used by the UU keyboard channel.
//!
//! UU advertises a `turns:` server in the room response, but the WebRTC
//! library currently ignores that URL.  The keyboard PCK stream uses the
//! same server directly: allocate a relay, create a permission for the
//! controlled peer, then put PCK frames in TURN Send/Data Indications.

use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use md5::{Digest, Md5};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use sha1::Sha1;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

const MAGIC_COOKIE: u32 = 0x2112_A442;
const ALLOCATE: u16 = 0x0003;
const ALLOCATE_SUCCESS: u16 = 0x0103;
const CREATE_PERMISSION: u16 = 0x0008;
const CREATE_PERMISSION_SUCCESS: u16 = 0x0108;
const SEND_INDICATION: u16 = 0x0016;
const DATA_INDICATION: u16 = 0x0017;
const USERNAME: u16 = 0x0006;
const MESSAGE_INTEGRITY: u16 = 0x0008;
const ERROR_CODE: u16 = 0x0009;
const LIFETIME: u16 = 0x000d;
const XOR_PEER_ADDRESS: u16 = 0x0012;
const DATA: u16 = 0x0013;
const REALM: u16 = 0x0014;
const NONCE: u16 = 0x0015;
const XOR_RELAYED_ADDRESS: u16 = 0x0016;
const REQUESTED_TRANSPORT: u16 = 0x0019;
const FINGERPRINT: u16 = 0x8028;

type HmacSha1 = Hmac<Sha1>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnServer {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub credential: String,
}

impl TurnServer {
    pub fn parse(url: &str, username: &str, credential: &str) -> Option<Self> {
        let rest = url.strip_prefix("turns:")?;
        let authority = rest.split('?').next()?.trim();
        let (host, port) = if let Some(v) = authority.strip_prefix('[') {
            let end = v.find(']')?;
            let host = v[..end].to_string();
            let port = v[end + 1..]
                .strip_prefix(':')
                .unwrap_or("443")
                .parse()
                .ok()?;
            (host, port)
        } else {
            let (h, p) = authority.rsplit_once(':').unwrap_or((authority, "443"));
            (h.to_string(), p.parse().ok()?)
        };
        if host.is_empty() || username.is_empty() || credential.is_empty() {
            return None;
        }
        Some(Self {
            host,
            port,
            username: username.into(),
            credential: credential.into(),
        })
    }
}

/// Extract TURN-over-TLS URLs from the room `iceServers` JSON value.
pub fn servers_from_json(v: &serde_json::Value) -> Vec<TurnServer> {
    let Some(items) = v.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        let user = item["username"].as_str().unwrap_or_default();
        let credential = item["credential"].as_str().unwrap_or_default();
        let urls: Vec<String> = match &item["urls"] {
            serde_json::Value::String(s) => vec![s.clone()],
            serde_json::Value::Array(a) => a
                .iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        };
        for url in urls {
            if let Some(s) = TurnServer::parse(&url, user, credential) {
                if !out.contains(&s) {
                    out.push(s);
                }
            }
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct TurnMessage {
    pub method: u16,
    pub transaction_id: [u8; 12],
    pub attrs: Vec<(u16, Vec<u8>)>,
}

impl TurnMessage {
    fn attr(&self, kind: u16) -> Option<&[u8]> {
        self.attrs
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, v)| v.as_slice())
    }
    fn error_code(&self) -> Option<(u16, String)> {
        let v = self.attr(ERROR_CODE)?;
        if v.len() < 4 {
            return None;
        }
        let code = ((v[2] & 7) as u16) * 100 + v[3] as u16;
        Some((code, String::from_utf8_lossy(&v[4..]).into_owned()))
    }
}

#[derive(Debug, Clone)]
pub struct Allocation {
    pub relayed_addr: Option<SocketAddr>,
    pub lifetime: Option<u32>,
}

#[derive(Debug)]
struct NoVerify;

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub struct TurnClient {
    rd: tokio::io::ReadHalf<TlsStream<TcpStream>>,
    wr: tokio::io::WriteHalf<TlsStream<TcpStream>>,
    server: TurnServer,
    realm: Vec<u8>,
    nonce: Vec<u8>,
    key: [u8; 16],
    peers: Vec<SocketAddr>,
}

impl TurnClient {
    pub async fn connect(server: TurnServer) -> Result<Self> {
        let tcp = TcpStream::connect((server.host.as_str(), server.port))
            .await
            .with_context(|| format!("connect TURN {}:{}", server.host, server.port))?;
        let mut cfg = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify))
            .with_no_client_auth();
        cfg.alpn_protocols.clear();
        let connector = TlsConnector::from(Arc::new(cfg));
        let name = ServerName::try_from(server.host.clone()).context("invalid TURN SNI")?;
        let tls = connector
            .connect(name, tcp)
            .await
            .context("TURN TLS handshake")?;
        let (rd, wr) = tokio::io::split(tls);
        let mut out = Self {
            rd,
            wr,
            server,
            realm: Vec::new(),
            nonce: Vec::new(),
            key: [0; 16],
            peers: Vec::new(),
        };
        out.refresh_challenge().await?;
        Ok(out)
    }

    async fn refresh_challenge(&mut self) -> Result<()> {
        let msg = build_message(
            ALLOCATE,
            &[(REQUESTED_TRANSPORT, vec![17, 0, 0, 0])],
            None,
            false,
        );
        self.write_message(&msg).await?;
        let response = self.read_message().await?;
        let (code, reason) = response
            .error_code()
            .ok_or_else(|| anyhow::anyhow!("TURN Allocate challenge missing error"))?;
        if code != 401 {
            bail!("TURN Allocate challenge: {code} {reason}");
        }
        self.realm = response.attr(REALM).unwrap_or_default().to_vec();
        self.nonce = response.attr(NONCE).unwrap_or_default().to_vec();
        if self.realm.is_empty() || self.nonce.is_empty() {
            bail!("TURN 401 missing realm or nonce");
        }
        self.key = auth_key(&self.server.username, &self.realm, &self.server.credential);
        Ok(())
    }

    fn adopt_challenge(&mut self, response: &TurnMessage) -> bool {
        let Some(realm) = response.attr(REALM) else {
            return false;
        };
        let Some(nonce) = response.attr(NONCE) else {
            return false;
        };
        self.realm = realm.to_vec();
        self.nonce = nonce.to_vec();
        self.key = auth_key(&self.server.username, &self.realm, &self.server.credential);
        true
    }

    pub async fn allocate(&mut self) -> Result<Allocation> {
        let attrs = vec![
            (REQUESTED_TRANSPORT, vec![17, 0, 0, 0]),
            (USERNAME, self.server.username.as_bytes().to_vec()),
            (REALM, self.realm.clone()),
            (NONCE, self.nonce.clone()),
        ];
        self.write_message(&build_message(ALLOCATE, &attrs, Some(&self.key), true))
            .await?;
        let mut response = self.read_message().await?;
        if response.method != ALLOCATE_SUCCESS
            && response.error_code().map(|(c, _)| c) == Some(438)
            && self.adopt_challenge(&response)
        {
            let retry = vec![
                (REQUESTED_TRANSPORT, vec![17, 0, 0, 0]),
                (USERNAME, self.server.username.as_bytes().to_vec()),
                (REALM, self.realm.clone()),
                (NONCE, self.nonce.clone()),
            ];
            self.write_message(&build_message(ALLOCATE, &retry, Some(&self.key), true))
                .await?;
            response = self.read_message().await?;
        }
        if response.method != ALLOCATE_SUCCESS {
            let (c, r) = response
                .error_code()
                .unwrap_or((0, "unknown response".into()));
            bail!("TURN Allocate failed: {c} {r}");
        }
        Ok(Allocation {
            relayed_addr: response
                .attr(XOR_RELAYED_ADDRESS)
                .and_then(|v| decode_xor_address(v)),
            lifetime: response
                .attr(LIFETIME)
                .and_then(|v| v.get(..4))
                .map(|v| u32::from_be_bytes(v.try_into().unwrap())),
        })
    }

    pub async fn create_permission(&mut self, peer: SocketAddr) -> Result<()> {
        let attrs = vec![
            (XOR_PEER_ADDRESS, encode_xor_address(peer)),
            (USERNAME, self.server.username.as_bytes().to_vec()),
            (REALM, self.realm.clone()),
            (NONCE, self.nonce.clone()),
        ];
        self.write_message(&build_message(
            CREATE_PERMISSION,
            &attrs,
            Some(&self.key),
            true,
        ))
        .await?;
        let mut response = self.read_message().await?;
        if response.method != CREATE_PERMISSION_SUCCESS
            && response.error_code().map(|(c, _)| c) == Some(438)
            && self.adopt_challenge(&response)
        {
            let retry = vec![
                (XOR_PEER_ADDRESS, encode_xor_address(peer)),
                (USERNAME, self.server.username.as_bytes().to_vec()),
                (REALM, self.realm.clone()),
                (NONCE, self.nonce.clone()),
            ];
            self.write_message(&build_message(
                CREATE_PERMISSION,
                &retry,
                Some(&self.key),
                true,
            ))
            .await?;
            response = self.read_message().await?;
        }
        if response.method != CREATE_PERMISSION_SUCCESS {
            let (c, r) = response
                .error_code()
                .unwrap_or((0, "unknown response".into()));
            bail!("TURN CreatePermission failed: {c} {r}");
        }
        if !self.peers.contains(&peer) {
            self.peers.push(peer);
        }
        Ok(())
    }

    pub async fn send(&mut self, peer: SocketAddr, data: &[u8]) -> Result<()> {
        let attrs = vec![
            (XOR_PEER_ADDRESS, encode_xor_address(peer)),
            (DATA, data.to_vec()),
        ];
        self.write_message(&build_message(SEND_INDICATION, &attrs, None, false))
            .await
    }

    pub async fn send_to_permission(&mut self, data: &[u8]) -> Result<()> {
        anyhow::ensure!(!self.peers.is_empty(), "TURN permission not established");
        let peers = self.peers.clone();
        for peer in peers {
            self.send(peer, data).await?;
        }
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<Option<(SocketAddr, Vec<u8>)>> {
        loop {
            let m = self.read_message().await?;
            if m.method != DATA_INDICATION {
                continue;
            }
            let Some(peer) = m.attr(XOR_PEER_ADDRESS).and_then(decode_xor_address) else {
                continue;
            };
            let Some(data) = m.attr(DATA) else { continue };
            return Ok(Some((peer, data.to_vec())));
        }
    }

    async fn write_message(&mut self, msg: &[u8]) -> Result<()> {
        self.wr.write_all(msg).await?;
        self.wr.flush().await?;
        Ok(())
    }

    async fn read_message(&mut self) -> Result<TurnMessage> {
        let mut head = [0u8; 20];
        self.rd
            .read_exact(&mut head)
            .await
            .context("read TURN header")?;
        let len = u16::from_be_bytes([head[2], head[3]]) as usize;
        if len > 65507 {
            bail!("invalid TURN message length {len}");
        }
        let mut body = vec![0u8; len];
        self.rd
            .read_exact(&mut body)
            .await
            .context("read TURN body")?;
        parse_message(&head, &body)
    }
}

fn auth_key(username: &str, realm: &[u8], password: &str) -> [u8; 16] {
    let mut h = Md5::new();
    h.update(username.as_bytes());
    h.update(b":");
    h.update(realm);
    h.update(b":");
    h.update(password.as_bytes());
    h.finalize().into()
}

fn attr(kind: u16, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + value.len() + 3);
    out.extend_from_slice(&kind.to_be_bytes());
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
    while out.len() % 4 != 0 {
        out.push(0);
    }
    out
}

fn build_message(
    method: u16,
    attrs: &[(u16, Vec<u8>)],
    key: Option<&[u8; 16]>,
    fingerprint: bool,
) -> Vec<u8> {
    let mut txid = [0u8; 12];
    let u = uuid::Uuid::new_v4();
    txid.copy_from_slice(&u.as_bytes()[..12]);
    let mut body = attrs
        .iter()
        .flat_map(|(k, v)| attr(*k, v))
        .collect::<Vec<_>>();
    let mi_len = if key.is_some() { 24 } else { 0 };
    let mut msg = Vec::with_capacity(20 + body.len() + mi_len + 8);
    msg.extend_from_slice(&method.to_be_bytes());
    msg.extend_from_slice(&((body.len() + mi_len) as u16).to_be_bytes());
    msg.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
    msg.extend_from_slice(&txid);
    msg.append(&mut body);
    if let Some(key) = key {
        let mut mac = HmacSha1::new_from_slice(key).expect("HMAC key");
        mac.update(&msg);
        msg.extend(attr(MESSAGE_INTEGRITY, &mac.finalize().into_bytes()));
    }
    if fingerprint {
        let len = msg.len() - 20 + 8;
        msg[2..4].copy_from_slice(&(len as u16).to_be_bytes());
        let crc = crc32fast::hash(&msg) ^ 0x5354_554e;
        msg.extend(attr(FINGERPRINT, &crc.to_be_bytes()));
    }
    msg
}

fn parse_message(head: &[u8; 20], body: &[u8]) -> Result<TurnMessage> {
    if u32::from_be_bytes(head[4..8].try_into().unwrap()) != MAGIC_COOKIE {
        bail!("bad TURN magic cookie");
    }
    let len = u16::from_be_bytes([head[2], head[3]]) as usize;
    if len != body.len() {
        bail!("TURN length mismatch");
    }
    let mut attrs = Vec::new();
    let mut p = 0;
    while p + 4 <= body.len() {
        let kind = u16::from_be_bytes([body[p], body[p + 1]]);
        let n = u16::from_be_bytes([body[p + 2], body[p + 3]]) as usize;
        p += 4;
        if p + n > body.len() {
            bail!("truncated TURN attribute");
        }
        attrs.push((kind, body[p..p + n].to_vec()));
        p += n;
        while p % 4 != 0 {
            p += 1;
        }
    }
    Ok(TurnMessage {
        method: u16::from_be_bytes([head[0], head[1]]),
        transaction_id: head[8..20].try_into().unwrap(),
        attrs,
    })
}

fn encode_xor_address(addr: SocketAddr) -> Vec<u8> {
    match addr.ip() {
        IpAddr::V4(ip) => {
            let mut v = vec![0, 1];
            v.extend_from_slice(&(addr.port() ^ (MAGIC_COOKIE >> 16) as u16).to_be_bytes());
            for (a, b) in ip.octets().into_iter().zip(MAGIC_COOKIE.to_be_bytes()) {
                v.push(a ^ b);
            }
            v
        }
        IpAddr::V6(ip) => {
            let mut v = vec![0, 2];
            v.extend_from_slice(&(addr.port() ^ (MAGIC_COOKIE >> 16) as u16).to_be_bytes());
            let mut mask = MAGIC_COOKIE.to_be_bytes().to_vec();
            mask.extend_from_slice(&[0; 12]);
            for (a, b) in ip.octets().into_iter().zip(mask) {
                v.push(a ^ b);
            }
            v
        }
    }
}

fn decode_xor_address(v: &[u8]) -> Option<SocketAddr> {
    if v.len() < 4 {
        return None;
    }
    let port = u16::from_be_bytes([v[2], v[3]]) ^ (MAGIC_COOKIE >> 16) as u16;
    match v[1] {
        1 if v.len() >= 8 => {
            let cookie = MAGIC_COOKIE.to_be_bytes();
            let ip = [
                v[4] ^ cookie[0],
                v[5] ^ cookie[1],
                v[6] ^ cookie[2],
                v[7] ^ cookie[3],
            ];
            Some(SocketAddr::from((ip, port)))
        }
        2 if v.len() >= 20 => {
            let mut mask = MAGIC_COOKIE.to_be_bytes().to_vec();
            mask.extend_from_slice(&[0; 12]);
            let mut ip = [0u8; 16];
            for i in 0..16 {
                ip[i] = v[4 + i] ^ mask[i];
            }
            Some(SocketAddr::from((ip, port)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_turn_url_and_json() {
        let s = TurnServer::parse("turns:112.28.240.178:443?transport=tcp", "u", "p").unwrap();
        assert_eq!(s.host, "112.28.240.178");
        assert_eq!(s.port, 443);
        let v = serde_json::json!([{"urls":["stun:x", "turns:[2001:db8::1]:443?transport=tcp"],"username":"u","credential":"p"}]);
        let got = servers_from_json(&v);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].host, "2001:db8::1");
    }

    #[test]
    fn xor_address_roundtrip() {
        for a in [
            "112.28.240.180:54234".parse().unwrap(),
            "[2001:db8::4]:443".parse().unwrap(),
        ] {
            assert_eq!(decode_xor_address(&encode_xor_address(a)), Some(a));
        }
    }

    #[test]
    fn authenticated_allocate_has_expected_attributes() {
        let key = [7u8; 16];
        let m = build_message(
            ALLOCATE,
            &[(REQUESTED_TRANSPORT, vec![17, 0, 0, 0])],
            Some(&key),
            true,
        );
        let head: [u8; 20] = m[..20].try_into().unwrap();
        let p = parse_message(&head, &m[20..]).unwrap();
        assert_eq!(p.method, ALLOCATE);
        assert!(p.attr(MESSAGE_INTEGRITY).is_some());
        assert!(p.attr(FINGERPRINT).is_some());
    }
}
