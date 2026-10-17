//! PCK control channel over a direct TLS connection to the controlled host.
//!
//! The official client keeps a second TLS connection (separate from WebRTC and
//! signalling) to the peer's dynamic port. Keyboard events and heartbeats are
//! sent there. This module provides the frame codec and a TLS transport with a
//! permissive verifier (the peer uses a self-signed certificate).

use anyhow::{bail, Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

pub const HEADER_LEN: usize = 28;

#[derive(Debug, Clone)]
pub struct Frame {
    pub ptype: u8,
    pub seq: u32,
    pub ts: u32,
    pub channel: u16,
    pub kind: u32,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < HEADER_LEN || &buf[..3] != b"PCK" {
            bail!("not a PCK frame");
        }
        let ptype = buf[8];
        let seq = u32::from_be_bytes([buf[9], buf[10], buf[11], buf[12]]);
        let ts = u32::from_le_bytes([buf[13], buf[14], buf[15], buf[16]]);
        let channel = u16::from_le_bytes([buf[17], buf[18]]);
        let kind = u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
        let plen = u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]) as usize;
        if buf.len() < HEADER_LEN + plen {
            bail!("short PCK payload");
        }
        Ok(Self {
            ptype,
            seq,
            ts,
            channel,
            kind,
            payload: buf[HEADER_LEN..HEADER_LEN + plen].to_vec(),
        })
    }
}

#[derive(Debug)]
struct NoVerify;

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Boxed transport so TLS can run either directly on a TcpStream or on a
/// duplex pipe that demultiplexes STUN connectivity checks.
pub trait PckIo: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> PckIo for T {}

pub struct PckTls {
    stream: TlsStream<Box<dyn PckIo>>,
}

impl PckTls {
    pub async fn connect(host: &str, port: u16, sni: &str) -> Result<Self> {
        Self::connect_with_alpn(host, port, sni, None).await
    }

    pub async fn connect_with_alpn(
        host: &str,
        port: u16,
        sni: &str,
        alpn: Option<&str>,
    ) -> Result<Self> {
        let mut config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify))
            .with_no_client_auth();
        if let Some(alpn) = alpn {
            config.alpn_protocols = vec![alpn.as_bytes().to_vec()];
        }
        let connector = TlsConnector::from(Arc::new(config));
        let tcp = TcpStream::connect((host, port))
            .await
            .with_context(|| format!("connect {host}:{port}"))?;
        let name = ServerName::try_from(sni.to_string()).context("invalid SNI")?;
        let stream = connector
            .connect(name, Box::new(tcp) as Box<dyn PckIo>)
            .await
            .context("TLS handshake")?;
        Ok(Self { stream })
    }

    pub async fn send(&mut self, frame: &[u8]) -> Result<()> {
        self.stream.write_all(frame).await?;
        self.stream.flush().await?;
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<Frame> {
        let mut head = [0u8; HEADER_LEN];
        self.stream.read_exact(&mut head).await?;
        if &head[..3] != b"PCK" {
            bail!("unexpected PCK stream prefix: {:02x?}", &head[..6]);
        }
        let plen = u32::from_le_bytes([head[24], head[25], head[26], head[27]]) as usize;
        let mut payload = vec![0u8; plen];
        self.stream.read_exact(&mut payload).await?;
        let mut full = head.to_vec();
        full.extend_from_slice(&payload);
        Frame::decode(&full)
    }
}

impl PckTls {
    /// Perform the ICE-TCP STUN binding exchange and return the open stream.
    /// The caller may then either start TLS (official relay path) or speak
    /// plain PCK frames directly (direct peer path).
    pub async fn ice_tcp_connect(
        peer: std::net::SocketAddr,
        creds: &crate::stun::IceCredentials,
    ) -> Result<TcpStream> {
        use tokio::time::{timeout, Duration};

        let mut tcp = TcpStream::connect(peer)
            .await
            .with_context(|| format!("connect PCK peer {peer}"))?;
        let _ = tcp.set_nodelay(true);
        let priority = 0x6e00_20ffu32;
        let tiebreaker = u64::from_be_bytes(
            uuid::Uuid::new_v4().as_bytes()[..8].try_into().unwrap(),
        );
        let mut last_err = anyhow::anyhow!("no STUN response");
        let mut controlling = true;
        for attempt in 0..5 {
            let txid = {
                let u = uuid::Uuid::new_v4();
                let mut t = [0u8; 12];
                t.copy_from_slice(&u.as_bytes()[..12]);
                t
            };
            let req = crate::stun::binding_request(creds, priority, controlling, tiebreaker, txid);
            tcp.write_all(&req).await.context("send STUN binding request")?;
            tcp.flush().await.ok();
            let mut lenb = [0u8; 2];
            match timeout(Duration::from_secs(4), tcp.read_exact(&mut lenb)).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    last_err = anyhow::anyhow!("read STUN length: {e}");
                    continue;
                }
                Err(_) => {
                    last_err = anyhow::anyhow!("STUN response timeout (attempt {attempt})");
                    continue;
                }
            }
            let len = u16::from_be_bytes(lenb) as usize;
            if !(20..=2048).contains(&len) {
                last_err = anyhow::anyhow!("bad STUN length {len}");
                continue;
            }
            let mut buf = vec![0u8; len];
            timeout(Duration::from_secs(4), tcp.read_exact(&mut buf))
                .await
                .context("STUN body timeout")??;
            let resp = crate::stun::parse_response(&buf)?;
            if resp.success {
                tracing::info!(peer = %peer, mapped = ?resp.mapped, "ICE-TCP binding succeeded");
                return Self::wait_peer_check(tcp, creds, peer).await;
            }
            if resp.error_code == Some(487) {
                controlling = !controlling;
            }
            tracing::warn!(?resp, controlling, "ICE-TCP binding rejected");
            last_err = anyhow::anyhow!(
                "STUN error {:?} {:?}",
                resp.error_code,
                resp.error_reason.unwrap_or_default()
            );
        }
        Err(last_err.context("ICE-TCP binding failed"))
    }

    /// The peer's ICE agent sends its own Binding Request shortly after our
    /// check succeeds.  The streamer will not accept TLS until that request
    /// has been answered, so consume and answer it before the handshake.
    async fn wait_peer_check(
        mut tcp: TcpStream,
        creds: &crate::stun::IceCredentials,
        peer: std::net::SocketAddr,
    ) -> Result<TcpStream> {
        use tokio::time::{timeout, Duration};
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                break;
            }
            let mut lenb = [0u8; 2];
            match timeout(left, tcp.read_exact(&mut lenb)).await {
                Ok(Ok(_)) => {}
                _ => break,
            }
            let len = u16::from_be_bytes(lenb) as usize;
            if !(20..=2048).contains(&len) {
                break;
            }
            let mut body = vec![0u8; len];
            if timeout(Duration::from_secs(2), tcp.read_exact(&mut body))
                .await
                .is_err()
            {
                break;
            }
            if body.len() >= 20 && u16::from_be_bytes([body[0], body[1]]) == crate::stun::BINDING_REQUEST {
                let resp = crate::stun::binding_response(&body, &creds.local_pwd, peer);
                if !resp.is_empty() {
                    tcp.write_all(&resp).await.context("send STUN binding response")?;
                    tracing::info!(peer = %peer, "answered peer ICE-TCP check");
                    return Ok(tcp);
                }
            }
        }
        Ok(tcp)
    }

    /// Dial the peer and run TLS on the ICE-bound stream.
    pub async fn dial_ice_tcp(
        peer: std::net::SocketAddr,
        creds: &crate::stun::IceCredentials,
        sni: &str,
    ) -> Result<PckTls> {
        Self::dial_ice_tcp_alpn(peer, creds, sni, None).await
    }

    pub async fn dial_ice_tcp_alpn(
        peer: std::net::SocketAddr,
        creds: &crate::stun::IceCredentials,
        sni: &str,
        alpn: Option<&str>,
    ) -> Result<PckTls> {
        let tcp = Self::ice_tcp_connect(peer, creds).await?;
        Self::connect_stream_muxed(tcp, creds, peer, sni, alpn).await
    }

    /// Dial the peer, but return the plain TCP stream after STUN so the caller
    /// can speak PCK directly (used to probe the direct-host transport).
    pub async fn dial_ice_tcp_raw(
        peer: std::net::SocketAddr,
        creds: &crate::stun::IceCredentials,
    ) -> Result<TcpStream> {
        Self::ice_tcp_connect(peer, creds).await
    }

    /// After the STUN exchange, capture (a) any bytes already queued by the
    /// peer and (b) the raw bytes returned for a TLS ClientHello.  This is a
    /// diagnostic for reversing the post-STUN framing.
    pub async fn probe_after_stun(
        peer: std::net::SocketAddr,
        creds: &crate::stun::IceCredentials,
        sni: &str,
    ) -> Result<String> {
        use tokio::time::{timeout, Duration};
        let mut tcp = Self::ice_tcp_connect(peer, creds).await?;
        let mut buf = [0u8; 4096];
        let mut report = String::new();
        match timeout(Duration::from_millis(2500), tcp.read(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => report.push_str(&format!("pre={} ", hex::encode(&buf[..n.min(64)]))),
            _ => report.push_str("pre=none "),
        }
        let mut config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify))
            .with_no_client_auth();
        config.alpn_protocols.clear();
        let name = if sni.is_empty() {
            ServerName::try_from(peer.ip().to_string())?
        } else {
            ServerName::try_from(sni.to_string())?
        };
        let mut conn = rustls::ClientConnection::new(Arc::new(config), name)?;
        let mut out = Vec::new();
        while conn.wants_write() {
            conn.write_tls(&mut out)?;
        }
        tcp.write_all(&out).await?;
        report.push_str(&format!("hello={} ", hex::encode(&out[..out.len().min(32)])));
        match timeout(Duration::from_secs(3), tcp.read(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => report.push_str(&format!("resp={}", hex::encode(&buf[..n.min(128)]))),
            Ok(Ok(_)) => report.push_str("resp=eof"),
            Ok(Err(e)) => report.push_str(&format!("resp=err:{e}")),
            Err(_) => report.push_str("resp=timeout"),
        }
        Ok(report)
    }

    /// Run TLS on an ICE-bound stream while answering the peer's own STUN
    /// connectivity checks that arrive interleaved with the handshake.
    ///
    /// After our check succeeds the peer sends a Binding Request (USERNAME
    /// `our_ufrag:peer_ufrag`, MESSAGE-INTEGRITY keyed with our local
    /// password).  It must be answered before the peer will process TLS.
    pub async fn connect_stream_muxed(
        tcp: TcpStream,
        creds: &crate::stun::IceCredentials,
        peer: std::net::SocketAddr,
        sni: &str,
        alpn: Option<&str>,
    ) -> Result<Self> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify))
            .with_no_client_auth();
        if let Some(alpn) = alpn {
            config.alpn_protocols = vec![alpn.as_bytes().to_vec()];
        }
        let connector = TlsConnector::from(Arc::new(config));
        let name = if sni.is_empty() {
            ServerName::try_from(peer.ip().to_string())?
        } else {
            ServerName::try_from(sni.to_string()).context("invalid SNI")?
        };

        let (client_side, server_side) = tokio::io::duplex(256 * 1024);
        let (mut srv_rd, mut srv_wr) = tokio::io::split(server_side);
        let (mut tr, mut tw) = tokio::io::split(tcp);
        let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let out_tx_stun = out_tx.clone();
        let out_tx_tls = out_tx.clone();

        // TCP -> TLS (STUN frames are consumed and answered here)
        let pwd = creds.local_pwd.clone();
        tokio::spawn(async move {
            let mut pending: Vec<u8> = Vec::new();
            let mut buf = [0u8; 16384];
            loop {
                let n = match tr.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                pending.extend_from_slice(&buf[..n]);
                loop {
                    if pending.is_empty() {
                        break;
                    }
                    if pending[0] == 0 {
                        if pending.len() < 2 {
                            break;
                        }
                        let len = u16::from_be_bytes([pending[0], pending[1]]) as usize;
                        if pending.len() < 2 + len {
                            break;
                        }
                        let msg = pending[2..2 + len].to_vec();
                        pending.drain(..2 + len);
                        if msg.len() >= 20 && u16::from_be_bytes([msg[0], msg[1]]) == 0x0001 {
                            let resp = crate::stun::binding_response(&msg, &pwd, peer);
                            if !resp.is_empty() {
                                let _ = out_tx_stun.send(resp);
                            }
                        }
                    } else {
                        let data = std::mem::take(&mut pending);
                        eprintln!(
                            "mux->tls {} bytes: {}",
                            data.len(),
                            hex::encode(&data[..data.len().min(64)])
                        );
                        if !std::path::Path::new("/tmp/uu_mux_first2.bin").exists() {
                            let _ = std::fs::write("/tmp/uu_mux_first2.bin", &data);
                        }
                        if srv_wr.write_all(&data).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });

        // TLS -> TCP
        tokio::spawn(async move {
            let mut buf = [0u8; 16384];
            loop {
                match srv_rd.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if out_tx_tls.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        // single writer task (STUN replies and TLS records share the socket)
        tokio::spawn(async move {
            while let Some(data) = out_rx.recv().await {
                if tw.write_all(&data).await.is_err() {
                    break;
                }
            }
        });

        let stream = connector
            .connect(name, Box::new(client_side) as Box<dyn PckIo>)
            .await
            .context("PCK TLS handshake (muxed)")?;
        Ok(Self { stream })
    }

    /// Wrap an already-connected (and ICE-bound) TCP stream in TLS.
    pub async fn connect_stream(tcp: TcpStream, sni: &str) -> Result<Self> {
        Self::connect_stream_alpn(tcp, sni, None).await
    }

    pub async fn connect_stream_alpn(tcp: TcpStream, sni: &str, alpn: Option<&str>) -> Result<Self> {
        let mut config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify))
            .with_no_client_auth();
        if let Some(alpn) = alpn {
            config.alpn_protocols = vec![alpn.as_bytes().to_vec()];
        }
        let connector = TlsConnector::from(Arc::new(config));
        let name = if sni.is_empty() {
            ServerName::try_from(tcp.peer_addr()?.ip().to_string())?
        } else {
            ServerName::try_from(sni.to_string()).context("invalid SNI")?
        };
        let stream = connector
            .connect(name, Box::new(tcp) as Box<dyn PckIo>)
            .await
            .context("PCK TLS handshake")?;
        Ok(Self { stream })
    }

    /// Split the TLS stream into independent read/write halves.
    pub fn into_split(
        self,
    ) -> (
        tokio::io::ReadHalf<TlsStream<Box<dyn PckIo>>>,
        tokio::io::WriteHalf<TlsStream<Box<dyn PckIo>>>,
    ) {
        tokio::io::split(self.stream)
    }
}
