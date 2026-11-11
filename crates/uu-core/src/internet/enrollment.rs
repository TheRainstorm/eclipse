//! A single connection-service invitation carries public TLS trust and a
//! private service credential. Each configuration owns its imported files.
use crate::device_book::{Service, Store};
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rustls_pki_types::{pem::PemObject, CertificateDer};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

const PREFIX: &str = "eclipse://connect/";
const MAX_CODE: usize = 128 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Enrollment {
    pub version: u32,
    pub name: String,
    pub url: String,
    pub token: Zeroizing<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca: Option<String>,
}
impl Enrollment {
    pub fn new(
        name: String,
        url: String,
        token: Zeroizing<String>,
        ca: Option<String>,
    ) -> Result<Self> {
        let value = Self {
            version: 1,
            name: name.trim().into(),
            url: normalize_url(&url)?,
            token: Zeroizing::new(token.trim().into()),
            ca: ca.map(|pem| format!("{}\n", pem.trim())),
        };
        value.validate()?;
        Ok(value)
    }
    pub fn decode(input: &str) -> Result<Self> {
        let input = input.trim();
        ensure!(input.len() <= MAX_CODE, "连接服务接入码过长");
        let encoded = input
            .strip_prefix(PREFIX)
            .context("请粘贴 Eclipse 连接服务接入码")?;
        let bytes = Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(encoded)
                .context("连接服务接入码格式无效")?,
        );
        let value: Self = serde_json::from_slice(&bytes).context("连接服务接入码内容无效")?;
        ensure!(
            value.version == 1,
            "连接服务接入码版本不受支持，请更新 Eclipse"
        );
        Self::new(value.name, value.url, value.token, value.ca)
    }
    pub fn encode(&self) -> Result<Zeroizing<String>> {
        self.validate()?;
        let bytes = Zeroizing::new(serde_json::to_vec(self)?);
        let code = Zeroizing::new(format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(&bytes)));
        ensure!(code.len() <= MAX_CODE, "连接服务接入码过长");
        Ok(code)
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1,
            "连接服务接入码版本不受支持，请更新 Eclipse"
        );
        ensure!(
            !self.name.trim().is_empty()
                && self.name.chars().count() <= 64
                && !self.name.chars().any(char::is_control),
            "连接服务名称应为1至64个字符"
        );
        normalize_url(&self.url)?;
        ensure!(
            self.token.len() == 64 && self.token.bytes().all(|b| b.is_ascii_hexdigit()),
            "连接服务访问令牌无效"
        );
        if let Some(pem) = &self.ca {
            ensure!(pem.len() <= 64 * 1024, "连接服务证书过大");
            let mut roots = rustls::RootCertStore::empty();
            for certificate in CertificateDer::pem_slice_iter(pem.as_bytes()) {
                roots.add(certificate.context("连接服务证书格式无效")?)?;
            }
            ensure!(!roots.is_empty(), "连接服务证书中没有有效的 PEM 证书");
        }
        Ok(())
    }
}
fn normalize_url(address: &str) -> Result<String> {
    let address = address.trim();
    ensure!(
        !address.is_empty() && address.len() <= 2048,
        "连接服务地址无效"
    );
    let value = if address.contains("://") {
        address.into()
    } else {
        format!("wss://{address}/v1/signal")
    };
    let url = url::Url::parse(&value).context("连接服务地址无效")?;
    ensure!(
        url.scheme() == "wss"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.port() != Some(0),
        "连接服务需要有效的 WSS 地址，凭据须由接入码单独提供"
    );
    Ok(url.into())
}

/// Re-import updates the same server entry and keeps device routes valid.
/// Existing streams retain their admitted credential files until retirement.
pub fn import(
    directory: &Path,
    invitation: &Enrollment,
    published_name: &str,
    host_enabled: bool,
) -> Result<Service> {
    invitation.validate()?;
    let store = Store::open(directory)?;
    let address = normalize_url(&invitation.url)?;
    let mut created = Vec::<PathBuf>::new();
    let mut imported = None;
    let result = store.edit(|book| {
        let previous = book
            .services
            .iter()
            .find(|s| normalize_url(&s.address).ok().as_deref() == Some(&address))
            .cloned();
        let id = previous
            .as_ref()
            .map(|s| s.id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let token_file = if let Some(service) = previous.as_ref().filter(|s| {
            super::server::read_token(&s.token_file).ok().as_deref()
                == Some(invitation.token.as_str())
        }) {
            service.token_file.clone()
        } else {
            let path = store.store_service_credential(&id, &invitation.token)?;
            created.push(path.clone());
            path
        };
        let certificate = if let Some(pem) = &invitation.ca {
            if let Some(path) = previous
                .as_ref()
                .and_then(|s| s.certificate.as_ref())
                .filter(|path| std::fs::read_to_string(path).ok().as_deref() == Some(pem))
            {
                Some(path.clone())
            } else {
                let path = directory.join(format!("service-ca-{id}-{}.pem", uuid::Uuid::new_v4()));
                crate::private_file::write_atomic(&path, pem.as_bytes())?;
                created.push(path.clone());
                Some(path)
            }
        } else {
            None
        };
        let service = Service {
            id,
            name: invitation.name.clone(),
            address: address.clone(),
            token_file,
            certificate,
            host_enabled,
            published_name: published_name.into(),
        };
        if let Some(previous) = book.services.iter_mut().find(|s| s.id == service.id) {
            *previous = service.clone();
        } else {
            book.services.push(service.clone());
        }
        imported = Some(service);
        Ok(())
    });
    if result.is_err() {
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
    imported.context("连接服务没有保存成功")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn enrollment() -> Enrollment {
        Enrollment::new(
            "Generated service".into(),
            "SIGNAL.example:443".into(),
            Zeroizing::new("ab".repeat(32)),
            None,
        )
        .unwrap()
    }
    #[test]
    fn invitation_roundtrip_validates_version_tls_and_credentials_without_paths() {
        let invitation = enrollment();
        let encoded = invitation.encode().unwrap();
        let decoded = Enrollment::decode(&encoded).unwrap();
        assert_eq!(decoded.name, invitation.name);
        assert_eq!(decoded.url, "wss://signal.example/v1/signal");
        assert_eq!(decoded.token.as_str(), invitation.token.as_str());
        assert!(decoded.ca.is_none());
        assert!(!encoded.contains("ab".repeat(32).as_str()));
        for address in [
            "http://example.test",
            "wss://user:secret@example.test",
            "wss://example.test/#secret",
        ] {
            assert!(Enrollment::new(
                "Fixture".into(),
                address.into(),
                invitation.token.clone(),
                None
            )
            .is_err());
        }
        let mut value = serde_json::to_value(&invitation).unwrap();
        value["version"] = 2.into();
        assert!(Enrollment::decode(&format!(
            "{PREFIX}{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).unwrap())
        ))
        .is_err());
        assert!(Enrollment::new(
            "Fixture".into(),
            invitation.url,
            Zeroizing::new("invalid".into()),
            None
        )
        .is_err());
    }
    #[test]
    fn repeated_import_reuses_identity_and_files_and_rotation_preserves_old_credentials() {
        let directory =
            std::env::temp_dir().join(format!("eclipse-enrollment-{}", uuid::Uuid::new_v4()));
        let invitation = enrollment();
        let first = import(&directory, &invitation, "owned-device", true).unwrap();
        let second = import(&directory, &invitation, "owned-device", true).unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(first.token_file, second.token_file);
        assert_eq!(
            Store::open(&directory)
                .unwrap()
                .read()
                .unwrap()
                .services
                .len(),
            1
        );
        let next = Enrollment::new(
            invitation.name,
            invitation.url,
            Zeroizing::new("cd".repeat(32)),
            None,
        )
        .unwrap();
        let rotated = import(&directory, &next, "owned-device", true).unwrap();
        assert_eq!(rotated.id, first.id);
        assert_ne!(rotated.token_file, first.token_file);
        assert_eq!(
            std::fs::read_to_string(first.token_file).unwrap(),
            "ab".repeat(32)
        );
        assert!(rotated.host_enabled);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
