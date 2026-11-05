//! Service-owned self-hosted entrances. The GUI transfers credential contents;
//! only the protected host chooses paths and writes its configuration.
use crate::device_book::{Destination, Service, Store};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Inspect,
    Save {
        revision: String,
        entry: Entry,
        token: Option<Zeroizing<String>>,
        certificate: CertificateChange,
    },
    Remove {
        revision: String,
        id: String,
    },
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum CertificateChange {
    #[default]
    Keep,
    Remove,
    Set {
        pem: String,
    },
}
/// No private paths or credential contents are returned to an ordinary GUI.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// Empty when creating; only the service allocates a new identity.
    pub id: String,
    pub name: String,
    pub address: String,
    pub published_name: String,
    pub host_enabled: bool,
    pub token_configured: bool,
    pub certificate_configured: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub revision: String,
    pub entries: Vec<Entry>,
    pub restart_required: bool,
}
fn revision(services: &[Service]) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(services)?)))
}
fn snapshot(store: &Store, active: &[Service]) -> Result<Snapshot> {
    let book = store.read()?;
    let current = revision(&book.services)?;
    Ok(Snapshot {
        restart_required: current != revision(active)?,
        revision: current,
        entries: book
            .services
            .iter()
            .map(|s| Entry {
                id: s.id.clone(),
                name: s.name.clone(),
                address: s.address.clone(),
                published_name: s.published_name.clone(),
                host_enabled: s.host_enabled,
                token_configured: !s.token_file.as_os_str().is_empty(),
                certificate_configured: s.certificate.is_some(),
            })
            .collect(),
    })
}
pub fn execute(directory: &Path, active: &[Service], request: Request) -> Result<Snapshot> {
    let store = Store::open(directory)?;
    let mut created = Vec::<PathBuf>::new();
    let result = match request {
        Request::Inspect => Ok(()),
        Request::Save { revision: expected, entry, token, certificate } => store.edit(|book| {
            ensure!(revision(&book.services)? == expected, "{}", uu_i18n::text("服务器配置已被其他窗口修改，请刷新后重试"));
            let previous = if entry.id.is_empty() { None }
                else { Some(book.service(&entry.id)?.clone()) };
            let id = previous.as_ref().map(|s| s.id.clone()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let token_file = if let Some(token) = token.as_ref().filter(|t| !t.trim().is_empty()) {
                let path = store.store_service_credential(&id, token)?;
                created.push(path.clone()); path
            } else {
                previous.as_ref().map(|s| s.token_file.clone())
                    .context(uu_i18n::text("请填写服务器访问令牌"))?
            };
            let certificate = match certificate {
                CertificateChange::Keep => previous.as_ref().and_then(|s| s.certificate.clone()),
                CertificateChange::Remove => None,
                CertificateChange::Set { pem } => {
                    ensure!(!pem.is_empty() && pem.len() <= 64 * 1024, "{}", uu_i18n::text("服务器证书大小无效"));
                    let path = directory.join(format!("service-ca-{id}-{}.pem", uuid::Uuid::new_v4()));
                    crate::private_file::write_atomic(&path, pem.as_bytes())?;
                    created.push(path.clone()); Some(path)
                },
            };
            let service = Service {
                id, name: entry.name.trim().into(), address: entry.address.trim().into(),
                token_file, certificate, host_enabled: entry.host_enabled,
                published_name: entry.published_name.trim().into(),
            };
            crate::internet::Settings { server: service.address.clone(),
                server_token: service.token_file.clone(), server_ca: service.certificate.clone(),
                ..Default::default() }.self_hosted()?.validate_local()?;
            if let Some(value) = book.services.iter_mut().find(|s| s.id == service.id) { *value = service; }
            else { book.services.push(service); }
            Ok(())
        }),
        Request::Remove { revision: expected, id } => store.edit(|book| {
            ensure!(revision(&book.services)? == expected, "{}", uu_i18n::text("服务器配置已被其他窗口修改，请刷新后重试"));
            book.service(&id)?;
            ensure!(!book.devices.iter().any(|p| p.redirect.is_none() && p.routes.iter()
                .any(|r| matches!(&r.destination, Destination::Vps { service, .. } if service == &id))),
                "{}", uu_i18n::text("该服务器仍用于设备连接，请先移除对应的连接地址"));
            book.services.retain(|s| s.id != id);
            Ok(())
        }),
    };
    if result.is_err() {
        // Even an error after an atomic write must not remove a credential now
        // referenced by durable configuration. Unreadable state is preserved.
        if let Ok(book) = store.read() {
            for path in created {
                if !book
                    .services
                    .iter()
                    .any(|s| s.token_file == path || s.certificate.as_ref() == Some(&path))
                {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }
    result?;
    // Old credential versions stay alive for admitted streams until restart.
    snapshot(&store, active)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("eclipse-entrances-{}", uuid::Uuid::new_v4())))
        }
        fn request(&self, active: &[Service], request: Request) -> Result<Snapshot> {
            execute(&self.0, active, request)
        }
        fn initial(&self) -> Snapshot {
            self.request(&[], Request::Inspect).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn entry() -> Entry {
        Entry {
            name: "Generated host".into(),
            address: "wss://signal.example/v1/signal".into(),
            published_name: "generated-host".into(),
            host_enabled: true,
            ..Default::default()
        }
    }
    fn save(
        revision: String,
        entry: Entry,
        token: Option<String>,
        certificate: CertificateChange,
    ) -> Request {
        Request::Save {
            revision,
            entry,
            token: token.map(Zeroizing::new),
            certificate,
        }
    }
    #[test]
    fn service_entrances_save_rotate_and_remove_without_exposing_credentials() {
        let f = Fixture::new();
        let first = f
            .request(
                &[],
                save(
                    f.initial().revision,
                    entry(),
                    Some("ab".repeat(32)),
                    CertificateChange::Keep,
                ),
            )
            .unwrap();
        assert!(first.restart_required);
        assert!(first.entries[0].token_configured);
        let active = Store::open(&f.0).unwrap().read().unwrap().services;
        let old = active[0].token_file.clone();
        let encoded = serde_json::to_string(&first).unwrap();
        assert!(!encoded.contains("service-access-") && !encoded.contains(&"ab".repeat(32)));
        assert!(
            !f.request(&active, Request::Inspect)
                .unwrap()
                .restart_required
        );
        let second = f
            .request(
                &active,
                save(
                    first.revision,
                    first.entries[0].clone(),
                    Some("cd".repeat(32)),
                    CertificateChange::Keep,
                ),
            )
            .unwrap();
        assert!(second.restart_required);
        assert_eq!(std::fs::read_to_string(&old).unwrap(), "ab".repeat(32));
        let after = f
            .request(
                &active,
                Request::Remove {
                    revision: second.revision,
                    id: second.entries[0].id.clone(),
                },
            )
            .unwrap();
        assert!(after.entries.is_empty());
        assert!(old.exists());
    }
    #[test]
    fn failed_credentials_and_conflicts_preserve_config_and_clean_only_new_files() {
        let f = Fixture::new();
        let initial = f.initial();
        assert!(f
            .request(
                &[],
                save(
                    initial.revision.clone(),
                    entry(),
                    None,
                    CertificateChange::Keep
                )
            )
            .is_err());
        assert!(f
            .request(
                &[],
                save(
                    initial.revision.clone(),
                    entry(),
                    Some("ab".repeat(32)),
                    CertificateChange::Set {
                        pem: "invalid PEM".into()
                    }
                )
            )
            .is_err());
        assert!(f.initial().entries.is_empty());
        assert!(!std::fs::read_dir(&f.0).unwrap().any(|v| v
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("service-")));
        let saved = f
            .request(
                &[],
                save(
                    initial.revision.clone(),
                    entry(),
                    Some("ab".repeat(32)),
                    CertificateChange::Keep,
                ),
            )
            .unwrap();
        assert!(f
            .request(
                &[],
                save(
                    initial.revision,
                    saved.entries[0].clone(),
                    Some("cd".repeat(32)),
                    CertificateChange::Keep
                )
            )
            .is_err());
        assert_eq!(f.initial().revision, saved.revision);
        let mut unknown = entry();
        unknown.id = uuid::Uuid::new_v4().to_string();
        assert!(f
            .request(
                &[],
                save(
                    saved.revision,
                    unknown,
                    Some("ab".repeat(32)),
                    CertificateChange::Keep
                )
            )
            .is_err());
    }
}
