//! Signaling / control-plane client.
//!
//! Two families of endpoints are involved, both recovered by observation:
//! * the *release feed*, which is plain HTTPS and returns a 302 to a CDN
//!   object (`https://api.nrd.nie.163.com/api/v1/release/dl/<id>?channel=gwqd`)
//! * the authenticated control API used to enumerate devices and open a
//!   session; its request signing is still under investigation
//!   (see `docs/protocol-notes.md`).

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::header::USER_AGENT;
use serde::Deserialize;

use crate::config::Config;
use crate::error::CoreError;
use crate::sign::{self, ClientIdentity};

/// One entry of NetEase's public release feed.
#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseTarget {
    pub id: u32,
    pub name: &'static str,
}

/// Platform ids observed on the live feed. Ids 0 and >5 return 422/404, so
/// this list is exhaustive for the public feed.
pub const RELEASE_TARGETS: &[ReleaseTarget] = &[
    ReleaseTarget {
        id: 1,
        name: "windows-x64",
    },
    ReleaseTarget {
        id: 2,
        name: "android",
    },
    ReleaseTarget { id: 3, name: "ios" },
    ReleaseTarget {
        id: 4,
        name: "macos",
    },
    ReleaseTarget {
        id: 5,
        name: "android-tv",
    },
];

const RELEASE_CHANNEL: &str = "gwqd";

#[derive(Clone)]
pub struct SignalClient {
    http: reqwest::Client,
    host: String,
    signing_key_path: std::path::PathBuf,
    signing_key_builtin_fallback: bool,
}

impl SignalClient {
    pub fn new(config: &Config) -> Result<Self, CoreError> {
        let http = reqwest::Client::builder()
            .user_agent("uu-remote-rs/0.0.1")
            .build()?;
        Ok(Self {
            http,
            host: config.signaling_host.clone(),
            signing_key_path: crate::signing_key::SigningKey::path(config),
            signing_key_builtin_fallback: std::env::var_os("UU_SIGNING_KEY_FILE").is_none(),
        })
    }

    /// Ask the release feed where a platform's installer now lives.
    ///
    /// Returns the resolved CDN URL by following the redirect manually so the
    /// caller can log it without downloading the payload.
    pub async fn release_url(&self, platform_id: u32) -> Result<String, CoreError> {
        let url = format!(
            "https://{}/api/v1/release/dl/{}?channel={}",
            self.host, platform_id, RELEASE_CHANNEL
        );
        let resp = self
            .http
            .get(&url)
            .header(USER_AGENT, "uu-remote-rs/0.0.1")
            .send()
            .await?;
        let status = resp.status();
        if status.is_redirection() {
            if let Some(loc) = resp.headers().get(reqwest::header::LOCATION) {
                return Ok(loc.to_str().unwrap_or_default().to_string());
            }
        }
        if status.is_success() {
            return Ok(resp.url().to_string());
        }
        Err(CoreError::Service {
            code: status.as_u16() as i32,
            message: format!("release feed returned {status}"),
        })
    }

    /// Placeholder for the authenticated control channel.
    ///
    /// Wiring this up requires the request-signing scheme recovered from the
    /// client (`nrd` signature + device attestation); until then we fail
    /// explicitly rather than pretending to be authenticated.
    pub async fn authenticated(&self) -> Result<(), CoreError> {
        Err(CoreError::Unauthenticated)
    }
}

#[cfg(test)]
mod credential_tests {
    use super::*;

    #[tokio::test]
    async fn missing_credentials_fail_before_any_network_request() {
        let client = SignalClient {
            http: reqwest::Client::new(),
            host: "127.0.0.1:9".into(),
            signing_key_path: std::env::temp_dir()
                .join(format!("uu-no-key-{}", uuid::Uuid::new_v4())),
            signing_key_builtin_fallback: false,
        };
        let result = client
            .call(
                &ClientIdentity::default(),
                None,
                SignedRequest {
                    method: "GET",
                    path_with_query: "/test",
                    body: None,
                },
            )
            .await;
        assert!(matches!(result, Err(CoreError::SigningKeyMissing)));
    }
}

/// A response from the signed API.
#[derive(Debug, Clone)]
pub struct ApiResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: serde_json::Value,
}

/// One signed request.
pub struct SignedRequest<'a> {
    pub method: &'a str,
    pub path_with_query: &'a str,
    pub body: Option<serde_json::Value>,
}

impl SignalClient {
    /// Call the API with the recovered request signature.
    ///
    /// This is the primitive the whole client builds on: every endpoint,
    /// including login and session creation, goes through here.
    pub async fn call(
        &self,
        identity: &ClientIdentity,
        token: Option<&str>,
        req: SignedRequest<'_>,
    ) -> Result<ApiResponse, CoreError> {
        let signing_key = crate::signing_key::SigningKey::load_for_pc(
            &self.signing_key_path,
            self.signing_key_builtin_fallback,
        )?;
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| CoreError::Config(e.to_string()))?
            .as_secs();

        let mut headers: BTreeMap<String, String> = identity.headers(ts);
        let body_str = req
            .body
            .as_ref()
            .map(|b| serde_json::to_string(b).expect("serialising JSON value"));
        let signature = sign::sign_request(
            signing_key.as_bytes(),
            req.method,
            req.path_with_query,
            &headers,
            body_str.as_deref(),
        );
        headers.insert("x-param-sign".to_string(), signature);

        let url = format!("https://{}{}", self.host, req.path_with_query);
        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|e| CoreError::Config(format!("bad method {}: {e}", req.method)))?;
        let mut builder = self.http.request(method, &url);
        for (k, v) in &headers {
            builder = builder.header(k, v);
        }
        builder = builder.header(USER_AGENT, "uu-remote-rs/0.0.1");
        if let Some(token) = token {
            builder = builder.header("Authorization", format!("Bearer {token}"));
        }
        if let Some(body) = &body_str {
            builder = builder
                .header("Content-Type", "application/json")
                .body(body.clone());
        }

        let resp = builder.send().await?;
        let status = resp.status().as_u16();
        let mut headers = BTreeMap::new();
        for (k, v) in resp.headers().iter() {
            if let Ok(v) = v.to_str() {
                headers.insert(k.as_str().to_string(), v.to_string());
            }
        }
        let text = resp.text().await?;
        let body = serde_json::from_str(&text).unwrap_or_else(|_| serde_json::Value::String(text));
        Ok(ApiResponse {
            status,
            headers,
            body,
        })
    }
}
