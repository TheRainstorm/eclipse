//! Package ordering is independent of the frontend and installation transport.
//! An explicit deployment may select an older package; automatic replacement
//! requires a strictly later complete package, never just a different revision.
use anyhow::{ensure, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};

pub const DEFERRED_HOST: i32 = 10;
pub const DEFERRED_INSTALLATION: i32 = 11;
pub const SUPERSEDED: i32 = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub revision: String,
    pub build: String,
    pub built_at_utc: Option<String>,
}
impl Package {
    pub fn read(directory: &Path) -> Result<Self> {
        let mut bytes = Vec::new();
        std::fs::File::open(directory.join("version.json"))?.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 64 * 1024, "package manifest is too large");
        Self::from_manifest(&serde_json::from_slice(&bytes)?)
    }
    pub fn from_manifest(manifest: &Value) -> Result<Self> {
        let revision = manifest["source_commit"].as_str().context("missing package revision")?;
        ensure!(revision.len() == 40 && revision.bytes().all(|b| b.is_ascii_hexdigit()), "invalid package revision");
        ensure!(manifest["sha256"].is_object(), "missing package payload identity");
        Ok(Self {
            revision: revision.into(),
            build: hex::encode(Sha256::digest(serde_json::to_vec(&manifest["sha256"])?)),
            built_at_utc: manifest["built_at_utc"].as_str().filter(|s| utc_key(s).is_some()).map(str::to_owned),
        })
    }
    pub fn is_later_than(&self, installed_revision: &str, installed_time: Option<&str>) -> bool {
        self.revision != installed_revision
            && self.built_at_utc.as_deref().and_then(utc_key)
                .zip(installed_time.and_then(utc_key)).is_some_and(|(new, old)| new > old)
    }
}

/// Normalize the existing packager's UTC ISO-8601 timestamp, including omitted
/// or fractional seconds, into a lexically ordered fixed-width key.
fn utc_key(value: &str) -> Option<[u8; 29]> {
    let body = value.strip_suffix("+00:00").or_else(|| value.strip_suffix('Z'))?;
    let (seconds, fraction) = body.split_once('.').unwrap_or((body, ""));
    let bytes = seconds.as_bytes();
    if bytes.len() != 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T'
        || bytes[13] != b':' || bytes[16] != b':' || fraction.len() > 9 || !fraction.bytes().all(|b| b.is_ascii_digit())
        || bytes.iter().enumerate().any(|(i,b)| ![4,7,10,13,16].contains(&i) && !b.is_ascii_digit()) { return None; }
    let number = |start| (bytes[start] - b'0') * 10 + bytes[start + 1] - b'0';
    if !(1..=12).contains(&number(5)) || !(1..=31).contains(&number(8))
        || number(11) > 23 || number(14) > 59 || number(17) > 59 { return None; }
    let mut key = [b'0'; 29];
    key[..19].copy_from_slice(bytes); key[19] = b'.';
    key[20..20 + fraction.len()].copy_from_slice(fraction.as_bytes());
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn package(revision: &str, time: Option<&str>) -> Package {
        Package { revision: revision.repeat(40), build: "c".repeat(64), built_at_utc: time.map(str::to_owned) }
    }
    #[test]
    fn automatic_updates_are_monotonic_and_matching_sources_are_noops() {
        let earlier = package("a", Some("2026-11-12T10:01:02.100+00:00"));
        let later = package("b", Some("2026-11-12T10:01:02.2Z"));
        assert!(later.is_later_than(&earlier.revision, earlier.built_at_utc.as_deref()));
        assert!(!earlier.is_later_than(&later.revision, later.built_at_utc.as_deref()));
        assert!(!later.is_later_than(&later.revision, earlier.built_at_utc.as_deref()));
        assert!(!later.is_later_than(&earlier.revision, None));
        assert!(!package("b", None).is_later_than(&earlier.revision, earlier.built_at_utc.as_deref()));
        assert!(!package("b", earlier.built_at_utc.as_deref()).is_later_than(&earlier.revision, earlier.built_at_utc.as_deref()));
    }
    #[test]
    fn timestamps_keep_utc_precision_and_reject_ambiguous_metadata() {
        assert_eq!(utc_key("2026-11-12T10:01:02Z"), utc_key("2026-11-12T10:01:02.000000+00:00"));
        for time in ["", "2026-11-12T10:01:02+08:00", "2026-99-12T10:01:02Z", "2026-11-12T25:01:02Z", "2026-11-12T10:01:02.invalidZ"] {
            assert!(utc_key(time).is_none());
        }
        let manifest = serde_json::json!({"source_commit":"a".repeat(40),"sha256":{"eclipse-cli.exe":"fixture"},"built_at_utc":"invalid"});
        assert!(Package::from_manifest(&manifest).unwrap().built_at_utc.is_none());
    }
}
