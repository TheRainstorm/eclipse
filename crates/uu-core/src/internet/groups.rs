//! Private device-group credentials. A group ID is public routing metadata;
//! possession of its token authorizes only that group's signaling namespace.
use anyhow::{ensure, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::sync::watch;
use uuid::Uuid;
use zeroize::Zeroizing;

const MAX_BYTES: usize = 128 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    pub token: Zeroizing<String>,
}
impl Group {
    pub fn new(name: String) -> Result<Self> {
        let mut bytes = Zeroizing::new([0u8; 32]);
        rand::rngs::OsRng.fill_bytes(bytes.as_mut());
        let group = Self {
            id: Uuid::new_v4(),
            name: name.trim().into(),
            token: hex::encode(bytes.as_ref()).into(),
        };
        group.validate()?;
        Ok(group)
    }
    fn validate(&self) -> Result<()> {
        ensure!(!self.id.is_nil(), "private group ID must not be nil");
        ensure!(
            !self.name.trim().is_empty()
                && self.name.chars().count() <= 64
                && !self.name.chars().any(char::is_control),
            "invalid private group name"
        );
        ensure!(
            self.token.len() == 64 && hex::decode(self.token.as_str()).is_ok(),
            "private group token must contain 64 hex characters"
        );
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Groups {
    pub version: u32,
    pub groups: Vec<Group>,
}
impl Default for Groups {
    fn default() -> Self {
        Self {
            version: 1,
            groups: Vec::new(),
        }
    }
}
impl Groups {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1 && self.groups.len() <= 256,
            "invalid private group registry version or capacity"
        );
        let mut ids = HashSet::new();
        for (index, group) in self.groups.iter().enumerate() {
            group.validate()?;
            ensure!(ids.insert(group.id), "duplicate private group ID");
            ensure!(
                !self.groups[..index]
                    .iter()
                    .any(|old| old.token.eq_ignore_ascii_case(&group.token)),
                "duplicate private group token"
            );
        }
        Ok(())
    }
    fn decode(bytes: &[u8]) -> Result<Self> {
        // Serde errors can echo secret field values. Keep this boundary opaque.
        let groups: Self = serde_json::from_slice(bytes)
            .map_err(|_| anyhow::anyhow!("invalid private group registry JSON"))?;
        groups.validate()?;
        Ok(groups)
    }
    pub fn load(path: &Path) -> Result<Self> {
        Self::decode(&read(path)?)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let bytes = Zeroizing::new(serde_json::to_vec_pretty(self)?);
        ensure!(
            bytes.len() <= MAX_BYTES,
            "private group registry exceeds 128 KiB"
        );
        crate::private_file::write_atomic(path, &bytes)?;
        Ok(())
    }
    /// Cooperative writers use one OS lock across load, edit and publication.
    /// Run the CLI as the service owner so atomic replacement keeps ownership.
    pub fn edit<R>(path: &Path, edit: impl FnOnce(&mut Self) -> Result<R>) -> Result<R> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        let _lock = crate::device_trust::TrustStore::lock_at(parent)?;
        let mut groups = if path.try_exists()? {
            Self::load(path)?
        } else {
            Self::default()
        };
        let result = edit(&mut groups)?;
        groups.save(path)?;
        Ok(result)
    }
}
fn read(path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(Vec::new());
    std::fs::File::open(path)?
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_BYTES,
        "private group registry exceeds 128 KiB"
    );
    Ok(bytes)
}

/// The same public group identity is used by invitations and WSS requests.
/// Other query fields retain their own meaning but never select a namespace.
pub fn query_group(query: &str) -> Result<Uuid> {
    let mut group = None;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if key != "group" {
            continue;
        }
        ensure!(
            group.is_none(),
            "connection service URL contains duplicate groups"
        );
        let id = Uuid::parse_str(&value)
            .map_err(|_| anyhow::anyhow!("invalid connection service group ID"))?;
        ensure!(!id.is_nil(), "connection service group ID must not be nil");
        group = Some(id);
    }
    group.ok_or_else(|| anyhow::anyhow!("connection service URL requires a private group"))
}

pub(super) struct Access {
    groups: RwLock<Groups>,
}
impl Access {
    pub fn new(groups: Groups) -> Result<Self> {
        groups.validate()?;
        Ok(Self {
            groups: RwLock::new(groups),
        })
    }
    fn replace(&self, groups: Groups) -> Result<()> {
        groups.validate()?;
        *self.groups.write().unwrap() = groups;
        Ok(())
    }
    pub fn authenticate(
        &self,
        request: &tokio_tungstenite::tungstenite::handshake::server::Request,
    ) -> Option<Uuid> {
        if request.uri().path() != "/v1/signal" {
            return None;
        }
        let group = query_group(request.uri().query().unwrap_or("")).ok()?;
        let header = request.headers().get("authorization")?;
        let groups = self.groups.read().unwrap();
        let token = groups.groups.iter().find(|g| g.id == group)?.token.as_str();
        let expected = Zeroizing::new(format!("Bearer {token}"));
        if header.as_bytes().ct_eq(expected.as_bytes()).into() {
            Some(group)
        } else {
            None
        }
    }
    pub async fn reload(self: Arc<Self>, path: PathBuf, mut stop: watch::Receiver<bool>) {
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last: Option<[u8; 32]> = None;
        let mut warned = false;
        loop {
            tokio::select! {
                _ = stop.changed() => if *stop.borrow() || stop.has_changed().is_err() { break; },
                _ = timer.tick() => {
                    let path = path.clone();
                    let loaded = tokio::task::spawn_blocking(move || read(&path)).await;
                    let result = match loaded {
                        Ok(Ok(bytes)) => {
                            let digest: [u8; 32] = Sha256::digest(&bytes).into();
                            if last == Some(digest) { continue; }
                            last = Some(digest);
                            Groups::decode(&bytes).and_then(|groups| self.replace(groups))
                        },
                        _ => Err(anyhow::anyhow!("cannot read private group registry")),
                    };
                    match result {
                        Ok(()) => { warned = false; tracing::info!("private connection groups reloaded"); },
                        Err(_) if !warned => { warned = true; tracing::warn!("private connection groups could not reload; retaining the previous valid configuration"); },
                        Err(_) => {},
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn group_registry_edits_serialize_and_failed_validation_keeps_the_previous_bytes() {
        let root = std::env::temp_dir().join(format!("eclipse-private-groups-{}", Uuid::new_v4()));
        let path = root.join("groups.json");
        let barrier = Arc::new(std::sync::Barrier::new(6));
        std::thread::scope(|threads| {
            for index in 0..6 {
                let path = &path;
                let barrier = barrier.clone();
                threads.spawn(move || {
                    barrier.wait();
                    Groups::edit(path, |groups| {
                        groups
                            .groups
                            .push(Group::new(format!("Generated group {index}"))?);
                        Ok(())
                    })
                    .unwrap();
                });
            }
        });
        let groups = Groups::load(&path).unwrap();
        assert_eq!(groups.groups.len(), 6);
        let original = std::fs::read(&path).unwrap();
        assert!(Groups::edit(&path, |groups| {
            groups.groups.push(groups.groups[0].clone());
            Ok(())
        })
        .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(Groups::edit::<()>(&path, |groups| {
            groups.groups.clear();
            anyhow::bail!("generated edit failure");
        })
        .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::write(&path, vec![b'x'; MAX_BYTES + 1]).unwrap();
        assert!(Groups::load(&path).is_err());
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            (MAX_BYTES + 1) as u64
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn group_query_and_bearer_must_select_exactly_the_same_private_namespace() {
        let first = Group::new("Generated first".into()).unwrap();
        let second = Group::new("Generated second".into()).unwrap();
        let request = |query: &str, token: &str| {
            tokio_tungstenite::tungstenite::http::Request::builder()
                .uri(format!("/v1/signal{query}"))
                .header("authorization", format!("Bearer {token}"))
                .body(())
                .unwrap()
        };
        let access = Access::new(Groups {
            version: 1,
            groups: vec![first.clone(), second.clone()],
        })
        .unwrap();
        let a = format!("?group={}", first.id);
        let b = format!("?group={}", second.id);
        assert_eq!(
            access.authenticate(&request(&a, &first.token)),
            Some(first.id)
        );
        assert_eq!(
            access.authenticate(&request(&b, &second.token)),
            Some(second.id)
        );
        for query in [
            String::new(),
            b,
            "?group=invalid".into(),
            format!("?group={}", Uuid::nil()),
            format!("{a}&group={}", first.id),
            format!("{a}&%67roup={}", second.id),
            format!("?group={}", Uuid::new_v4()),
        ] {
            assert!(access
                .authenticate(&request(&query, &first.token))
                .is_none());
        }
        assert!(access.authenticate(&request(&a, &second.token)).is_none());
        assert_eq!(
            query_group(&format!("other=kept&%67roup={}", first.id)).unwrap(),
            first.id
        );
        let mut duplicate_token = second.clone();
        duplicate_token.token = first.token.to_ascii_uppercase().into();
        assert!(Groups {
            version: 1,
            groups: vec![first, duplicate_token]
        }
        .validate()
        .is_err());
    }
}
