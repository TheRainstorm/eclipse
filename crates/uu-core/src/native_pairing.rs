//! Native, user-shared invitations. No vendor account or shared production secret.
//! A short-lived 256-bit capability travels only inside a pinned TLS 1.3 channel.
use anyhow::{bail, ensure, Context, Result};
use rand::{rngs::OsRng, RngCore};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt, fs,
    io::{Read, Write},
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use zeroize::{Zeroize, Zeroizing};

/// Remove this host's published capability when its listener/session ends.
/// A replacement file belongs to its writer and must be left alone.
pub(crate) struct PublishedInvitation {
    path: std::path::PathBuf,
    digest: [u8; 32],
}
impl Drop for PublishedInvitation {
    fn drop(&mut self) {
        let Ok(file) = fs::File::open(&self.path) else {
            return;
        };
        let mut bytes = Zeroizing::new(Vec::new());
        if file.take(MAX_INVITE + 1).read_to_end(&mut bytes).is_ok()
            && bytes.len() <= MAX_INVITE as usize
            && <[u8; 32]>::from(Sha256::digest(&*bytes)) == self.digest
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

const PROTOCOL: u32 = 1;
const TLS_NAME: &str = "eclipse.local";
const TIMEOUT: Duration = Duration::from_secs(5);
const MAX_INVITE: u64 = 16 * 1024;
pub(crate) const BROKER_INVITATION_LIFETIME: Duration = Duration::from_secs(30);

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

/// This file grants access. Never put its contents in logs, command arguments or Git.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invitation {
    version: u32,
    address: SocketAddr,
    certificate: Vec<u8>,
    token: Vec<u8>,
    expires_at: u64,
    // Only an authenticated, on-demand broker response can establish this
    // local deadline. It cannot be supplied in an invitation file or on wire.
    #[serde(skip)]
    broker_deadline: Option<Instant>,
    #[serde(skip)]
    observed_client_ip: Option<IpAddr>,
}
impl Drop for Invitation {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}
impl fmt::Debug for Invitation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Invitation")
            .field("address", &self.address)
            .field("credentials", &"[REDACTED]")
            .finish()
    }
}
impl Invitation {
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }
    fn validate_format(&self) -> Result<()> {
        ensure!(
            self.version == PROTOCOL
                && self.token.len() == 32
                && !self.certificate.is_empty()
                && self.certificate.len() <= 8192,
            "invalid native invitation"
        );
        ensure!(
            !self.address.ip().is_unspecified()
                && !self.address.ip().is_multicast()
                && self.address.port() != 0,
            "invitation needs a concrete unicast address"
        );
        Ok(())
    }
    fn validate(&self) -> Result<()> {
        self.validate_format()?;
        let live = match self.broker_deadline {
            Some(deadline) => Instant::now() < deadline,
            None => self.expires_at > now()?,
        };
        ensure!(live, "native invitation expired; request a new one from the host (file invitations also require synchronized clocks)");
        Ok(())
    }
    /// Broker media capabilities are issued after the authenticated request.
    /// Start the client timer BEFORE sending that request, so transit time does
    /// not extend their lifetime. The host independently enforces its expiry.
    pub(crate) fn from_authenticated_broker(
        mut self,
        requested_at: Instant,
        observed_client_ip: Option<IpAddr>,
    ) -> Result<Self> {
        self.validate_format()?;
        if let Some(ip) = observed_client_ip {
            ensure!(
                !ip.is_unspecified()
                    && !ip.is_multicast()
                    && ip.is_ipv4() == self.address.is_ipv4(),
                "invalid observed client address"
            );
        }
        self.broker_deadline = Some(requested_at + BROKER_INVITATION_LIFETIME);
        self.observed_client_ip = observed_client_ip;
        self.validate()?;
        Ok(self)
    }
    pub(crate) fn observed_client_ip(&self) -> Option<IpAddr> {
        self.observed_client_ip
    }
    pub fn load(path: &Path) -> Result<Self> {
        let file = fs::File::open(path).context("cannot open native invitation file")?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && metadata.len() <= MAX_INVITE,
            "native invitation must be a small regular file"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                metadata.permissions().mode() & 0o077 == 0,
                "native invitation is private; set its file permissions to 600"
            );
        }
        let mut data = Zeroizing::new(Vec::new());
        file.take(MAX_INVITE + 1).read_to_end(&mut data)?;
        ensure!(
            data.len() <= MAX_INVITE as usize,
            "native invitation file is too large"
        );
        let invite: Self = serde_json::from_slice(&data)
            .map_err(|_| anyhow::anyhow!("invalid native invitation file"))?;
        invite.validate()?;
        Ok(invite)
    }
    pub fn save_new(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let data = Zeroizing::new(
            serde_json::to_vec_pretty(self).context("cannot serialize native invitation")?,
        );
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(path)
            .context("native invitation output must be a new private file")?;
        file.write_all(&data)?;
        file.sync_all()?;
        Ok(())
    }
    pub(crate) fn publish_new(&self, path: &Path) -> Result<PublishedInvitation> {
        let bytes = Zeroizing::new(serde_json::to_vec_pretty(self)?);
        let digest = Sha256::digest(&*bytes).into();
        self.save_new(path)?;
        Ok(PublishedInvitation {
            path: path.into(),
            digest,
        })
    }
}

pub struct HostCredentials {
    invitation: Invitation,
    acceptor: TlsAcceptor,
}
impl HostCredentials {
    pub fn new(address: SocketAddr, lifetime: Duration) -> Result<Self> {
        ensure!(
            (1..=3600).contains(&lifetime.as_secs()),
            "invitation lifetime must be 1..=3600 seconds"
        );
        let certified = rcgen::generate_simple_self_signed(vec![TLS_NAME.into()])?;
        let certificate = certified.cert.der().to_vec();
        let key =
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der()));
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![CertificateDer::from(certificate.clone())], key)?;
        let mut token = vec![0u8; 32];
        OsRng.fill_bytes(&mut token);
        let invitation = Invitation {
            version: PROTOCOL,
            address,
            certificate,
            token,
            expires_at: now()?
                .checked_add(lifetime.as_secs())
                .context("invitation lifetime overflow")?,
            broker_deadline: None,
            observed_client_ip: None,
        };
        invitation.validate()?;
        Ok(Self {
            invitation,
            acceptor: TlsAcceptor::from(Arc::new(tls)),
        })
    }
    pub fn invitation(&self) -> &Invitation {
        &self.invitation
    }
    /// Capture, encoding and input must be started only after this succeeds.
    pub async fn accept(
        &self,
        socket: TcpStream,
    ) -> Result<tokio_rustls::server::TlsStream<TcpStream>> {
        timeout(TIMEOUT, async {
            socket.set_nodelay(true)?;
            let mut stream = self
                .acceptor
                .accept(socket)
                .await
                .context("native TLS handshake rejected")?;
            let mut received = Zeroizing::new([0u8; 32]);
            stream.read_exact(&mut received[..]).await?;
            let matches = bool::from(received[..].ct_eq(&self.invitation.token));
            let accepted = matches && now()? < self.invitation.expires_at;
            stream.write_all(&[u8::from(accepted)]).await?;
            stream.flush().await?;
            ensure!(accepted, "native pairing rejected");
            Ok(stream)
        })
        .await
        .context("native pairing timed out")?
    }
}

pub async fn connect(
    invitation: &Invitation,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
    invitation.validate()?;
    timeout(TIMEOUT, async {
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(invitation.certificate.clone()))
            .context("invalid native host certificate")?;
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let socket = TcpStream::connect(invitation.address)
            .await
            .context("cannot reach native host")?;
        socket.set_nodelay(true)?;
        let mut stream = TlsConnector::from(Arc::new(tls))
            .connect(ServerName::try_from(TLS_NAME)?, socket)
            .await
            .context("native host certificate verification failed")?;
        stream.write_all(&invitation.token).await?;
        stream.flush().await?;
        let response = stream.read_u8().await?;
        if response != 1 {
            bail!("native pairing rejected; check the invitation and its expiry");
        }
        Ok(stream)
    })
    .await
    .context("native pairing timed out")?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn published_invitation_cleanup_preserves_a_replacement_file() {
        let path =
            std::env::temp_dir().join(format!("eclipse-published-{}.json", uuid::Uuid::new_v4()));
        let credentials =
            HostCredentials::new("127.0.0.1:32123".parse().unwrap(), Duration::from_secs(60))
                .unwrap();
        let published = credentials.invitation().publish_new(&path).unwrap();
        assert!(path.exists());
        drop(published);
        assert!(!path.exists());
        let published = credentials.invitation().publish_new(&path).unwrap();
        fs::write(&path, b"replacement owned by another writer").unwrap();
        drop(published);
        assert_eq!(
            fs::read(&path).unwrap(),
            b"replacement owned by another writer"
        );
        fs::remove_file(path).unwrap();
    }
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    async fn host() -> (TcpListener, HostCredentials) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let credentials =
            HostCredentials::new(listener.local_addr().unwrap(), Duration::from_secs(60)).unwrap();
        (listener, credentials)
    }
    #[tokio::test]
    async fn actual_tls13_pairing_allows_only_the_invited_host_and_capability() {
        let (listener, credentials) = host().await;
        let invitation = credentials.invitation().clone();
        assert!(!format!("{invitation:?}").contains(&hex::encode(&invitation.token)));
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut stream = credentials.accept(socket).await.unwrap();
            assert_eq!(
                stream.get_ref().1.protocol_version(),
                Some(rustls::ProtocolVersion::TLSv1_3)
            );
            assert_eq!(stream.read_u8().await.unwrap(), 42);
            stream.write_u8(43).await.unwrap();
            stream.flush().await.unwrap();
        });
        let mut stream = connect(&invitation).await.unwrap();
        stream.write_u8(42).await.unwrap();
        stream.flush().await.unwrap();
        assert_eq!(stream.read_u8().await.unwrap(), 43);
        task.await.unwrap();
    }
    #[tokio::test]
    async fn wrong_capability_is_rejected_before_any_session_is_returned() {
        let (listener, credentials) = host().await;
        let mut invitation = credentials.invitation().clone();
        invitation.token[0] ^= 1;
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            assert!(credentials.accept(socket).await.is_err());
        });
        assert!(connect(&invitation).await.is_err());
        task.await.unwrap();
    }
    #[tokio::test]
    async fn different_host_certificate_cannot_impersonate_the_paired_host() {
        let (listener, credentials) = host().await;
        let mut invitation = credentials.invitation().clone();
        let other = HostCredentials::new(invitation.address, Duration::from_secs(60)).unwrap();
        invitation.certificate = other.invitation().certificate.clone();
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            assert!(credentials.accept(socket).await.is_err());
        });
        assert!(connect(&invitation).await.is_err());
        task.await.unwrap();
    }
    #[tokio::test]
    async fn expired_server_invitation_is_rejected_even_if_client_changes_its_clock_hint() {
        let (listener, mut credentials) = host().await;
        let invitation = credentials
            .invitation()
            .clone()
            .from_authenticated_broker(Instant::now(), None)
            .unwrap();
        credentials.invitation.expires_at = now().unwrap() - 1;
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            assert!(credentials.accept(socket).await.is_err());
        });
        assert!(connect(&invitation).await.is_err());
        task.await.unwrap();
    }
    #[test]
    fn fresh_broker_deadline_is_local_bounded_and_cannot_be_injected_from_a_file() {
        let credentials =
            HostCredentials::new("127.0.0.1:12345".parse().unwrap(), Duration::from_secs(60))
                .unwrap();
        let mut invitation = credentials.invitation().clone();
        // The absolute timestamp appears expired on a viewer whose clock is
        // ahead, while the fresh TLS broker response starts a local timer.
        invitation.expires_at = 1;
        assert!(invitation.validate().is_err());
        let fresh = invitation
            .clone()
            .from_authenticated_broker(Instant::now(), None)
            .unwrap();
        assert!(fresh.validate().is_ok());
        let observed = invitation
            .clone()
            .from_authenticated_broker(Instant::now(), Some("127.0.0.2".parse().unwrap()))
            .unwrap();
        assert_eq!(
            observed.observed_client_ip(),
            Some("127.0.0.2".parse().unwrap())
        );
        let mut observed_wire = serde_json::to_value(&observed).unwrap();
        assert!(observed_wire.get("observed_client_ip").is_none());
        observed_wire["observed_client_ip"] = serde_json::json!("127.0.0.2");
        assert!(serde_json::from_value::<Invitation>(observed_wire).is_err());
        let mut wire = serde_json::to_value(&fresh).unwrap();
        assert!(wire.get("broker_deadline").is_none());
        let loaded: Invitation = serde_json::from_value(wire.clone()).unwrap();
        assert!(loaded.broker_deadline.is_none());
        assert!(loaded.validate().is_err());
        wire["broker_deadline"] = serde_json::json!("2099-01-01");
        assert!(serde_json::from_value::<Invitation>(wire).is_err());
        assert!(invitation
            .from_authenticated_broker(Instant::now() - Duration::from_secs(31), None)
            .is_err());
    }
    #[tokio::test]
    async fn unauthenticated_idle_socket_is_closed_after_the_pairing_deadline() {
        let (listener, credentials) = host().await;
        let socket = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (incoming, _) = listener.accept().await.unwrap();
        let started = std::time::Instant::now();
        let result = credentials.accept(incoming).await;
        assert!(result.unwrap_err().to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(7));
        drop(socket);
    }

    #[test]
    fn private_file_roundtrip_does_not_overwrite_and_rejects_shared_permissions() {
        let credentials =
            HostCredentials::new("127.0.0.1:12345".parse().unwrap(), Duration::from_secs(60))
                .unwrap();
        let invitation = credentials.invitation().clone();
        let path = std::env::temp_dir().join(format!(
            "uu-native-{}.eclipse-invite.json",
            uuid::Uuid::new_v4()
        ));
        invitation.save_new(&path).unwrap();
        let outcome = std::panic::catch_unwind(|| {
            let loaded = Invitation::load(&path).unwrap();
            assert_eq!(loaded.token, invitation.token);
            assert!(invitation.save_new(&path).is_err());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
                fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
                assert!(Invitation::load(&path).is_err());
            }
        });
        fs::remove_file(path).unwrap();
        outcome.unwrap()
    }
}
