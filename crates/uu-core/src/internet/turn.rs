//! Coturn REST credentials belong to a single signaling session, not a saved
//! device profile. The shared secret remains on the signaling host.
use anyhow::{ensure, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::{io::Read, path::PathBuf, time::{Duration, SystemTime, UNIX_EPOCH}};
use webrtc::ice_transport::{ice_credential_type::RTCIceCredentialType, ice_server::RTCIceServer};

pub struct Config {
    pub urls: Vec<String>,
    pub secret_file: PathBuf,
    pub credential_lifetime: Duration,
}
pub(super) struct Credentials {
    urls: Vec<String>,
    key: ring::hmac::Key,
    lifetime: u64,
}
impl Credentials {
    pub fn load(config: Config) -> Result<Self> {
        ensure!(!config.urls.is_empty() && config.urls.len() <= 8, "TURN requires one to eight URLs");
        for url in &config.urls {
            let parsed = webrtc::ice::url::Url::parse_url(url)?;
            ensure!(url.len() <= 2048 && parsed.scheme == webrtc::ice::url::SchemeType::Turn
                && parsed.proto == webrtc::ice::url::ProtoType::Udp,
                "this build supports TURN over UDP only");
        }
        let lifetime = config.credential_lifetime.as_secs();
        ensure!((300..=172800).contains(&lifetime), "TURN credential lifetime must be 300..172800 seconds");
        let mut secret = zeroize::Zeroizing::new(String::new());
        std::fs::File::open(config.secret_file)?.take(4097).read_to_string(&mut secret)?;
        let secret = secret.trim();
        ensure!((32..=4096).contains(&secret.len()), "TURN shared secret must contain 32..4096 bytes");
        Ok(Self { urls: config.urls, key: ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret.as_bytes()), lifetime })
    }
    pub fn issue(&self, session: &str, role: &str) -> Result<Vec<RTCIceServer>> {
        self.issue_at(session, role, SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
    }
    fn issue_at(&self, session: &str, role: &str, now: u64) -> Result<Vec<RTCIceServer>> {
        let expiry = now.checked_add(self.lifetime).ok_or_else(||anyhow::anyhow!("TURN credential expiry overflow"))?;
        let username = format!("{expiry}:{session}-{role}");
        let credential = STANDARD.encode(ring::hmac::sign(&self.key, username.as_bytes()).as_ref());
        Ok(vec![RTCIceServer { urls: self.urls.clone(), username, credential,
            credential_type: RTCIceCredentialType::Password }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_match_coturn_rest_and_are_scoped_to_role_and_expiry() {
        let key=ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,b"generated shared secret for owned fixture");
        let credentials=Credentials{urls:vec!["turn:127.0.0.1:3478".into()],key,lifetime:3600};
        let host=credentials.issue_at("session", "host", 1_000_000).unwrap();
        assert_eq!(host[0].username,"1003600:session-host");
        let signature=STANDARD.decode(&host[0].credential).unwrap();
        ring::hmac::verify(&credentials.key,host[0].username.as_bytes(),&signature).unwrap();
        assert_ne!(host[0].credential,credentials.issue_at("session","viewer",1_000_000).unwrap()[0].credential);
        assert_ne!(host[0].credential,credentials.issue_at("session","host",1_000_001).unwrap()[0].credential);
        assert!(credentials.issue_at("session","host",u64::MAX).is_err());
    }
}
