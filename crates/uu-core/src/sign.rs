//! Request signing for the UU Remote HTTP API.
//!
//! Recovered by hooking `BCryptCreateHash`/`BCryptHashData` in the running
//! Windows client and confirming 17/17 captured HMACs (see
//! `docs/protocol-notes.md` §2.6). The scheme is:
//!
//! ```text
//! headers  = all request headers whose lowercased name starts with "x-param-",
//!            excluding "x-param-sign", lowercased, SORTED by name,
//!            rendered as "k=v" and joined with "&"  (empty values included)
//! body     = the exact JSON string sent, or "" when there is no body
//! message  = METHOD + pathWithQuery + headers + body
//! signature= hex(HMAC-SHA256(key, message))
//! ```
//!
//! Two different keys exist in the product: the web bundle signs with its own
//! key, while the native client (which this crate speaks to) uses the key
//! below. They are NOT interchangeable — the server rejects the wrong one with
//! `code 1003`.

use std::collections::BTreeMap;

use hmac::{Hmac, Mac};
use sha2::Sha256;

pub type HmacSha256 = Hmac<Sha256>;

/// Native client signing key. Verified against captured traffic.
pub const NATIVE_SIGN_KEY: &[u8; 24] = b"REMOVED_SERVICE_KEY_0000";

/// Web bundle signing key (kept for reference; the native API rejects it).
pub const WEB_SIGN_KEY: &[u8; 24] = b"REMOVED_SERVICE_KEY_0000";

const HEADER_PREFIX: &str = "x-param-";
const SIGN_HEADER: &str = "x-param-sign";

/// Platform identity sent as `X-Param-*` headers.
///
/// The values are fixed by the Windows client build; changing `version_name`
/// without a matching server-side release is what produced `code 1001`
/// during probing, so they are kept in one place.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClientIdentity {
    /// `X-Param-PLAT`; `"1"` is what the Windows build sends.
    pub platform: String,
    pub version_name: String,
    pub version_code: String,
    pub package: String,
    pub channel: String,
    pub language: String,
    /// `X-Param-CNT`; the client sends the same value as `language`.
    pub country: String,
    /// `X-Param-REL`; `"prod"` on release builds.
    pub release: String,
    pub client_id: String,
    pub device_id: String,
    pub user_id: Option<String>,
}

impl Default for ClientIdentity {
    fn default() -> Self {
        Self {
            platform: "1".into(),
            version_name: "4.40.1.2090".into(),
            version_code: "2090".into(),
            package: "com.netease.uuremote".into(),
            channel: "gwqd".into(),
            language: "zh-CN".into(),
            country: "zh-CN".into(),
            release: "prod".into(),
            client_id: String::new(),
            device_id: String::new(),
            user_id: None,
        }
    }
}

impl ClientIdentity {
    /// Build the signed header set, including the current timestamp.
    ///
    /// A `BTreeMap` is used deliberately: iteration order is the sorted order
    /// the canonical string requires, so the two can never drift apart.
    pub fn headers(&self, unix_ts: u64) -> BTreeMap<String, String> {
        let mut h = BTreeMap::new();
        h.insert("x-param-plat".to_string(), self.platform.clone());
        h.insert("x-param-vn".to_string(), self.version_name.clone());
        h.insert("x-param-vc".to_string(), self.version_code.clone());
        h.insert("x-param-pkgn".to_string(), self.package.clone());
        h.insert("x-param-chn".to_string(), self.channel.clone());
        h.insert("x-param-lang".to_string(), self.language.clone());
        h.insert("x-param-cnt".to_string(), self.country.clone());
        h.insert("x-param-rel".to_string(), self.release.clone());
        h.insert("x-param-opr".to_string(), String::new());
        h.insert("x-param-system-id".to_string(), String::new());
        h.insert("x-param-ts".to_string(), unix_ts.to_string());
        h.insert("x-param-client-id".to_string(), self.client_id.clone());
        h.insert("x-param-device-id".to_string(), self.device_id.clone());
        if let Some(user) = &self.user_id {
            h.insert("x-param-user-id".to_string(), user.clone());
        }
        h
    }
}

/// Render the canonical string that gets signed.
///
/// `headers` must be the full header map; the function itself applies the
/// `x-param-` filter, lowercasing and sorting, so callers cannot get the
/// ordering rules wrong.
pub fn canonical_string(
    method: &str,
    path_with_query: &str,
    headers: &BTreeMap<String, String>,
    body: Option<&str>,
) -> String {
    let mut parts: Vec<(String, String)> = headers
        .iter()
        .filter(|(k, _)| {
            let k = k.to_ascii_lowercase();
            k.starts_with(HEADER_PREFIX) && k != SIGN_HEADER
        })
        .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
        .collect();
    parts.sort_by(|a, b| a.0.cmp(&b.0));

    let mut out = String::with_capacity(256);
    out.push_str(method);
    out.push_str(path_with_query);
    for (i, (k, v)) in parts.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(k);
        out.push('=');
        out.push_str(v);
    }
    if let Some(b) = body {
        out.push_str(b);
    }
    out
}

/// HMAC-SHA256 of the canonical string, lowercase hex.
pub fn sign_message(key: &[u8], message: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Sign one request and return the `X-Param-SIGN` value.
pub fn sign_request(
    key: &[u8],
    method: &str,
    path_with_query: &str,
    headers: &BTreeMap<String, String>,
    body: Option<&str>,
) -> String {
    let message = canonical_string(method, path_with_query, headers, body);
    sign_message(key, &message)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real captures: (canonical message, expected signature). These came from
    /// hooking BCrypt in the live client, so they are ground truth.
    const VECTORS: &[(&str, &str)] = &[
        (
            "GET/api/v1/release/fetch/updatex-param-chn=gwqd&x-param-client-id=&x-param-cnt=zh-CN&x-param-device-id=aeawvih7lubbnfna&x-param-lang=zh-CN&x-param-opr=&x-param-pkgn=com.netease.uuremote&x-param-plat=1&x-param-rel=prod&x-param-system-id=&x-param-ts=1792218924&x-param-vc=2090&x-param-vn=4.40.1.2090",
            "ea434c005c09f71486e30deaaa6b7bd7cba60c2a513ec4df2a7e4bfbbbca6371",
        ),
        (
            "GET/api/v1/campaign/placements/market_tab/tagsx-param-chn=gwqd&x-param-client-id=570ec35c-1b66-4d97-b39b-daff4ee2c505&x-param-cnt=zh-CN&x-param-device-id=aeawvih7lubbnfna&x-param-lang=zh-CN&x-param-opr=&x-param-pkgn=com.netease.uuremote&x-param-plat=1&x-param-rel=prod&x-param-system-id=&x-param-ts=1792218925&x-param-user-id=aebgvih7zyakodn7&x-param-vc=2090&x-param-vn=4.40.1.2090",
            "1f8dd02ed69181f21b26ba68107eaabf39f570793ed6e8a41d870e9618234a9f",
        ),
    ];

    #[test]
    fn captured_vectors_verify() {
        for (message, expected) in VECTORS {
            assert_eq!(&sign_message(NATIVE_SIGN_KEY, message), expected, "message={message}");
        }
    }

    #[test]
    fn canonical_string_matches_capture_shape() {
        let id = ClientIdentity {
            client_id: String::new(),
            device_id: "aeawvih7lubbnfna".into(),
            ..ClientIdentity::default()
        };
        let headers = id.headers(1_792_218_924);
        let msg = canonical_string("GET", "/api/v1/release/fetch/update", &headers, None);
        assert_eq!(
            msg,
            "GET/api/v1/release/fetch/updatex-param-chn=gwqd&x-param-client-id=&x-param-cnt=zh-CN&x-param-device-id=aeawvih7lubbnfna&x-param-lang=zh-CN&x-param-opr=&x-param-pkgn=com.netease.uuremote&x-param-plat=1&x-param-rel=prod&x-param-system-id=&x-param-ts=1792218924&x-param-vc=2090&x-param-vn=4.40.1.2090"
        );
        assert_eq!(
            sign_request(NATIVE_SIGN_KEY, "GET", "/api/v1/release/fetch/update", &headers, None),
            "ea434c005c09f71486e30deaaa6b7bd7cba60c2a513ec4df2a7e4bfbbbca6371"
        );
    }

    #[test]
    fn body_is_appended_verbatim() {
        let mut h = BTreeMap::new();
        h.insert("x-param-ts".to_string(), "5".to_string());
        let msg = canonical_string("POST", "/p", &h, Some(r#"{"a":1}"#));
        assert_eq!(msg, r#"POST/px-param-ts=5{"a":1}"#);
    }

    #[test]
    fn sign_header_is_excluded_from_canonical_string() {
        let mut h = BTreeMap::new();
        h.insert("x-param-ts".to_string(), "5".to_string());
        h.insert("x-param-sign".to_string(), "deadbeef".to_string());
        assert_eq!(canonical_string("GET", "/p", &h, None), "GET/px-param-ts=5");
    }

    #[test]
    fn web_and_native_keys_differ() {
        assert_ne!(NATIVE_SIGN_KEY, WEB_SIGN_KEY);
        let msg = "GET/x";
        assert_ne!(
            sign_message(NATIVE_SIGN_KEY, msg),
            sign_message(WEB_SIGN_KEY, msg)
        );
    }
}
