//! A configuration directory owns both its client state and its host profile.
//! Windows runs one selected host from protected storage and mirrors host data
//! back as the owning user; a different frontend never edits that profile.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

pub fn directory_key(directory: &Path) -> Result<String> {
    let absolute = if directory.is_absolute() {
        directory.to_path_buf()
    } else {
        std::env::current_dir()?.join(directory)
    };
    let mut normalized = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(part.as_os_str()),
        }
    }
    let mut ancestor = normalized.as_path();
    let mut tail = Vec::new();
    while !ancestor.try_exists()? {
        tail.push(
            ancestor
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("configuration directory unavailable"))?,
        );
        ancestor = ancestor
            .parent()
            .ok_or_else(|| anyhow::anyhow!("configuration directory unavailable"))?;
    }
    let mut path = ancestor.canonicalize()?;
    for part in tail.into_iter().rev() {
        path.push(part);
    }
    #[cfg(windows)]
    let bytes = path.to_string_lossy().to_lowercase().into_bytes();
    #[cfg(not(windows))]
    let bytes = path.as_os_str().as_encoded_bytes().to_vec();
    Ok(hex::encode(Sha256::digest(bytes)))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Info {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub version: u32,
    pub info: Info,
    pub user_data: PathBuf,
    pub revision: String,
    pub build: String,
}
impl Binding {
    pub fn new(user_data: PathBuf, revision: String, build: String) -> Result<Self> {
        let id = directory_key(&user_data)?;
        let label = if user_data.file_name().is_some_and(|s| s == "data") {
            user_data
                .parent()
                .and_then(Path::parent)
                .and_then(Path::file_name)
        } else {
            user_data.file_name()
        };
        let name = label
            .map(|s| {
                s.to_string_lossy()
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(64)
                    .collect()
            })
            .unwrap_or_else(|| "Eclipse".into());
        let value = Self {
            version: 1,
            info: Info { id, name },
            user_data,
            revision,
            build,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1 && valid_id(&self.info.id) && self.user_data.is_absolute(),
            "Invalid Eclipse host profile"
        );
        ensure!(
            self.revision.len() == 40
                && self.revision.bytes().all(|c| c.is_ascii_hexdigit())
                && self.build.len() == 64
                && self.build.bytes().all(|c| c.is_ascii_hexdigit()),
            "Invalid host profile build binding"
        );
        ensure!(
            !self.info.name.is_empty()
                && self.info.name.chars().count() <= 64
                && !self.info.name.chars().any(char::is_control),
            "Invalid host profile name"
        );
        Ok(())
    }
    pub fn matches_build(&self, revision: &str, build: &str) -> bool {
        self.revision == revision && self.build == build
    }
    pub fn source(&self) -> PathBuf {
        self.user_data.join("server")
    }
    pub fn config(&self, root: &Path) -> crate::Config {
        let root = root.join("profiles").join(&self.info.id);
        crate::Config {
            data_dir: root.join("data/eclipse"),
            state_dir: root.join("state/eclipse"),
            signaling_host: String::new(),
        }
    }
}
pub fn valid_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Runs as the ordinary GUI user, before requesting installation authorization.
pub fn prepare_source(config: &crate::Config) -> Result<()> {
    let source = config.at("server");
    if source.join("device-trust/connections.json").try_exists()? {
        ensure!(
            source.join("device-trust/identity.pk8").is_file(),
            "服务端身份文件缺失，原配置已保留；请恢复此目录的备份后重试。"
        );
        crate::device_book::Store::open(source.join("device-trust"))?.read()?;
        crate::device_trust::TrustStore::open(source.join("device-trust"))?;
        return Ok(());
    }
    let client = crate::device_book::Store::open(config.at("device-trust"))?.read()?;
    let store = crate::device_book::Store::open(source.join("device-trust"))?;
    crate::device_trust::TrustStore::open(source.join("device-trust"))?;
    let mut book = crate::device_book::Book::default();
    book.legacy_imported = true;
    book.sharing = client.sharing;
    book.sharing.auto_start = false;
    for mut service in client.services.into_iter().filter(|s| s.host_enabled) {
        let token = crate::device_trust::read_private(&service.token_file)?;
        service.token_file =
            store.store_service_credential(&service.id, std::str::from_utf8(&token)?)?;
        if let Some(path) = &service.certificate {
            let bytes = crate::device_trust::read_private(path)?;
            let destination = source
                .join("device-trust")
                .join(format!("service-ca-{}.pem", service.id));
            crate::private_file::write_atomic(&destination, &bytes)?;
            service.certificate = Some(destination);
        }
        book.services.push(service);
    }
    store.edit(|value| {
        *value = book;
        Ok(())
    })?;
    Ok(())
}

/// Destination writes inherit the caller's token. Privileged callers stage
/// only under protected roots; exporting into user storage requires impersonation.
pub struct DataSnapshot {
    source: PathBuf,
    files: Vec<(PathBuf, zeroize::Zeroizing<Vec<u8>>)>,
}
impl DataSnapshot {
    pub fn digest(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        for (path, bytes) in &self.files {
            hash.update(path.as_os_str().as_encoded_bytes());
            hash.update(Sha256::digest(bytes.as_slice()));
        }
        hash.finalize().into()
    }
    pub fn capture(source: &Path) -> Result<Self> {
        let files = crate::profile_storage::snapshot(source)?;
        let mut captured = Vec::new();
        for (relative, expected) in &files {
            let bytes = fs::read(source.join(relative))?;
            ensure!(
                <[u8; 32]>::from(Sha256::digest(&bytes)) == *expected,
                "Host profile changed during copy"
            );
            let bytes = if relative == Path::new("device-trust/connections.json") {
                serde_json::to_vec_pretty(
                    &crate::device_book::Store::open(source.join("device-trust"))?.read()?,
                )?
            } else {
                bytes
            };
            captured.push((relative.clone(), zeroize::Zeroizing::new(bytes)));
        }
        ensure!(
            crate::profile_storage::snapshot(source)? == files,
            "Host profile changed during copy; retry after the operation finishes"
        );
        Ok(Self {
            source: source.to_path_buf(),
            files: captured,
        })
    }
    pub fn publish(&self, destination: &Path) -> Result<()> {
        let marker = destination.join(".eclipse-snapshot.json");
        let previous: std::collections::BTreeMap<PathBuf, String> = match fs::read(&marker) {
            Ok(bytes) => {
                ensure!(
                    bytes.len() <= 2 * 1024 * 1024,
                    "Host snapshot inventory is too large"
                );
                serde_json::from_slice(&bytes)?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
            Err(e) => return Err(e.into()),
        };
        for relative in previous.keys() {
            ensure!(
                !relative.as_os_str().is_empty()
                    && relative
                        .components()
                        .all(|c| matches!(c, Component::Normal(_))),
                "Invalid host snapshot inventory path"
            );
        }
        let mut published = std::collections::BTreeMap::new();
        for (relative, original) in &self.files {
            let mut bytes = zeroize::Zeroizing::new(original.to_vec());
            if relative == Path::new("device-trust/connections.json") {
                let mut book: crate::device_book::Book = serde_json::from_slice(&bytes)?;
                book.sharing.validate()?;
                for service in &mut book.services {
                    for path in std::iter::once(&mut service.token_file)
                        .chain(service.certificate.iter_mut())
                    {
                        let suffix = path.strip_prefix(&self.source).map_err(|_| {
                            anyhow::anyhow!(
                                "服务凭据不在此配置内，请在原配置中重新保存连接服务后再迁移。"
                            )
                        })?;
                        ensure!(
                            self.files.iter().any(|(relative, _)| relative == suffix),
                            "服务凭据缺失，原配置已保留。"
                        );
                        if let Ok(inside_store) = suffix.strip_prefix("device-trust") {
                            // Persist store-relative paths: a raw directory copy
                            // must not keep references to its previous location.
                            *path = inside_store.to_path_buf();
                        } else {
                            // Historical hosts kept credentials beside the trust
                            // store. Materialize those inside it before persisting
                            // a relative reference; never use '..' escape paths.
                            let alias = PathBuf::from("host-credentials").join(hex::encode(
                                Sha256::digest(suffix.as_os_str().as_encoded_bytes()),
                            ));
                            let bytes = &self
                                .files
                                .iter()
                                .find(|(relative, _)| relative == suffix)
                                .expect("validated captured credential")
                                .1;
                            let relative = PathBuf::from("device-trust").join(&alias);
                            crate::private_file::write_atomic(&destination.join(&relative), bytes)?;
                            published
                                .insert(relative, hex::encode(Sha256::digest(bytes.as_slice())));
                            *path = alias;
                        }
                    }
                }
                bytes = zeroize::Zeroizing::new(serde_json::to_vec_pretty(&book)?);
            }
            let target = destination.join(relative);
            if fs::read(&target).ok().as_deref() != Some(bytes.as_slice()) {
                crate::private_file::write_atomic(&target, &bytes)?;
            }
            published.insert(
                relative.clone(),
                hex::encode(Sha256::digest(bytes.as_slice())),
            );
        }
        // Deleted secrets (for example a cleared pairing password) must not reappear
        // when the application folder is copied to another computer.
        for (relative, expected) in previous
            .iter()
            .filter(|(relative, _)| !published.contains_key(*relative))
        {
            let target = destination.join(relative);
            match fs::read(&target) {
                Ok(bytes) => {
                    ensure!(
                        hex::encode(Sha256::digest(&bytes)) == *expected,
                        "Host backup was edited separately; its changed file was preserved"
                    );
                    fs::remove_file(target)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        crate::private_file::write_atomic(&marker, &serde_json::to_vec_pretty(&published)?)?;
        Ok(())
    }
}
pub fn copy_data(source: &Path, destination: &Path) -> Result<()> {
    DataSnapshot::capture(source)?.publish(destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_profiles_separate_client_identity_and_preserve_copied_server_state() {
        let root =
            std::env::temp_dir().join(format!("eclipse-host-profiles-{}", uuid::Uuid::new_v4()));
        let first = crate::Config {
            data_dir: root.join("one/data"),
            state_dir: root.join("one/state"),
            signaling_host: String::new(),
        };
        let second = crate::Config {
            data_dir: root.join("two/data"),
            state_dir: root.join("two/state"),
            signaling_host: String::new(),
        };
        let client = crate::device_trust::TrustStore::open(first.at("device-trust")).unwrap();
        prepare_source(&first).unwrap();
        prepare_source(&second).unwrap();
        let host = crate::device_trust::TrustStore::open(first.at("server/device-trust")).unwrap();
        assert_ne!(client.card("Client").id(), host.card("Server").id());
        let a = Binding::new(first.data_dir.clone(), "a".repeat(40), "b".repeat(64)).unwrap();
        let b = Binding::new(second.data_dir.clone(), "a".repeat(40), "b".repeat(64)).unwrap();
        assert_ne!(a.info.id, b.info.id);
        assert_ne!(a.config(&root).data_dir, b.config(&root).data_dir);
        copy_data(&a.source(), &a.config(&root).data_dir).unwrap();
        let copied =
            crate::device_trust::TrustStore::open(a.config(&root).at("device-trust")).unwrap();
        assert_eq!(copied.card("Copied").id(), host.card("Server").id());
        let store = crate::device_book::Store::open(a.source().join("device-trust")).unwrap();
        let service_id = uuid::Uuid::new_v4().to_string();
        let credential = store
            .store_service_credential(&service_id, &"ab".repeat(32))
            .unwrap();
        let mut service = crate::device_book::Service {
            id: service_id,
            name: "Fixture".into(),
            address: "signal.example:8443".into(),
            token_file: credential,
            certificate: None,
            host_enabled: true,
            published_name: "Fixture".into(),
        };
        store.save_service(service.clone()).unwrap();
        copy_data(&a.source(), &a.config(&root).data_dir).unwrap();
        let copied_book = crate::device_book::Store::open(a.config(&root).at("device-trust"))
            .unwrap()
            .read()
            .unwrap();
        assert!(copied_book.services[0]
            .token_file
            .starts_with(&a.config(&root).data_dir));
        let assert_movable = || {
            let destination = a.config(&root).data_dir;
            let moved = root.join("transported-server");
            fs::rename(&destination, &moved).unwrap();
            let moved_book = crate::device_book::Store::open(moved.join("device-trust"))
                .unwrap()
                .read()
                .unwrap();
            assert!(moved_book.services[0].token_file.starts_with(&moved));
            assert_eq!(
                fs::read_to_string(&moved_book.services[0].token_file).unwrap(),
                "ab".repeat(32)
            );
            fs::rename(&moved, &destination).unwrap();
        };
        assert_movable();
        let legacy_credential = a.source().join("service-credentials/legacy-token");
        crate::private_file::write_atomic(&legacy_credential, "ab".repeat(32).as_bytes()).unwrap();
        service.token_file = legacy_credential;
        store.save_service(service.clone()).unwrap();
        copy_data(&a.source(), &a.config(&root).data_dir).unwrap();
        assert_movable();
        service.token_file = root.join("outside-profile-secret");
        store.save_service(service.clone()).unwrap();
        assert!(
            copy_data(&a.source(), &a.config(&root).data_dir).is_err(),
            "a host must not inherit a privileged arbitrary credential pathname"
        );
        store
            .edit(|book| {
                book.services.clear();
                Ok(())
            })
            .unwrap();
        let secret = Path::new("device-trust/retired-secret");
        crate::private_file::write_atomic(&a.source().join(secret), b"temporary secret").unwrap();
        copy_data(&a.source(), &a.config(&root).data_dir).unwrap();
        assert!(a.config(&root).data_dir.join(secret).is_file());
        fs::remove_file(a.source().join(secret)).unwrap();
        copy_data(&a.source(), &a.config(&root).data_dir).unwrap();
        assert!(!a.config(&root).data_dir.join(secret).exists());
        fs::remove_file(first.at("server/device-trust/identity.pk8")).unwrap();
        assert!(prepare_source(&first).is_err());
        assert!(!first.at("server/device-trust/identity.pk8").exists());
        assert!(a.matches_build(&"a".repeat(40), &"b".repeat(64)));
        assert!(!a.matches_build(&"c".repeat(40), &"b".repeat(64)));
        fs::remove_dir_all(root).unwrap();
    }
}
