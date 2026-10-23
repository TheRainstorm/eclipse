//! Private LAN identities and paired-device storage, independent of vendor accounts.
use anyhow::{ensure, Context, Result};
use rand::{rngs::OsRng, RngCore};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt, fs,
    io::{Read, Write},
    path::Path,
};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

pub(crate) const TLS_NAME: &str = "eclipse.local";
const LIMIT: u64 = 1024 * 1024;
fn read<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let meta = fs::symlink_metadata(path).context("cannot read private LAN configuration")?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= LIMIT,
        "LAN configuration must be a small private regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            meta.permissions().mode() & 0o077 == 0,
            "LAN configuration permissions must be 600"
        );
    }
    let mut bytes = Zeroizing::new(Vec::new());
    fs::File::open(path)?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= LIMIT as usize,
        "LAN configuration exceeds limit"
    );
    serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid LAN configuration; original file was preserved"))
}
fn save<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = Zeroizing::new(serde_json::to_vec_pretty(value)?);
    ensure!(
        bytes.len() <= LIMIT as usize,
        "LAN configuration exceeds limit"
    );
    crate::private_file::write_atomic(path, &bytes).context("cannot save private LAN configuration")
}
fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.chars().count() <= 64 && !name.chars().any(char::is_control)
}
fn valid_id(id: &str) -> bool {
    id.len() <= 36 && uuid::Uuid::parse_str(id).is_ok()
}
pub(crate) fn fresh_token() -> Vec<u8> {
    let mut token = vec![0; 32];
    OsRng.fill_bytes(&mut token);
    token
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Identity {
    version: u8,
    pub id: String,
    pub certificate: Vec<u8>,
    pub key: Vec<u8>,
}
impl Drop for Identity {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}
impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LanIdentity")
            .field("id", &self.id)
            .field("key", &"[REDACTED]")
            .finish()
    }
}
impl Identity {
    pub fn open(path: &Path) -> Result<Self> {
        if !path.try_exists()? {
            let certified = rcgen::generate_simple_self_signed(vec![TLS_NAME.into()])?;
            let value = Self {
                version: 1,
                id: uuid::Uuid::new_v4().to_string(),
                certificate: certified.cert.der().to_vec(),
                key: certified.key_pair.serialize_der(),
            };
            let bytes = Zeroizing::new(serde_json::to_vec_pretty(&value)?);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            // Never replace an existing server identity, including when two
            // launchers race. A failed partial write is preserved for recovery.
            match options.open(path) {
                Ok(mut file) => {
                    file.write_all(&bytes)?;
                    file.sync_all()?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
        }
        let identity: Self = read(path)?;
        ensure!(
            identity.version == 1
                && valid_id(&identity.id)
                && !identity.key.is_empty()
                && identity.key.len() <= 8192
                && !identity.certificate.is_empty()
                && identity.certificate.len() <= 8192,
            "invalid LAN identity; original file was preserved"
        );
        Ok(identity)
    }
    pub fn fingerprint(&self) -> String {
        hex::encode(Sha256::digest(&self.certificate))
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedClient {
    pub id: String,
    pub name: String,
    token_hash: [u8; 32],
}
impl fmt::Debug for AuthorizedClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthorizedClient")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish()
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Grants {
    version: u8,
    server_id: String,
    pub clients: Vec<AuthorizedClient>,
}
impl Grants {
    pub fn open(path: &Path, server_id: &str) -> Result<Self> {
        if !path.try_exists()? {
            return Ok(Self {
                version: 1,
                server_id: server_id.into(),
                clients: Vec::new(),
            });
        }
        let value: Self = read(path)?;
        ensure!(
            value.version == 1
                && value.server_id == server_id
                && value.clients.len() <= 64
                && value
                    .clients
                    .iter()
                    .all(|p| valid_id(&p.id) && valid_name(&p.name)),
            "invalid LAN authorization list; original file was preserved"
        );
        Ok(value)
    }
    pub fn authorize(&mut self, path: &Path, id: &str, name: &str, token: &[u8]) -> Result<()> {
        ensure!(
            valid_id(id) && valid_name(name) && token.len() == 32,
            "invalid paired client"
        );
        let old = self.clients.clone();
        let hash = Sha256::digest(token).into();
        if let Some(peer) = self.clients.iter_mut().find(|p| p.id == id) {
            peer.name = name.into();
            peer.token_hash = hash;
        } else {
            ensure!(self.clients.len() < 64, "paired-client limit reached");
            self.clients.push(AuthorizedClient {
                id: id.into(),
                name: name.into(),
                token_hash: hash,
            });
        }
        if let Err(error) = save(path, self) {
            self.clients = old;
            return Err(error);
        }
        Ok(())
    }
    pub fn accepts(&self, id: &str, token: &[u8]) -> bool {
        token.len() == 32
            && self
                .clients
                .iter()
                .find(|p| p.id == id)
                .is_some_and(|peer| {
                    bool::from(peer.token_hash.ct_eq(Sha256::digest(token).as_slice()))
                })
    }
    pub fn revoke(&mut self, path: &Path, id: &str) -> Result<()> {
        let old = self.clients.clone();
        self.clients.retain(|p| p.id != id);
        if let Err(error) = save(path, self) {
            self.clients = old;
            return Err(error);
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub certificate: Vec<u8>,
    pub(crate) token: Vec<u8>,
}
impl Drop for Device {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}
impl fmt::Debug for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairedDevice")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("endpoint", &self.endpoint)
            .field("credentials", &"[REDACTED]")
            .finish()
    }
}
impl Device {
    pub fn fingerprint(&self) -> String {
        hex::encode(Sha256::digest(&self.certificate))
    }
    fn validate(&self) -> bool {
        valid_id(&self.id)
            && valid_name(&self.name)
            && crate::native_discovery::endpoint(&self.endpoint).is_ok()
            && !self.certificate.is_empty()
            && self.certificate.len() <= 8192
            && self.token.len() == 32
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Devices {
    version: u8,
    pub client_id: String,
    pub devices: Vec<Device>,
    #[serde(default)]
    pub manual: Vec<String>,
}
impl Devices {
    pub fn open(path: &Path) -> Result<Self> {
        if !path.try_exists()? {
            return Ok(Self {
                version: 1,
                client_id: uuid::Uuid::new_v4().to_string(),
                devices: Vec::new(),
                manual: Vec::new(),
            });
        }
        let value: Self = read(path)?;
        ensure!(
            value.version == 1
                && valid_id(&value.client_id)
                && value.devices.len() <= 64
                && value.devices.iter().all(Device::validate)
                && value.manual.len() <= 64
                && value
                    .manual
                    .iter()
                    .all(|v| crate::native_discovery::endpoint(v).is_ok()),
            "invalid paired devices; original file was preserved"
        );
        Ok(value)
    }
    pub fn add_endpoint(&mut self, path: &Path, endpoint: &str) -> Result<()> {
        let endpoint = crate::native_discovery::endpoint(endpoint)?;
        if self.manual.contains(&endpoint) {
            return Ok(());
        }
        ensure!(self.manual.len() < 64, "manual-device limit reached");
        self.manual.push(endpoint);
        if let Err(error) = save(path, self) {
            self.manual.pop();
            return Err(error);
        }
        Ok(())
    }
    pub fn remove_endpoint(&mut self, path: &Path, endpoint: &str) -> Result<()> {
        let old = self.manual.clone();
        self.manual.retain(|e| e != endpoint);
        if let Err(error) = save(path, self) {
            self.manual = old;
            return Err(error);
        }
        Ok(())
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        save(path, self)
    }
    pub fn paired(&self, id: &str) -> Option<&Device> {
        self.devices.iter().find(|p| p.id == id)
    }
    pub fn remember(&mut self, path: &Path, device: Device) -> Result<()> {
        ensure!(device.validate(), "invalid paired device");
        if let Some(existing) = self.paired(&device.id) {
            ensure!(
                existing.certificate == device.certificate,
                "server identity changed; explicitly forget the old device before pairing again"
            );
        }
        let old = self.devices.clone();
        if let Some(existing) = self.devices.iter_mut().find(|p| p.id == device.id) {
            *existing = device;
        } else {
            ensure!(self.devices.len() < 64, "paired-device limit reached");
            self.devices.push(device);
        }
        if let Err(error) = save(path, self) {
            self.devices = old;
            return Err(error);
        }
        Ok(())
    }
    pub fn forget(&mut self, path: &Path, id: &str) -> Result<()> {
        let old = self.devices.clone();
        self.devices.retain(|p| p.id != id);
        if let Err(error) = save(path, self) {
            self.devices = old;
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let p =
                std::env::temp_dir().join(format!("eclipse-lan-store-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn manual_addresses_survive_restart_and_failed_writes_do_not_change_memory() {
        let fixture = Fixture::new();
        let path = fixture.0.join("devices.json");
        let mut book = Devices::open(&path).unwrap();
        book.add_endpoint(&path, "127.0.0.1").unwrap();
        let endpoint = format!("127.0.0.1:{}", crate::native_discovery::SERVICE_PORT);
        book.add_endpoint(&path, &endpoint).unwrap();
        assert_eq!(book.manual, [endpoint.clone()]);
        assert_eq!(Devices::open(&path).unwrap().manual, book.manual);
        let bad_path = path.join("parent-is-file.json");
        assert!(book.add_endpoint(&bad_path, "example.invalid").is_err());
        assert!(book.remove_endpoint(&bad_path, &endpoint).is_err());
        assert_eq!(book.manual, [endpoint.clone()]);
        assert!(book.add_endpoint(&path, "https://example.invalid").is_err());
        book.remove_endpoint(&path, &endpoint).unwrap();
        assert!(Devices::open(&path).unwrap().manual.is_empty());
        // Old paired-device files predate manual addresses and remain readable.
        let mut old = serde_json::to_value(&book).unwrap();
        old.as_object_mut().unwrap().remove("manual");
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        assert!(Devices::open(&path).unwrap().manual.is_empty());
    }
    #[test]
    fn identity_is_stable_and_corrupt_existing_files_are_never_replaced() {
        let f = Fixture::new();
        let p = f.0.join("identity.json");
        let a = Identity::open(&p).unwrap();
        let b = Identity::open(&p).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert!(!format!("{a:?}").contains(&format!("{:?}", a.key)));
        fs::write(&p, b"damaged-private-fixture").unwrap();
        let e = Identity::open(&p).unwrap_err().to_string();
        assert!(!e.contains("damaged-private-fixture"));
        assert_eq!(fs::read(p).unwrap(), b"damaged-private-fixture");
    }
    #[test]
    fn random_client_authorization_is_persistent_revocable_and_not_a_public_fingerprint() {
        let f = Fixture::new();
        let path = f.0.join("grants.json");
        let server = uuid::Uuid::new_v4().to_string();
        let client = uuid::Uuid::new_v4().to_string();
        let token = fresh_token();
        let mut grants = Grants::open(&path, &server).unwrap();
        grants
            .authorize(&path, &client, "Generated client", &token)
            .unwrap();
        assert!(grants.accepts(&client, &token));
        assert!(!grants.accepts(&client, &fresh_token()));
        assert!(!grants.accepts(&client, &token[..31]));
        let mut grants = Grants::open(&path, &server).unwrap();
        assert!(grants.accepts(&client, &token));
        grants.revoke(&path, &client).unwrap();
        assert!(!grants.accepts(&client, &token));
    }
    #[test]
    fn paired_device_certificate_changes_require_explicit_forget_and_tokens_are_private() {
        let f = Fixture::new();
        let p = f.0.join("devices.json");
        let mut devices = Devices::open(&p).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let device = Device {
            id: id.clone(),
            name: "Generated server".into(),
            endpoint: "127.0.0.1:47998".into(),
            certificate: vec![1, 2, 3],
            token: fresh_token(),
        };
        assert!(!format!("{device:?}").contains(&format!("{:?}", device.token)));
        devices.remember(&p, device.clone()).unwrap();
        let mut changed = device.clone();
        changed.certificate = vec![3, 2, 1];
        assert!(devices.remember(&p, changed.clone()).is_err());
        assert_eq!(
            Devices::open(&p).unwrap().paired(&id).unwrap().certificate,
            device.certificate
        );
        devices.forget(&p, &id).unwrap();
        devices.remember(&p, changed).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o077, 0);
            fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(Devices::open(&p).is_err());
        }
    }
}
