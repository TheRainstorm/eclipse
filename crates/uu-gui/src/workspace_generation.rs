//! A live workspace belongs to one frozen application/SDK build generation.
//!
//! Updated executables must not hand new UI behavior to an older live owner.
//! Old owners and their streams remain untouched; each generation still uses
//! the existing authenticated, user-private local-workspace protocol.
//!
//! The identity comes only from compiled constants. A clean frozen build has a
//! stable generation. Different dirty GUI builds at the same revision/version
//! and SDK are intentionally indistinguishable here; development snapshots
//! must use a distinct commit/version when that distinction matters. Source
//! archives without a revision remain deterministic using version plus SDK.
use sha2::{Digest, Sha256};
use std::path::PathBuf;

const PREFIX: &str = "stream-workspace-v3-";
const DOMAIN: &[u8] = b"eclipse/local-stream-workspace/generation/v3\0";

pub(super) fn directory(config: &uu_core::Config) -> PathBuf {
    config.at(derive(
        env!("CARGO_PKG_VERSION"),
        env!("UU_BUILD_COMMIT"),
        uu_plugin_sdk::SDK_ID,
    ))
}

fn derive(version: &str, revision: &str, sdk: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(DOMAIN);
    // Length framing preserves component boundaries, including empty strings
    // and embedded separators. Fixed-width lengths are portable across hosts.
    for component in [version, revision, sdk] {
        hash.update(
            u64::try_from(component.len())
                .expect("compiled build identity length fits u64")
                .to_be_bytes(),
        );
        hash.update(component.as_bytes());
    }
    let digest = hash.finalize();
    // A 128-bit generation suffix keeps the entire component below 64 ASCII
    // bytes and leaves headroom for endpoint filenames on Windows. This is an
    // identity namespace, not a replacement for endpoint authentication.
    let mut name = String::with_capacity(PREFIX.len() + 32);
    name.push_str(PREFIX);
    for byte in &digest[..16] {
        use std::fmt::Write;
        write!(&mut name, "{byte:02x}").expect("writing into String cannot fail");
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_tuple_is_stable_and_each_component_is_significant() {
        let name = derive("1.2.3-dev", "0123456789abcdef", "sdk-a");
        assert_eq!(name, derive("1.2.3-dev", "0123456789abcdef", "sdk-a"));
        for changed in [
            derive("1.2.4-dev", "0123456789abcdef", "sdk-a"),
            derive("1.2.3-dev", "fedcba9876543210", "sdk-a"),
            derive("1.2.3-dev", "0123456789abcdef", "sdk-b"),
        ] {
            assert_ne!(name, changed);
        }
    }

    #[test]
    fn component_boundaries_cannot_alias_by_concatenation_or_separators() {
        assert_ne!(derive("ab", "c", "d"), derive("a", "bc", "d"));
        assert_ne!(derive("a", "bc", "d"), derive("a", "b", "cd"));
        assert_ne!(derive("a\0b", "c", "d"), derive("a", "b\0c", "d"));
        assert_ne!(derive("a", "", "bc"), derive("a", "b", "c"));
    }

    #[test]
    fn missing_revision_is_stable_without_process_or_disk_fallback() {
        assert_eq!(derive("1.2.3", "", "sdk"), derive("1.2.3", "", "sdk"));
        assert_ne!(
            derive("1.2.3", "", "sdk"),
            derive("1.2.3", "unknown", "sdk")
        );
        assert_ne!(derive("1.2.3", "", "sdk"), derive("1.2.4", "", "sdk"));
    }

    #[test]
    fn directory_is_a_bounded_ascii_component_under_the_config_root() {
        let config = uu_core::Config {
            data_dir: PathBuf::from("generated-data"),
            state_dir: PathBuf::from("generated-state"),
            signaling_host: String::new(),
        };
        let path = directory(&config);
        assert_eq!(path.parent(), Some(config.data_dir.as_path()));
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(name.len() <= 64);
        let suffix = name.strip_prefix(PREFIX).unwrap();
        assert_eq!(suffix.len(), 32);
        assert!(suffix
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)));
    }
}
