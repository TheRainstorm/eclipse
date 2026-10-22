//! Device identity and per-peer grants, shared by every rendezvous provider.
//! Private keys never leave this store. Grants are local, directional decisions.
use anyhow::{ensure, Context, Result};
use fs2::FileExt;
use ring::signature::{self, Ed25519KeyPair, KeyPair};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};
use zeroize::Zeroizing;

const MAX_FILE: u64 = 1024 * 1024;

pub(crate) fn read_private(path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    let meta = fs::symlink_metadata(path).context("无法读取设备私有配置")?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= MAX_FILE,
        "设备配置必须是有界普通文件，原文件已保留"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            meta.permissions().mode() & 0o077 == 0,
            "设备私有配置权限必须为 600"
        );
    }
    let mut bytes = Zeroizing::new(Vec::new());
    fs::File::open(path)?
        .take(MAX_FILE + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_FILE as usize, "设备配置过大");
    Ok(bytes)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicDevice {
    pub name: String,
    pub public_key: String,
}
impl PublicDevice {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.name.trim().is_empty()
                && self.name.chars().count() <= 64
                && !self.name.chars().any(char::is_control),
            "设备名称无效"
        );
        ensure!(
            self.public_key.len() == 64
                && hex::decode(&self.public_key).is_ok_and(|k| k.len() == 32),
            "设备公钥无效"
        );
        Ok(())
    }
    pub fn id(&self) -> String {
        // Validate before treating a card as an identity. A name is never an ID.
        hex::encode(Sha256::digest(
            hex::decode(&self.public_key).unwrap_or_default(),
        ))
    }
    pub fn verify(&self, data: &[u8], proof: &str) -> Result<()> {
        self.validate()?;
        let signature = hex::decode(proof).context("设备身份凭据格式无效")?;
        ensure!(signature.len() == 64, "设备身份凭据长度无效");
        signature::UnparsedPublicKey::new(&signature::ED25519, hex::decode(&self.public_key)?)
            .verify(data, &signature)
            .map_err(|_| anyhow::anyhow!("设备身份验证失败"))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    pub view: bool,
    pub control: bool,
}
impl Permissions {
    pub const VIEW: Self = Self {
        view: true,
        control: false,
    };
    pub const CONTROL: Self = Self {
        view: true,
        control: true,
    };
    pub fn validate(self) -> Result<()> {
        ensure!(
            self.view && (!self.control || self.view),
            "设备授权必须允许观看"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Peer {
    pub device: PublicDevice,
    pub inbound: Option<Permissions>,
    #[serde(default)]
    pub revoked: bool,
    /// Old bearer authorizations already migrated to this public identity.
    #[serde(default)]
    pub legacy_ids: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Book {
    version: u32,
    peers: Vec<Peer>,
}
impl Default for Book {
    fn default() -> Self {
        Self {
            version: 2,
            peers: Vec::new(),
        }
    }
}
impl Book {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 2 && self.peers.len() <= 256,
            "设备授权配置无效，原文件已保留"
        );
        let mut ids = std::collections::HashSet::new();
        let mut aliases = std::collections::HashSet::new();
        for peer in &self.peers {
            peer.device.validate()?;
            ensure!(
                ids.insert(peer.device.id()) && peer.legacy_ids.len() <= 64,
                "设备授权记录重复"
            );
            if let Some(grant) = peer.inbound {
                grant.validate()?;
            }
            ensure!(
                !peer.revoked || peer.inbound.is_none(),
                "撤销的设备仍有访问权限"
            );
            for alias in &peer.legacy_ids {
                ensure!(
                    alias.len() <= 128 && !alias.is_empty() && aliases.insert(alias),
                    "旧设备授权映射重复"
                );
            }
        }
        Ok(())
    }
}

pub(crate) struct FileLock(fs::File);
impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

/// Clone shares the immutable identity; each update reloads under an OS lock,
/// so independent stream windows cannot overwrite another window's grants.
#[derive(Clone)]
pub struct TrustStore {
    directory: PathBuf,
    identity: Arc<Ed25519KeyPair>,
}
impl TrustStore {
    pub fn open(directory: impl Into<PathBuf>) -> Result<Self> {
        let directory = directory.into();
        fs::create_dir_all(&directory)?;
        let _lock = Self::lock_at(&directory)?;
        let path = directory.join("identity.pk8");
        if !path.try_exists()? {
            let generated = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
                .map_err(|_| anyhow::anyhow!("无法生成设备身份"))?;
            crate::private_file::write_atomic(&path, generated.as_ref())?;
        }
        let bytes = read_private(&path)?;
        let identity = Ed25519KeyPair::from_pkcs8(&bytes)
            .map_err(|_| anyhow::anyhow!("设备私钥损坏，原文件已保留；不会自动更换身份"))?;
        let value = Self {
            directory: directory.clone(),
            identity: Arc::new(identity),
        };
        value.load()?.validate()?;
        Ok(value)
    }
    pub(crate) fn lock_at(directory: &Path) -> Result<FileLock> {
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(directory.join("store.lock"))?;
        FileExt::lock_exclusive(&file)?;
        Ok(FileLock(file))
    }
    fn load(&self) -> Result<Book> {
        let path = self.directory.join("peers.json");
        if !path.try_exists()? {
            return Ok(Book::default());
        }
        let book: Book = serde_json::from_slice(&read_private(&path)?)
            .map_err(|_| anyhow::anyhow!("设备授权配置损坏，原文件已保留"))?;
        book.validate()?;
        Ok(book)
    }
    fn update(&self, change: impl FnOnce(&mut Book) -> Result<()>) -> Result<()> {
        let _lock = Self::lock_at(&self.directory)?;
        let mut book = self.load()?;
        change(&mut book)?;
        book.validate()?;
        let bytes = Zeroizing::new(serde_json::to_vec_pretty(&book)?);
        crate::private_file::write_atomic(&self.directory.join("peers.json"), &bytes)?;
        Ok(())
    }
    pub fn card(&self, name: &str) -> PublicDevice {
        PublicDevice {
            name: name.into(),
            public_key: hex::encode(self.identity.public_key().as_ref()),
        }
    }
    pub fn sign(&self, data: &[u8]) -> String {
        hex::encode(self.identity.sign(data).as_ref())
    }
    pub fn peers(&self) -> Result<Vec<Peer>> {
        Ok(self.load()?.peers)
    }
    pub fn peer(&self, id: &str) -> Result<Option<Peer>> {
        Ok(self.load()?.peers.into_iter().find(|p| p.device.id() == id))
    }
    pub fn remember(&self, device: PublicDevice) -> Result<()> {
        device.validate()?;
        self.update(|b| {
            if let Some(peer) = b.peers.iter_mut().find(|p| p.device.id() == device.id()) {
                peer.device = device;
            } else {
                b.peers.push(Peer {
                    device,
                    inbound: None,
                    revoked: false,
                    legacy_ids: Vec::new(),
                });
            }
            Ok(())
        })
    }
    pub fn authorize(
        &self,
        device: PublicDevice,
        permissions: Permissions,
        legacy: Option<&str>,
    ) -> Result<()> {
        device.validate()?;
        permissions.validate()?;
        self.update(|b| {
            if let Some(alias) = legacy {
                if let Some(existing) = b
                    .peers
                    .iter()
                    .find(|p| p.legacy_ids.iter().any(|id| id == alias))
                {
                    ensure!(
                        existing.device.id() == device.id() && !existing.revoked,
                        "旧授权已迁移或撤销，不能用旧凭据重新授权"
                    );
                }
            }
            if !b.peers.iter().any(|p| p.device.id() == device.id()) {
                b.peers.push(Peer {
                    device: device.clone(),
                    inbound: None,
                    revoked: false,
                    legacy_ids: Vec::new(),
                });
            }
            let peer = b
                .peers
                .iter_mut()
                .find(|p| p.device.id() == device.id())
                .unwrap();
            peer.device = device;
            peer.inbound = Some(permissions);
            peer.revoked = false;
            if let Some(alias) = legacy {
                if !peer.legacy_ids.iter().any(|id| id == alias) {
                    peer.legacy_ids.push(alias.into());
                }
            }
            Ok(())
        })
    }
    pub fn revoke(&self, id: &str) -> Result<()> {
        self.update(|b| {
            let peer = b
                .peers
                .iter_mut()
                .find(|p| p.device.id() == id)
                .context("未找到设备授权")?;
            peer.inbound = None;
            peer.revoked = true;
            Ok(())
        })
    }
    pub fn legacy_migratable(&self, alias: &str, peer_id: &str) -> Result<bool> {
        Ok(self
            .load()?
            .peers
            .iter()
            .find(|p| p.legacy_ids.iter().any(|id| id == alias))
            .is_none_or(|p| p.device.id() == peer_id && !p.revoked && p.inbound.is_some()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistent_identity_and_independent_grants_survive_concurrent_windows_and_revocation() {
        let root = std::env::temp_dir().join(format!("eclipse-trust-{}", uuid::Uuid::new_v4()));
        let host = TrustStore::open(root.join("host")).unwrap();
        let a = TrustStore::open(root.join("a")).unwrap();
        let c = TrustStore::open(root.join("c")).unwrap();
        let public = host.card("Host");
        assert_eq!(
            public,
            TrustStore::open(root.join("host")).unwrap().card("Host")
        );
        let a_card = a.card("A");
        let c_card = c.card("C");
        let copy = TrustStore::open(root.join("host")).unwrap();
        let ac = a_card.clone();
        let cc = c_card.clone();
        std::thread::scope(|s| {
            s.spawn(|| {
                host.authorize(ac, Permissions::CONTROL, Some("old-a"))
                    .unwrap()
            });
            s.spawn(|| copy.authorize(cc, Permissions::VIEW, None).unwrap());
        });
        assert_eq!(host.peers().unwrap().len(), 2);
        let proof = a.sign(b"new session");
        a_card.verify(b"new session", &proof).unwrap();
        assert!(a_card.verify(b"another session", &proof).is_err());
        assert!(c_card.verify(b"new session", &proof).is_err());
        host.revoke(&a_card.id()).unwrap();
        assert!(host.peer(&a_card.id()).unwrap().unwrap().inbound.is_none());
        assert_eq!(
            host.peer(&c_card.id()).unwrap().unwrap().inbound,
            Some(Permissions::VIEW)
        );
        assert!(host
            .authorize(a_card.clone(), Permissions::CONTROL, Some("old-a"))
            .is_err());
        assert!(
            a.peer(&public.id()).unwrap().is_none(),
            "inbound grant is not reverse authorization"
        );
        let identity = fs::read(root.join("host/identity.pk8")).unwrap();
        fs::write(root.join("host/identity.pk8"), b"corrupt").unwrap();
        assert!(TrustStore::open(root.join("host")).is_err());
        assert_eq!(
            fs::read(root.join("host/identity.pk8")).unwrap(),
            b"corrupt"
        );
        assert!(!identity.is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
