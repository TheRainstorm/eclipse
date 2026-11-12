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
const GROUP_ERROR: &str = "连接服务接入码的设备组无效或缺失，请向管理员获取新接入码";

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
        let url = url::Url::parse(&normalize_url(&self.url)?)?;
        super::server::groups::query_group(url.query().unwrap_or("")).context(GROUP_ERROR)?;
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
    let mut url = url::Url::parse(&value).context("连接服务地址无效")?;
    ensure!(
        url.scheme() == "wss"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.port() != Some(0),
        "连接服务需要有效的 WSS 地址，凭据须由接入码单独提供"
    );
    normalize_group(&mut url)?;
    Ok(url.into())
}

/// Build a stable group endpoint without putting the rotating credential in
/// its identity. Other query bytes and their relative order remain unchanged.
/// An already scoped base must refer to the same group.
pub fn group_url(base: &str, group: &uuid::Uuid) -> Result<String> {
    let mut url = url::Url::parse(&normalize_url(base)?)?;
    match normalize_group(&mut url)? {
        Some(existing) => ensure!(existing == *group, "连接服务地址无效"),
        None => {
            let query = match url.query() {
                Some(query) => format!("{query}&group={group}"),
                None => format!("group={group}"),
            };
            url.set_query(Some(&query));
        }
    }
    normalize_group(&mut url)?;
    Ok(url.into())
}

fn normalize_group(url: &mut url::Url) -> Result<Option<uuid::Uuid>> {
    let Some(query) = url.query() else {
        return Ok(None);
    };
    if !url.query_pairs().any(|(key, _)| key == "group") {
        return Ok(None);
    }
    let group = super::server::groups::query_group(query).context(GROUP_ERROR)?;
    let mut other = Vec::new();
    for field in query.split('&') {
        let pair = url::form_urlencoded::parse(field.as_bytes()).next();
        if !pair.is_some_and(|(key, _)| key == "group") {
            other.push(field);
        }
    }
    // Only the owned group parameter moves. Sorting or decoding unrelated
    // parameters could alter repeated values or invalidate signed URLs.
    let group_field = format!("group={group}");
    other.push(&group_field);
    let canonical = other.join("&");
    url.set_query(Some(&canonical));
    Ok(Some(group))
}

/// Re-import updates the same server entry and keeps device routes valid.
/// Existing streams retain their admitted credential files until retirement.
pub enum Publication<'a> {
    /// The host owns its published identity; an existing custom room stays valid.
    PreserveOrDefault(&'a str),
    /// A client mirrors the actual host identity confirmed by management IPC.
    Mirror(&'a str),
}
pub fn import(
    directory: &Path,
    invitation: &Enrollment,
    publication: Publication<'_>,
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
        let published_name = match publication {
            Publication::PreserveOrDefault(default) => previous
                .as_ref()
                .map(|s| s.published_name.as_str())
                .filter(|name| !name.is_empty())
                .unwrap_or(default),
            Publication::Mirror(actual) => actual,
        }
        .to_owned();
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
            published_name,
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
            group_url("SIGNAL.example:443", &uuid::Uuid::new_v4()).unwrap(),
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
        assert_eq!(decoded.url, invitation.url);
        assert!(decoded
            .url
            .starts_with("wss://signal.example/v1/signal?group="));
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
        let first = import(
            &directory,
            &invitation,
            Publication::PreserveOrDefault("owned-device"),
            true,
        )
        .unwrap();
        let second = import(
            &directory,
            &invitation,
            Publication::PreserveOrDefault("owned-device"),
            true,
        )
        .unwrap();
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
        let rotated = import(
            &directory,
            &next,
            Publication::PreserveOrDefault("owned-device"),
            true,
        )
        .unwrap();
        assert_eq!(rotated.id, first.id);
        assert_ne!(rotated.token_file, first.token_file);
        assert_eq!(
            std::fs::read_to_string(first.token_file).unwrap(),
            "ab".repeat(32)
        );
        assert!(rotated.host_enabled);
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn equivalent_urls_preserve_custom_host_rooms_and_clients_mirror_the_confirmed_room() {
        let root =
            std::env::temp_dir().join(format!("eclipse-room-import-{}", uuid::Uuid::new_v4()));
        let host = root.join("host");
        let client = root.join("client");
        let invitation = enrollment();
        let first = import(
            &host,
            &invitation,
            Publication::PreserveOrDefault("custom-room"),
            true,
        )
        .unwrap();
        let equivalent = Enrollment::new(
            invitation.name.clone(),
            invitation.url.replace("group=", "%67roup="),
            invitation.token.clone(),
            None,
        )
        .unwrap();
        let again = import(
            &host,
            &equivalent,
            Publication::PreserveOrDefault("new-public-key-id"),
            true,
        )
        .unwrap();
        assert_eq!(first.id, again.id);
        assert_eq!(again.published_name, "custom-room");
        assert_eq!(
            Store::open(&host).unwrap().read().unwrap().services.len(),
            1
        );
        let previous = import(
            &client,
            &invitation,
            Publication::PreserveOrDefault("old-client-room"),
            false,
        )
        .unwrap();
        let mirrored = import(
            &client,
            &equivalent,
            Publication::Mirror(&again.published_name),
            true,
        )
        .unwrap();
        assert_eq!(mirrored.id, previous.id);
        assert_eq!(mirrored.published_name, "custom-room");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn group_endpoints_canonicalize_only_the_owned_parameter_and_reject_ambiguous_scopes() {
        let group = uuid::Uuid::new_v4();
        let base = "wss://signal.example/v1/signal";
        let canonical = group_url(base, &group).unwrap();
        assert_eq!(canonical, format!("{base}?group={group}"));
        assert_eq!(group_url(&canonical, &group).unwrap(), canonical);
        assert_ne!(group_url(&format!("{base}/"), &group).unwrap(), canonical);
        let scoped = normalize_url(&format!(
            "{base}?%67roup={}&signed=%2f+%20&item=2&item=1",
            group.to_string().to_uppercase()
        ))
        .unwrap();
        assert_eq!(
            scoped,
            format!("{base}?signed=%2f+%20&item=2&item=1&group={group}")
        );
        assert_eq!(
            group_url(&format!("{base}?signed=%2f+%20&item=2&item=1"), &group).unwrap(),
            scoped
        );
        assert_ne!(
            normalize_url(&format!("{base}/?group={group}")).unwrap(),
            canonical,
            "a different HTTP path must not silently reuse a service identity"
        );
        assert_ne!(
            normalize_url(&format!("{base}?item=1&item=2&group={group}")).unwrap(),
            normalize_url(&format!("{base}?item=2&item=1&group={group}")).unwrap(),
            "unrelated repeated query parameters keep their original order"
        );
        assert_eq!(
            normalize_url("wss://signal.example/v1/signal?signature=raw%2f/").unwrap(),
            "wss://signal.example/v1/signal?signature=raw%2f/"
        );
        assert!(group_url(&canonical, &uuid::Uuid::new_v4()).is_err());
        assert!(group_url(base, &uuid::Uuid::nil()).is_err());
        for query in [
            format!("group={group}&group={group}"),
            "group=invalid".into(),
            "group=".into(),
            format!("group={}", uuid::Uuid::nil()),
        ] {
            assert!(normalize_url(&format!("{base}?{query}")).is_err());
        }
    }

    #[test]
    fn private_groups_keep_routes_launches_and_device_trust_separate_while_credentials_rotate() {
        use crate::{
            device_book::{Destination, Launch, Profile},
            device_trust::{Permissions, PublicDevice, TrustStore},
        };
        let root =
            std::env::temp_dir().join(format!("eclipse-group-import-{}", uuid::Uuid::new_v4()));
        let store = Store::open(&root).unwrap();
        let trust = TrustStore::open(&root).unwrap();
        let peer = PublicDevice {
            name: "Paired device".into(),
            public_key: "12".repeat(32),
        };
        trust
            .authorize(
                peer.clone(),
                Permissions {
                    view: true,
                    control: true,
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        let peers_before = std::fs::read(root.join("peers.json")).unwrap();
        let local_identity = trust.card("Local device").id();
        let original = enrollment();
        let group_a = uuid::Uuid::new_v4();
        let group_b = uuid::Uuid::new_v4();
        let a = Enrollment::new(
            "Group A".into(),
            group_url("wss://signal.example/v1/signal", &group_a).unwrap(),
            original.token.clone(),
            None,
        )
        .unwrap();
        let first = import(
            &root,
            &a,
            Publication::PreserveOrDefault("custom-room-a"),
            true,
        )
        .unwrap();
        let mut profile = Profile::new(
            "Saved paired device",
            Destination::Vps {
                service: first.id.clone(),
                device: "shared-room".into(),
            },
        );
        profile.peer = Some(peer.id());
        store
            .edit(|book| {
                book.devices.push(profile.clone());
                Ok(())
            })
            .unwrap();
        let profiles_before = serde_json::to_value(store.read().unwrap().devices).unwrap();
        let frozen_path = store.publish_launch(&profile.id).unwrap();
        let frozen_before = std::fs::read(&frozen_path).unwrap();

        let b = Enrollment::new(
            "Group B".into(),
            group_url("wss://signal.example/v1/signal", &group_b).unwrap(),
            Zeroizing::new("cd".repeat(32)),
            None,
        )
        .unwrap();
        let second = import(
            &root,
            &b,
            Publication::PreserveOrDefault("new-host-id"),
            true,
        )
        .unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(
            second.published_name, "new-host-id",
            "a new group never inherits another group's custom room"
        );
        assert_eq!(store.read().unwrap().services.len(), 2);
        assert_eq!(store.launch(&profile.id).unwrap().service.unwrap(), first);

        let rotated =
            Enrollment::new(a.name, a.url, Zeroizing::new("ef".repeat(32)), None).unwrap();
        let refreshed = import(
            &root,
            &rotated,
            Publication::PreserveOrDefault("changed-default-id"),
            true,
        )
        .unwrap();
        assert_eq!(refreshed.id, first.id);
        assert_eq!(refreshed.published_name, "custom-room-a");
        assert_ne!(refreshed.token_file, first.token_file);
        assert_eq!(store.read().unwrap().service(&second.id).unwrap(), &second);
        assert_eq!(
            store.launch(&profile.id).unwrap().service.unwrap(),
            refreshed
        );
        assert_eq!(std::fs::read(&frozen_path).unwrap(), frozen_before);
        let frozen = Launch::read(&frozen_path).unwrap();
        assert_eq!(frozen.service.unwrap(), first);
        assert_eq!(frozen.expected_peer.as_deref(), Some(peer.id().as_str()));
        assert_eq!(
            std::fs::read_to_string(&first.token_file).unwrap(),
            "ab".repeat(32)
        );
        assert_eq!(
            serde_json::to_value(store.read().unwrap().devices).unwrap(),
            profiles_before
        );
        assert_eq!(
            std::fs::read(root.join("peers.json")).unwrap(),
            peers_before
        );
        assert_eq!(trust.card("Local device").id(), local_identity);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invitations_without_a_private_group_are_rejected_before_any_import_write() {
        let mut unscoped = enrollment();
        unscoped.url = "wss://signal.example/v1/signal".into();
        assert!(Enrollment::new(
            unscoped.name.clone(),
            unscoped.url.clone(),
            unscoped.token.clone(),
            None
        )
        .err()
        .unwrap()
        .to_string()
        .contains("设备组无效或缺失"));
        let raw = format!(
            "{PREFIX}{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&unscoped).unwrap())
        );
        assert!(Enrollment::decode(&raw)
            .err()
            .unwrap()
            .to_string()
            .contains("设备组无效或缺失"));
        assert!(unscoped
            .encode()
            .err()
            .unwrap()
            .to_string()
            .contains("设备组无效或缺失"));
        let directory =
            std::env::temp_dir().join(format!("eclipse-reject-unscoped-{}", uuid::Uuid::new_v4()));
        assert!(import(
            &directory,
            &unscoped,
            Publication::PreserveOrDefault("host-room"),
            true
        )
        .err()
        .unwrap()
        .to_string()
        .contains("设备组无效或缺失"));
        assert!(!directory.exists());
    }
}
